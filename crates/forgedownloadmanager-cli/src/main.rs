use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use forgedownloadmanager_core::checksum::random_token;
use forgedownloadmanager_core::config::{EngineConfig, DEFAULT_USER_AGENT};
use forgedownloadmanager_core::control::{self, interrupted_label};
use forgedownloadmanager_core::model::{AddRequest, ChecksumAlgo, Download, DownloadStatus, DuplicatePolicy};
use forgedownloadmanager_core::store::Store;
use forgedownloadmanager_core::filename::ensure_direct_child;
use forgedownloadmanager_core::{insert_request, Engine, EngineEvent};

mod lock;

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
        } => {
            let mut request = AddRequest::new(url, dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))));
            request.segments = segments;
            request.filename = name;
            request.start = !no_start;
            request.referer = referer;
            request.user_agent = user_agent;
            request.headers = parse_headers(&headers)?;
            request.insecure_tls = insecure_tls;
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
    }
}

async fn add_offline(data_dir: &Path, request: AddRequest) -> Result<i32, String> {
    let _owner = match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Owner(owner) => owner,
        lock::Attach::Forward { addr, token } => {
            let body = serde_json::json!({
                "url": request.url,
                "save_dir": request.save_dir,
                "filename": request.filename,
                "segments": request.segments,
                "start": false,
                "referer": request.referer,
                "user_agent": request.user_agent,
                "headers": request.headers,
                "insecure_tls": request.insecure_tls,
                "duplicate_policy": request.duplicate_policy,
                "checksum_algo": request.checksum_algo,
                "checksum_expected": request.checksum_expected,
            });
            let id = forward_add(addr, &token, &body.to_string()).await?;
            println!("{id}");
            return Ok(0);
        }
    };
    let store = Store::open(&data_dir.join("forge.db")).map_err(|err| err.to_string())?;
    let segments = store.setting_u64("default_segments").map_err(|err| err.to_string())?.unwrap_or(8) as u32;
    let ua = store.setting_string("user_agent").map_err(|err| err.to_string())?.unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());
    let id = insert_request(&store, &request, segments, &ua).map_err(|err| err.to_string())?;
    println!("{id}");
    Ok(0)
}

async fn add_online(data_dir: &Path, request: AddRequest) -> Result<i32, String> {
    match lock::attach(data_dir).map_err(|err| err.to_string())? {
        lock::Attach::Forward { addr, token } => {
            let body = serde_json::json!({
                "url": request.url,
                "save_dir": request.save_dir,
                "filename": request.filename,
                "segments": request.segments,
                "start": true,
                "referer": request.referer,
                "user_agent": request.user_agent,
                "headers": request.headers,
                "insecure_tls": request.insecure_tls,
                "duplicate_policy": request.duplicate_policy,
                "checksum_algo": request.checksum_algo,
                "checksum_expected": request.checksum_expected,
            });
            let id = forward_add(addr, &token, &body.to_string()).await?;
            println!("{id}");
            let _ = std::io::stdout().flush();
            wait_remote(addr, &token, id).await
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
            post_ok(addr, &token, &format!("/v1/downloads/{resolved}/resume"), "").await?;
            wait_remote(addr, &token, resolved).await
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

async fn post_ok(addr: std::net::SocketAddr, token: &str, path: &str, body: &str) -> Result<(), String> {
    let (status, text) = control::control_exchange(addr, token, "POST", path, body, None, None)
        .await
        .map_err(|err| err.to_string())?;
    if status >= 400 {
        return Err(if text.is_empty() { format!("control API returned {status}") } else { text });
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

async fn wait_remote(addr: std::net::SocketAddr, token: &str, id: uuid::Uuid) -> Result<i32, String> {
    loop {
        if ctrl_c_pending() {
            return Ok(130);
        }
        let rows = remote_downloads(addr, token).await?;
        let focused = rows.iter().find(|download| download.id == id);
        if let Some(download) = focused {
            if matches!(download.status, DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Stopped)
                && !rows.iter().any(|row| row.status.is_active())
            {
                return Ok(if download.status == DownloadStatus::Failed { 2 } else { 0 });
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
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
    if let Some(dir) = std::env::var_os("FORGE_DM_DATA") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("ForgeDownloadManager");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join("Library/Application Support/ForgeDownloadManager");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(xdg).join("forgedownloadmanager");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".local/share/forgedownloadmanager");
        }
    }
    PathBuf::from("forgedownloadmanager-data")
}