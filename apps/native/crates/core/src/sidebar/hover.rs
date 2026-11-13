//! What `SidebarThreadTooltip` (`apps/web/src/components/sidebar/SidebarThreadRow.tsx`) shows
//! beyond the row: the model line, prior handoffs, the checkout warning
//! (`resolveLocalCheckoutBranchMismatch`), the terminal count and the pull request list
//! (`pullRequestListLines`, `packages/source-control-core/src/shared/threadPullRequests.ts`).

use super::thread::SidebarThread;
use crate::providers::{self, ProviderEntry};
use crate::pull_requests::link::ThreadLink;
use crate::pull_requests::thread_badge::{ChainKind, resolve_thread_pull_request_chains};
use crate::timeline::parse_iso_ms;

/// The instance the thread runs on now: the runtime's while one exists, else the selection's.
pub fn model_instance_id(thread: &SidebarThread) -> &str {
    if thread.runtime.is_some() { &thread.provider_instance_id } else { &thread.model_selection.instance_id }
}

/// The model line's text: the selected model's trigger name (else the raw slug), plus the
/// instance's name when a badge tells instances of one driver apart.
pub fn model_text(thread: &SidebarThread, entry: &ProviderEntry, entries: &[ProviderEntry]) -> String {
    let slug = thread.model_selection.model.as_str();
    let label = entry.provider.models.iter().find(|model| model.slug == slug).map_or(slug, providers::trigger_model_name);
    if providers::shows_instance_badge(entry, entries) { format!("{label} · {}", entry.display_name) } else { label.to_owned() }
}

/// `Handed off from …`: every earlier instance in the thread's history, by name when known.
pub fn handed_off_from(thread: &SidebarThread, entries: &[ProviderEntry]) -> Option<String> {
    let current = model_instance_id(thread);
    let names: Vec<&str> = thread
        .provider_instance_history
        .iter()
        .filter(|id| id.as_str() != current)
        .map(|id| entries.iter().find(|entry| entry.instance_id() == id).map_or(id.as_str(), |entry| entry.display_name.as_str()))
        .collect();
    (!names.is_empty()).then(|| format!("Handed off from {}", names.join(", ")))
}

/// The checkout a local-mode thread's branch is compared against: the project root, and only
/// when the thread records a branch.
pub fn checkout_to_compare<'a>(thread: &SidebarThread, workspace_root: Option<&'a str>) -> Option<&'a str> {
    let local = thread.worktree_path.is_none();
    let branch = thread.branch.as_deref().is_some_and(|branch| !branch.is_empty());
    workspace_root.filter(|root| local && branch && !root.is_empty())
}

/// `resolveLocalCheckoutBranchMismatch`: a local-mode thread whose checkout moved to another branch.
pub fn branch_mismatch(thread: &SidebarThread, current_branch: Option<&str>) -> bool {
    if thread.worktree_path.is_some() {
        return false;
    }
    match (thread.branch.as_deref().filter(|branch| !branch.is_empty()), current_branch.filter(|branch| !branch.is_empty())) {
        (Some(thread_branch), Some(current)) => thread_branch != current,
        _ => false,
    }
}

/// `terminalProcessLabel`.
pub fn terminal_process_label(count: usize) -> String {
    format!("{count} terminal {} running", if count == 1 { "process" } else { "processes" })
}

/// One `PullRequestListLine`: a link, its depth in its chain, and the chain's size on its first line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestLine<'a> {
    pub link: &'a ThreadLink,
    pub depth: usize,
    pub stack: Option<(ChainKind, usize)>,
}

impl PullRequestLine<'_> {
    /// The trailing `stack · 3` / `chain · 2` label.
    pub fn stack_label(&self) -> Option<String> {
        self.stack.map(|(kind, size)| format!("{} · {size}", if kind == ChainKind::Native { "stack" } else { "chain" }))
    }
}

fn latest_updated_at(link: &ThreadLink) -> i64 {
    parse_iso_ms(link.snapshot.as_ref().and_then(|snapshot| snapshot.updated_at.as_deref()).unwrap_or(&link.linked_at)).unwrap_or(0)
}

/// `pullRequestListLines(resolveThreadPullRequestChains(visibleThreadPullRequests(links)))`:
/// chains by their newest activity, each laid out bottom first.
pub fn pull_request_list_lines(links: &[ThreadLink]) -> Vec<PullRequestLine<'_>> {
    let mut chains = resolve_thread_pull_request_chains(links);
    chains.sort_by_key(|chain| std::cmp::Reverse(chain.layers.iter().map(|link| latest_updated_at(link)).max().unwrap_or(0)));
    chains
        .into_iter()
        .flat_map(|chain| {
            let size = chain.layers.len();
            let kind = chain.kind;
            chain.layers.into_iter().zip(chain.depths).enumerate().map(move |(index, (link, depth))| PullRequestLine {
                link,
                depth,
                stack: (index == 0 && size > 1).then_some((kind, size)),
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "hover_tests.rs"]
mod tests;
