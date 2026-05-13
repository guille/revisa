use super::diff::DiffOp;

/// A single visual row in the scroll-synced diff view.
/// Both panels render the same number of `AlignedRow`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlignedRow {
    /// Both sides have a line. `modified` is true for Replace ops, false for Equal.
    Both {
        left_line: usize,
        right_line: usize,
        modified: bool,
    },
    /// Only the left (old) side has a line — right shows padding.
    LeftOnly { left_line: usize },
    /// Only the right (new) side has a line — left shows padding.
    RightOnly { right_line: usize },
}

/// A hunk is a contiguous group of changed rows (non-Equal) with optional
/// surrounding context lines.
#[derive(Debug, Clone)]
pub struct Hunk {
    /// Index range into the `AlignedRow` vec for this hunk (start..end).
    pub row_range: std::ops::Range<usize>,
}

/// Build the aligned row map from diff operations.
pub fn build_aligned_rows(ops: &[DiffOp]) -> Vec<AlignedRow> {
    let mut rows = Vec::new();

    for op in ops {
        match op {
            DiffOp::Equal {
                old_range,
                new_range,
            } => {
                for (l, r) in old_range.clone().zip(new_range.clone()) {
                    rows.push(AlignedRow::Both {
                        left_line: l,
                        right_line: r,
                        modified: false,
                    });
                }
            }
            DiffOp::Delete { old_range } => {
                for l in old_range.clone() {
                    rows.push(AlignedRow::LeftOnly { left_line: l });
                }
            }
            DiffOp::Insert { new_range } => {
                for r in new_range.clone() {
                    rows.push(AlignedRow::RightOnly { right_line: r });
                }
            }
            DiffOp::Replace {
                old_range,
                new_range,
            } => {
                // Pair up lines where possible, then emit remaining as one-sided.
                let mut old_iter = old_range.clone();
                let mut new_iter = new_range.clone();

                loop {
                    match (old_iter.next(), new_iter.next()) {
                        (Some(l), Some(r)) => {
                            rows.push(AlignedRow::Both {
                                left_line: l,
                                right_line: r,
                                modified: true,
                            });
                        }
                        (Some(l), None) => {
                            rows.push(AlignedRow::LeftOnly { left_line: l });
                        }
                        (None, Some(r)) => {
                            rows.push(AlignedRow::RightOnly { right_line: r });
                        }
                        (None, None) => break,
                    }
                }
            }
        }
    }

    rows
}

/// Extract hunks from the aligned row map.
/// A hunk is a maximal contiguous run of changed rows, extended by `context`
/// equal rows on each side. Overlapping hunks are merged.
pub fn extract_hunks(rows: &[AlignedRow], ops: &[DiffOp], context: usize) -> Vec<Hunk> {
    // First, mark which rows come from changed ops.
    let mut changed = vec![false; rows.len()];
    let mut row_idx = 0;
    for op in ops {
        let count = match op {
            DiffOp::Equal { old_range, .. } | DiffOp::Delete { old_range } => old_range.len(),
            DiffOp::Insert { new_range } => new_range.len(),
            DiffOp::Replace {
                old_range,
                new_range,
            } => old_range.len().max(new_range.len()),
        };
        let is_change = !matches!(op, DiffOp::Equal { .. });
        for i in 0..count {
            if row_idx + i < changed.len() {
                changed[row_idx + i] = is_change;
            }
        }
        row_idx += count;
    }

    // Find contiguous changed regions, extend by context, merge overlapping.
    let mut hunks = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        if changed[i] {
            // Find end of changed region.
            let start = i;
            while i < rows.len() && changed[i] {
                i += 1;
            }
            let end = i;

            // Extend by context.
            let ctx_start = start.saturating_sub(context);
            let ctx_end = (end + context).min(rows.len());

            // Merge with previous hunk if overlapping.
            if let Some(prev) = hunks.last_mut() {
                let prev: &mut Hunk = prev;
                if ctx_start <= prev.row_range.end {
                    prev.row_range.end = ctx_end;
                    continue;
                }
            }

            hunks.push(Hunk {
                row_range: ctx_start..ctx_end,
            });
        } else {
            i += 1;
        }
    }

    hunks
}

/// Find the row index of the next hunk start after `current_row`.
/// Returns `None` if there are no more hunks.
pub fn next_hunk_row(hunks: &[Hunk], current_row: usize) -> Option<usize> {
    hunks
        .iter()
        .find(|h| h.row_range.start > current_row)
        .map(|h| h.row_range.start)
}

/// Find the row index of the previous hunk start before `current_row`.
/// Returns `None` if there are no earlier hunks.
pub fn prev_hunk_row(hunks: &[Hunk], current_row: usize) -> Option<usize> {
    hunks
        .iter()
        .rev()
        .find(|h| h.row_range.start < current_row)
        .map(|h| h.row_range.start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::diff::diff_lines;

    #[test]
    fn test_equal_only() {
        let diff = diff_lines("a\nb\nc\n", "a\nb\nc\n");
        let rows = build_aligned_rows(&diff.ops);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: false,
            }
        );
    }

    #[test]
    fn test_insertion_produces_right_only() {
        let diff = diff_lines("a\nc\n", "a\nb\nc\n");
        let rows = build_aligned_rows(&diff.ops);
        // a(both), b(right-only), c(both)
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: false,
            }
        );
        assert_eq!(rows[1], AlignedRow::RightOnly { right_line: 1 });
        assert_eq!(
            rows[2],
            AlignedRow::Both {
                left_line: 1,
                right_line: 2,
                modified: false,
            }
        );
    }

    #[test]
    fn test_deletion_produces_left_only() {
        let diff = diff_lines("a\nb\nc\n", "a\nc\n");
        let rows = build_aligned_rows(&diff.ops);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], AlignedRow::LeftOnly { left_line: 1 });
    }

    #[test]
    fn test_replace_pairs_then_leftovers() {
        // Replace 2 old lines with 3 new lines
        let diff = diff_lines("old1\nold2\n", "new1\nnew2\nnew3\n");
        let rows = build_aligned_rows(&diff.ops);
        // Should pair: Both(0,0), Both(1,1), RightOnly(2)
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: true,
            }
        );
        assert_eq!(
            rows[1],
            AlignedRow::Both {
                left_line: 1,
                right_line: 1,
                modified: true,
            }
        );
        assert_eq!(rows[2], AlignedRow::RightOnly { right_line: 2 });
    }

    #[test]
    fn test_replace_more_old_than_new() {
        let diff = diff_lines("old1\nold2\nold3\n", "new1\n");
        let rows = build_aligned_rows(&diff.ops);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: true,
            }
        );
        assert_eq!(rows[1], AlignedRow::LeftOnly { left_line: 1 });
        assert_eq!(rows[2], AlignedRow::LeftOnly { left_line: 2 });
    }

    #[test]
    fn test_all_added() {
        let diff = diff_lines("", "a\nb\n");
        let rows = build_aligned_rows(&diff.ops);
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|r| matches!(r, AlignedRow::RightOnly { .. }))
        );
    }

    #[test]
    fn test_all_deleted() {
        let diff = diff_lines("a\nb\n", "");
        let rows = build_aligned_rows(&diff.ops);
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|r| matches!(r, AlignedRow::LeftOnly { .. }))
        );
    }

    #[test]
    fn test_extract_hunks_single() {
        let diff = diff_lines("a\nb\nc\nd\ne\n", "a\nB\nc\nd\ne\n");
        let rows = build_aligned_rows(&diff.ops);
        let hunks = extract_hunks(&rows, &diff.ops, 1);
        assert_eq!(hunks.len(), 1);
        // Changed line is index 1 (b->B), with context=1: rows 0..3
        assert_eq!(hunks[0].row_range.start, 0);
        assert_eq!(hunks[0].row_range.end, 3);
    }

    #[test]
    fn test_extract_hunks_merged() {
        // Two close changes that merge with context
        let diff = diff_lines("a\nb\nc\nd\ne\n", "a\nB\nc\nD\ne\n");
        let rows = build_aligned_rows(&diff.ops);
        let hunks = extract_hunks(&rows, &diff.ops, 1);
        // Changes at row 1 and row 3, with context=1 they overlap → single hunk
        assert_eq!(hunks.len(), 1);
    }

    #[test]
    fn test_extract_hunks_separate() {
        // Two far-apart changes
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\nI\nj\n";
        let diff = diff_lines(old, new);
        let rows = build_aligned_rows(&diff.ops);
        let hunks = extract_hunks(&rows, &diff.ops, 1);
        assert_eq!(hunks.len(), 2);
    }

    #[test]
    fn test_next_prev_hunk() {
        let hunks = vec![
            Hunk { row_range: 2..5 },
            Hunk { row_range: 10..15 },
            Hunk { row_range: 20..25 },
        ];
        assert_eq!(next_hunk_row(&hunks, 0), Some(2));
        assert_eq!(next_hunk_row(&hunks, 5), Some(10));
        assert_eq!(next_hunk_row(&hunks, 25), None);
        assert_eq!(prev_hunk_row(&hunks, 25), Some(20));
        assert_eq!(prev_hunk_row(&hunks, 10), Some(2));
        assert_eq!(prev_hunk_row(&hunks, 2), None);
    }

    #[test]
    fn test_context_zero_hunks() {
        // With context=0, only changed rows form hunks (no surrounding context).
        let old = "a\nb\nc\nd\ne";
        let new = "a\nB\nc\nd\nE";
        let diff = diff_lines(old, new);
        let rows = build_aligned_rows(&diff.ops);
        let hunks = extract_hunks(&rows, &diff.ops, 0);
        // Two separate changes: line 2 and line 5 — should be two hunks.
        assert_eq!(hunks.len(), 2);
        // Each hunk should be exactly 1 row (the changed row, no context).
        assert_eq!(hunks[0].row_range.len(), 1);
        assert_eq!(hunks[1].row_range.len(), 1);
    }
}
