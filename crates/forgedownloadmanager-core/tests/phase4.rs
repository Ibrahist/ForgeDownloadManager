use std::path::Path;
use std::time::Duration;

use forgedownloadmanager_core::control::{self, control_exchange};
use forgedownloadmanager_core::grabber;
use forgedownloadmanager_core::model::{AddRequest, DownloadStatus};
use forgedownloadmanager_core::schedule::{OnCompleteAction, Queue};
use forgedownloadmanager_core::testutil::{Behavior, Mock};
use forgedownloadmanager_core::{Engine, EngineConfig};

async fn engine_in(dir: &Path, concurrent: usize) -> Engine {
    let mut cfg = EngineConfig::new(dir.join("forge.db"));
    cfg.min_segment_bytes = Some(1024);
    cfg.flush_interval = Duration::from_millis(50);
    cfg.max_concurrent_downloads = Some(concurrent);
    Engine::open(cfg).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_crud_and_reassignment() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 3).await;

    // Default queue exists initially
    let queues = engine.list_queues().unwrap();
    assert_eq!(queues.len(), 1);
    assert_eq!(queues[0].name, "Default");
    let default_id = queues[0].id;

    // Create custom queue
    let mut custom = Queue::new("NightQueue");
    custom.concurrency = 2;
    custom.speed_limit_bps = Some(100_000);
    custom.schedule_start = Some("02:00".into());
    custom.schedule_end = Some("06:00".into());
    custom.days_mask = 62;
    custom.on_complete = OnCompleteAction::None;
    engine.create_queue(custom.clone()).unwrap();

    let listed = engine.list_queues().unwrap();
    assert_eq!(listed.len(), 2);

    let by_id = engine.get_queue(custom.id).unwrap().expect("found by id");
    assert_eq!(by_id.name, "NightQueue");
    assert_eq!(by_id.concurrency, 2);
    assert_eq!(by_id.speed_limit_bps, Some(100_000));
    assert_eq!(by_id.schedule_start.as_deref(), Some("02:00"));
    assert_eq!(by_id.schedule_end.as_deref(), Some("06:00"));

    let by_name = engine.get_queue_by_name("nightqueue").unwrap().expect("found by name");
    assert_eq!(by_name.id, custom.id);

    // Update queue
    custom.concurrency = 4;
    custom.on_complete = OnCompleteAction::Quit;
    engine.update_queue(custom.clone()).unwrap();

    let updated = engine.get_queue(custom.id).unwrap().unwrap();
    assert_eq!(updated.concurrency, 4);
    assert_eq!(updated.on_complete, OnCompleteAction::Quit);

    // Add download assigned to this custom queue
    let mut req = AddRequest::new("http://example.com/test.bin", dir.path().join("save"));
    req.queue_id = Some(custom.id);
    req.start = false;
    let dl_id = engine.add(req).await.unwrap();

    let dl = engine.get(dl_id).unwrap();
    assert_eq!(dl.queue_id, custom.id);

    // Delete custom queue -> download is reassigned to default queue
    engine.delete_queue(custom.id).unwrap();
    assert!(engine.get_queue(custom.id).unwrap().is_none());

    let reassigned = engine.get(dl_id).unwrap();
    assert_eq!(reassigned.queue_id, default_id);

    // Cannot delete default queue
    let err = engine.delete_queue(default_id).unwrap_err();
    assert!(err.to_string().contains("Default"));

    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_concurrency_limits_active_jobs() {
    let mock = Mock::spawn(Behavior::new(vec![42u8; 5000])).await;
    let dir = tempfile::tempdir().unwrap();
    let save = dir.path().join("save");
    // Engine allows 4 concurrent downloads globally
    let engine = engine_in(dir.path(), 4).await;

    // Create a queue with concurrency 1
    let mut q = Queue::new("RestrictedQueue");
    q.concurrency = 1;
    engine.create_queue(q.clone()).unwrap();

    let mut r1 = AddRequest::new(mock.url("/file"), &save);
    r1.filename = Some("file1.bin".into());
    r1.queue_id = Some(q.id);
    let id1 = engine.add(r1).await.unwrap();

    let mut r2 = AddRequest::new(mock.url("/file"), &save);
    r2.filename = Some("file2.bin".into());
    r2.queue_id = Some(q.id);
    let id2 = engine.add(r2).await.unwrap();

    // Wait until both complete
    let d1 = engine.wait_for_terminal(id1).await.unwrap();
    let d2 = engine.wait_for_terminal(id2).await.unwrap();
    assert_eq!(d1.status, DownloadStatus::Completed);
    assert_eq!(d2.status, DownloadStatus::Completed);

    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_schedule_window_closed_blocks_start() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 3).await;

    // Create queue with days_mask = 0 (never open on any day!)
    let mut q = Queue::new("NeverOpenQueue");
    q.days_mask = 0;
    engine.create_queue(q.clone()).unwrap();

    let mut req = AddRequest::new("http://example.com/unreachable.bin", dir.path().join("save"));
    req.queue_id = Some(q.id);
    req.start = true;
    let id = engine.add(req).await.unwrap();

    // Give scheduler time to pump
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Must remain queued because window is closed
    let row = engine.get(id).unwrap();
    assert_eq!(row.status, DownloadStatus::Queued);

    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grabber_pattern_expansion() {
    // Numeric with zero padding
    let urls = grabber::expand_pattern("https://example.com/img_[001-003].jpg").unwrap();
    assert_eq!(
        urls,
        vec![
            "https://example.com/img_001.jpg",
            "https://example.com/img_002.jpg",
            "https://example.com/img_003.jpg",
        ]
    );

    // Alphabetical
    let urls = grabber::expand_pattern("https://example.com/vol_[a-b].zip").unwrap();
    assert_eq!(
        urls,
        vec![
            "https://example.com/vol_a.zip",
            "https://example.com/vol_b.zip",
        ]
    );

    // Multi-bracket Cartesian product
    let urls = grabber::expand_pattern("https://example.com/[x-y]_[1-2].dat").unwrap();
    assert_eq!(
        urls,
        vec![
            "https://example.com/x_1.dat",
            "https://example.com/x_2.dat",
            "https://example.com/y_1.dat",
            "https://example.com/y_2.dat",
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grabber_batch_parser() {
    let text = "
        # List of ISOs
        https://distro.org/arch.iso
        https://distro.org/fedora.iso

        # Duplicate
        https://distro.org/arch.iso
    ";
    let urls = grabber::parse_batch_urls(text).unwrap();
    assert_eq!(
        urls,
        vec![
            "https://distro.org/arch.iso".to_string(),
            "https://distro.org/fedora.iso".to_string(),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grabber_same_domain_crawler() {
    let html = r##"
        <!DOCTYPE html>
        <html>
        <body>
            <a href="/reports/q1.pdf">Q1</a>
            <a href="https://example.com/reports/q2.pdf">Q2</a>
            <a href="/images/logo.png">Logo</a>
            <a href="https://otherdomain.com/q3.pdf">External</a>
        </body>
        </html>
    "##;

    let base = url::Url::parse("https://example.com/reports/index.html").unwrap();
    let links = grabber::extract_links(html, &base);
    let str_links: Vec<String> = links.into_iter().map(|u| u.to_string()).collect();

    assert!(str_links.contains(&"https://example.com/reports/q1.pdf".to_string()));
    assert!(str_links.contains(&"https://example.com/reports/q2.pdf".to_string()));
    assert!(str_links.contains(&"https://example.com/images/logo.png".to_string()));
    assert!(str_links.contains(&"https://otherdomain.com/q3.pdf".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_api_queue_and_grabber_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path(), 3).await;
    let token = "test_phase4_token".to_string();
    let addr = control::serve(engine.clone(), token.clone()).await.unwrap();

    // 1. GET /v1/queues
    let (status, text) = control_exchange(addr, &token, "GET", "/v1/queues", "", None, None).await.unwrap();
    assert_eq!(status, 200);
    let list: Vec<Queue> = serde_json::from_str(&text).unwrap();
    assert_eq!(list.len(), 1);

    // 2. POST /v1/queues
    let create_body = serde_json::json!({
        "name": "ApiQueue",
        "concurrency": 2,
        "speed_limit_bps": 50000,
        "schedule_start": "01:00",
        "schedule_end": "05:00",
        "days_mask": 127,
        "on_complete": "none"
    });
    let (status, text) = control_exchange(addr, &token, "POST", "/v1/queues", &create_body.to_string(), None, None).await.unwrap();
    assert_eq!(status, 200);
    let created: Queue = serde_json::from_str(&text).unwrap();
    assert_eq!(created.name, "ApiQueue");
    assert_eq!(created.concurrency, 2);

    // 3. GET /v1/queues/:id
    let (status, text) = control_exchange(addr, &token, "GET", &format!("/v1/queues/{}", created.id), "", None, None).await.unwrap();
    assert_eq!(status, 200);
    let fetched: Queue = serde_json::from_str(&text).unwrap();
    assert_eq!(fetched.id, created.id);

    // 4. PUT /v1/queues/:id
    let update_body = serde_json::json!({
        "concurrency": 5
    });
    let (status, text) = control_exchange(addr, &token, "PUT", &format!("/v1/queues/{}", created.id), &update_body.to_string(), None, None).await.unwrap();
    assert_eq!(status, 200);
    let updated: Queue = serde_json::from_str(&text).unwrap();
    assert_eq!(updated.concurrency, 5);

    // 5. POST /v1/grabber/expand
    let expand_body = serde_json::json!({
        "pattern": "https://example.com/doc_[1-2].pdf"
    });
    let (status, text) = control_exchange(addr, &token, "POST", "/v1/grabber/expand", &expand_body.to_string(), None, None).await.unwrap();
    assert_eq!(status, 200);
    let urls: Vec<String> = serde_json::from_str(&text).unwrap();
    assert_eq!(urls, vec!["https://example.com/doc_1.pdf", "https://example.com/doc_2.pdf"]);

    // 6. DELETE /v1/queues/:id
    let (status, _) = control_exchange(addr, &token, "DELETE", &format!("/v1/queues/{}", created.id), "", None, None).await.unwrap();
    assert_eq!(status, 200);

    engine.shutdown().await;
}
