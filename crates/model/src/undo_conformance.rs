//! The behaviour every [`UndoStorage`] must give the [`UndoLog`] above it,
//! written once and run against each: the in-memory log a page uses, and the
//! SQLite one `meshfox view` keeps. (Not test-only code: the server's own
//! tests call it too.)

use crate::undo::{Payload, UndoLog, UndoStorage, MAX_DEPTH};
use serde_json::json;

type Make<'a, S> = &'a dyn Fn() -> UndoLog<S>;

/// Runs every check against logs made by `make` (a fresh, empty log each call).
pub fn run_all<S: UndoStorage>(make: Make<S>) {
    push_assigns_increasing_seq(make);
    a_diff_row_has_no_document_data_and_a_document_row_has_a_splice_not_two_copies(make);
    a_history_listing_leaves_the_replay_payload_in_the_database(make);
    depth_cap_keeps_only_the_most_recent_max_depth_rows(make);
    reconcile_startup_drift_seeds_silently_on_a_fresh_log(make);
    reconcile_startup_drift_is_a_noop_when_unchanged(make);
    reconcile_startup_drift_records_an_external_edit_when_content_moved(make);
    a_fresh_log_cannot_undo_or_redo(make);
    peek_undo_returns_the_row_at_the_cursor_without_moving_it(make);
    commit_undo_moves_the_cursor_back_one_step_and_leaves_the_row_in_place(make);
    commit_undo_updates_last_raw_so_a_later_reconcile_sees_no_drift(make);
    commit_redo_moves_the_cursor_forward_and_updates_last_raw(make);
    a_fresh_push_after_undoing_drops_the_redo_tail(make);
    undo_after_undo_then_a_fresh_edit_does_not_leave_can_undo_disagreeing_with_peek_undo(make);
    redo_after_undo_then_a_fresh_edit_then_undo_reaches_the_surviving_row_past_the_gap(make);
    history_around_marks_applied_vs_redo_tail_on_either_side_of_the_cursor(make);
    history_around_caps_only_the_applied_side_never_the_redo_tail(make);
    replay_window_splits_the_log_at_the_cursor(make);
}

fn push_assigns_increasing_seq<S: UndoStorage>(make: Make<S>) {
    let log = make();
    log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
        .unwrap();
    log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
        .unwrap();

    let history = log.history(10).unwrap();
    assert_eq!(history.len(), 2);
    // Most recent first.
    assert_eq!(history[0].seq, 2);
    assert_eq!(history[1].seq, 1);
}

fn a_diff_row_has_no_document_data_and_a_document_row_has_a_splice_not_two_copies<
    S: UndoStorage,
>(
    make: Make<S>,
) {
    let log = make();
    log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
        .unwrap();
    log.push("raw_replace", Payload::Raw { before: "old" }, "new")
        .unwrap();

    let history = log.history(10).unwrap();
    let diff_row = history
        .iter()
        .find(|e| e.op_kind == "node_upserted")
        .unwrap();
    assert!(diff_row.diff_json.is_some());
    assert!(diff_row.raw_before.is_none());
    assert!(diff_row.raw_after.is_none());
    assert!(diff_row.splice.is_none());

    let raw_row = history.iter().find(|e| e.op_kind == "raw_replace").unwrap();
    assert!(raw_row.diff_json.is_none());
    // Not two whole documents any more: the changed span, which replays
    // in both directions.
    assert!(raw_row.raw_before.is_none());
    assert!(raw_row.raw_after.is_none());
    assert!(raw_row.splice.is_some());
    assert_eq!(raw_row.replay_document("new", true).as_deref(), Some("old"));
    assert_eq!(
        raw_row.replay_document("old", false).as_deref(),
        Some("new")
    );
}

fn a_history_listing_leaves_the_replay_payload_in_the_database<S: UndoStorage>(make: Make<S>) {
    let log = make();
    log.push("raw_replace", Payload::Raw { before: "old" }, "new")
        .unwrap();
    log.push("node_upserted", Payload::Diff(json!({"a": 1})), "newer")
        .unwrap();

    let listing = log.history_around(10).unwrap();
    assert_eq!(listing.len(), 2);
    for item in &listing {
        assert!(item.entry.splice.is_none(), "{:?}", item.entry);
        assert!(item.entry.raw_before.is_none() && item.entry.raw_after.is_none());
    }
    // The kind and the summary data are still there to describe it.
    assert!(listing.iter().any(|h| h.entry.diff_json.is_some()));
    // And the real row, read for replay, still carries its span.
    assert!(log.peek_undo().unwrap().is_some());
    assert!(log.history(10).unwrap().iter().any(|e| e.splice.is_some()));
}

fn depth_cap_keeps_only_the_most_recent_max_depth_rows<S: UndoStorage>(make: Make<S>) {
    let log = make();
    for i in 0..(MAX_DEPTH + 5) {
        log.push("node_upserted", Payload::Diff(json!({"i": i})), "doc")
            .unwrap();
    }
    let history = log.history(10_000).unwrap();
    assert_eq!(history.len() as i64, MAX_DEPTH);
    // The oldest 5 pushes were evicted — the newest surviving row is
    // the very last one pushed, the oldest surviving is exactly
    // MAX_DEPTH back from it.
    assert_eq!(history.first().unwrap().seq, MAX_DEPTH + 5);
    assert_eq!(history.last().unwrap().seq, 6);
}

fn reconcile_startup_drift_seeds_silently_on_a_fresh_log<S: UndoStorage>(make: Make<S>) {
    let log = make();
    let drifted = log.reconcile_startup_drift("# Doc\n").unwrap();
    assert!(!drifted);
    assert!(log.history(10).unwrap().is_empty());
}

fn reconcile_startup_drift_is_a_noop_when_unchanged<S: UndoStorage>(make: Make<S>) {
    let log = make();
    log.reconcile_startup_drift("# Doc\n").unwrap();
    let drifted = log.reconcile_startup_drift("# Doc\n").unwrap();
    assert!(!drifted);
    assert!(log.history(10).unwrap().is_empty());
}

fn reconcile_startup_drift_records_an_external_edit_when_content_moved<S: UndoStorage>(
    make: Make<S>,
) {
    let log = make();
    log.reconcile_startup_drift("# Doc v1\n").unwrap();
    let drifted = log.reconcile_startup_drift("# Doc v2\n").unwrap();
    assert!(drifted);

    let history = log.history(10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].op_kind, "external_edit");
    assert_eq!(
        history[0].replay_document("# Doc v2\n", true).as_deref(),
        Some("# Doc v1\n")
    );
    assert_eq!(
        history[0].replay_document("# Doc v1\n", false).as_deref(),
        Some("# Doc v2\n")
    );

    // A third call sees the now-updated last_raw, not the original.
    let drifted_again = log.reconcile_startup_drift("# Doc v2\n").unwrap();
    assert!(!drifted_again);
    assert_eq!(log.history(10).unwrap().len(), 1);
}

fn a_fresh_log_cannot_undo_or_redo<S: UndoStorage>(make: Make<S>) {
    let log = make();
    assert!(!log.can_undo().unwrap());
    assert!(!log.can_redo().unwrap());
    assert!(log.peek_undo().unwrap().is_none());
    assert!(log.peek_redo().unwrap().is_none());
}

fn peek_undo_returns_the_row_at_the_cursor_without_moving_it<S: UndoStorage>(make: Make<S>) {
    let log = make();
    log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
        .unwrap();
    log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
        .unwrap();

    assert!(log.can_undo().unwrap());
    assert!(!log.can_redo().unwrap());
    let entry = log.peek_undo().unwrap().unwrap();
    assert_eq!(entry.seq, 2);
    // A second peek sees the exact same row — nothing moved.
    assert_eq!(log.peek_undo().unwrap().unwrap().seq, 2);
}

fn commit_undo_moves_the_cursor_back_one_step_and_leaves_the_row_in_place<S: UndoStorage>(
    make: Make<S>,
) {
    let log = make();
    log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
        .unwrap();
    log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
        .unwrap();

    let entry = log.peek_undo().unwrap().unwrap();
    assert_eq!(entry.seq, 2);
    log.commit_undo(entry.seq, "doc-v1").unwrap();

    assert!(log.can_undo().unwrap());
    assert!(log.can_redo().unwrap());
    assert_eq!(log.peek_undo().unwrap().unwrap().seq, 1);
    assert_eq!(log.peek_redo().unwrap().unwrap().seq, 2);
    // The row itself is untouched, not deleted.
    assert_eq!(log.history(10).unwrap().len(), 2);
}

fn commit_undo_updates_last_raw_so_a_later_reconcile_sees_no_drift<S: UndoStorage>(make: Make<S>) {
    let log = make();
    log.reconcile_startup_drift("doc-v0").unwrap();
    log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
        .unwrap();

    let entry = log.peek_undo().unwrap().unwrap();
    log.commit_undo(entry.seq, "doc-v0").unwrap();

    assert!(!log.reconcile_startup_drift("doc-v0").unwrap());
}

fn commit_redo_moves_the_cursor_forward_and_updates_last_raw<S: UndoStorage>(make: Make<S>) {
    let log = make();
    log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
        .unwrap();
    log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
        .unwrap();
    let entry = log.peek_undo().unwrap().unwrap();
    log.commit_undo(entry.seq, "doc-v1").unwrap();

    let redo_entry = log.peek_redo().unwrap().unwrap();
    assert_eq!(redo_entry.seq, 2);
    log.commit_redo(redo_entry.seq, "doc-v2").unwrap();

    assert!(!log.can_redo().unwrap());
    assert!(log.can_undo().unwrap());
    assert!(!log.reconcile_startup_drift("doc-v2").unwrap());
}

fn a_fresh_push_after_undoing_drops_the_redo_tail<S: UndoStorage>(make: Make<S>) {
    let log = make();
    log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
        .unwrap();
    log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
        .unwrap();
    let entry = log.peek_undo().unwrap().unwrap();
    log.commit_undo(entry.seq, "doc-v1").unwrap();
    assert!(log.can_redo().unwrap());

    // A brand-new edit lands on top of the undone cursor instead of
    // being redone — same as any other editor, this discards the redo
    // tail rather than branching history.
    log.push("node_upserted", Payload::Diff(json!({"a": 3})), "doc-v3")
        .unwrap();

    assert!(!log.can_redo().unwrap());
    let history = log.history(10).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[0].diff_json.as_deref(),
        Some(json!({"a": 3}).to_string().as_str())
    );
}

// Regression test for a real bug (found while designing the web e2e
// suite, before this fix): edit once, undo it, edit again (this is
// exactly what discards the first edit's own row as a stale redo
// tail — see `push`'s own doc comment), then undo that second edit
// too. `commit_undo` used to set the new cursor to `entry_seq - 1`
// arithmetically, landing on the now-deleted first row's own `seq` —
// a value `AUTOINCREMENT` never reuses — so `can_undo` (`cursor > 0`)
// kept reporting `true` forever from that phantom position, while
// `peek_undo` (which actually looks the row up) correctly found
// nothing and silently no-opped: an undo button stuck permanently
// "enabled" but inert. See `commit_undo`'s own doc comment for the fix.
fn undo_after_undo_then_a_fresh_edit_does_not_leave_can_undo_disagreeing_with_peek_undo<
    S: UndoStorage,
>(
    make: Make<S>,
) {
    let log = make();
    log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
        .unwrap();
    let first = log.peek_undo().unwrap().unwrap();
    log.commit_undo(first.seq, "doc-v0").unwrap();

    log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
        .unwrap();
    let second = log.peek_undo().unwrap().unwrap();
    log.commit_undo(second.seq, "doc-v0-again").unwrap();

    assert!(!log.can_undo().unwrap());
    assert!(log.peek_undo().unwrap().is_none());
}

// Mirror image of the test above, for `peek_redo`: after the same
// edit/undo/edit/undo sequence, the low-seq row from the *first* edit
// is gone (truncated by the second edit's own redo-tail cleanup) while
// the second edit's own row survives, undone, above the cursor.
// `peek_redo` used to look for it at exactly `cursor + 1` (a gap left
// behind by that truncation, since `seq` never gets reused) instead of
// wherever it actually survived — `can_redo` (`MAX(seq) > cursor`,
// never assumed a fixed offset) correctly said `true` the whole time.
fn redo_after_undo_then_a_fresh_edit_then_undo_reaches_the_surviving_row_past_the_gap<
    S: UndoStorage,
>(
    make: Make<S>,
) {
    let log = make();
    log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
        .unwrap();
    let first = log.peek_undo().unwrap().unwrap();
    log.commit_undo(first.seq, "doc-v0").unwrap();

    log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
        .unwrap();
    let second = log.peek_undo().unwrap().unwrap();
    log.commit_undo(second.seq, "doc-v0-again").unwrap();

    assert!(log.can_redo().unwrap());
    let redoable = log.peek_redo().unwrap();
    assert!(
        redoable.is_some(),
        "can_redo said true but peek_redo found nothing"
    );
    assert_eq!(redoable.unwrap().seq, second.seq);
}

fn history_around_marks_applied_vs_redo_tail_on_either_side_of_the_cursor<S: UndoStorage>(
    make: Make<S>,
) {
    let log = make();
    log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
        .unwrap();
    log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
        .unwrap();
    log.push("node_upserted", Payload::Diff(json!({"a": 3})), "doc-v3")
        .unwrap();
    // Undo the most recent one, so seq 3 sits in the redo tail and 1/2
    // stay applied.
    let entry = log.peek_undo().unwrap().unwrap();
    assert_eq!(entry.seq, 3);
    log.commit_undo(entry.seq, "doc-v2").unwrap();

    let listed = log.history_around(10).unwrap();
    let seqs_and_applied: Vec<(i64, bool)> =
        listed.iter().map(|h| (h.entry.seq, h.applied)).collect();
    // Most-recent/most-future first throughout: the redo tail (seq 3),
    // then the applied steps in descending order (seq 2, seq 1).
    assert_eq!(seqs_and_applied, vec![(3, false), (2, true), (1, true)]);
}

fn history_around_caps_only_the_applied_side_never_the_redo_tail<S: UndoStorage>(make: Make<S>) {
    let log = make();
    for i in 0..5 {
        log.push("node_upserted", Payload::Diff(json!({"i": i})), "doc")
            .unwrap();
    }
    // Undo three of the five, so seqs 3/4/5 sit in the redo tail.
    for _ in 0..3 {
        let entry = log.peek_undo().unwrap().unwrap();
        log.commit_undo(entry.seq, "doc").unwrap();
    }
    assert_eq!(log.cursor().unwrap(), 2);

    // A `limit` smaller than the redo tail's own size still returns
    // every redo-tail row — only the applied side is capped.
    let listed = log.history_around(1).unwrap();
    let seqs_and_applied: Vec<(i64, bool)> =
        listed.iter().map(|h| (h.entry.seq, h.applied)).collect();
    assert_eq!(
        seqs_and_applied,
        vec![(5, false), (4, false), (3, false), (2, true)]
    );
}

fn replay_window_splits_the_log_at_the_cursor<S: UndoStorage>(make: Make<S>) {
    let log = make();
    log.push("raw_replace", Payload::Raw { before: "v0" }, "v1")
        .unwrap();
    log.push("raw_replace", Payload::Raw { before: "v1" }, "v2")
        .unwrap();
    log.push("raw_replace", Payload::Raw { before: "v2" }, "v3")
        .unwrap();
    let entry = log.peek_undo().unwrap().unwrap();
    log.commit_undo(entry.seq, "v2").unwrap();

    let window = log.replay_window().unwrap();
    let applied: Vec<i64> = window.applied.iter().map(|e| e.seq).collect();
    let redo: Vec<i64> = window.redo.iter().map(|e| e.seq).collect();
    assert_eq!(applied, vec![2, 1]);
    assert_eq!(redo, vec![3]);
    // Each side replays from the current document ("v2").
    assert_eq!(
        window.applied[0].replay_document("v2", true).as_deref(),
        Some("v1")
    );
    assert_eq!(
        window.redo[0].replay_document("v2", false).as_deref(),
        Some("v3")
    );
}
