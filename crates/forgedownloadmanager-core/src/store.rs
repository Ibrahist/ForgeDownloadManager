use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Row};
use uuid::Uuid;

use crate::error::Error;
use crate::model::{
    i64_to_u64, now_ms, u64_to_i64, ChecksumAlgo, Download, DownloadStatus, DuplicatePolicy,
    Segment, SegmentStatus, SiteRule, SiteRuleAction,
};
use crate::schedule::{current_local_time, is_window_active, OnCompleteAction, Queue};
use crate::Result;

pub const DEFAULT_QUEUE_ID: &str = "00000000-0000-4000-8000-000000000001";

pub fn default_queue_id() -> Uuid {
    Uuid::parse_str(DEFAULT_QUEUE_ID).expect("static queue id")
}

pub struct Store {
    // transaction() needs &mut Connection. Callers share Store through a mutex, so the
    // exclusive borrow stays inside the cell and the public methods keep &self.
    conn: RefCell<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut conn = Connection::open(path)?;
        init_pragmas(&conn)?;
        migrate(&mut conn)?;
        Ok(Self { conn: RefCell::new(conn) })
    }

    pub fn open_readonly(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(Self { conn: RefCell::new(conn) })
    }

    pub fn list_if_exists(path: &Path) -> Result<Vec<Download>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        Self::open_readonly(path)?.list()
    }

    pub fn insert_download(&self, download: &Download) -> Result<()> {
        let headers = json_pairs(&download.headers)?;
        let cookies = json_pairs(&download.cookies)?;
        self.conn.borrow().execute(
            "INSERT INTO downloads (
                id, url, final_url, filename, save_dir, partial_path, queue_id, priority, status,
                resumable, total_bytes, downloaded_bytes, etag, last_modified, content_type,
                segment_count, user_agent, referer, headers_json, insecure_tls, checksum_algo,
                checksum_expected, error_code, error_message, retry_count, max_retries,
                duplicate_policy, created_at, updated_at, started_at, completed_at,
                cookies_json, proxy_json, credential_id, speed_limit_bps
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, ?20, ?21,
                ?22, ?23, ?24, ?25, ?26,
                ?27, ?28, ?29, ?30, ?31,
                ?32, ?33, ?34, ?35
            )",
            params![
                download.id.to_string(),
                download.url,
                download.final_url,
                download.filename,
                path_str(&download.save_dir),
                path_str(&download.partial_path),
                download.queue_id.to_string(),
                download.priority,
                download.status.as_str(),
                i64::from(download.resumable),
                opt_u64(download.total_bytes)?,
                u64_to_i64(download.downloaded_bytes)?,
                download.etag,
                download.last_modified,
                download.content_type,
                download.segment_count,
                download.user_agent,
                download.referer,
                headers,
                i64::from(download.insecure_tls),
                download.checksum_algo.map(|algo| algo.as_str().to_string()),
                download.checksum_expected,
                download.error_code,
                download.error_message,
                download.retry_count,
                download.max_retries,
                download.duplicate_policy.as_str(),
                download.created_at,
                download.updated_at,
                download.started_at,
                download.completed_at,
                cookies,
                download.proxy,
                download.credential_id.map(|id| id.to_string()),
                opt_u64(download.speed_limit_bps)?,
            ],
        )?;
        Ok(())
    }

    pub fn set_file_location(&self, id: Uuid, filename: &str, path: &Path) -> Result<()> {
        let changed = self.conn.borrow().execute(
            "UPDATE downloads SET filename=?1, partial_path=?2, updated_at=?3 WHERE id=?4",
            params![filename, path_str(path), now_ms(), id.to_string()],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Ok(())
    }

    pub fn update_probe(
        &self,
        id: Uuid,
        final_url: &str,
        filename: &str,
        partial_path: &Path,
        resumable: bool,
        total_bytes: Option<u64>,
        etag: Option<&str>,
        last_modified: Option<&str>,
        content_type: Option<&str>,
        segment_count: u32,
    ) -> Result<()> {
        self.conn.borrow().execute(
            "UPDATE downloads SET
                final_url=?1, filename=?2, partial_path=?3, resumable=?4, total_bytes=?5,
                etag=?6, last_modified=?7, content_type=?8, segment_count=?9, updated_at=?10
             WHERE id=?11",
            params![
                final_url,
                filename,
                path_str(partial_path),
                i64::from(resumable),
                opt_u64(total_bytes)?,
                etag,
                last_modified,
                content_type,
                segment_count,
                now_ms(),
                id.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn set_status(
        &self,
        id: Uuid,
        status: DownloadStatus,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<()> {
        let now = now_ms();
        let changed = self.conn.borrow().execute(
            "UPDATE downloads SET
                status=?1,
                error_code=?2,
                error_message=?3,
                updated_at=?4,
                started_at=CASE WHEN ?1 = 'downloading' AND started_at IS NULL THEN ?4 ELSE started_at END,
                completed_at=CASE WHEN ?1 = 'completed' THEN ?4 ELSE completed_at END
             WHERE id=?5",
            params![status.as_str(), error_code, error_message, now, id.to_string()],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Ok(())
    }

    pub fn set_retry_count(&self, id: Uuid, count: u32) -> Result<()> {
        self.conn.borrow().execute(
            "UPDATE downloads SET retry_count = MAX(retry_count, ?1), updated_at = ?2 WHERE id = ?3",
            params![count, now_ms(), id.to_string()],
        )?;
        Ok(())
    }

    /// Shrink one segment and insert the tail. The caller has already published the new end in memory.
    pub fn split_segment(&self, download_id: Uuid, parent_id: Uuid, new_end: u64, tail: &Segment) -> Result<()> {
        let now = now_ms();
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE segments SET end_byte=?1, updated_at=?2 WHERE id=?3",
            params![u64_to_i64(new_end)?, now, parent_id.to_string()],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(parent_id));
        }
        tx.execute(
            "INSERT INTO segments (id, download_id, start_byte, end_byte, cursor_byte, status, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                tail.id.to_string(),
                tail.download_id.to_string(),
                u64_to_i64(tail.start_byte)?,
                opt_u64(tail.end_byte)?,
                u64_to_i64(tail.cursor_byte)?,
                tail.status.as_str(),
                tail.updated_at,
            ],
        )?;
        tx.execute(
            "UPDATE downloads SET segment_count = segment_count + 1, updated_at=?1 WHERE id=?2",
            params![now, download_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn update_cursors(
        &self,
        download_id: Uuid,
        downloaded: u64,
        rows: &[(Uuid, u64, SegmentStatus)],
    ) -> Result<()> {
        let now = now_ms();
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        for (id, cursor, status) in rows {
            tx.execute(
                "UPDATE segments SET cursor_byte=?1, status=?2, updated_at=?3 WHERE id=?4",
                params![u64_to_i64(*cursor)?, status.as_str(), now, id.to_string()],
            )?;
        }
        tx.execute(
            "UPDATE downloads SET downloaded_bytes=?1, updated_at=?2 WHERE id=?3",
            params![u64_to_i64(downloaded)?, now, download_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish_open_ended(&self, download_id: Uuid, segment_id: Uuid, total: u64) -> Result<()> {
        let total = u64_to_i64(total)?;
        let now = now_ms();
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE segments SET end_byte=?1, cursor_byte=?1, status='done', updated_at=?2 WHERE id=?3",
            params![total, now, segment_id.to_string()],
        )?;
        tx.execute(
            "UPDATE downloads SET total_bytes=?1, downloaded_bytes=?1, resumable=0, updated_at=?2 WHERE id=?3",
            params![total, now, download_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Result<Download> {
        self.conn
            .borrow()
            .query_row("SELECT * FROM downloads WHERE id=?1", [id.to_string()], download_from_row)
            .optional()?
            .ok_or(Error::NotFound(id))
    }

    pub fn list(&self) -> Result<Vec<Download>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT * FROM downloads ORDER BY priority ASC, created_at ASC",
        )?;
        let rows = stmt.query_map([], download_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn resolve(&self, prefix: &str) -> Result<Uuid> {
        let prefix = prefix.trim().to_ascii_lowercase();
        if prefix.is_empty() {
            return Err(Error::InvalidId("empty id".into()));
        }
        if let Ok(id) = Uuid::parse_str(&prefix) {
            let _ = self.get(id)?;
            return Ok(id);
        }
        let compact = prefix.replace('-', "");
        let matches: Vec<Uuid> = self
            .list()?
            .into_iter()
            .filter(|download| {
                let full = download.id.to_string();
                full.starts_with(&prefix) || full.replace('-', "").starts_with(&compact)
            })
            .map(|download| download.id)
            .collect();
        match matches.as_slice() {
            [id] => Ok(*id),
            [] => Err(Error::InvalidId(format!("no download matches {prefix}"))),
            _ => Err(Error::InvalidId(format!("{prefix} matches more than one download"))),
        }
    }

    pub fn segments_for(&self, id: Uuid) -> Result<Vec<Segment>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT * FROM segments WHERE download_id=?1 ORDER BY start_byte ASC",
        )?;
        let rows = stmt.query_map([id.to_string()], segment_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn replace_segments(&self, id: Uuid, segments: &[Segment]) -> Result<()> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM segments WHERE download_id=?1", [id.to_string()])?;
        for segment in segments {
            tx.execute(
                "INSERT INTO segments (id, download_id, start_byte, end_byte, cursor_byte, status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    segment.id.to_string(),
                    segment.download_id.to_string(),
                    u64_to_i64(segment.start_byte)?,
                    opt_u64(segment.end_byte)?,
                    u64_to_i64(segment.cursor_byte)?,
                    segment.status.as_str(),
                    segment.updated_at,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_download(&self, id: Uuid) -> Result<()> {
        let changed = self.conn.borrow().execute("DELETE FROM downloads WHERE id=?1", [id.to_string()])?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Ok(())
    }

    pub fn log(&self, id: Uuid, level: &str, message: &str) -> Result<()> {
        self.conn.borrow().execute(
            "INSERT INTO download_log (download_id, ts, level, message) VALUES (?1, ?2, ?3, ?4)",
            params![id.to_string(), now_ms(), level, message],
        )?;
        Ok(())
    }

    pub fn has_log(&self, id: Uuid, message: &str) -> Result<bool> {
        let count: i64 = self.conn.borrow().query_row(
            "SELECT COUNT(*) FROM download_log WHERE download_id=?1 AND message=?2",
            params![id.to_string(), message],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn claim_queued(&self, limit: usize) -> Result<Vec<Uuid>> {
        let (hour, minute, weekday) = current_local_time();
        self.claim_queued_for_open_queues(limit, (hour, minute, weekday))
    }

    pub fn claim_queued_for_open_queues(
        &self,
        global_limit: usize,
        current_time: (u32, u32, u8),
    ) -> Result<Vec<Uuid>> {
        if global_limit == 0 {
            return Ok(Vec::new());
        }
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;

        let mut q_stmt = tx.prepare(
            "SELECT id, concurrency, schedule_start, schedule_end, days_mask FROM queues",
        )?;
        let queue_specs: Vec<(String, usize, Option<String>, Option<String>, u8)> = q_stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let conc: i64 = row.get(1)?;
                let start: Option<String> = row.get(2)?;
                let end: Option<String> = row.get(3)?;
                let mask: i64 = row.get(4)?;
                Ok((
                    id,
                    usize::try_from(conc).unwrap_or(1).max(1),
                    start,
                    end,
                    u8::try_from(mask).unwrap_or(127),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(q_stmt);

        let mut remaining_global = global_limit;
        let mut selected_ids = Vec::new();

        for (q_id, conc, start, end, mask) in queue_specs {
            if remaining_global == 0 {
                break;
            }

            if !is_window_active(
                start.as_deref(),
                end.as_deref(),
                mask,
                current_time.0,
                current_time.1,
                current_time.2,
            ) {
                continue;
            }

            let active: i64 = tx.query_row(
                "SELECT COUNT(*) FROM downloads WHERE queue_id = ?1 AND status IN ('downloading', 'probing')",
                [&q_id],
                |row| row.get(0),
            )?;
            let active_count = usize::try_from(active).unwrap_or(0);
            let q_slots = conc.saturating_sub(active_count);
            if q_slots == 0 {
                continue;
            }

            let take = q_slots.min(remaining_global);
            let mut d_stmt = tx.prepare(
                "SELECT id FROM downloads WHERE status = 'queued' AND queue_id = ?1
                 ORDER BY priority ASC, created_at ASC LIMIT ?2",
            )?;
            let ids: Vec<String> = d_stmt
                .query_map(params![q_id, i64::try_from(take).unwrap_or(i64::MAX)], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(d_stmt);

            remaining_global = remaining_global.saturating_sub(ids.len());
            selected_ids.extend(ids);
        }

        let now = now_ms();
        for id in &selected_ids {
            tx.execute(
                "UPDATE downloads
                 SET status = 'downloading', updated_at = ?1,
                     started_at = COALESCE(started_at, ?1)
                 WHERE id = ?2 AND status = 'queued'",
                params![now, id],
            )?;
        }
        tx.commit()?;

        selected_ids
            .into_iter()
            .map(|id| Uuid::parse_str(&id).map_err(|err| Error::InvalidId(err.to_string())))
            .collect()
    }

    pub fn count_active(&self) -> Result<usize> {
        let count: i64 = self.conn.borrow().query_row(
            "SELECT COUNT(*) FROM downloads WHERE status IN ('downloading', 'probing')",
            [],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    pub fn requeue_interrupted(&self) -> Result<usize> {
        let changed = self.conn.borrow().execute(
            "UPDATE downloads SET status = 'queued', updated_at = ?1
             WHERE status IN ('downloading', 'probing', 'ready')",
            [now_ms()],
        )?;
        Ok(changed)
    }

    pub fn list_queues(&self) -> Result<Vec<Queue>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT id, name, concurrency, speed_limit_bps, schedule_start, schedule_end,
                    days_mask, on_complete, on_complete_command, created_at
             FROM queues ORDER BY name ASC",
        )?;
        let rows = stmt.query_map([], map_queue)?;
        let mut queues = Vec::new();
        for row in rows {
            queues.push(row?);
        }
        Ok(queues)
    }

    pub fn get_queue(&self, id: Uuid) -> Result<Option<Queue>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT id, name, concurrency, speed_limit_bps, schedule_start, schedule_end,
                    days_mask, on_complete, on_complete_command, created_at
             FROM queues WHERE id = ?1",
        )?;
        stmt.query_row([id.to_string()], map_queue).optional().map_err(Error::from)
    }

    pub fn get_queue_by_name(&self, name: &str) -> Result<Option<Queue>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT id, name, concurrency, speed_limit_bps, schedule_start, schedule_end,
                    days_mask, on_complete, on_complete_command, created_at
             FROM queues WHERE LOWER(name) = LOWER(?1)",
        )?;
        stmt.query_row([name], map_queue).optional().map_err(Error::from)
    }

    pub fn create_queue(&self, queue: &Queue) -> Result<()> {
        self.conn.borrow().execute(
            "INSERT INTO queues (
                id, name, concurrency, speed_limit_bps, schedule_start, schedule_end,
                days_mask, on_complete, on_complete_command, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                queue.id.to_string(),
                queue.name,
                u64_to_i64(queue.concurrency as u64)?,
                opt_u64(queue.speed_limit_bps)?,
                queue.schedule_start,
                queue.schedule_end,
                i64::from(queue.days_mask),
                queue.on_complete.as_str(),
                queue.on_complete_command,
                queue.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn update_queue(&self, queue: &Queue) -> Result<()> {
        let changed = self.conn.borrow().execute(
            "UPDATE queues SET
                name = ?1, concurrency = ?2, speed_limit_bps = ?3, schedule_start = ?4,
                schedule_end = ?5, days_mask = ?6, on_complete = ?7, on_complete_command = ?8
             WHERE id = ?9",
            params![
                queue.name,
                u64_to_i64(queue.concurrency as u64)?,
                opt_u64(queue.speed_limit_bps)?,
                queue.schedule_start,
                queue.schedule_end,
                i64::from(queue.days_mask),
                queue.on_complete.as_str(),
                queue.on_complete_command,
                queue.id.to_string(),
            ],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(queue.id));
        }
        Ok(())
    }

    pub fn delete_queue(&self, id: Uuid) -> Result<()> {
        if id == default_queue_id() {
            return Err(Error::Failed("cannot delete the Default queue".into()));
        }
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE downloads SET queue_id = ?1 WHERE queue_id = ?2",
            params![DEFAULT_QUEUE_ID, id.to_string()],
        )?;
        let changed = tx.execute("DELETE FROM queues WHERE id = ?1", params![id.to_string()])?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn queue_counts(&self, id: Uuid) -> Result<(usize, usize)> {
        let conn = self.conn.borrow();
        let active: i64 = conn.query_row(
            "SELECT COUNT(*) FROM downloads WHERE queue_id = ?1 AND status IN ('downloading', 'probing')",
            [id.to_string()],
            |row| row.get(0),
        )?;
        let queued: i64 = conn.query_row(
            "SELECT COUNT(*) FROM downloads WHERE queue_id = ?1 AND status = 'queued'",
            [id.to_string()],
            |row| row.get(0),
        )?;
        Ok((usize::try_from(active).unwrap_or(0), usize::try_from(queued).unwrap_or(0)))
    }

    pub fn queue_concurrency(&self, id: Uuid) -> Result<usize> {
        let value: i64 = self.conn.borrow().query_row(
            "SELECT concurrency FROM queues WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(value).unwrap_or(1).max(1))
    }

    pub fn list_site_rules(&self) -> Result<Vec<SiteRule>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare("SELECT id, pattern, action, created_at FROM site_rules ORDER BY created_at ASC")?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let pattern: String = row.get(1)?;
            let action_str: String = row.get(2)?;
            let created_at: i64 = row.get(3)?;
            Ok((id, pattern, action_str, created_at))
        })?;
        let mut list = Vec::new();
        for item in rows {
            let (id, pattern, action_str, created_at) = item?;
            let id = Uuid::parse_str(&id).map_err(|e| Error::Failed(e.to_string()))?;
            let action = SiteRuleAction::parse(&action_str).unwrap_or(SiteRuleAction::Intercept);
            list.push(SiteRule {
                id,
                pattern,
                action,
                created_at,
            });
        }
        Ok(list)
    }

    pub fn get_site_rule(&self, id: Uuid) -> Result<Option<SiteRule>> {
        let conn = self.conn.borrow();
        conn.query_row(
            "SELECT id, pattern, action, created_at FROM site_rules WHERE id = ?1",
            [id.to_string()],
            |row| {
                let id: String = row.get(0)?;
                let pattern: String = row.get(1)?;
                let action_str: String = row.get(2)?;
                let created_at: i64 = row.get(3)?;
                Ok((id, pattern, action_str, created_at))
            },
        )
        .optional()
        .map_err(Error::from)?
        .map(|(id, pattern, action_str, created_at)| {
            let id = Uuid::parse_str(&id).map_err(|e| Error::Failed(e.to_string()))?;
            let action = SiteRuleAction::parse(&action_str).unwrap_or(SiteRuleAction::Intercept);
            Ok(SiteRule {
                id,
                pattern,
                action,
                created_at,
            })
        })
        .transpose()
    }

    pub fn create_site_rule(&self, rule: SiteRule) -> Result<SiteRule> {
        self.conn.borrow().execute(
            "INSERT INTO site_rules (id, pattern, action, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                rule.id.to_string(),
                rule.pattern,
                rule.action.as_str(),
                rule.created_at,
            ],
        )?;
        Ok(rule)
    }

    pub fn delete_site_rule(&self, id: Uuid) -> Result<()> {
        let changed = self.conn.borrow().execute("DELETE FROM site_rules WHERE id = ?1", [id.to_string()])?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Ok(())
    }

    pub fn match_site_rule(&self, host: &str) -> Result<Option<SiteRuleAction>> {
        let rules = self.list_site_rules()?;
        for rule in rules {
            if rule.matches_host(host) {
                return Ok(Some(rule.action));
            }
        }
        Ok(None)
    }

    pub fn setting_u64(&self, key: &str) -> Result<Option<u64>> {
        let raw: Option<String> = self
            .conn
            .borrow()
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |row| row.get(0))
            .optional()?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        Ok(value.as_u64().or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok())))
    }

    pub fn queue_speed_limit(&self, id: Uuid) -> Result<Option<u64>> {
        let value: Option<i64> = self.conn.borrow().query_row(
            "SELECT speed_limit_bps FROM queues WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        Ok(value.map(i64_to_u64).filter(|rate| *rate > 0))
    }

    pub fn setting_value(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let raw: Option<String> = self
            .conn
            .borrow()
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |row| row.get(0))
            .optional()?;
        raw.map(|raw| serde_json::from_str(&raw).map_err(Error::from)).transpose()
    }

    pub fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        crate::config::validate_setting(key, value)?;
        let raw = serde_json::to_string(value)?;
        self.conn.borrow().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, raw],
        )?;
        Ok(())
    }

    pub fn upsert_credential(&self, host: &str, username: &str, scheme: &str) -> Result<Credential> {
        if let Some(existing) = self.find_credential(host, username)? {
            self.conn.borrow().execute(
                "UPDATE credentials SET auth_scheme=?1 WHERE id=?2",
                params![scheme, existing.id.to_string()],
            )?;
            return Ok(existing);
        }
        let row = Credential {
            id: Uuid::new_v4(),
            host: host.to_string(),
            username: username.to_string(),
            keychain_id: Uuid::new_v4().to_string(),
            auth_scheme: scheme.to_string(),
        };
        self.conn.borrow().execute(
            "INSERT INTO credentials (id, host, username, keychain_id, auth_scheme, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.id.to_string(),
                row.host,
                row.username,
                row.keychain_id,
                row.auth_scheme,
                now_ms()
            ],
        )?;
        Ok(row)
    }

    pub fn find_credential(&self, host: &str, username: &str) -> Result<Option<Credential>> {
        self.conn
            .borrow()
            .query_row(
                "SELECT id, host, username, keychain_id, auth_scheme FROM credentials WHERE host=?1 AND username=?2",
                params![host, username],
                credential_from_row,
            )
            .optional()
            .map_err(Error::from)
    }

    pub fn get_credential(&self, id: Uuid) -> Result<Credential> {
        self.conn
            .borrow()
            .query_row(
                "SELECT id, host, username, keychain_id, auth_scheme FROM credentials WHERE id=?1",
                [id.to_string()],
                credential_from_row,
            )
            .optional()?
            .ok_or(Error::NotFound(id))
    }

    pub fn setting_string(&self, key: &str) -> Result<Option<String>> {
        let raw: Option<String> = self
            .conn
            .borrow()
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |row| row.get(0))
            .optional()?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        Ok(Some(match value {
            serde_json::Value::String(text) => text,
            other => other.to_string(),
        }))
    }
}

fn init_pragmas(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    // foreign_keys is per connection. WAL keeps the CLI reader from blocking the engine.
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        );",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if current < 1 {
        let tx = conn.transaction()?;
        tx.execute_batch(include_str!("../migrations/001_init.sql"))?;
        seed(&tx)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
            [now_ms()],
        )?;
        tx.commit()?;
    }
    // Fresh databases run version 1 and then this insert. Existing Phase 1 databases only run this.
    if current < 2 {
        let tx = conn.transaction()?;
        seed_proxy_settings(&tx)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (2, ?1)",
            [now_ms()],
        )?;
        tx.commit()?;
    }
    Ok(())
}

fn seed_proxy_settings(conn: &Connection) -> Result<()> {
    let settings = [
        ("proxy_mode", "\"direct\""),
        ("proxy_url", "null"),
        ("speed_limit_bps", "null"),
    ];
    for (key, value) in settings {
        conn.execute(
            "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
    }
    Ok(())
}

fn seed(conn: &Connection) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "INSERT INTO queues (
            id, name, concurrency, days_mask, on_complete, created_at
        ) VALUES (?1, 'Default', 3, 127, 'none', ?2)",
        params![DEFAULT_QUEUE_ID, now],
    )?;
    let categories = [
        ("00000000-0000-4000-8000-000000000002", "Video", r#"["mp4","mkv","avi","mov","webm","m4v","mpg","mpeg","wmv","flv","ts"]"#, 1),
        ("00000000-0000-4000-8000-000000000003", "Music", r#"["mp3","flac","wav","aac","ogg","m4a","wma","opus"]"#, 2),
        ("00000000-0000-4000-8000-000000000004", "Documents", r#"["pdf","doc","docx","xls","xlsx","ppt","pptx","txt","rtf","odt","csv"]"#, 3),
        ("00000000-0000-4000-8000-000000000005", "Compressed", r#"["zip","rar","7z","tar","gz","bz2","xz","iso"]"#, 4),
        ("00000000-0000-4000-8000-000000000006", "Programs", r#"["exe","msi","dmg","pkg","deb","rpm","apk","appimage"]"#, 5),
        ("00000000-0000-4000-8000-000000000007", "Other", "[]", 6),
    ];
    for (id, name, extensions, sort) in categories {
        conn.execute(
            "INSERT INTO categories (id, name, extensions, sort_order) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, extensions, sort],
        )?;
    }
    let settings = [
        ("max_concurrent_downloads", "3"),
        ("per_host_connections", "16"),
        ("default_segments", "8"),
        ("min_segment_bytes", "1048576"),
        ("user_agent", "\"ForgeDownloadManager/0.1\""),
    ];
    for (key, value) in settings {
        conn.execute("INSERT INTO settings (key, value) VALUES (?1, ?2)", params![key, value])?;
    }
    Ok(())
}

fn map_queue(row: &Row<'_>) -> rusqlite::Result<Queue> {
    let id_str: String = row.get(0)?;
    let id = Uuid::parse_str(&id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let name: String = row.get(1)?;
    let concurrency: i64 = row.get(2)?;
    let speed_limit_bps: Option<i64> = row.get(3)?;
    let schedule_start: Option<String> = row.get(4)?;
    let schedule_end: Option<String> = row.get(5)?;
    let days_mask: i64 = row.get(6)?;
    let on_complete_str: String = row.get(7)?;
    let on_complete = OnCompleteAction::parse(&on_complete_str).unwrap_or(OnCompleteAction::None);
    let on_complete_command: Option<String> = row.get(8)?;
    let created_at: i64 = row.get(9)?;

    Ok(Queue {
        id,
        name,
        concurrency: usize::try_from(concurrency).unwrap_or(1).max(1),
        speed_limit_bps: speed_limit_bps.map(i64_to_u64).filter(|r| *r > 0),
        schedule_start,
        schedule_end,
        days_mask: u8::try_from(days_mask).unwrap_or(127),
        on_complete,
        on_complete_command,
        created_at,
    })
}

fn download_from_row(row: &Row<'_>) -> rusqlite::Result<Download> {
    let status: String = row.get("status")?;
    let policy: String = row.get("duplicate_policy")?;
    let algo: Option<String> = row.get("checksum_algo")?;
    let headers: Option<String> = row.get("headers_json")?;
    let cookies: Option<String> = row.get("cookies_json")?;
    let credential: Option<String> = row.get("credential_id")?;
    Ok(Download {
        id: parse_uuid(row, "id")?,
        url: row.get("url")?,
        final_url: row.get("final_url")?,
        filename: row.get("filename")?,
        save_dir: PathBuf::from(row.get::<_, String>("save_dir")?),
        partial_path: PathBuf::from(row.get::<_, String>("partial_path")?),
        queue_id: parse_uuid(row, "queue_id")?,
        priority: row.get("priority")?,
        status: parse_status(&status)?,
        resumable: row.get::<_, i64>("resumable")? != 0,
        total_bytes: opt_u64_col(row, "total_bytes")?,
        downloaded_bytes: i64_to_u64(row.get("downloaded_bytes")?),
        etag: row.get("etag")?,
        last_modified: row.get("last_modified")?,
        content_type: row.get("content_type")?,
        segment_count: row.get::<_, i64>("segment_count")?.try_into().unwrap_or(1),
        user_agent: row.get("user_agent")?,
        referer: row.get("referer")?,
        headers: parse_headers(headers)?,
        cookies: parse_headers(cookies)?,
        proxy: row.get("proxy_json")?,
        credential_id: match credential {
            Some(value) => Some(Uuid::parse_str(&value).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
            })?),
            None => None,
        },
        speed_limit_bps: opt_u64_col(row, "speed_limit_bps")?,
        insecure_tls: row.get::<_, i64>("insecure_tls")? != 0,
        checksum_algo: match algo {
            Some(value) => Some(parse_algo(&value)?),
            None => None,
        },
        checksum_expected: row.get("checksum_expected")?,
        error_code: row.get("error_code")?,
        error_message: row.get("error_message")?,
        retry_count: row.get::<_, i64>("retry_count")?.try_into().unwrap_or(0),
        max_retries: row.get::<_, i64>("max_retries")?.try_into().unwrap_or(5),
        duplicate_policy: parse_policy(&policy)?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        started_at: row.get("started_at")?,
        completed_at: row.get("completed_at")?,
    })
}

fn segment_from_row(row: &Row<'_>) -> rusqlite::Result<Segment> {
    let status: String = row.get("status")?;
    Ok(Segment {
        id: parse_uuid(row, "id")?,
        download_id: parse_uuid(row, "download_id")?,
        start_byte: i64_to_u64(row.get("start_byte")?),
        end_byte: opt_u64_col(row, "end_byte")?,
        cursor_byte: i64_to_u64(row.get("cursor_byte")?),
        status: parse_segment_status(&status)?,
        updated_at: row.get("updated_at")?,
    })
}

fn parse_uuid(row: &Row<'_>, column: &str) -> rusqlite::Result<Uuid> {
    let text: String = row.get(column)?;
    Uuid::parse_str(&text).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn sql_convert(err: Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
}

fn parse_status(value: &str) -> rusqlite::Result<DownloadStatus> {
    DownloadStatus::parse(value).map_err(sql_convert)
}

fn parse_segment_status(value: &str) -> rusqlite::Result<SegmentStatus> {
    SegmentStatus::parse(value).map_err(sql_convert)
}

fn parse_policy(value: &str) -> rusqlite::Result<DuplicatePolicy> {
    DuplicatePolicy::parse(value).map_err(sql_convert)
}

fn parse_algo(value: &str) -> rusqlite::Result<ChecksumAlgo> {
    ChecksumAlgo::parse(value).map_err(sql_convert)
}

fn json_pairs(pairs: &[(String, String)]) -> Result<Option<String>> {
    if pairs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_string(pairs)?))
    }
}

#[derive(Debug, Clone)]
pub struct Credential {
    pub id: Uuid,
    pub host: String,
    pub username: String,
    pub keychain_id: String,
    pub auth_scheme: String,
}

fn credential_from_row(row: &Row<'_>) -> rusqlite::Result<Credential> {
    Ok(Credential {
        id: parse_uuid(row, "id")?,
        host: row.get("host")?,
        username: row.get("username")?,
        keychain_id: row.get("keychain_id")?,
        auth_scheme: row.get("auth_scheme")?,
    })
}

fn parse_headers(raw: Option<String>) -> rusqlite::Result<Vec<(String, String)>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    serde_json::from_str(&raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn opt_u64(value: Option<u64>) -> Result<Option<i64>> {
    value.map(u64_to_i64).transpose()
}

fn opt_u64_col(row: &Row<'_>, column: &str) -> rusqlite::Result<Option<u64>> {
    let value: Option<i64> = row.get(column)?;
    Ok(value.map(i64_to_u64))
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AddRequest;

    fn sample(dir: &Path) -> Download {
        let id = Uuid::new_v4();
        let now = now_ms();
        Download {
            id,
            url: "https://example.com/file.bin".into(),
            final_url: None,
            filename: "file.bin".into(),
            save_dir: dir.to_path_buf(),
            partial_path: dir.join("file.bin.forgepart"),
            queue_id: default_queue_id(),
            priority: 0,
            status: DownloadStatus::Queued,
            resumable: false,
            total_bytes: Some(10),
            downloaded_bytes: 0,
            etag: None,
            last_modified: None,
            content_type: None,
            segment_count: 4,
            user_agent: Some("test".into()),
            referer: None,
            headers: vec![("X-Test".into(), "1".into())],
            cookies: vec![("sid".into(), "1".into())],
            proxy: None,
            credential_id: None,
            speed_limit_bps: None,
            insecure_tls: false,
            checksum_algo: Some(ChecksumAlgo::Sha256),
            checksum_expected: Some("abc".into()),
            error_code: None,
            error_message: None,
            retry_count: 0,
            max_retries: 5,
            duplicate_policy: DuplicatePolicy::Rename,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn roundtrip_and_cascade() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("forge.db");
        let store = Store::open(&db).unwrap();
        assert_eq!(store.setting_u64("default_segments").unwrap(), Some(8));
        let download = sample(dir.path());
        store.insert_download(&download).unwrap();
        let loaded = store.get(download.id).unwrap();
        assert_eq!(loaded.filename, "file.bin");
        assert_eq!(loaded.headers, vec![("X-Test".into(), "1".into())]);
        assert_eq!(loaded.cookies, vec![("sid".into(), "1".into())]);
        assert_eq!(store.setting_string("proxy_mode").unwrap().as_deref(), Some("direct"));
        assert_eq!(loaded.checksum_algo, Some(ChecksumAlgo::Sha256));
        let segment = Segment {
            id: Uuid::new_v4(),
            download_id: download.id,
            start_byte: 0,
            end_byte: Some(10),
            cursor_byte: 0,
            status: SegmentStatus::Pending,
            updated_at: now_ms(),
        };
        store.replace_segments(download.id, &[segment.clone()]).unwrap();
        store.delete_download(download.id).unwrap();
        assert!(store.segments_for(download.id).unwrap().is_empty());
        assert!(store.get(download.id).is_err());
        // Opening again does not duplicate the seeded queue.
        drop(store);
        let again = Store::open(&db).unwrap();
        assert_eq!(again.queue_concurrency(default_queue_id()).unwrap(), 3);
        let _ = AddRequest::new("https://example.com", dir.path());
    }
}
