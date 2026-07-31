use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The kind of change for a file pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeKind {
    /// File exists in both directories (content differs).
    Modified,
    /// File only exists in the left (old) directory.
    Deleted,
    /// File only exists in the right (new) directory.
    Added,
    /// File was renamed (and possibly modified). Similarity is 0–100%.
    Renamed { similarity: u8 },
}

impl fmt::Display for FileChangeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Modified => write!(f, "modified"),
            Self::Deleted => write!(f, "deleted"),
            Self::Added => write!(f, "added"),
            Self::Renamed { similarity } => write!(f, "renamed ({similarity}% similar)"),
        }
    }
}

/// A paired file entry with its relative path and change kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePair {
    /// Relative path from the root directory (e.g. `src/main.rs`).
    /// For renames, this is the **new** path.
    pub relative_path: PathBuf,
    /// Original relative path before rename (only set for `Renamed` files).
    pub old_relative_path: Option<PathBuf>,
    pub kind: FileChangeKind,
    /// Absolute path to the left (old) file, if it exists.
    pub left_path: Option<PathBuf>,
    /// Absolute path to the right (new) file, if it exists.
    pub right_path: Option<PathBuf>,
    /// Unix permission mode of the old file (e.g. `0o100644`).
    pub left_mode: Option<u32>,
    /// Unix permission mode of the new file (e.g. `0o100755`).
    pub right_mode: Option<u32>,
}

impl FilePair {
    /// Format the mode change for display (e.g. "100644 → 100755").
    /// Returns `None` if there is no mode change.
    /// Return old and new modes if they differ.
    pub fn mode_change(&self) -> Option<(u32, u32)> {
        if let (Some(a), Some(b)) = (self.left_mode, self.right_mode)
            && a != b
        {
            return Some((a, b));
        }
        None
    }
}

/// Format a 3-bit octal permission value as rwx string (e.g. `0o7` → `"rwx"`).
pub fn format_rwx(bits: u32) -> String {
    let r = if bits & 4 != 0 { 'r' } else { '-' };
    let w = if bits & 2 != 0 { 'w' } else { '-' };
    let x = if bits & 1 != 0 { 'x' } else { '-' };
    format!("{r}{w}{x}")
}

/// Read the Unix permission bits for a file (e.g. `0o755`), excluding file type bits.
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o7777)
}

/// Walk two directory trees and pair files by relative path.
///
/// Symlinks are resolved when reading content, but the walk itself follows symlinks
/// to find all files. Files present in both directories with identical content are
/// skipped (they are unchanged).
pub fn walk_and_pair(
    left_dir: &Path,
    right_dir: &Path,
    filter_unchanged: bool,
) -> io::Result<Vec<FilePair>> {
    let left_files = collect_relative_paths(left_dir)?;
    let right_files = collect_relative_paths(right_dir)?;

    let all_paths: BTreeSet<&PathBuf> = left_files.iter().chain(right_files.iter()).collect();

    let mut pairs = Vec::new();

    for rel_path in all_paths {
        let in_left = left_files.contains(rel_path);
        let in_right = right_files.contains(rel_path);

        let (kind, left_path, right_path, left_mode, right_mode) = match (in_left, in_right) {
            (true, true) => {
                let lp = left_dir.join(rel_path);
                let rp = right_dir.join(rel_path);
                if filter_unchanged {
                    let (identical, lm, rm) = compare_files(&lp, &rp)?;
                    if identical && lm == rm {
                        continue;
                    }
                    (FileChangeKind::Modified, Some(lp), Some(rp), lm, rm)
                } else {
                    let lm = file_mode(&lp);
                    let rm = file_mode(&rp);
                    (FileChangeKind::Modified, Some(lp), Some(rp), lm, rm)
                }
            }
            (true, false) => {
                let lp = left_dir.join(rel_path);
                let lm = file_mode(&lp);
                (FileChangeKind::Deleted, Some(lp), None, lm, None)
            }
            (false, true) => {
                let rp = right_dir.join(rel_path);
                let rm = file_mode(&rp);
                (FileChangeKind::Added, None, Some(rp), None, rm)
            }
            (false, false) => unreachable!(),
        };

        pairs.push(FilePair {
            relative_path: rel_path.clone(),
            old_relative_path: None,
            kind,
            left_path,
            right_path,
            left_mode,
            right_mode,
        });
    }

    detect_renames(&mut pairs, 50);

    Ok(pairs)
}

/// Maximum number of lines to consider for rename similarity computation.
/// Files larger than this are skipped to avoid expensive diff operations.
const RENAME_MAX_LINES: usize = 10_000;

/// Number of full line diffs run by rename detection (bench instrumentation).
#[cfg(feature = "dev-tools")]
pub static RENAME_DIFFS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Detect renames among Deleted/Added pairs by content similarity.
///
/// Exact matches are resolved first via a content map in O(D + A). Remaining
/// candidate pairs are pruned with `similarity_upper_bound` (lossless: derived
/// from line counts alone), scored with a full line diff in parallel, then
/// greedily assigned from highest similarity downward. Binary files (non-UTF-8)
/// and files exceeding `RENAME_MAX_LINES` are skipped.
fn detect_renames(pairs: &mut Vec<FilePair>, threshold: u8) {
    use rayon::prelude::*;

    // Collect indices of deleted and added files.
    let deleted_indices: Vec<usize> = pairs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.kind == FileChangeKind::Deleted)
        .map(|(i, _)| i)
        .collect();
    let added_indices: Vec<usize> = pairs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.kind == FileChangeKind::Added)
        .map(|(i, _)| i)
        .collect();

    if deleted_indices.is_empty() || added_indices.is_empty() {
        return;
    }

    // Read file contents using stored absolute paths. Binary files yield None.
    let read_content = |pair: &FilePair, is_left: bool| -> Option<(String, usize)> {
        let path = if is_left {
            pair.left_path.as_ref()?
        } else {
            pair.right_path.as_ref()?
        };
        let text = fs::read_to_string(path).ok()?;
        let lines = text.lines().count();
        // Skip files that are too large for efficient diffing.
        if lines > RENAME_MAX_LINES {
            return None;
        }
        Some((text, lines))
    };

    let deleted_contents: Vec<Option<(String, usize)>> = deleted_indices
        .iter()
        .map(|&i| read_content(&pairs[i], true))
        .collect();
    let added_contents: Vec<Option<(String, usize)>> = added_indices
        .iter()
        .map(|&i| read_content(&pairs[i], false))
        .collect();

    let mut matched_del = vec![false; deleted_indices.len()];
    let mut matched_add = vec![false; added_indices.len()];
    let mut renames: Vec<(usize, usize, u8)> = Vec::new();

    // Exact-content fast path: matches identical files without any diffing.
    let mut by_content: HashMap<&str, Vec<usize>> = HashMap::new();
    for (di, content) in deleted_contents.iter().enumerate() {
        if let Some((text, _)) = content {
            by_content.entry(text.as_str()).or_default().push(di);
        }
    }
    for (ai, content) in added_contents.iter().enumerate() {
        let Some((text, _)) = content else { continue };
        if let Some(unmatched) = by_content.get_mut(text.as_str())
            && let Some(di) = unmatched.pop()
        {
            matched_del[di] = true;
            matched_add[ai] = true;
            renames.push((deleted_indices[di], added_indices[ai], 100));
        }
    }

    // Pairwise scoring for the rest, skipping pairs whose similarity is
    // provably below the threshold: first by line counts alone (O(1)), then
    // by line-hash multiset intersection (O(a + b), vs a full diff).
    let hashes_for = |contents: &[Option<(String, usize)>], matched: &[bool]| {
        contents
            .iter()
            .enumerate()
            .map(|(i, c)| match c {
                Some((text, _)) if !matched[i] => Some(line_hashes(text)),
                _ => None,
            })
            .collect::<Vec<Option<Vec<u64>>>>()
    };
    let deleted_hashes = hashes_for(&deleted_contents, &matched_del);
    let added_hashes = hashes_for(&added_contents, &matched_add);

    let mut candidate_pairs: Vec<(usize, usize)> = Vec::new();
    for (di, del) in deleted_hashes.iter().enumerate() {
        let Some(del_hashes) = del else { continue };
        for (ai, add) in added_hashes.iter().enumerate() {
            let Some(add_hashes) = add else { continue };
            if similarity_upper_bound(del_hashes.len(), add_hashes.len()) < threshold {
                continue;
            }
            let common = sorted_intersection_count(del_hashes, add_hashes);
            if similarity_from_matches(common, del_hashes.len() + add_hashes.len()) >= threshold {
                candidate_pairs.push((di, ai));
            }
        }
    }

    let mut candidates: Vec<(usize, usize, u8)> = candidate_pairs
        .par_iter()
        .filter_map(|&(di, ai)| {
            let (del_text, _) = deleted_contents[di].as_ref()?;
            let (add_text, _) = added_contents[ai].as_ref()?;
            #[cfg(feature = "dev-tools")]
            RENAME_DIFFS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let sim = line_similarity(del_text, add_text);
            if sim >= threshold {
                Some((di, ai, sim))
            } else {
                None
            }
        })
        .collect();

    // Sort by similarity descending for globally optimal greedy matching.
    candidates.sort_unstable_by_key(|b| std::cmp::Reverse(b.2));

    // Greedily assign matches: highest similarity wins.
    for (di, ai, sim) in candidates {
        if matched_del[di] || matched_add[ai] {
            continue;
        }
        matched_del[di] = true;
        matched_add[ai] = true;
        renames.push((deleted_indices[di], added_indices[ai], sim));
    }

    // Apply renames: merge deleted+added into renamed pairs.
    let mut remove_indices: Vec<usize> = Vec::new();

    for (del_idx, add_idx, similarity) in &renames {
        let old_path = pairs[*del_idx].relative_path.clone();
        let new_path = pairs[*add_idx].relative_path.clone();
        let left_path = pairs[*del_idx].left_path.clone();
        let right_path = pairs[*add_idx].right_path.clone();
        let left_mode = pairs[*del_idx].left_mode;
        let right_mode = pairs[*add_idx].right_mode;

        pairs[*del_idx] = FilePair {
            relative_path: new_path,
            old_relative_path: Some(old_path),
            kind: FileChangeKind::Renamed {
                similarity: *similarity,
            },
            left_path,
            right_path,
            left_mode,
            right_mode,
        };
        remove_indices.push(*add_idx);
    }

    // Remove the added entries that were merged.
    let remove: HashSet<usize> = remove_indices.into_iter().collect();
    let mut idx = 0;
    pairs.retain(|_| {
        let keep = !remove.contains(&idx);
        idx += 1;
        keep
    });
}

/// Upper bound on `line_similarity` from line counts alone:
/// equal lines cannot exceed the smaller file.
fn similarity_upper_bound(a_lines: usize, b_lines: usize) -> u8 {
    similarity_from_matches(a_lines.min(b_lines), a_lines + b_lines)
}

/// Sorted hashes of a text's lines, for cheap similarity upper bounds.
/// Hashing strips line endings; collisions and CRLF folding can only
/// overcount matches, so bounds derived from these stay upper bounds.
fn line_hashes(text: &str) -> Vec<u64> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hashes: Vec<u64> = text
        .lines()
        .map(|line| {
            let mut h = DefaultHasher::new();
            line.hash(&mut h);
            h.finish()
        })
        .collect();
    hashes.sort_unstable();
    hashes
}

/// Multiset intersection size of two sorted slices. Diff-equal lines cannot
/// exceed this, making it an upper bound on `line_similarity` matches.
fn sorted_intersection_count(a: &[u64], b: &[u64]) -> usize {
    let (mut i, mut j, mut count) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                count += 1;
                i += 1;
                j += 1;
            }
        }
    }
    count
}

/// Compute line-level similarity between two text contents as a percentage (0–100).
///
/// Uses `similar::TextDiff` to count matching lines, then computes
/// `2 * matches / (lines_a + lines_b) * 100`.
fn line_similarity(a: &str, b: &str) -> u8 {
    let total_lines = a.lines().count() + b.lines().count();

    let diff = similar::TextDiff::configure()
        .algorithm(similar::Algorithm::Myers)
        .diff_lines(a, b);

    let mut matches = 0usize;
    for change in diff.iter_all_changes() {
        if change.tag() == similar::ChangeTag::Equal {
            matches += 1;
        }
    }

    similarity_from_matches(matches, total_lines)
}

/// Similarity percentage from a matching-line count: `2 * matches * 100 / total`.
/// Both empty (total 0) counts as identical.
fn similarity_from_matches(matches: usize, total_lines: usize) -> u8 {
    if total_lines == 0 {
        return 100;
    }
    ((2 * matches * 100) / total_lines).min(100) as u8
}

/// Recursively collect all file paths relative to `root`.
fn collect_relative_paths(root: &Path) -> io::Result<BTreeSet<PathBuf>> {
    let mut result = BTreeSet::new();
    let mut visited_dirs = HashSet::new();
    // Track the root's canonical path to detect symlink loops.
    let canonical_root = fs::canonicalize(root)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", root.display())))?;
    visited_dirs.insert(canonical_root.clone());
    walk_dir_recursive(root, root, &mut result, &mut visited_dirs)?;
    Ok(result)
}

fn walk_dir_recursive(
    root: &Path,
    current: &Path,
    result: &mut BTreeSet<PathBuf>,
    visited_dirs: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            eprintln!("Warning: permission denied reading {}", current.display());
            return Ok(());
        }
        Err(e) => {
            return Err(io::Error::new(
                e.kind(),
                format!("{}: {e}", current.display()),
            ));
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Warning: error reading directory entry: {e}");
                continue;
            }
        };
        let path = entry.path();
        // Resolve symlinks for metadata check.
        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("Warning: permission denied reading {}", path.display());
                continue;
            }
            Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
        };

        if metadata.is_dir() {
            // Detect symlink loops: only canonicalize if entry is a symlink.
            let is_symlink = fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
            if is_symlink {
                let canonical = fs::canonicalize(&path)
                    .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
                if !visited_dirs.insert(canonical) {
                    continue; // symlink loop — skip
                }
            }
            walk_dir_recursive(root, &path, result, visited_dirs)?;
        } else if metadata.is_file() {
            let rel = path.strip_prefix(root).expect("path should be under root");
            result.insert(rel.to_path_buf());
        }
    }

    Ok(())
}

/// Compare two files: returns (content_identical, left_mode, right_mode).
fn compare_files(a: &Path, b: &Path) -> io::Result<(bool, Option<u32>, Option<u32>)> {
    use std::os::unix::fs::PermissionsExt;
    let meta_a =
        fs::metadata(a).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", a.display())))?;
    let meta_b =
        fs::metadata(b).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", b.display())))?;
    let mode_a = Some(meta_a.permissions().mode() & 0o7777);
    let mode_b = Some(meta_b.permissions().mode() & 0o7777);
    // Fast path: different sizes means different content.
    if meta_a.len() != meta_b.len() {
        return Ok((false, mode_a, mode_b));
    }
    let content_a =
        fs::read(a).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", a.display())))?;
    let content_b =
        fs::read(b).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", b.display())))?;
    Ok((content_a == content_b, mode_a, mode_b))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_dirs(
        left_files: &[(&str, &str)],
        right_files: &[(&str, &str)],
    ) -> (tempfile::TempDir, tempfile::TempDir) {
        let left_dir = tempfile::tempdir().unwrap();
        let right_dir = tempfile::tempdir().unwrap();

        for (name, content) in left_files {
            let path = left_dir.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }

        for (name, content) in right_files {
            let path = right_dir.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }

        (left_dir, right_dir)
    }

    #[test]
    fn test_identical_files_are_skipped() {
        let (left, right) =
            setup_dirs(&[("foo.rs", "fn main() {}")], &[("foo.rs", "fn main() {}")]);
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_identical_files_included_without_filter() {
        let (left, right) =
            setup_dirs(&[("foo.rs", "fn main() {}")], &[("foo.rs", "fn main() {}")]);
        let pairs = walk_and_pair(left.path(), right.path(), false).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].kind, FileChangeKind::Modified);
        assert_eq!(pairs[0].relative_path, PathBuf::from("foo.rs"));
    }

    #[test]
    fn test_modified_file() {
        let (left, right) = setup_dirs(
            &[("foo.rs", "fn main() { old() }")],
            &[("foo.rs", "fn main() { new() }")],
        );
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].kind, FileChangeKind::Modified);
        assert_eq!(pairs[0].relative_path, PathBuf::from("foo.rs"));
        assert!(pairs[0].left_path.is_some());
        assert!(pairs[0].right_path.is_some());
    }

    #[test]
    fn test_deleted_file() {
        let (left, right) = setup_dirs(&[("gone.rs", "content")], &[]);
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].kind, FileChangeKind::Deleted);
        assert!(pairs[0].left_path.is_some());
        assert!(pairs[0].right_path.is_none());
    }

    #[test]
    fn test_added_file() {
        let (left, right) = setup_dirs(&[], &[("new.rs", "content")]);
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].kind, FileChangeKind::Added);
        assert!(pairs[0].left_path.is_none());
        assert!(pairs[0].right_path.is_some());
    }

    #[test]
    fn test_nested_directories() {
        let (left, right) = setup_dirs(
            &[("src/lib.rs", "old"), ("src/util/helper.rs", "old")],
            &[("src/lib.rs", "new"), ("src/util/helper.rs", "new")],
        );
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 2);
        let paths: Vec<_> = pairs.iter().map(|p| p.relative_path.clone()).collect();
        assert!(paths.contains(&PathBuf::from("src/lib.rs")));
        assert!(paths.contains(&PathBuf::from("src/util/helper.rs")));
    }

    #[test]
    fn test_mixed_changes() {
        let (left, right) = setup_dirs(
            &[
                ("kept.rs", "same"),
                ("modified.rs", "old"),
                ("deleted.rs", "bye"),
            ],
            &[
                ("kept.rs", "same"),
                ("modified.rs", "new"),
                ("added.rs", "hello"),
            ],
        );
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 3); // modified, deleted, added — "kept" is skipped
        let kinds: Vec<_> = pairs.iter().map(|p| &p.kind).collect();
        assert!(kinds.contains(&&FileChangeKind::Modified));
        assert!(kinds.contains(&&FileChangeKind::Deleted));
        assert!(kinds.contains(&&FileChangeKind::Added));
    }

    #[test]
    fn test_empty_directories() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_symlink_resolved() {
        let (left, right) = setup_dirs(&[("foo.rs", "content")], &[]);
        // Create a symlink in right pointing to the left file
        let target = left.path().join("foo.rs");
        let link = right.path().join("foo.rs");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        // Both sides have identical content (symlink resolved), so should be empty
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_symlink_loop_does_not_hang() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.rs"), "content").unwrap();
        // Create a symlink loop: sub/loop -> parent dir
        std::os::unix::fs::symlink(dir.path(), sub.join("loop")).unwrap();

        let other = tempfile::tempdir().unwrap();
        // Should complete without infinite recursion
        let pairs = walk_and_pair(dir.path(), other.path(), true).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].relative_path, PathBuf::from("sub/file.rs"));
    }

    #[test]
    fn test_rename_identical_content() {
        // File moved from old.rs to new.rs with identical content → Renamed(100%).
        let (left, right) = setup_dirs(
            &[("old.rs", "fn main() {}\nfn foo() {}\nfn bar() {}\n")],
            &[("new.rs", "fn main() {}\nfn foo() {}\nfn bar() {}\n")],
        );
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 1);
        assert!(matches!(
            pairs[0].kind,
            FileChangeKind::Renamed { similarity: 100 }
        ));
        assert_eq!(pairs[0].relative_path, PathBuf::from("new.rs"));
        assert_eq!(pairs[0].old_relative_path, Some(PathBuf::from("old.rs")));
        assert!(pairs[0].left_path.is_some());
        assert!(pairs[0].right_path.is_some());
    }

    #[test]
    fn test_rename_with_modifications() {
        // File moved and partially modified — should still be detected as rename.
        let old_content = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n";
        let new_content =
            "line1\nline2\nchanged\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n";
        let (left, right) = setup_dirs(
            &[("src/old_name.rs", old_content)],
            &[("src/new_name.rs", new_content)],
        );
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 1);
        assert!(matches!(pairs[0].kind, FileChangeKind::Renamed { .. }));
        assert_eq!(pairs[0].relative_path, PathBuf::from("src/new_name.rs"));
        assert_eq!(
            pairs[0].old_relative_path,
            Some(PathBuf::from("src/old_name.rs"))
        );
    }

    #[test]
    fn test_no_rename_when_too_different() {
        // Completely different content — should remain as separate delete + add.
        let (left, right) = setup_dirs(
            &[(
                "old.rs",
                "completely different content here\nnothing alike\n",
            )],
            &[("new.rs", "brand new file\nwith unrelated stuff\n")],
        );
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 2);
        let kinds: Vec<_> = pairs.iter().map(|p| &p.kind).collect();
        assert!(kinds.contains(&&FileChangeKind::Deleted));
        assert!(kinds.contains(&&FileChangeKind::Added));
    }

    #[test]
    fn test_rename_best_match_wins() {
        // Two added files, one deleted file. The deleted file is most similar to one of them.
        let original = "fn main() {\n    println!(\"hello\");\n}\n";
        let close_match = "fn main() {\n    println!(\"hello world\");\n}\n";
        let far_match = "struct Foo {\n    bar: i32,\n}\n";
        let (left, right) = setup_dirs(
            &[("original.rs", original)],
            &[("close.rs", close_match), ("far.rs", far_match)],
        );
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        // Should have 2 entries: renamed (original→close) + added (far).
        assert_eq!(pairs.len(), 2);
        let renamed = pairs
            .iter()
            .find(|p| matches!(p.kind, FileChangeKind::Renamed { .. }));
        assert!(renamed.is_some(), "should detect a rename");
        let renamed = renamed.unwrap();
        assert_eq!(renamed.relative_path, PathBuf::from("close.rs"));
        assert_eq!(
            renamed.old_relative_path,
            Some(PathBuf::from("original.rs"))
        );
    }

    #[test]
    fn test_rename_does_not_affect_modified_files() {
        // A modified file + a rename should both be detected correctly.
        let (left, right) = setup_dirs(
            &[
                ("keep.rs", "old content"),
                ("moved.rs", "fn x() {}\nfn y() {}\n"),
            ],
            &[
                ("keep.rs", "new content"),
                ("renamed.rs", "fn x() {}\nfn y() {}\n"),
            ],
        );
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 2);
        let modified = pairs.iter().find(|p| p.kind == FileChangeKind::Modified);
        assert!(modified.is_some());
        assert_eq!(modified.unwrap().relative_path, PathBuf::from("keep.rs"));
        let renamed = pairs
            .iter()
            .find(|p| matches!(p.kind, FileChangeKind::Renamed { .. }));
        assert!(renamed.is_some());
        assert_eq!(renamed.unwrap().relative_path, PathBuf::from("renamed.rs"));
    }

    #[test]
    fn test_line_similarity_identical() {
        assert_eq!(line_similarity("a\nb\nc", "a\nb\nc"), 100);
    }

    #[test]
    fn test_line_similarity_empty() {
        assert_eq!(line_similarity("", ""), 100);
    }

    #[test]
    fn test_line_similarity_completely_different() {
        assert_eq!(line_similarity("a\nb\nc", "x\ny\nz"), 0);
    }

    #[test]
    fn test_similarity_upper_bound_edges() {
        assert_eq!(similarity_upper_bound(0, 0), 100);
        assert_eq!(similarity_upper_bound(10, 10), 100);
        assert_eq!(similarity_upper_bound(0, 5), 0);
        assert_eq!(similarity_upper_bound(1, 3), 50);
        assert_eq!(similarity_upper_bound(40, 250), 27);
    }

    #[test]
    fn test_similarity_upper_bounds_chain() {
        // count bound >= intersection bound >= actual similarity.
        let samples = [
            ("a\nb\nc", "a\nb\nc"),
            ("a\nb\nc", "x\ny\nz"),
            ("a\nb", "a\nb\nc\nd\ne\nf"),
            ("", "a\nb"),
            ("a\nb\nc\nd", "b\nc"),
            ("b\na\nc", "a\nb\nc"),
            ("a\na\na", "a\nx\na"),
            ("a\r\nb", "a\nb"),
        ];
        for (a, b) in samples {
            let (ha, hb) = (line_hashes(a), line_hashes(b));
            let count_bound = similarity_upper_bound(ha.len(), hb.len());
            let intersection_bound =
                similarity_from_matches(sorted_intersection_count(&ha, &hb), ha.len() + hb.len());
            let actual = line_similarity(a, b);
            assert!(
                count_bound >= intersection_bound && intersection_bound >= actual,
                "bound chain violated for {a:?} vs {b:?}: \
                 {count_bound} >= {intersection_bound} >= {actual}"
            );
        }
    }

    #[test]
    fn test_sorted_intersection_count_multiset() {
        assert_eq!(sorted_intersection_count(&[1, 1, 2, 3], &[1, 2, 2, 4]), 2);
        assert_eq!(sorted_intersection_count(&[], &[1, 2]), 0);
        assert_eq!(sorted_intersection_count(&[5, 5, 5], &[5, 5, 5]), 3);
    }

    #[test]
    fn test_rename_duplicate_identical_contents() {
        // Two identical deleted files, one identical added file: exactly one
        // rename; the other stays deleted.
        let content = "line one\nline two\nline three\n";
        let (left, right) = setup_dirs(
            &[("dup_a.txt", content), ("dup_b.txt", content)],
            &[("moved.txt", content)],
        );
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 2);
        let renamed: Vec<_> = pairs
            .iter()
            .filter(|p| matches!(p.kind, FileChangeKind::Renamed { .. }))
            .collect();
        assert_eq!(renamed.len(), 1);
        assert_eq!(renamed[0].kind, FileChangeKind::Renamed { similarity: 100 });
        assert_eq!(renamed[0].relative_path, PathBuf::from("moved.txt"));
        assert_eq!(
            pairs
                .iter()
                .filter(|p| p.kind == FileChangeKind::Deleted)
                .count(),
            1
        );
    }

    #[test]
    fn test_rename_size_mismatch_not_matched() {
        // The small deleted file's lines all appear in the large added file,
        // but line counts alone cap similarity far below the threshold.
        let small = "a\nb\n";
        let mut large = String::from("a\nb\n");
        for i in 0..40 {
            use std::fmt::Write;
            let _ = writeln!(large, "l{i}");
        }
        let (left, right) = setup_dirs(&[("small.txt", small)], &[("large.txt", &large)]);
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(
            pairs
                .iter()
                .all(|p| !matches!(p.kind, FileChangeKind::Renamed { .. }))
        );
    }

    #[test]
    fn test_rename_multiple_competing() {
        // D1 and D2 both deleted; A1 and A2 both added.
        // D1 is 90% similar to A1 and 60% to A2.
        // D2 is 80% similar to A1 and 70% to A2.
        // Global best: D1↔A1 (90%), D2↔A2 (70%).
        let base = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n";
        let d1 = base; // identical to base
        let d2 = "l1\nl2\nX\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n"; // 1 line changed
        let a1 = "l1\nY\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n"; // 1 line changed from base
        let a2 = "l1\nl2\nl3\nZ\nl5\nW\nl7\nl8\nl9\nl10\n"; // 2 lines changed from d2
        let (left, right) = setup_dirs(
            &[("d1.rs", d1), ("d2.rs", d2)],
            &[("a1.rs", a1), ("a2.rs", a2)],
        );
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 2);
        // Both should be renames.
        assert!(
            pairs
                .iter()
                .all(|p| matches!(p.kind, FileChangeKind::Renamed { .. }))
        );
        // D1 should match A1 (highest similarity).
        let d1_rename = pairs
            .iter()
            .find(|p| p.old_relative_path.as_deref() == Some(Path::new("d1.rs")));
        assert!(d1_rename.is_some());
        assert_eq!(d1_rename.unwrap().relative_path, PathBuf::from("a1.rs"));
    }

    #[test]
    fn test_rename_binary_files_not_matched() {
        // Binary content in left, text in right — should not be matched as rename.
        let (left, right) = setup_dirs(&[], &[("new.rs", "fn main() {}")]);
        // Write binary content to left.
        let bin_path = left.path().join("old.bin");
        fs::write(&bin_path, [0x00, 0x01, 0xFF, 0xFE, 0x89, 0x50]).unwrap();
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(pairs.iter().any(|p| p.kind == FileChangeKind::Deleted));
        assert!(pairs.iter().any(|p| p.kind == FileChangeKind::Added));
    }

    #[test]
    fn test_rename_empty_files_match() {
        // Two empty files: deleted empty + added empty → rename at 100%.
        let (left, right) = setup_dirs(&[("old_empty.rs", "")], &[("new_empty.rs", "")]);
        let pairs = walk_and_pair(left.path(), right.path(), true).unwrap();
        assert_eq!(pairs.len(), 1);
        assert!(matches!(
            pairs[0].kind,
            FileChangeKind::Renamed { similarity: 100 }
        ));
    }
}
