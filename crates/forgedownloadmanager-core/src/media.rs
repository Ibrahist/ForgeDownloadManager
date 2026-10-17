//! Media stream handling: HLS (.m3u8), DASH (.mpd), ffmpeg remuxing, and optional yt-dlp plugin.
//!
//! Phase 6 additions:
//! - HLS parser: master and media playlists, variant selection, initialization segments.
//! - DASH parser: MPD representations, SegmentTemplate / SegmentList expansion.
//! - ffmpeg detector & remuxer: -c copy stream concatenation.
//! - yt-dlp plugin: consent-gated downloader and format extractor.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::Error;
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaProbeResult {
    pub media_type: String, // "hls_master", "hls_media", "dash", "ytdlp", "direct"
    pub title: Option<String>,
    pub duration: Option<f64>,
    pub segment_count: Option<usize>,
    pub best_url: Option<String>,
    pub variants: Vec<MediaVariantSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaVariantSummary {
    pub id: String,
    pub bandwidth: Option<u64>,
    pub resolution: Option<String>,
    pub codecs: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStatus {
    pub available: bool,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaToolsStatus {
    pub ffmpeg: ToolStatus,
    pub ytdlp: ToolStatus,
}

pub fn check_media_tools(plugin_dir: Option<&Path>) -> MediaToolsStatus {
    let ffmpeg_path = FfmpegRemuxer::detect_ffmpeg();
    let ytdlp_path = YtDlpPlugin::detect_ytdlp(plugin_dir);

    MediaToolsStatus {
        ffmpeg: ToolStatus {
            available: ffmpeg_path.is_some(),
            path: ffmpeg_path.map(|p| p.to_string_lossy().to_string()),
        },
        ytdlp: ToolStatus {
            available: ytdlp_path.is_some(),
            path: ytdlp_path.map(|p| p.to_string_lossy().to_string()),
        },
    }
}

pub async fn probe_media_stream(
    client: &reqwest::Client,
    url_str: &str,
    plugin_dir: Option<&Path>,
) -> Result<MediaProbeResult> {
    let url = Url::parse(url_str).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    let path_lower = url.path().to_ascii_lowercase();

    // 1. Check if direct HLS manifest
    if path_lower.ends_with(".m3u8") {
        if let Ok(resp) = client.get(url.clone()).send().await {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    if let Ok(playlist) = parse_hls(&text, &url) {
                        match playlist {
                            HlsPlaylist::Master { ref variants } => {
                                let var_summaries = variants.iter().enumerate().map(|(i, v)| {
                                    MediaVariantSummary {
                                        id: format!("v{i}"),
                                        bandwidth: v.bandwidth,
                                        resolution: v.resolution.map(|(w, h)| format!("{w}x{h}")),
                                        codecs: v.codecs.clone(),
                                        url: v.uri.clone(),
                                    }
                                }).collect();
                                let best = playlist.best_variant().map(|v| v.uri.clone());
                                return Ok(MediaProbeResult {
                                    media_type: "hls_master".into(),
                                    title: None,
                                    duration: None,
                                    segment_count: None,
                                    best_url: best,
                                    variants: var_summaries,
                                });
                            }
                            HlsPlaylist::Media { ref segments, .. } => {
                                let dur = playlist.total_duration();
                                let count = segments.len();
                                return Ok(MediaProbeResult {
                                    media_type: "hls_media".into(),
                                    title: None,
                                    duration: dur,
                                    segment_count: Some(count),
                                    best_url: Some(url_str.to_string()),
                                    variants: Vec::new(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. Check if direct DASH manifest
    if path_lower.ends_with(".mpd") {
        if let Ok(resp) = client.get(url.clone()).send().await {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    if let Ok(manifest) = parse_dash(&text, &url) {
                        let var_summaries = manifest.representations.iter().map(|r| {
                            MediaVariantSummary {
                                id: r.id.clone(),
                                bandwidth: r.bandwidth,
                                resolution: match (r.width, r.height) {
                                    (Some(w), Some(h)) => Some(format!("{w}x{h}")),
                                    _ => None,
                                },
                                codecs: r.mime_type.clone(),
                                url: r.init_url.clone().unwrap_or_default(),
                            }
                        }).collect();
                        let best = manifest.best_representation().and_then(|r| r.init_url.clone());
                        return Ok(MediaProbeResult {
                            media_type: "dash".into(),
                            title: None,
                            duration: manifest.duration,
                            segment_count: manifest.best_representation().map(|r| r.segment_urls.len()),
                            best_url: best,
                            variants: var_summaries,
                        });
                    }
                }
            }
        }
    }

    // 3. Fallback to yt-dlp if available
    if let Some(ytdlp) = YtDlpPlugin::detect_ytdlp(plugin_dir) {
        if let Ok(info) = YtDlpPlugin::extract_info(&ytdlp, url_str) {
            let variants = info.formats.iter().map(|f| {
                MediaVariantSummary {
                    id: f.format_id.clone(),
                    bandwidth: f.tbr.map(|t| (t * 1000.0) as u64),
                    resolution: f.resolution.clone(),
                    codecs: f.vcodec.clone(),
                    url: f.url.clone(),
                }
            }).collect();
            return Ok(MediaProbeResult {
                media_type: "ytdlp".into(),
                title: Some(info.title),
                duration: info.duration,
                segment_count: None,
                best_url: info.best_url,
                variants,
            });
        }
    }

    Ok(MediaProbeResult {
        media_type: "direct".into(),
        title: None,
        duration: None,
        segment_count: None,
        best_url: Some(url_str.to_string()),
        variants: Vec::new(),
    })
}

// ============================================================================
// HLS (HTTP Live Streaming)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HlsVariant {
    pub uri: String,
    pub bandwidth: Option<u64>,
    pub resolution: Option<(u32, u32)>,
    pub codecs: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HlsSegment {
    pub uri: String,
    pub duration: f64,
    pub title: Option<String>,
    pub byte_range: Option<(u64, u64)>, // (length, offset)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HlsPlaylist {
    Master {
        variants: Vec<HlsVariant>,
    },
    Media {
        target_duration: Option<f64>,
        init_segment: Option<String>,
        segments: Vec<HlsSegment>,
    },
}

impl HlsPlaylist {
    /// Returns the variant with the highest bandwidth or resolution.
    pub fn best_variant(&self) -> Option<&HlsVariant> {
        match self {
            HlsPlaylist::Master { variants } => {
                variants.iter().max_by_key(|v| {
                    let res_score = v.resolution.map(|(w, h)| (w as u64) * (h as u64)).unwrap_or(0);
                    v.bandwidth.unwrap_or(0) + res_score * 10
                })
            }
            HlsPlaylist::Media { .. } => None,
        }
    }

    /// If this is a media playlist, returns the total duration in seconds.
    pub fn total_duration(&self) -> Option<f64> {
        match self {
            HlsPlaylist::Media { segments, .. } => {
                Some(segments.iter().map(|s| s.duration).sum())
            }
            HlsPlaylist::Master { .. } => None,
        }
    }
}

/// Parses an HLS manifest (.m3u8), resolving relative URLs against `base_url`.
pub fn parse_hls(content: &str, base_url: &Url) -> Result<HlsPlaylist> {
    let mut is_master = false;
    let mut variants = Vec::new();
    let mut segments = Vec::new();
    let mut target_duration = None;
    let mut init_segment = None;

    let mut lines = content.lines().map(|l| l.trim()).filter(|l| !l.is_empty());

    let first = lines.next().ok_or_else(|| Error::Failed("empty HLS manifest".into()))?;
    if !first.starts_with("#EXTM3U") {
        return Err(Error::Failed("invalid HLS: missing #EXTM3U header".into()));
    }

    let mut pending_variant: Option<(Option<u64>, Option<(u32, u32)>, Option<String>)> = None;
    let mut pending_duration: Option<f64> = None;
    let mut pending_title: Option<String> = None;
    let mut pending_byterange: Option<(u64, u64)> = None;

    for line in lines {
        if line.starts_with("#EXT-X-STREAM-INF:") {
            is_master = true;
            let attr_part = &line["#EXT-X-STREAM-INF:".len()..];
            let bw = parse_hls_attr(attr_part, "BANDWIDTH").and_then(|s| s.parse::<u64>().ok());
            let res = parse_hls_attr(attr_part, "RESOLUTION").and_then(|s| {
                let (w, h) = s.split_once('x')?;
                Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))
            });
            let codecs = parse_hls_attr(attr_part, "CODECS");
            pending_variant = Some((bw, res, codecs));
        } else if line.starts_with("#EXT-X-TARGETDURATION:") {
            let dur_str = &line["#EXT-X-TARGETDURATION:".len()..];
            target_duration = dur_str.trim().parse::<f64>().ok();
        } else if line.starts_with("#EXT-X-MAP:") {
            let attr_part = &line["#EXT-X-MAP:".len()..];
            if let Some(uri) = parse_hls_attr(attr_part, "URI") {
                if let Ok(resolved) = base_url.join(&uri) {
                    init_segment = Some(resolved.to_string());
                }
            }
        } else if line.starts_with("#EXTINF:") {
            let rest = &line["#EXTINF:".len()..];
            let (dur_part, title_part) = match rest.split_once(',') {
                Some((d, t)) => (d, Some(t.trim().to_string())),
                None => (rest, None),
            };
            pending_duration = dur_part.trim().parse::<f64>().ok();
            pending_title = title_part;
        } else if line.starts_with("#EXT-X-BYTERANGE:") {
            let rest = &line["#EXT-X-BYTERANGE:".len()..];
            if let Some((len_str, off_str)) = rest.split_once('@') {
                if let (Ok(l), Ok(o)) = (len_str.trim().parse::<u64>(), off_str.trim().parse::<u64>()) {
                    pending_byterange = Some((l, o));
                }
            } else if let Ok(l) = rest.trim().parse::<u64>() {
                pending_byterange = Some((l, 0));
            }
        } else if line.starts_with('#') {
            // Other tags (EXT-X-ENDLIST, EXT-X-VERSION, comments) ignored
            continue;
        } else {
            // Non-comment line is a URI
            let resolved = base_url
                .join(line)
                .map_err(|e| Error::Failed(format!("invalid segment URL '{line}': {e}")))?;
            let uri = resolved.to_string();

            if is_master {
                let (bw, res, codecs) = pending_variant.take().unwrap_or((None, None, None));
                variants.push(HlsVariant {
                    uri,
                    bandwidth: bw,
                    resolution: res,
                    codecs,
                });
            } else {
                let duration = pending_duration.take().unwrap_or(0.0);
                let title = pending_title.take();
                let byte_range = pending_byterange.take();
                segments.push(HlsSegment {
                    uri,
                    duration,
                    title,
                    byte_range,
                });
            }
        }
    }

    if is_master {
        Ok(HlsPlaylist::Master { variants })
    } else {
        Ok(HlsPlaylist::Media {
            target_duration,
            init_segment,
            segments,
        })
    }
}

fn parse_hls_attr(attrs: &str, name: &str) -> Option<String> {
    let key = format!("{name}=");
    let pos = attrs.find(&key)?;
    let val_after = &attrs[pos + key.len()..];
    if val_after.starts_with('"') {
        let inner = &val_after[1..];
        let end = inner.find('"')?;
        Some(inner[..end].to_string())
    } else {
        let end = val_after.find(',').unwrap_or(val_after.len());
        Some(val_after[..end].trim().to_string())
    }
}

// ============================================================================
// DASH (Dynamic Adaptive Streaming over HTTP)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DashRepresentation {
    pub id: String,
    pub mime_type: Option<String>,
    pub bandwidth: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub init_url: Option<String>,
    pub segment_urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DashManifest {
    pub duration: Option<f64>,
    pub representations: Vec<DashRepresentation>,
}

impl DashManifest {
    /// Returns the representation with the highest bandwidth.
    pub fn best_representation(&self) -> Option<&DashRepresentation> {
        self.representations.iter().max_by_key(|r| r.bandwidth.unwrap_or(0))
    }
}

/// Parses a DASH manifest (.mpd XML) without external XML parser crates.
pub fn parse_dash(content: &str, base_url: &Url) -> Result<DashManifest> {
    let duration = parse_dash_duration(content);
    let mut representations = Vec::new();

    // Look for <Representation ...> tags
    let mut rest = content;
    while let Some(pos) = rest.find("<Representation") {
        let tag_rest = &rest[pos..];
        let tag_end = tag_rest.find('>').ok_or_else(|| Error::Failed("malformed XML".into()))?;
        let tag_header = &tag_rest[..tag_end];

        let id = extract_xml_attr(tag_header, "id").unwrap_or_else(|| format!("rep_{}", representations.len()));
        let bandwidth = extract_xml_attr(tag_header, "bandwidth").and_then(|s| s.parse::<u64>().ok());
        let width = extract_xml_attr(tag_header, "width").and_then(|s| s.parse::<u32>().ok());
        let height = extract_xml_attr(tag_header, "height").and_then(|s| s.parse::<u32>().ok());
        let mime_type = extract_xml_attr(tag_header, "mimeType");

        // Find representation block or parent template
        let block_end = tag_rest.find("</Representation>").unwrap_or(tag_rest.len());
        let rep_block = &tag_rest[..block_end];

        // Check for SegmentTemplate
        let (init_url, segment_urls) = extract_dash_segments(rep_block, content, base_url, &id, duration)?;

        representations.push(DashRepresentation {
            id,
            mime_type,
            bandwidth,
            width,
            height,
            init_url,
            segment_urls,
        });

        rest = &tag_rest[tag_end + 1..];
    }

    Ok(DashManifest {
        duration,
        representations,
    })
}

fn extract_xml_attr(tag: &str, name: &str) -> Option<String> {
    let search_patterns = [
        format!(" {name}=\""),
        format!("\t{name}=\""),
        format!("\n{name}=\""),
        format!(" {name}='"),
        format!("\t{name}='"),
        format!("\n{name}='"),
    ];
    for pat in &search_patterns {
        if let Some(pos) = tag.find(pat.as_str()) {
            let quote = pat.chars().last().unwrap();
            let val_after = &tag[pos + pat.len()..];
            let end = val_after.find(quote)?;
            return Some(val_after[..end].to_string());
        }
    }
    None
}

fn parse_dash_duration(content: &str) -> Option<f64> {
    let dur_str = extract_xml_attr(content, "mediaPresentationDuration")?;
    // ISO 8601 duration string: PT1H2M3.4S or PT30S
    let dur_str = dur_str.strip_prefix("PT")?;
    let mut total = 0.0;
    let mut current_num = String::new();

    for ch in dur_str.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            current_num.push(ch);
        } else {
            let val = current_num.parse::<f64>().unwrap_or(0.0);
            current_num.clear();
            match ch {
                'H' => total += val * 3600.0,
                'M' => total += val * 60.0,
                'S' => total += val,
                _ => {}
            }
        }
    }
    Some(total)
}

fn extract_dash_segments(
    rep_block: &str,
    manifest: &str,
    base_url: &Url,
    rep_id: &str,
    manifest_duration: Option<f64>,
) -> Result<(Option<String>, Vec<String>)> {
    let mut init_url = None;
    let mut segment_urls = Vec::new();

    // 1. Check for SegmentTemplate in rep_block, or fallback to manifest level
    let template_tag = if let Some(pos) = rep_block.find("<SegmentTemplate") {
        let slice = &rep_block[pos..];
        let end = slice.find('>').unwrap_or(slice.len());
        Some(&slice[..end])
    } else if let Some(pos) = manifest.find("<SegmentTemplate") {
        let slice = &manifest[pos..];
        let end = slice.find('>').unwrap_or(slice.len());
        Some(&slice[..end])
    } else {
        None
    };

    if let Some(tmpl) = template_tag {
        let timescale = extract_xml_attr(tmpl, "timescale").and_then(|s| s.parse::<f64>().ok()).unwrap_or(1.0);
        let duration = extract_xml_attr(tmpl, "duration").and_then(|s| s.parse::<f64>().ok());
        let start_number = extract_xml_attr(tmpl, "startNumber").and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
        let init_pattern = extract_xml_attr(tmpl, "initialization");
        let media_pattern = extract_xml_attr(tmpl, "media");

        if let Some(init_tmpl) = init_pattern {
            let resolved_init = init_tmpl.replace("$RepresentationID$", rep_id);
            if let Ok(u) = base_url.join(&resolved_init) {
                init_url = Some(u.to_string());
            }
        }

        if let Some(media_tmpl) = media_pattern {
            let seg_dur_sec = duration.map(|d| d / timescale).unwrap_or(4.0);
            let total_dur = manifest_duration.unwrap_or(60.0);
            let seg_count = (total_dur / seg_dur_sec).ceil() as usize;

            for i in 0..seg_count.min(1000) {
                let num = start_number + i;
                let seg_rel = media_tmpl
                    .replace("$RepresentationID$", rep_id)
                    .replace("$Number$", &num.to_string())
                    .replace("$Number%05d$", &format!("{num:05}"))
                    .replace("$Number%04d$", &format!("{num:04}"));

                if let Ok(u) = base_url.join(&seg_rel) {
                    segment_urls.push(u.to_string());
                }
            }
        }
        return Ok((init_url, segment_urls));
    }

    // 2. Check for SegmentList
    let list_block = rep_block.find("<SegmentList").map(|p| &rep_block[p..]).or_else(|| {
        manifest.find("<SegmentList").map(|p| &manifest[p..])
    });

    if let Some(block) = list_block {
        let mut rest = block;
        while let Some(pos) = rest.find("<SegmentURL") {
            let slice = &rest[pos..];
            let end = slice.find('>').unwrap_or(slice.len());
            let tag = &slice[..end];
            if let Some(media) = extract_xml_attr(tag, "media") {
                if let Ok(u) = base_url.join(&media) {
                    segment_urls.push(u.to_string());
                }
            }
            rest = &slice[end..];
            if rest.starts_with("</SegmentList>") {
                break;
            }
        }
    }

    Ok((init_url, segment_urls))
}

// ============================================================================
// FFmpeg Remuxer
// ============================================================================

pub struct FfmpegRemuxer;

pub fn create_media_client() -> Result<reqwest::Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .user_agent(crate::config::DEFAULT_USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    Ok(client)
}

impl FfmpegRemuxer {
    /// Detects ffmpeg binary location on system PATH or standard directories.
    pub fn detect_ffmpeg() -> Option<PathBuf> {
        if let Ok(env_path) = std::env::var("FFMPEG_PATH") {
            let p = PathBuf::from(env_path);
            if p.is_file() {
                return Some(p);
            }
        }

        let binary_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

        // Check PATH
        if let Ok(path_var) = std::env::var("PATH") {
            let separator = if cfg!(windows) { ';' } else { ':' };
            for dir in path_var.split(separator) {
                let candidate = Path::new(dir).join(binary_name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }

        // Standard fallback locations
        #[cfg(windows)]
        {
            let candidates = [
                PathBuf::from(r"C:\ffmpeg\bin\ffmpeg.exe"),
                PathBuf::from(r"C:\Program Files\ffmpeg\bin\ffmpeg.exe"),
            ];
            for c in &candidates {
                if c.is_file() {
                    return Some(c.clone());
                }
            }
        }

        #[cfg(not(windows))]
        {
            let candidates = [
                PathBuf::from("/usr/bin/ffmpeg"),
                PathBuf::from("/usr/local/bin/ffmpeg"),
                PathBuf::from("/opt/homebrew/bin/ffmpeg"),
            ];
            for c in &candidates {
                if c.is_file() {
                    return Some(c.clone());
                }
            }
        }

        None
    }

    /// Remuxes segment files into a single destination file using ffmpeg -c copy.
    /// Falls back to direct concatenation for MPEG-TS files if ffmpeg is unavailable.
    pub fn remux_to_file(
        segments: &[PathBuf],
        output_file: &Path,
        ffmpeg_bin: Option<&Path>,
    ) -> Result<()> {
        if segments.is_empty() {
            return Err(Error::Failed("no segments to remux".into()));
        }

        let resolved_ffmpeg = ffmpeg_bin
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(Self::detect_ffmpeg);

        if let Some(ffmpeg) = resolved_ffmpeg {
            // Create concat text file
            let parent_dir = output_file
                .parent()
                .ok_or_else(|| Error::Failed("invalid output file path".into()))?;
            let concat_file = parent_dir.join(format!("concat_{}.txt", uuid::Uuid::new_v4().simple()));

            let mut concat_content = String::new();
            for seg in segments {
                let path_str = seg.to_string_lossy().replace('\\', "/");
                concat_content.push_str(&format!("file '{path_str}'\n"));
            }
            fs::write(&concat_file, concat_content)?;

            let status = Command::new(&ffmpeg)
                .args([
                    "-f", "concat",
                    "-safe", "0",
                    "-i", &concat_file.to_string_lossy(),
                    "-c", "copy",
                    "-y",
                    &output_file.to_string_lossy(),
                ])
                .status()
                .map_err(|e| Error::Failed(format!("executing ffmpeg: {e}")))?;

            let _ = fs::remove_file(&concat_file);

            if !status.success() {
                return Err(Error::Failed(format!("ffmpeg remux failed with status {status}")));
            }
            return Ok(());
        }

        // Fallback: If all segments are .ts files, concatenate directly
        let all_ts = segments.iter().all(|s| {
            s.extension().and_then(|ext| ext.to_str()).map(|e| e.eq_ignore_ascii_case("ts")).unwrap_or(false)
        });

        if all_ts {
            let mut out = File::create(output_file)?;
            for seg in segments {
                let mut inp = File::open(seg)?;
                std::io::copy(&mut inp, &mut out)?;
            }
            return Ok(());
        }

        if segments.len() == 1 {
            fs::copy(&segments[0], output_file)?;
            return Ok(());
        }

        Err(Error::Failed(
            "ffmpeg is required to remux media streams. Please install ffmpeg and ensure it is on PATH."
                .into(),
        ))
    }
}

// ============================================================================
// yt-dlp Plugin (Consent-Gated Extractor)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YtDlpFormat {
    pub format_id: String,
    pub ext: String,
    pub resolution: Option<String>,
    pub url: String,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub tbr: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YtDlpMediaInfo {
    pub title: String,
    pub duration: Option<f64>,
    pub webpage_url: String,
    pub thumbnail: Option<String>,
    pub formats: Vec<YtDlpFormat>,
    pub best_url: Option<String>,
}

pub struct YtDlpPlugin;

impl YtDlpPlugin {
    /// Detects yt-dlp on system PATH or within the application plugins directory.
    pub fn detect_ytdlp(plugin_dir: Option<&Path>) -> Option<PathBuf> {
        let binary_name = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };

        if let Some(dir) = plugin_dir {
            let candidate = dir.join(binary_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }

        if let Ok(env_path) = std::env::var("YTDLP_PATH") {
            let p = PathBuf::from(env_path);
            if p.is_file() {
                return Some(p);
            }
        }

        if let Ok(path_var) = std::env::var("PATH") {
            let separator = if cfg!(windows) { ';' } else { ':' };
            for dir in path_var.split(separator) {
                let candidate = Path::new(dir).join(binary_name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }

        None
    }

    /// Extracts video/stream information from a supported media URL using yt-dlp.
    pub fn extract_info(ytdlp_bin: &Path, url: &str) -> Result<YtDlpMediaInfo> {
        let output = Command::new(ytdlp_bin)
            .args(["--dump-json", "--no-playlist", "--skip-download", url])
            .output()
            .map_err(|e| Error::Failed(format!("running yt-dlp: {e}")))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Failed(format!("yt-dlp extraction failed: {err}")));
        }

        let json_str = String::from_utf8_lossy(&output.stdout);
        let val: serde_json::Value = serde_json::from_str(&json_str)?;

        let title = val.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled Media").to_string();
        let duration = val.get("duration").and_then(|v| v.as_f64());
        let webpage_url = val.get("webpage_url").and_then(|v| v.as_str()).unwrap_or(url).to_string();
        let thumbnail = val.get("thumbnail").and_then(|v| v.as_str()).map(|s| s.to_string());
        let best_url = val.get("url").and_then(|v| v.as_str()).map(|s| s.to_string());

        let mut formats = Vec::new();
        if let Some(fmt_arr) = val.get("formats").and_then(|v| v.as_array()) {
            for f in fmt_arr {
                if let (Some(fid), Some(furl)) = (
                    f.get("format_id").and_then(|v| v.as_str()),
                    f.get("url").and_then(|v| v.as_str()),
                ) {
                    formats.push(YtDlpFormat {
                        format_id: fid.to_string(),
                        ext: f.get("ext").and_then(|v| v.as_str()).unwrap_or("mp4").to_string(),
                        resolution: f.get("resolution").and_then(|v| v.as_str()).map(|s| s.to_string()),
                        url: furl.to_string(),
                        vcodec: f.get("vcodec").and_then(|v| v.as_str()).map(|s| s.to_string()),
                        acodec: f.get("acodec").and_then(|v| v.as_str()).map(|s| s.to_string()),
                        tbr: f.get("tbr").and_then(|v| v.as_f64()),
                    });
                }
            }
        }

        Ok(YtDlpMediaInfo {
            title,
            duration,
            webpage_url,
            thumbnail,
            formats,
            best_url,
        })
    }

    pub async fn install_plugin(
        client: Option<&reqwest::Client>,
        target_dir: &Path,
        consent: bool,
    ) -> Result<PathBuf> {
        if !consent {
            return Err(Error::Failed(
                "explicit user consent is required before installing the yt-dlp plugin".into(),
            ));
        }

        fs::create_dir_all(target_dir)?;

        let (url, filename) = if cfg!(windows) {
            (
                "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe",
                "yt-dlp.exe",
            )
        } else if cfg!(target_os = "macos") {
            (
                "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos",
                "yt-dlp",
            )
        } else {
            (
                "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp",
                "yt-dlp",
            )
        };

        let dest = target_dir.join(filename);
        let custom_client;
        let c = match client {
            Some(cl) => cl,
            None => {
                custom_client = create_media_client()?;
                &custom_client
            }
        };

        let resp = c.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Failed(format!(
                "failed downloading yt-dlp: HTTP {}",
                resp.status()
            )));
        }

        let bytes = resp.bytes().await?;
        fs::write(&dest, bytes)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o755));
        }

        Ok(dest)
    }
}

// ============================================================================
// Media Stream Downloader Coordinator
// ============================================================================

/// Downloads a list of media segments into a temporary staging folder,
/// then remuxes them into `output_file` using `FfmpegRemuxer`.
pub async fn download_and_remux_stream(
    client: &reqwest::Client,
    segment_urls: &[String],
    output_file: &Path,
    cancel: &CancellationToken,
) -> Result<u64> {
    if segment_urls.is_empty() {
        return Err(Error::Failed("segment list is empty".into()));
    }

    let parent_dir = output_file
        .parent()
        .ok_or_else(|| Error::Failed("invalid output file path".into()))?;
    let temp_stage_dir = parent_dir.join(format!("staging_{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&temp_stage_dir)?;

    let mut segment_paths = Vec::new();
    let mut total_bytes = 0u64;

    for (index, seg_url) in segment_urls.iter().enumerate() {
        if cancel.is_cancelled() {
            let _ = fs::remove_dir_all(&temp_stage_dir);
            return Err(Error::Cancelled);
        }

        let ext = seg_url
            .split('?')
            .next()
            .unwrap_or(seg_url)
            .rsplit('.')
            .next()
            .unwrap_or("ts");
        let seg_path = temp_stage_dir.join(format!("seg_{index:05}.{ext}"));

        let resp = client.get(seg_url).send().await?;
        if !resp.status().is_success() {
            let _ = fs::remove_dir_all(&temp_stage_dir);
            return Err(Error::Failed(format!(
                "failed to download segment {index}: HTTP {}",
                resp.status()
            )));
        }

        let bytes = resp.bytes().await?;
        total_bytes += bytes.len() as u64;
        fs::write(&seg_path, bytes)?;
        segment_paths.push(seg_path);
    }

    // Remux all segments into the final output file
    let remux_res = FfmpegRemuxer::remux_to_file(&segment_paths, output_file, None);
    let _ = fs::remove_dir_all(&temp_stage_dir);

    remux_res.map(|_| total_bytes)
}

/// Resolves a media URL (master playlist, media playlist, DASH MPD) to a list of segment URLs.
pub async fn resolve_media_segments(
    client: &reqwest::Client,
    url_str: &str,
) -> Result<Vec<String>> {
    let parsed_url = Url::parse(url_str).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    let path_lower = parsed_url.path().to_ascii_lowercase();

    if path_lower.ends_with(".m3u8") {
        let resp = client.get(parsed_url.clone()).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Failed(format!("HTTP {}", resp.status())));
        }
        let text = resp.text().await?;
        let playlist = parse_hls(&text, &parsed_url)?;
        match playlist {
            HlsPlaylist::Master { .. } => {
                let best = playlist
                    .best_variant()
                    .ok_or_else(|| Error::Failed("no variants found in master playlist".into()))?;
                let var_url = Url::parse(&best.uri).map_err(|e| Error::InvalidUrl(e.to_string()))?;
                let vresp = client.get(var_url.clone()).send().await?;
                let vtext = vresp.text().await?;
                let vplaylist = parse_hls(&vtext, &var_url)?;
                match vplaylist {
                    HlsPlaylist::Media { segments, init_segment, .. } => {
                        let mut res = Vec::new();
                        if let Some(init) = init_segment {
                            res.push(init);
                        }
                        res.extend(segments.into_iter().map(|s| s.uri));
                        return Ok(res);
                    }
                    _ => return Err(Error::Failed("expected media playlist from variant".into())),
                }
            }
            HlsPlaylist::Media { segments, init_segment, .. } => {
                let mut res = Vec::new();
                if let Some(init) = init_segment {
                    res.push(init);
                }
                res.extend(segments.into_iter().map(|s| s.uri));
                return Ok(res);
            }
        }
    }

    if path_lower.ends_with(".mpd") {
        let resp = client.get(parsed_url.clone()).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Failed(format!("HTTP {}", resp.status())));
        }
        let text = resp.text().await?;
        let manifest = parse_dash(&text, &parsed_url)?;
        let best = manifest
            .best_representation()
            .ok_or_else(|| Error::Failed("no representations found in DASH manifest".into()))?;
        let mut res = Vec::new();
        if let Some(ref init) = best.init_url {
            res.push(init.clone());
        }
        res.extend(best.segment_urls.clone());
        return Ok(res);
    }

    // Direct stream
    Ok(vec![url_str.to_string()])
}

/// Convenience helper: Resolves all media segments for `url_str`, downloads them, and remuxes into `output_file`.
pub async fn download_media_url(
    client: &reqwest::Client,
    url_str: &str,
    output_file: &Path,
    cancel: &CancellationToken,
) -> Result<u64> {
    let segments = resolve_media_segments(client, url_str).await?;
    download_and_remux_stream(client, &segments, output_file, cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hls_master_playlist() {
        let base = Url::parse("https://cdn.example.com/hls/master.m3u8").unwrap();
        let content = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360
360p.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1400000,RESOLUTION=842x480
480p.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720,CODECS="avc1.4d401f,mp4a.40.2"
720p.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080
https://cdn.example.com/hls/1080p.m3u8
"#;
        let playlist = parse_hls(content, &base).unwrap();
        match &playlist {
            HlsPlaylist::Master { variants } => {
                assert_eq!(variants.len(), 4);
                assert_eq!(variants[0].uri, "https://cdn.example.com/hls/360p.m3u8");
                assert_eq!(variants[0].bandwidth, Some(800000));
                assert_eq!(variants[0].resolution, Some((640, 360)));

                let best = playlist.best_variant().expect("best variant");
                assert_eq!(best.uri, "https://cdn.example.com/hls/1080p.m3u8");
                assert_eq!(best.resolution, Some((1920, 1080)));
            }
            _ => panic!("expected master playlist"),
        }
    }

    #[test]
    fn parse_hls_media_playlist() {
        let base = Url::parse("https://cdn.example.com/hls/720p.m3u8").unwrap();
        let content = r#"#EXTM3U
#EXT-X-VERSION:4
#EXT-X-TARGETDURATION:6
#EXT-X-MAP:URI="init.mp4"
#EXTINF:5.000,Segment 1
seg_001.mp4
#EXTINF:6.000,Segment 2
seg_002.mp4
#EXTINF:4.500,
https://cdn.example.com/hls/seg_003.mp4
#EXT-X-ENDLIST
"#;
        let playlist = parse_hls(content, &base).unwrap();
        match &playlist {
            HlsPlaylist::Media {
                target_duration,
                init_segment,
                segments,
            } => {
                assert_eq!(*target_duration, Some(6.0));
                assert_eq!(init_segment.as_deref(), Some("https://cdn.example.com/hls/init.mp4"));
                assert_eq!(segments.len(), 3);
                assert_eq!(segments[0].uri, "https://cdn.example.com/hls/seg_001.mp4");
                assert_eq!(segments[0].duration, 5.0);
                assert_eq!(segments[0].title.as_deref(), Some("Segment 1"));
                assert_eq!(segments[2].uri, "https://cdn.example.com/hls/seg_003.mp4");
                assert_eq!(playlist.total_duration(), Some(15.5));
            }
            _ => panic!("expected media playlist"),
        }
    }

    #[test]
    fn parse_dash_mpd_manifest() {
        let base = Url::parse("https://dash.example.com/manifest.mpd").unwrap();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1M30S">
  <Period>
    <AdaptationSet mimeType="video/mp4">
      <Representation id="v1" bandwidth="2000000" width="1280" height="720">
        <SegmentTemplate timescale="1000" duration="4000" initialization="init-$RepresentationID$.mp4" media="chunk-$RepresentationID$-$Number%05d$.m4s" startNumber="1"/>
      </Representation>
      <Representation id="v2" bandwidth="4500000" width="1920" height="1080">
        <SegmentTemplate timescale="1000" duration="4000" initialization="init-$RepresentationID$.mp4" media="chunk-$RepresentationID$-$Number%05d$.m4s" startNumber="1"/>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let manifest = parse_dash(content, &base).unwrap();
        assert_eq!(manifest.duration, Some(90.0));
        assert_eq!(manifest.representations.len(), 2);

        let best = manifest.best_representation().expect("best rep");
        assert_eq!(best.id, "v2");
        assert_eq!(best.bandwidth, Some(4500000));
        assert_eq!(best.init_url.as_deref(), Some("https://dash.example.com/init-v2.mp4"));
        assert!(!best.segment_urls.is_empty());
        assert_eq!(best.segment_urls[0], "https://dash.example.com/chunk-v2-00001.m4s");
    }

    #[test]
    fn ffmpeg_ts_fallback_concatenation() {
        let dir = tempfile::tempdir().unwrap();
        let s1 = dir.path().join("seg1.ts");
        let s2 = dir.path().join("seg2.ts");
        let out = dir.path().join("joined.ts");

        fs::write(&s1, b"SYNC_TS_HEADER_PART1").unwrap();
        fs::write(&s2, b"SYNC_TS_HEADER_PART2").unwrap();

        // Pass invalid ffmpeg path to test direct TS concatenation fallback
        let invalid_path = PathBuf::from("nonexistent_ffmpeg_bin");
        FfmpegRemuxer::remux_to_file(&[s1, s2], &out, Some(&invalid_path)).unwrap();

        let joined = fs::read(&out).unwrap();
        assert_eq!(joined, b"SYNC_TS_HEADER_PART1SYNC_TS_HEADER_PART2");
    }

    #[test]
    fn ytdlp_consent_required() {
        let dir = tempfile::tempdir().unwrap();
        let res = tokio::runtime::Runtime::new().unwrap().block_on(
            YtDlpPlugin::install_plugin(None, dir.path(), false)
        );
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("consent"));
    }
}
