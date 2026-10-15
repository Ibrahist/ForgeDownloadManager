use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use reqwest::Client;
use tokio::sync::{broadcast, watch, Notify, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use crate::checksum::{self, hashes_match};
use crate::config::{EngineConfig, ResolvedConfig, DEFAULT_USER_AGENT, FILENAME_LOCK_MARK};
use crate::download::{self, SegmentRt};
use crate::error::Error;
use crate::events::EngineEvent;
use crate::filename::{self, partial_name};
use crate::model::{
    now_ms, AddRequest, ChecksumAlgo, Download, DownloadStatus, DuplicatePolicy, Segment,
    SegmentProgress, SegmentStatus,
};
use crate::probe::{self, Probe, RequestMeta};
use crate::segment::plan_segments;
use crate::store::{self, Store};
use crate::writer::SegmentFile;
use crate::Result;

/// One engine owner. Clone shares the scheduler, the database lock, and the event bus.
#[derive(Clone)]
pub struct Engine {
    shared: Arc<Shared>,
}

struct Shared {
    store: Mutex<Store>,
    jobs: Mutex<HashMap<Uuid, Arc<Job>>>,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    /// Held across claim and job insert so pause cannot miss an in-flight start.
    life: Mutex<()>,
    client: Client,
    insecure_client: Client,
    cfg: ResolvedConfig,
    events: broadcast::Sender<EngineEvent>,
    wake: Notify,
    shutdown: CancellationToken,
}

struct Job {
    id: Uuid,
    cancel: CancellationToken,
    pause: AtomicBool,
    segments: Mutex<Vec<Arc<SegmentRt>>>,
    speed: Mutex<SpeedWindow>,
    done_tx: watch::Sender<bool>,
    total_bytes: Mutex<Option<u64>>,
}

struct SpeedWindow {
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedWindow {
    fn observe(&mut self, bytes: u64) -> u64 {
        let now = Instant::now();
        self.samples.push_back((now, bytes));
        while self.samples.len() > 1
            && now.saturating_duration_since(self.samples[0].0) > Duration::from_secs(3)
        {
            self.samples.pop_front();
        }
        let Some(&(started, then)) = self.samples.front() else {
            return 0;
        };
        let elapsed = now.saturating_duration_since(started).as_secs_f64();
        if elapsed < 0.05 {
            return 0;
        }
        ((bytes.saturating_sub(then) as f64) / elapsed) as u64
    }
}

impl Job {
    fn new(id: Uuid) -> Self {
        let (done_tx, _) = watch::channel(false);
        Self {
            id,
            cancel: CancellationToken::new(),
            pause: AtomicBool::new(false),
            segments: Mutex::new(Vec::new()),
            speed: Mutex::new(SpeedWindow { samples: VecDeque::new() }),
            done_tx,
            total_bytes: Mutex::new(None),
        }
    }
}

struct JobGuard {
    shared: Arc<Shared>,
    job: Arc<Job>,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        lock(&self.shared.jobs).remove(&self.job.id);
        let _ = self.job.done_tx.send(true);
        self.shared.wake.notify_one();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

impl Engine {
    pub async fn open(cfg: EngineConfig) -> Result<Self> {
        let store = Store::open(&cfg.db_path)?;
        let recovered = store.requeue_interrupted()?;
        if recovered > 0 {
            tracing::info!("requeued {recovered} interrupted downloads");
        }
        let resolved = resolve_config(&store, &cfg)?;
        let client = build_client(&resolved.user_agent, false)?;
        let insecure_client = build_client(&resolved.user_agent, true)?;
        let (events, _) = broadcast::channel(1024);
        let shared = Arc::new(Shared {
            store: Mutex::new(store),
            jobs: Mutex::new(HashMap::new()),
            hosts: Mutex::new(HashMap::new()),
            life: Mutex::new(()),
            client,
            insecure_client,
            cfg: resolved,
            events,
            wake: Notify::new(),
            shutdown: CancellationToken::new(),
        });
        tokio::spawn(scheduler(Arc::clone(&shared)));
        tokio::spawn(flusher(Arc::clone(&shared)));
        tokio::spawn(progress_loop(Arc::clone(&shared)));
        let engine = Self { shared };
        engine.shared.wake.notify_one();
        Ok(engine)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.shared.events.subscribe()
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shared.shutdown.clone()
    }

    /// Stop the scheduler. Does not cancel active jobs; call [`pause_all`] first.
    pub fn request_shutdown(&self) {
        self.shared.shutdown.cancel();
    }

    pub async fn shutdown(&self) {
        self.request_shutdown();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if lock(&self.shared.jobs).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn add(&self, request: AddRequest) -> Result<Uuid> {
        let id = self.with_store(|store| {
            insert_request(store, &request, self.shared.cfg.default_segments, &self.shared.cfg.user_agent)
        })?;
        let _ = self.shared.events.send(EngineEvent::Added { id });
        if request.start {
            self.shared.wake.notify_one();
        }
        Ok(id)
    }

    pub fn list(&self) -> Result<Vec<Download>> {
        let mut rows = self.with_store(|store| store.list())?;
        let jobs = lock(&self.shared.jobs);
        for download in &mut rows {
            let Some(job) = jobs.get(&download.id) else {
                continue;
            };
            let segments = lock(&job.segments);
            if segments.is_empty() {
                continue;
            }
            download.downloaded_bytes = segments
                .iter()
                .map(|segment| segment.cursor.load(Ordering::Acquire).saturating_sub(segment.start))
                .sum();
        }
        Ok(rows)
    }

    pub fn get(&self, id: Uuid) -> Result<Download> {
        self.list()?.into_iter().find(|download| download.id == id).ok_or(Error::NotFound(id))
    }

    pub fn resolve(&self, prefix: &str) -> Result<Uuid> {
        self.with_store(|store| store.resolve(prefix))
    }

    pub async fn pause(&self, id: Uuid) -> Result<()> {
        let waiting = {
            let _life = lock(&self.shared.life);
            let jobs = lock(&self.shared.jobs);
            jobs.get(&id).map(|job| {
                job.pause.store(true, Ordering::Release);
                job.cancel.cancel();
                job.done_tx.subscribe()
            })
        };
        if let Some(mut watch) = waiting {
            if !*watch.borrow() {
                let _ = watch.changed().await;
            }
            return Ok(());
        }
        let download = self.with_store(|store| store.get(id))?;
        if download.status == DownloadStatus::Completed {
            return Ok(());
        }
        self.with_store(|store| store.set_status(id, DownloadStatus::Paused, None, None))?;
        let _ = self.shared.events.send(EngineEvent::Paused { id });
        Ok(())
    }

    pub async fn pause_all(&self) -> Result<()> {
        let mut waiting = Vec::new();
        {
            let _life = lock(&self.shared.life);
            let queued = self.with_store(|store| store.list())?;
            for download in queued {
                if matches!(download.status, DownloadStatus::Queued | DownloadStatus::Ready) {
                    self.with_store(|store| {
                        store.set_status(download.id, DownloadStatus::Paused, None, None)
                    })?;
                    let _ = self.shared.events.send(EngineEvent::Paused { id: download.id });
                }
            }
            for job in lock(&self.shared.jobs).values() {
                job.pause.store(true, Ordering::Release);
                job.cancel.cancel();
                waiting.push(job.done_tx.subscribe());
            }
        }
        for mut watch in waiting {
            if !*watch.borrow() {
                let _ = watch.changed().await;
            }
        }
        Ok(())
    }

    pub async fn resume(&self, id: Uuid) -> Result<()> {
        {
            let _life = lock(&self.shared.life);
            if lock(&self.shared.jobs).contains_key(&id) {
                return Ok(());
            }
            let download = self.with_store(|store| store.get(id))?;
            match download.status {
                DownloadStatus::Completed => {
                    return Err(Error::InvalidState("download is complete".into()));
                }
                DownloadStatus::Queued | DownloadStatus::Downloading | DownloadStatus::Probing | DownloadStatus::Ready => {
                    return Ok(());
                }
                DownloadStatus::Paused | DownloadStatus::Failed | DownloadStatus::Stopped => {
                    if download.error_code.as_deref() == Some("remote_changed") {
                        return Err(Error::RemoteChanged);
                    }
                    if download.error_code.as_deref() == Some("checksum_mismatch") {
                        return Err(Error::InvalidState(
                            "checksum mismatch; the partial file was kept".into(),
                        ));
                    }
                    self.with_store(|store| store.set_status(id, DownloadStatus::Queued, None, None))?;
                }
            }
        }
        self.shared.wake.notify_one();
        Ok(())
    }

    pub async fn remove(&self, id: Uuid, delete_file: bool) -> Result<()> {
        self.pause(id).await?;
        let download = self.with_store(|store| store.get(id))?;
        let mut deleted = false;
        if delete_file {
            let names = [
                download.partial_path.clone(),
                download.final_path(),
                download.save_dir.join(partial_name(&download.filename)),
            ];
            for path in names {
                if filename::ensure_direct_child(&download.save_dir, &path).is_err() {
                    continue;
                }
                if path.exists() {
                    std::fs::remove_file(&path)?;
                    deleted = true;
                }
            }
        }
        self.with_store(|store| store.delete_download(id))?;
        let _ = self.shared.events.send(EngineEvent::Removed { id, deleted_file: deleted });
        Ok(())
    }

    pub async fn wait_for_terminal(&self, id: Uuid) -> Result<Download> {
        let mut events = self.subscribe();
        loop {
            let download = self.get(id)?;
            if matches!(
                download.status,
                DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Stopped
            ) {
                return Ok(download);
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                _ = events.recv() => {}
            }
        }
    }

    pub async fn wait_until_idle(&self) -> Result<()> {
        loop {
            if !self.list()?.iter().any(|download| download.status.is_active()) {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if !self.list()?.iter().any(|download| download.status.is_active()) {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn with_store<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let store = lock(&self.shared.store);
        f(&store)
    }
}

fn resolve_config(store: &Store, cfg: &EngineConfig) -> Result<ResolvedConfig> {
    let setting = store.setting_u64("max_concurrent_downloads")?.unwrap_or(3) as usize;
    let queue = store.queue_concurrency(store::default_queue_id()).unwrap_or(3);
    let max_concurrent = cfg.max_concurrent_downloads.unwrap_or(setting.min(queue)).max(1);
    let per_host = cfg
        .per_host_connections
        .unwrap_or(store.setting_u64("per_host_connections")?.unwrap_or(16) as usize)
        .max(1);
    let default_segments = cfg
        .default_segments
        .unwrap_or(store.setting_u64("default_segments")?.unwrap_or(8) as u32)
        .clamp(1, 32);
    let min_segment_bytes = cfg.min_segment_bytes.or(store.setting_u64("min_segment_bytes")?).unwrap_or(1_048_576).max(1);
    let user_agent = cfg
        .user_agent
        .clone()
        .or(store.setting_string("user_agent")?)
        .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());
    Ok(ResolvedConfig {
        max_concurrent_downloads: max_concurrent,
        per_host_connections: per_host,
        default_segments,
        min_segment_bytes,
        user_agent,
        flush_interval: cfg.flush_interval,
        progress_interval: cfg.progress_interval,
    })
}

fn build_client(user_agent: &str, insecure: bool) -> Result<Client> {
    // ring is selected here so rustls does not pull aws-lc (that crate needs cmake).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut builder = Client::builder()
        .user_agent(user_agent)
        .redirect(reqwest::redirect::Policy::limited(10))
        .referer(false)
        .no_proxy()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(120))
        .tcp_nodelay(true)
        // Phase 1 has no retry scheduler. The default client would retry a dropped
        // connection and hide the failure the user has to resume.
        .retry(reqwest::retry::never());
    if insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }
    Ok(builder.build()?)
}

/// Insert one download without starting the scheduler. `start: false` is stored as paused.
pub fn insert_request(store: &Store, request: &AddRequest, default_segments: u32, default_ua: &str) -> Result<Uuid> {
    let url = validate_http_url(&request.url)?;
    filename::validate_headers(&request.headers)?;
    if request.insecure_tls {
        tracing::warn!("TLS certificate verification is disabled for this download ({url})");
    }
    std::fs::create_dir_all(&request.save_dir)?;
    let save_dir = request.save_dir.canonicalize().map_err(|_| Error::PathEscape)?;
    let raw_name = match &request.filename {
        Some(name) => name.clone(),
        None => filename::filename_from_url(&url).unwrap_or_else(|| "download".to_string()),
    };
    let filename = filename::allocate_unique(&save_dir, &raw_name, request.duplicate_policy)?;
    let partial = save_dir.join(partial_name(&filename));
    filename::ensure_direct_child(&save_dir, &partial)?;
    let segment_count = request.segments.unwrap_or(default_segments).clamp(1, 32);
    let user_agent = request.user_agent.clone().unwrap_or_else(|| default_ua.to_string());
    let now = now_ms();
    let id = Uuid::new_v4();
    let download = Download {
        id,
        url: url.to_string(),
        final_url: None,
        filename,
        save_dir,
        partial_path: partial,
        queue_id: store::default_queue_id(),
        priority: 0,
        status: if request.start { DownloadStatus::Queued } else { DownloadStatus::Paused },
        resumable: false,
        total_bytes: None,
        downloaded_bytes: 0,
        etag: None,
        last_modified: None,
        content_type: None,
        segment_count,
        user_agent: Some(user_agent),
        referer: request.referer.clone(),
        headers: request.headers.clone(),
        insecure_tls: request.insecure_tls,
        checksum_algo: request.checksum_algo,
        checksum_expected: request.checksum_expected.clone(),
        error_code: None,
        error_message: None,
        retry_count: 0,
        max_retries: 5,
        duplicate_policy: request.duplicate_policy,
        created_at: now,
        updated_at: now,
        started_at: None,
        completed_at: None,
    };
    store.insert_download(&download)?;
    if request.filename.is_some() {
        store.log(id, "info", FILENAME_LOCK_MARK)?;
    }
    Ok(id)
}

fn validate_http_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|err| Error::InvalidUrl(err.to_string()))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(Error::InvalidUrl("only http and https are supported in this phase".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::InvalidUrl(
            "URL userinfo is not accepted; authentication is a later phase".into(),
        ));
    }
    if url.host_str().is_none() {
        return Err(Error::InvalidUrl("URL has no host".into()));
    }
    Ok(url)
}

async fn scheduler(shared: Arc<Shared>) {
    loop {
        tokio::select! {
            _ = shared.shutdown.cancelled() => break,
            _ = shared.wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
        if shared.shutdown.is_cancelled() {
            break;
        }
        if let Err(err) = pump(Arc::clone(&shared)) {
            tracing::error!("scheduler: {err}");
        }
    }
}

fn pump(shared: Arc<Shared>) -> Result<()> {
    // life → store → jobs. The job is in the map before the worker is spawned.
    let _life = lock(&shared.life);
    let ids = {
        let store = lock(&shared.store);
        let active = store.count_active()?;
        let slots = shared.cfg.max_concurrent_downloads.saturating_sub(active);
        store.claim_queued(slots)?
    };
    let mut started = Vec::new();
    {
        let mut jobs = lock(&shared.jobs);
        for id in ids {
            let job = Arc::new(Job::new(id));
            jobs.insert(id, Arc::clone(&job));
            started.push(job);
        }
    }
    for job in started {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move { run_job(shared, job).await });
    }
    Ok(())
}

async fn run_job(shared: Arc<Shared>, job: Arc<Job>) {
    let _guard = JobGuard { shared: Arc::clone(&shared), job: Arc::clone(&job) };
    let outcome = if job.pause.load(Ordering::Acquire) || job.cancel.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        execute(&shared, &job).await
    };
    match outcome {
        Ok(DownloadStatus::Completed) => {}
        Ok(DownloadStatus::Paused) | Err(Error::Cancelled) => {
            if let Err(err) = mark_paused(&shared, &job).await {
                tracing::warn!("pause {}: {err}", job.id);
            }
        }
        Ok(status) => {
            tracing::debug!("job {} finished as {}", job.id, status.as_str());
        }
        Err(err) => {
            if job.pause.load(Ordering::Acquire) {
                if let Err(mark) = mark_paused(&shared, &job).await {
                    tracing::warn!("pause {}: {mark}", job.id);
                }
            } else if let Err(mark) = mark_failed(&shared, &job, &err).await {
                tracing::warn!("fail {}: {mark}", job.id);
            }
        }
    }
}

async fn mark_paused(shared: &Shared, job: &Job) -> Result<()> {
    if let Err(err) = persist_job(shared, job).await {
        tracing::warn!("persist before pause failed: {err}");
    }
    let status = lock(&shared.store).get(job.id)?.status;
    if status == DownloadStatus::Completed {
        return Ok(());
    }
    lock(&shared.store).set_status(job.id, DownloadStatus::Paused, None, None)?;
    let _ = shared.events.send(EngineEvent::Paused { id: job.id });
    Ok(())
}

async fn mark_failed(shared: &Shared, job: &Job, err: &Error) -> Result<()> {
    if let Err(persist) = persist_job(shared, job).await {
        tracing::warn!("persist before failure failed: {persist}");
    }
    let status = lock(&shared.store).get(job.id)?.status;
    if status == DownloadStatus::Completed {
        return Ok(());
    }
    lock(&shared.store).set_status(job.id, DownloadStatus::Failed, Some(err.code()), Some(&err.to_string()))?;
    let _ = shared.events.send(EngineEvent::Failed {
        id: job.id,
        reason: err.code().to_string(),
        message: err.to_string(),
    });
    Ok(())
}

async fn execute(shared: &Shared, job: &Job) -> Result<DownloadStatus> {
    let mut download = lock(&shared.store).get(job.id)?;
    if !matches!(
        download.status,
        DownloadStatus::Downloading | DownloadStatus::Probing | DownloadStatus::Queued
    ) {
        return Ok(download.status);
    }
    lock(&shared.store).set_status(job.id, DownloadStatus::Probing, None, None)?;
    let meta = RequestMeta {
        user_agent: download.user_agent.clone().unwrap_or_else(|| shared.cfg.user_agent.clone()),
        referer: download.referer.clone(),
        headers: download.headers.clone(),
    };
    let client = if download.insecure_tls { &shared.insecure_client } else { &shared.client };
    let target = download.final_url.clone().filter(|url| !url.is_empty()).unwrap_or_else(|| download.url.clone());
    let host = host_semaphore(shared, &target)?;
    let probe = {
        let _permit = download::acquire_host(&host, &job.cancel).await?;
        probe::probe_url(client, &target, &meta).await?
    };
    if job.pause.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }

    let mut segments = lock(&shared.store).segments_for(job.id)?;
    let mut has_progress = progressed(&download, &segments);
    if has_progress {
        probe::ensure_same_remote(
            download.etag.as_deref(),
            download.last_modified.as_deref(),
            download.total_bytes,
            &probe,
        )?;
        if !download.partial_path.exists() && download.final_path().exists() && fully_downloaded(&segments) {
            return complete_existing(shared, &download, &download.final_path()).await;
        }
        if !download.partial_path.exists() {
            tracing::warn!("partial missing for {}; downloading again", job.id);
            for segment in &mut segments {
                segment.cursor_byte = segment.start_byte;
                segment.status = SegmentStatus::Pending;
            }
            let rows: Vec<(Uuid, u64, SegmentStatus)> = segments
                .iter()
                .map(|segment| (segment.id, segment.cursor_byte, segment.status))
                .collect();
            let store = lock(&shared.store);
            if !segments.is_empty() {
                store.replace_segments(job.id, &segments)?;
            }
            store.update_cursors(job.id, 0, &rows)?;
            download.downloaded_bytes = 0;
            has_progress = false;
        }
    }
    if has_progress && !probe.resumable {
        return Err(Error::NotResumable);
    }

    let filename = choose_filename(shared, &download, &probe, &segments, has_progress)?;
    let partial_path = download.save_dir.join(partial_name(&filename));
    filename::ensure_direct_child(&download.save_dir, &partial_path)?;
    if probe.total_bytes == Some(0) {
        SegmentFile::create(&partial_path, Some(0))?;
        remember_probe(shared, job.id, &probe, &filename, &partial_path, 1)?;
        download = lock(&shared.store).get(job.id)?;
        return finalize(shared, job, &download).await;
    }

    if segments.is_empty() {
        segments = plan_new_segments(shared, job.id, &download, &probe)?;
    }
    let segment_count = u32::try_from(segments.len()).unwrap_or(1).max(1);
    remember_probe(shared, job.id, &probe, &filename, &partial_path, segment_count)?;
    let _ = shared.events.send(EngineEvent::Probed {
        id: job.id,
        final_url: probe.final_url.clone(),
        total_bytes: probe.total_bytes,
        resumable: probe.resumable,
        content_type: probe.content_type.clone(),
        filename: filename.clone(),
    });
    download = lock(&shared.store).get(job.id)?;
    if fully_downloaded(&segments) && download.partial_path.exists() {
        return finalize(shared, job, &download).await;
    }

    let known_total = probe.total_bytes.or(download.total_bytes);
    let fresh = segments.iter().all(|segment| segment.cursor_byte <= segment.start_byte);
    let mut runtime = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let preallocate = if index == 0 { known_total } else { None };
        let file = SegmentFile::create(&partial_path, preallocate)?;
        if index == 0 && fresh {
            if let Some(total) = known_total {
                std::fs::OpenOptions::new().write(true).open(&partial_path)?.set_len(total)?;
            }
        }
        runtime.push(Arc::new(SegmentRt {
            id: segment.id,
            start: segment.start_byte,
            end: segment.end_byte,
            cursor: AtomicU64::new(segment.cursor_byte),
            eof: AtomicBool::new(false),
            file,
        }));
    }
    *lock(&job.segments) = runtime.clone();
    *lock(&job.total_bytes) = known_total;
    lock(&shared.store).set_status(job.id, DownloadStatus::Downloading, None, None)?;
    if has_progress {
        let _ = shared.events.send(EngineEvent::Resumed { id: job.id });
    } else {
        let _ = shared.events.send(EngineEvent::Started { id: job.id });
    }
    if job.pause.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }

    let transfer = if !probe.resumable {
        let segment = runtime.first().ok_or_else(|| Error::Failed("no segment".into()))?;
        if segment.end.is_none() {
            download::fetch_until_eof(client, &meta, &probe.final_url, segment, &job.cancel, &host).await?;
            let cursor = segment.cursor.load(Ordering::Acquire);
            lock(&shared.store).finish_open_ended(job.id, segment.id, cursor)?;
        } else {
            download::fetch_bounded(client, &meta, &probe.final_url, segment, &job.cancel, &host, false).await?;
        }
        Ok(())
    } else {
        join_ranges(client, &meta, &probe.final_url, &runtime, &job.cancel, host, &job.pause).await
    };
    transfer?;
    if job.pause.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    for segment in &runtime {
        let cursor = segment.cursor.load(Ordering::Acquire);
        if let Some(end) = segment.end {
            if cursor < end {
                return Err(Error::Incomplete);
            }
        } else if !segment.eof.load(Ordering::Acquire) {
            return Err(Error::Incomplete);
        }
    }
    persist_job(shared, job).await?;
    download = lock(&shared.store).get(job.id)?;
    finalize(shared, job, &download).await
}

fn progressed(download: &Download, segments: &[Segment]) -> bool {
    download.downloaded_bytes > 0 || segments.iter().any(|segment| segment.cursor_byte > segment.start_byte)
}

fn fully_downloaded(segments: &[Segment]) -> bool {
    !segments.is_empty()
        && segments.iter().all(|segment| segment.end_byte.is_some_and(|end| segment.cursor_byte >= end))
}

fn choose_filename(
    shared: &Shared,
    download: &Download,
    probe: &Probe,
    segments: &[Segment],
    has_progress: bool,
) -> Result<String> {
    let locked = lock(&shared.store).has_log(download.id, FILENAME_LOCK_MARK)?;
    if locked || !segments.is_empty() || has_progress || download.partial_path.exists() {
        return Ok(download.filename.clone());
    }
    let mut filename = download.filename.clone();
    if let Some(suggested) = &probe.filename {
        filename = filename::allocate_unique(&download.save_dir, suggested, download.duplicate_policy)?;
    }
    if !filename.contains('.') {
        if let Some(ext) = probe.content_type.as_deref().and_then(filename::extension_for_mime) {
            filename = filename::allocate_unique(
                &download.save_dir,
                &format!("{filename}.{ext}"),
                download.duplicate_policy,
            )?;
        }
    }
    Ok(filename)
}

fn plan_new_segments(shared: &Shared, id: Uuid, download: &Download, probe: &Probe) -> Result<Vec<Segment>> {
    let rows = if probe.resumable {
        let total = probe.total_bytes.ok_or_else(|| Error::Failed("resumable probe has no size".into()))?;
        plan_segments(total, download.segment_count, shared.cfg.min_segment_bytes)
            .into_iter()
            .map(|range| new_segment(id, range.start, Some(range.end)))
            .collect()
    } else if let Some(total) = probe.total_bytes {
        vec![new_segment(id, 0, Some(total))]
    } else {
        vec![new_segment(id, 0, None)]
    };
    lock(&shared.store).replace_segments(id, &rows)?;
    Ok(rows)
}

fn new_segment(download_id: Uuid, start: u64, end: Option<u64>) -> Segment {
    Segment {
        id: Uuid::new_v4(),
        download_id,
        start_byte: start,
        end_byte: end,
        cursor_byte: start,
        status: SegmentStatus::Pending,
        updated_at: now_ms(),
    }
}

fn remember_probe(
    shared: &Shared,
    id: Uuid,
    probe: &Probe,
    filename: &str,
    partial_path: &Path,
    segment_count: u32,
) -> Result<()> {
    lock(&shared.store).update_probe(
        id,
        &probe.final_url,
        filename,
        partial_path,
        probe.resumable,
        probe.total_bytes,
        probe.etag.as_deref(),
        probe.last_modified.as_deref(),
        probe.content_type.as_deref(),
        segment_count,
    )
}

async fn join_ranges(
    client: &Client,
    meta: &RequestMeta,
    url: &str,
    segments: &[Arc<SegmentRt>],
    cancel: &CancellationToken,
    host: Arc<Semaphore>,
    pause: &AtomicBool,
) -> Result<()> {
    let mut set = JoinSet::new();
    for segment in segments {
        let done = segment.end.is_some_and(|end| segment.cursor.load(Ordering::Acquire) >= end);
        if done {
            continue;
        }
        let segment = Arc::clone(segment);
        let client = client.clone();
        let meta = meta.clone();
        let url = url.to_string();
        let cancel = cancel.clone();
        let host = Arc::clone(&host);
        set.spawn(async move {
            download::fetch_bounded(&client, &meta, &url, &segment, &cancel, &host, true).await
        });
    }
    let mut failure: Option<Error> = None;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(Error::Cancelled)) => {
                if failure.is_none() {
                    failure = Some(Error::Cancelled);
                }
            }
            Ok(Err(err)) => {
                if failure.as_ref().map(|err| matches!(err, Error::Cancelled)).unwrap_or(true) {
                    failure = Some(err);
                }
                if !pause.load(Ordering::Acquire) {
                    cancel.cancel();
                }
            }
            Err(err) => {
                failure = Some(Error::Join(err));
                cancel.cancel();
            }
        }
    }
    match failure {
        None => Ok(()),
        Some(Error::Cancelled) if pause.load(Ordering::Acquire) => Err(Error::Cancelled),
        Some(Error::Cancelled) => Err(Error::Failed("download cancelled".into())),
        Some(err) => Err(err),
    }
}

async fn finalize(shared: &Shared, job: &Job, download: &Download) -> Result<DownloadStatus> {
    let sha = hash_path(&download.partial_path, ChecksumAlgo::Sha256).await?;
    verify_checksum(download, &sha, &download.partial_path).await?;
    let (filename, path) = place_final(download)?;
    {
        let store = lock(&shared.store);
        store.set_file_location(download.id, &filename, &path)?;
        store.set_status(download.id, DownloadStatus::Completed, None, None)?;
    }
    let _ = shared.events.send(EngineEvent::Completed { id: job.id, path, sha256: sha });
    Ok(DownloadStatus::Completed)
}

async fn complete_existing(shared: &Shared, download: &Download, path: &Path) -> Result<DownloadStatus> {
    let sha = hash_path(path, ChecksumAlgo::Sha256).await?;
    verify_checksum(download, &sha, path).await?;
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| download.filename.clone());
    {
        let store = lock(&shared.store);
        store.set_file_location(download.id, &filename, path)?;
        store.set_status(download.id, DownloadStatus::Completed, None, None)?;
    }
    let _ = shared.events.send(EngineEvent::Completed {
        id: download.id,
        path: path.to_path_buf(),
        sha256: sha,
    });
    Ok(DownloadStatus::Completed)
}

async fn verify_checksum(download: &Download, sha256: &str, path: &Path) -> Result<()> {
    let (Some(algo), Some(expected)) = (download.checksum_algo, download.checksum_expected.as_deref()) else {
        return Ok(());
    };
    let actual = if algo == ChecksumAlgo::Sha256 {
        sha256.to_string()
    } else {
        hash_path(path, algo).await?
    };
    if hashes_match(expected, &actual) {
        Ok(())
    } else {
        Err(Error::ChecksumMismatch { expected: expected.to_string(), actual })
    }
}

fn place_final(download: &Download) -> Result<(String, PathBuf)> {
    filename::ensure_direct_child(&download.save_dir, &download.partial_path)?;
    let mut filename = download.filename.clone();
    let mut dest = download.save_dir.join(&filename);
    filename::ensure_direct_child(&download.save_dir, &dest)?;
    if dest.exists() {
        match download.duplicate_policy {
            DuplicatePolicy::Overwrite => std::fs::remove_file(&dest)?,
            DuplicatePolicy::Ask => return Err(Error::DuplicateAsk),
            DuplicatePolicy::Rename => {
                filename = filename::allocate_unique(&download.save_dir, &filename, DuplicatePolicy::Rename)?;
                dest = download.save_dir.join(&filename);
                filename::ensure_direct_child(&download.save_dir, &dest)?;
            }
        }
    }
    if dest.exists() {
        std::fs::remove_file(&dest)?;
    }
    std::fs::rename(&download.partial_path, &dest)?;
    Ok((filename, dest))
}

async fn hash_path(path: &Path, algo: ChecksumAlgo) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || checksum::hash_file(&path, algo)).await?
}

/// Sync segment files, then store the cursors captured before that sync.
///
/// A crash can repeat a suffix. Recording a cursor read after `sync_data` could
/// mark bytes durable that never reached disk, and resume would leave a hole.
async fn persist_job(shared: &Shared, job: &Job) -> Result<()> {
    let snapshot: Vec<(Uuid, u64, u64, SegmentStatus, SegmentFile)> = {
        let segments = lock(&job.segments);
        if segments.is_empty() {
            return Ok(());
        }
        segments
            .iter()
            .map(|segment| {
                let cursor = segment.cursor.load(Ordering::Acquire);
                let done = segment.eof.load(Ordering::Acquire)
                    || segment.end.is_some_and(|end| cursor >= end);
                let status = if done { SegmentStatus::Done } else { SegmentStatus::Active };
                (segment.id, segment.start, cursor, status, segment.file.clone())
            })
            .collect()
    };
    let downloaded = snapshot
        .iter()
        .map(|(_, start, cursor, _, _)| cursor.saturating_sub(*start))
        .sum();
    let files: Vec<SegmentFile> = snapshot.iter().map(|(_, _, _, _, file)| file.clone()).collect();
    tokio::task::spawn_blocking(move || {
        for file in files {
            file.sync()?;
        }
        Ok::<(), Error>(())
    })
    .await??;
    let rows: Vec<(Uuid, u64, SegmentStatus)> = snapshot
        .into_iter()
        .map(|(id, _, cursor, status, _)| (id, cursor, status))
        .collect();
    lock(&shared.store).update_cursors(job.id, downloaded, &rows)
}

fn host_semaphore(shared: &Shared, url: &str) -> Result<Arc<Semaphore>> {
    let key = host_key(url)?;
    let mut hosts = lock(&shared.hosts);
    let permits = shared.cfg.per_host_connections.max(1);
    let entry = hosts.entry(key).or_insert_with(|| Arc::new(Semaphore::new(permits)));
    Ok(Arc::clone(entry))
}

fn host_key(url: &str) -> Result<String> {
    let url = Url::parse(url).map_err(|err| Error::InvalidUrl(err.to_string()))?;
    let host = url.host_str().ok_or_else(|| Error::InvalidUrl("URL has no host".into()))?;
    let port = url.port_or_known_default().unwrap_or(0);
    Ok(format!("{}|{host}|{port}", url.scheme()))
}

async fn flusher(shared: Arc<Shared>) {
    loop {
        tokio::select! {
            _ = shared.shutdown.cancelled() => break,
            _ = tokio::time::sleep(shared.cfg.flush_interval) => {}
        }
        if shared.shutdown.is_cancelled() {
            break;
        }
        let jobs: Vec<Arc<Job>> = lock(&shared.jobs).values().cloned().collect();
        for job in jobs {
            if let Err(err) = persist_job(&shared, &job).await {
                tracing::debug!("flush {}: {err}", job.id);
            }
        }
    }
}

async fn progress_loop(shared: Arc<Shared>) {
    loop {
        tokio::select! {
            _ = shared.shutdown.cancelled() => break,
            _ = tokio::time::sleep(shared.cfg.progress_interval) => {}
        }
        if shared.shutdown.is_cancelled() {
            break;
        }
        let jobs: Vec<Arc<Job>> = lock(&shared.jobs).values().cloned().collect();
        for job in jobs {
            emit_progress(&shared, &job);
        }
    }
}

fn emit_progress(shared: &Shared, job: &Job) {
    let segments = lock(&job.segments);
    if segments.is_empty() {
        return;
    }
    let downloaded = segments
        .iter()
        .map(|segment| segment.cursor.load(Ordering::Acquire).saturating_sub(segment.start))
        .sum();
    let view: Vec<SegmentProgress> = segments
        .iter()
        .map(|segment| SegmentProgress {
            start: segment.start,
            end: segment.end,
            cursor: segment.cursor.load(Ordering::Acquire),
        })
        .collect();
    drop(segments);
    let speed = lock(&job.speed).observe(downloaded);
    let total = *lock(&job.total_bytes);
    let eta_ms = total.and_then(|total| {
        if speed == 0 || downloaded >= total {
            None
        } else {
            Some(total.saturating_sub(downloaded).saturating_mul(1000) / speed)
        }
    });
    let _ = shared.events.send(EngineEvent::Progress {
        id: job.id,
        downloaded_bytes: downloaded,
        total_bytes: total,
        speed_bps: speed,
        eta_ms,
        segments: view,
    });
}
