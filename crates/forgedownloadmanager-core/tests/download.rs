use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use forgedownloadmanager_core::checksum::{self, hash_file};
use forgedownloadmanager_core::control::{self, control_exchange};
use forgedownloadmanager_core::model::{AddRequest, ChecksumAlgo, Download, DownloadStatus};
use forgedownloadmanager_core::testutil::{Behavior, Mock};
use forgedownloadmanager_core::{Engine, EngineConfig};

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i.wrapping_mul(17).wrapping_add(3)) % 251) as u8).collect()
}

fn file_hash(bytes: &[u8]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.bin");
    std::fs::write(&path, bytes).unwrap();
    hash_file(&path, ChecksumAlgo::Sha256).unwrap()
}

async fn engine_in(dir: &Path, concurrent: usize) -> Engine {
    let mut cfg = EngineConfig::new(dir.join("forge.db"));
    cfg.min_segment_bytes = Some(1024);
    cfg.flush_interval = Duration::from_millis(100);
    cfg.max_concurrent_downloads = Some(concurrent);
    Engine::open(cfg).await.unwrap()
}

async fn run_until(engine: &Engine, url: &str, save: &Path, segments: u32) -> Download {
    let mut request = AddRequest::new(url, save);
    request.segments = Some(segments);
    let id = engine.add(request).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), engine.wait_for_terminal(id))
        .await
        .expect("download timed out")
        .unwrap()
}

fn assert_hash(download: &Download, bytes: &[u8]) {
    assert_eq!(download.status, DownloadStatus::Completed, "{:?}", download.error_message);
    let actual = hash_file(&download.final_path(), ChecksumAlgo::Sha256).unwrap();
    assert_eq!(actual, file_hash(bytes));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_segment_hash_when_size_is_not_divisible() {
    let bytes = pattern(7550);
    let mock = Mock::spawn(Behavior::new(bytes.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let save = dir.path().join("save");
    let engine = engine_in(dir.path(), 2).await;
    let download = run_until(&engine, &mock.url("/file"), &save, 3).await;
    assert_hash(&download, &bytes);
    assert!(download.segment_count >= 2, "segments {}", download.segment_count);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_range_download_matches_and_resume_is_rejected() {
    let bytes = pattern(180_000);
    let mut behavior = Behavior::new(bytes.clone());
    behavior.accept_ranges = false;
    let mock = Mock::spawn(behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let save = dir.path().join("save");
    let engine = engine_in(dir.path(), 2).await;
    let download = run_until(&engine, &mock.url("/file"), &save, 4).await;
    assert_hash(&download, &bytes);
    let err = engine.resume(download.id).await.unwrap_err();
    assert!(err.to_string().contains("complete"), "{err}");

    let mut slow = Behavior::new(pattern(400_000));
    slow.accept_ranges = false;
    slow.delay = Duration::from_millis(20);
    slow.chunk_size = 4 * 1024;
    let slow_mock = Mock::spawn(slow).await;
    let mut request = AddRequest::new(slow_mock.url("/file"), &save);
    request.segments = Some(1);
    let id = engine.add(request).await.unwrap();
    let saw_bytes = wait_until(Duration::from_secs(10), || {
        let row = engine.get(id).unwrap();
        row.downloaded_bytes > 0 && row.status != DownloadStatus::Completed
    })
    .await;
    assert!(saw_bytes, "slow single stream made no observable progress");
    engine.pause(id).await.unwrap();
    engine.resume(id).await.unwrap();
    let failed = tokio::time::timeout(Duration::from_secs(20), engine.wait_for_terminal(id))
        .await
        .expect("resume timed out")
        .unwrap();
    assert_eq!(failed.status, DownloadStatus::Failed);
    assert_eq!(failed.error_code.as_deref(), Some("not_resumable_interrupted"));
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redirect_records_final_url_and_hash() {
    let bytes = pattern(90_000);
    let mock = Mock::spawn(Behavior::new(bytes.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 2).await;
    let download = run_until(&engine, &mock.url("/redirect"), &dir.path().join("save"), 2).await;
    assert_hash(&download, &bytes);
    let final_url = download.final_url.unwrap();
    assert!(final_url.contains("/file"), "{final_url}");
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnect_then_resume_matches_hash() {
    let bytes = pattern(300_000);
    let mut behavior = Behavior::new(bytes.clone());
    behavior.disconnect_after = Some(24 * 1024);
    behavior.disconnects_left = Arc::new(AtomicUsize::new(1));
    behavior.chunk_size = 8 * 1024;
    let mock = Mock::spawn(behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 2).await;
    let first = run_until(&engine, &mock.url("/file"), &dir.path().join("save"), 2).await;
    assert_eq!(first.status, DownloadStatus::Failed, "first attempt should break");
    assert!(first.downloaded_bytes > 0, "flush should have kept a prefix");
    engine.resume(first.id).await.unwrap();
    let second = tokio::time::timeout(Duration::from_secs(30), engine.wait_for_terminal(first.id))
        .await
        .expect("resume timed out")
        .unwrap();
    assert_hash(&second, &bytes);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn etag_change_keeps_the_partial() {
    let bytes = pattern(500_000);
    let mut behavior = Behavior::new(bytes);
    behavior.delay = Duration::from_millis(15);
    behavior.chunk_size = 8 * 1024;
    let mock = Mock::spawn(behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 2).await;
    let mut request = AddRequest::new(mock.url("/file"), dir.path().join("save"));
    request.segments = Some(2);
    let id = engine.add(request).await.unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || engine.get(id).unwrap().downloaded_bytes > 0).await,
        "no progress"
    );
    engine.pause(id).await.unwrap();
    mock.behavior.lock().unwrap().etag = Some("\"v2\"".into());
    engine.resume(id).await.unwrap();
    let failed = tokio::time::timeout(Duration::from_secs(20), engine.wait_for_terminal(id))
        .await
        .expect("etag check timed out")
        .unwrap();
    assert_eq!(failed.error_code.as_deref(), Some("remote_changed"));
    assert!(failed.partial_path.exists() || dir.path().join("save").join(format!("{}.forgepart", failed.filename)).exists());
    let again = engine.resume(id).await.unwrap_err();
    assert!(again.to_string().contains("remote file changed"), "{again}");
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gone_link_uses_the_refresh_message() {
    let mut behavior = Behavior::new(pattern(1000));
    behavior.force_status = Some(410);
    let mock = Mock::spawn(behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 1).await;
    let download = run_until(&engine, &mock.url("/file"), &dir.path().join("save"), 1).await;
    assert_eq!(download.status, DownloadStatus::Failed);
    assert_eq!(download.error_code.as_deref(), Some("link_expired"));
    assert_eq!(download.error_message.as_deref(), Some("link expired, refresh from browser"));
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunked_unknown_length_matches_hash() {
    let bytes = pattern(70_000);
    let mut behavior = Behavior::new(bytes.clone());
    behavior.accept_ranges = false;
    behavior.chunked = true;
    behavior.chunk_size = 4096;
    let mock = Mock::spawn(behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 1).await;
    let download = run_until(&engine, &mock.url("/file"), &dir.path().join("save"), 4).await;
    assert_hash(&download, &bytes);
    assert!(!download.resumable);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_file_completes_without_a_body() {
    let mock = Mock::spawn(Behavior::new(Vec::new())).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 1).await;
    let download = run_until(&engine, &mock.url("/file"), &dir.path().join("save"), 4).await;
    assert_eq!(download.status, DownloadStatus::Completed, "{:?}", download.error_message);
    assert_eq!(std::fs::metadata(download.final_path()).unwrap().len(), 0);
    let gets = mock.requests.lock().unwrap().iter().filter(|line| line.starts_with("GET")).count();
    assert_eq!(gets, 0, "Content-Length 0 must not download a body: {:?}", mock.requests.lock().unwrap());
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn content_disposition_cannot_escape_the_save_dir() {
    let bytes = pattern(4096);
    let mut behavior = Behavior::new(bytes.clone());
    behavior.content_disposition = Some("attachment; filename=\"../../evil.bin\"".into());
    let mock = Mock::spawn(behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let save = dir.path().join("save");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let engine = engine_in(dir.path(), 1).await;
    let download = run_until(&engine, &mock.url("/file"), &save, 1).await;
    assert_hash(&download, &bytes);
    assert_eq!(download.filename, "evil.bin");
    assert!(save.join("evil.bin").exists());
    assert!(!outside.join("evil.bin").exists());
    assert!(!dir.path().join("evil.bin").exists());
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_holds_progress_then_resume_matches() {
    let bytes = pattern(900_000);
    let mut behavior = Behavior::new(bytes.clone());
    behavior.delay = Duration::from_millis(25);
    behavior.chunk_size = 4 * 1024;
    let mock = Mock::spawn(behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 2).await;
    let mut request = AddRequest::new(mock.url("/file"), dir.path().join("save"));
    request.segments = Some(4);
    let id = engine.add(request).await.unwrap();
    assert!(
        wait_until(Duration::from_secs(15), || {
            let row = engine.get(id).unwrap();
            row.downloaded_bytes > 0 && row.total_bytes.is_some_and(|total| row.downloaded_bytes < total)
        })
        .await,
        "finished before pause"
    );
    engine.pause(id).await.unwrap();
    let paused = engine.get(id).unwrap();
    assert_eq!(paused.status, DownloadStatus::Paused);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let held = engine.get(id).unwrap();
    assert_eq!(held.downloaded_bytes, paused.downloaded_bytes);
    engine.resume(id).await.unwrap();
    let done = tokio::time::timeout(Duration::from_secs(30), engine.wait_for_terminal(id))
        .await
        .expect("resume timed out")
        .unwrap();
    assert_hash(&done, &bytes);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn max_concurrent_keeps_the_second_download_queued() {
    let bytes = pattern(32_768);
    let mut behavior = Behavior::new(bytes.clone());
    behavior.stall = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let stall = Arc::clone(&behavior.stall);
    let mock = Mock::spawn(behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 1).await;
    let save = dir.path().join("save");
    let first = {
        let mut request = AddRequest::new(mock.url("/file"), &save);
        request.filename = Some("one.bin".into());
        engine.add(request).await.unwrap()
    };
    assert!(
        wait_until(Duration::from_secs(10), || {
            matches!(
                engine.get(first).unwrap().status,
                DownloadStatus::Downloading | DownloadStatus::Probing
            )
        })
        .await,
        "first download did not start"
    );
    let second = {
        let mut request = AddRequest::new(mock.url("/file"), &save);
        request.filename = Some("two.bin".into());
        engine.add(request).await.unwrap()
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(engine.get(second).unwrap().status, DownloadStatus::Queued);
    stall.store(false, Ordering::Release);
    let done = tokio::time::timeout(Duration::from_secs(20), engine.wait_until_idle())
        .await
        .expect("queue did not drain");
    done.unwrap();
    assert_hash(&engine.get(first).unwrap(), &bytes);
    assert_hash(&engine.get(second).unwrap(), &bytes);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checksum_match_and_mismatch() {
    let bytes = pattern(48_000);
    let expected = file_hash(&bytes);
    let mock = Mock::spawn(Behavior::new(bytes.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let save = dir.path().join("save");
    let engine = engine_in(dir.path(), 2).await;
    let mut ok = AddRequest::new(mock.url("/file"), &save);
    ok.filename = Some("ok.bin".into());
    ok.checksum_algo = Some(ChecksumAlgo::Sha256);
    ok.checksum_expected = Some(expected);
    let id = engine.add(ok).await.unwrap();
    let download = tokio::time::timeout(Duration::from_secs(20), engine.wait_for_terminal(id))
        .await
        .unwrap()
        .unwrap();
    assert_hash(&download, &bytes);

    let mut bad = AddRequest::new(mock.url("/file"), &save);
    bad.filename = Some("bad.bin".into());
    bad.checksum_algo = Some(ChecksumAlgo::Sha256);
    bad.checksum_expected = Some("deadbeef".into());
    let bad_id = engine.add(bad).await.unwrap();
    let failed = tokio::time::timeout(Duration::from_secs(20), engine.wait_for_terminal(bad_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, DownloadStatus::Failed);
    assert_eq!(failed.error_code.as_deref(), Some("checksum_mismatch"));
    assert!(!failed.final_path().exists());
    assert!(failed.partial_path.exists(), "partial {}", failed.partial_path.display());
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_api_rejects_bad_callers_and_pauses() {
    let bytes = pattern(8192);
    let mut behavior = Behavior::new(bytes);
    behavior.stall = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let stall = Arc::clone(&behavior.stall);
    let mock = Mock::spawn(behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 2).await;
    let token = checksum::random_token().unwrap();
    let addr = control::serve(engine.clone(), token.clone()).await.unwrap();

    let (status, _) = control_exchange(addr, "nope", "GET", "/v1/health", "", None, None).await.unwrap();
    assert_eq!(status, 401);
    let (status, _) = control_exchange(addr, &token, "GET", "/v1/health", "", Some("https://evil.test"), None)
        .await
        .unwrap();
    assert_eq!(status, 403);
    let (status, _) = control_exchange(addr, &token, "GET", "/v1/health", "", None, Some("evil.test")).await.unwrap();
    assert_eq!(status, 403);
    let (status, body) = control_exchange(addr, &token, "GET", "/v1/health", "", None, None).await.unwrap();
    assert_eq!(status, 200, "{body}");

    let save = dir.path().join("save");
    let payload = serde_json::json!({
        "url": mock.url("/file"),
        "save_dir": &save,
        "filename": "from-api.bin",
        "segments": 1,
        "start": true
    })
    .to_string();
    let (status, body) = control_exchange(addr, &token, "POST", "/v1/downloads", &payload, None, None)
        .await
        .unwrap();
    assert_eq!(status, 200, "{body}");
    let id: String = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        wait_until(Duration::from_secs(10), || {
            matches!(
                engine.get(id.parse().unwrap()).unwrap().status,
                DownloadStatus::Downloading | DownloadStatus::Probing
            )
        })
        .await
    );
    let (status, _) = control_exchange(addr, &token, "POST", &format!("/v1/downloads/{id}/pause"), "", None, None)
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(engine.get(id.parse().unwrap()).unwrap().status, DownloadStatus::Paused);
    let (status, list) = control_exchange(addr, &token, "GET", "/v1/downloads", "", None, None).await.unwrap();
    assert_eq!(status, 200, "{list}");
    assert!(list.contains(&id));
    stall.store(false, Ordering::Release);
    engine.shutdown().await;
}

async fn wait_until(limit: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < limit {
        if pred() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    pred()
}