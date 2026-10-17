use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use forgedownloadmanager_core::checksum::random_token;
use forgedownloadmanager_core::config::{self, EngineConfig, DEFAULT_USER_AGENT};
use forgedownloadmanager_core::control::{self, interrupted_label};
use forgedownloadmanager_core::filename::ensure_direct_child;
use forgedownloadmanager_core::grabber;
use forgedownloadmanager_core::media::{
    check_media_tools, download_media_url, probe_media_stream, MediaProbeResult,
    MediaToolsStatus, YtDlpPlugin,
};
use forgedownloadmanager_core::model::{AddRequest, ChecksumAlgo, Download, DownloadStatus, DuplicatePolicy};
use forgedownloadmanager_core::schedule::{OnCompleteAction, Queue};
use forgedownloadmanager_core::secrets::{KeyringSecrets, SecretStore};
use forgedownloadmanager_core::store::Store;
use forgedownloadmanager_core::{insert_request, CancellationToken, Engine, EngineEvent};
use uuid::Uuid;

mod lock;
mod host;

#[derive(Parser)]
#[command(name = "forge-dm", version, about = "Forge Download Manager")]
struct Cli {
    /// Database directory. Defaults to the platform data dir or FORGE_DM_DATA.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download a URL. Stays in the foreground until that download finishes.
    Add {
        url: String,
        /// Directory that receives the file. Defaults to the current directory.
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        segments: Option<u32>,
        #[arg(long)]
        name: Option<String>,
        /// Store the download as paused and return.
        #[arg(long)]
        no_start: bool,
        #[arg(long)]
        referer: Option<String>,
        #[arg(long)]
        user_agent: Option<String>,
        /// Extra request header, `Name: value`.
        #[arg(long = "header")]
        headers: Vec<String>,
        /// Skip TLS verification for this download.
        #[arg(long)]
        insecure_tls: bool,
        /// `algo:hex`, where algo is md5, sha1, or sha256.
        #[arg(long)]
        checksum: Option<String>,
        /// `rename`, `overwrite`, or `ask`.
        #[arg(long)]
        duplicate: Option<String>,
        /// Cookie pair `Name=Value`. Repeat for more than one cookie.
        #[arg(long = "cookie")]
        cookies: Vec<String>,
        /// Per-download byte rate. `0` is unlimited.
        #[arg(long)]
        speed_limit: Option<u64>,
        /// `direct`, `system`, or a proxy URL (`http://`, `https://`, `socks5://`).
        #[arg(long)]
        proxy: Option<String>,
        /// User name for Basic, Digest, or FTP. The password is not an argument.
        #[arg(long)]
        user: Option<String>,
        /// Read one password line from stdin.
        #[arg(long)]
        password_stdin: bool,
        /// Target queue name or id.
        #[arg(long)]
        queue: Option<String>,
        /// Download priority (lower number runs first, default 0).
        #[arg(long)]
        priority: Option<i64>,
    },
    /// Show downloads. Does not start the engine.
    List {
        #[arg(long)]
        json: bool,
    },
    Pause { id: String },
    Resume { id: String },
    Remove {
        id: String,
        #[arg(long)]
        delete_file: bool,
    },
    /// Run queued downloads until the queue is idle.
    Run,
    /// Manage queues and schedules.
    Queue {
        #[command(subcommand)]
        action: QueueCommand,
    },
    /// Batch download URL patterns, link lists, or crawl a website.
    Grab {
        /// URL pattern e.g. "http://example.com/file[01-10].zip" or start URL for crawl.
        pattern_or_url: Option<String>,
        /// File containing a list of URLs (one per line).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Crawl same domain starting from pattern_or_url.
        #[arg(long)]
        crawl: bool,
        /// Maximum crawl depth (default 1).
        #[arg(long, default_value_t = 1)]
        max_depth: usize,
        /// Maximum links to collect (default 50).
        #[arg(long, default_value_t = 50)]
        max_links: usize,
        /// Allowed extensions when crawling (e.g. --ext pdf --ext zip).
        #[arg(long = "ext")]
        extensions: Vec<String>,
        /// Directory that receives the files.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Target queue name or id.
        #[arg(long)]
        queue: Option<String>,
        /// Show expanded/discovered URLs without downloading.
        #[arg(long)]
        dry_run: bool,
    },
    /// Store a password in the OS keychain. `HOST` is `host:port`, for example `example.com:443`.
    Auth {
        #[command(subcommand)]
        action: AuthCommand,
    },
    /// Read or write one engine setting.
    Config {
        #[command(subcommand)]
        action: ConfigCommand,
    },
    /// Run or register the Native Messaging host for browser extensions.
    Host {
        /// Install and register the native messaging host manifest for browsers.
        #[arg(long)]
        install: bool,
        /// Uninstall and unregister the native messaging host manifest.
        #[arg(long)]
        uninstall: bool,
    },
    /// Media stream and video extraction tools (HLS, DASH, yt-dlp, ffmpeg).
    Media {
        #[command(subcommand)]
        action: MediaCommand,
    },
}

#[derive(Subcommand)]
enum QueueCommand {
    /// List all queues.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Add a new queue.
    Add {
        name: String,
        #[arg(long)]
        concurrency: Option<usize>,
        #[arg(long)]
        speed_limit: Option<u64>,
        /// Window start time "HH:MM" (local time).
        #[arg(long)]
        start: Option<String>,
        /// Window end time "HH:MM" (local time).
        #[arg(long)]
        end: Option<String>,
        /// Bitmask for active days (bit 0=Sun..bit 6=Sat, default 127).
        #[arg(long, default_value_t = 127)]
        days: u8,
        /// Completion action: none, open_file, run_command, sleep, shutdown, quit.
        #[arg(long, default_value = "none")]
        on_complete: String,
        /// Command to execute when on_complete is run_command.
        #[arg(long)]
        command: Option<String>,
    },
    /// Update an existing queue.
    Update {
        name_or_id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        concurrency: Option<usize>,
        #[arg(long)]
        speed_limit: Option<u64>,
        #[arg(long)]
        start: Option<String>,
        #[arg(long)]
        end: Option<String>,
        #[arg(long)]
        days: Option<u8>,
        #[arg(long)]
        on_complete: Option<String>,
        #[arg(long)]
        command: Option<String>,
    },
    /// Remove a queue (downloads are reassigned to Default).
    Remove {
        name_or_id: String,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// `forge-dm auth set HOST USER` reads the password from stdin.
    Set { host: String, username: String },
}

#[derive(Subcommand)]
enum ConfigCommand {
    Get { key: String },
    Set { key: String, value: String },
}

#[tokio::main]
async fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            std::process::exit(if err.use_stderr() { 1 } else { 0 });
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let code = match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<i32, String> {
    let data_dir = cli.data_dir.unwrap_or_else(default_data_dir);
    match cli.command {
        Command::Add {
            url,
            dir,
            segments,
            name,
            no_start,
            referer,
            user_agent,
            headers,
            insecure_tls,
            checksum,
            duplicate,
            cookies,
            speed_limit,
            proxy,
            user,
            password_stdin,
            queue,
            priority,
        } => {
            let mut request = AddRequest::new(url, dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))));
            request.segments = segments;
            request.filename = name;
            request.start = !no_start;
            request.referer = referer;
            request.user_agent = user_agent;
            request.headers = parse_headers(&headers)?;
            request.insecure_tls = insecure_tls;
            request.cookies = parse_cookies(&cookies)?;
            request.speed_limit_bps = speed_limit;
            if let Some(proxy) = proxy {
                request.proxy = Some(forgedownloadmanager_core::proxy::override_from_flag(&proxy).map_err(|err| err.to_string())?);
            }
            request.username = user;
            if password_stdin {
                request.password = Some(read_secret_line()?);
            }
            if let Some(queue_name_or_id) = queue {
                let qid = resolve_queue_id(&data_dir, &queue_name_or_id).await?;
                request.queue_id = Some(qid);
            }
            request.priority = priority;
            if insecure_tls {
                eprintln!("warning: TLS certificate verification is disabled for this download");
            }
            if let Some(checksum) = checksum {
                let (algo, expected) = parse_checksum(&checksum)?;
                request.checksum_algo = Some(algo);
                request.checksum_expected = Some(expected);
            }
            if let Some(policy) = duplicate {
                request.duplicate_policy = DuplicatePolicy::parse(&policy).map_err(|err| err.to_string())?;
            }
            if no_start {
                add_offline(&data_dir, request).await
            } else {
                add_online(&data_dir, request).await
            }
        }
        Command::List { json } => list_cmd(&data_dir, json).await,
        Command::Pause { id } => pause_cmd(&data_dir, &id).await,
        Command::Resume { id } => resume_cmd(&data_dir, &id).await,
        Command::Remove { id, delete_file } => remove_cmd(&data_dir, &id, delete_file).await,
        Command::Run => run_cmd(&data_dir).await,
        Command::Queue { action } => queue_cmd(&data_dir, action).await,
        Command::Grab {
            pattern_or_url,
            file,
            crawl,
            max_depth,
            max_links,
            extensions,
            dir,
            queue,
            dry_run,
        } => {
            grab_cmd(
                &data_dir,
                pattern_or_url,
                file,
                crawl,
                max_depth,
                max_links,
                extensions,
                dir,
                queue,
                dry_run,
            )
            .await
        }
        Command::Auth { action } => match action {
            AuthCommand::Set { host, username } => auth_set(&data_dir, &host, &username).await,
        },
        Command::Config { action } => match action {
            ConfigCommand::Get { key } => config_get(&data_dir, &key).await,
            ConfigCommand::Set { key, value } => config_set(&data_dir, &key, &value).await,
        },
        Command::Host { install, uninstall } => {
            if install {
                host::install_host(&data_dir)
            } else if uninstall {
                host::uninstall_host(&data_dir)
            } else {
                host::run_host(&data_dir).await
            }
        }
        Command::Media { action } => media_cmd(&data_dir, action).await,
    }
}

async fn add_offline(data_dir: &Path, request: AddRequest) -> Result<i32, String> {
    let _owner = match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Owner(owner) => owner,
        lock::Attach::Forward { addr, token } => {
            let body = add_body(&request);
            let id = forward_add(addr, &token, &body.to_string()).await?;
            println!("{id}");
            return Ok(0);
        }
    };
    let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
    let segments = store.setting_u64("default_segments").map_err(|err| err.to_string())?.unwrap_or(8) as u32;
    let ua = store.setting_string("user_agent").map_err(|err| err.to_string())?.unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());
    let secrets = KeyringSecrets;
    let id = insert_request(&store, &request, segments, &ua, &secrets).map_err(|err| err.to_string())?;
    println!("{id}");
    Ok(0)
}

async fn add_online(data_dir: &Path, request: AddRequest) -> Result<i32, String> {
    match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Forward { addr, token } => {
            let body = add_body(&request);
            let id = forward_add(addr, &token, &body.to_string()).await?;
            println!("{id}");
            let _ = std::io::stdout().flush();
            wait_remote(data_dir, addr, &token, id).await
        }
        lock::Attach::Owner(owner) => {
            let engine = open_engine(&owner).await?;
            let id = engine.add(request).await.map_err(|err| err.to_string())?;
            println!("{id}");
            let _ = std::io::stdout().flush();
            let code = wait_local(&engine, &owner.data_dir, Some(id)).await?;
            Ok(code)
        }
    }
}

async fn list_cmd(data_dir: &Path, json: bool) -> Result<i32, String> {
    if let Some((addr, token)) = lock::supervisor(data_dir).map_err(|err| err.to_string())? {
        let rows = remote_downloads(addr, &token).await?;
        print_downloads(&rows, json, true);
        return Ok(0);
    }
    let rows = Store::list_if_exists(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
    print_downloads(&rows, json, false);
    Ok(0)
}

async fn pause_cmd(data_dir: &Path, id: &str) -> Result<i32, String> {
    match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Forward { addr, token } => {
            let resolved = remote_resolve(addr, &token, id).await?;
            post_ok(addr, &token, &format!("/v1/downloads/{resolved}/pause"), "").await?;
            Ok(0)
        }
        lock::Attach::Owner(owner) => {
            let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
            let id = store.resolve(id).map_err(|err| err.to_string())?;
            let download = store.get(id).map_err(|err| err.to_string())?;
            if download.status == DownloadStatus::Completed {
                return Err("download is complete".into());
            }
            store.set_status(id, DownloadStatus::Paused, None, None).map_err(|err| err.to_string())?;
            drop(owner);
            Ok(0)
        }
    }
}

async fn resume_cmd(data_dir: &Path, id: &str) -> Result<i32, String> {
    match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Forward { addr, token } => {
            let resolved = remote_resolve(addr, &token, id).await?;
            // The owner can finish and close the listener between this post and the
            // next poll. A refused connection after that exit is read from SQLite.
            let started = Instant::now();
            loop {
                if ctrl_c_pending() {
                    return Ok(130);
                }
                match post_forward(addr, &token, &format!("/v1/downloads/{resolved}/resume"), "").await {
                    Ok(()) => return wait_remote(data_dir, addr, &token, resolved).await,
                    Err(ForwardError::Status(err)) => return Err(err),
                    Err(ForwardError::Unreachable(err)) => {
                        if let Some(code) = status_after_owner_exit(data_dir, resolved)? {
                            return Ok(code);
                        }
                        if started.elapsed() > Duration::from_secs(5) {
                            return Err(err);
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            }
        }
        lock::Attach::Owner(owner) => {
            let engine = open_engine(&owner).await?;
            let id = engine.resolve(id).map_err(|err| err.to_string())?;
            engine.resume(id).await.map_err(|err| err.to_string())?;
            wait_local(&engine, &owner.data_dir, Some(id)).await
        }
    }
}

async fn remove_cmd(data_dir: &Path, id: &str, delete_file: bool) -> Result<i32, String> {
    match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Forward { addr, token } => {
            let resolved = remote_resolve(addr, &token, id).await?;
            let body = serde_json::json!({ "delete_file": delete_file }).to_string();
            post_ok(addr, &token, &format!("/v1/downloads/{resolved}/remove"), &body).await?;
            Ok(0)
        }
        lock::Attach::Owner(_owner) => {
            let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
            let id = store.resolve(id).map_err(|err| err.to_string())?;
            let download = store.get(id).map_err(|err| err.to_string())?;
            if delete_file {
                for path in [
                    download.partial_path.clone(),
                    download.final_path(),
                    download.save_dir.join(format!("{}.forgepart", download.filename)),
                ] {
                    if ensure_direct_child(&download.save_dir, &path).is_ok() && path.exists() {
                        std::fs::remove_file(&path).map_err(|err| err.to_string())?;
                    }
                }
            }
            store.delete_download(id).map_err(|err| err.to_string())?;
            Ok(0)
        }
    }
}

async fn run_cmd(data_dir: &Path) -> Result<i32, String> {
    match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Forward { addr, token } => wait_remote_idle(addr, &token).await,
        lock::Attach::Owner(owner) => {
            let engine = open_engine(&owner).await?;
            wait_local(&engine, &owner.data_dir, None).await
        }
    }
}

async fn open_engine(owner: &lock::Owner) -> Result<Engine, String> {
    let engine = Engine::open(EngineConfig::new(owner.data_dir.join("forge.db")))
        .await
        .map_err(|err| err.to_string())?;
    let token = random_token().map_err(|err| err.to_string())?;
    let addr = control::serve(engine.clone(), token.clone()).await.map_err(|err| err.to_string())?;
    lock::write_info(&owner.data_dir, addr.port(), &token).map_err(|err| err.to_string())?;
    Ok(engine)
}

async fn wait_local(engine: &Engine, data_dir: &Path, focus: Option<uuid::Uuid>) -> Result<i32, String> {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let printer = tokio::spawn(print_progress(engine.clone(), stop_rx));
    let interrupted = async {
        tokio::signal::ctrl_c().await.ok();
        let _ = engine.pause_all().await;
        130
    };
    tokio::pin!(interrupted);
    let code = tokio::select! {
        code = &mut interrupted => code,
        code = async {
            let mut code = 0;
            if let Some(id) = focus {
                let download = engine.wait_for_terminal(id).await.map_err(|err| err.to_string())?;
                if download.status == DownloadStatus::Failed {
                    if let Some(message) = &download.error_message {
                        eprintln!("{message}");
                    }
                    code = 2;
                }
            }
            engine.wait_until_idle().await.map_err(|err| err.to_string())?;
            Ok::<i32, String>(code)
        } => code?,
    };
    let _ = stop_tx.send(());
    engine.shutdown().await;
    lock::clear_info(data_dir);
    let _ = printer.await;
    Ok(code)
}

async fn print_progress(engine: Engine, mut stop: tokio::sync::oneshot::Receiver<()>) {
    let mut events = engine.subscribe();
    let tty = std::io::stderr().is_terminal();
    let mut last = Instant::now() - Duration::from_secs(2);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            event = events.recv() => {
                let Ok(event) = event else { continue };
                match event {
                    EngineEvent::Progress { id, downloaded_bytes, total_bytes, speed_bps, .. } => {
                        let now = Instant::now();
                        if !tty && now.duration_since(last) < Duration::from_secs(1) {
                            continue;
                        }
                        last = now;
                        let total = total_bytes.map(|value| value.to_string()).unwrap_or_else(|| "?".into());
                        if tty {
                            eprint!("\r{id} {downloaded_bytes}/{total} {speed_bps} B/s");
                            let _ = std::io::stderr().flush();
                        } else {
                            eprintln!("{id} {downloaded_bytes}/{total} {speed_bps} B/s");
                        }
                    }
                    EngineEvent::Completed { id, path, .. } => {
                        eprintln!("\n{id} -> {}", path.display());
                    }
                    EngineEvent::Failed { message, .. } => {
                        eprintln!("\n{message}");
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn forward_add(addr: std::net::SocketAddr, token: &str, body: &str) -> Result<uuid::Uuid, String> {
    let (status, text) = control::control_exchange(addr, token, "POST", "/v1/downloads", body, None, None)
        .await
        .map_err(|err| err.to_string())?;
    if status != 200 {
        return Err(text);
    }
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|err| err.to_string())?;
    let id = value.get("id").and_then(|id| id.as_str()).ok_or_else(|| text.clone())?;
    uuid::Uuid::parse_str(id).map_err(|err| err.to_string())
}

enum ForwardError {
    /// The listener answered with an HTTP error.
    Status(String),
    /// No listener accepted the connection.
    Unreachable(String),
}

async fn post_ok(addr: std::net::SocketAddr, token: &str, path: &str, body: &str) -> Result<(), String> {
    post_forward(addr, token, path, body).await.map_err(|err| match err {
        ForwardError::Status(text) | ForwardError::Unreachable(text) => text,
    })
}

async fn post_forward(addr: std::net::SocketAddr, token: &str, path: &str, body: &str) -> Result<(), ForwardError> {
    let (status, text) = match control::control_exchange(addr, token, "POST", path, body, None, None).await {
        Ok(response) => response,
        Err(err) => return Err(ForwardError::Unreachable(err.to_string())),
    };
    if status >= 400 {
        return Err(ForwardError::Status(if text.is_empty() {
            format!("control API returned {status}")
        } else {
            text
        }));
    }
    Ok(())
}

async fn remote_downloads(addr: std::net::SocketAddr, token: &str) -> Result<Vec<Download>, String> {
    let (status, text) = control::control_exchange(addr, token, "GET", "/v1/downloads", "", None, None)
        .await
        .map_err(|err| err.to_string())?;
    if status != 200 {
        return Err(text);
    }
    serde_json::from_str(&text).map_err(|err| err.to_string())
}

async fn remote_resolve(addr: std::net::SocketAddr, token: &str, prefix: &str) -> Result<uuid::Uuid, String> {
    if let Ok(id) = uuid::Uuid::parse_str(prefix) {
        return Ok(id);
    }
    let rows = remote_downloads(addr, token).await?;
    let compact = prefix.to_ascii_lowercase().replace('-', "");
    let matches: Vec<_> = rows
        .into_iter()
        .filter(|download| {
            let full = download.id.to_string();
            full.starts_with(&prefix.to_ascii_lowercase()) || full.replace('-', "").starts_with(&compact)
        })
        .map(|download| download.id)
        .collect();
    match matches.as_slice() {
        [id] => Ok(*id),
        [] => Err(format!("no download matches {prefix}")),
        _ => Err(format!("{prefix} matches more than one download")),
    }
}

fn ctrl_c_pending() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Once;
    static FLAG: AtomicBool = AtomicBool::new(false);
    static START: Once = Once::new();
    // Windows allows one console handler. Register it once and poll the flag.
    START.call_once(|| {
        tokio::spawn(async {
            let _ = tokio::signal::ctrl_c().await;
            FLAG.store(true, Ordering::SeqCst);
        });
    });
    FLAG.load(Ordering::SeqCst)
}

async fn wait_remote(data_dir: &Path, addr: std::net::SocketAddr, token: &str, id: uuid::Uuid) -> Result<i32, String> {
    // `add` closes the listener as soon as the queue is idle. The poll can land
    // in that gap and see a refused connection for a download that already finished.
    let mut failing_since: Option<Instant> = None;
    loop {
        if ctrl_c_pending() {
            return Ok(130);
        }
        match remote_downloads(addr, token).await {
            Ok(rows) => {
                failing_since = None;
                if let Some(code) = finished_code(&rows, id) {
                    return Ok(code);
                }
            }
            Err(err) => {
                if let Some(code) = status_after_owner_exit(data_dir, id)? {
                    return Ok(code);
                }
                let since = failing_since.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(5) {
                    return Err(err);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn finished_code(rows: &[Download], id: uuid::Uuid) -> Option<i32> {
    let download = rows.iter().find(|download| download.id == id)?;
    if matches!(download.status, DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Stopped)
        && !rows.iter().any(|row| row.status.is_active())
    {
        Some(if download.status == DownloadStatus::Failed { 2 } else { 0 })
    } else {
        None
    }
}

/// `Some` when the supervising process has released the lock and the download is terminal.
fn status_after_owner_exit(data_dir: &Path, id: uuid::Uuid) -> Result<Option<i32>, String> {
    match lock::supervisor(data_dir).map_err(|err| err.to_string())? {
        Some(_) => Ok(None),
        None => {
            let rows = Store::list_if_exists(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
            Ok(finished_code(&rows, id))
        }
    }
}

async fn wait_remote_idle(addr: std::net::SocketAddr, token: &str) -> Result<i32, String> {
    loop {
        if ctrl_c_pending() {
            return Ok(130);
        }
        let rows = match remote_downloads(addr, token).await {
            Ok(rows) => rows,
            Err(_) => return Ok(0),
        };
        if !rows.iter().any(|row| row.status.is_active()) {
            let failed = rows.iter().any(|row| row.status == DownloadStatus::Failed);
            return Ok(if failed { 2 } else { 0 });
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn print_downloads(rows: &[Download], json: bool, supervisor: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(rows).unwrap_or_else(|_| "[]".into()));
        return;
    }
    if rows.is_empty() {
        println!("no downloads");
        return;
    }
    for download in rows {
        let total = download.total_bytes.map(|value| value.to_string()).unwrap_or_else(|| "?".into());
        println!(
            "{}  {:<12}  {}/{}  {}",
            download.id,
            interrupted_label(download.status, supervisor),
            download.downloaded_bytes,
            total,
            download.filename
        );
    }
}

fn add_body(request: &AddRequest) -> serde_json::Value {
    serde_json::json!({
        "url": request.url,
        "save_dir": request.save_dir,
        "filename": request.filename,
        "segments": request.segments,
        "start": request.start,
        "referer": request.referer,
        "user_agent": request.user_agent,
        "headers": request.headers,
        "cookies": request.cookies,
        "proxy": request.proxy,
        "speed_limit_bps": request.speed_limit_bps,
        "username": request.username,
        "password": request.password,
        "max_retries": request.max_retries,
        "insecure_tls": request.insecure_tls,
        "duplicate_policy": request.duplicate_policy,
        "checksum_algo": request.checksum_algo,
        "checksum_expected": request.checksum_expected,
        "queue_id": request.queue_id,
        "priority": request.priority,
    })
}

fn parse_cookies(cookies: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for cookie in cookies {
        let Some((name, value)) = cookie.split_once('=') else {
            return Err(format!("cookie `{cookie}` needs Name=Value"));
        };
        let name = name.trim();
        if name.is_empty() {
            return Err("cookie name is empty".into());
        }
        out.push((name.to_string(), value.trim().to_string()));
    }
    Ok(out)
}

fn read_secret_line() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|err| err.to_string())?;
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(line)
}

async fn auth_set(data_dir: &Path, host: &str, username: &str) -> Result<i32, String> {
    let password = read_secret_line()?;
    match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Forward { addr, token } => {
            let body = serde_json::json!({ "host": host, "username": username, "password": password }).to_string();
            post_ok(addr, &token, "/v1/credentials", &body).await?;
            Ok(0)
        }
        lock::Attach::Owner(_owner) => {
            let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
            let credential = store.upsert_credential(host, username, "any").map_err(|err| err.to_string())?;
            KeyringSecrets.set_secret(&credential.keychain_id, &password).map_err(|err| err.to_string())?;
            println!("{}", credential.id);
            Ok(0)
        }
    }
}

async fn config_get(data_dir: &Path, key: &str) -> Result<i32, String> {
    if !config::SETTING_KEYS.contains(&key) {
        return Err(format!("unknown setting {key}"));
    }
    match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Forward { addr, token } => {
            let (status, text) = control::control_exchange(addr, &token, "GET", &format!("/v1/settings/{key}"), "", None, None)
                .await
                .map_err(|err| err.to_string())?;
            if status != 200 {
                return Err(text);
            }
            println!("{text}");
            Ok(0)
        }
        lock::Attach::Owner(_owner) => {
            let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
            let value = store.setting_value(key).map_err(|err| err.to_string())?;
            println!("{}", serde_json::to_string(&value).map_err(|err| err.to_string())?);
            Ok(0)
        }
    }
}

async fn config_set(data_dir: &Path, key: &str, raw: &str) -> Result<i32, String> {
    let value = config::parse_setting_value(key, raw).map_err(|err| err.to_string())?;
    match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Forward { addr, token } => {
            let body = serde_json::json!({ "key": key, "value": value }).to_string();
            post_ok(addr, &token, "/v1/settings", &body).await?;
            Ok(0)
        }
        lock::Attach::Owner(_owner) => {
            let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
            store.set_setting(key, &value).map_err(|err| err.to_string())?;
            Ok(0)
        }
    }
}

fn parse_headers(headers: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for header in headers {
        let Some((name, value)) = header.split_once(':') else {
            return Err(format!("header `{header}` needs a colon"));
        };
        out.push((name.trim().to_string(), value.trim().to_string()));
    }
    Ok(out)
}

fn parse_checksum(value: &str) -> Result<(ChecksumAlgo, String), String> {
    let Some((algo, hex)) = value.split_once(':') else {
        return Err("checksum must look like sha256:<hex>".into());
    };
    let algo = ChecksumAlgo::parse(algo).map_err(|err| err.to_string())?;
    Ok((algo, hex.trim().to_string()))
}

fn default_data_dir() -> PathBuf {
    lock::data_dir()
}

async fn resolve_queue_id(data_dir: &Path, name_or_id: &str) -> Result<Uuid, String> {
    if let Ok(id) = Uuid::parse_str(name_or_id) {
        return Ok(id);
    }
    match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Forward { addr, token } => {
            let (status, text) = control::control_exchange(addr, &token, "GET", &format!("/v1/queues/{name_or_id}"), "", None, None)
                .await
                .map_err(|err| err.to_string())?;
            if status != 200 {
                return Err(format!("queue '{name_or_id}' not found"));
            }
            let q: Queue = serde_json::from_str(&text).map_err(|err| err.to_string())?;
            Ok(q.id)
        }
        lock::Attach::Owner(_owner) => {
            let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
            let q = store.get_queue_by_name(name_or_id).map_err(|err| err.to_string())?
                .ok_or_else(|| format!("queue '{name_or_id}' not found"))?;
            Ok(q.id)
        }
    }
}

async fn queue_cmd(data_dir: &Path, action: QueueCommand) -> Result<i32, String> {
    match action {
        QueueCommand::List { json } => {
            let queues = match lock::supervisor(data_dir).map_err(|err| err.to_string())? {
                Some((addr, token)) => {
                    let (status, text) = control::control_exchange(addr, &token, "GET", "/v1/queues", "", None, None)
                        .await
                        .map_err(|err| err.to_string())?;
                    if status != 200 {
                        return Err(text);
                    }
                    serde_json::from_str::<Vec<Queue>>(&text).map_err(|err| err.to_string())?
                }
                None => {
                    let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
                    store.list_queues().map_err(|err| err.to_string())?
                }
            };

            if json {
                println!("{}", serde_json::to_string_pretty(&queues).map_err(|err| err.to_string())?);
            } else {
                println!("{:<36} {:<15} {:<6} {:<12} {:<13} {:<6} {:<12}",
                    "ID", "NAME", "CONC", "SPEED LIMIT", "SCHEDULE", "DAYS", "ON COMPLETE");
                for q in &queues {
                    let speed = q.speed_limit_bps.map(|s| format!("{s} B/s")).unwrap_or_else(|| "unlimited".into());
                    let schedule = match (&q.schedule_start, &q.schedule_end) {
                        (Some(s), Some(e)) => format!("{s}-{e}"),
                        _ => "always".into(),
                    };
                    println!("{:<36} {:<15} {:<6} {:<12} {:<13} {:<6} {:<12}",
                        q.id, q.name, q.concurrency, speed, schedule, q.days_mask, q.on_complete.as_str());
                }
            }
            Ok(0)
        }
        QueueCommand::Add {
            name,
            concurrency,
            speed_limit,
            start,
            end,
            days,
            on_complete,
            command,
        } => {
            let action = OnCompleteAction::parse(&on_complete).map_err(|err| err.to_string())?;
            let body = serde_json::json!({
                "name": name,
                "concurrency": concurrency,
                "speed_limit_bps": speed_limit,
                "schedule_start": start,
                "schedule_end": end,
                "days_mask": days,
                "on_complete": action.as_str(),
                "on_complete_command": command,
            });

            match lock::attach(data_dir).map_err(|err| err.to_string())? {
                lock::Attach::Forward { addr, token } => {
                    let (status, text) = control::control_exchange(addr, &token, "POST", "/v1/queues", &body.to_string(), None, None)
                        .await
                        .map_err(|err| err.to_string())?;
                    if status != 200 {
                        return Err(text);
                    }
                    let q: Queue = serde_json::from_str(&text).map_err(|err| err.to_string())?;
                    println!("{}", q.id);
                }
                lock::Attach::Owner(_owner) => {
                    let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
                    let queue = Queue {
                        id: Uuid::new_v4(),
                        name,
                        concurrency: concurrency.unwrap_or(3).max(1),
                        speed_limit_bps: speed_limit,
                        schedule_start: start,
                        schedule_end: end,
                        days_mask: days,
                        on_complete: action,
                        on_complete_command: command,
                        created_at: forgedownloadmanager_core::model::now_ms(),
                    };
                    store.create_queue(&queue).map_err(|err| err.to_string())?;
                    println!("{}", queue.id);
                }
            }
            Ok(0)
        }
        QueueCommand::Update {
            name_or_id,
            name,
            concurrency,
            speed_limit,
            start,
            end,
            days,
            on_complete,
            command,
        } => {
            let action_parsed = match on_complete.as_deref() {
                Some(act) => Some(OnCompleteAction::parse(act).map_err(|err| err.to_string())?),
                None => None,
            };
            let body = serde_json::json!({
                "name": name,
                "concurrency": concurrency,
                "speed_limit_bps": speed_limit,
                "schedule_start": start,
                "schedule_end": end,
                "days_mask": days,
                "on_complete": action_parsed.map(|a| a.as_str()),
                "on_complete_command": command,
            });

            match lock::attach(data_dir).map_err(|err| err.to_string())? {
                lock::Attach::Forward { addr, token } => {
                    let (status, text) = control::control_exchange(
                        addr,
                        &token,
                        "PUT",
                        &format!("/v1/queues/{name_or_id}"),
                        &body.to_string(),
                        None,
                        None,
                    )
                    .await
                    .map_err(|err| err.to_string())?;
                    if status != 200 {
                        return Err(text);
                    }
                    let q: Queue = serde_json::from_str(&text).map_err(|err| err.to_string())?;
                    println!("{}", q.id);
                }
                lock::Attach::Owner(_owner) => {
                    let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
                    let mut queue = if let Ok(id) = Uuid::parse_str(&name_or_id) {
                        store.get_queue(id).map_err(|err| err.to_string())?.ok_or_else(|| format!("queue '{name_or_id}' not found"))?
                    } else {
                        store.get_queue_by_name(&name_or_id).map_err(|err| err.to_string())?.ok_or_else(|| format!("queue '{name_or_id}' not found"))?
                    };
                    if let Some(n) = name {
                        queue.name = n;
                    }
                    if let Some(c) = concurrency {
                        queue.concurrency = c.max(1);
                    }
                    if let Some(s) = speed_limit {
                        queue.speed_limit_bps = if s > 0 { Some(s) } else { None };
                    }
                    if start.is_some() {
                        queue.schedule_start = start;
                    }
                    if end.is_some() {
                        queue.schedule_end = end;
                    }
                    if let Some(d) = days {
                        queue.days_mask = d;
                    }
                    if let Some(a) = action_parsed {
                        queue.on_complete = a;
                    }
                    if command.is_some() {
                        queue.on_complete_command = command;
                    }
                    store.update_queue(&queue).map_err(|err| err.to_string())?;
                    println!("{}", queue.id);
                }
            }
            Ok(0)
        }
        QueueCommand::Remove { name_or_id } => {
            match lock::attach(data_dir).map_err(|err| err.to_string())? {
                lock::Attach::Forward { addr, token } => {
                    let (status, text) = control::control_exchange(
                        addr,
                        &token,
                        "DELETE",
                        &format!("/v1/queues/{name_or_id}"),
                        "",
                        None,
                        None,
                    )
                    .await
                    .map_err(|err| err.to_string())?;
                    if status != 200 {
                        return Err(text);
                    }
                }
                lock::Attach::Owner(_owner) => {
                    let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
                    let id = if let Ok(parsed_id) = Uuid::parse_str(&name_or_id) {
                        parsed_id
                    } else {
                        store.get_queue_by_name(&name_or_id).map_err(|err| err.to_string())?
                            .ok_or_else(|| format!("queue '{name_or_id}' not found"))?.id
                    };
                    store.delete_queue(id).map_err(|err| err.to_string())?;
                }
            }
            println!("queue removed");
            Ok(0)
        }
    }
}

async fn grab_cmd(
    data_dir: &Path,
    pattern_or_url: Option<String>,
    file: Option<PathBuf>,
    crawl: bool,
    max_depth: usize,
    max_links: usize,
    extensions: Vec<String>,
    dir: Option<PathBuf>,
    queue: Option<String>,
    dry_run: bool,
) -> Result<i32, String> {
    let urls = if let Some(path) = file {
        let content = std::fs::read_to_string(&path)
            .map_err(|err| format!("failed to read file {}: {err}", path.display()))?;
        grabber::parse_batch_urls(&content).map_err(|err| err.to_string())?
    } else if crawl {
        let start_url = pattern_or_url.ok_or("crawl requires a start URL")?;
        grabber::crawl_urls(&start_url, max_depth, max_links, &extensions)
            .await
            .map_err(|err| err.to_string())?
    } else if let Some(pattern) = pattern_or_url {
        grabber::expand_pattern(&pattern).map_err(|err| err.to_string())?
    } else {
        return Err("grab requires a URL/pattern or --file".into());
    };

    if urls.is_empty() {
        println!("No URLs found.");
        return Ok(0);
    }

    if dry_run {
        println!("Discovered {} URLs:", urls.len());
        for u in &urls {
            println!("  {u}");
        }
        return Ok(0);
    }

    let save_dir = dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let queue_id = if let Some(ref q) = queue {
        Some(resolve_queue_id(data_dir, q).await?)
    } else {
        None
    };

    println!("Queueing {} downloads...", urls.len());
    let mut added = 0;
    for u in urls {
        let mut request = AddRequest::new(u, save_dir.clone());
        request.queue_id = queue_id;
        request.start = false;
        if add_offline(data_dir, request).await.is_ok() {
            added += 1;
        }
    }
    println!("Successfully queued {added} downloads.");
    Ok(0)
}

#[derive(Subcommand)]
enum MediaCommand {
    /// Probe a media stream (HLS .m3u8, DASH .mpd) or URL for video/audio streams.
    Probe {
        url: String,
        #[arg(long)]
        json: bool,
    },
    /// Download and remux an HLS or DASH stream to a local video file.
    Download {
        url: String,
        /// Output directory (defaults to current directory).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Custom filename for the remuxed video.
        #[arg(long)]
        name: Option<String>,
    },
    /// Check availability of ffmpeg and yt-dlp tools.
    Tools {
        #[arg(long)]
        json: bool,
    },
    /// Install yt-dlp plugin helper from official GitHub release (requires explicit --consent).
    InstallYtdlp {
        /// Explicit user consent to download official yt-dlp release binary.
        #[arg(long)]
        consent: bool,
    },
}

async fn media_cmd(data_dir: &Path, action: MediaCommand) -> Result<i32, String> {
    let plugins_dir = data_dir.join("plugins");
    match action {
        MediaCommand::Probe { url, json } => {
            let result: MediaProbeResult = match lock::attach(data_dir).map_err(|err| err.to_string())? {
                lock::Attach::Forward { addr, token } => {
                    let req_body = serde_json::json!({ "url": url }).to_string();
                    let (status, text) = control::control_exchange(
                        addr,
                        &token,
                        "POST",
                        "/v1/media/probe",
                        &req_body,
                        None,
                        None,
                    )
                    .await
                    .map_err(|err| err.to_string())?;
                    if status != 200 {
                        return Err(text);
                    }
                    serde_json::from_str(&text).map_err(|err| err.to_string())?
                }
                lock::Attach::Owner(_owner) => {
                    let client = forgedownloadmanager_core::media::create_media_client().map_err(|e| e.to_string())?;
                    probe_media_stream(&client, &url, Some(&plugins_dir)).await.map_err(|err| err.to_string())?
                }
            };

            if json {
                println!("{}", serde_json::to_string_pretty(&result).map_err(|err| err.to_string())?);
            } else {
                println!("Media Probe Result:");
                println!("  Type: {}", result.media_type);
                if let Some(title) = &result.title {
                    println!("  Title: {}", title);
                }
                if let Some(duration) = result.duration {
                    println!("  Duration: {:.1}s", duration);
                }
                if let Some(count) = result.segment_count {
                    println!("  Segments: {}", count);
                }
                if let Some(best) = &result.best_url {
                    println!("  Best Stream URL: {}", best);
                }
                if !result.variants.is_empty() {
                    println!("  Variants / Streams ({}):", result.variants.len());
                    for (idx, v) in result.variants.iter().enumerate() {
                        let res = v.resolution.as_deref().unwrap_or("unknown-res");
                        let bw = v.bandwidth.map(|b| format!("{} kbps", b / 1000)).unwrap_or_else(|| "unknown-bw".into());
                        let codecs = v.codecs.as_deref().unwrap_or("unknown-codecs");
                        println!("    [{idx}] {} ({bw}) codecs: {codecs}", res);
                        println!("        URL: {}", v.url);
                    }
                }
            }
            Ok(0)
        }
        MediaCommand::Tools { json } => {
            let status: MediaToolsStatus = match lock::attach(data_dir).map_err(|err| err.to_string())? {
                lock::Attach::Forward { addr, token } => {
                    let (st, text) = control::control_exchange(
                        addr,
                        &token,
                        "GET",
                        "/v1/media/tools",
                        "",
                        None,
                        None,
                    )
                    .await
                    .map_err(|err| err.to_string())?;
                    if st != 200 {
                        return Err(text);
                    }
                    serde_json::from_str(&text).map_err(|err| err.to_string())?
                }
                lock::Attach::Owner(_owner) => {
                    check_media_tools(Some(&plugins_dir))
                }
            };

            if json {
                println!("{}", serde_json::to_string_pretty(&status).map_err(|err| err.to_string())?);
            } else {
                println!("Media Tools Status:");
                if status.ffmpeg.available {
                    println!("  ffmpeg: Installed ({})", status.ffmpeg.path.as_deref().unwrap_or(""));
                } else {
                    println!("  ffmpeg: Not installed (required for MP4 remuxing / fMP4 concatenation)");
                }
                if status.ytdlp.available {
                    println!("  yt-dlp: Installed ({})", status.ytdlp.path.as_deref().unwrap_or(""));
                } else {
                    println!("  yt-dlp: Not installed (run 'forge-dm media install-ytdlp --consent' to install)");
                }
            }
            Ok(0)
        }
        MediaCommand::Download { url, dir, name } => {
            let save_dir = dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let default_name = format!("stream_{}.mp4", uuid::Uuid::new_v4().simple());
            let filename = name.unwrap_or(default_name);
            let output_file = save_dir.join(&filename);

            let bytes_downloaded = match lock::attach(data_dir).map_err(|err| err.to_string())? {
                lock::Attach::Forward { addr, token } => {
                    let req_body = serde_json::json!({
                        "url": url,
                        "output_path": output_file.to_string_lossy(),
                    })
                    .to_string();
                    let (st, text) = control::control_exchange(
                        addr,
                        &token,
                        "POST",
                        "/v1/media/download",
                        &req_body,
                        None,
                        None,
                    )
                    .await
                    .map_err(|err| err.to_string())?;
                    if st != 200 {
                        return Err(text);
                    }
                    let res: serde_json::Value = serde_json::from_str(&text).map_err(|err| err.to_string())?;
                    res.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0)
                }
                lock::Attach::Owner(_owner) => {
                    let client = forgedownloadmanager_core::media::create_media_client().map_err(|e| e.to_string())?;
                    let cancel = CancellationToken::new();
                    println!("Downloading media stream segments...");
                    download_media_url(&client, &url, &output_file, &cancel)
                        .await
                        .map_err(|err| err.to_string())?
                }
            };
            println!("Stream downloaded ({bytes_downloaded} bytes) and saved to: {}", output_file.display());
            Ok(0)
        }
        MediaCommand::InstallYtdlp { consent } => {
            if !consent {
                return Err("Installing yt-dlp plugin requires explicit user consent.\nRun: forge-dm media install-ytdlp --consent".into());
            }
            println!("Downloading official yt-dlp release binary to {}...", plugins_dir.display());
            let path = YtDlpPlugin::install_plugin(None, &plugins_dir, consent)
                .await
                .map_err(|err| err.to_string())?;
            println!("yt-dlp successfully installed to: {}", path.display());
            Ok(0)
        }
    }
}