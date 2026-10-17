use std::collections::{HashSet, VecDeque};

use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::Error;
use crate::Result;

pub const MAX_EXPANSION_LIMIT: usize = 2000;

/// Expand bracket patterns in a URL string.
/// Supports:
/// - Numeric ranges: `[1-5]` or `[01-10]` (preserves zero-padding).
/// - Alphabetical ranges: `[a-z]` or `[A-Z]`.
/// - Multiple brackets: Cartesian product of all ranges.
pub fn expand_pattern(pattern: &str) -> Result<Vec<String>> {
    let mut current_results = vec![String::new()];
    let mut remainder = pattern;

    while let Some(start_idx) = remainder.find('[') {
        let prefix = &remainder[..start_idx];
        let after_bracket = &remainder[start_idx + 1..];

        let end_idx = match after_bracket.find(']') {
            Some(idx) => idx,
            None => {
                // Unclosed bracket, treat as literal
                for res in &mut current_results {
                    res.push_str(prefix);
                    res.push('[');
                    res.push_str(after_bracket);
                }
                remainder = "";
                break;
            }
        };

        let bracket_content = &after_bracket[..end_idx];
        remainder = &after_bracket[end_idx + 1..];

        // Append the prefix to all current results
        for res in &mut current_results {
            res.push_str(prefix);
        }

        // Try parsing bracket as range
        if let Some(options) = parse_range(bracket_content) {
            let mut next_results = Vec::new();
            for base in &current_results {
                for opt in &options {
                    if next_results.len() >= MAX_EXPANSION_LIMIT {
                        return Err(Error::Failed(format!(
                            "pattern expansion exceeds safety limit of {MAX_EXPANSION_LIMIT} URLs"
                        )));
                    }
                    let mut expanded = base.clone();
                    expanded.push_str(opt);
                    next_results.push(expanded);
                }
            }
            current_results = next_results;
        } else {
            // Not a valid range, treat literally as `[content]`
            for res in &mut current_results {
                res.push('[');
                res.push_str(bracket_content);
                res.push(']');
            }
        }
    }

    if !remainder.is_empty() {
        for res in &mut current_results {
            res.push_str(remainder);
        }
    }

    Ok(current_results)
}

fn parse_range(content: &str) -> Option<Vec<String>> {
    let (start_str, end_str) = content.split_once('-')?;
    let start_str = start_str.trim();
    let end_str = end_str.trim();

    // Check for numeric range
    if let (Ok(start_num), Ok(end_num)) = (start_str.parse::<u64>(), end_str.parse::<u64>()) {
        if start_num <= end_num {
            let count = (end_num - start_num + 1) as usize;
            if count > MAX_EXPANSION_LIMIT {
                return None;
            }
            let pad_width = if start_str.starts_with('0') && start_str.len() > 1 {
                start_str.len()
            } else {
                0
            };

            let mut items = Vec::with_capacity(count);
            for n in start_num..=end_num {
                if pad_width > 0 {
                    items.push(format!("{:0width$}", n, width = pad_width));
                } else {
                    items.push(n.to_string());
                }
            }
            return Some(items);
        }
    }

    // Check for alphabetical range (single char to single char)
    if start_str.len() == 1 && end_str.len() == 1 {
        let start_char = start_str.chars().next()?;
        let end_char = end_str.chars().next()?;

        if (start_char.is_ascii_lowercase() && end_char.is_ascii_lowercase() && start_char <= end_char)
            || (start_char.is_ascii_uppercase() && end_char.is_ascii_uppercase() && start_char <= end_char)
        {
            let mut items = Vec::new();
            for c in (start_char as u8)..=(end_char as u8) {
                items.push((c as char).to_string());
            }
            return Some(items);
        }
    }

    None
}

/// Parse a batch file / text content into a list of URLs.
/// Lines starting with '#' or empty lines are ignored.
pub fn parse_batch_urls(content: &str) -> Result<Vec<String>> {
    let mut urls = Vec::new();
    let mut seen = HashSet::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parsed = Url::parse(line).map_err(|err| Error::Failed(format!("invalid URL '{line}': {err}")))?;
        let s = parsed.to_string();
        if seen.insert(s.clone()) {
            urls.push(s);
        }
    }

    Ok(urls)
}

/// Extracts links and media references from HTML.
pub fn extract_links(html: &str, base: &Url) -> Vec<Url> {
    let mut urls = Vec::new();
    let mut seen = HashSet::new();

    // Scan for href="..." or src="..."
    let mut rest = html;
    while let Some((pos, attr_len)) = {
        let h = rest.find("href=").map(|p| (p, 5));
        let s = rest.find("src=").map(|p| (p, 4));
        match (h, s) {
            (Some(a), Some(b)) => if a.0 <= b.0 { Some(a) } else { Some(b) },
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    } {
        let tag_after = &rest[pos + attr_len..];
        let quote = tag_after.chars().next();
        let value = match quote {
            Some('"') => {
                let inner = &tag_after[1..];
                inner.split('"').next()
            }
            Some('\'') => {
                let inner = &tag_after[1..];
                inner.split('\'').next()
            }
            _ => None,
        };

        if let Some(link) = value {
            let link = link.trim();
            if !link.is_empty() && !link.starts_with('#') && !link.starts_with("javascript:") {
                if let Ok(resolved) = base.join(link) {
                    if resolved.scheme() == "http" || resolved.scheme() == "https" {
                        let mut normalized = resolved;
                        normalized.set_fragment(None);
                        if seen.insert(normalized.to_string()) {
                            urls.push(normalized);
                        }
                    }
                }
            }
        }
        rest = &tag_after[1.min(tag_after.len())..];
    }

    urls
}

/// Same-domain web crawler.
pub async fn crawl_same_domain(
    client: &reqwest::Client,
    start_url: &str,
    max_depth: usize,
    max_links: usize,
    allowed_extensions: &[String],
    cancel: &CancellationToken,
) -> Result<Vec<String>> {
    let root = Url::parse(start_url).map_err(|err| Error::Failed(format!("invalid start URL: {err}")))?;
    let root_domain = root.domain().ok_or_else(|| Error::Failed("URL has no domain".into()))?.to_lowercase();

    let mut visited_pages = HashSet::new();
    let mut collected_links = HashSet::new();
    let mut result_urls = Vec::new();

    let mut queue = VecDeque::new();
    queue.push_back((root.clone(), 0usize));
    visited_pages.insert(root.to_string());

    let ext_set: HashSet<String> = allowed_extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();

    while let Some((page_url, depth)) = queue.pop_front() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if result_urls.len() >= max_links {
            break;
        }

        let resp = match client.get(page_url.clone()).send().await {
            Ok(r) => r,
            Err(err) => {
                tracing::warn!("crawl failed to fetch {page_url}: {err}");
                continue;
            }
        };

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        let is_html = content_type.contains("text/html");
        let body = match resp.text().await {
            Ok(t) => t,
            Err(_) => continue,
        };

        let links = if is_html {
            extract_links(&body, &page_url)
        } else {
            Vec::new()
        };

        for link in links {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if result_urls.len() >= max_links {
                break;
            }

            // Same-domain check
            let link_domain = match link.domain() {
                Some(d) => d.to_lowercase(),
                None => continue,
            };
            if link_domain != root_domain {
                continue;
            }

            let link_str = link.to_string();
            let ext = link
                .path_segments()
                .and_then(|mut segs| segs.next_back())
                .and_then(|filename| filename.rsplit_once('.'))
                .map(|(_, ext)| ext.to_ascii_lowercase())
                .unwrap_or_default();

            let matches_ext = ext_set.is_empty() || ext_set.contains(&ext);

            if matches_ext && collected_links.insert(link_str.clone()) {
                result_urls.push(link_str.clone());
            }

            // If we have remaining depth and this looks like an HTML page or directory
            if depth + 1 < max_depth && !visited_pages.contains(&link_str) {
                // If ext is empty or html/htm/php/asp, queue for crawling
                if ext.is_empty() || ext == "html" || ext == "htm" || ext == "php" || ext == "asp" {
                    visited_pages.insert(link_str);
                    queue.push_back((link, depth + 1));
                }
            }
        }
    }

    Ok(result_urls)
}

/// Convenience crawl function that creates its own client and cancellation token.
pub async fn crawl_urls(
    start_url: &str,
    max_depth: usize,
    max_links: usize,
    allowed_extensions: &[String],
) -> Result<Vec<String>> {
    let client = reqwest::Client::builder()
        .user_agent(crate::config::DEFAULT_USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let cancel = CancellationToken::new();
    crawl_same_domain(&client, start_url, max_depth, max_links, allowed_extensions, &cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_single_number_range() {
        let pattern = "http://example.com/file[1-3].zip";
        let urls = expand_pattern(pattern).unwrap();
        assert_eq!(
            urls,
            vec![
                "http://example.com/file1.zip",
                "http://example.com/file2.zip",
                "http://example.com/file3.zip",
            ]
        );
    }

    #[test]
    fn expand_padded_number_range() {
        let pattern = "http://example.com/file[08-11].zip";
        let urls = expand_pattern(pattern).unwrap();
        assert_eq!(
            urls,
            vec![
                "http://example.com/file08.zip",
                "http://example.com/file09.zip",
                "http://example.com/file10.zip",
                "http://example.com/file11.zip",
            ]
        );
    }

    #[test]
    fn expand_char_range() {
        let pattern = "http://example.com/doc_[a-c].pdf";
        let urls = expand_pattern(pattern).unwrap();
        assert_eq!(
            urls,
            vec![
                "http://example.com/doc_a.pdf",
                "http://example.com/doc_b.pdf",
                "http://example.com/doc_c.pdf",
            ]
        );
    }

    #[test]
    fn expand_cartesian_product() {
        let pattern = "http://example.com/[a-b]/[1-2].bin";
        let urls = expand_pattern(pattern).unwrap();
        assert_eq!(
            urls,
            vec![
                "http://example.com/a/1.bin",
                "http://example.com/a/2.bin",
                "http://example.com/b/1.bin",
                "http://example.com/b/2.bin",
            ]
        );
    }

    #[test]
    fn expand_no_brackets() {
        let pattern = "http://example.com/single.iso";
        let urls = expand_pattern(pattern).unwrap();
        assert_eq!(urls, vec!["http://example.com/single.iso"]);
    }

    #[test]
    fn parse_batch_filters_comments_and_blanks() {
        let content = r#"
            # Download list
            https://example.com/file1.zip

            https://example.com/file2.zip
            # Another comment
            https://example.com/file1.zip
        "#;
        let urls = parse_batch_urls(content).unwrap();
        assert_eq!(
            urls,
            vec![
                "https://example.com/file1.zip".to_string(),
                "https://example.com/file2.zip".to_string(),
            ]
        );
    }

    #[test]
    fn extract_links_relative_and_absolute() {
        let base = Url::parse("https://example.com/gallery/index.html").unwrap();
        let html = r##"
            <html>
                <a href="photo1.jpg">Photo 1</a>
                <a href="/docs/readme.pdf">Docs</a>
                <img src="https://example.com/images/banner.png" />
                <a href="https://other.com/file.zip">External</a>
                <a href="#section">Fragment</a>
            </html>
        "##;
        let links = extract_links(html, &base);
        let link_strings: Vec<String> = links.into_iter().map(|u| u.to_string()).collect();
        assert!(link_strings.contains(&"https://example.com/gallery/photo1.jpg".to_string()));
        assert!(link_strings.contains(&"https://example.com/docs/readme.pdf".to_string()));
        assert!(link_strings.contains(&"https://example.com/images/banner.png".to_string()));
        assert!(link_strings.contains(&"https://other.com/file.zip".to_string()));
    }
}
