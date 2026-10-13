//! Line-based three-way merge (`diff3`) of two edits of the same text.
//!
//! `merge3(base, mine, theirs)` takes the text both edits started from and
//! the two results, and either combines them or reports exactly which line
//! ranges could not be combined. It is a pure function over strings — it
//! knows nothing about canvases — so it works for a node's body and for a
//! whole file alike, and it never writes anything.
//!
//! The rules are `git merge`'s own: a change made on one side only is
//! taken; the same change made on both sides is taken once; two changes
//! that overlap *or touch* (adjacent lines, or two insertions at the same
//! point) are a conflict unless they produced identical text. Lines are
//! compared with their terminator, so `"x"` and `"x\n"` differ, and a
//! CRLF file edited with LF lines conflicts rather than being quietly
//! normalised.

use serde::Serialize;
use similar::{DiffOp, TextDiff};
use std::ops::Range;

/// The outcome of [`merge3`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merge {
    /// Both edits combined without overlap.
    Clean(String),
    /// At least one region was changed differently on both sides.
    Conflict(MergeConflict),
}

/// What could not be combined, and the text with `git`-style markers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MergeConflict {
    /// Every conflicting region, in document order.
    pub hunks: Vec<ConflictHunk>,
    /// The whole merged text with `<<<<<<< mine` / `=======` /
    /// `>>>>>>> theirs` around each conflicting region, for a caller
    /// (an agent, a person in a terminal) that wants to resolve it in text.
    pub marked: String,
}

/// One region both sides changed differently, as zero-based, end-exclusive
/// *line* ranges in each of the three inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConflictHunk {
    pub base: Range<usize>,
    pub mine: Range<usize>,
    pub theirs: Range<usize>,
}

/// How the two sides of a [`DiffRow`] relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// The same line on both sides.
    Same,
    /// A line on each side, different.
    Changed,
    /// Only on the left.
    Removed,
    /// Only on the right.
    Added,
}

/// One row of a side-by-side comparison: the line on each side, with its
/// zero-based index in that side's own text (the numbering a
/// [`ConflictHunk`] uses), without its line terminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: DiffKind,
    pub left: Option<(usize, String)>,
    pub right: Option<(usize, String)>,
}

/// Aligns two texts line by line for a side-by-side view: equal lines are
/// paired, a run of replaced lines is paired up in order (the longer side's
/// surplus becomes `Removed`/`Added` rows), and pure deletions/insertions
/// have an empty opposite side. Every line of each text appears exactly once,
/// in order.
pub fn side_by_side(left: &str, right: &str) -> Vec<DiffRow> {
    let diff = TextDiff::from_lines(left, right);
    let old = diff.old_slices();
    let new = diff.new_slices();
    let line = |s: &str| s.trim_end_matches('\n').trim_end_matches('\r').to_string();
    let mut rows = Vec::new();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for k in 0..len {
                    rows.push(DiffRow {
                        kind: DiffKind::Same,
                        left: Some((old_index + k, line(old[old_index + k]))),
                        right: Some((new_index + k, line(new[new_index + k]))),
                    });
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for k in 0..old_len {
                    rows.push(DiffRow {
                        kind: DiffKind::Removed,
                        left: Some((old_index + k, line(old[old_index + k]))),
                        right: None,
                    });
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for k in 0..new_len {
                    rows.push(DiffRow {
                        kind: DiffKind::Added,
                        left: None,
                        right: Some((new_index + k, line(new[new_index + k]))),
                    });
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                for k in 0..old_len.max(new_len) {
                    let l = (k < old_len).then(|| (old_index + k, line(old[old_index + k])));
                    let r = (k < new_len).then(|| (new_index + k, line(new[new_index + k])));
                    let kind = match (&l, &r) {
                        (Some(_), Some(_)) => DiffKind::Changed,
                        (Some(_), None) => DiffKind::Removed,
                        _ => DiffKind::Added,
                    };
                    rows.push(DiffRow {
                        kind,
                        left: l,
                        right: r,
                    });
                }
            }
        }
    }
    rows
}

/// One changed region of a side relative to the base: `base[b0..b1]` became
/// `side[s0..s1]`. Either range may be empty (a pure insertion or deletion).
#[derive(Debug, Clone, Copy)]
struct Hunk {
    b0: usize,
    b1: usize,
    s0: usize,
    s1: usize,
}

/// Merges `mine` and `theirs`, two edits of `base`.
pub fn merge3(base: &str, mine: &str, theirs: &str) -> Merge {
    // The cheap cases are also the most common ones, and they must not
    // depend on how the diff happens to align.
    if mine == theirs || base == theirs {
        return Merge::Clean(mine.to_string());
    }
    if base == mine {
        return Merge::Clean(theirs.to_string());
    }

    let base_diff = TextDiff::from_lines(base, mine);
    let base_lines: Vec<&str> = base_diff.old_slices().to_vec();
    let mine_lines: Vec<&str> = base_diff.new_slices().to_vec();
    let mine_hunks = hunks(base_diff.ops());

    let theirs_diff = TextDiff::from_lines(base, theirs);
    let theirs_lines: Vec<&str> = theirs_diff.new_slices().to_vec();
    let theirs_hunks = hunks(theirs_diff.ops());

    let mut merged = String::new();
    let mut marked = String::new();
    let mut conflicts = Vec::new();
    let mut base_pos = 0usize;
    // Lines the side has gained (+) or lost (−) in everything before
    // `base_pos`, so a base index maps to the side's own index.
    let mut mine_delta = 0isize;
    let mut theirs_delta = 0isize;
    let (mut i, mut j) = (0usize, 0usize);

    while i < mine_hunks.len() || j < theirs_hunks.len() {
        // Seed the group with whichever side changes the earlier base line.
        let take_mine = match (mine_hunks.get(i), theirs_hunks.get(j)) {
            (Some(m), Some(t)) => m.b0 <= t.b0,
            (Some(_), None) => true,
            _ => false,
        };
        let first = if take_mine {
            mine_hunks[i]
        } else {
            theirs_hunks[j]
        };
        let (group_start, mut group_end) = (first.b0, first.b1);
        let (mut mine_end, mut theirs_end) = (i, j);
        // Grow the group while the next change of either side overlaps or
        // touches it (`>=`, not `>`: adjacent changes are a conflict, as in
        // git).
        loop {
            let mut grew = false;
            while let Some(h) = mine_hunks.get(mine_end) {
                if h.b0 <= group_end {
                    group_end = group_end.max(h.b1);
                    mine_end += 1;
                    grew = true;
                } else {
                    break;
                }
            }
            while let Some(h) = theirs_hunks.get(theirs_end) {
                if h.b0 <= group_end {
                    group_end = group_end.max(h.b1);
                    theirs_end += 1;
                    grew = true;
                } else {
                    break;
                }
            }
            if !grew {
                break;
            }
        }

        // Untouched lines between the previous group and this one.
        let untouched: String = base_lines[base_pos..group_start].concat();
        merged.push_str(&untouched);
        marked.push_str(&untouched);

        let mine_range = side_range(&mine_hunks[i..mine_end], group_start, group_end, mine_delta);
        let theirs_range = side_range(
            &theirs_hunks[j..theirs_end],
            group_start,
            group_end,
            theirs_delta,
        );
        let mine_text: String = mine_lines[mine_range.clone()].concat();
        let theirs_text: String = theirs_lines[theirs_range.clone()].concat();
        let base_text: String = base_lines[group_start..group_end].concat();

        let resolved = if mine_end == i {
            // Only theirs changed this region.
            Some(&theirs_text)
        } else if theirs_end == j || mine_text == theirs_text {
            Some(&mine_text)
        } else if mine_text == base_text {
            Some(&theirs_text)
        } else if theirs_text == base_text {
            Some(&mine_text)
        } else {
            None
        };
        match resolved {
            Some(text) => {
                merged.push_str(text);
                marked.push_str(text);
            }
            None => {
                conflicts.push(ConflictHunk {
                    base: group_start..group_end,
                    mine: mine_range,
                    theirs: theirs_range,
                });
                // Best effort for the plain `merged` text; only `marked`
                // is handed out for a conflict.
                merged.push_str(&mine_text);
                marked.push_str("<<<<<<< mine\n");
                push_terminated(&mut marked, &mine_text);
                marked.push_str("=======\n");
                push_terminated(&mut marked, &theirs_text);
                marked.push_str(">>>>>>> theirs\n");
            }
        }

        for h in &mine_hunks[i..mine_end] {
            mine_delta += (h.s1 - h.s0) as isize - (h.b1 - h.b0) as isize;
        }
        for h in &theirs_hunks[j..theirs_end] {
            theirs_delta += (h.s1 - h.s0) as isize - (h.b1 - h.b0) as isize;
        }
        i = mine_end;
        j = theirs_end;
        base_pos = group_end;
    }

    let tail: String = base_lines[base_pos..].concat();
    merged.push_str(&tail);
    marked.push_str(&tail);

    if conflicts.is_empty() {
        Merge::Clean(merged)
    } else {
        Merge::Conflict(MergeConflict {
            hunks: conflicts,
            marked,
        })
    }
}

/// Appends `text`, adding a newline if it is non-empty and lacks one, so a
/// marker line never lands on the end of a content line.
fn push_terminated(out: &mut String, text: &str) {
    out.push_str(text);
    if !text.is_empty() && !text.ends_with('\n') {
        out.push('\n');
    }
}

/// The side's own line range for the base region `[group_start, group_end)`,
/// given the hunks of that side inside it and the side's net line delta over
/// everything before the region. A side with no hunk in the region has the
/// base lines there, just shifted by the delta.
fn side_range(group: &[Hunk], group_start: usize, group_end: usize, delta: isize) -> Range<usize> {
    let lo = (group_start as isize + delta) as usize;
    let inside: isize = group
        .iter()
        .map(|h| (h.s1 - h.s0) as isize - (h.b1 - h.b0) as isize)
        .sum();
    let hi = (group_end as isize + delta + inside) as usize;
    lo..hi
}

/// Folds a diff's raw operations into non-overlapping changed regions,
/// merging a delete that is immediately followed by an insert (or any other
/// run of adjacent non-equal operations) into one.
fn hunks(ops: &[DiffOp]) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::new();
    for op in ops {
        let h = match *op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => Hunk {
                b0: old_index,
                b1: old_index + old_len,
                s0: new_index,
                s1: new_index,
            },
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => Hunk {
                b0: old_index,
                b1: old_index,
                s0: new_index,
                s1: new_index + new_len,
            },
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => Hunk {
                b0: old_index,
                b1: old_index + old_len,
                s0: new_index,
                s1: new_index + new_len,
            },
        };
        match out.last_mut() {
            Some(last) if last.b1 == h.b0 && last.s1 == h.s0 => {
                last.b1 = h.b1;
                last.s1 = h.s1;
            }
            _ => out.push(h),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(base: &str, mine: &str, theirs: &str) -> String {
        match merge3(base, mine, theirs) {
            Merge::Clean(text) => text,
            Merge::Conflict(c) => panic!("expected a clean merge, got {c:?}"),
        }
    }

    fn conflict(base: &str, mine: &str, theirs: &str) -> MergeConflict {
        match merge3(base, mine, theirs) {
            Merge::Conflict(c) => c,
            Merge::Clean(text) => panic!("expected a conflict, merged to {text:?}"),
        }
    }

    #[test]
    fn edits_in_different_places_are_both_kept() {
        let base = "a\nb\nc\nd\ne\nf\n";
        let mine = "A\nb\nc\nd\ne\nf\n";
        let theirs = "a\nb\nc\nd\ne\nF\n";
        assert_eq!(clean(base, mine, theirs), "A\nb\nc\nd\ne\nF\n");
    }

    #[test]
    fn a_change_on_one_side_only_is_taken() {
        let base = "one\ntwo\n";
        assert_eq!(clean(base, base, "one\n2\n"), "one\n2\n");
        assert_eq!(clean(base, "one\n2\n", base), "one\n2\n");
    }

    #[test]
    fn the_same_change_on_both_sides_is_taken_once() {
        let base = "a\nb\nc\n";
        let both = "a\nB\nc\n";
        assert_eq!(clean(base, both, both), both);
        // Same change, plus a different one far away on one side.
        let base = "a\nb\nc\nd\ne\nf\ng\n";
        let mine = "a\nB\nc\nd\ne\nf\ng\n";
        let theirs = "a\nB\nc\nd\ne\nf\nG\n";
        assert_eq!(clean(base, mine, theirs), "a\nB\nc\nd\ne\nf\nG\n");
    }

    #[test]
    fn different_changes_to_the_same_line_conflict() {
        let c = conflict("a\nb\nc\n", "a\nmine\nc\n", "a\ntheirs\nc\n");
        assert_eq!(c.hunks.len(), 1);
        assert_eq!(c.hunks[0].base, 1..2);
        assert_eq!(c.hunks[0].mine, 1..2);
        assert_eq!(c.hunks[0].theirs, 1..2);
        assert_eq!(
            c.marked,
            "a\n<<<<<<< mine\nmine\n=======\ntheirs\n>>>>>>> theirs\nc\n"
        );
    }

    #[test]
    fn adjacent_changes_conflict_like_git() {
        let c = conflict("a\nb\nc\nd\n", "a\nB\nc\nd\n", "a\nb\nC\nd\n");
        assert_eq!(c.hunks.len(), 1);
        assert_eq!(c.hunks[0].base, 1..3);
    }

    #[test]
    fn a_deletion_beside_an_untouched_neighbour_merges() {
        let base = "a\nb\nc\nd\ne\n";
        let mine = "a\nc\nd\ne\n"; // deleted b
        let theirs = "a\nb\nc\nd\nE\n"; // edited e
        assert_eq!(clean(base, mine, theirs), "a\nc\nd\nE\n");
    }

    #[test]
    fn a_deletion_against_an_edit_of_the_same_line_conflicts() {
        let c = conflict("a\nb\nc\n", "a\nc\n", "a\nB\nc\n");
        assert_eq!(c.hunks[0].mine, 1..1);
        assert_eq!(c.hunks[0].theirs, 1..2);
        assert_eq!(c.marked, "a\n<<<<<<< mine\n=======\nB\n>>>>>>> theirs\nc\n");
    }

    #[test]
    fn insertions_at_the_same_point_conflict_unless_identical() {
        let base = "a\nz\n";
        let c = conflict(base, "a\nx\nz\n", "a\ny\nz\n");
        assert_eq!(c.hunks[0].base, 1..1);
        assert_eq!(clean(base, "a\nx\nz\n", "a\nx\nz\n"), "a\nx\nz\n");
    }

    #[test]
    fn insertions_far_apart_both_land() {
        let base = "a\nb\nc\nd\ne\n";
        let mine = "X\na\nb\nc\nd\ne\n";
        let theirs = "a\nb\nc\nd\ne\nY\n";
        assert_eq!(clean(base, mine, theirs), "X\na\nb\nc\nd\ne\nY\n");
    }

    #[test]
    fn line_numbers_of_later_conflicts_account_for_earlier_edits() {
        let base = "1\n2\n3\n4\n5\n6\n";
        // Mine inserts two lines at the top and edits line 5;
        // theirs deletes line 2 and edits line 5 differently.
        let mine = "p\nq\n1\n2\n3\n4\nM\n6\n";
        let theirs = "1\n3\n4\nT\n6\n";
        let c = conflict(base, mine, theirs);
        assert_eq!(c.hunks.len(), 1);
        assert_eq!(c.hunks[0].base, 4..5);
        assert_eq!(c.hunks[0].mine, 6..7);
        assert_eq!(c.hunks[0].theirs, 3..4);
        assert!(c.marked.starts_with("p\nq\n1\n3\n4\n<<<<<<< mine\nM\n"));
    }

    #[test]
    fn empty_base_with_the_same_text_on_both_sides_is_clean() {
        assert_eq!(clean("", "hello\n", "hello\n"), "hello\n");
        assert_eq!(clean("", "hello\n", ""), "hello\n");
        conflict("", "hello\n", "world\n");
    }

    #[test]
    fn a_missing_final_newline_is_a_difference() {
        let base = "a\nb\nc\n";
        let mine = "A\nb\nc\n";
        let theirs = "a\nb\nc"; // no trailing newline
                                // Different lines, so both edits survive exactly.
        assert_eq!(clean(base, mine, theirs), "A\nb\nc");
        // Appending a blank line against dropping the newline touches the
        // same last line and conflicts.
        conflict(base, "a\nb\nc\n\n", theirs);
    }

    #[test]
    fn crlf_lines_survive_untouched() {
        let base = "a\r\nb\r\nc\r\nd\r\n";
        let mine = "A\r\nb\r\nc\r\nd\r\n";
        let theirs = "a\r\nb\r\nc\r\nD\r\n";
        assert_eq!(clean(base, mine, theirs), "A\r\nb\r\nc\r\nD\r\n");
    }

    #[test]
    fn unicode_text_merges_by_line() {
        let base = "привет\nмир\nあいう\nend\n";
        let mine = "Привет\nмир\nあいう\nend\n";
        let theirs = "привет\nмир\nあいう\nконец\n";
        assert_eq!(clean(base, mine, theirs), "Привет\nмир\nあいう\nконец\n");
    }

    #[test]
    fn a_blocks_rewritten_output_merges_with_edits_to_the_prose() {
        let base = concat!(
            "Intro text.\n\n",
            "```sh name=\"build\"\n",
            "make\n",
            "```\n",
            "<!-- meshfox:output name=\"build\" hash=\"0123\" -->\n",
            "old output\n",
            "<!-- /meshfox:output -->\n\n",
            "Closing text.\n",
        );
        // I edit the prose above the block.
        let mine = base.replace("Intro text.", "Intro text, revised.");
        // A run rewrites the cached output.
        let theirs = base.replace("old output", "new output");
        let merged = clean(base, &mine, &theirs);
        assert!(merged.contains("Intro text, revised."));
        assert!(merged.contains("new output"));
        assert!(!merged.contains("old output"));
    }

    #[test]
    fn editing_the_code_while_the_output_is_rewritten_keeps_both() {
        // The fence close sits between the last code line and the output.
        let base = concat!(
            "```sh name=\"build\"\n",
            "make\n",
            "```\n",
            "<!-- meshfox:output name=\"build\" hash=\"0123\" -->\n",
            "old\n",
            "<!-- /meshfox:output -->\n",
        );
        let mine = base.replace("make\n", "make all\n");
        let theirs = base.replace("old\n", "new\n");
        let merged = clean(base, &mine, &theirs);
        assert!(merged.contains("make all\n"));
        assert!(merged.contains("new\n"));
    }

    #[test]
    fn identical_inputs_and_unchanged_sides_short_circuit() {
        assert_eq!(clean("x\n", "x\n", "x\n"), "x\n");
        assert_eq!(clean("x\n", "y\n", "x\n"), "y\n");
        assert_eq!(clean("x\n", "x\n", "y\n"), "y\n");
        assert_eq!(clean("x\n", "y\n", "y\n"), "y\n");
    }

    #[test]
    fn marked_text_terminates_an_unterminated_last_line() {
        let c = conflict("a", "b", "c");
        assert_eq!(c.marked, "<<<<<<< mine\nb\n=======\nc\n>>>>>>> theirs\n");
    }

    fn kinds(rows: &[DiffRow]) -> Vec<DiffKind> {
        rows.iter().map(|r| r.kind).collect()
    }

    #[test]
    fn side_by_side_pairs_equal_lines_and_replaced_runs() {
        let rows = side_by_side("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(
            kinds(&rows),
            vec![DiffKind::Same, DiffKind::Changed, DiffKind::Same]
        );
        assert_eq!(rows[1].left, Some((1, "b".to_string())));
        assert_eq!(rows[1].right, Some((1, "B".to_string())));
    }

    #[test]
    fn side_by_side_keeps_every_line_of_each_side_once_and_in_order() {
        let left = "a\nb\nc\nd\ne\n";
        let right = "a\nX\nY\nZ\nd\ne\nf\n";
        let rows = side_by_side(left, right);
        let l: Vec<usize> = rows
            .iter()
            .filter_map(|r| r.left.as_ref().map(|x| x.0))
            .collect();
        let r: Vec<usize> = rows
            .iter()
            .filter_map(|r| r.right.as_ref().map(|x| x.0))
            .collect();
        assert_eq!(l, vec![0, 1, 2, 3, 4]);
        assert_eq!(r, vec![0, 1, 2, 3, 4, 5, 6]);
        // b,c replaced by X,Y (paired), Z surplus on the right, f appended.
        assert_eq!(
            kinds(&rows),
            vec![
                DiffKind::Same,
                DiffKind::Changed,
                DiffKind::Changed,
                DiffKind::Added,
                DiffKind::Same,
                DiffKind::Same,
                DiffKind::Added,
            ]
        );
    }

    #[test]
    fn side_by_side_shows_pure_deletions_and_strips_terminators() {
        let rows = side_by_side("a\r\nb\r\n", "a\r\n");
        assert_eq!(kinds(&rows), vec![DiffKind::Same, DiffKind::Removed]);
        assert_eq!(rows[0].left, Some((0, "a".to_string())));
        assert_eq!(rows[1].right, None);
        assert!(side_by_side("", "").is_empty());
    }
}
