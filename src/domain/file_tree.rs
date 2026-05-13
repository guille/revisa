use std::collections::BTreeMap;
use std::path::Path;

/// A node in the file tree. Either a directory (with children) or a file (leaf).
#[derive(Debug)]
pub enum TreeNode {
    Dir {
        /// Directory name (just the component, not full path).
        name: String,
        /// Children sorted by name, directories first, then files.
        children: Vec<TreeNode>,
        /// Whether this directory is expanded in the UI.
        expanded: bool,
    },
    File {
        /// File name (just the component).
        name: String,
        /// Index into the flat `file_pairs` list.
        file_idx: usize,
    },
}

/// A flattened entry for rendering — one per visible row in the sidebar.
#[derive(Debug, Clone)]
pub struct FlatEntry {
    /// Indentation depth (0 = root level).
    pub depth: usize,
    /// The content of this row.
    pub kind: FlatEntryKind,
}

#[derive(Debug, Clone)]
pub enum FlatEntryKind {
    Dir {
        /// Directory name.
        name: String,
        /// Index into the tree's node list for toggling expand/collapse.
        /// This is a path of child indices from the root.
        path: Vec<usize>,
        expanded: bool,
        /// Full relative directory path (e.g. "src/ui").
        dir_path: std::path::PathBuf,
    },
    File {
        /// File name (just the basename).
        name: String,
        /// Index into `file_pairs`.
        file_idx: usize,
    },
}

/// Key for BTreeMap that sorts dirs and files separately while keeping
/// alphabetical order within each group. Files sort after dirs with the
/// same name (the `is_file` bool: false < true).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TreeKey {
    name: String,
    is_file: bool,
}

/// Intermediate builder for constructing the tree.
enum DirBuilder {
    Dir(BTreeMap<TreeKey, DirBuilder>),
    Leaf(String, usize),
}

/// Build a tree from flat file paths.
/// Each path component becomes a directory node; the final component is a file leaf.
pub fn build_tree(paths: &[(usize, &Path)]) -> Vec<TreeNode> {
    let mut root: BTreeMap<TreeKey, DirBuilder> = BTreeMap::new();

    for &(file_idx, path) in paths {
        let components: Vec<&str> = path
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect();

        if components.is_empty() {
            continue;
        }

        if components.len() == 1 {
            let key = TreeKey {
                name: components[0].to_string(),
                is_file: true,
            };
            root.entry(key)
                .or_insert_with(|| DirBuilder::Leaf(components[0].to_string(), file_idx));
        } else {
            insert_into_tree(&mut root, &components, file_idx);
        }
    }

    builder_to_nodes(root)
}

fn insert_into_tree(map: &mut BTreeMap<TreeKey, DirBuilder>, components: &[&str], file_idx: usize) {
    let name = components[0];

    if components.len() == 1 {
        let key = TreeKey {
            name: name.to_string(),
            is_file: true,
        };
        map.entry(key)
            .or_insert_with(|| DirBuilder::Leaf(name.to_string(), file_idx));
    } else {
        let key = TreeKey {
            name: name.to_string(),
            is_file: false,
        };
        let entry = map
            .entry(key)
            .or_insert_with(|| DirBuilder::Dir(BTreeMap::new()));
        if let DirBuilder::Dir(children) = entry {
            insert_into_tree(children, &components[1..], file_idx);
        }
    }
}

fn builder_to_nodes(map: BTreeMap<TreeKey, DirBuilder>) -> Vec<TreeNode> {
    let mut nodes = Vec::new();

    for (key, builder) in map {
        match builder {
            DirBuilder::Dir(children) => {
                nodes.push(TreeNode::Dir {
                    name: key.name,
                    children: builder_to_nodes(children),
                    expanded: true,
                });
            }
            DirBuilder::Leaf(name, file_idx) => {
                nodes.push(TreeNode::File { name, file_idx });
            }
        }
    }

    nodes
}

/// Flatten the tree into a list of visible entries for rendering.
pub fn flatten_tree(roots: &[TreeNode], depth: usize) -> Vec<FlatEntry> {
    let mut result = Vec::new();
    flatten_recursive(
        roots,
        depth,
        &mut Vec::new(),
        &mut std::path::PathBuf::new(),
        &mut result,
    );
    result
}

fn flatten_recursive(
    nodes: &[TreeNode],
    depth: usize,
    path: &mut Vec<usize>,
    dir_path: &mut std::path::PathBuf,
    result: &mut Vec<FlatEntry>,
) {
    for (i, node) in nodes.iter().enumerate() {
        path.push(i);
        match node {
            TreeNode::Dir {
                name,
                children,
                expanded,
            } => {
                dir_path.push(name);
                result.push(FlatEntry {
                    depth,
                    kind: FlatEntryKind::Dir {
                        name: name.clone(),
                        path: path.clone(),
                        expanded: *expanded,
                        dir_path: dir_path.clone(),
                    },
                });
                if *expanded {
                    flatten_recursive(children, depth + 1, path, dir_path, result);
                }
                dir_path.pop();
            }
            TreeNode::File { name, file_idx } => {
                result.push(FlatEntry {
                    depth,
                    kind: FlatEntryKind::File {
                        name: name.clone(),
                        file_idx: *file_idx,
                    },
                });
            }
        }
        path.pop();
    }
}

/// Toggle the expanded state of a directory node at the given path.
pub fn toggle_dir(roots: &mut [TreeNode], path: &[usize]) {
    if path.is_empty() {
        return;
    }
    let mut nodes: &mut [TreeNode] = roots;
    for &idx in &path[..path.len() - 1] {
        if let Some(TreeNode::Dir { children, .. }) = nodes.get_mut(idx) {
            nodes = children;
        } else {
            return;
        }
    }
    if let Some(TreeNode::Dir { expanded, .. }) = nodes.get_mut(path[path.len() - 1]) {
        *expanded = !*expanded;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_flat_files_at_root() {
        let paths: Vec<(usize, &Path)> = vec![(0, Path::new("a.rs")), (1, Path::new("b.rs"))];
        let tree = build_tree(&paths);
        assert_eq!(tree.len(), 2);
        assert!(matches!(&tree[0], TreeNode::File { name, file_idx: 0 } if name == "a.rs"));
        assert!(matches!(&tree[1], TreeNode::File { name, file_idx: 1 } if name == "b.rs"));
    }

    #[test]
    fn test_nested_dirs() {
        let paths: Vec<(usize, &Path)> = vec![
            (0, Path::new("src/main.rs")),
            (1, Path::new("src/lib.rs")),
            (2, Path::new("README.md")),
        ];
        let tree = build_tree(&paths);
        // Alphabetical: README.md, then src/
        assert_eq!(tree.len(), 2);
        assert!(matches!(&tree[0], TreeNode::File { name, file_idx: 2 } if name == "README.md"));
        assert!(matches!(&tree[1], TreeNode::Dir { name, .. } if name == "src"));

        if let TreeNode::Dir { children, .. } = &tree[1] {
            assert_eq!(children.len(), 2);
            assert!(
                matches!(&children[0], TreeNode::File { name, file_idx: 1 } if name == "lib.rs")
            );
            assert!(
                matches!(&children[1], TreeNode::File { name, file_idx: 0 } if name == "main.rs")
            );
        }
    }

    #[test]
    fn test_flatten_expanded() {
        let paths: Vec<(usize, &Path)> =
            vec![(0, Path::new("src/main.rs")), (1, Path::new("README.md"))];
        let tree = build_tree(&paths);
        let flat = flatten_tree(&tree, 0);
        // Alphabetical: README.md (depth 0), src/ (depth 0), main.rs (depth 1)
        assert_eq!(flat.len(), 3);
        assert_eq!(flat[0].depth, 0);
        assert!(matches!(&flat[0].kind, FlatEntryKind::File { name, .. } if name == "README.md"));
        assert_eq!(flat[1].depth, 0);
        assert!(matches!(&flat[1].kind, FlatEntryKind::Dir { name, .. } if name == "src"));
        assert_eq!(flat[2].depth, 1);
        assert!(matches!(&flat[2].kind, FlatEntryKind::File { name, .. } if name == "main.rs"));
    }

    #[test]
    fn test_flatten_collapsed() {
        let paths: Vec<(usize, &Path)> =
            vec![(0, Path::new("src/main.rs")), (1, Path::new("README.md"))];
        let mut tree = build_tree(&paths);
        // Collapse src/ (now at index 1 after README.md).
        toggle_dir(&mut tree, &[1]);
        let flat = flatten_tree(&tree, 0);
        // Only: README.md, src/ (collapsed)
        assert_eq!(flat.len(), 2);
    }

    #[test]
    fn test_toggle_dir() {
        let paths: Vec<(usize, &Path)> = vec![(0, Path::new("src/a.rs"))];
        let mut tree = build_tree(&paths);
        assert!(matches!(&tree[0], TreeNode::Dir { expanded: true, .. }));
        toggle_dir(&mut tree, &[0]);
        assert!(matches!(
            &tree[0],
            TreeNode::Dir {
                expanded: false,
                ..
            }
        ));
        toggle_dir(&mut tree, &[0]);
        assert!(matches!(&tree[0], TreeNode::Dir { expanded: true, .. }));
    }

    #[test]
    fn test_deeply_nested() {
        let paths: Vec<(usize, &Path)> = vec![(0, Path::new("a/b/c/d.rs"))];
        let tree = build_tree(&paths);
        let flat = flatten_tree(&tree, 0);
        // a/ (0), b/ (1), c/ (2), d.rs (3)
        assert_eq!(flat.len(), 4);
        assert_eq!(flat[3].depth, 3);
        assert!(matches!(
            &flat[3].kind,
            FlatEntryKind::File { file_idx: 0, .. }
        ));
    }

    #[test]
    fn test_empty_input() {
        let tree = build_tree(&[]);
        assert!(tree.is_empty());
        let flat = flatten_tree(&tree, 0);
        assert!(flat.is_empty());
    }
}
