//! Fetches a `link` node's OpenGraph social preview (title/description/
//! image) for the `preview="true"` attribute (see `meshfox_core::canvas`'s
//! `Node::preview`), and caches the result in memory for the life of
//! whichever process owns the `PreviewCache` — the web server's own
//! `AppState` (one per `meshfox view`) or the TUI's own `App` (one per
//! `meshfox tui`, called in-process since the TUI links this crate as a
//! library rather than talking to a server over HTTP).
//!
//! A `.canvas.md` file is often opened from an untrusted source (shared,
//! downloaded), and its `link` targets are entirely attacker-controlled —
//! so every fetch here is SSRF-hardened: only `http`/`https`, only
//! resolved addresses that aren't loopback/private/link-local/etc., and
//! the connection is pinned to the exact address that was validated (so a
//! second DNS lookup mid-request — a "DNS rebind" — can't swap in an
//! unsafe address after the check). Redirects are followed manually, one
//! hop at a time, re-running the same validation on every hop, since
//! blindly auto-following redirects is the classic way this kind of check
//! gets bypassed.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Mutex;
use std::time::Duration;

use scraper::{Html, Selector};
use serde::Serialize;

const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: u8 = 5;
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_DESCRIPTION_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewMeta {
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewError {
    /// Scheme/host/redirect target rejected by the SSRF check.
    Blocked,
    /// Network error, non-2xx status, or response too large.
    Fetch,
    /// 2xx response whose `Content-Type` isn't `text/html`.
    NotHtml,
}

#[derive(Debug, Clone)]
enum CacheEntry {
    Loaded(PreviewMeta),
    Failed,
}

/// One entry per URL, alive for exactly as long as the process that owns
/// it — never persisted, never shared across processes (see this module's
/// own doc comment). A failed fetch is cached too (as `Failed`), so a
/// broken link isn't re-fetched every time its node is rendered.
#[derive(Default)]
pub struct PreviewCache(Mutex<HashMap<String, CacheEntry>>);

impl PreviewCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Some(cached-or-freshly-fetched)` on success, `None` if the fetch
    /// failed (blocked, network error, not HTML, ...) — callers show "no
    /// preview available" rather than surfacing which of those it was, so
    /// this endpoint can't be used as a network probe (see module doc).
    pub async fn get_or_fetch(&self, url: &str) -> Option<PreviewMeta> {
        if let Some(entry) = self.0.lock().unwrap().get(url).cloned() {
            return match entry {
                CacheEntry::Loaded(meta) => Some(meta),
                CacheEntry::Failed => None,
            };
        }
        let result = fetch_og_preview(url).await;
        let entry = match &result {
            Ok(meta) => CacheEntry::Loaded(meta.clone()),
            Err(_) => CacheEntry::Failed,
        };
        self.0.lock().unwrap().insert(url.to_string(), entry);
        result.ok()
    }
}

/// `true` for an address that must never be connected to on this fetch's
/// behalf: loopback, RFC1918/link-local/multicast/unspecified, or their
/// IPv6 equivalents (including an IPv4-mapped IPv6 address wrapping one of
/// the above). Not exhaustive against every conceivable internal range,
/// but covers what a local machine or private network actually answers to.
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || is_unique_local_v6(v6)
                || is_unicast_link_local_v6(v6)
                || v6.to_ipv4_mapped().is_some_and(is_blocked_ipv4)
        }
    }
}

fn is_blocked_ipv4(v4: Ipv4Addr) -> bool {
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_multicast()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_documentation()
}

/// `fc00::/7` — IPv6 unique local addresses (the RFC1918 analogue).
/// `Ipv6Addr::is_unique_local` is unstable, so this is done by hand.
fn is_unique_local_v6(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

/// `fe80::/10` — IPv6 link-local unicast. Same "stable-Rust doesn't have
/// this yet" situation as `is_unique_local_v6`.
fn is_unicast_link_local_v6(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

/// Resolves `host:port` and pins to the first resolved address that isn't
/// blocked — pinning (rather than letting the connection re-resolve later)
/// is what actually closes the DNS-rebinding gap: it doesn't matter that
/// other, possibly-unsafe addresses came back in the same answer, since
/// this exact validated address is the only one ever connected to.
async fn resolve_validated(host: &str, port: u16) -> Result<SocketAddr, PreviewError> {
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| PreviewError::Blocked)?;
    addrs
        .into_iter()
        .find(|addr| !is_blocked_ip(addr.ip()))
        .ok_or(PreviewError::Blocked)
}

/// Fetches `url`, following redirects manually (re-validating every hop),
/// capped at `max_bytes`. `require_html` additionally rejects a non-
/// `text/html` response before reading its body (used for the page fetch;
/// the image fetch doesn't set it). Returns the body bytes and the final
/// URL actually fetched (redirect targets can differ from `url`, and a
/// page's own `og:image` may be relative to *that* URL).
async fn safe_get(
    url: &str,
    max_bytes: usize,
    require_html: bool,
) -> Result<(Vec<u8>, reqwest::Url), PreviewError> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let parsed = reqwest::Url::parse(&current).map_err(|_| PreviewError::Blocked)?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(PreviewError::Blocked);
        }
        let host = parsed.host_str().ok_or(PreviewError::Blocked)?;
        let port = parsed.port_or_known_default().ok_or(PreviewError::Blocked)?;
        let addr = resolve_validated(host, port).await?;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(FETCH_TIMEOUT)
            .resolve(host, addr)
            .build()
            .map_err(|_| PreviewError::Fetch)?;

        let resp = client
            .get(parsed.clone())
            .send()
            .await
            .map_err(|_| PreviewError::Fetch)?;

        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(PreviewError::Fetch)?;
            current = parsed
                .join(location)
                .map_err(|_| PreviewError::Fetch)?
                .to_string();
            continue;
        }
        if !resp.status().is_success() {
            return Err(PreviewError::Fetch);
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if require_html && !content_type.as_deref().is_some_and(|ct| ct.starts_with("text/html"))
        {
            return Err(PreviewError::NotHtml);
        }

        let mut resp = resp;
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|_| PreviewError::Fetch)? {
            body.extend_from_slice(&chunk);
            if body.len() > max_bytes {
                return Err(PreviewError::Fetch);
            }
        }
        return Ok((body, parsed));
    }
    Err(PreviewError::Fetch)
}

/// Fetches `url`'s page and extracts its OpenGraph metadata. `og:image`
/// (if present and not already absolute) is resolved against the final
/// fetched URL.
pub async fn fetch_og_preview(url: &str) -> Result<PreviewMeta, PreviewError> {
    let (body, final_url) = safe_get(url, MAX_HTML_BYTES, true).await?;
    let html = String::from_utf8_lossy(&body);
    let mut meta = parse_og_tags(&html);
    if let Some(image) = &meta.image {
        if let Ok(resolved) = final_url.join(image) {
            meta.image = Some(resolved.to_string());
        }
    }
    Ok(meta)
}

/// Fetches `url`'s raw bytes (no `Content-Type` requirement) — used by the
/// TUI to download a preview's `og:image` for local decoding, since
/// (unlike the web UI, which just hotlinks the image URL in an `<img>`)
/// `ratatui-image` needs the actual decoded bytes.
pub async fn fetch_image_bytes(url: &str) -> Result<Vec<u8>, PreviewError> {
    safe_get(url, MAX_IMAGE_BYTES, false).await.map(|(b, _)| b)
}

fn parse_og_tags(html: &str) -> PreviewMeta {
    let doc = Html::parse_document(html);
    let meta_sel = Selector::parse("meta").expect("static selector");
    let title_sel = Selector::parse("title").expect("static selector");

    let mut og: HashMap<String, String> = HashMap::new();
    for el in doc.select(&meta_sel) {
        let attrs = el.value();
        let key = attrs.attr("property").or_else(|| attrs.attr("name"));
        if let (Some(key), Some(content)) = (key, attrs.attr("content")) {
            if key.starts_with("og:") {
                og.entry(key.to_string())
                    .or_insert_with(|| content.to_string());
            }
        }
    }

    let title = og.get("og:title").cloned().or_else(|| {
        doc.select(&title_sel)
            .next()
            .map(|t| t.text().collect::<String>().trim().to_string())
            .filter(|t| !t.is_empty())
    });
    let description = og
        .get("og:description")
        .map(|d| truncate_chars(d, MAX_DESCRIPTION_CHARS));
    let image = og.get("og:image").cloned();

    PreviewMeta {
        title,
        description,
        image,
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_loopback_and_private_and_link_local_v4() {
        assert!(is_blocked_ip("127.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip("10.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip("172.16.0.1".parse().unwrap()));
        assert!(is_blocked_ip("192.168.1.1".parse().unwrap()));
        assert!(is_blocked_ip("169.254.1.1".parse().unwrap()));
        assert!(is_blocked_ip("0.0.0.0".parse().unwrap()));
    }

    #[test]
    fn allows_ordinary_public_v4() {
        assert!(!is_blocked_ip("93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn blocks_loopback_and_unique_local_and_link_local_v6() {
        assert!(is_blocked_ip("::1".parse().unwrap()));
        assert!(is_blocked_ip("fc00::1".parse().unwrap()));
        assert!(is_blocked_ip("fd12:3456:789a::1".parse().unwrap()));
        assert!(is_blocked_ip("fe80::1".parse().unwrap()));
        assert!(is_blocked_ip("::".parse().unwrap()));
    }

    #[test]
    fn blocks_ipv4_mapped_private_v6() {
        assert!(is_blocked_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip("::ffff:10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn allows_ordinary_public_v6() {
        assert!(!is_blocked_ip("2606:2800:220:1:248:1893:25c8:1946"
            .parse()
            .unwrap()));
    }

    #[test]
    fn parses_og_tags_and_falls_back_to_title() {
        let html = r#"<html><head>
            <meta property="og:title" content="Example Title">
            <meta property="og:description" content="Example description.">
            <meta property="og:image" content="/img/preview.png">
            <title>Fallback Title</title>
        </head></html>"#;
        let meta = parse_og_tags(html);
        assert_eq!(meta.title.as_deref(), Some("Example Title"));
        assert_eq!(meta.description.as_deref(), Some("Example description."));
        assert_eq!(meta.image.as_deref(), Some("/img/preview.png"));
    }

    #[test]
    fn falls_back_to_title_tag_when_no_og_title() {
        let html = "<html><head><title>Plain Title</title></head></html>";
        let meta = parse_og_tags(html);
        assert_eq!(meta.title.as_deref(), Some("Plain Title"));
        assert_eq!(meta.description, None);
        assert_eq!(meta.image, None);
    }

    #[test]
    fn truncates_long_description() {
        let long = "a".repeat(400);
        let html = format!(r#"<meta property="og:description" content="{long}">"#);
        let meta = parse_og_tags(&html);
        let desc = meta.description.unwrap();
        assert!(desc.chars().count() <= MAX_DESCRIPTION_CHARS + 1);
        assert!(desc.ends_with('…'));
    }

    /// Rejected before any DNS lookup or connection attempt — no network
    /// access needed for this test to be meaningful.
    #[tokio::test]
    async fn disallowed_schemes_and_malformed_urls_are_rejected() {
        for bad in ["ftp://example.com/", "file:///etc/passwd", "not a url"] {
            assert_eq!(
                fetch_og_preview(bad).await,
                Err(PreviewError::Blocked),
                "expected {bad:?} to be rejected"
            );
        }
    }

    /// The whole point of pinning to a validated address (see
    /// `resolve_validated`'s own doc comment) is that this must be
    /// rejected *before* ever connecting — proven here by actually having
    /// something listening on loopback that would happily serve an
    /// OpenGraph page if the SSRF check were ever bypassed or regressed.
    /// If this test starts failing because the fetch *succeeded*, that's
    /// the SSRF check itself having broken, not a flaky test.
    #[tokio::test]
    async fn loopback_target_is_blocked_even_when_something_is_listening() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let body = r#"<html><head><meta property="og:title" content="should never be seen"></head></html>"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let result = fetch_og_preview(&format!("http://{addr}/")).await;
        assert_eq!(result, Err(PreviewError::Blocked));
    }
}
