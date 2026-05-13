use std::collections::{BTreeSet, HashSet};
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

/// Detect renames among Deleted/Added pairs by content similarity.
///
/// Computes a similarity score for every (deleted, added) pair, then greedily
/// assigns matches from highest similarity downward. Uses rayon for parallel
/// similarity computation. Binary files (non-UTF-8) and files exceeding
/// `RENAME_MAX_LINES` are skipped.
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
    let read_content = |pair: &FilePair, is_left: bool| -> Option<String> {
        let path = if is_left {
            pair.left_path.as_ref()?
        } else {
            pair.right_path.as_ref()?
        };
        let text = fs::read_to_string(path).ok()?;
        // Skip files that are too large for efficient diffing.
        if text.lines().count() > RENAME_MAX_LINES {
            return None;
        }
        Some(text)
    };

    let deleted_contents: Vec<Option<String>> = deleted_indices
        .iter()
        .map(|&i| read_content(&pairs[i], true))
        .collect();
    let added_contents: Vec<Option<String>> = added_indices
        .iter()
        .map(|&i| read_content(&pairs[i], false))
        .collect();

    // Build all candidate pairs and compute similarity in parallel.
    let candidate_pairs: Vec<(usize, usize)> = (0..deleted_indices.len())
        .flat_map(|di| (0..added_indices.len()).map(move |ai| (di, ai)))
        .collect();

    let mut candidates: Vec<(usize, usize, u8)> = candidate_pairs
        .par_iter()
        .filter_map(|&(di, ai)| {
            let del_text = deleted_contents[di].as_deref()?;
            let add_text = added_contents[ai].as_deref()?;
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
    let mut matched_del = vec![false; deleted_indices.len()];
    let mut matched_add = vec![false; added_indices.len()];
    let mut renames: Vec<(usize, usize, u8)> = Vec::new();

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

    // Remove the added entries that were merged (in reverse order to preserve indices).
    remove_indices.sort_unstable();
    for idx in remove_indices.into_iter().rev() {
        pairs.remove(idx);
    }
}

/// Compute line-level similarity between two text contents as a percentage (0–100).
///
/// Uses `similar::TextDiff` to count matching lines, then computes
/// `2 * matches / (lines_a + lines_b) * 100`.
fn line_similarity(a: &str, b: &str) -> u8 {
    let a_lines = a.lines().count();
    let b_lines = b.lines().count();
    let max_lines = a_lines + b_lines;
    if max_lines == 0 {
        return 100; // both empty = identical
    }

    let diff = similar::TextDiff::configure()
        .algorithm(similar::Algorithm::Myers)
        .diff_lines(a, b);

    let mut matches = 0usize;
    for change in diff.iter_all_changes() {
        if change.tag() == similar::ChangeTag::Equal {
            matches += 1;
        }
    }

    let similarity = (2 * matches * 100) / max_lines;
    similarity.min(100) as u8
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
