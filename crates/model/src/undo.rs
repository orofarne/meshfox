//! Undo/redo history of one document: the log's own logic, with no storage
//! in it. `meshfox view` keeps the log in a SQLite session file
//! (`meshfox-server`'s `SqliteStorage`); a page in a browser keeps it in
//! memory ([`MemoryStorage`]). Both are an [`UndoStorage`] under the same
//! [`UndoLog`], so *what undo means* — cursor moves, redo-tail truncation,
//! gaps in `seq`, depth cap, drift against an outside edit, replay — is one
//! piece of code, run against both by [`conformance`].
//!
//! The log never builds a document from a structured diff (that needs the
//! node-level setters of whoever owns the document): [`UndoLog::peek_undo`]
//! and friends hand back the [`UndoEntry`], and the caller replays it and
//! reports back with [`UndoLog::commit_undo`]/[`UndoLog::commit_redo`].
//! A document-wide step is stored as a [`Splice`] — the one span of text that
//! differs — and replays exactly.
//!
//! `seq` numbers behave like SQLite's `AUTOINCREMENT`: never reused, so a
//! redo-tail truncation or depth eviction leaves gaps. Cursor arithmetic is
//! therefore always "the nearest surviving row", never `± 1`.

use std::io;
use std::sync::Mutex;

use crate::body_rev::body_rev;

/// Kept well under any real editing session's own history without ever
/// growing unbounded (~200 steps).
pub const MAX_DEPTH: i64 = 200;

/// One recorded step of history.
#[derive(Debug, Clone, PartialEq)]
pub struct UndoEntry {
    pub seq: i64,
    pub created_at: String,
    pub op_kind: String,
    pub diff_json: Option<String>,
    /// Both whole documents — only on rows written before [`Splice`]
    /// existed; a newer document-wide step has `splice` instead.
    pub raw_before: Option<String>,
    pub raw_after: Option<String>,
    pub splice: Option<Splice>,
}

impl UndoEntry {
    /// The document one step further in `undo`'s direction, given the
    /// `current` document: for a document-wide step, what the document was
    /// before (`undo`) or becomes after (redo) this step. `None` if this row
    /// carries no document-wide replay data (a structured `Diff` row — the
    /// caller replays those itself), or the step no longer applies to
    /// `current` (see [`Splice::undo`]).
    pub fn replay_document(&self, current: &str, undo: bool) -> Option<String> {
        if let Some(splice) = &self.splice {
            return if undo {
                splice.undo(current)
            } else {
                splice.redo(current)
            };
        }
        if undo {
            self.raw_before.clone()
        } else {
            self.raw_after.clone()
        }
    }
}

/// A document-wide step stored as the one span that changed: `before` and
/// `after` share their first `prefix_len` and last `suffix_len` bytes, and
/// differ only in what lies between (`before_mid` / `after_mid`). Both whole
/// documents are also fingerprinted (`body_rev`) so a replay can tell whether
/// the document it is handed is the one this step belongs to.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Splice {
    pub prefix_len: usize,
    pub suffix_len: usize,
    pub before_mid: String,
    pub after_mid: String,
    pub before_rev: String,
    pub after_rev: String,
}

impl Splice {
    /// The splice that turns `before` into `after` (and back).
    pub fn between(before: &str, after: &str) -> Splice {
        let common_prefix = before
            .bytes()
            .zip(after.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        // Both strings agree on these bytes, so a char boundary in one is
        // one in the other.
        let mut prefix_len = common_prefix;
        while !before.is_char_boundary(prefix_len) {
            prefix_len -= 1;
        }
        let max_suffix = before.len().min(after.len()) - prefix_len;
        let mut suffix_len = before
            .bytes()
            .rev()
            .zip(after.bytes().rev())
            .take(max_suffix)
            .take_while(|(a, b)| a == b)
            .count();
        while !before.is_char_boundary(before.len() - suffix_len)
            || !after.is_char_boundary(after.len() - suffix_len)
        {
            suffix_len -= 1;
        }
        Splice {
            prefix_len,
            suffix_len,
            before_mid: before[prefix_len..before.len() - suffix_len].to_string(),
            after_mid: after[prefix_len..after.len() - suffix_len].to_string(),
            before_rev: body_rev(before),
            after_rev: body_rev(after),
        }
    }

    /// The document before this step, given `current` — normally the
    /// document this step produced.
    pub fn undo(&self, current: &str) -> Option<String> {
        self.apply(current, &self.after_rev, &self.after_mid, &self.before_mid)
    }

    /// The document after this step, given `current` — normally the document
    /// this step started from.
    pub fn redo(&self, current: &str) -> Option<String> {
        self.apply(current, &self.before_rev, &self.before_mid, &self.after_mid)
    }

    fn apply(
        &self,
        current: &str,
        expected_rev: &str,
        from_mid: &str,
        to_mid: &str,
    ) -> Option<String> {
        // Exactly the document this step belongs to: put the span back where
        // it came from.
        if body_rev(current) == expected_rev && current.len() >= self.prefix_len + self.suffix_len {
            let suffix_start = current.len() - self.suffix_len;
            return Some(format!(
                "{}{}{}",
                &current[..self.prefix_len],
                to_mid,
                &current[suffix_start..]
            ));
        }
        // The document differs somewhere from what this step saw (an earlier
        // step was replayed that isn't byte-for-byte reversible): still safe
        // if the changed span can be found in exactly one place.
        if from_mid.is_empty() {
            return None;
        }
        let mut at = current.match_indices(from_mid);
        match (at.next(), at.next()) {
            (Some((i, _)), None) => Some(format!(
                "{}{}{}",
                &current[..i],
                to_mid,
                &current[i + from_mid.len()..]
            )),
            _ => None,
        }
    }
}

/// What a [`UndoLog::push`] call records for one step.
pub enum Payload<'a> {
    /// A small structured before/after description, replayed by the owner of
    /// the document. Only the document's new text is kept (as the log's
    /// `last_raw`), never the old one.
    Diff(serde_json::Value),
    /// The whole document's text before this op; stored as a [`Splice`].
    Raw { before: &'a str },
    /// A [`Splice`] for replay, plus a small `diff` kept only to describe the
    /// step to a person (e.g. a rename's old/new id) — never replayed.
    RawWithDiff {
        before: &'a str,
        diff: serde_json::Value,
    },
}

/// Every step this log can still replay, with its replay payload, split at
/// the cursor — what a base search walks (see [`UndoLog::replay_window`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayWindow {
    /// Steps at or below the cursor, newest first: applying each one's *undo*
    /// to the current document, in this order, yields the document as it was
    /// before that step.
    pub applied: Vec<UndoEntry>,
    /// Steps above the cursor (the redo tail), oldest first: applying each
    /// one's *redo* to the current document, in this order, walks forward
    /// through what an undo threw away.
    pub redo: Vec<UndoEntry>,
}

/// One entry as listed by [`UndoLog::history_around`], resolved against the
/// cursor at the time of that call.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub entry: UndoEntry,
    /// `true` if `entry.seq` was at or below the cursor (an undo would revert
    /// it), `false` if it sat in the redo tail.
    pub applied: bool,
}

/// A row about to be recorded.
pub struct NewRow<'a> {
    pub created_at: &'a str,
    pub op_kind: &'a str,
    pub diff_json: Option<&'a str>,
    pub splice_json: Option<&'a str>,
}

/// Which rows a listing wants, by `seq` against the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    All,
    AtOrBelow(i64),
    Above(i64),
}

/// Where the log's rows and its cursor live. Every method is one atomic
/// operation of the store; the *meaning* of them is [`UndoLog`]'s.
pub trait UndoStorage {
    /// The cursor, `0` when nothing was ever recorded.
    fn cursor(&self) -> io::Result<i64>;
    /// The largest `seq` still held, `0` for none.
    fn max_seq(&self) -> io::Result<i64>;
    /// The row at `seq`, with its replay payload.
    fn row(&self, seq: i64) -> io::Result<Option<UndoEntry>>;
    /// The smallest surviving `seq` strictly above `seq`.
    fn min_seq_above(&self, seq: i64) -> io::Result<Option<i64>>;
    /// The largest surviving `seq` strictly below `seq`, `0` for none.
    fn max_seq_below(&self, seq: i64) -> io::Result<i64>;
    /// The document text the log last saw, `None` if it has no record at all.
    fn last_raw(&self) -> io::Result<Option<String>>;
    /// Starts the record (cursor `0`) with `last_raw`, unless one exists.
    fn seed(&self, last_raw: &str) -> io::Result<()>;
    /// Moves the cursor and the remembered document text.
    fn set_position(&self, cursor: i64, last_raw: &str) -> io::Result<()>;
    /// In one step: drops every row above the cursor, records `row` under a
    /// fresh `seq` (never one used before), points the cursor at it with
    /// `last_raw`, and evicts the oldest rows beyond `max_depth`.
    fn append(&self, row: NewRow<'_>, last_raw: &str, max_depth: i64) -> io::Result<()>;
    /// Rows in `range`, by `seq` ascending or descending, at most `limit`.
    /// `payload` false may leave out the replay payload (`raw_*`, `splice`):
    /// a listing is only ever described for a person.
    fn list(
        &self,
        range: Range,
        descending: bool,
        limit: Option<usize>,
        payload: bool,
    ) -> io::Result<Vec<UndoEntry>>;
}

pub struct UndoLog<S: UndoStorage> {
    storage: S,
}

impl<S: UndoStorage> UndoLog<S> {
    pub fn new(storage: S) -> Self {
        UndoLog { storage }
    }

    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// Records one history step: truncates any redo tail past the cursor,
    /// adds the row, moves the cursor onto it with `full_after` as the
    /// remembered document, then caps total depth to [`MAX_DEPTH`].
    pub fn push(&self, op_kind: &str, payload: Payload, full_after: &str) -> io::Result<()> {
        let splice_of = |before: &str| {
            serde_json::to_string(&Splice::between(before, full_after))
                .map_err(|e| io::Error::other(e.to_string()))
        };
        let (diff_json, splice_json): (Option<String>, Option<String>) = match payload {
            Payload::Diff(v) => (Some(v.to_string()), None),
            Payload::Raw { before } => (None, Some(splice_of(before)?)),
            Payload::RawWithDiff { before, diff } => {
                (Some(diff.to_string()), Some(splice_of(before)?))
            }
        };
        let created_at = crate::timestamp::now_utc_rfc3339();
        self.storage.append(
            NewRow {
                created_at: &created_at,
                op_kind,
                diff_json: diff_json.as_deref(),
                splice_json: splice_json.as_deref(),
            },
            full_after,
            MAX_DEPTH,
        )
    }

    /// Compares `current_raw` (freshly read, at startup) against what this log
    /// last recorded as the document. Never recorded anything: seeds it with
    /// `current_raw`, returns `false`. Recorded and unchanged: `false`.
    /// Recorded and different — something wrote the document without going
    /// through this log — records one `"external_edit"` step and returns
    /// `true`.
    pub fn reconcile_startup_drift(&self, current_raw: &str) -> io::Result<bool> {
        match self.storage.last_raw()? {
            None => {
                self.storage.seed(current_raw)?;
                Ok(false)
            }
            Some(last_raw) if last_raw == current_raw => Ok(false),
            Some(last_raw) => {
                self.push(
                    "external_edit",
                    Payload::Raw { before: &last_raw },
                    current_raw,
                )?;
                Ok(true)
            }
        }
    }

    /// How many steps into the log the document has been committed, `0` for
    /// "nothing to undo".
    pub fn cursor(&self) -> io::Result<i64> {
        self.storage.cursor()
    }

    /// `true` if there's a step an undo could act on right now.
    pub fn can_undo(&self) -> io::Result<bool> {
        Ok(self.cursor()? > 0)
    }

    /// `true` if a row past the cursor still sits in the log (a fresh edit
    /// would have truncated it).
    pub fn can_redo(&self) -> io::Result<bool> {
        let cursor = self.cursor()?;
        Ok(self.storage.max_seq()? > cursor)
    }

    /// The entry an undo would act on right now — the row at the cursor —
    /// without moving anything.
    pub fn peek_undo(&self) -> io::Result<Option<UndoEntry>> {
        let cursor = self.cursor()?;
        if cursor == 0 {
            return Ok(None);
        }
        self.storage.row(cursor)
    }

    /// The entry a redo would act on right now — the smallest surviving `seq`
    /// strictly above the cursor. Deliberately not `cursor + 1`: a redo-tail
    /// truncation or depth eviction can leave a gap there while a later row
    /// survives.
    pub fn peek_redo(&self) -> io::Result<Option<UndoEntry>> {
        let cursor = self.cursor()?;
        match self.storage.min_seq_above(cursor)? {
            Some(seq) => self.storage.row(seq),
            None => Ok(None),
        }
    }

    /// The cursor value undoing `entry_seq` would produce — the largest
    /// surviving `seq` strictly below it, `0` if none — without moving
    /// anything. Not `entry_seq - 1`: `seq` is never reused, so that can land
    /// on a gap with no row, leaving `can_undo` true off a phantom position.
    pub fn cursor_after_undoing(&self, entry_seq: i64) -> io::Result<i64> {
        self.storage.max_seq_below(entry_seq)
    }

    /// Moves the cursor back past `entry_seq` (the row [`Self::peek_undo`]
    /// returned), `new_raw` being the document after the undo. The row stays
    /// (a redo can reach it).
    pub fn commit_undo(&self, entry_seq: i64, new_raw: &str) -> io::Result<()> {
        let new_cursor = self.cursor_after_undoing(entry_seq)?;
        self.storage.set_position(new_cursor, new_raw)
    }

    /// Moves the cursor forward onto `entry_seq` (the row [`Self::peek_redo`]
    /// returned).
    pub fn commit_redo(&self, entry_seq: i64, new_raw: &str) -> io::Result<()> {
        self.storage.set_position(entry_seq, new_raw)
    }

    /// Most recent steps first, up to `limit`, with their payloads.
    pub fn history(&self, limit: usize) -> io::Result<Vec<UndoEntry>> {
        self.storage.list(Range::All, true, Some(limit), true)
    }

    /// All retained steps with their replay payload, split at the cursor.
    pub fn replay_window(&self) -> io::Result<ReplayWindow> {
        let cursor = self.cursor()?;
        Ok(ReplayWindow {
            applied: self
                .storage
                .list(Range::AtOrBelow(cursor), true, None, true)?,
            redo: self.storage.list(Range::Above(cursor), false, None, true)?,
        })
    }

    /// Every applied step, newest first, capped to the last `limit`, behind
    /// the *entire* redo tail (never capped; it is bounded by how much was
    /// undone) — the whole list by `seq` descending, each entry knowing
    /// which side of the cursor it is on. Without replay payloads: a panel of
    /// 200 steps must not pull 200 copies of the document into memory.
    pub fn history_around(&self, limit: usize) -> io::Result<Vec<HistoryEntry>> {
        let cursor = self.cursor()?;
        let redo_tail = self.storage.list(Range::Above(cursor), true, None, false)?;
        let applied = self
            .storage
            .list(Range::AtOrBelow(cursor), true, Some(limit), false)?;
        Ok(redo_tail
            .into_iter()
            .map(|entry| HistoryEntry {
                entry,
                applied: false,
            })
            .chain(applied.into_iter().map(|entry| HistoryEntry {
                entry,
                applied: true,
            }))
            .collect())
    }

    /// Walks the cursor to `target_seq`, one undo or redo at a time, handing
    /// each entry to `step` — which turns the current document into the next
    /// (replaying the entry, and doing whatever else a step means to its
    /// owner, e.g. writing it out) or fails. Returns the final document and
    /// whether anything moved.
    ///
    /// Running out of steps in a direction stops quietly (an out-of-range
    /// target ends where it could), so does a backstop on the number of
    /// steps; a failing `step` stops with its error. `target_seq` must be `0`
    /// or an existing row's `seq`: one in a gap between two rows is reachable
    /// from neither side, and the walk would flip direction forever.
    pub fn jump_to<E: From<io::Error>>(
        &self,
        target_seq: i64,
        mut raw: String,
        mut step: impl FnMut(&UndoEntry, &str, bool) -> Result<String, E>,
    ) -> Result<(String, bool), E> {
        const MAX_STEPS: u32 = 10_000;
        let mut steps = 0u32;
        loop {
            let cursor = self.cursor()?;
            if cursor == target_seq || steps >= MAX_STEPS {
                break;
            }
            let undo = cursor > target_seq;
            let entry = if undo {
                self.peek_undo()?
            } else {
                self.peek_redo()?
            };
            let Some(entry) = entry else {
                break;
            };
            let next = step(&entry, &raw, undo)?;
            if undo {
                self.commit_undo(entry.seq, &next)?;
            } else {
                self.commit_redo(entry.seq, &next)?;
            }
            raw = next;
            steps += 1;
        }
        Ok((raw, steps > 0))
    }
}

#[derive(Default)]
struct MemoryInner {
    rows: Vec<UndoEntry>,
    /// `(cursor, last_raw)`, `None` until the log is seeded or first written.
    meta: Option<(i64, String)>,
    /// The next `seq` to hand out; only ever grows.
    next_seq: i64,
}

/// A log held in memory: what a page in a browser uses.
#[derive(Default)]
pub struct MemoryStorage {
    inner: Mutex<MemoryInner>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

fn without_payload(entry: &UndoEntry) -> UndoEntry {
    UndoEntry {
        raw_before: None,
        raw_after: None,
        splice: None,
        ..entry.clone()
    }
}

impl UndoStorage for MemoryStorage {
    fn cursor(&self) -> io::Result<i64> {
        Ok(self.inner.lock().unwrap().meta.as_ref().map_or(0, |m| m.0))
    }

    fn max_seq(&self) -> io::Result<i64> {
        Ok(self.inner.lock().unwrap().rows.last().map_or(0, |r| r.seq))
    }

    fn row(&self, seq: i64) -> io::Result<Option<UndoEntry>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .rows
            .iter()
            .find(|r| r.seq == seq)
            .cloned())
    }

    fn min_seq_above(&self, seq: i64) -> io::Result<Option<i64>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .rows
            .iter()
            .map(|r| r.seq)
            .find(|s| *s > seq))
    }

    fn max_seq_below(&self, seq: i64) -> io::Result<i64> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .rows
            .iter()
            .map(|r| r.seq)
            .filter(|s| *s < seq)
            .max()
            .unwrap_or(0))
    }

    fn last_raw(&self) -> io::Result<Option<String>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .meta
            .as_ref()
            .map(|m| m.1.clone()))
    }

    fn seed(&self, last_raw: &str) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.meta.is_none() {
            inner.meta = Some((0, last_raw.to_string()));
        }
        Ok(())
    }

    fn set_position(&self, cursor: i64, last_raw: &str) -> io::Result<()> {
        self.inner.lock().unwrap().meta = Some((cursor, last_raw.to_string()));
        Ok(())
    }

    fn append(&self, row: NewRow<'_>, last_raw: &str, max_depth: i64) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let cursor = inner.meta.as_ref().map_or(0, |m| m.0);
        inner.rows.retain(|r| r.seq <= cursor);
        inner.next_seq += 1;
        let seq = inner.next_seq;
        let splice = match row.splice_json {
            Some(json) => serde_json::from_str(json).ok(),
            None => None,
        };
        inner.rows.push(UndoEntry {
            seq,
            created_at: row.created_at.to_string(),
            op_kind: row.op_kind.to_string(),
            diff_json: row.diff_json.map(str::to_string),
            raw_before: None,
            raw_after: None,
            splice,
        });
        inner.meta = Some((seq, last_raw.to_string()));
        let over = inner.rows.len().saturating_sub(max_depth as usize);
        inner.rows.drain(..over);
        Ok(())
    }

    fn list(
        &self,
        range: Range,
        descending: bool,
        limit: Option<usize>,
        payload: bool,
    ) -> io::Result<Vec<UndoEntry>> {
        let inner = self.inner.lock().unwrap();
        let mut rows: Vec<UndoEntry> = inner
            .rows
            .iter()
            .filter(|r| match range {
                Range::All => true,
                Range::AtOrBelow(c) => r.seq <= c,
                Range::Above(c) => r.seq > c,
            })
            .map(|r| {
                if payload {
                    r.clone()
                } else {
                    without_payload(r)
                }
            })
            .collect();
        if descending {
            rows.reverse();
        }
        if let Some(limit) = limit {
            rows.truncate(limit);
        }
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_log() -> UndoLog<MemoryStorage> {
        UndoLog::new(MemoryStorage::new())
    }

    #[test]
    fn the_memory_log_behaves_as_every_log_must() {
        crate::undo_conformance::run_all(&memory_log);
    }

    #[test]
    fn jump_to_walks_undo_and_redo_to_a_target_and_stops_where_it_runs_out() {
        let log = memory_log();
        let docs = ["v0", "v1", "v2", "v3"];
        for pair in docs.windows(2) {
            log.push("raw_replace", Payload::Raw { before: pair[0] }, pair[1])
                .unwrap();
        }
        let replay = |entry: &UndoEntry, current: &str, undo: bool| -> io::Result<String> {
            entry
                .replay_document(current, undo)
                .ok_or_else(|| io::Error::other("does not apply"))
        };
        // From v3 back to the start, in one walk.
        let (raw, moved) = log.jump_to(0, "v3".into(), replay).unwrap();
        assert_eq!((raw.as_str(), moved), ("v0", true));
        assert!(!log.can_undo().unwrap() && log.can_redo().unwrap());
        // Forward to seq 2.
        let (raw, moved) = log.jump_to(2, raw, replay).unwrap();
        assert_eq!((raw.as_str(), moved), ("v2", true));
        // Already there: nothing moves.
        let (raw, moved) = log.jump_to(2, raw, replay).unwrap();
        assert_eq!((raw.as_str(), moved), ("v2", false));
        // Past the end: goes as far as it can, without an error.
        let (raw, moved) = log.jump_to(99, raw, replay).unwrap();
        assert_eq!((raw.as_str(), moved), ("v3", true));
        assert_eq!(log.cursor().unwrap(), 3);
    }

    #[test]
    fn a_step_that_fails_stops_the_walk_with_its_error() {
        let log = memory_log();
        log.push("raw_replace", Payload::Raw { before: "a" }, "b")
            .unwrap();
        log.push("raw_replace", Payload::Raw { before: "b" }, "c")
            .unwrap();
        let err = log
            .jump_to::<io::Error>(0, "c".into(), |_, _, _| Err(io::Error::other("no")))
            .unwrap_err();
        assert_eq!(err.to_string(), "no");
        assert_eq!(log.cursor().unwrap(), 2, "nothing was committed");
    }

    #[test]
    fn seq_numbers_are_never_reused_after_a_redo_tail_is_dropped() {
        let log = memory_log();
        log.push("a", Payload::Raw { before: "0" }, "1").unwrap();
        let first = log.peek_undo().unwrap().unwrap();
        log.commit_undo(first.seq, "0").unwrap();
        log.push("b", Payload::Raw { before: "0" }, "2").unwrap();
        // The dropped row's number is not handed out again.
        assert_eq!(first.seq, 1);
        assert_eq!(log.peek_undo().unwrap().unwrap().seq, 2);
    }

    // ---- Splice: a document-wide step stored as the one span that changed ----

    fn roundtrip(before: &str, after: &str) -> Splice {
        let splice = Splice::between(before, after);
        assert_eq!(
            splice.undo(after).as_deref(),
            Some(before),
            "undo of {before:?} -> {after:?}"
        );
        assert_eq!(
            splice.redo(before).as_deref(),
            Some(after),
            "redo of {before:?} -> {after:?}"
        );
        splice
    }

    #[test]
    fn a_splice_replays_every_shape_of_change_in_both_directions() {
        roundtrip("abc", "abc");
        roundtrip("abc", "abXc");
        roundtrip("abXc", "abc");
        roundtrip("", "new document");
        roundtrip("old document", "");
        roundtrip("start middle end", "start MIDDLE end");
        roundtrip("a\nb\nc\n", "a\nb\nc\nd\n");
        // The same text before and after a repeated pattern.
        roundtrip("xyxyxy", "xyxy");
        roundtrip("aaaa", "aaaaaa");
    }

    #[test]
    fn a_splice_never_cuts_through_a_multibyte_character() {
        // The changed character shares leading/trailing bytes with its
        // neighbours in UTF-8 (Cyrillic а/б, a two-byte pair), so a naive
        // byte-wise common prefix/suffix would split one.
        for (before, after) in [
            ("привет мир", "привет, мир"),
            ("аб", "ав"),
            ("héllo wörld", "héllo wörld!"),
            ("日本語のテキスト", "日本語のテキストです"),
            ("a→b", "a←b"),
        ] {
            let splice = roundtrip(before, after);
            // `before_mid`/`after_mid` are valid strings by construction;
            // what matters is the replay above reproduced exact text.
            assert!(before.is_char_boundary(splice.prefix_len));
            assert!(before.is_char_boundary(before.len() - splice.suffix_len));
        }
    }

    #[test]
    fn a_splice_of_a_small_edit_stores_the_edit_not_the_document() {
        let before = format!("{}\nbody a\n{}", "x".repeat(500_000), "y".repeat(500_000));
        let after = before.replacen("body a", "body a edited", 1);
        let splice = Splice::between(&before, &after);
        assert_eq!(splice.before_mid, "");
        assert_eq!(splice.after_mid, " edited");
        let stored = serde_json::to_string(&splice).unwrap();
        assert!(
            stored.len() < 1_000,
            "stored {} bytes for a 7-byte edit",
            stored.len()
        );
        assert_eq!(splice.undo(&after).as_deref(), Some(before.as_str()));
    }

    /// The document differs somewhere else from what the step saw (an
    /// earlier step was replayed that wasn't byte-for-byte reversible):
    /// still replayable when the changed span sits in exactly one place, and
    /// refused — not guessed at — when it doesn't.
    #[test]
    fn a_splice_tolerates_drift_elsewhere_only_when_the_span_is_unambiguous() {
        let before = "head\nline one\ntail\n";
        let after = "head\nline ONE\ntail\n";
        let splice = Splice::between(before, after);
        // Whitespace elsewhere moved: the offsets no longer line up, but
        // "ONE" is in one place only.
        let drifted = "head \nline ONE\ntail\n";
        assert_eq!(
            splice.undo(drifted).as_deref(),
            Some("head \nline one\ntail\n")
        );
        // The span appears twice: refuse.
        let ambiguous = "ONE head\nline ONE\ntail\n";
        assert_eq!(splice.undo(ambiguous), None);
        // A pure insertion has no span to look for: refuse when drifted.
        let insertion = Splice::between("ab", "aXb");
        assert_eq!(insertion.undo("a  b"), None);
        // ...but replays exactly against the right document.
        assert_eq!(insertion.undo("aXb").as_deref(), Some("ab"));
    }
}
