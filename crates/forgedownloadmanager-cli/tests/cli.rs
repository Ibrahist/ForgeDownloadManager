use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use forgedownloadmanager_core::checksum::hash_file;
use forgedownloadmanager_core::model::{ChecksumAlgo, Download};
use forgedownloadmanager_core::testutil::{Behavior, Mock};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_forge-dm")
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i.wrapping_mul(17).wrapping_add(3)) % 251) as u8).collect()
}

fn temp_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "forge-dm-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn sha256(bytes: &[u8]) -> String {
    let dir = temp_dir("hash");
    let path = dir.join("source.bin");
    fs::write(&path, bytes).unwrap();
    let hash = hash_file(&path, ChecksumAlgo::Sha256).unwrap();
    let _ = fs::remove_dir_all(dir);
    hash
}

fn output(args: &[String]) -> std::process::Output {
    Command::new(bin()).args(args).output().expect("spawn forge-dm")
}

fn saved_hash(save: &std::path::Path) -> String {
    let file = fs::read_dir(save)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|ext| ext.to_str()) != Some("forgepart"))
        .expect("finished file");
    hash_file(&file, ChecksumAlgo::Sha256).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_downloads_a_range_file() {
    let bytes = pattern(200_000);
    let expect = sha256(&bytes);
    let mock = Mock::spawn(Behavior::new(bytes)).await;
    let data = temp_dir("data");
    let save = temp_dir("save");
    let args = vec![
        "--data-dir".into(),
        data.display().to_string(),
        "add".into(),
        mock.url("/file"),
        "--dir".into(),
        save.display().to_string(),
        "--segments".into(),
        "4".into(),
    ];
    let result = tokio::task::spawn_blocking(move || output(&args)).await.unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(saved_hash(&save), expect);
    let _ = fs::remove_dir_all(data);
    let _ = fs::remove_dir_all(save);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_and_run_recovers_the_byte_range() {
    let bytes = pattern(16 * 1024 * 1024);
    let expect = sha256(&bytes);
    let mut behavior = Behavior::new(bytes);
    behavior.delay = Duration::from_millis(80);
    behavior.chunk_size = 64 * 1024;
    let mock = Mock::spawn(behavior).await;
    let data = temp_dir("crash-data");
    let save = temp_dir("crash-save");
    let mut child = Command::new(bin())
        .args([
            "--data-dir",
            &data.display().to_string(),
            "add",
            &mock.url("/file"),
            "--dir",
            &save.display().to_string(),
            "--segments",
            "4",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("download finished or stalled before a partial flush");
        }
        if child.try_wait().unwrap().is_some() {
            panic!("add exited before it could be killed");
        }
        let data_arg = data.display().to_string();
        let listed = tokio::task::spawn_blocking(move || {
            Command::new(bin()).args(["--data-dir", &data_arg, "list", "--json"]).output().unwrap()
        })
        .await
        .unwrap();
        if listed.status.success() {
            if let Ok(rows) = serde_json::from_slice::<Vec<Download>>(&listed.stdout) {
                if rows.iter().any(|row| {
                    row.downloaded_bytes > 0 && row.total_bytes.is_some_and(|total| row.downloaded_bytes < total)
                }) {
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    // The default flush is one second. Stay in this window before the process is killed.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    if child.try_wait().unwrap().is_some() {
        panic!("add finished before the crash point");
    }
    child.kill().unwrap();
    let _ = child.wait();
    let data_run = data.display().to_string();
    let finished = tokio::task::spawn_blocking(move || {
        Command::new(bin()).args(["--data-dir", &data_run, "run"]).output().unwrap()
    })
    .await
    .unwrap();
    assert!(finished.status.success(), "{}", String::from_utf8_lossy(&finished.stderr));
    assert_eq!(saved_hash(&save), expect);
    let _ = fs::remove_dir_all(&data);
    let _ = fs::remove_dir_all(&save);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_process_can_pause_and_resume() {
    // Slow enough that pause lands while bytes remain. A fast file can finish,
    // shut the listener down, and reset the following resume connection.
    let bytes = pattern(4_000_000);
    let expect = sha256(&bytes);
    let mut behavior = Behavior::new(bytes);
    behavior.delay = Duration::from_millis(40);
    behavior.chunk_size = 4 * 1024;
    let mock = Mock::spawn(behavior).await;
    let data = temp_dir("pause-data");
    let save = temp_dir("pause-save");
    let mut child = Command::new(bin())
        .args([
            "--data-dir",
            &data.display().to_string(),
            "add",
            &mock.url("/file"),
            "--dir",
            &save.display().to_string(),
            "--segments",
            "4",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let id = tokio::task::spawn_blocking(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        line.trim().to_string()
    })
    .await
    .unwrap();
    assert!(id.len() >= 8, "id `{id}`");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("no progress before pause");
        }
        let data_arg = data.display().to_string();
        let listed = tokio::task::spawn_blocking(move || {
            Command::new(bin()).args(["--data-dir", &data_arg, "list", "--json"]).output().unwrap()
        })
        .await
        .unwrap();
        if let Ok(rows) = serde_json::from_slice::<Vec<Download>>(&listed.stdout) {
            if rows.iter().any(|row| row.downloaded_bytes > 0 && row.status == forgedownloadmanager_core::model::DownloadStatus::Downloading)
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let data_pause = data.display().to_string();
    let id_pause = id.clone();
    let paused = tokio::task::spawn_blocking(move || {
        Command::new(bin()).args(["--data-dir", &data_pause, "pause", &id_pause]).output().unwrap()
    })
    .await
    .unwrap();
    assert!(paused.status.success(), "{}", String::from_utf8_lossy(&paused.stderr));
    tokio::time::sleep(Duration::from_millis(400)).await;
    let data_resume = data.display().to_string();
    let id_resume = id.clone();
    let resumed = tokio::task::spawn_blocking(move || {
        Command::new(bin())
            .args(["--data-dir", &data_resume, "resume", &id_resume])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
    let finished = tokio::task::spawn_blocking(move || child.wait()).await.unwrap().unwrap();
    assert!(finished.success(), "add exited {finished}");
    assert_eq!(saved_hash(&save), expect);
    let _ = fs::remove_dir_all(&data);
    let _ = fs::remove_dir_all(&save);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_messaging_host_stdio_exchange() {
    use std::io::{Read, Write};

    let data = temp_dir("host-data");
    let mut child = Command::new(bin())
        .args(["--data-dir", data.to_str().unwrap(), "host"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn host");

    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");

    // Helper: send message
    let send_msg = |stdin: &mut std::process::ChildStdin, val: serde_json::Value| {
        let bytes = serde_json::to_vec(&val).unwrap();
        let len = (bytes.len() as u32).to_ne_bytes();
        stdin.write_all(&len).unwrap();
        stdin.write_all(&bytes).unwrap();
        stdin.flush().unwrap();
    };

    // Helper: read response
    let read_msg = |stdout: &mut std::process::ChildStdout| -> serde_json::Value {
        let mut len_bytes = [0u8; 4];
        stdout.read_exact(&mut len_bytes).unwrap();
        let len = u32::from_ne_bytes(len_bytes) as usize;
        let mut buf = vec![0u8; len];
        stdout.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    };

    // 1. Send Ping
    send_msg(&mut stdin, serde_json::json!({ "type": "ping" }));
    let resp = read_msg(&mut stdout);
    assert_eq!(resp.get("type").and_then(|v| v.as_str()), Some("pong"));
    assert_eq!(resp.get("version").and_then(|v| v.as_str()), Some(env!("CARGO_PKG_VERSION")));

    // 2. Send SiteRules
    send_msg(&mut stdin, serde_json::json!({ "type": "site_rules" }));
    let resp = read_msg(&mut stdout);
    assert_eq!(resp.get("type").and_then(|v| v.as_str()), Some("site_rules"));
    assert!(resp.get("rules").unwrap().is_array());

    // 3. Send Add (Offline SQLite queuing)
    send_msg(&mut stdin, serde_json::json!({
        "type": "add",
        "url": "https://example.com/test_archive.zip",
        "filename": "archive.zip",
        "referer": "https://example.com",
        "cookies": [{"name": "auth", "value": "secret"}]
    }));
    let resp = read_msg(&mut stdout);
    assert_eq!(resp.get("type").and_then(|v| v.as_str()), Some("add_response"));
    assert_eq!(resp.get("success").and_then(|v| v.as_bool()), Some(true));
    assert!(resp.get("id").unwrap().is_string());
    assert_eq!(resp.get("offline").and_then(|v| v.as_bool()), Some(true));

    // Drop stdin to signal EOF and close host
    drop(stdin);
    let status = child.wait().expect("wait host");
    assert!(status.success());

    let _ = fs::remove_dir_all(&data);
}

#[test]
fn media_tools_cli_json() {
    let data = temp_dir("media-tools");
    let args = vec![
        "--data-dir".into(),
        data.display().to_string(),
        "media".into(),
        "tools".into(),
        "--json".into(),
    ];
    let res = output(&args);
    assert!(res.status.success(), "{}", String::from_utf8_lossy(&res.stderr));
    let val: serde_json::Value = serde_json::from_slice(&res.stdout).expect("valid json");
    assert!(val.get("ffmpeg").is_some());
    assert!(val.get("ytdlp").is_some());
    let _ = fs::remove_dir_all(&data);
}

#[test]
fn media_install_ytdlp_requires_consent() {
    let data = temp_dir("media-consent");
    let args = vec![
        "--data-dir".into(),
        data.display().to_string(),
        "media".into(),
        "install-ytdlp".into(),
    ];
    let res = output(&args);
    assert!(!res.status.success());
    let stderr = String::from_utf8_lossy(&res.stderr);
    assert!(stderr.contains("consent") || stderr.contains("Consent"));
    let _ = fs::remove_dir_all(&data);
}