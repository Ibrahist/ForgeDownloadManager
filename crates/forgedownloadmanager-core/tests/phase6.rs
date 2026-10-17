use std::fs;
use std::path::Path;
use std::time::Duration;

use forgedownloadmanager_core::control::control_exchange;
use forgedownloadmanager_core::media::{
    create_media_client, resolve_media_segments, FfmpegRemuxer, MediaProbeResult, MediaToolsStatus,
    YtDlpPlugin,
};
use forgedownloadmanager_core::{CancellationToken, Engine, EngineConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone)]
struct RouteServer {
    addr: std::net::SocketAddr,
    shutdown: CancellationToken,
}

impl RouteServer {
    async fn spawn(routes: Vec<(&'static str, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((mut socket, _)) = accepted else { continue };
                        let routes = routes.clone();
                        tokio::spawn(async move {
                            let mut buf = [0u8; 4096];
                            let n = match socket.read(&mut buf).await {
                                Ok(n) if n > 0 => n,
                                _ => return,
                            };
                            let req_str = String::from_utf8_lossy(&buf[..n]);
                            let req_line = req_str.lines().next().unwrap_or("");
                            let path = req_line.split_whitespace().nth(1).unwrap_or("/");

                            if let Some((_, body)) = routes.iter().find(|(p, _)| *p == path) {
                                let resp = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    body.len()
                                );
                                let _ = socket.write_all(resp.as_bytes()).await;
                                let _ = socket.write_all(body).await;
                            } else {
                                let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                                let _ = socket.write_all(resp.as_bytes()).await;
                            }
                        });
                    }
                }
            }
        });

        Self { addr, shutdown }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

async fn engine_in(dir: &Path) -> Engine {
    let mut cfg = EngineConfig::new(dir.join("forge.db"));
    cfg.min_segment_bytes = Some(1024);
    cfg.flush_interval = Duration::from_millis(50);
    cfg.max_concurrent_downloads = Some(3);
    Engine::open(cfg).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hls_master_probe_and_download() {
    let master_m3u8 = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360
360p.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720,CODECS="avc1.4d401f,mp4a.40.2"
720p.m3u8
"#;

    let media_m3u8 = r#"#EXTM3U
#EXT-X-TARGETDURATION:4
#EXTINF:4.0,
seg0.ts
#EXTINF:4.0,
seg1.ts
#EXT-X-ENDLIST
"#;

    let seg0 = vec![0x47u8; 1200];
    let seg1 = vec![0x47u8; 1800];

    let server = RouteServer::spawn(vec![
        ("/hls/master.m3u8", master_m3u8.as_bytes().to_vec()),
        ("/hls/720p.m3u8", media_m3u8.as_bytes().to_vec()),
        ("/hls/seg0.ts", seg0.clone()),
        ("/hls/seg1.ts", seg1.clone()),
    ])
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path()).await;

    // 1. Probe the master playlist
    let master_url = server.url("/hls/master.m3u8");
    let probe = engine.probe_media(&master_url).await.expect("probe media");
    assert_eq!(probe.media_type, "hls_master");
    assert_eq!(probe.variants.len(), 2);
    let best = probe.best_url.expect("best variant");
    assert!(best.ends_with("/hls/720p.m3u8"));

    // 2. Download and remux stream
    let output_file = dir.path().join("video.ts");
    let bytes = engine
        .download_media_url(&master_url, &output_file)
        .await
        .expect("download media url");

    assert_eq!(bytes, (seg0.len() + seg1.len()) as u64);
    assert!(output_file.exists());
    let written = fs::read(&output_file).expect("read output file");
    assert_eq!(written.len(), 3000);
    assert_eq!(&written[..1200], &seg0[..]);
    assert_eq!(&written[1200..], &seg1[..]);

    server.shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dash_manifest_probe_and_segment_resolution() {
    let mpd = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT8S" type="static">
  <Period>
    <AdaptationSet mimeType="video/mp4">
      <Representation id="720p" bandwidth="2500000" width="1280" height="720">
        <SegmentTemplate initialization="init-$RepresentationID$.mp4" media="chunk-$RepresentationID$-$Number$.m4s" startNumber="1" duration="2000" timescale="1000"/>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>
"#;

    let server = RouteServer::spawn(vec![
        ("/dash/manifest.mpd", mpd.as_bytes().to_vec()),
    ])
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path()).await;

    let mpd_url = server.url("/dash/manifest.mpd");
    let probe = engine.probe_media(&mpd_url).await.expect("probe dash");
    assert_eq!(probe.media_type, "dash");
    assert_eq!(probe.duration, Some(8.0));
    assert_eq!(probe.segment_count, Some(4));
    assert_eq!(probe.variants.len(), 1);
    assert_eq!(probe.variants[0].resolution.as_deref(), Some("1280x720"));

    // Resolve segments
    let client = create_media_client().unwrap();
    let segments = resolve_media_segments(&client, &mpd_url)
        .await
        .expect("resolve segments");

    // 1 init + 4 chunks = 5 segments
    assert_eq!(segments.len(), 5);
    assert!(segments[0].ends_with("init-720p.mp4"));
    assert!(segments[1].ends_with("chunk-720p-1.m4s"));
    assert!(segments[4].ends_with("chunk-720p-4.m4s"));

    server.shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_api_media_tools_and_probe_endpoints() {
    let playlist_m3u8 = r#"#EXTM3U
#EXT-X-TARGETDURATION:5
#EXTINF:5.0,
segment1.ts
#EXT-X-ENDLIST
"#;

    let server = RouteServer::spawn(vec![
        ("/media/stream.m3u8", playlist_m3u8.as_bytes().to_vec()),
        ("/media/segment1.ts", vec![0x47; 500]),
    ])
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(dir.path()).await;

    // Start HTTP control listener
    let token = "media-test-token";
    let addr = forgedownloadmanager_core::control::serve(engine.clone(), token.to_string())
        .await
        .unwrap();

    // 1. GET /v1/media/tools
    let (status, text) = control_exchange(addr, token, "GET", "/v1/media/tools", "", None, None)
        .await
        .expect("exchange tools");
    assert_eq!(status, 200);
    let tools: MediaToolsStatus = serde_json::from_str(&text).expect("valid tools json");
    // tools should contain ffmpeg and ytdlp fields
    assert_eq!(tools.ffmpeg.available, FfmpegRemuxer::detect_ffmpeg().is_some());
    assert_eq!(tools.ytdlp.available, YtDlpPlugin::detect_ytdlp(None).is_some());

    // 2. POST /v1/media/probe
    let probe_body = serde_json::json!({
        "url": server.url("/media/stream.m3u8")
    })
    .to_string();

    let (status, text) = control_exchange(
        addr,
        token,
        "POST",
        "/v1/media/probe",
        &probe_body,
        None,
        None,
    )
    .await
    .expect("exchange probe");
    assert_eq!(status, 200);
    let probe_res: MediaProbeResult = serde_json::from_str(&text).expect("valid probe json");
    assert_eq!(probe_res.media_type, "hls_media");
    assert_eq!(probe_res.segment_count, Some(1));

    // 3. POST /v1/media/download
    let output_file = dir.path().join("dl_stream.ts");
    let dl_body = serde_json::json!({
        "segment_urls": [server.url("/media/segment1.ts")],
        "output_path": output_file.to_string_lossy(),
    })
    .to_string();

    let (status, text) = control_exchange(
        addr,
        token,
        "POST",
        "/v1/media/download",
        &dl_body,
        None,
        None,
    )
    .await
    .expect("exchange download");
    assert_eq!(status, 200);
    let dl_val: serde_json::Value = serde_json::from_str(&text).expect("valid dl json");
    assert_eq!(dl_val.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(dl_val.get("bytes").and_then(|v| v.as_u64()), Some(500));
    assert!(output_file.exists());

    server.shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ytdlp_consent_gate_enforcement() {
    let tmp = tempfile::tempdir().unwrap();
    // Attempting to install without explicit consent must immediately fail
    let res = YtDlpPlugin::install_plugin(None, tmp.path(), false).await;
    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert!(err.contains("consent"), "error was: {err}");
}

#[test]
fn ts_fallback_remuxing_order() {
    let tmp = tempfile::tempdir().unwrap();
    let seg0 = tmp.path().join("0.ts");
    let seg1 = tmp.path().join("1.ts");
    let seg2 = tmp.path().join("2.ts");
    let out = tmp.path().join("out.ts");

    fs::write(&seg0, b"HEADER_").unwrap();
    fs::write(&seg1, b"MIDDLE_").unwrap();
    fs::write(&seg2, b"FOOTER").unwrap();

    FfmpegRemuxer::remux_to_file(&[seg0, seg1, seg2], &out, None).expect("remux");
    let content = fs::read_to_string(&out).unwrap();
    assert_eq!(content, "HEADER_MIDDLE_FOOTER");
}
