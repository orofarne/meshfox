//! RFC3339 helpers for the `createdAt`/`updatedAt` `meshfox:node` attributes
//! (see SPEC.md's "Timestamps" section). Thin wrappers over the `time`
//! crate rather than hand-rolled parsing — `mdcanvas::ParseError::InvalidTimestamp`
//! treats a malformed value as a hard parse error, so this needs to be
//! actually correct (leap years, days-per-month, valid offsets), not just
//! shape-plausible.
//!
//! Any valid RFC3339 offset is accepted on parse (someone hand-typing
//! `--created-at` may reasonably want their own local offset preserved
//! verbatim in the file — see `node meta --created-at`). meshfox's own
//! automatic stamps (`insert_child_node`, `set_node_body`) always use
//! `now_utc_rfc3339`, which is always `Z` — so two auto-stamped nodes are
//! always directly string-comparable, even though a hand-typed one with a
//! literal offset might not be against one of those.

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// The current instant, formatted as UTC RFC3339 (`Z`, never a numeric
/// offset) — what meshfox itself writes whenever it stamps `createdAt`/
/// `updatedAt` automatically.
pub fn now_utc_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("OffsetDateTime::now_utc() always formats as RFC3339")
}

/// Whether `s` parses as a valid RFC3339 timestamp, any offset included —
/// used to validate `createdAt`/`updatedAt` at parse time (`mdcanvas::parse`)
/// and `--created-at` at the CLI/MCP boundary, before either ever reaches
/// the file.
pub fn is_valid_rfc3339(s: &str) -> bool {
    OffsetDateTime::parse(s, &Rfc3339).is_ok()
}

/// Unix timestamp (UTC-based, offset-independent) for an already-valid
/// RFC3339 string — e.g. straight off a `Node`'s own `created_at`/
/// `updated_at`. What `constraint.rs` exposes as `.created_at_ts`/
/// `.updated_at_ts`, so a constraint script can do real arithmetic instead
/// of only ever comparing the raw strings. `None` if `s` doesn't parse.
pub fn unix_timestamp(s: &str) -> Option<i64> {
    OffsetDateTime::parse(s, &Rfc3339)
        .ok()
        .map(|t| t.unix_timestamp())
}

/// Parses `s` as either an absolute RFC3339 timestamp or a relative
/// duration measured back from now — a bare integer followed by `s`
/// (seconds), `m` (minutes), `h` (hours), `d` (days), or `w` (weeks), e.g.
/// `7d`, `2w`, `1h`, `30m`. Returns the resolved Unix timestamp either way.
///
/// Used only by `node find`'s date filters (`--since`, `--created-after`,
/// ...) — unlike a Starlark constraint (`constraint.rs`, deliberately has
/// no `now()` of its own so the same document always checks the same way
/// regardless of when `meshfox check` runs), a `node find` query is a
/// one-shot, imperative lookup with no such determinism to protect —
/// "now" here is exactly as safe as it is in `git log --since` or
/// `journalctl --since`.
pub fn parse_since(s: &str) -> Option<i64> {
    if let Ok(t) = OffsetDateTime::parse(s, &Rfc3339) {
        return Some(t.unix_timestamp());
    }
    let s = s.trim();
    if s.len() < 2 {
        return None;
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num.parse().ok()?;
    let seconds = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 60 * 60,
        "d" => n * 60 * 60 * 24,
        "w" => n * 60 * 60 * 24 * 7,
        _ => return None,
    };
    Some(OffsetDateTime::now_utc().unix_timestamp() - seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_valid_and_utc() {
        let s = now_utc_rfc3339();
        assert!(s.ends_with('Z'));
        assert!(is_valid_rfc3339(&s));
    }

    #[test]
    fn accepts_an_explicit_offset() {
        assert!(is_valid_rfc3339("2026-08-29T14:00:00+03:00"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(!is_valid_rfc3339("not a timestamp"));
        assert!(!is_valid_rfc3339("2026-13-40T99:99:99Z"));
        assert!(!is_valid_rfc3339("2026-08-29"));
    }

    #[test]
    fn unix_timestamp_is_offset_independent() {
        let utc = unix_timestamp("2026-08-29T11:00:00Z").unwrap();
        let plus3 = unix_timestamp("2026-08-29T14:00:00+03:00").unwrap();
        assert_eq!(utc, plus3);
    }

    #[test]
    fn parse_since_accepts_absolute_rfc3339() {
        assert_eq!(
            parse_since("2026-08-29T11:00:00Z"),
            unix_timestamp("2026-08-29T11:00:00Z")
        );
    }

    #[test]
    fn parse_since_accepts_relative_durations() {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        assert_eq!(parse_since("60s"), Some(now - 60));
        assert_eq!(parse_since("5m"), Some(now - 300));
        assert_eq!(parse_since("2h"), Some(now - 7200));
        assert_eq!(parse_since("1d"), Some(now - 86400));
        assert_eq!(parse_since("1w"), Some(now - 604800));
    }

    #[test]
    fn parse_since_rejects_garbage() {
        assert_eq!(parse_since("not a duration"), None);
        assert_eq!(parse_since("7x"), None);
        assert_eq!(parse_since(""), None);
        assert_eq!(parse_since("d"), None);
    }
}
