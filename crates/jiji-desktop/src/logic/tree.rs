//! Shaping for the diff pane's file tree: paths become a directory tree,
//! runs of single-child directories compact into one row, and the tree
//! flattens against a set of collapsed paths.

use std::collections::{BTreeMap, HashSet};

use jiji_core::snapshot::FileStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirRow {
    /// Full repo-relative prefix, no trailing slash — unique among rows
    /// and the collapse key.
    pub path: String,
    /// What the row shows: one segment, or a compacted run like
    /// "src/lib/components" when every level between holds nothing else.
    pub label: String,
    pub depth: usize,
    /// Aggregates over every file underneath, for the collapsed summary.
    pub file_count: usize,
    pub added: u32,
    pub removed: u32,
    pub has_conflict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub path: String,
    pub name: String,
    pub depth: usize,
    /// Position in the diff's file order — the scroll target.
    pub index: usize,
    pub status: FileStatus,
    pub has_conflict: bool,
    pub added: u32,
    pub removed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRow {
    Dir(DirRow),
    File(FileRow),
}

impl TreeRow {
    pub fn depth(&self) -> usize {
        match self {
            TreeRow::Dir(dir) => dir.depth,
            TreeRow::File(file) => file.depth,
        }
    }
}

/// One file as the tree needs it.
pub struct TreeFile<'a> {
    pub path: &'a str,
    pub status: FileStatus,
    pub has_conflict: bool,
    pub added: u32,
    pub removed: u32,
}

#[derive(Default)]
struct DirNode {
    path: String,
    label: String,
    dirs: BTreeMap<String, DirNode>,
    files: Vec<FileRow>,
}

/// Every row of the tree, depth-first, fully expanded. Directories come
/// before files at each level, both alphabetical — the file-browser
/// convention, which may differ from the diff's own file order; `index`
/// keeps each file pointed at its section regardless.
pub fn build_tree_rows(files: &[TreeFile]) -> Vec<TreeRow> {
    let mut root = DirNode::default();
    for (index, file) in files.iter().enumerate() {
        let mut segments: Vec<&str> = file.path.split('/').collect();
        let name = segments.pop().unwrap_or_default();
        let mut node = &mut root;
        for segment in segments {
            let parent = node.path.clone();
            node = node
                .dirs
                .entry(segment.to_owned())
                .or_insert_with(|| DirNode {
                    path: if parent.is_empty() {
                        segment.to_owned()
                    } else {
                        format!("{parent}/{segment}")
                    },
                    label: segment.to_owned(),
                    ..DirNode::default()
                });
        }
        node.files.push(FileRow {
            path: file.path.to_owned(),
            name: name.to_owned(),
            depth: 0,
            index,
            status: file.status,
            has_conflict: file.has_conflict,
            added: file.added,
            removed: file.removed,
        });
    }
    let mut rows = Vec::new();
    walk(&root, 0, &mut rows);
    rows
}

/// (file count, added, removed, has conflict) of everything under `node`.
fn walk(node: &DirNode, depth: usize, out: &mut Vec<TreeRow>) -> (usize, u32, u32, bool) {
    let mut total = (0, 0, 0, false);
    for dir in node.dirs.values() {
        // Compact pass-through levels: a chain of directories with a
        // single child each renders as one row, so depth is spent on real
        // branching.
        let mut dir = dir;
        let mut label = dir.label.clone();
        while dir.dirs.len() == 1 && dir.files.is_empty() {
            let only = dir.dirs.values().next().unwrap();
            label = format!("{label}/{}", only.label);
            dir = only;
        }
        let at = out.len();
        out.push(TreeRow::Dir(DirRow {
            path: dir.path.clone(),
            label,
            depth,
            file_count: 0,
            added: 0,
            removed: 0,
            has_conflict: false,
        }));
        let sub = walk(dir, depth + 1, out);
        if let TreeRow::Dir(row) = &mut out[at] {
            row.file_count = sub.0;
            row.added = sub.1;
            row.removed = sub.2;
            row.has_conflict = sub.3;
        }
        total = (
            total.0 + sub.0,
            total.1 + sub.1,
            total.2 + sub.2,
            total.3 || sub.3,
        );
    }
    let mut files: Vec<&FileRow> = node.files.iter().collect();
    files.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.name.cmp(&b.name))
    });
    for file in files {
        out.push(TreeRow::File(FileRow {
            depth,
            ..file.clone()
        }));
        total = (
            total.0 + 1,
            total.1 + file.added,
            total.2 + file.removed,
            total.3 || file.has_conflict,
        );
    }
    total
}

/// The rows on screen: a collapsed directory keeps its own row and drops
/// everything underneath.
pub fn visible_tree_rows<'a>(rows: &'a [TreeRow], collapsed: &HashSet<String>) -> Vec<&'a TreeRow> {
    let mut out = Vec::new();
    // Depth of the shallowest collapsed row whose subtree we are inside;
    // rows at or below it are hidden until the walk surfaces again.
    let mut hide_below = usize::MAX;
    for row in rows {
        if row.depth() >= hide_below {
            continue;
        }
        out.push(row);
        hide_below = match row {
            TreeRow::Dir(dir) if collapsed.contains(&dir.path) => dir.depth + 1,
            _ => usize::MAX,
        };
    }
    out
}

/// Which row highlights for the file the diff is scrolled to: the file
/// itself, or — when an ancestor is collapsed — the shallowest collapsed
/// ancestor, which is the row standing in for it on screen.
pub fn active_row_key<'a>(
    rows: &'a [TreeRow],
    collapsed: &HashSet<String>,
    path: &'a str,
) -> &'a str {
    for row in rows {
        if let TreeRow::Dir(dir) = row {
            if collapsed.contains(&dir.path)
                && path.starts_with(dir.path.as_str())
                && path.as_bytes().get(dir.path.len()) == Some(&b'/')
            {
                return &dir.path;
            }
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, added: u32) -> TreeFile<'_> {
        TreeFile {
            path,
            status: FileStatus::Modified,
            has_conflict: false,
            added,
            removed: 0,
        }
    }

    fn labels(rows: &[&TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                TreeRow::Dir(dir) => format!("{}{}/", "  ".repeat(dir.depth), dir.label),
                TreeRow::File(file) => format!("{}{}", "  ".repeat(file.depth), file.name),
            })
            .collect()
    }

    #[test]
    fn single_child_directories_compact_and_dirs_sort_before_files() {
        let files = [
            file("README.md", 1),
            file("src/lib/components/diff/find.ts", 2),
            file("src/lib/components/diff/tree.ts", 3),
            file("src/lib/api.ts", 4),
        ];
        let rows = build_tree_rows(&files);
        let all: Vec<&TreeRow> = rows.iter().collect();
        assert_eq!(
            labels(&all),
            [
                "src/lib/",
                "  components/diff/",
                "    find.ts",
                "    tree.ts",
                "  api.ts",
                "README.md",
            ]
        );
        let TreeRow::Dir(top) = &rows[0] else {
            panic!("dir row");
        };
        assert_eq!((top.file_count, top.added), (3, 9));
        // Files keep pointing at their section in the diff's own order.
        let TreeRow::File(api) = &rows[4] else {
            panic!("file row");
        };
        assert_eq!(api.index, 3);
    }

    #[test]
    fn collapsing_hides_a_subtree_and_stands_in_for_its_files() {
        let files = [file("a/b/x.rs", 1), file("a/y.rs", 1), file("z.rs", 1)];
        let rows = build_tree_rows(&files);
        let collapsed = HashSet::from(["a".to_owned()]);
        let visible = visible_tree_rows(&rows, &collapsed);
        assert_eq!(labels(&visible), ["a/", "z.rs"]);
        assert_eq!(active_row_key(&rows, &collapsed, "a/b/x.rs"), "a");
        assert_eq!(active_row_key(&rows, &collapsed, "z.rs"), "z.rs");
        // A sibling whose name merely starts with the collapsed prefix is
        // not inside it.
        assert_eq!(active_row_key(&rows, &collapsed, "ab.rs"), "ab.rs");
    }
}
