//! Which pull request a thread row shows, and how: ports `resolveThreadPullRequestChains`,
//! `resolveThreadCurrentPullRequest(Link)` and `resolveThreadPullRequestBadge` from
//! `packages/source-control-core/src/shared/threadPullRequests.ts`, and `prStatusIndicator`,
//! `linkedPullRequestSnapshotStatus` and `resolveThreadPullRequestBadgePresentation` from
//! `apps/web/src/components/ThreadStatusIndicators.tsx`. Tones stand in for the web's color
//! classes. Keys are trimmed and lowercased; per-host canonical spellings (Azure DevOps aliases,
//! Forgejo ports) are not ported.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use super::link::{ThreadLink, change_request_provider};
use super::wire::{PrState, StackLayer};
use crate::timeline::parse_iso_ms;

/// `visibleThreadPullRequests`: dismissed stack members hidden, input order kept.
pub fn visible_thread_pull_requests(links: &[ThreadLink]) -> Vec<&ThreadLink> {
    links.iter().filter(|link| link.source != "stack-dismissed").collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainKind {
    Native,
    Derived,
}

/// `ThreadPullRequestChain`: layers bottom first, one depth per layer (siblings share one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPullRequestChain<'a> {
    pub kind: ChainKind,
    pub layers: Vec<&'a ThreadLink>,
    pub depths: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadCurrentPullRequest<'a> {
    Single(&'a ThreadLink),
    /// Every open link, the highest layer of the newest chain first.
    Stack { open: Vec<&'a ThreadLink>, top: &'a ThreadLink },
}

impl<'a> ThreadCurrentPullRequest<'a> {
    pub fn link(&self) -> &'a ThreadLink {
        match self {
            ThreadCurrentPullRequest::Single(link) => link,
            ThreadCurrentPullRequest::Stack { top, .. } => top,
        }
    }
}

fn is_open(link: &ThreadLink) -> bool {
    link.snapshot.as_ref().is_none_or(|snapshot| snapshot.state == PrState::Open)
}

fn latest_updated_at(link: &ThreadLink) -> i64 {
    parse_iso_ms(link.snapshot.as_ref().and_then(|snapshot| snapshot.updated_at.as_deref()).unwrap_or(&link.linked_at)).unwrap_or(0)
}

fn repository_key(link: &ThreadLink) -> String {
    format!("{}/{}", link.host.trim().to_lowercase(), link.repository.trim().to_lowercase())
}

/// `threadPullRequestKeyOf`.
fn identity(link: &ThreadLink) -> String {
    format!("{}#{}", repository_key(link), link.number)
}

impl<'a> ThreadPullRequestChain<'a> {
    /// `layersFromTop`: deepest first, later siblings before earlier ones.
    fn layers_from_top(&self) -> Vec<&'a ThreadLink> {
        let mut entries: Vec<(usize, usize, &'a ThreadLink)> = self.layers.iter().enumerate().map(|(index, link)| (self.depths[index], index, *link)).collect();
        entries.sort_by(|left, right| right.0.cmp(&left.0).then(right.1.cmp(&left.1)));
        entries.into_iter().map(|(_, _, link)| link).collect()
    }
}

/// `resolveThreadPullRequestChains`.
pub fn resolve_thread_pull_request_chains(links: &[ThreadLink]) -> Vec<ThreadPullRequestChain<'_>> {
    chains_of(&visible_thread_pull_requests(links))
}

fn chains_of<'a>(visible: &[&'a ThreadLink]) -> Vec<ThreadPullRequestChain<'a>> {
    let mut chains = Vec::new();
    let mut placed: HashSet<String> = HashSet::new();

    let mut native_stacks: Vec<(String, Vec<&'a ThreadLink>)> = Vec::new();
    for &link in visible {
        let Some(stack) = &link.stack else { continue };
        let key = format!("{}#stack:{}", repository_key(link), stack.id);
        match native_stacks.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, members)) => members.push(link),
            None => native_stacks.push((key, vec![link])),
        }
    }
    let stack_layers = |link: &'a ThreadLink| link.stack.as_ref().map_or(&[][..], |stack| stack.layers.as_slice());
    for (_, mut members) in native_stacks {
        // Saved snapshots can lag behind newer members of the same stack.
        let member_numbers: HashSet<u64> = members.iter().map(|member| member.number).collect();
        let coverage_of = |layers: &[StackLayer]| layers.iter().filter(|layer| member_numbers.contains(&layer.number)).count();
        let mut layers = stack_layers(members[0]);
        let mut coverage = coverage_of(layers);
        for &member in &members[1..] {
            let candidate = stack_layers(member);
            let candidate_coverage = coverage_of(candidate);
            if candidate_coverage > coverage || (candidate_coverage == coverage && candidate.len() > layers.len()) {
                layers = candidate;
                coverage = candidate_coverage;
            }
        }
        let order: HashMap<u64, usize> = layers.iter().enumerate().map(|(index, layer)| (layer.number, index)).collect();
        members.sort_by_key(|member| order.get(&member.number).copied().unwrap_or(layers.len()));
        placed.extend(members.iter().map(|member| identity(member)));
        chains.push(ThreadPullRequestChain { kind: ChainKind::Native, depths: (0..members.len()).collect(), layers: members });
    }

    let remaining: Vec<&'a ThreadLink> = visible.iter().copied().filter(|link| !placed.contains(&identity(link))).collect();
    let branch_key = |link: &ThreadLink, branch: &str| format!("{}:{branch}", repository_key(link));
    // A head branch two links share names no parent.
    let mut by_head: HashMap<String, Option<&'a ThreadLink>> = HashMap::new();
    for &link in &remaining {
        let Some(snapshot) = &link.snapshot else { continue };
        let key = branch_key(link, &snapshot.head_branch);
        let shared = by_head.contains_key(&key);
        by_head.insert(key, if shared { None } else { Some(link) });
    }
    let parent_of = |link: &'a ThreadLink| -> Option<&'a ThreadLink> {
        let snapshot = link.snapshot.as_ref()?;
        let parent = (*by_head.get(&branch_key(link, &snapshot.base_branch))?)?;
        (!std::ptr::eq(parent, link)).then_some(parent)
    };
    let mut children: HashMap<String, Vec<&'a ThreadLink>> = HashMap::new();
    for &link in &remaining {
        if let Some(parent) = parent_of(link) {
            children.entry(identity(parent)).or_default().push(link);
        }
    }
    for &base in &remaining {
        if parent_of(base).is_some() {
            continue;
        }
        let (mut layers, mut depths) = (Vec::new(), Vec::new());
        let mut pending = vec![(base, 0)];
        while let Some((link, depth)) = pending.pop() {
            let key = identity(link);
            // Two links with one identity would otherwise share a child list and expand forever.
            if !placed.insert(key.clone()) {
                continue;
            }
            layers.push(link);
            depths.push(depth);
            if let Some(below) = children.get(&key) {
                pending.extend(below.iter().rev().map(|child| (*child, depth + 1)));
            }
        }
        if !layers.is_empty() {
            chains.push(ThreadPullRequestChain { kind: ChainKind::Derived, layers, depths });
        }
    }
    for &link in &remaining {
        if !placed.contains(&identity(link)) {
            chains.push(ThreadPullRequestChain { kind: ChainKind::Derived, layers: vec![link], depths: vec![0] });
        }
    }
    chains
}

/// `resolveThreadCurrentPullRequest`.
pub fn resolve_thread_current_pull_request(links: &[ThreadLink]) -> Option<ThreadCurrentPullRequest<'_>> {
    let visible = visible_thread_pull_requests(links);
    let open: Vec<&ThreadLink> = visible.iter().copied().filter(|link| is_open(link)).collect();
    if let [only] = open[..] {
        return Some(ThreadCurrentPullRequest::Single(only));
    }
    let chains = chains_of(&visible);
    if open.len() > 1 {
        let mut open_chains: Vec<Vec<&ThreadLink>> =
            chains.iter().map(|chain| chain.layers_from_top().into_iter().filter(|link| is_open(link)).collect::<Vec<_>>()).filter(|layers| !layers.is_empty()).collect();
        let ordered = if open_chains.len() == 1 {
            open_chains.remove(0)
        } else {
            let newest_linked_at = |layers: &Vec<&ThreadLink>| layers.iter().map(|link| parse_iso_ms(&link.linked_at).unwrap_or(0)).max().unwrap_or(0);
            open_chains.sort_by_key(|layers| Reverse(newest_linked_at(layers)));
            open_chains.concat()
        };
        let top = *ordered.first()?;
        return Some(ThreadCurrentPullRequest::Stack { open: ordered, top });
    }
    if let [chain] = &chains[..] {
        return chain.layers_from_top().first().map(|link| ThreadCurrentPullRequest::Single(link));
    }
    let mut latest = *visible.first()?;
    let mut latest_timestamp = latest_updated_at(latest);
    for &link in &visible[1..] {
        let timestamp = latest_updated_at(link);
        if timestamp > latest_timestamp {
            latest = link;
            latest_timestamp = timestamp;
        }
    }
    Some(ThreadCurrentPullRequest::Single(latest))
}

/// `resolveThreadCurrentPullRequestLink`.
pub fn resolve_thread_current_pull_request_link(links: &[ThreadLink]) -> Option<&ThreadLink> {
    resolve_thread_current_pull_request(links).map(|current| current.link())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeState {
    Open,
    Closed,
    Merged,
    Draft,
}

/// `ThreadPullRequestBadge`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPullRequestBadge {
    Stack { layers: usize, state: BadgeState },
    PullRequest { others: usize, state: BadgeState },
}

impl ThreadPullRequestBadge {
    pub fn state(&self) -> BadgeState {
        match self {
            ThreadPullRequestBadge::Stack { state, .. } | ThreadPullRequestBadge::PullRequest { state, .. } => *state,
        }
    }

    /// Whether the badge stands for several pull requests, so clicking it opens the list.
    pub fn opens_list(&self) -> bool {
        matches!(self, ThreadPullRequestBadge::Stack { .. } | ThreadPullRequestBadge::PullRequest { others: 1.., .. })
    }
}

/// `resolveThreadPullRequestBadge`.
pub fn resolve_thread_pull_request_badge(links: &[ThreadLink]) -> Option<ThreadPullRequestBadge> {
    let visible = visible_thread_pull_requests(links);
    if visible.is_empty() {
        return None;
    }
    let states: Vec<PrState> = visible.iter().map(|link| link.snapshot.as_ref().map_or(PrState::Open, |snapshot| snapshot.state)).collect();
    let state = if visible.iter().all(|link| link.snapshot.as_ref().is_some_and(|snapshot| snapshot.state == PrState::Open && snapshot.is_draft)) {
        BadgeState::Draft
    } else if states.contains(&PrState::Open) {
        BadgeState::Open
    } else if states.iter().all(|state| *state == PrState::Merged) {
        BadgeState::Merged
    } else {
        BadgeState::Closed
    };
    if visible.len() > 1 && chains_of(&visible).len() == 1 {
        return Some(ThreadPullRequestBadge::Stack { layers: visible.len(), state });
    }
    Some(ThreadPullRequestBadge::PullRequest { others: visible.len() - 1, state })
}

/// `PullRequestGlyph`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrGlyph {
    PullRequest,
    Draft,
    Closed,
    Merged,
    Stack,
}

/// What the app colors a glyph with: a state's tone, or muted while the status is pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrTone {
    Open,
    Draft,
    Closed,
    Merged,
    Muted,
}

impl BadgeState {
    /// `resolvePullRequestState`'s key. A state this client does not know reads as open.
    pub fn of(state: PrState, is_draft: bool) -> BadgeState {
        match state {
            PrState::Closed => BadgeState::Closed,
            PrState::Merged => BadgeState::Merged,
            PrState::Open | PrState::Unknown if is_draft => BadgeState::Draft,
            PrState::Open | PrState::Unknown => BadgeState::Open,
        }
    }

    /// `PULL_REQUEST_STATE_PRESENTATION`.
    pub fn label(self) -> &'static str {
        match self {
            BadgeState::Open => "Open",
            BadgeState::Draft => "Draft",
            BadgeState::Closed => "Closed",
            BadgeState::Merged => "Merged",
        }
    }

    pub fn tone(self) -> PrTone {
        match self {
            BadgeState::Open => PrTone::Open,
            BadgeState::Draft => PrTone::Draft,
            BadgeState::Closed => PrTone::Closed,
            BadgeState::Merged => PrTone::Merged,
        }
    }

    pub fn glyph(self) -> PrGlyph {
        match self {
            BadgeState::Open => PrGlyph::PullRequest,
            BadgeState::Draft => PrGlyph::Draft,
            BadgeState::Closed => PrGlyph::Closed,
            BadgeState::Merged => PrGlyph::Merged,
        }
    }
}

/// A host's `changeRequest` terminology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeRequestTerms {
    pub short_label: &'static str,
    pub plural: &'static str,
}

/// `sourceControlClients.get(kind).changeRequest`: no kind means the first host, GitHub.
pub fn terms_for_provider(kind: Option<&str>) -> ChangeRequestTerms {
    match kind {
        None | Some("github" | "bitbucket" | "azure-devops" | "forgejo" | "gitcafe") => ChangeRequestTerms { short_label: "PR", plural: "pull requests" },
        Some("gitlab") => ChangeRequestTerms { short_label: "MR", plural: "merge requests" },
        Some(_) => ChangeRequestTerms { short_label: "change request", plural: "change requests" },
    }
}

/// `changeRequestTerminologyForUrl`.
pub fn terms_for_url(url: Option<&str>) -> ChangeRequestTerms {
    terms_for_provider(url.and_then(change_request_provider))
}

/// The `pr` of a thread's `VcsStatusResult`, the fields the badge reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPr {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PrState,
    pub is_draft: bool,
}

/// `PrStatusIndicator`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatusIndicator {
    pub label: String,
    pub tone: PrTone,
    pub glyph: PrGlyph,
    pub tooltip: String,
    pub tooltip_lead: String,
    pub tooltip_title: String,
    pub url: String,
}

/// `prStatusIndicator`, for a host kind such as `"github"`.
pub fn pr_status_indicator(pr: &ThreadPr, provider: Option<&str>) -> PrStatusIndicator {
    let short_label = terms_for_provider(provider).short_label;
    let state = BadgeState::of(pr.state, pr.is_draft);
    let tooltip_lead = format!("{short_label} #{} - {}", pr.number, state.label());
    PrStatusIndicator {
        label: format!("{short_label} {}", state.label().to_lowercase()),
        tone: state.tone(),
        glyph: state.glyph(),
        tooltip: format!("{tooltip_lead}: {}", pr.title),
        tooltip_lead,
        tooltip_title: pr.title.clone(),
        url: pr.url.clone(),
    }
}

/// `LinkedThreadPullRequestStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedPullRequestStatus {
    pub pr: ThreadPr,
    pub provider: &'static str,
}

/// `linkedPullRequestSnapshotStatus`: `None` until the link has been synced.
pub fn linked_snapshot_status(link: &ThreadLink) -> Option<LinkedPullRequestStatus> {
    let snapshot = link.snapshot.as_ref()?;
    Some(LinkedPullRequestStatus {
        pr: ThreadPr { number: link.number, title: snapshot.title.clone(), url: link.url.clone(), state: snapshot.state, is_draft: snapshot.is_draft },
        provider: change_request_provider(&link.url).unwrap_or("github"),
    })
}

/// `ThreadPullRequestBadgePresentation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadgePresentation {
    pub glyph: PrGlyph,
    pub tone: PrTone,
    pub label: String,
    pub text: String,
}

/// `resolveThreadPullRequestBadgePresentation`: `number` and `url` name the pull request the row
/// shows, `status` its indicator once read.
pub fn resolve_badge_presentation(badge: Option<&ThreadPullRequestBadge>, number: Option<u64>, url: Option<&str>, status: Option<&PrStatusIndicator>) -> Option<BadgePresentation> {
    let terms = terms_for_url(url);
    if let Some(ThreadPullRequestBadge::Stack { layers, state }) = badge {
        return Some(BadgePresentation {
            glyph: PrGlyph::Stack,
            tone: state.tone(),
            label: format!("Stack of {layers} {}, {}", terms.plural, state.label().to_lowercase()),
            text: layers.to_string(),
        });
    }
    let number = number.filter(|_| url.is_some())?;
    let tooltip = status.map_or_else(|| format!("{} #{number}, status pending", terms.short_label), |status| status.tooltip.clone());
    if let Some(ThreadPullRequestBadge::PullRequest { others: others @ 1.., state }) = badge {
        return Some(BadgePresentation {
            glyph: state.glyph(),
            tone: state.tone(),
            label: format!("{tooltip}, and {others} more linked; overall {}", state.label().to_lowercase()),
            text: format!("+{}", others + 1),
        });
    }
    Some(BadgePresentation {
        glyph: status.map_or(PrGlyph::PullRequest, |status| status.glyph),
        tone: status.map_or(PrTone::Muted, |status| status.tone),
        label: tooltip,
        text: number.to_string(),
    })
}

#[cfg(test)]
#[path = "thread_badge_tests.rs"]
mod tests;
