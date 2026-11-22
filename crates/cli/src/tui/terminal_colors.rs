//! The terminal's own default foreground/background, asked once at startup
//! with OSC 10 / OSC 11 (`ESC ] 10 ; ? ST`). Used where a color has to be a
//! literal value instead of "whatever the terminal does": SVG text (so a
//! formula follows the terminal's light/dark colors) and the backdrop an
//! image is flattened onto (Sixel has no alpha, and some terminals drop it
//! for inline images too).
//!
//! The query is followed by DA1 (`ESC [ c`), which every terminal answers and
//! answers *after* the OSC replies it does support — so the reply to DA1 says
//! "that was all" without waiting out a timeout on terminals that ignore OSC
//! 10/11, and its reply never leaks into the input the app reads later.

use std::sync::OnceLock;

type Rgb = (u8, u8, u8);

static COLORS: OnceLock<(Option<Rgb>, Option<Rgb>)> = OnceLock::new();

/// The terminal's default foreground, if it told us.
pub fn fg() -> Option<Rgb> {
    COLORS.get().and_then(|c| c.0)
}

/// The terminal's default background, if it told us.
pub fn bg() -> Option<Rgb> {
    COLORS.get().and_then(|c| c.1)
}

/// `rgb:RRRR/GGGG/BBBB` (1–4 hex digits per channel, scaled to 8 bits) or
/// `#rrggbb`.
pub fn parse_color(spec: &str) -> Option<Rgb> {
    let spec = spec.trim();
    if let Some(hex) = spec.strip_prefix('#') {
        if hex.len() == 6 {
            let v = u32::from_str_radix(hex, 16).ok()?;
            return Some(((v >> 16) as u8, (v >> 8) as u8, v as u8));
        }
        return None;
    }
    let channels = spec.strip_prefix("rgb:")?;
    let mut out = [0u8; 3];
    let mut parts = channels.split('/');
    for slot in &mut out {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 4 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(part, 16).ok()?;
        let max = (1u32 << (4 * part.len())) - 1;
        *slot = ((value * 255 + max / 2) / max) as u8;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((out[0], out[1], out[2]))
}

/// What a reply buffer holds so far.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Replies {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    /// The DA1 reply (`ESC [ ? … c`) arrived: nothing more is coming.
    pub complete: bool,
}

pub fn parse_replies(buf: &[u8]) -> Replies {
    let text = String::from_utf8_lossy(buf);
    let mut replies = Replies::default();
    let mut rest: &str = &text;
    while let Some(at) = rest.find('\x1b') {
        rest = &rest[at + 1..];
        if let Some(body) = rest.strip_prefix(']') {
            // OSC: terminated by BEL or ST (`ESC \`).
            let end = body.find(['\x07', '\x1b']).unwrap_or(body.len());
            if let Some(spec) = body[..end].strip_prefix("10;") {
                replies.fg = parse_color(spec);
            } else if let Some(spec) = body[..end].strip_prefix("11;") {
                replies.bg = parse_color(spec);
            }
        } else if let Some(body) = rest.strip_prefix("[?") {
            if body
                .find(|c: char| !(c.is_ascii_digit() || c == ';'))
                .is_some_and(|i| body[i..].starts_with('c'))
            {
                replies.complete = true;
            }
        }
    }
    replies
}

/// Asks the terminal and remembers the answer. Raw mode must already be on.
/// Does nothing under tmux/screen (the query would need passthrough), when
/// stdin/stdout aren't a terminal, or off Unix.
pub fn query() {
    #[cfg(unix)]
    {
        if std::env::var_os("TMUX").is_some() || std::env::var_os("STY").is_some() {
            return;
        }
        let _ = COLORS.set(query_unix());
    }
}

#[cfg(unix)]
fn query_unix() -> (Option<Rgb>, Option<Rgb>) {
    use std::io::Write;
    use std::time::{Duration, Instant};

    // SAFETY: isatty only inspects the descriptors.
    if unsafe { libc::isatty(0) == 0 || libc::isatty(1) == 0 } {
        return (None, None);
    }
    let mut out = std::io::stdout();
    if out
        .write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c")
        .and_then(|_| out.flush())
        .is_err()
    {
        return (None, None);
    }
    let deadline = Instant::now() + Duration::from_millis(300);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || buf.len() > 4096 {
            break;
        }
        let mut fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, count 1.
        let ready = unsafe { libc::poll(&mut fd, 1, left.as_millis().min(300) as i32) };
        if ready <= 0 {
            break;
        }
        let mut chunk = [0u8; 256];
        // SAFETY: `chunk` is a valid writable buffer of the given length.
        let n = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        if parse_replies(&buf).complete {
            break;
        }
    }
    let replies = parse_replies(&buf);
    (replies.fg, replies.bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xterm_color_specs() {
        assert_eq!(parse_color("rgb:ffff/0000/8080"), Some((255, 0, 128)));
        assert_eq!(parse_color("rgb:ff/00/80"), Some((255, 0, 128)));
        assert_eq!(parse_color("rgb:f/0/8"), Some((255, 0, 136)));
        assert_eq!(parse_color("#1a2b3c"), Some((0x1a, 0x2b, 0x3c)));
        for bad in [
            "",
            "rgb:",
            "rgb:ff/00",
            "rgb:ff/00/80/00",
            "rgb:zz/00/00",
            "#12345",
            "red",
        ] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn reads_osc_replies_with_either_terminator_and_the_da1_sentinel() {
        let both = b"\x1b]10;rgb:e5e5/e5e5/e5e5\x07\x1b]11;rgb:2727/2828/2222\x1b\\\x1b[?62;4c";
        assert_eq!(
            parse_replies(both),
            Replies {
                fg: Some((229, 229, 229)),
                bg: Some((39, 40, 34)),
                complete: true
            }
        );
        // A terminal without OSC 10/11 answers only DA1.
        assert_eq!(
            parse_replies(b"\x1b[?1;2c"),
            Replies {
                fg: None,
                bg: None,
                complete: true
            }
        );
        // Partial input is not complete.
        assert!(!parse_replies(b"\x1b]11;rgb:0000/0000/00").complete);
    }
}
