//! The changed-files card under a turn's last assistant message (`ChangedFilesTree.tsx`,
//! `apps/web/src/lib/turnDiffTree.ts`) and which message carries each turn's summary
//! (`assistantMessageId` in `deriveThreadCheckpointSummaries`, `client-runtime/src/state/threadCheckpoints.ts`).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use crate::diff::{CheckpointFile, FileTotals, TurnDiffSummary, turn_diff_summaries};
use crate::thread::ThreadProjection;

/// Each turn's checkpoint summary, keyed by the last assistant message of its run. A run with
/// several checkpoints shows the newest.
pub fn turn_diffs_by_message(projection: &ThreadProjection) -> HashMap<String, Arc<TurnDiffSummary>> {
    let summaries = turn_diff_summaries(&projection.checkpoints);
    if summaries.is_empty() {
        return HashMap::new();
    }
    let mut message_by_run: HashMap<&str, Option<&str>> = summaries.iter().map(|summary| (summary.run_id.as_str(), None)).collect();
    let mut missing = message_by_run.len();
    for message in projection.messages.iter().rev() {
        if missing == 0 {
            break;
        }
        let value = &message.value;
        if value.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(slot) = value.get("runId").and_then(Value::as_str).and_then(|run| message_by_run.get_mut(run)) else { continue };
        if slot.is_none() {
            *slot = Some(value.get("id").and_then(Value::as_str).unwrap_or(message.id.as_str()));
            missing -= 1;
        }
    }
    let mut by_message = HashMap::new();
    // Oldest first, so the newest checkpoint of a run is the one kept.
    for summary in summaries.iter().rev() {
        if let Some(Some(message_id)) = message_by_run.get(summary.run_id.as_str()) {
            by_message.insert((*message_id).to_owned(), Arc::new(summary.clone()));
        }
    }
    by_message
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStat {
    pub additions: u64,
    pub deletions: u64,
}

impl DiffStat {
    pub fn is_zero(&self) -> bool {
        self.additions == 0 && self.deletions == 0
    }
}

impl From<FileTotals> for DiffStat {
    fn from(totals: FileTotals) -> Self {
        DiffStat { additions: totals.additions, deletions: totals.deletions }
    }
}

/// `TurnDiffTreeNode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeNode {
    Directory { name: String, path: String, stat: DiffStat, children: Vec<TreeNode> },
    File { name: String, path: String, stat: DiffStat },
}

impl TreeNode {
    fn name(&self) -> &str {
        match self {
            TreeNode::Directory { name, .. } | TreeNode::File { name, .. } => name,
        }
    }
}

#[derive(Default)]
struct Directory {
    name: String,
    path: String,
    stat: DiffStat,
    directories: Vec<Directory>,
    files: Vec<TreeNode>,
}

/// `Intl.Collator(undefined, { numeric: true, sensitivity: "base" })`: case-insensitive, with runs
/// of digits compared by value.
pub fn compare_names(left: &str, right: &str) -> Ordering {
    let (mut a, mut b) = (left.chars().peekable(), right.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |chars: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        chars.next();
                    }
                    digits
                };
                let (x, y) = (take(&mut a), take(&mut b));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// `buildTurnDiffTree`: directories first, both sorted by name; single-child directory chains
/// merge into one row; each directory sums its files.
pub fn changed_file_tree(files: &[CheckpointFile]) -> Vec<TreeNode> {
    let mut root = Directory::default();
    for file in files {
        let normalized = file.path.replace('\\', "/");
        let segments: Vec<&str> = normalized.split('/').filter(|segment| !segment.is_empty()).collect();
        let Some((name, parents)) = segments.split_last() else { continue };
        let stat = DiffStat { additions: file.additions, deletions: file.deletions };
        let mut directory = &mut root;
        directory.stat.additions += stat.additions;
        directory.stat.deletions += stat.deletions;
        for segment in parents {
            let index = match directory.directories.iter().position(|child| child.name == *segment) {
                Some(index) => index,
                None => {
                    let path = if directory.path.is_empty() { (*segment).to_owned() } else { format!("{}/{segment}", directory.path) };
                    directory.directories.push(Directory { name: (*segment).to_owned(), path, ..Directory::default() });
                    directory.directories.len() - 1
                }
            };
            directory = &mut directory.directories[index];
            directory.stat.additions += stat.additions;
            directory.stat.deletions += stat.deletions;
        }
        directory.files.push(TreeNode::File { name: (*name).to_owned(), path: segments.join("/"), stat });
    }
    tree_nodes(root)
}

fn tree_nodes(directory: Directory) -> Vec<TreeNode> {
    let mut directories = directory.directories;
    directories.sort_by(|a, b| compare_names(&a.name, &b.name));
    let mut nodes: Vec<TreeNode> = directories
        .into_iter()
        .map(|child| {
            let (name, path, stat) = (child.name.clone(), child.path.clone(), child.stat);
            compact(TreeNode::Directory { name, path, stat, children: tree_nodes(child) })
        })
        .collect();
    let mut files = directory.files;
    files.sort_by(|a, b| compare_names(a.name(), b.name()));
    nodes.extend(files);
    nodes
}

fn compact(node: TreeNode) -> TreeNode {
    match node {
        TreeNode::Directory { name, children, .. } if matches!(children.as_slice(), [TreeNode::Directory { .. }]) => {
            let Some(TreeNode::Directory { name: child, path, stat, children }) = children.into_iter().next() else { unreachable!() };
            compact(TreeNode::Directory { name: format!("{name}/{child}"), path, stat, children })
        }
        node => node,
    }
}

/// One drawn row of the card's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFileRow<'a> {
    pub node: &'a TreeNode,
    pub depth: usize,
    /// For directories: whether its children are listed.
    pub expanded: bool,
}

/// `collectExpandedRows`: a directory is open per its override, else per the card's all-open toggle.
pub fn changed_file_rows<'a>(nodes: &'a [TreeNode], overrides: &HashMap<String, bool>, all_expanded: bool) -> Vec<ChangedFileRow<'a>> {
    fn collect<'a>(nodes: &'a [TreeNode], overrides: &HashMap<String, bool>, all: bool, depth: usize, rows: &mut Vec<ChangedFileRow<'a>>) {
        for node in nodes {
            let expanded = match node {
                TreeNode::Directory { path, .. } => overrides.get(path).copied().unwrap_or(all),
                TreeNode::File { .. } => false,
            };
            rows.push(ChangedFileRow { node, depth, expanded });
            if let (TreeNode::Directory { children, .. }, true) = (node, expanded) {
                collect(children, overrides, all, depth + 1, rows);
            }
        }
    }
    let mut rows = Vec::new();
    collect(nodes, overrides, all_expanded, 0, &mut rows);
    rows
}

pub fn has_directories(nodes: &[TreeNode]) -> bool {
    nodes.iter().any(|node| matches!(node, TreeNode::Directory { .. }))
}

/// Every directory path, for resetting overrides when the all-open toggle flips.
pub fn directory_paths(nodes: &[TreeNode]) -> HashSet<String> {
    let mut paths = HashSet::new();
    let mut stack: Vec<&TreeNode> = nodes.iter().collect();
    while let Some(node) = stack.pop() {
        if let TreeNode::Directory { path, children, .. } = node {
            paths.insert(path.clone());
            stack.extend(children);
        }
    }
    paths
}

/// `toLocaleString()` in en-US: thousands grouped with commas.
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The header: `N changed file(s)`.
pub fn changed_files_label(count: u64) -> String {
    format!("{} changed file{}", grouped(count), if count == 1 { "" } else { "s" })
}

/// The footer button when the wire sent only a prefix of the files.
pub fn hidden_files_label(hidden: u64) -> String {
    format!("{} more file{} in the full diff", grouped(hidden), if hidden == 1 { "" } else { "s" })
}
