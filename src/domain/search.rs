use crate::app::FileDiffData;
use crate::domain::hunk::AlignedRow;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Which side of the diff a search match is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchSide {
    Left,
    Right,
}

/// A single search match location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// Index into `file_pairs` / `diff_cache`.
    pub file_index: usize,
    /// Index into `aligned_rows` for that file.
    pub data_row: usize,
    /// Which side of the diff.
    pub side: MatchSide,
    /// Byte range within the original line text.
    pub byte_range: Range<usize>,
}

/// Background search results: (query, per-file matches).
pub type BgSearchResults = Option<(String, HashMap<usize, Vec<SearchMatch>>)>;

/// Search state, owned by `AppState`.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct SearchState {
    /// Whether the search sidebar is visible.
    pub open: bool,
    /// Whether the sidebar was hidden before search was opened.
    /// Used to restore sidebar visibility when search closes.
    pub sidebar_was_hidden: bool,
    /// Whether the search input needs focus on the next frame.
    pub needs_focus: bool,
    /// Current query string (preserved across open/close).
    pub query: String,
    /// Per-file match results, keyed by file_index.
    /// Primary storage — all derived structures are built from this.
    file_results: HashMap<usize, Vec<SearchMatch>>,
    /// Flattened, sorted list of all matches across all files.
    /// Sorted by `(file_index, data_row, side)`. Rebuilt when `file_results` changes.
    all_matches: Vec<SearchMatch>,
    /// Index into `all_matches` of the currently focused match.
    /// Only valid when `!all_matches.is_empty()`.
    pub current_match: usize,
    /// Per-row render lookup for the currently selected file.
    /// Keyed by `data_row`, derived from `file_results[selected_file]`.
    /// Rebuilt when selected_file changes or file_results changes.
    render_map: HashMap<usize, Vec<SearchMatch>>,
    /// The query that produced current results.
    pub cached_query: String,
    /// Set of file indices that have been searched for the current query.
    pub searched_files: HashSet<usize>,
    /// Timestamp when the query last changed (for debouncing).
    pub query_changed_at: Option<std::time::Instant>,
    /// The query string that was last dispatched to the background worker.
    /// Used to ignore stale results when the query changes again before results arrive.
    pub dispatched_query: String,
    /// Whether a background search is currently in flight.
    pub searching: bool,
    /// Set of file indices whose search result groups are collapsed in the sidebar.
    pub collapsed_groups: HashSet<usize>,
    /// Cached match index reverse lookup: (file_index, data_row, side, byte_start, byte_end) → index in all_matches.
    /// Rebuilt alongside `all_matches`.
    match_index_map: HashMap<(usize, usize, MatchSide, usize, usize), usize>,
    /// Cached display model for the search sidebar.
    /// Rebuilt when search results or selected file changes.
    cached_groups: Vec<SearchFileGroup>,
    /// Cached sorted file list with match counts.
    cached_files_with_matches: Vec<(usize, usize)>,
    /// When true, the search panel should scroll the current match into view.
    /// Set by match navigation (Ctrl+Up/Down), consumed by the panel renderer.
    pub scroll_to_current: bool,
}

/// A single result row in the search sidebar display.
pub struct SearchResultRow {
    /// Index into `all_matches`.
    pub match_idx: usize,
    /// 1-based line number.
    pub line_num: usize,
    /// Full line text (for preview rendering).
    pub line_text: Box<str>,
    /// Byte range of the match within `line_text`.
    pub byte_range: std::ops::Range<usize>,
}

/// A file group in the search sidebar display.
pub struct SearchFileGroup {
    pub file_idx: usize,
    pub rel_path: String,
    pub match_count: usize,
    /// Index into `all_matches` of the first match in this group.
    pub first_match_idx: Option<usize>,
    pub rows: Vec<SearchResultRow>,
}

impl SearchState {
    /// Update search results synchronously. Used in tests; production code
    /// uses `dispatch_background_search` + `apply_background_results` instead.
    #[cfg(test)]
    pub fn update_matches(
        &mut self,
        diff_cache: &HashMap<usize, FileDiffData>,
        excluded_files: &[bool],
        selected_file: usize,
    ) {
        if self.query.is_empty() {
            self.clear_results();
            return;
        }

        let query_changed = self.query != self.cached_query;
        if query_changed {
            self.file_results.clear();
            self.searched_files.clear();
            self.cached_query.clone_from(&self.query);
        }

        let mut changed = query_changed;

        for (&file_idx, data) in diff_cache {
            if file_idx < excluded_files.len() && excluded_files[file_idx] {
                // Remove results for newly excluded files.
                if self.file_results.remove(&file_idx).is_some() {
                    self.searched_files.remove(&file_idx);
                    changed = true;
                }
                continue;
            }
            if self.searched_files.contains(&file_idx) {
                continue;
            }
            let matches = compute_file_matches(
                file_idx,
                &SearchableFileData::from_diff_data(data),
                &self.query,
            );
            self.file_results.insert(file_idx, matches);
            self.searched_files.insert(file_idx);
            changed = true;
        }

        if changed {
            self.rebuild_derived(selected_file);
        }
    }

    /// Rebuild `render_map` for a new selected file (e.g., after file switch).
    pub fn rebuild_render_map(&mut self, selected_file: usize) {
        self.render_map.clear();
        if let Some(matches) = self.file_results.get(&selected_file) {
            for m in matches {
                self.render_map
                    .entry(m.data_row)
                    .or_default()
                    .push(m.clone());
            }
        }
    }

    /// Clear all search results.
    pub fn clear_results(&mut self) {
        self.file_results.clear();
        self.all_matches.clear();
        self.current_match = 0;
        self.render_map.clear();
        self.cached_query.clear();
        self.searched_files.clear();
        self.match_index_map.clear();
        self.cached_files_with_matches.clear();
        self.cached_groups.clear();
        self.scroll_to_current = false;
    }

    /// Apply results computed by the background search worker.
    /// Ignores results if the query has changed since dispatch.
    pub fn apply_background_results(
        &mut self,
        query: String,
        results: HashMap<usize, Vec<SearchMatch>>,
        selected_file: usize,
    ) {
        self.searching = false;
        if query != self.query {
            return; // stale results, query changed since dispatch
        }
        self.file_results = results;
        self.cached_query = query;
        self.searched_files = self.file_results.keys().copied().collect();
        self.rebuild_derived(selected_file);
    }

    /// Mark the query as changed, starting the debounce timer.
    pub fn mark_query_changed(&mut self) {
        self.query_changed_at = Some(std::time::Instant::now());
    }

    /// Whether a search is in progress (debounce pending or background worker running).
    pub fn is_searching(&self) -> bool {
        self.query_changed_at.is_some() || self.searching
    }

    /// Navigate to the next match. Returns the target match if one exists.
    pub fn next_match(&mut self) -> Option<&SearchMatch> {
        if self.all_matches.is_empty() {
            return None;
        }
        self.current_match = (self.current_match + 1) % self.all_matches.len();
        self.scroll_to_current = true;
        Some(&self.all_matches[self.current_match])
    }

    /// Navigate to the previous match. Returns the target match if one exists.
    pub fn prev_match(&mut self) -> Option<&SearchMatch> {
        if self.all_matches.is_empty() {
            return None;
        }
        if self.current_match == 0 {
            self.current_match = self.all_matches.len() - 1;
        } else {
            self.current_match -= 1;
        }
        self.scroll_to_current = true;
        Some(&self.all_matches[self.current_match])
    }

    /// Get the currently focused match, if any.
    pub fn current(&self) -> Option<&SearchMatch> {
        self.all_matches.get(self.current_match)
    }

    /// Total match count.
    pub fn total_matches(&self) -> usize {
        self.all_matches.len()
    }

    /// Whether there are any matches.
    pub fn has_matches(&self) -> bool {
        !self.all_matches.is_empty()
    }

    /// Get a match by index into `all_matches`.
    pub fn match_at(&self, idx: usize) -> Option<&SearchMatch> {
        self.all_matches.get(idx)
    }

    /// Per-row render lookup for the currently selected file.
    pub fn render_map(&self) -> &HashMap<usize, Vec<SearchMatch>> {
        &self.render_map
    }

    /// Cached display model for the search sidebar.
    pub fn cached_groups(&self) -> &[SearchFileGroup] {
        &self.cached_groups
    }

    /// Sorted file indices that have matches (for testing).
    #[cfg(test)]
    pub fn files_with_matches(&self) -> Vec<(usize, usize)> {
        let mut files: Vec<(usize, usize)> = self
            .file_results
            .iter()
            .filter(|(_, matches)| !matches.is_empty())
            .map(|(&idx, matches)| (idx, matches.len()))
            .collect();
        files.sort_by_key(|&(idx, _)| idx);
        files
    }

    fn rebuild_derived(&mut self, selected_file: usize) {
        // Rebuild all_matches.
        self.all_matches.clear();
        let mut file_indices: Vec<usize> = self.file_results.keys().copied().collect();
        file_indices.sort_unstable();
        for idx in &file_indices {
            if let Some(matches) = self.file_results.get(idx) {
                self.all_matches.extend(matches.iter().cloned());
            }
        }

        // Clamp current_match.
        if self.all_matches.is_empty() || self.current_match >= self.all_matches.len() {
            self.current_match = 0;
        }

        // Rebuild match_index_map.
        self.match_index_map.clear();
        for (i, m) in self.all_matches.iter().enumerate() {
            self.match_index_map.insert(
                (
                    m.file_index,
                    m.data_row,
                    m.side,
                    m.byte_range.start,
                    m.byte_range.end,
                ),
                i,
            );
        }

        // Rebuild cached_files_with_matches.
        self.cached_files_with_matches = file_indices
            .iter()
            .filter_map(|&idx| {
                let matches = self.file_results.get(&idx)?;
                if matches.is_empty() {
                    None
                } else {
                    Some((idx, matches.len()))
                }
            })
            .collect();

        // Rebuild render_map.
        self.rebuild_render_map(selected_file);
    }

    /// Rebuild the cached display groups for the search sidebar.
    /// Call after `rebuild_derived()` when diff data and file pairs are available.
    pub fn rebuild_display_cache(
        &mut self,
        diff_cache: &HashMap<usize, FileDiffData>,
        file_pairs: &[crate::domain::file_pair::FilePair],
    ) {
        self.cached_groups.clear();
        for &(file_idx, match_count) in &self.cached_files_with_matches {
            let rel_path = file_pairs.get(file_idx).map_or_else(String::new, |fp| {
                fp.relative_path.to_string_lossy().to_string()
            });

            let mut rows = Vec::new();
            let mut first_match_idx = None;

            if let Some(matches) = self.file_results.get(&file_idx)
                && let Some(diff_data) = diff_cache.get(&file_idx)
            {
                for m in matches {
                    let (line_num, line_text) = match &diff_data.aligned_rows[m.data_row] {
                        AlignedRow::Both {
                            left_line,
                            right_line,
                            ..
                        } => match m.side {
                            MatchSide::Left => (
                                *left_line + 1,
                                Box::from(diff_data.old_lines.line(*left_line)),
                            ),
                            MatchSide::Right => (
                                *right_line + 1,
                                Box::from(diff_data.new_lines.line(*right_line)),
                            ),
                        },
                        AlignedRow::LeftOnly { left_line } => (
                            *left_line + 1,
                            Box::from(diff_data.old_lines.line(*left_line)),
                        ),
                        AlignedRow::RightOnly { right_line } => (
                            *right_line + 1,
                            Box::from(diff_data.new_lines.line(*right_line)),
                        ),
                    };

                    let match_key = (
                        m.file_index,
                        m.data_row,
                        m.side,
                        m.byte_range.start,
                        m.byte_range.end,
                    );
                    let match_idx = self.match_index_map.get(&match_key).copied().unwrap_or(0);

                    if first_match_idx.is_none() {
                        first_match_idx = Some(match_idx);
                    }

                    rows.push(SearchResultRow {
                        match_idx,
                        line_num,
                        line_text,
                        byte_range: m.byte_range.clone(),
                    });
                }
            }

            self.cached_groups.push(SearchFileGroup {
                file_idx,
                rel_path,
                match_count,
                first_match_idx,
                rows,
            });
        }
    }
}

/// Lightweight snapshot of the data needed for search (avoids cloning styled spans, fold state, etc.).
pub struct SearchableFileData {
    pub aligned_rows: std::sync::Arc<Vec<AlignedRow>>,
    pub old_lines: std::sync::Arc<crate::app::LineIndex>,
    pub new_lines: std::sync::Arc<crate::app::LineIndex>,
    pub skip: bool,
}

impl SearchableFileData {
    /// Snapshot a file for background searching. Content and rows are
    /// Arc-shared with `FileDiffData`, so this is refcount bumps, not copies.
    pub fn from_diff_data(data: &FileDiffData) -> Self {
        Self {
            aligned_rows: std::sync::Arc::clone(&data.aligned_rows),
            old_lines: std::sync::Arc::clone(&data.old_lines),
            new_lines: std::sync::Arc::clone(&data.new_lines),
            skip: data.too_large_message.is_some() || data.binary,
        }
    }
}

/// Compute all search matches for a single file.
pub fn compute_file_matches(
    file_index: usize,
    data: &SearchableFileData,
    query: &str,
) -> Vec<SearchMatch> {
    if query.is_empty() || data.skip {
        return Vec::new();
    }

    let query_lower: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    // All-ASCII queries take a byte-window fast path on ASCII lines.
    let query_ascii: Option<Vec<u8>> = query_lower
        .iter()
        .all(char::is_ascii)
        .then(|| query_lower.iter().map(|&c| c as u8).collect());
    let query = PreparedQuery {
        lower: &query_lower,
        ascii: query_ascii.as_deref(),
    };
    let mut scratch = LineScratch::default();
    let mut matches = Vec::new();

    for (data_row, row) in data.aligned_rows.iter().enumerate() {
        match row {
            AlignedRow::Both {
                left_line,
                right_line,
                ..
            } => {
                find_in_line(
                    data.old_lines.line(*left_line),
                    query,
                    &mut scratch,
                    file_index,
                    data_row,
                    MatchSide::Left,
                    &mut matches,
                );
                find_in_line(
                    data.new_lines.line(*right_line),
                    query,
                    &mut scratch,
                    file_index,
                    data_row,
                    MatchSide::Right,
                    &mut matches,
                );
            }
            AlignedRow::LeftOnly { left_line } => {
                find_in_line(
                    data.old_lines.line(*left_line),
                    query,
                    &mut scratch,
                    file_index,
                    data_row,
                    MatchSide::Left,
                    &mut matches,
                );
            }
            AlignedRow::RightOnly { right_line } => {
                find_in_line(
                    data.new_lines.line(*right_line),
                    query,
                    &mut scratch,
                    file_index,
                    data_row,
                    MatchSide::Right,
                    &mut matches,
                );
            }
        }
    }

    matches
}

/// A lowercased query, with a byte form when it is all-ASCII.
#[derive(Clone, Copy)]
struct PreparedQuery<'a> {
    lower: &'a [char],
    ascii: Option<&'a [u8]>,
}

/// Reusable buffers for the char-based fallback path, avoiding per-line
/// allocations when searching non-ASCII content.
#[derive(Default)]
struct LineScratch {
    chars: Vec<(usize, char)>,
    lower: Vec<char>,
}

/// Case-insensitive substring search.
///
/// ASCII lines with ASCII queries are matched on byte windows without any
/// allocation; ASCII bytes never occur inside multi-byte UTF-8 sequences, so
/// the resulting offsets are valid char boundaries. Everything else falls
/// back to char-based iteration, which produces correct byte ranges even for
/// multi-byte UTF-8 characters.
fn find_in_line(
    line: &str,
    query: PreparedQuery<'_>,
    scratch: &mut LineScratch,
    file_index: usize,
    data_row: usize,
    side: MatchSide,
    out: &mut Vec<SearchMatch>,
) {
    let query_lower = query.lower;
    if query_lower.is_empty() {
        return;
    }

    if let Some(q) = query.ascii
        && line.is_ascii()
    {
        let bytes = line.as_bytes();
        if bytes.len() < q.len() {
            return;
        }
        for i in 0..=bytes.len() - q.len() {
            if bytes[i..i + q.len()].eq_ignore_ascii_case(q) {
                out.push(SearchMatch {
                    file_index,
                    data_row,
                    side,
                    byte_range: i..i + q.len(),
                });
            }
        }
        return;
    }

    scratch.chars.clear();
    scratch.chars.extend(line.char_indices());
    if scratch.chars.len() < query_lower.len() {
        return;
    }

    scratch.lower.clear();
    scratch.lower.extend(scratch.chars.iter().map(|&(_, c)| {
        let mut lower = c.to_lowercase();
        // For most characters, to_lowercase yields exactly one char.
        // For the rare multi-char case (e.g., 'İ' -> 'i̇'), take just the first.
        lower.next().unwrap_or(c)
    }));

    let line_chars = &scratch.chars;
    let line_lower = &scratch.lower;
    let end = line_chars.len() - query_lower.len() + 1;

    for i in 0..end {
        if line_lower[i..i + query_lower.len()] == *query_lower {
            let byte_start = line_chars[i].0;
            let byte_end = if i + query_lower.len() < line_chars.len() {
                line_chars[i + query_lower.len()].0
            } else {
                line.len()
            };
            out.push(SearchMatch {
                file_index,
                data_row,
                side,
                byte_range: byte_start..byte_end,
            });
        }
    }
}

/// Convert a byte offset in a string to a char offset.
/// Used for `Galley::pos_from_cursor` which takes `CCursor` (char-based).
pub fn byte_offset_to_char_offset(s: &str, byte_offset: usize) -> usize {
    s[..byte_offset].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{FileDiffData, LineIndex};
    use crate::domain::fold::FoldState;
    use crate::domain::hunk::AlignedRow;

    #[allow(clippy::needless_pass_by_value)]
    fn make_diff_data(old: Vec<&str>, new: Vec<&str>, rows: Vec<AlignedRow>) -> FileDiffData {
        let n = rows.len();
        FileDiffData {
            old_lines: std::sync::Arc::new(LineIndex::new(old.join("\n"))),
            new_lines: std::sync::Arc::new(LineIndex::new(new.join("\n"))),
            aligned_rows: std::sync::Arc::new(rows),
            hunks: Vec::new(),
            left_styled: vec![Vec::new(); n],
            right_styled: vec![Vec::new(); n],
            too_large_message: None,
            binary: false,
            fold_state: FoldState::new(n, &[], 3, 20, 2),
        }
    }

    #[test]
    fn test_basic_search() {
        let data = make_diff_data(
            vec!["hello world", "foo bar"],
            vec!["hello world", "baz qux"],
            vec![
                AlignedRow::Both {
                    left_line: 0,
                    right_line: 0,
                    modified: false,
                },
                AlignedRow::Both {
                    left_line: 1,
                    right_line: 1,
                    modified: true,
                },
            ],
        );

        let matches = compute_file_matches(0, &SearchableFileData::from_diff_data(&data), "hello");
        // "hello" appears in row 0, both left and right (same content).
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].data_row, 0);
        assert_eq!(matches[0].side, MatchSide::Left);
        assert_eq!(matches[0].byte_range, 0..5);
        assert_eq!(matches[1].side, MatchSide::Right);
    }

    #[test]
    fn test_case_insensitive() {
        let data = make_diff_data(
            vec!["Hello WORLD"],
            vec!["hello world"],
            vec![AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: true,
            }],
        );

        let matches =
            compute_file_matches(0, &SearchableFileData::from_diff_data(&data), "hello world");
        assert_eq!(matches.len(), 2); // Both sides match.
    }

    #[test]
    fn test_left_only_right_only() {
        let data = make_diff_data(
            vec!["removed line"],
            vec!["added line"],
            vec![
                AlignedRow::LeftOnly { left_line: 0 },
                AlignedRow::RightOnly { right_line: 0 },
            ],
        );

        let matches = compute_file_matches(0, &SearchableFileData::from_diff_data(&data), "line");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].side, MatchSide::Left);
        assert_eq!(matches[0].data_row, 0);
        assert_eq!(matches[1].side, MatchSide::Right);
        assert_eq!(matches[1].data_row, 1);
    }

    #[test]
    fn test_empty_query() {
        let data = make_diff_data(
            vec!["hello"],
            vec!["hello"],
            vec![AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: false,
            }],
        );

        let matches = compute_file_matches(0, &SearchableFileData::from_diff_data(&data), "");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_no_matches() {
        let data = make_diff_data(
            vec!["hello"],
            vec!["world"],
            vec![AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: true,
            }],
        );

        let matches = compute_file_matches(0, &SearchableFileData::from_diff_data(&data), "xyz");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_multiple_matches_same_line() {
        let data = make_diff_data(
            vec!["abcabc"],
            vec!["defdef"],
            vec![AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: true,
            }],
        );

        let matches = compute_file_matches(0, &SearchableFileData::from_diff_data(&data), "abc");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].byte_range, 0..3);
        assert_eq!(matches[1].byte_range, 3..6);
    }

    #[test]
    fn test_unicode_byte_offsets() {
        // "café" has multi-byte é (2 bytes in UTF-8).
        let data = make_diff_data(
            vec!["café latte"],
            vec!["café latte"],
            vec![AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: false,
            }],
        );

        let matches = compute_file_matches(0, &SearchableFileData::from_diff_data(&data), "latte");
        assert_eq!(matches.len(), 2);
        // "café " is 6 bytes (c=1, a=1, f=1, é=2, space=1), so "latte" starts at byte 6.
        assert_eq!(matches[0].byte_range, 6..11);
    }

    #[test]
    fn test_too_large_file() {
        let mut data = make_diff_data(
            vec!["hello"],
            vec!["hello"],
            vec![AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: false,
            }],
        );
        data.too_large_message = Some("Too large".into());

        let matches = compute_file_matches(0, &SearchableFileData::from_diff_data(&data), "hello");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_byte_to_char_offset() {
        assert_eq!(byte_offset_to_char_offset("hello", 0), 0);
        assert_eq!(byte_offset_to_char_offset("hello", 5), 5);
        // "café" = c(1) a(1) f(1) é(2) = 5 bytes, 4 chars
        assert_eq!(byte_offset_to_char_offset("café", 5), 4);
        // Byte offset 3 is start of é, which is char index 3.
        assert_eq!(byte_offset_to_char_offset("café", 3), 3);
    }

    #[test]
    fn test_search_state_navigation() {
        let mut state = SearchState {
            query: "test".into(),
            ..Default::default()
        };

        // Create mock data for two files.
        let mut diff_cache = HashMap::new();
        diff_cache.insert(
            0,
            make_diff_data(
                vec!["test line"],
                vec!["test line"],
                vec![AlignedRow::Both {
                    left_line: 0,
                    right_line: 0,
                    modified: false,
                }],
            ),
        );
        diff_cache.insert(
            1,
            make_diff_data(
                vec!["another test"],
                vec!["another test"],
                vec![AlignedRow::Both {
                    left_line: 0,
                    right_line: 0,
                    modified: false,
                }],
            ),
        );

        let excluded = vec![false, false];
        state.update_matches(&diff_cache, &excluded, 0);

        // Should have matches in both files.
        assert_eq!(state.total_matches(), 4); // 2 per file (left+right).
        assert_eq!(state.current_match, 0);

        // Navigate forward.
        let m = state.next_match().expect("should have match").clone();
        assert_eq!(state.current_match, 1);
        assert_eq!(m.file_index, 0);

        // Navigate to wrap-around.
        state.current_match = state.all_matches.len() - 1;
        let m = state.next_match().expect("should have match").clone();
        assert_eq!(state.current_match, 0);
        assert_eq!(m.file_index, 0);
    }

    #[test]
    fn test_excluded_files() {
        let mut state = SearchState {
            query: "test".into(),
            ..Default::default()
        };

        let mut diff_cache = HashMap::new();
        diff_cache.insert(
            0,
            make_diff_data(
                vec!["test"],
                vec!["test"],
                vec![AlignedRow::Both {
                    left_line: 0,
                    right_line: 0,
                    modified: false,
                }],
            ),
        );
        diff_cache.insert(
            1,
            make_diff_data(
                vec!["test"],
                vec!["test"],
                vec![AlignedRow::Both {
                    left_line: 0,
                    right_line: 0,
                    modified: false,
                }],
            ),
        );

        // Exclude file 1.
        let excluded = vec![false, true];
        state.update_matches(&diff_cache, &excluded, 0);

        assert_eq!(state.total_matches(), 2); // Only file 0.
        assert!(!state.file_results.contains_key(&1));
    }

    #[test]
    fn test_incremental_search() {
        let mut state = SearchState {
            query: "test".into(),
            ..Default::default()
        };

        let mut diff_cache = HashMap::new();
        diff_cache.insert(
            0,
            make_diff_data(
                vec!["test"],
                vec!["test"],
                vec![AlignedRow::Both {
                    left_line: 0,
                    right_line: 0,
                    modified: false,
                }],
            ),
        );

        let excluded = vec![false, false];
        state.update_matches(&diff_cache, &excluded, 0);
        assert_eq!(state.total_matches(), 2);

        // Add file 1 (background thread delivered it).
        diff_cache.insert(
            1,
            make_diff_data(
                vec!["test again"],
                vec!["test again"],
                vec![AlignedRow::Both {
                    left_line: 0,
                    right_line: 0,
                    modified: false,
                }],
            ),
        );
        state.update_matches(&diff_cache, &excluded, 0);
        assert_eq!(state.total_matches(), 4); // Now includes file 1.
    }

    #[test]
    fn test_files_with_matches() {
        let mut state = SearchState {
            query: "test".into(),
            ..Default::default()
        };

        let mut diff_cache = HashMap::new();
        diff_cache.insert(
            0,
            make_diff_data(
                vec!["test"],
                vec!["test"],
                vec![AlignedRow::Both {
                    left_line: 0,
                    right_line: 0,
                    modified: false,
                }],
            ),
        );
        diff_cache.insert(
            1,
            make_diff_data(
                vec!["no match here"],
                vec!["no match here"],
                vec![AlignedRow::Both {
                    left_line: 0,
                    right_line: 0,
                    modified: false,
                }],
            ),
        );

        let excluded = vec![false, false];
        state.update_matches(&diff_cache, &excluded, 0);

        let files = state.files_with_matches();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], (0, 2)); // File 0 has 2 matches.
    }

    /// Run `find_in_line` for a query, optionally suppressing the ASCII fast
    /// path to force the char-based fallback.
    fn line_matches(line: &str, query: &str, allow_ascii: bool) -> Vec<SearchMatch> {
        let lower: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
        let ascii: Option<Vec<u8>> = (allow_ascii && lower.iter().all(char::is_ascii))
            .then(|| lower.iter().map(|&c| c as u8).collect());
        let prepared = PreparedQuery {
            lower: &lower,
            ascii: ascii.as_deref(),
        };
        let mut out = Vec::new();
        let mut scratch = LineScratch::default();
        find_in_line(
            line,
            prepared,
            &mut scratch,
            0,
            0,
            MatchSide::Left,
            &mut out,
        );
        out
    }

    #[test]
    fn test_ascii_fast_path_matches_char_path() {
        let cases = [
            ("Config CONFIG config", "config"),
            ("aaaa", "aa"), // overlapping matches
            ("Fn fn FN", "fn"),
            ("no match here", "xyz"),
            ("f", "fn"), // line shorter than query
            ("edge", "edge"),
            ("Kelvin \u{212A} sign", "k"), // non-ASCII line → char path both ways
            ("größe Größe", "größe"),      // non-ASCII query → char path both ways
        ];
        for (line, query) in cases {
            assert_eq!(
                line_matches(line, query, true),
                line_matches(line, query, false),
                "fast path diverges for {line:?} / {query:?}"
            );
        }
    }

    #[test]
    fn test_ascii_fast_path_ranges() {
        let matches = line_matches("Config x config", "config", true);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].byte_range, 0..6);
        assert_eq!(matches[1].byte_range, 9..15);
    }
}
