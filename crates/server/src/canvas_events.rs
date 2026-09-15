//! Sequenced broadcast log for `ServerEvent`s (canvas mutations, autorun
//! triggers), backing `/api/watch` — a thin, `ServerEvent`-typed wrapper
//! over the shared `seq_log::SeqLog` primitive (see its own module doc
//! comment for the mechanism itself, and for why `run_registry`/
//! `tty_registry` deliberately don't also sit on top of it).

use crate::seq_log::{SeqItem, SeqLog};
use crate::ServerEvent;
use tokio::sync::broadcast;

/// Small on purpose — this buffer's only job is detecting "you missed
/// something", not serving as history; a client that actually fell behind
/// this far is told to resync from scratch rather than replay potentially
/// hundreds of individually-irrelevant `Changed` events.
const CAPACITY: usize = 256;

pub type SeqEvent = SeqItem<ServerEvent>;

pub struct CanvasEventLog(SeqLog<ServerEvent>);

impl CanvasEventLog {
    pub fn new() -> Self {
        CanvasEventLog(SeqLog::new(CAPACITY))
    }

    pub fn push(&self, event: ServerEvent) {
        self.0.push(event);
    }

    pub fn subscribe_from(&self, since: u64) -> (Vec<SeqEvent>, broadcast::Receiver<SeqEvent>, bool) {
        self.0.subscribe_from(since)
    }
}

impl Default for CanvasEventLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_late_subscriber_gets_the_backlog_with_no_gap() {
        let log = CanvasEventLog::new();
        log.push(ServerEvent::Changed);
        log.push(ServerEvent::RunStarted { node_id: "n".into(), block: "b".into() });

        let (backlog, _rx, gap) = log.subscribe_from(0);
        assert_eq!(backlog.len(), 2);
        assert_eq!(backlog[0].seq, 0);
        assert_eq!(backlog[1].seq, 1);
        assert!(!gap);
    }

    #[test]
    fn a_since_older_than_the_buffer_reports_a_gap() {
        let log = CanvasEventLog::new();
        for _ in 0..(CAPACITY + 10) {
            log.push(ServerEvent::Changed);
        }
        let (_backlog, _rx, gap) = log.subscribe_from(0);
        assert!(gap);
    }

    #[tokio::test]
    async fn a_live_event_after_subscribing_is_delivered_once() {
        let log = CanvasEventLog::new();
        let (backlog, mut rx, gap) = log.subscribe_from(0);
        assert!(backlog.is_empty());
        assert!(!gap);

        log.push(ServerEvent::Changed);
        let item = rx.recv().await.unwrap();
        assert_eq!(item.seq, 0);
    }
}
