//! The pull request list: filtering, grouping, ranking and quick actions, ported from
//! `apps/web/src/components/pullRequest/pullRequestList.logic.ts` and
//! `pullRequestListGroups.logic.ts`. One environment, so keys carry no environment scope.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::query::{ListFilters, ReviewFilter};
use super::wire::{Action, Actor, ChecksState, Label, ListEntry, Mergeability, PrState, ProviderSummary, ReviewDecision};
use crate::timeline::parse_iso_ms;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Involvement {
    #[default]
    All,
    Reviewing,
    Authored,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListState {
    All,
    #[default]
    Open,
    Closed,
    Merged,
}

/// `PullRequestListSort`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sort {
    #[default]
    Ready,
    Blocked,
    Updated,
    Newest,
    Oldest,
    Largest,
    Smallest,
}

impl Sort {
    pub const ALL: [Sort; 7] = [Sort::Ready, Sort::Blocked, Sort::Updated, Sort::Newest, Sort::Oldest, Sort::Largest, Sort::Smallest];

    /// `SORT_OPTIONS`.
    pub fn label(self) -> &'static str {
        match self {
            Sort::Ready => "Merge readiness",
            Sort::Blocked => "Blocked on me",
            Sort::Updated => "Recently updated",
            Sort::Newest => "Newest shown",
            Sort::Oldest => "Oldest shown",
            Sort::Largest => "Largest shown",
            Sort::Smallest => "Smallest shown",
        }
    }
}

impl Involvement {
    pub const ALL: [Involvement; 3] = [Involvement::All, Involvement::Reviewing, Involvement::Authored];

    pub fn label(self) -> &'static str {
        match self {
            Involvement::All => "All",
            Involvement::Reviewing => "Reviewing",
            Involvement::Authored => "Authored",
        }
    }
}

impl ListState {
    pub const ALL: [ListState; 4] = [ListState::All, ListState::Open, ListState::Closed, ListState::Merged];

    pub fn label(self) -> &'static str {
        match self {
            ListState::All => "All",
            ListState::Open => "Open",
            ListState::Closed => "Closed",
            ListState::Merged => "Merged",
        }
    }

    pub fn holds(self, state: PrState) -> bool {
        matches!(
            (self, state),
            (ListState::All, _) | (ListState::Open, PrState::Open) | (ListState::Closed, PrState::Closed) | (ListState::Merged, PrState::Merged)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupKey {
    ReviewRequested,
    Authored,
    Others,
}

impl GroupKey {
    pub fn label(self) -> &'static str {
        match self {
            GroupKey::ReviewRequested => "Review requested",
            GroupKey::Authored => "Authored",
            GroupKey::Others => "Others",
        }
    }
}

/// A group of rows, as indexes into the entries it was built from. `label` is empty when the
/// list is not grouped (an involvement filter is on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub key: GroupKey,
    pub label: &'static str,
    pub entries: Vec<usize>,
}

fn normalize(value: Option<&str>) -> Option<String> {
    value.map(|value| value.trim().to_lowercase()).filter(|value| !value.is_empty())
}

/// `pullRequestEntryKey`.
pub fn entry_key(entry: &ListEntry) -> String {
    format!("{}:{}#{}", entry.host, entry.repository, entry.number)
}

/// `pullRequestEntryViewer`: the signed-in login on the row's host, lowercased.
pub fn entry_viewer(entry: &ListEntry, viewers: &HashMap<String, String>) -> Option<String> {
    normalize(viewers.get(&entry.host).map(String::as_str))
}

fn authored_by_viewer(entry: &ListEntry, viewers: &HashMap<String, String>) -> bool {
    entry_viewer(entry, viewers).is_some_and(|viewer| normalize(entry.author.as_ref().map(|a| a.login.as_str())).as_deref() == Some(&viewer))
}

/// `matchesPullRequestQuery`.
pub fn matches_query(entry: &ListEntry, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let author = entry.author.as_ref().map_or("", |author| author.login.as_str());
    format!("#{} {} {} {} {author}", entry.number, entry.title, entry.repository, entry.head_branch).to_lowercase().contains(&needle)
}

/// `filterPullRequestsByInvolvement`.
pub fn matches_involvement(entry: &ListEntry, viewers: &HashMap<String, String>, involvement: Involvement) -> bool {
    match involvement {
        Involvement::All => true,
        Involvement::Reviewing => entry.viewer_review_requested,
        Involvement::Authored => authored_by_viewer(entry, viewers),
    }
}

/// `resolvePullRequestAuthorFilter`: `author:me` (or `@me`) names the signed-in viewer.
pub fn resolve_author_filter(author: &str, viewer: Option<&str>) -> String {
    let trimmed = author.trim();
    let me = trimmed.strip_prefix('@').unwrap_or(trimmed).eq_ignore_ascii_case("me");
    match viewer.map(str::trim).filter(|viewer| !viewer.is_empty()) {
        Some(viewer) if me => viewer.to_owned(),
        _ => trimmed.to_owned(),
    }
}

/// `matchesPullRequestFilters`. Checks are left to the host, as the web does.
pub fn matches_filters(entry: &ListEntry, filters: &ListFilters, viewer: Option<&str>) -> bool {
    let labels: Vec<String> = entry.labels.iter().map(|label| label.name.trim().to_lowercase()).collect();
    let holds = |label: &String| labels.contains(&label.trim().to_lowercase());
    let draft = filters.draft.is_none_or(|draft| entry.is_draft == (draft == super::query::DraftFilter::Only));
    let review = filters.review.is_none_or(|review| match review {
        ReviewFilter::None => entry.review_decision.is_none(),
        ReviewFilter::Approved => entry.review_decision == Some(ReviewDecision::Approved),
        ReviewFilter::ChangesRequested => entry.review_decision == Some(ReviewDecision::ChangesRequested),
        ReviewFilter::ReviewRequired => entry.review_decision == Some(ReviewDecision::ReviewRequired),
    });
    let included = filters.labels.iter().all(|group| group.iter().any(holds));
    let excluded = !filters.excluded_labels.iter().any(holds);
    let author = filters.author.as_ref().is_none_or(|author| {
        entry.author.as_ref().is_some_and(|actor| actor.login.to_lowercase() == resolve_author_filter(author, viewer).to_lowercase())
    });
    draft && review && included && excluded && author
}

/// `groupPullRequestsByInvolvement`: authored, then review requested, then the rest.
pub fn group_by_involvement(entries: &[ListEntry], rows: &[usize], viewers: &HashMap<String, String>) -> Vec<Group> {
    let mut buckets: [Vec<usize>; 3] = Default::default();
    for &index in rows {
        let entry = &entries[index];
        let bucket = if authored_by_viewer(entry, viewers) {
            0
        } else if entry.viewer_review_requested {
            1
        } else {
            2
        };
        buckets[bucket].push(index);
    }
    [GroupKey::Authored, GroupKey::ReviewRequested, GroupKey::Others]
        .into_iter()
        .zip(buckets)
        .filter(|(_, rows)| !rows.is_empty())
        .map(|(key, entries)| Group { key, label: key.label(), entries })
        .collect()
}

fn timestamp(value: &str) -> Option<i64> {
    parse_iso_ms(value)
}

/// `comparePullRequestsByRecency`: newest update first, undated rows last.
pub fn compare_by_recency(left: &ListEntry, right: &ListEntry) -> Ordering {
    match (timestamp(&left.updated_at), timestamp(&right.updated_at)) {
        (Some(left), Some(right)) => right.cmp(&left),
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

/// `scorePullRequestMatch`: host search also matches bodies and commits, which score lowest.
pub fn score_match(entry: &ListEntry, query: &str) -> u32 {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return 0;
    }
    let number = needle.strip_prefix('#').unwrap_or(&needle);
    if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
        return if entry.number.to_string() == number { 100 } else { 0 };
    }
    let title = entry.title.to_lowercase();
    let terms: Vec<&str> = needle.split_whitespace().collect();
    if title == needle {
        90
    } else if title.contains(&needle) {
        80
    } else if terms.len() > 1 && terms.iter().all(|term| title.contains(term)) {
        70
    } else if entry.head_branch.to_lowercase().contains(&needle) {
        60
    } else if entry.author.as_ref().is_some_and(|author| author.login.to_lowercase().contains(&needle)) {
        50
    } else if entry.repository.to_lowercase().contains(&needle) {
        40
    } else if terms.iter().any(|term| title.contains(term)) {
        30
    } else {
        10
    }
}

/// Rows a search found only in fields the row does not show (`MATCHED_ELSEWHERE_SCORE`).
pub fn matched_elsewhere(entry: &ListEntry, query: &str) -> bool {
    !query.is_empty() && score_match(entry, query) <= 10
}

fn measured(entry: &ListEntry) -> bool {
    entry.additions + entry.deletions > 0
}

fn size(entry: &ListEntry) -> i64 {
    (entry.additions + entry.deletions) as i64
}

fn readiness_tier(entry: &ListEntry) -> u8 {
    if entry.mergeability == Mergeability::Conflicting {
        4
    } else if entry.state != PrState::Open {
        3
    } else if entry.is_draft {
        2
    } else if entry.checks_state == Some(ChecksState::Passing) && entry.review_decision == Some(ReviewDecision::Approved) {
        0
    } else if entry.checks_state == Some(ChecksState::Passing) {
        1
    } else {
        2
    }
}

fn blocked_on_author_tier(entry: &ListEntry) -> u8 {
    if entry.state != PrState::Open {
        6
    } else if entry.mergeability == Mergeability::Conflicting {
        0
    } else if entry.review_decision == Some(ReviewDecision::ChangesRequested) {
        1
    } else if entry.checks_state == Some(ChecksState::Failing) {
        2
    } else if entry.is_draft {
        3
    } else if entry.checks_state == Some(ChecksState::Passing) && entry.review_decision == Some(ReviewDecision::Approved) {
        5
    } else {
        4
    }
}

fn updated_or_created(entry: &ListEntry) -> i64 {
    timestamp(&entry.updated_at).or_else(|| timestamp(&entry.created_at)).unwrap_or(0)
}

/// `sortPullRequestGroups`. Searches keep the host's relevance order for the ranked sorts.
pub fn sort_groups(entries: &[ListEntry], groups: &mut [Group], sort: Sort, search_text: &str, involvement: Involvement) {
    let searching = !search_text.trim().is_empty();
    let by_tier = |tier: fn(&ListEntry) -> u8| {
        move |left: &usize, right: &usize| {
            let (left, right) = (&entries[*left], &entries[*right]);
            tier(left).cmp(&tier(right)).then_with(|| compare_by_recency(left, right))
        }
    };
    for group in groups.iter_mut() {
        match sort {
            Sort::Ready if searching => {}
            Sort::Ready => group.entries.sort_by(|left, right| {
                let (left, right) = (&entries[*left], &entries[*right]);
                readiness_tier(left)
                    .cmp(&readiness_tier(right))
                    .then_with(|| measured(right).cmp(&measured(left)))
                    .then_with(|| size(left).cmp(&size(right)))
                    .then_with(|| compare_by_recency(left, right))
            }),
            Sort::Blocked if searching => {}
            Sort::Blocked => {
                let role = match group.key {
                    GroupKey::Others => involvement,
                    GroupKey::Authored => Involvement::Authored,
                    GroupKey::ReviewRequested => Involvement::Reviewing,
                };
                match role {
                    Involvement::All => {}
                    Involvement::Authored => group.entries.sort_by(by_tier(blocked_on_author_tier)),
                    Involvement::Reviewing => group.entries.sort_by(by_tier(|entry| u8::from(entry.state != PrState::Open))),
                }
            }
            Sort::Updated => {}
            Sort::Newest | Sort::Oldest => group.entries.sort_by(|left, right| {
                let (left, right) = (&entries[*left], &entries[*right]);
                let (lc, rc) = (timestamp(&left.created_at), timestamp(&right.created_at));
                let dated = lc.unwrap_or(0).cmp(&rc.unwrap_or(0));
                rc.is_some()
                    .cmp(&lc.is_some())
                    .then(if sort == Sort::Newest { dated.reverse() } else { dated })
                    .then_with(|| updated_or_created(right).cmp(&updated_or_created(left)))
            }),
            Sort::Largest | Sort::Smallest => group.entries.sort_by(|left, right| {
                let (left, right) = (&entries[*left], &entries[*right]);
                let sized = size(left).cmp(&size(right));
                measured(right)
                    .cmp(&measured(left))
                    .then(if sort == Sort::Largest { sized.reverse() } else { sized })
                    .then_with(|| updated_or_created(right).cmp(&updated_or_created(left)))
            }),
        }
    }
}

/// The rows shown, as `pullRequestGroups` and `usePullRequestDisplayGroups` build them: the
/// involvement and local filters applied, text matched locally unless the host searched it,
/// grouped when involvement is "all", then sorted.
pub struct Shown<'a> {
    pub entries: &'a [ListEntry],
    pub viewers: &'a HashMap<String, String>,
    pub providers: &'a [ProviderSummary],
    pub involvement: Involvement,
    pub filters: &'a ListFilters,
    pub text: &'a str,
    /// The host answered this text, so its matches stand even where no row field shows it.
    pub answered: bool,
    pub sort: Sort,
    /// The listing's state: a row an action moved out of it leaves (`applyPullRequestOverrides`).
    pub state: ListState,
}

pub fn shown_groups(input: &Shown) -> Vec<Group> {
    let searching: HashSet<&str> = input.providers.iter().filter(|p| p.searches_on_host).map(|p| p.host.as_str()).collect();
    let mut rows: Vec<usize> = (0..input.entries.len())
        .filter(|index| {
            let entry = &input.entries[*index];
            input.state.holds(entry.state)
                && matches_involvement(entry, input.viewers, input.involvement)
                && (input.filters.is_empty() || matches_filters(entry, input.filters, entry_viewer(entry, input.viewers).as_deref()))
                && (input.text.is_empty() || (input.answered && searching.contains(entry.host.as_str())) || matches_query(entry, input.text))
        })
        .collect();
    if input.text.is_empty() {
        rows.sort_by(|left, right| compare_by_recency(&input.entries[*left], &input.entries[*right]));
    }
    let mut groups = if input.involvement == Involvement::All {
        group_by_involvement(input.entries, &rows, input.viewers)
    } else {
        vec![Group { key: GroupKey::Others, label: "", entries: rows }]
    };
    sort_groups(input.entries, &mut groups, input.sort, input.text, input.involvement);
    groups
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuickAction {
    Close,
    Merge,
    Ready,
    Reopen,
}

impl QuickAction {
    pub fn action(self) -> Action {
        match self {
            QuickAction::Close => Action::Close,
            QuickAction::Merge => Action::Merge,
            QuickAction::Ready => Action::Ready,
            QuickAction::Reopen => Action::Reopen,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            QuickAction::Close => "Close",
            QuickAction::Merge => "Merge",
            QuickAction::Ready => "Ready for review",
            QuickAction::Reopen => "Reopen",
        }
    }
}

/// `pullRequestHostActions`: what each host says it can do from a list, else what its kind could
/// before hosts reported it (GitHub's `changeRequestActionsWhenUnreported`).
pub fn host_actions(providers: &[ProviderSummary], entry: &ListEntry) -> HashSet<Action> {
    if let Some(actions) = providers.iter().find(|p| p.host.eq_ignore_ascii_case(&entry.host)).and_then(|p| p.actions.as_ref()) {
        return actions.iter().copied().collect();
    }
    match entry.provider.as_str() {
        "github" => [Action::Close, Action::Merge, Action::Ready, Action::Reopen].into_iter().collect(),
        _ => HashSet::new(),
    }
}

/// `pullRequestQuickActions`.
pub fn quick_actions(state: PrState, is_draft: bool, host: &HashSet<Action>) -> Vec<QuickAction> {
    let for_state: &[QuickAction] = match state {
        PrState::Merged | PrState::Unknown => &[],
        PrState::Closed => &[QuickAction::Reopen],
        PrState::Open if is_draft => &[QuickAction::Close, QuickAction::Ready],
        PrState::Open => &[QuickAction::Close, QuickAction::Merge],
    };
    for_state.iter().copied().filter(|action| host.contains(&action.action())).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorFacet {
    pub actor: Actor,
    pub count: usize,
    pub merged: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelFacet {
    pub label: Label,
    pub count: usize,
}

/// `collectPullRequestListFacets`: authors by merged then total count, labels by count.
pub fn facets(entries: &[ListEntry], state: ListState) -> (Vec<AuthorFacet>, Vec<LabelFacet>) {
    let mut authors: Vec<AuthorFacet> = Vec::new();
    let mut author_index: HashMap<String, usize> = HashMap::new();
    let mut labels: Vec<LabelFacet> = Vec::new();
    let mut label_index: HashMap<String, usize> = HashMap::new();
    let mut seen = HashSet::new();
    for entry in entries {
        if !seen.insert(entry_key(entry)) {
            continue;
        }
        let in_state = state.holds(entry.state);
        if let Some(actor) = &entry.author
            && let Some(key) = normalize(Some(&actor.login))
        {
            let index = *author_index.entry(key).or_insert_with(|| {
                authors.push(AuthorFacet { actor: actor.clone(), count: 0, merged: 0 });
                authors.len() - 1
            });
            authors[index].count += usize::from(in_state);
            authors[index].merged += usize::from(entry.state == PrState::Merged);
        }
        if !in_state {
            continue;
        }
        for label in &entry.labels {
            let Some(key) = normalize(Some(&label.name)) else { continue };
            let index = *label_index.entry(key).or_insert_with(|| {
                labels.push(LabelFacet { label: label.clone(), count: 0 });
                labels.len() - 1
            });
            labels[index].count += 1;
        }
    }
    authors.retain(|author| author.count > 0);
    authors.sort_by(|l, r| r.merged.cmp(&l.merged).then(r.count.cmp(&l.count)).then_with(|| l.actor.login.cmp(&r.actor.login)));
    labels.sort_by(|l, r| r.count.cmp(&l.count).then_with(|| l.label.name.cmp(&r.label.name)));
    (authors, labels)
}

/// `pullRequestLabelColor`: a six-digit hex, or none.
pub fn label_color(color: Option<&str>) -> Option<u32> {
    let hex = color?.trim().trim_start_matches('#');
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| u32::from_str_radix(hex, 16).ok()).flatten()
}

/// `withDiffStat`: GitHub listings omit line counts, which `pullRequests.listStats` fills in.
pub fn needs_stats(entry: &ListEntry) -> bool {
    entry.additions == 0 && entry.deletions == 0
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
