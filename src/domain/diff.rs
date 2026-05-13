use similar::{ChangeTag, TextDiff};
use std::ops::Range;

/// Result of diffing two files at the line level.
#[derive(Debug)]
pub struct LineDiff {
    /// The operations describing how the old text maps to the new text.
    pub ops: Vec<DiffOp>,
}

/// A single diff operation on a range of lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    /// Lines `old_range` and `new_range` are equal.
    Equal {
        old_range: Range<usize>,
        new_range: Range<usize>,
    },
    /// Lines `old_range` were deleted (no corresponding new lines).
    Delete { old_range: Range<usize> },
    /// Lines `new_range` were inserted (no corresponding old lines).
    Insert { new_range: Range<usize> },
    /// Lines `old_range` were replaced by `new_range`.
    Replace {
        old_range: Range<usize>,
        new_range: Range<usize>,
    },
}

/// Compute a line-level diff between two texts.
pub fn diff_lines(old: &str, new: &str) -> LineDiff {
    let text_diff = TextDiff::configure()
        .algorithm(similar::Algorithm::Histogram)
        .diff_lines(old, new);
    let ops = text_diff
        .ops()
        .iter()
        .map(|op| match op {
            similar::DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => DiffOp::Equal {
                old_range: *old_index..*old_index + len,
                new_range: *new_index..*new_index + len,
            },
            similar::DiffOp::Delete {
                old_index, old_len, ..
            } => DiffOp::Delete {
                old_range: *old_index..*old_index + old_len,
            },
            similar::DiffOp::Insert {
                new_index, new_len, ..
            } => DiffOp::Insert {
                new_range: *new_index..*new_index + new_len,
            },
            similar::DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => DiffOp::Replace {
                old_range: *old_index..*old_index + old_len,
                new_range: *new_index..*new_index + new_len,
            },
        })
        .collect();

    LineDiff { ops }
}

/// Summary statistics for a file diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffStat {
    /// Number of lines added.
    pub added: usize,
    /// Number of lines deleted.
    pub deleted: usize,
}

/// Compute line-level diff statistics from diff operations.
pub fn diff_stat(ops: &[DiffOp]) -> DiffStat {
    let mut added = 0usize;
    let mut deleted = 0usize;
    for op in ops {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { old_range } => deleted += old_range.len(),
            DiffOp::Insert { new_range } => added += new_range.len(),
            DiffOp::Replace {
                old_range,
                new_range,
            } => {
                deleted += old_range.len();
                added += new_range.len();
            }
        }
    }
    DiffStat { added, deleted }
}

/// A single inline (word-level) change span within a line.
/// Uses byte ranges into the original line string for zero-copy ergonomics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSpan {
    pub tag: InlineTag,
    /// Byte range into the source line.
    pub range: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineTag {
    Equal,
    Delete,
    Insert,
}

/// Minimum ratio of matching tokens to total tokens for inline refinement.
/// Below this threshold, the lines are too different for word-level highlighting
/// to be useful, so we skip it and treat the entire line as changed.
const INLINE_MIN_RATIO: f32 = 0.4;

/// Compute word-level inline diff between an old line and a new line.
/// Returns (old_spans, new_spans) where each span is a byte range into
/// the respective input line, tagged as equal/delete/insert.
///
/// If the lines are too dissimilar (below `INLINE_MIN_RATIO`), returns empty
/// spans so the caller can fall back to whole-line highlighting.
pub fn diff_inline(old_line: &str, new_line: &str) -> (Vec<InlineSpan>, Vec<InlineSpan>) {
    let old_tokens = tokenize(old_line);
    let new_tokens = tokenize(new_line);

    let diff = TextDiff::configure()
        .algorithm(similar::Algorithm::Histogram)
        .diff_slices(&old_tokens, &new_tokens);

    // Check similarity ratio — if too low, inline highlights would be noisy.
    if diff.ratio() < INLINE_MIN_RATIO {
        return (Vec::new(), Vec::new());
    }

    let mut old_spans = Vec::new();
    let mut new_spans = Vec::new();
    let mut old_offset = 0usize;
    let mut new_offset = 0usize;

    for change in diff.iter_all_changes() {
        let text = change.value();
        let len = text.len();
        match change.tag() {
            ChangeTag::Equal => {
                old_spans.push(InlineSpan {
                    tag: InlineTag::Equal,
                    range: old_offset..old_offset + len,
                });
                new_spans.push(InlineSpan {
                    tag: InlineTag::Equal,
                    range: new_offset..new_offset + len,
                });
                old_offset += len;
                new_offset += len;
            }
            ChangeTag::Delete => {
                old_spans.push(InlineSpan {
                    tag: InlineTag::Delete,
                    range: old_offset..old_offset + len,
                });
                old_offset += len;
            }
            ChangeTag::Insert => {
                new_spans.push(InlineSpan {
                    tag: InlineTag::Insert,
                    range: new_offset..new_offset + len,
                });
                new_offset += len;
            }
        }
    }

    (old_spans, new_spans)
}

/// Tokenize a line into words, whitespace runs, and individual punctuation.
/// This gives better inline diff results than `similar::from_words` which
/// groups consecutive non-whitespace as a single token.
fn tokenize(s: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut chars = s.char_indices().peekable();
    while let Some(&(start, ch)) = chars.peek() {
        if ch.is_alphanumeric() || ch == '_' {
            let mut end = start;
            while let Some(&(i, c)) = chars.peek() {
                if c.is_alphanumeric() || c == '_' {
                    end = i + c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(&s[start..end]);
        } else if ch.is_whitespace() {
            let mut end = start;
            while let Some(&(i, c)) = chars.peek() {
                if c.is_whitespace() {
                    end = i + c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(&s[start..end]);
        } else {
            // Single punctuation / symbol character (including multi-byte Unicode).
            chars.next();
            tokens.push(&s[start..start + ch.len_utf8()]);
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_texts() {
        let diff = diff_lines("hello\nworld\n", "hello\nworld\n");
        assert_eq!(diff.ops.len(), 1);
        assert_eq!(
            diff.ops[0],
            DiffOp::Equal {
                old_range: 0..2,
                new_range: 0..2,
            }
        );
    }

    #[test]
    fn test_single_line_change() {
        let diff = diff_lines("old line\n", "new line\n");
        assert_eq!(diff.ops.len(), 1);
        assert_eq!(
            diff.ops[0],
            DiffOp::Replace {
                old_range: 0..1,
                new_range: 0..1,
            }
        );
    }

    #[test]
    fn test_insertion() {
        let diff = diff_lines("a\nc\n", "a\nb\nc\n");
        assert_eq!(diff.ops.len(), 3);
        assert_eq!(
            diff.ops[0],
            DiffOp::Equal {
                old_range: 0..1,
                new_range: 0..1,
            }
        );
        assert_eq!(diff.ops[1], DiffOp::Insert { new_range: 1..2 });
        assert_eq!(
            diff.ops[2],
            DiffOp::Equal {
                old_range: 1..2,
                new_range: 2..3,
            }
        );
    }

    #[test]
    fn test_deletion() {
        let diff = diff_lines("a\nb\nc\n", "a\nc\n");
        assert_eq!(diff.ops.len(), 3);
        assert_eq!(
            diff.ops[0],
            DiffOp::Equal {
                old_range: 0..1,
                new_range: 0..1,
            }
        );
        assert_eq!(diff.ops[1], DiffOp::Delete { old_range: 1..2 });
        assert_eq!(
            diff.ops[2],
            DiffOp::Equal {
                old_range: 2..3,
                new_range: 1..2,
            }
        );
    }

    #[test]
    fn test_all_new() {
        let diff = diff_lines("", "a\nb\n");
        assert_eq!(diff.ops.len(), 1);
        assert_eq!(diff.ops[0], DiffOp::Insert { new_range: 0..2 });
    }

    #[test]
    fn test_all_deleted() {
        let diff = diff_lines("a\nb\n", "");
        assert_eq!(diff.ops.len(), 1);
        assert_eq!(diff.ops[0], DiffOp::Delete { old_range: 0..2 });
    }

    #[test]
    fn test_multi_hunk() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nB\nc\nD\ne\n";
        let diff = diff_lines(old, new);
        // Should have: equal(a), replace(b->B), equal(c), replace(d->D), equal(e)
        assert_eq!(diff.ops.len(), 5);
    }

    #[test]
    fn test_inline_diff_word_change() {
        let old = "hello world";
        let new = "hello earth";
        let (old_spans, new_spans) = diff_inline(old, new);
        // old should have: "hello " equal, "world" delete
        // new should have: "hello " equal, "earth" insert
        assert!(
            old_spans
                .iter()
                .any(|s| s.tag == InlineTag::Delete && &old[s.range.clone()] == "world")
        );
        assert!(
            new_spans
                .iter()
                .any(|s| s.tag == InlineTag::Insert && &new[s.range.clone()] == "earth")
        );
        assert!(
            old_spans
                .iter()
                .any(|s| s.tag == InlineTag::Equal && old[s.range.clone()].contains("hello"))
        );
    }

    #[test]
    fn test_inline_diff_identical() {
        let (old_spans, new_spans) = diff_inline("same text", "same text");
        assert!(old_spans.iter().all(|s| s.tag == InlineTag::Equal));
        assert!(new_spans.iter().all(|s| s.tag == InlineTag::Equal));
    }

    #[test]
    fn test_inline_diff_completely_different() {
        // Lines with no similarity should return empty spans (min_ratio guard).
        let (old_spans, new_spans) = diff_inline("aaa", "zzz");
        assert!(old_spans.is_empty());
        assert!(new_spans.is_empty());
    }

    #[test]
    fn test_inline_diff_ranges_cover_full_line() {
        let old = "foo bar baz";
        let new = "foo qux baz";
        let (old_spans, _new_spans) = diff_inline(old, new);
        // All spans' ranges should cover the full old line without gaps
        let total: usize = old_spans.iter().map(|s| s.range.len()).sum();
        assert_eq!(total, old.len());
    }

    #[test]
    fn test_empty_vs_empty() {
        let diff = diff_lines("", "");
        assert!(diff.ops.is_empty());
    }

    #[test]
    fn test_diff_stat_mixed() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nB\nC\nd\ne\n";
        let diff = diff_lines(old, new);
        let stat = diff_stat(&diff.ops);
        // b,c replaced by B,C (2 deleted, 2 added) + e inserted (1 added)
        assert_eq!(stat.deleted, 2);
        assert_eq!(stat.added, 3);
    }

    #[test]
    fn test_diff_stat_identical() {
        let text = "a\nb\nc\n";
        let stat = diff_stat(&diff_lines(text, text).ops);
        assert_eq!(stat.added, 0);
        assert_eq!(stat.deleted, 0);
    }

    #[test]
    fn test_inline_diff_word_boundary() {
        // Only "arg" vs "args" should differ, not "foo(arg:" as a whole.
        let (old_spans, _new_spans) = diff_inline("def foo(arg: str):", "def foo(args: str):");
        let deleted: Vec<_> = old_spans
            .iter()
            .filter(|s| s.tag == InlineTag::Delete)
            .collect();
        assert_eq!(deleted.len(), 1);
        assert_eq!(&"def foo(arg: str):"[deleted[0].range.clone()], "arg");
    }

    #[test]
    fn test_inline_diff_function_arg() {
        let (old_spans, new_spans) = diff_inline("print(arg)", "print(args)");
        let deleted: Vec<_> = old_spans
            .iter()
            .filter(|s| s.tag == InlineTag::Delete)
            .collect();
        let inserted: Vec<_> = new_spans
            .iter()
            .filter(|s| s.tag == InlineTag::Insert)
            .collect();
        assert_eq!(deleted.len(), 1);
        assert_eq!(&"print(arg)"[deleted[0].range.clone()], "arg");
        assert_eq!(inserted.len(), 1);
        assert_eq!(&"print(args)"[inserted[0].range.clone()], "args");
    }

    #[test]
    fn test_inline_diff_dotted_path() {
        let (old_spans, new_spans) = diff_inline("print(os.environ)", "print(os.sys)");
        let deleted: Vec<_> = old_spans
            .iter()
            .filter(|s| s.tag == InlineTag::Delete)
            .collect();
        let inserted: Vec<_> = new_spans
            .iter()
            .filter(|s| s.tag == InlineTag::Insert)
            .collect();
        assert_eq!(deleted.len(), 1);
        assert_eq!(&"print(os.environ)"[deleted[0].range.clone()], "environ");
        assert_eq!(inserted.len(), 1);
        assert_eq!(&"print(os.sys)"[inserted[0].range.clone()], "sys");
    }

    #[test]
    fn test_tokenize_multibyte_utf8() {
        let tokens = tokenize("café_résumé + λ");
        assert_eq!(tokens, vec!["café_résumé", " ", "+", " ", "λ"]);
    }

    #[test]
    fn test_tokenize_cjk() {
        let tokens = tokenize("hello世界");
        assert_eq!(tokens, vec!["hello世界"]);
    }
}
