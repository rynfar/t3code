//! The explorer's tree: folders listed lazily as they expand (`useDirectoryEntries.ts`), Pierre's
//! compact tree semantics as `FileBrowserPanel.tsx` configures them (closed initially, folders
//! first, empty-folder chains flattened, `hide-non-matches` search over full paths), expand all
//! skipping ignored folders (`buildTreeEntries`), and refresh of what was listed.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use super::{Entry, EntryKind, ancestor_directories, parent_directory};

/// One visible row. A flattened chain (`a/b/c` with nothing else inside) is one row whose `path`
/// is the deepest folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub path: String,
    pub label: String,
    pub depth: usize,
    pub kind: EntryKind,
    pub expanded: bool,
    pub ignored: bool,
    pub loading: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FileTree {
    children: HashMap<String, Vec<Entry>>,
    expanded: HashSet<String>,
    loading: HashMap<String, u64>,
    errors: HashMap<String, String>,
    generation: u64,
    /// While on, every folder that loads has its non-ignored subfolders expanded and listed.
    expand_all: bool,
}

/// `defaultChildrenComparator`: folders first, dot-entries first, then case-insensitive names.
fn compare(left: &Entry, right: &Entry) -> Ordering {
    let folder = |entry: &Entry| entry.kind != EntryKind::Directory;
    let dot = |entry: &Entry| !entry.name().starts_with('.');
    folder(left)
        .cmp(&folder(right))
        .then_with(|| dot(left).cmp(&dot(right)))
        .then_with(|| left.name().to_lowercase().cmp(&right.name().to_lowercase()))
        .then_with(|| left.name().cmp(right.name()))
}

impl FileTree {
    pub fn is_loaded(&self, directory: &str) -> bool {
        self.children.contains_key(directory)
    }

    pub fn is_loading(&self, directory: &str) -> bool {
        self.loading.contains_key(directory)
    }

    pub fn is_expanded(&self, directory: &str) -> bool {
        self.expanded.contains(directory)
    }

    /// The root listing arrived: the tree has something to show.
    pub fn ready(&self) -> bool {
        self.is_loaded("")
    }

    pub fn expanding_all(&self) -> bool {
        self.expand_all
    }

    /// Starts a listing of `directory`. Returns the generation its answer must carry, or `None`
    /// when it is listed or already being listed and `refresh` is false.
    pub fn begin_load(&mut self, directory: &str, refresh: bool) -> Option<u64> {
        if !refresh && (self.is_loaded(directory) || self.is_loading(directory)) {
            return None;
        }
        self.generation += 1;
        self.loading.insert(directory.to_owned(), self.generation);
        Some(self.generation)
    }

    /// Stores a listing. Returns the folders that now need listing: a lone subfolder of an
    /// expanded folder (it shows flattened, so it opens with its parent) and, during expand all,
    /// every non-ignored subfolder. A stale answer (an older generation) is dropped.
    pub fn finish_load(&mut self, directory: &str, generation: u64, mut entries: Vec<Entry>) -> Vec<String> {
        if self.loading.get(directory) != Some(&generation) {
            return Vec::new();
        }
        self.loading.remove(directory);
        self.errors.remove(directory);
        entries.sort_by(compare);
        let lone_folder = match entries.as_slice() {
            [only] if only.kind == EntryKind::Directory => Some(only.path.clone()),
            _ => None,
        };
        let mut open: Vec<String> = Vec::new();
        if self.expand_all {
            open.extend(entries.iter().filter(|entry| entry.kind == EntryKind::Directory && !entry.ignored).map(|entry| entry.path.clone()));
        }
        if self.is_expanded(directory) {
            open.extend(lone_folder.filter(|folder| !open.contains(folder)));
        }
        self.children.insert(directory.to_owned(), entries);
        let mut load = Vec::new();
        for folder in open {
            self.expanded.insert(folder.clone());
            if !self.is_loaded(&folder) && !self.is_loading(&folder) {
                load.push(folder);
            }
        }
        load
    }

    /// A listing failed; it is retried on the next expand, refresh or reconnect.
    pub fn fail_load(&mut self, directory: &str, generation: u64, message: String) {
        if self.loading.get(directory) == Some(&generation) {
            self.loading.remove(directory);
            self.errors.insert(directory.to_owned(), message);
        }
    }

    /// The first error of a folder still in the tree.
    pub fn error(&self) -> Option<&str> {
        let reachable = self.reachable_directories();
        let mut errors: Vec<(&String, &String)> = self.errors.iter().filter(|(path, _)| reachable.contains(path.as_str())).collect();
        errors.sort();
        errors.first().map(|(_, message)| message.as_str())
    }

    pub fn failed_directories(&self) -> Vec<String> {
        self.errors.keys().cloned().collect()
    }

    /// Opens or closes a folder. Returns it when it opened and still needs listing.
    pub fn toggle(&mut self, directory: &str) -> Option<String> {
        if self.expanded.remove(directory) {
            self.expand_all = false;
            return None;
        }
        self.expanded.insert(directory.to_owned());
        (!self.is_loaded(directory)).then(|| directory.to_owned())
    }

    /// Opens a folder. Returns it when it still needs listing.
    pub fn expand(&mut self, directory: &str) -> Option<String> {
        if directory.is_empty() || !self.expanded.insert(directory.to_owned()) {
            return None;
        }
        (!self.is_loaded(directory) && !self.is_loading(directory)).then(|| directory.to_owned())
    }

    pub fn collapse(&mut self, directory: &str) -> bool {
        let collapsed = self.expanded.remove(directory);
        if collapsed {
            self.expand_all = false;
        }
        collapsed
    }

    /// `loadFileAncestors` plus the reveal: every folder above `path` opens. Returns those that
    /// need listing, outermost first.
    pub fn reveal(&mut self, path: &str) -> Vec<String> {
        let mut load = Vec::new();
        for ancestor in ancestor_directories(path) {
            if !ancestor.is_empty() {
                self.expanded.insert(ancestor.clone());
            }
            if !self.is_loaded(&ancestor) {
                load.push(ancestor);
            }
        }
        load
    }

    /// The kind of a listed path.
    pub fn kind(&self, path: &str) -> Option<EntryKind> {
        self.children.get(parent_directory(path))?.iter().find(|entry| entry.path == path).map(|entry| entry.kind)
    }

    fn reachable_directories(&self) -> HashSet<&str> {
        let mut reachable = HashSet::from([""]);
        let mut stack = vec![""];
        while let Some(directory) = stack.pop() {
            for entry in self.children.get(directory).into_iter().flatten() {
                if entry.kind == EntryKind::Directory && reachable.insert(entry.path.as_str()) {
                    stack.push(entry.path.as_str());
                }
            }
        }
        reachable
    }

    /// Folders to list again on refresh: those listed (or failed) and still reachable.
    pub fn refresh_targets(&self) -> Vec<String> {
        let reachable = self.reachable_directories();
        let mut targets: Vec<String> = self
            .children
            .keys()
            .chain(self.errors.keys())
            .filter(|path| reachable.contains(path.as_str()))
            .cloned()
            .collect();
        targets.sort();
        targets.dedup();
        targets
    }

    fn expandable_directories(&self) -> impl Iterator<Item = &Entry> {
        self.children.values().flatten().filter(|entry| entry.kind == EntryKind::Directory && !entry.ignored)
    }

    pub fn has_directories(&self) -> bool {
        self.children.values().flatten().any(|entry| entry.kind == EntryKind::Directory)
    }

    /// `areAllDirectoriesExpanded` over the non-ignored folders.
    pub fn all_expanded(&self) -> bool {
        let mut any = false;
        for entry in self.expandable_directories() {
            any = true;
            if !self.expanded.contains(&entry.path) {
                return false;
            }
        }
        any
    }

    /// Expand all: opens every known non-ignored folder and keeps opening what loads. Returns
    /// the folders that need listing.
    pub fn expand_all(&mut self) -> Vec<String> {
        self.expand_all = true;
        let folders: Vec<String> = self.expandable_directories().map(|entry| entry.path.clone()).collect();
        let mut load = Vec::new();
        for folder in folders {
            if !self.is_loaded(&folder) && !self.is_loading(&folder) {
                load.push(folder.clone());
            }
            self.expanded.insert(folder);
        }
        load.sort();
        load
    }

    pub fn collapse_all(&mut self) {
        self.expand_all = false;
        self.expanded.clear();
    }

    /// The rows the tree shows without a search.
    pub fn rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        walk(&self.children, "", 0, &|path| self.expanded.contains(path), &|path| self.is_loading(path), &mut rows);
        rows
    }

    /// `hide-non-matches` search: entries whose path contains the query (case-insensitively),
    /// plus their folders, all open. `found` are server search results merged into the listed
    /// entries, as `buildTreeEntries` merges them.
    pub fn search_rows(&self, query: &str, found: &[Entry]) -> Vec<TreeRow> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return self.rows();
        }
        let mut known: HashMap<&str, &Entry> = HashMap::new();
        for entry in self.children.values().flatten().chain(found) {
            known.entry(entry.path.as_str()).or_insert(entry);
        }
        let mut visible: HashMap<String, Entry> = HashMap::new();
        for entry in known.values() {
            if !entry.path.to_lowercase().contains(&needle) {
                continue;
            }
            visible.insert(entry.path.clone(), (*entry).clone());
            for ancestor in ancestor_directories(&entry.path).into_iter().skip(1) {
                let folder = known.get(ancestor.as_str()).map_or_else(
                    || Entry { path: ancestor.clone(), kind: EntryKind::Directory, ignored: false },
                    |entry| (*entry).clone(),
                );
                visible.insert(ancestor, folder);
            }
        }
        let mut children: HashMap<String, Vec<Entry>> = HashMap::new();
        for entry in visible.into_values() {
            children.entry(parent_directory(&entry.path).to_owned()).or_default().push(entry);
        }
        for entries in children.values_mut() {
            entries.sort_by(compare);
        }
        let mut rows = Vec::new();
        walk(&children, "", 0, &|_| true, &|_| false, &mut rows);
        rows
    }
}

fn walk(
    children: &HashMap<String, Vec<Entry>>,
    directory: &str,
    depth: usize,
    expanded: &dyn Fn(&str) -> bool,
    loading: &dyn Fn(&str) -> bool,
    rows: &mut Vec<TreeRow>,
) {
    for entry in children.get(directory).into_iter().flatten() {
        if entry.kind == EntryKind::File {
            rows.push(TreeRow {
                path: entry.path.clone(),
                label: entry.name().to_owned(),
                depth,
                kind: EntryKind::File,
                expanded: false,
                ignored: entry.ignored,
                loading: false,
            });
            continue;
        }
        let mut last = entry;
        let mut label = entry.name().to_owned();
        while let Some([only]) = children.get(&last.path).map(Vec::as_slice) {
            if only.kind != EntryKind::Directory {
                break;
            }
            label.push('/');
            label.push_str(only.name());
            last = only;
        }
        let open = expanded(&last.path);
        rows.push(TreeRow {
            path: last.path.clone(),
            label,
            depth,
            kind: EntryKind::Directory,
            expanded: open,
            ignored: entry.ignored || last.ignored,
            loading: open && loading(&last.path),
        });
        if open {
            walk(children, &last.path, depth + 1, expanded, loading, rows);
        }
    }
}
