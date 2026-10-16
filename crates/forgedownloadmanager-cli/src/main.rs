use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use forgedownloadmanager_core::checksum::random_token;
use forgedownloadmanager_core::config::{self, EngineConfig, DEFAULT_USER_AGENT};
use forgedownloadmanager_core::control::{self, interrupted_label};
use forgedownloadmanager_core::filename::ensure_direct_child;
use forgedownloadmanager_core::model::{AddRequest, ChecksumAlgo, Download, DownloadStatus, DuplicatePolicy};
use forgedownloadmanager_core::secrets::{KeyringSecrets, SecretStore};
use forgedownloadmanager_core::store::Store;
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
        Command::Auth { action } => match action {
            AuthCommand::Set { host, username } => auth_set(&data_dir, &host, &username).await,
        },
        Command::Config { action } => match action {
            ConfigCommand::Get { key } => config_get(&data_dir, &key).await,
            ConfigCommand::Set { key, value } => config_set(&data_dir, &key, &value).await,
        },
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