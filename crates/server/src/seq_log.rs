//! Generic seq + ring-buffer + `subscribe_from` primitive — a monotonic
//! `seq` per pushed item, a capped `VecDeque` backlog, and a subscribe call
//! that atomically returns backlog-since-`seq` plus a live
//! `tokio::sync::broadcast::Receiver` under one lock, so nothing already in
//! flight when a caller subscribes is ever missed or double-delivered.
//! Extracted from `canvas_events::CanvasEventLog`, its first user, once a
//! second (`ServerEvent`) confirmed the shape was worth sharing rather than
//! copying again.
//!
//! Deliberately **not** also used by `run_registry`/`tty_registry`, despite
//! looking superficially like the same pattern a third and fourth time —
//! each has a real structural property this generic doesn't (and, for
//! `run_registry`, specifically shouldn't) try to accommodate:
//! - `run_registry::RunHandle`'s terminal `Done` event is deliberately
//!   *never* pushed into its backlog ring buffer at all — only output lines
//!   are. A late subscriber instead reads `RunHandle::outcome()` (a plain,
//!   always-current field) up front, rather than replaying `Done` out of a
//!   backlog. Folding `Done` into a seq'd buffer the way this generic
//!   assumes every broadcast item belongs there would change that
//!   deliberate "outcome is a field, `Done` is broadcast-only" property.
//! - `tty_registry`'s `ByteRing` isn't item/seq-based to begin with: it's a
//!   flat byte buffer that evicts by trimming exactly enough leading bytes
//!   to fit a byte cap, erasing chunk boundaries — a genuinely different
//!   data structure (byte-weighted, identity-less) from this one
//!   (count-capped, discretely-seq'd items), not just a different payload
//!   type.
//!
//! See TODO.canvas.md for the fuller record of this decision.

use std::collections::VecDeque;
use std::sync::Mutex;
use tokio::sync::broadcast;

/// One broadcast item, tagged with its own place in the sequence — what a
/// subscriber's `since` cursor advances past on each reconnect.
#[derive(Debug, Clone)]
pub struct SeqItem<T> {
    pub seq: u64,
    pub item: T,
}

struct RingBuffer<T> {
    cap: usize,
    next_seq: u64,
    items: VecDeque<SeqItem<T>>,
}

impl<T: Clone> RingBuffer<T> {
    fn push(&mut self, item: T) -> SeqItem<T> {
        let seq = self.next_seq;
        self.next_seq += 1;
        let entry = SeqItem { seq, item };
        if self.items.len() >= self.cap {
            self.items.pop_front();
        }
        self.items.push_back(entry.clone());
        entry
    }

    /// Every buffered item at or after `since`, plus whether `since` was
    /// already older than anything still in the buffer (meaning some items
    /// in between may have been evicted — the caller can't tell "nothing
    /// happened" from "something happened but fell off the front" in that
    /// case).
    fn since(&self, since: u64) -> (Vec<SeqItem<T>>, bool) {
        let gap = matches!(self.items.front(), Some(oldest) if since < oldest.seq);
        let backlog = self.items.iter().filter(|i| i.seq >= since).cloned().collect();
        (backlog, gap)
    }
}

pub struct SeqLog<T: Clone> {
    log: Mutex<RingBuffer<T>>,
    tx: broadcast::Sender<SeqItem<T>>,
}

impl<T: Clone> SeqLog<T> {
    pub fn new(cap: usize) -> Self {
        let (tx, _) = broadcast::channel(1024);
        SeqLog {
            log: Mutex::new(RingBuffer { cap, next_seq: 0, items: VecDeque::new() }),
            tx,
        }
    }

    /// Records `item` and broadcasts it to every live subscriber — a
    /// no-subscribers `send` error is the normal, common case (nobody's
    /// watching right now), not something worth surfacing, since the item
    /// is already durably in the ring buffer for whoever subscribes next.
    pub fn push(&self, item: T) {
        let entry = self.log.lock().unwrap().push(item);
        let _ = self.tx.send(entry);
    }

    /// Backlog since `since` (inclusive) plus a receiver for whatever comes
    /// next — subscribing *before* reading the buffered backlog (both
    /// happen under the same lock here) is what guarantees no item already
    /// in flight when this is called is ever missed or double-delivered.
    /// The trailing `bool` is `true` when `since` predates everything still
    /// in the buffer — the caller should treat that as "do a full resync",
    /// not trust the (possibly incomplete) backlog alone.
    pub fn subscribe_from(&self, since: u64) -> (Vec<SeqItem<T>>, broadcast::Receiver<SeqItem<T>>, bool) {
        let log = self.log.lock().unwrap();
        let rx = self.tx.subscribe();
        let (backlog, gap) = log.since(since);
        (backlog, rx, gap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPACITY: usize = 4;

    #[test]
    fn a_late_subscriber_gets_the_backlog_with_no_gap() {
        let log: SeqLog<&str> = SeqLog::new(CAPACITY);
        log.push("a");
        log.push("b");

        let (backlog, _rx, gap) = log.subscribe_from(0);
        assert_eq!(backlog.len(), 2);
        assert_eq!(backlog[0].seq, 0);
        assert_eq!(backlog[1].seq, 1);
        assert!(!gap);
    }

    #[test]
    fn subscribing_from_a_later_seq_skips_earlier_backlog_with_no_gap() {
        let log: SeqLog<&str> = SeqLog::new(CAPACITY);
        log.push("a");
        log.push("b");
        log.push("c");

        let (backlog, _rx, gap) = log.subscribe_from(2);
        assert_eq!(backlog.len(), 1);
        assert_eq!(backlog[0].seq, 2);
        assert!(!gap);
    }

    #[test]
    fn a_since_older_than_the_buffer_reports_a_gap() {
        let log: SeqLog<&str> = SeqLog::new(CAPACITY);
        for _ in 0..(CAPACITY + 2) {
            log.push("x");
        }
        // seq 0 has fallen off the front of a `CAPACITY`-sized buffer —
        // asking for it back can't be answered completely.
        let (_backlog, _rx, gap) = log.subscribe_from(0);
        assert!(gap);
    }

    #[test]
    fn a_fresh_subscriber_with_no_backlog_interest_sees_no_gap() {
        let log: SeqLog<&str> = SeqLog::new(CAPACITY);
        log.push("a");
        // `u64::MAX` is how a first-time (non-reconnecting) client asks for
        // "nothing before now" — never older than the buffer's oldest item.
        let (backlog, _rx, gap) = log.subscribe_from(u64::MAX);
        assert!(backlog.is_empty());
        assert!(!gap);
    }

    #[tokio::test]
    async fn a_live_item_after_subscribing_is_delivered_once() {
        let log: SeqLog<&str> = SeqLog::new(CAPACITY);
        let (backlog, mut rx, gap) = log.subscribe_from(0);
        assert!(backlog.is_empty());
        assert!(!gap);

        log.push("a");
        let item = rx.recv().await.unwrap();
        assert_eq!(item.seq, 0);
        assert_eq!(item.item, "a");
    }
}
