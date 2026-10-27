//! The header `meshfox serve --http` puts on every request it forwards to a
//! worker: `X-Meshfox-Gateway-Root: <served root, percent-encoded>`.
//!
//! It is how a worker learns, per request, that the caller reached it through
//! the gateway and what the gateway's root is — the two facts it needs to
//! answer "open that canvas" with a gateway URL (`../<slug>/`) instead of a
//! loopback port, and to refuse opening files on the server's machine. A
//! per-process flag would be wrong: the same worker is also reachable
//! directly on `127.0.0.1:<port>`, where a gateway URL would be a dead link.
//!
//! Workers listen on loopback without authentication, so any local process
//! could send this header. All it changes is the *shape* of the answers that
//! caller gets, so nothing is trusted beyond that; the gateway still strips
//! any `X-Meshfox-*` header a client sends and sets its own.

use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

pub const GATEWAY_ROOT_HEADER: &str = "x-meshfox-gateway-root";

/// Prefix shared by every header the gateway owns; a client's copies are dropped.
pub const GATEWAY_HEADER_PREFIX: &str = "x-meshfox-";

/// The header value for `root`: every byte outside `[A-Za-z0-9._~/-]` as `%XX`.
pub fn encode_root(root: &Path) -> String {
    let mut out = String::new();
    for &b in root.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'~' | b'/' | b'-' => {
                out.push(char::from(b))
            }
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The root a header value names. `None` unless it decodes to an absolute path.
pub fn decode_root(value: &str) -> Option<PathBuf> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(out));
    (path.is_absolute() && !path.as_os_str().is_empty()).then_some(path)
}

/// The URL, relative to the page that asked (always at `/c/<slug>/`), of
/// the canvas at `target` under `root`; `None` when `target` is outside it.
pub fn canvas_url(root: &Path, target: &Path) -> Option<String> {
    let rel = target.strip_prefix(root).ok()?;
    let slug = crate::slug::slug_for_rel(rel).ok()?;
    Some(format!("../{slug}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_survive_a_round_trip_including_odd_bytes() {
        for root in ["/srv/notes", "/home/me/My Notes", "/tmp/caf\u{e9} 100%/x"] {
            let encoded = encode_root(Path::new(root));
            assert!(encoded.is_ascii() && !encoded.contains(' '), "{encoded}");
            assert_eq!(decode_root(&encoded), Some(PathBuf::from(root)));
        }
        assert_eq!(encode_root(Path::new("/a b")), "/a%20b");
    }

    #[test]
    fn decode_rejects_relative_empty_and_malformed_values() {
        assert_eq!(decode_root(""), None);
        assert_eq!(decode_root("relative/path"), None);
        assert_eq!(decode_root("/bad%"), None);
        assert_eq!(decode_root("/bad%G1"), None);
        assert_eq!(decode_root("/ok"), Some(PathBuf::from("/ok")));
    }

    #[test]
    fn canvas_url_is_relative_to_the_page_and_stays_under_the_root() {
        let root = Path::new("/srv/notes");
        let url = canvas_url(root, Path::new("/srv/notes/sub/todo.canvas.md")).unwrap();
        let slug = crate::slug::slug_for_rel(Path::new("sub/todo.canvas.md")).unwrap();
        assert_eq!(url, format!("../{slug}/"));
        assert_eq!(canvas_url(root, Path::new("/srv/other/x.canvas.md")), None);
        assert_eq!(canvas_url(root, Path::new("/srv/notes")), None);
    }
}
