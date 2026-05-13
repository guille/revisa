use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Tracks which files have been marked as "reviewed" by the user.
#[derive(Debug)]
pub struct ReviewState {
    /// Map from relative path to reviewed status.
    reviewed: BTreeMap<PathBuf, bool>,
}

impl ReviewState {
    /// Create a new review state with all files marked as unreviewed.
    pub fn new(files: Vec<PathBuf>) -> Self {
        let reviewed = files.into_iter().map(|p| (p, false)).collect();
        Self { reviewed }
    }

    /// Check if a file is marked as reviewed.
    pub fn is_reviewed(&self, path: &Path) -> bool {
        self.reviewed.get(path).copied().unwrap_or(false)
    }

    /// Toggle the reviewed state of a file. Returns the new state.
    pub fn toggle(&mut self, path: &Path) -> bool {
        if let Some(val) = self.reviewed.get_mut(path) {
            *val = !*val;
            *val
        } else {
            false
        }
    }

    /// Mark a file as reviewed.
    pub fn mark_reviewed(&mut self, path: &Path) {
        if let Some(val) = self.reviewed.get_mut(path) {
            *val = true;
        }
    }

    /// How many files have been reviewed.
    #[cfg(test)]
    pub fn reviewed_count(&self) -> usize {
        self.reviewed_count_excluding(&[])
    }

    /// Total number of tracked files.
    #[cfg(test)]
    pub fn total_count(&self) -> usize {
        self.total_count_excluding(&[])
    }

    /// Return the next unreviewed file after `current`, wrapping around.
    /// Returns `None` if all files are reviewed.
    #[cfg(test)]
    pub fn next_unreviewed_after(&self, current: &Path) -> Option<&PathBuf> {
        self.next_unreviewed_after_excluding(current, &[])
    }

    /// Like `next_unreviewed_after` but skips files under excluded directories.
    pub fn next_unreviewed_after_excluding(
        &self,
        current: &Path,
        excluded: &[PathBuf],
    ) -> Option<&PathBuf> {
        let keys: Vec<_> = self.reviewed.keys().collect();
        let current_idx = keys.iter().position(|k| k.as_path() == current);

        let start = current_idx.map_or(0, |i| i + 1);
        let len = keys.len();

        for offset in 0..len {
            let idx = (start + offset) % len;
            let key = keys[idx];
            if excluded.iter().any(|dir| key.starts_with(dir)) {
                continue;
            }
            if !self.reviewed.get(key).copied().unwrap_or(true) {
                return Some(key);
            }
        }

        None
    }

    /// Reviewed count excluding files under given directories.
    pub fn reviewed_count_excluding(&self, excluded: &[PathBuf]) -> usize {
        self.reviewed
            .iter()
            .filter(|(k, v)| **v && !excluded.iter().any(|dir| k.starts_with(dir)))
            .count()
    }

    /// Total count excluding files under given directories.
    pub fn total_count_excluding(&self, excluded: &[PathBuf]) -> usize {
        self.reviewed
            .keys()
            .filter(|k| !excluded.iter().any(|dir| k.starts_with(dir)))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn test_initial_state_all_unreviewed() {
        let state = ReviewState::new(paths(&["a.rs", "b.rs", "c.rs"]));
        assert!(!state.is_reviewed(Path::new("a.rs")));
        assert!(!state.is_reviewed(Path::new("b.rs")));
        assert_eq!(state.reviewed_count(), 0);
        assert_eq!(state.total_count(), 3);
    }

    #[test]
    fn test_toggle() {
        let mut state = ReviewState::new(paths(&["a.rs"]));
        assert!(!state.is_reviewed(Path::new("a.rs")));

        let new = state.toggle(Path::new("a.rs"));
        assert!(new);
        assert!(state.is_reviewed(Path::new("a.rs")));

        let new = state.toggle(Path::new("a.rs"));
        assert!(!new);
        assert!(!state.is_reviewed(Path::new("a.rs")));
    }

    #[test]
    fn test_mark_reviewed() {
        let mut state = ReviewState::new(paths(&["a.rs"]));
        state.mark_reviewed(Path::new("a.rs"));
        assert!(state.is_reviewed(Path::new("a.rs")));
        assert_eq!(state.reviewed_count(), 1);
    }

    #[test]
    fn test_next_unreviewed_after() {
        let mut state = ReviewState::new(paths(&["a.rs", "b.rs", "c.rs"]));
        state.mark_reviewed(Path::new("a.rs"));

        let next = state.next_unreviewed_after(Path::new("a.rs"));
        assert_eq!(next, Some(&PathBuf::from("b.rs")));
    }

    #[test]
    fn test_next_unreviewed_wraps_around() {
        let mut state = ReviewState::new(paths(&["a.rs", "b.rs", "c.rs"]));
        state.mark_reviewed(Path::new("b.rs"));
        state.mark_reviewed(Path::new("c.rs"));

        let next = state.next_unreviewed_after(Path::new("b.rs"));
        assert_eq!(next, Some(&PathBuf::from("a.rs")));
    }

    #[test]
    fn test_next_unreviewed_all_reviewed() {
        let mut state = ReviewState::new(paths(&["a.rs", "b.rs"]));
        state.mark_reviewed(Path::new("a.rs"));
        state.mark_reviewed(Path::new("b.rs"));

        assert_eq!(state.next_unreviewed_after(Path::new("a.rs")), None);
    }

    #[test]
    fn test_unknown_path() {
        let state = ReviewState::new(paths(&["a.rs"]));
        assert!(!state.is_reviewed(Path::new("unknown.rs")));
    }

    #[test]
    fn test_reviewed_count_excluding() {
        let mut state = ReviewState::new(paths(&["src/a.rs", "src/b.rs", "tests/c.rs"]));
        state.mark_reviewed(Path::new("src/a.rs"));
        state.mark_reviewed(Path::new("tests/c.rs"));

        let excluded = vec![PathBuf::from("src")];

        assert_eq!(state.reviewed_count_excluding(&excluded), 1); // only tests/c.rs
        assert_eq!(state.total_count_excluding(&excluded), 1); // only tests/c.rs
    }

    #[test]
    fn test_next_unreviewed_after_excluding() {
        let mut state = ReviewState::new(paths(&["src/a.rs", "b.rs", "src/c.rs"]));
        state.mark_reviewed(Path::new("src/a.rs"));

        let excluded = vec![PathBuf::from("src")];

        // Should skip src/c.rs and return b.rs
        let next = state.next_unreviewed_after_excluding(Path::new("src/a.rs"), &excluded);
        assert_eq!(next, Some(&PathBuf::from("b.rs")));
    }

    #[test]
    fn test_next_unreviewed_excluding_all_excluded() {
        let state = ReviewState::new(paths(&["src/a.rs", "src/b.rs"]));

        let excluded = vec![PathBuf::from("src")];

        assert_eq!(
            state.next_unreviewed_after_excluding(Path::new("src/a.rs"), &excluded),
            None
        );
    }

    #[test]
    fn test_next_unreviewed_excluding_wraps_around() {
        // Files: a.rs (reviewed), src/b.rs (excluded), c.rs (unreviewed).
        // Starting from c.rs, wrapping should find... nothing after c since c itself is unreviewed
        // but we start from c+1. Let's start from a.rs instead.
        let mut state = ReviewState::new(paths(&["a.rs", "src/b.rs", "c.rs"]));
        state.mark_reviewed(Path::new("a.rs"));

        let excluded = vec![PathBuf::from("src")];

        // From a.rs: next is src/b.rs (excluded), then c.rs (unreviewed) → found.
        assert_eq!(
            state.next_unreviewed_after_excluding(Path::new("a.rs"), &excluded),
            Some(&PathBuf::from("c.rs"))
        );
    }

    #[test]
    fn test_next_unreviewed_excluding_skips_to_wrap() {
        // All files after current are excluded, must wrap to find one before.
        let mut state = ReviewState::new(paths(&["a.rs", "b.rs", "vendor/c.rs"]));
        state.mark_reviewed(Path::new("b.rs"));

        let excluded = vec![PathBuf::from("vendor")];

        // From b.rs: next is vendor/c.rs (excluded), wrap → a.rs (unreviewed) → found.
        assert_eq!(
            state.next_unreviewed_after_excluding(Path::new("b.rs"), &excluded),
            Some(&PathBuf::from("a.rs"))
        );
    }
}
