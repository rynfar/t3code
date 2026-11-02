//! The diff file tree (`components/diffs/diffFileTree.logic.ts` with Pierre's tree options
//! `flattenEmptyDirectories: true` and `initialExpansion: "open"`): directories in the order their
//! first file appears in the patch, single-child directory chains merged into one row.

use std::collections::HashSet;

use super::{ChangeKind, FileDiff};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRowKind {
    /// `path` ends with `/` and identifies the directory for expand/collapse.
    Directory { path: String, expanded: bool },
    File { file: usize, change: ChangeKind },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: usize,
    pub label: String,
    pub kind: TreeRowKind,
}

#[derive(Default)]
struct Node {
    name: String,
    directories: Vec<Node>,
    files: Vec<(String, usize)>,
    /// First file index under this node: siblings rank by it.
    first: usize,
}

impl Node {
    fn insert(&mut self, segments: &[&str], file: usize) {
        match segments {
            [] => {}
            [name] => self.files.push(((*name).to_owned(), file)),
            [directory, rest @ ..] => {
                let index = match self.directories.iter().position(|node| node.name == *directory) {
                    Some(index) => index,
                    None => {
                        self.directories.push(Node { name: (*directory).to_owned(), first: file, ..Node::default() });
                        self.directories.len() - 1
                    }
                };
                self.directories[index].insert(rest, file);
            }
        }
    }
}

pub fn file_tree_rows(files: &[FileDiff], collapsed_directories: &HashSet<String>) -> Vec<TreeRow> {
    let mut root = Node::default();
    for (index, file) in files.iter().enumerate() {
        let segments: Vec<&str> = file.path().split('/').filter(|segment| !segment.is_empty()).collect();
        root.insert(&segments, index);
    }
    let mut rows = Vec::new();
    emit(&root, "", 0, files, collapsed_directories, &mut rows);
    rows
}

fn emit(node: &Node, prefix: &str, depth: usize, files: &[FileDiff], collapsed: &HashSet<String>, rows: &mut Vec<TreeRow>) {
    enum Entry<'a> {
        Directory(&'a Node),
        File(&'a str, usize),
    }
    let mut entries: Vec<(usize, Entry)> = node
        .directories
        .iter()
        .map(|directory| (directory.first, Entry::Directory(directory)))
        .chain(node.files.iter().map(|(name, file)| (*file, Entry::File(name, *file))))
        .collect();
    entries.sort_by_key(|(first, _)| *first);
    for (_, entry) in entries {
        match entry {
            Entry::File(name, file) => rows.push(TreeRow {
                depth,
                label: name.to_owned(),
                kind: TreeRowKind::File { file, change: files[file].change },
            }),
            Entry::Directory(directory) => {
                let mut label = directory.name.clone();
                let mut current = directory;
                while current.files.is_empty() && current.directories.len() == 1 {
                    current = &current.directories[0];
                    label = format!("{label}/{}", current.name);
                }
                let path = format!("{prefix}{label}/");
                let expanded = !collapsed.contains(&path);
                rows.push(TreeRow {
                    depth,
                    label,
                    kind: TreeRowKind::Directory { path: path.clone(), expanded },
                });
                if expanded {
                    emit(current, &path, depth + 1, files, collapsed, rows);
                }
            }
        }
    }
}
