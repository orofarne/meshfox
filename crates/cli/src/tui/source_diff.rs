//! The side-by-side comparison the source editor shows when a save found
//! the file changed elsewhere and could not combine it with the buffer
//! (`App::save_source_editor`) — the terminal counterpart to the web
//! editors' "compare" (`web/src/ConflictDiff.tsx`).
//!
//! Left is the file as it is on disk now ("theirs"), right is the editor's
//! buffer ("yours"). Lines that both sides changed (the worker's
//! `conflicts`) are marked on top of the ordinary line diff. The view only
//! reads: leaving it is `Esc` (back to the buffer, nothing decided),
//! `t` (take theirs: the buffer becomes the file as it is) or `m` (keep
//! mine: the next save is written over the file as it is now).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use meshfox_core::{side_by_side, DiffKind, DiffRow};

use crate::worker_client::ConflictLines;

/// What the editor should do after a key in the comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffOutcome {
    /// Nothing beyond what the view handled itself (scroll, jump).
    Stay,
    /// Back to the buffer, nothing decided.
    Back,
    /// Replace the buffer with the file as it is on disk.
    TakeTheirs,
    /// Write the buffer over the file as it is on disk.
    KeepMine,
}

pub struct SourceDiff {
    /// The file as someone else left it, and its revision — what a
    /// "keep mine" save is written against.
    pub theirs_text: String,
    pub theirs_rev: String,
    pub rows: Vec<DiffRow>,
    /// `conflict_rows[i]`: row `i` holds a line a conflict covers.
    pub conflict_rows: Vec<bool>,
    /// First row of each conflicting region, in order — `n`/`N` targets.
    pub conflict_starts: Vec<usize>,
    /// Whether the worker actually tried to merge and the edits overlapped
    /// (`true`), or no merge was possible at all (`false`: the version the
    /// buffer started from is no longer in the undo history).
    pub overlapped: bool,
    /// Which region `n`/`N` last landed on (an index into the conflict
    /// starts, or — with none marked — the differing runs). Tracked rather
    /// than derived from `scroll`, which the end of the document clamps.
    focus: usize,
    /// First visible row, and how many columns of each line are scrolled off.
    pub scroll: usize,
    pub hscroll: usize,
    /// The body's height as last drawn — what `PgUp`/`PgDn` move by. Set by
    /// the renderer each frame.
    pub view_height: usize,
}

impl SourceDiff {
    /// Compares `theirs` with `mine`, marking the lines `conflicts` cover.
    pub fn new(
        theirs_text: String,
        theirs_rev: String,
        mine: &str,
        conflicts: &[ConflictLines],
    ) -> Self {
        let rows = side_by_side(&theirs_text, mine);
        let in_conflict = |idx: usize, side: fn(&ConflictLines) -> &std::ops::Range<usize>| {
            conflicts.iter().any(|c| side(c).contains(&idx))
        };
        let conflict_rows: Vec<bool> = rows
            .iter()
            .map(|row| {
                row.left
                    .as_ref()
                    .is_some_and(|(i, _)| in_conflict(*i, |c| &c.theirs))
                    || row
                        .right
                        .as_ref()
                        .is_some_and(|(i, _)| in_conflict(*i, |c| &c.mine))
            })
            .collect();
        let mut conflict_starts = Vec::new();
        for (i, flagged) in conflict_rows.iter().enumerate() {
            if *flagged && (i == 0 || !conflict_rows[i - 1]) {
                conflict_starts.push(i);
            }
        }
        // Land on the first thing worth looking at: the first conflict, or
        // failing that the first difference of any kind.
        let first = conflict_starts
            .first()
            .copied()
            .or_else(|| rows.iter().position(|r| r.kind != DiffKind::Same));
        SourceDiff {
            theirs_text,
            theirs_rev,
            rows,
            conflict_rows,
            conflict_starts,
            overlapped: !conflicts.is_empty(),
            focus: 0,
            scroll: first.map(|i| i.saturating_sub(2)).unwrap_or(0),
            hscroll: 0,
            view_height: 10,
        }
    }

    fn max_scroll(&self) -> usize {
        self.rows.len().saturating_sub(self.view_height.max(1))
    }

    fn scroll_by(&mut self, delta: isize) {
        let target = self.scroll as isize + delta;
        self.scroll = target.clamp(0, self.max_scroll() as isize) as usize;
    }

    /// `n`/`N`: the next/previous conflicting region (wrapping), or — when
    /// nothing is marked — the next/previous run of differing rows.
    fn jump(&mut self, forward: bool) {
        let targets: Vec<usize> = if self.conflict_starts.is_empty() {
            let mut starts = Vec::new();
            let mut prev_same = true;
            for (i, row) in self.rows.iter().enumerate() {
                let differs = row.kind != DiffKind::Same;
                if differs && prev_same {
                    starts.push(i);
                }
                prev_same = !differs;
            }
            starts
        } else {
            self.conflict_starts.clone()
        };
        if targets.is_empty() {
            return;
        }
        let n = targets.len();
        self.focus = if forward {
            (self.focus + 1) % n
        } else {
            (self.focus + n - 1) % n
        };
        self.scroll = targets[self.focus].saturating_sub(2).min(self.max_scroll());
    }

    pub fn on_key(&mut self, key: KeyEvent) -> DiffOutcome {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return DiffOutcome::Stay;
        }
        let page = self.view_height.saturating_sub(1).max(1) as isize;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return DiffOutcome::Back,
            KeyCode::Char('t') => return DiffOutcome::TakeTheirs,
            KeyCode::Char('m') => return DiffOutcome::KeepMine,
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = self.max_scroll(),
            KeyCode::Char('n') => self.jump(true),
            KeyCode::Char('N') | KeyCode::Char('p') => self.jump(false),
            KeyCode::Right | KeyCode::Char('l') => self.hscroll += 8,
            KeyCode::Left | KeyCode::Char('h') => self.hscroll = self.hscroll.saturating_sub(8),
            _ => {}
        }
        DiffOutcome::Stay
    }

    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollDown => self.scroll_by(3),
            MouseEventKind::ScrollUp => self.scroll_by(-3),
            MouseEventKind::ScrollRight => self.hscroll += 8,
            MouseEventKind::ScrollLeft => self.hscroll = self.hscroll.saturating_sub(8),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn lines(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn marks_the_rows_a_conflict_covers_and_lands_on_the_first() {
        let theirs = lines(30).replace("line 20", "line twenty");
        let mine = lines(30).replace("line 20", "line 20 MINE");
        let conflicts = vec![ConflictLines {
            mine: 19..20,
            theirs: 19..20,
        }];
        let diff = SourceDiff::new(theirs, "rev".into(), &mine, &conflicts);
        assert!(diff.overlapped);
        assert_eq!(diff.conflict_starts, vec![19]);
        assert_eq!(diff.conflict_rows.iter().filter(|c| **c).count(), 1);
        assert!(diff.conflict_rows[19]);
        // Parked two rows above it, so some context shows.
        assert_eq!(diff.scroll, 17);
    }

    #[test]
    fn without_conflicts_it_lands_on_the_first_difference_and_says_so() {
        let theirs = lines(30).replace("line 25", "line 25 theirs");
        let diff = SourceDiff::new(theirs, "rev".into(), &lines(30), &[]);
        assert!(!diff.overlapped);
        assert!(diff.conflict_starts.is_empty());
        assert_eq!(diff.scroll, 22);
    }

    #[test]
    fn scrolling_is_clamped_and_n_jumps_between_regions_wrapping() {
        let mut theirs = lines(60);
        theirs = theirs
            .replace("line 10\n", "T10\n")
            .replace("line 50\n", "T50\n");
        let mine = lines(60)
            .replace("line 10\n", "M10\n")
            .replace("line 50\n", "M50\n");
        let conflicts = vec![
            ConflictLines {
                mine: 9..10,
                theirs: 9..10,
            },
            ConflictLines {
                mine: 49..50,
                theirs: 49..50,
            },
        ];
        let mut diff = SourceDiff::new(theirs, "rev".into(), &mine, &conflicts);
        diff.view_height = 20;
        assert_eq!(diff.conflict_starts, vec![9, 49]);
        assert_eq!(diff.scroll, 7);

        // The second region sits near the end, where the view is clamped.
        diff.on_key(key(KeyCode::Char('n')));
        assert_eq!(diff.scroll, 40);
        // Wraps back to the first (not stuck on the clamped one).
        diff.on_key(key(KeyCode::Char('n')));
        assert_eq!(diff.scroll, 7);
        diff.on_key(key(KeyCode::Char('N')));
        assert_eq!(diff.scroll, 40);

        diff.on_key(key(KeyCode::Char('G')));
        assert_eq!(diff.scroll, 40);
        diff.on_key(key(KeyCode::Char('j')));
        assert_eq!(diff.scroll, 40, "clamped at the end");
        diff.on_key(key(KeyCode::Char('g')));
        diff.on_key(key(KeyCode::Char('k')));
        assert_eq!(diff.scroll, 0, "clamped at the start");
    }

    #[test]
    fn the_decision_keys_report_what_they_mean() {
        let mut diff = SourceDiff::new("a\n".into(), "rev".into(), "b\n", &[]);
        assert_eq!(
            diff.on_key(key(KeyCode::Char('t'))),
            DiffOutcome::TakeTheirs
        );
        assert_eq!(diff.on_key(key(KeyCode::Char('m'))), DiffOutcome::KeepMine);
        assert_eq!(diff.on_key(key(KeyCode::Esc)), DiffOutcome::Back);
        assert_eq!(diff.on_key(key(KeyCode::Char('j'))), DiffOutcome::Stay);
        // Ctrl-s must not be mistaken for a decision here.
        let ctrl_s = KeyEvent {
            modifiers: KeyModifiers::CONTROL,
            ..key(KeyCode::Char('s'))
        };
        assert_eq!(diff.on_key(ctrl_s), DiffOutcome::Stay);
    }

    #[test]
    fn horizontal_scroll_never_goes_negative() {
        let mut diff = SourceDiff::new("a\n".into(), "rev".into(), "b\n", &[]);
        diff.on_key(key(KeyCode::Char('h')));
        assert_eq!(diff.hscroll, 0);
        diff.on_key(key(KeyCode::Char('l')));
        diff.on_key(key(KeyCode::Char('l')));
        assert_eq!(diff.hscroll, 16);
        diff.on_key(key(KeyCode::Char('h')));
        assert_eq!(diff.hscroll, 8);
    }
}
