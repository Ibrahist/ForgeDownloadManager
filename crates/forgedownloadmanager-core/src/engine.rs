use std::collections::{HashMap, HashSet, VecDeque};
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

use crate::auth::{self, AuthMaterial, AuthScheme};
use crate::checksum::{self, hashes_match};
use crate::config::{EngineConfig, ResolvedConfig, DEFAULT_USER_AGENT, FILENAME_LOCK_MARK};
use crate::download::{self, Fetch, SegmentRt};
use crate::error::Error;
use crate::events::EngineEvent;
use crate::filename::{self, partial_name};
use crate::limiter::{self, Pace, TokenBucket};
use crate::model::{
    now_ms, AddRequest, ChecksumAlgo, Download, DownloadStatus, DuplicatePolicy, Segment,
    SegmentProgress, SegmentStatus,
};
use crate::probe::{self, Probe, RequestMeta};
use crate::proxy::{self, ResolvedProxy};
use crate::retry;
use crate::secrets::{KeyringSecrets, SecretStore};
use crate::segment::{plan_segments, split_remainder};
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
    clients: Mutex<HashMap<String, Client>>,
    global_limit: Arc<Mutex<TokenBucket>>,
    secrets: Arc<dyn SecretStore>,
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
        let secrets = cfg.secrets.clone().unwrap_or_else(|| Arc::new(KeyringSecrets));
        let limit = TokenBucket::new(resolved.speed_limit_bps);
        let (events, _) = broadcast::channel(1024);
        let shared = Arc::new(Shared {
            store: Mutex::new(store),
            jobs: Mutex::new(HashMap::new()),
            hosts: Mutex::new(HashMap::new()),
            life: Mutex::new(()),
            clients: Mutex::new(HashMap::new()),
            global_limit: Arc::new(Mutex::new(limit)),
            secrets,
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
        let secrets = Arc::clone(&self.shared.secrets);
        let id = self.with_store(|store| {
            insert_request(
                store,
                &request,
                self.shared.cfg.default_segments,
                &self.shared.cfg.user_agent,
                secrets.as_ref(),
            )
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

    /// Persisted segments, with in-memory cursor and end overlaid while the job is running.
    /// After pause the job leaves the map, so the returned cursors are the last flush.
    pub fn segments(&self, id: Uuid) -> Result<Vec<Segment>> {
        let mut rows = self.with_store(|store| store.segments_for(id))?;
        let jobs = lock(&self.shared.jobs);
        if let Some(job) = jobs.get(&id) {
            let live = lock(&job.segments);
            for row in &mut rows {
                if let Some(segment) = live.iter().find(|segment| segment.id == row.id) {
                    row.cursor_byte = segment.cursor.load(Ordering::Acquire);
                    if let Some(end) = segment.live_end() {
                        row.end_byte = Some(end);
                    }
                }
            }
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

    pub fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        self.with_store(|store| store.set_setting(key, value))
    }

    pub fn setting_value(&self, key: &str) -> Result<Option<serde_json::Value>> {
        self.with_store(|store| store.setting_value(key))
    }

    /// Store a password in the keychain and remember the username for `host`.
    pub fn save_credential(&self, host: &str, username: &str, password: &str) -> Result<Uuid> {
        let secrets = Arc::clone(&self.shared.secrets);
        self.with_store(|store| {
            let credential = store.upsert_credential(host, username, "any")?;
            secrets.set_secret(&credential.keychain_id, password)?;
            Ok(credential.id)
        })
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
    let speed_limit_bps = match cfg.speed_limit_bps {
        Some(limit) => limit,
        None => store
            .setting_value("speed_limit_bps")?
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
    };
    Ok(ResolvedConfig {
        max_concurrent_downloads: max_concurrent,
        per_host_connections: per_host,
        default_segments,
        min_segment_bytes,
        user_agent,
        speed_limit_bps,
        flush_interval: cfg.flush_interval,
        progress_interval: cfg.progress_interval,
    })
}

fn build_client(user_agent: &str, insecure: bool, proxy: &ResolvedProxy) -> Result<Client> {
    // ring is selected here so rustls does not pull aws-lc (that crate needs cmake).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut builder = Client::builder()
        .user_agent(user_agent)
        .redirect(reqwest::redirect::Policy::limited(10))
        .referer(false)
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(120))
        .tcp_nodelay(true)
        // Retries belong to the engine. reqwest's own retry would hide a disconnect.
        .retry(reqwest::retry::never());
    builder = match proxy {
        ResolvedProxy::Direct => builder.no_proxy(),
        ResolvedProxy::All(url) => builder.proxy(reqwest::Proxy::all(url)?),
        ResolvedProxy::Https(url) => builder.proxy(reqwest::Proxy::https(url)?),
    };
    if insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }
    Ok(builder.build()?)
}

fn client_for(shared: &Shared, download: &Download) -> Result<Client> {
    let target = Url::parse(download.final_url.as_deref().unwrap_or(&download.url))
        .or_else(|_| Url::parse(&download.url))
        .map_err(|err| Error::InvalidUrl(err.to_string()))?;
    let proxy = resolve_download_proxy(shared, download, &target)?;
    let key = format!("{}|{}", download.insecure_tls, proxy.cache_key());
    let mut clients = lock(&shared.clients);
    if let Some(client) = clients.get(&key) {
        return Ok(client.clone());
    }
    let client = build_client(&shared.cfg.user_agent, download.insecure_tls, &proxy)?;
    clients.insert(key, client.clone());
    Ok(client)
}

fn resolve_download_proxy(shared: &Shared, download: &Download, target: &Url) -> Result<ResolvedProxy> {
    if let Some(raw) = download.proxy.as_deref() {
        let over = proxy::parse_override(raw)?;
        return proxy::resolve_proxy(&over.mode, over.url.as_deref(), target, env_var);
    }
    let store = lock(&shared.store);
    let mode = store.setting_string("proxy_mode")?.unwrap_or_else(|| "direct".into());
    let configured = match store.setting_value("proxy_url")? {
        Some(serde_json::Value::String(url)) => Some(url),
        _ => None,
    };
    proxy::resolve_proxy(&mode, configured.as_deref(), target, env_var)
}

/// Insert one download without starting the scheduler. `start: false` is stored as paused.
pub fn insert_request(
    store: &Store,
    request: &AddRequest,
    default_segments: u32,
    default_ua: &str,
    secrets: &dyn SecretStore,
) -> Result<Uuid> {
    let url = validate_url(&request.url)?;
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
    let credential_id = store_credential(store, secrets, &url, request)?;
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
        cookies: request.cookies.clone(),
        proxy: request.proxy.clone(),
        credential_id,
        speed_limit_bps: request.speed_limit_bps,
        insecure_tls: request.insecure_tls,
        checksum_algo: request.checksum_algo,
        checksum_expected: request.checksum_expected.clone(),
        error_code: None,
        error_message: None,
        retry_count: 0,
        max_retries: request.max_retries.unwrap_or(5),
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

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn validate_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|err| Error::InvalidUrl(err.to_string()))?;
    if !matches!(url.scheme(), "http" | "https" | "ftp" | "ftps") {
        return Err(Error::InvalidUrl("only http, https, ftp, and ftps are supported".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::InvalidUrl(
            "URL userinfo is not accepted; use --user and --password-stdin".into(),
        ));
    }
    if url.host_str().is_none() {
        return Err(Error::InvalidUrl("URL has no host".into()));
    }
    Ok(url)
}

fn store_credential(store: &Store, secrets: &dyn SecretStore, url: &Url, request: &AddRequest) -> Result<Option<Uuid>> {
    let host = auth::credential_host(url);
    match (&request.username, &request.password) {
        (Some(username), Some(password)) => {
            let credential = store.upsert_credential(&host, username, "any")?;
            secrets.set_secret(&credential.keychain_id, password)?;
            Ok(Some(credential.id))
        }
        (Some(username), None) => {
            let credential = store.find_credential(&host, username)?.ok_or_else(|| {
                Error::Failed("no stored credential for this host; pass --password-stdin".into())
            })?;
            Ok(Some(credential.id))
        }
        (None, Some(_)) => Err(Error::Failed("--password-stdin requires --user".into())),
        (None, None) => Ok(None),
    }
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

async fn execute(shared: &Arc<Shared>, job: &Job) -> Result<DownloadStatus> {
    let mut download = lock(&shared.store).get(job.id)?;
    if !matches!(
        download.status,
        DownloadStatus::Downloading | DownloadStatus::Probing | DownloadStatus::Queued
    ) {
        return Ok(download.status);
    }
    lock(&shared.store).set_status(job.id, DownloadStatus::Probing, None, None)?;
    let target_url = download_target(&download)?;
    let wire = if matches!(target_url.scheme(), "ftp" | "ftps") {
        let (username, password) = ftp_identity(shared, &download)?;
        let info = crate::ftp::probe_size(
            &target_url,
            download.insecure_tls,
            username.as_deref(),
            password.as_deref(),
        )
        .await?;
        Wire::Ftp {
            probe: Probe {
                final_url: target_url.to_string(),
                total_bytes: info.total_bytes,
                resumable: info.total_bytes.is_some(),
                etag: None,
                last_modified: None,
                content_type: None,
                filename: None,
            },
            url: target_url,
            username,
            password,
        }
    } else {
        let meta = request_meta(shared, &download)?;
        let client = client_for(shared, &download)?;
        let target = target_url.to_string();
        let host = host_semaphore(shared, &target)?;
        let probe = {
            let _permit = download::acquire_host(&host, &job.cancel).await?;
            probe::probe_url(&client, &target, &meta).await?
        };
        Wire::Http { probe, client, meta }
    };
    if job.pause.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    let probe = wire.probe().clone();

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
        segments = if matches!(wire, Wire::Ftp { .. }) {
            // FTP is one REST stream. A parallel range plan would leave a hole.
            plan_single_segment(shared, job.id, &probe)?
        } else {
            plan_new_segments(shared, job.id, &download, &probe)?
        };
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
            bounded: segment.end_byte.is_some(),
            end: AtomicU64::new(segment.end_byte.unwrap_or(u64::MAX)),
            cursor: AtomicU64::new(segment.cursor_byte),
            eof: AtomicBool::new(false),
            file,
            gate: Mutex::new(()),
        }));
    }
    *lock(&job.segments) = runtime;
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

    let pace = pace_for(shared, &download)?;
    let hook = retry_hook(shared, job.id);
    let host = host_semaphore(shared, &probe.final_url)?;
    let transfer = match &wire {
        Wire::Ftp { url, username, password, .. } => {
            let segment = first_segment(job)?;
            transfer_ftp(
                url,
                download.insecure_tls,
                username.clone(),
                password.clone(),
                &segment,
                &pace,
                &job.cancel,
                &host,
                download.max_retries,
                hook.as_ref(),
            )
            .await?;
            if !segment.bounded {
                let cursor = segment.cursor.load(Ordering::Acquire);
                lock(&shared.store).finish_open_ended(job.id, segment.id, cursor)?;
            }
            Ok(())
        }
        Wire::Http { client, meta, .. } => {
            if probe.resumable {
                join_ranges(
                    shared,
                    job,
                    client,
                    meta,
                    &probe.final_url,
                    &job.cancel,
                    host,
                    &job.pause,
                    &pace,
                    download.max_retries,
                    &partial_path,
                    &hook,
                )
                .await
            } else {
                let segment = first_segment(job)?;
                let fetch = Fetch {
                    client,
                    meta,
                    url: &probe.final_url,
                    cancel: &job.cancel,
                    host: host.as_ref(),
                    pace: &pace,
                    max_retries: download.max_retries,
                    on_retry: hook.as_ref(),
                };
                if segment.bounded {
                    download::fetch_bounded(&fetch, &segment, false).await?;
                } else {
                    download::fetch_until_eof(&fetch, &segment).await?;
                    let cursor = segment.cursor.load(Ordering::Acquire);
                    lock(&shared.store).finish_open_ended(job.id, segment.id, cursor)?;
                }
                Ok(())
            }
        }
    };
    transfer?;
    if job.pause.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    // Tails created by a split live on the job, not in the original plan.
    let unfinished = lock(&job.segments).iter().any(|segment| !segment.finished());
    if unfinished {
        return Err(Error::Incomplete);
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

fn plan_single_segment(shared: &Shared, id: Uuid, probe: &Probe) -> Result<Vec<Segment>> {
    let rows = match probe.total_bytes {
        Some(total) => vec![new_segment(id, 0, Some(total))],
        None => vec![new_segment(id, 0, None)],
    };
    lock(&shared.store).replace_segments(id, &rows)?;
    Ok(rows)
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
    shared: &Shared,
    job: &Job,
    client: &Client,
    meta: &RequestMeta,
    url: &str,
    cancel: &CancellationToken,
    host: Arc<Semaphore>,
    pause: &AtomicBool,
    pace: &Pace,
    max_retries: u32,
    partial: &Path,
    on_retry: &Arc<dyn Fn(u32) + Send + Sync>,
) -> Result<()> {
    let mut set = JoinSet::new();
    let mut inflight = HashSet::new();
    schedule_idle(&mut set, &mut inflight, job, client, meta, url, cancel, &host, pace, max_retries, on_retry);
    let mut failure: Option<Error> = None;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((id, Ok(()))) => {
                inflight.remove(&id);
                // The finished attempt has dropped its host permit, so a free slot can take a tail.
                if failure.is_none() && host.available_permits() > 0 {
                    let _ = try_split(shared, job, partial);
                }
            }
            Ok((id, Err(Error::Cancelled))) => {
                inflight.remove(&id);
                if failure.is_none() {
                    failure = Some(Error::Cancelled);
                }
            }
            Ok((id, Err(err))) => {
                inflight.remove(&id);
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
        if failure.is_none() {
            schedule_idle(&mut set, &mut inflight, job, client, meta, url, cancel, &host, pace, max_retries, on_retry);
        }
    }
    match failure {
        None => Ok(()),
        Some(Error::Cancelled) if pause.load(Ordering::Acquire) => Err(Error::Cancelled),
        Some(Error::Cancelled) => Err(Error::Failed("download cancelled".into())),
        Some(err) => Err(err),
    }
}

fn schedule_idle(
    set: &mut JoinSet<(Uuid, Result<()>)>,
    inflight: &mut HashSet<Uuid>,
    job: &Job,
    client: &Client,
    meta: &RequestMeta,
    url: &str,
    cancel: &CancellationToken,
    host: &Arc<Semaphore>,
    pace: &Pace,
    max_retries: u32,
    on_retry: &Arc<dyn Fn(u32) + Send + Sync>,
) {
    let pending: Vec<Arc<SegmentRt>> = {
        let segments = lock(&job.segments);
        segments
            .iter()
            .filter(|segment| !segment.finished() && !inflight.contains(&segment.id))
            .map(Arc::clone)
            .collect()
    };
    for segment in pending {
        inflight.insert(segment.id);
        let client = client.clone();
        let meta = meta.clone();
        let url = url.to_string();
        let cancel = cancel.clone();
        let host = Arc::clone(host);
        let pace = pace.clone();
        let on_retry = Arc::clone(on_retry);
        set.spawn(async move {
            let fetch = Fetch {
                client: &client,
                meta: &meta,
                url: &url,
                cancel: &cancel,
                host: host.as_ref(),
                pace: &pace,
                max_retries,
                on_retry: on_retry.as_ref(),
            };
            let result = download::fetch_bounded(&fetch, &segment, true).await;
            (segment.id, result)
        });
    }
}

/// Shrink the largest unfinished piece and append its tail. One split per finished worker.
///
/// The segment list lock is released before the gate. `commit_chunk` holds that gate and then
/// the file, so this path holds the gate and then the list. The new end is published before
/// SQLite; a failed write puts the old end back and leaves the original plan runnable.
fn try_split(shared: &Shared, job: &Job, partial: &Path) -> Option<Arc<SegmentRt>> {
    let min = shared.cfg.min_segment_bytes;
    let (parent, count) = {
        let segments = lock(&job.segments);
        if segments.len() >= 32 {
            return None;
        }
        let parts: Vec<(u64, u64)> = segments
            .iter()
            .map(|segment| {
                if !segment.bounded || segment.finished() {
                    (0, 0)
                } else {
                    (segment.cursor.load(Ordering::Acquire), segment.end.load(Ordering::Acquire))
                }
            })
            .collect();
        let (index, _) = crate::segment::propose_split(&parts, min, segments.len())?;
        (Arc::clone(&segments[index]), segments.len())
    };
    if count >= 32 {
        return None;
    }
    let gate = parent.gate.lock().unwrap_or_else(|err| err.into_inner());
    let cursor = parent.cursor.load(Ordering::Acquire);
    let end = parent.end.load(Ordering::Acquire);
    let mid = split_remainder(cursor, end, min)?;
    parent.end.store(mid, Ordering::Release);
    let tail_row = new_segment(job.id, mid, Some(end));
    let file = match SegmentFile::create(partial, None) {
        Ok(file) => file,
        Err(err) => {
            parent.end.store(end, Ordering::Release);
            tracing::warn!("split file for {}: {err}", job.id);
            return None;
        }
    };
    let tail = Arc::new(SegmentRt {
        id: tail_row.id,
        start: mid,
        bounded: true,
        end: AtomicU64::new(end),
        cursor: AtomicU64::new(mid),
        eof: AtomicBool::new(false),
        file,
        gate: Mutex::new(()),
    });
    if let Err(err) = lock(&shared.store).split_segment(job.id, parent.id, mid, &tail_row) {
        parent.end.store(end, Ordering::Release);
        tracing::warn!("split not stored for {}: {err}", job.id);
        return None;
    }
    lock(&job.segments).push(Arc::clone(&tail));
    drop(gate);
    Some(tail)
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
                let done = segment.finished();
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
            end: segment.live_end(),
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

enum Wire {
    Http { probe: Probe, client: Client, meta: RequestMeta },
    Ftp {
        probe: Probe,
        url: Url,
        username: Option<String>,
        password: Option<String>,
    },
}

impl Wire {
    fn probe(&self) -> &Probe {
        match self {
            Self::Http { probe, .. } | Self::Ftp { probe, .. } => probe,
        }
    }
}

fn download_target(download: &Download) -> Result<Url> {
    let raw = download
        .final_url
        .clone()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| download.url.clone());
    Url::parse(&raw).map_err(|err| Error::InvalidUrl(err.to_string()))
}

fn request_meta(shared: &Shared, download: &Download) -> Result<RequestMeta> {
    Ok(RequestMeta {
        user_agent: download.user_agent.clone().unwrap_or_else(|| shared.cfg.user_agent.clone()),
        referer: download.referer.clone(),
        headers: download.headers.clone(),
        cookies: download.cookies.clone(),
        auth: load_auth(shared, download)?,
    })
}

fn load_auth(shared: &Shared, download: &Download) -> Result<Option<AuthMaterial>> {
    let Some(id) = download.credential_id else {
        return Ok(None);
    };
    let credential = lock(&shared.store).get_credential(id)?;
    let password = shared.secrets.get_secret(&credential.keychain_id)?.ok_or(Error::AuthRequired)?;
    Ok(Some(AuthMaterial::new(credential.username, password, AuthScheme::parse(&credential.auth_scheme))))
}

fn ftp_identity(shared: &Shared, download: &Download) -> Result<(Option<String>, Option<String>)> {
    let Some(id) = download.credential_id else {
        return Ok((None, None));
    };
    let credential = lock(&shared.store).get_credential(id)?;
    let password = shared.secrets.get_secret(&credential.keychain_id)?.ok_or(Error::AuthRequired)?;
    Ok((Some(credential.username), Some(password)))
}

fn pace_for(shared: &Shared, download: &Download) -> Result<Pace> {
    let queue = lock(&shared.store).queue_speed_limit(download.queue_id)?;
    let local = limiter::tighter(queue, download.speed_limit_bps);
    Ok(Pace::new(Arc::clone(&shared.global_limit), local))
}

fn retry_hook(shared: &Arc<Shared>, id: Uuid) -> Arc<dyn Fn(u32) + Send + Sync> {
    let shared = Arc::clone(shared);
    Arc::new(move |count| {
        if let Err(err) = lock(&shared.store).set_retry_count(id, count) {
            tracing::warn!("retry count for {id}: {err}");
        }
    })
}

fn first_segment(job: &Job) -> Result<Arc<SegmentRt>> {
    lock(&job.segments)
        .first()
        .map(Arc::clone)
        .ok_or_else(|| Error::Failed("no segment".into()))
}

async fn transfer_ftp(
    url: &Url,
    insecure: bool,
    username: Option<String>,
    password: Option<String>,
    segment: &Arc<SegmentRt>,
    pace: &Pace,
    cancel: &CancellationToken,
    host: &Arc<Semaphore>,
    max_retries: u32,
    on_retry: &(dyn Fn(u32) + Send + Sync),
) -> Result<()> {
    let url = url.clone();
    retry::retry_loop(max_retries, cancel, |count| on_retry(count), || {
        let url = url.clone();
        let username = username.clone();
        let password = password.clone();
        let segment = Arc::clone(segment);
        let pace = pace.clone();
        let cancel = cancel.clone();
        let host = Arc::clone(host);
        async move {
            let _permit = download::acquire_host(&host, &cancel).await?;
            crate::ftp::fetch(&url, insecure, username.as_deref(), password.as_deref(), &segment, &pace, &cancel).await
        }
    })
    .await
}
