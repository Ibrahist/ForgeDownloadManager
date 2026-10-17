use std::path::Path;
use std::time::Duration;

use forgedownloadmanager_core::control::{self, control_exchange};
use forgedownloadmanager_core::model::{DownloadStatus, SiteRule, SiteRuleAction};
use forgedownloadmanager_core::testutil::{Behavior, Mock};
use forgedownloadmanager_core::{Engine, EngineConfig};
use uuid::Uuid;

async fn engine_in(dir: &Path) -> Engine {
    let mut cfg = EngineConfig::new(dir.join("forge.db"));
    cfg.min_segment_bytes = Some(1024);
    cfg.flush_interval = Duration::from_millis(50);
    Engine::open(cfg).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn site_rules_crud_and_matching() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path()).await;

    // Initially no site rules
    let rules = engine.list_site_rules().unwrap();
    assert_eq!(rules.len(), 0);

    // Create rules
    let r1 = SiteRule::new("*.github.com", SiteRuleAction::Intercept);
    let r2 = SiteRule::new("ads.example.com", SiteRuleAction::Exclude);
    let r3 = SiteRule::new("*tracker*", SiteRuleAction::Exclude);

    let id1 = r1.id;
    let id2 = r2.id;
    let id3 = r3.id;

    engine.create_site_rule(r1).unwrap();
    engine.create_site_rule(r2).unwrap();
    engine.create_site_rule(r3).unwrap();

    let listed = engine.list_site_rules().unwrap();
    assert_eq!(listed.len(), 3);

    // Get rule by id
    let fetched = engine.get_site_rule(id1).unwrap().expect("found");
    assert_eq!(fetched.pattern, "*.github.com");
    assert_eq!(fetched.action, SiteRuleAction::Intercept);

    // Test match_site_rule
    assert_eq!(
        engine.match_site_rule("raw.github.com").unwrap(),
        Some(SiteRuleAction::Intercept)
    );
    assert_eq!(
        engine.match_site_rule("github.com").unwrap(),
        Some(SiteRuleAction::Intercept)
    );
    assert_eq!(
        engine.match_site_rule("ads.example.com").unwrap(),
        Some(SiteRuleAction::Exclude)
    );
    assert_eq!(
        engine.match_site_rule("user-tracker-service.net").unwrap(),
        Some(SiteRuleAction::Exclude)
    );
    assert_eq!(
        engine.match_site_rule("clean-site.org").unwrap(),
        None
    );

    // Delete rule
    engine.delete_site_rule(id2).unwrap();
    assert!(engine.get_site_rule(id2).unwrap().is_none());
    assert_eq!(engine.list_site_rules().unwrap().len(), 2);

    // After deletion, ads.example.com no longer matches
    assert_eq!(engine.match_site_rule("ads.example.com").unwrap(), None);

    // Delete remaining rules
    engine.delete_site_rule(id1).unwrap();
    engine.delete_site_rule(id3).unwrap();
    assert_eq!(engine.list_site_rules().unwrap().len(), 0);

    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_api_site_rules_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path()).await;
    let token = "test-phase5-token".to_string();
    let addr = control::serve(engine.clone(), token.clone()).await.unwrap();

    // GET /v1/site-rules -> empty initially
    let (status, body) = control_exchange(addr, &token, "GET", "/v1/site-rules", "", None, None)
        .await
        .unwrap();
    assert_eq!(status, 200);
    let rules: Vec<SiteRule> = serde_json::from_str(&body).unwrap();
    assert_eq!(rules.len(), 0);

    // POST /v1/site-rules -> create rule
    let new_rule = serde_json::json!({
        "pattern": "*.mirror.kernel.org",
        "action": "intercept"
    });
    let (status, body) = control_exchange(
        addr,
        &token,
        "POST",
        "/v1/site-rules",
        &new_rule.to_string(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    let created: SiteRule = serde_json::from_str(&body).unwrap();
    assert_eq!(created.pattern, "*.mirror.kernel.org");
    assert_eq!(created.action, SiteRuleAction::Intercept);

    // GET /v1/site-rules -> contains created rule
    let (status, body) = control_exchange(addr, &token, "GET", "/v1/site-rules", "", None, None)
        .await
        .unwrap();
    assert_eq!(status, 200);
    let rules: Vec<SiteRule> = serde_json::from_str(&body).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].id, created.id);

    // DELETE /v1/site-rules/:id
    let (status, _) = control_exchange(
        addr,
        &token,
        "DELETE",
        &format!("/v1/site-rules/{}", created.id),
        "",
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(status, 200);

    // Verify deleted
    let (status, body) = control_exchange(addr, &token, "GET", "/v1/site-rules", "", None, None)
        .await
        .unwrap();
    assert_eq!(status, 200);
    let rules: Vec<SiteRule> = serde_json::from_str(&body).unwrap();
    assert_eq!(rules.len(), 0);

    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extension_download_cookies_and_referer_forwarding() {
    let mut b = Behavior::new(vec![7u8; 4000]);
    b.require_basic = None;
    let mock = Mock::spawn(b).await;
    let dir = tempfile::tempdir().unwrap();
    let save = dir.path().join("save");
    let engine = engine_in(dir.path()).await;
    let token = "token-ext".to_string();
    let addr = control::serve(engine.clone(), token.clone()).await.unwrap();

    // Extension sends POST /v1/downloads with cookies, referer, and custom filename
    let add_body = serde_json::json!({
        "url": mock.url("/file"),
        "dir": save.to_str().unwrap(),
        "filename": "intercepted_file.bin",
        "cookies": [
            {"name": "session_id", "value": "xyz123"},
            {"name": "forge_auth", "value": "securetoken"}
        ],
        "referer": "https://example.com/download-page",
        "user_agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ForgeBrowserExt/1.0"
    });

    let (status, body) = control_exchange(
        addr,
        &token,
        "POST",
        "/v1/downloads",
        &add_body.to_string(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    let resp: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = Uuid::parse_str(resp.get("id").unwrap().as_str().unwrap()).unwrap();

    // Verify download row
    let dl = engine.get(id).unwrap();
    assert_eq!(dl.filename, "intercepted_file.bin");
    assert_eq!(dl.referer.as_deref(), Some("https://example.com/download-page"));
    assert_eq!(
        dl.user_agent.as_deref(),
        Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64) ForgeBrowserExt/1.0")
    );
    assert!(dl.cookies.iter().any(|(k, v)| k == "session_id" && v == "xyz123"));
    assert!(dl.cookies.iter().any(|(k, v)| k == "forge_auth" && v == "securetoken"));

    // Wait for completion
    let finished = engine.wait_for_terminal(id).await.unwrap();
    assert_eq!(finished.status, DownloadStatus::Completed);
    assert_eq!(finished.downloaded_bytes, 4000);

    engine.shutdown().await;
}
