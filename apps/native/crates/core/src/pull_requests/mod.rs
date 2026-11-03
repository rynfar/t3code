//! Pull requests: wire types, the list's search syntax and ranking, the detail panel's derived
//! state, and thread links. Ports `packages/client-runtime/src/pullRequestQuery.ts`,
//! `apps/web/src/components/pullRequest/pullRequestList.logic.ts`,
//! `pullRequestListPreferences.ts`, `pullRequestDetail.logic.ts` and
//! `client-runtime/src/pullRequestLinkInput.ts` and `changeRequestLinks.ts`.

pub mod detail;
pub mod link;
pub mod target;
pub mod list;
pub mod query;
pub mod wire;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use list::{Involvement, ListState, Sort};
use query::{ChecksFilter, DraftFilter, ListFilters, ReviewFilter, truncate_utf16};
use wire::{Action, ListEntry, MergeMethod, PrState, Stack};

pub mod method {
    pub const LIST: &str = "pullRequests.list";
    pub const LIST_STATS: &str = "pullRequests.listStats";
    pub const FILTER_OPTIONS: &str = "pullRequests.filterOptions";
    pub const DETAIL: &str = "pullRequests.detail";
    pub const CHECKS: &str = "pullRequests.checks";
    pub const ACTIVITY: &str = "pullRequests.activity";
    pub const RUN_ACTION: &str = "pullRequests.runAction";
    pub const INVALIDATE: &str = "pullRequests.invalidate";
    pub const STACK: &str = "pullRequests.stack";
    pub const SUBSCRIBE_REFRESHES: &str = "pullRequests.subscribeRefreshes";
}

/// `PAGE_SIZE` and `MAX_PAGE_SIZE` on the page.
pub const PAGE_SIZE: u32 = 99;
pub const MAX_PAGE_SIZE: u32 = 500;
/// `MAX_PULL_REQUEST_STATS_REFS`.
pub const MAX_STATS_REFS: usize = 500;
/// `SEARCH_DEBOUNCE_MS`.
pub const SEARCH_DEBOUNCE_MS: u64 = 250;

/// `PullRequestRef`: which pull request, read through which project.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrRef {
    pub project_id: String,
    pub host: Option<String>,
    pub repository: String,
    pub number: u64,
}

impl PrRef {
    pub fn of(entry: &ListEntry) -> PrRef {
        PrRef { project_id: entry.project_id.clone(), host: Some(entry.host.clone()), repository: entry.repository.clone(), number: entry.number }
    }

    pub fn payload(&self) -> Value {
        let mut payload = json!({ "projectId": self.project_id, "repository": self.repository, "number": self.number });
        if let Some(host) = &self.host {
            payload["host"] = json!(host);
        }
        payload
    }

    /// `pullRequestSurfaceId`-like identity, with the repository lowercased.
    pub fn key(&self) -> String {
        format!("{}:{}#{}", self.host.as_deref().unwrap_or(""), self.repository.to_lowercase(), self.number)
    }
}

/// `PullRequestListPreferences`, persisted under the web's key name. Defaults: open, all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ListPreferences {
    pub involvement: Involvement,
    pub state: ListState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<ChecksFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    pub sort: Sort,
    /// The last merge method picked (`uiState.pullRequestMergeMethod`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_method: Option<MergeMethod>,
}

pub const PREFERENCES_KEY: &str = "t3.pullRequests.preferences";

impl ListPreferences {
    /// What the Filters menu sets (`menuFilters`): labels each in their own group.
    pub fn menu_filters(&self) -> ListFilters {
        ListFilters {
            draft: self.draft,
            review: self.review,
            checks: self.checks,
            author: self.author.clone(),
            labels: self.labels.iter().map(|label| vec![label.clone()]).collect(),
            excluded_labels: Vec::new(),
        }
    }

    pub fn set_menu_filters(&mut self, filters: &ListFilters) {
        self.draft = filters.draft;
        self.review = filters.review;
        self.checks = filters.checks;
        self.author = filters.author.clone();
        self.labels = filters.labels.iter().flatten().take(10).cloned().collect();
    }

    /// The Filters button's badge count (`filterCount`).
    pub fn filter_count(&self) -> usize {
        [self.state != ListState::Open, self.involvement != Involvement::All, self.host.is_some(), self.project_id.is_some()]
            .into_iter()
            .chain([self.draft.is_some(), self.review.is_some(), self.checks.is_some(), self.author.is_some()])
            .filter(|set| *set)
            .count()
            + self.labels.len()
    }

    /// Whether anything narrows the list, for the empty state's copy.
    pub fn filtered(&self) -> bool {
        !self.menu_filters().is_empty() || self.state != ListState::Open || self.involvement != Involvement::All || self.project_id.is_some() || self.host.is_some()
    }
}

/// The `pullRequests.list` input for these preferences and the sent query.
pub fn list_input(preferences: &ListPreferences, sent_query: &str, limit: u32, cursors: Option<&std::collections::HashMap<String, String>>) -> Value {
    let parsed = query::parse_query(sent_query);
    let filters = preferences.menu_filters().overlay(&parsed.filters);
    let mut input = Map::new();
    input.insert("state".into(), json!(preferences.state));
    input.insert("involvement".into(), json!(preferences.involvement));
    input.insert("limit".into(), json!(limit));
    if let Some(project) = &preferences.project_id {
        input.insert("projectId".into(), json!(project));
    }
    if let Some(host) = &preferences.host {
        input.insert("host".into(), json!(host));
    }
    if !filters.is_empty() {
        input.insert("filters".into(), json!(filters));
    }
    if !parsed.text.is_empty() {
        input.insert("query".into(), json!(truncate_utf16(&parsed.text, 200).trim_end()));
    }
    if let Some(cursors) = cursors.filter(|cursors| !cursors.is_empty()) {
        input.insert("cursors".into(), json!(cursors));
    }
    Value::Object(input)
}

/// `pullRequests.listStats` inputs for the rows still without line counts, in batches the
/// contract accepts (`pullRequestStatsBatches`).
pub fn stats_inputs<'a>(entries: impl IntoIterator<Item = &'a ListEntry>) -> Vec<Value> {
    let refs: Vec<Value> = entries.into_iter().filter(|entry| list::needs_stats(entry)).map(|entry| PrRef::of(entry).payload()).collect();
    refs.chunks(MAX_STATS_REFS).map(|batch| json!({ "refs": batch })).collect()
}

/// `pullRequests.invalidate`: one pull request's host reads, or every read when `None`.
pub fn invalidate_input(reference: Option<&PrRef>) -> Value {
    match reference {
        Some(reference) => json!({ "reference": reference.payload() }),
        None => json!({}),
    }
}

/// The project's merge method setting (`usePullRequestDefaultMergeMethodResolver`): its
/// override when it has one, null included, else the environment's.
pub fn project_default_merge_method(settings: &crate::server_config::ServerSettings, project_id: &str) -> Option<MergeMethod> {
    match settings.project_settings_overrides.get(project_id).and_then(|overrides| overrides.pull_request_merge_method) {
        Some(method) => method,
        None => settings.pull_request_merge_method,
    }
}

/// A stack-scoped merge (`PullRequestStackMenu`): this layer and the unmerged ones below it.
#[derive(Debug, Clone, PartialEq)]
pub struct StackMerge {
    pub count: usize,
    pub input: Value,
}

/// `None` when the host could not take it: this layer is not open, or a layer below is closed,
/// a draft, or has no known head to pin.
pub fn stack_merge(stack: &Stack, reference: &PrRef, method: MergeMethod) -> Option<StackMerge> {
    let position = stack.layers.iter().position(|layer| layer.number == reference.number)?;
    if stack.layers[position].state != PrState::Open {
        return None;
    }
    let layers: Vec<_> = stack.layers[..=position].iter().filter(|layer| layer.state != PrState::Merged).collect();
    if layers.iter().any(|layer| layer.state != PrState::Open || layer.is_draft == Some(true)) {
        return None;
    }
    let heads = layers.iter().map(|layer| Some(json!({ "number": layer.number, "headSha": layer.head_sha.as_deref()? }))).collect::<Option<Vec<_>>>()?;
    let mut input = action_input(reference, Action::Merge, Some(method));
    input["stackNumber"] = json!(stack.number);
    input["expectedStackHeads"] = json!(heads);
    Some(StackMerge { count: layers.len(), input })
}

/// `PullRequestActionInput`.
pub fn action_input(reference: &PrRef, action: Action, merge_method: Option<MergeMethod>) -> Value {
    let mut payload = reference.payload();
    payload["action"] = json!(action.as_str());
    if let Some(method) = merge_method.filter(|_| action == Action::Merge || action == Action::EnableAutoMerge) {
        payload["mergeMethod"] = json!(method.as_str());
    }
    payload
}

/// What the page says for a failed pull request read, from the error's tag and fields
/// (`PullRequestUnavailableError.message`, `PullRequestOperationError.message`). Tagged errors
/// cross the wire without their message, so it is rebuilt here.
pub fn failure_message(tag: Option<&str>, error: &Value, fallback: &str) -> String {
    let text = |key: &str| error.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty());
    match tag {
        Some("PullRequestUnavailableError") => match text("reason") {
            Some("cli-missing") => text("requirement").unwrap_or("The tool this host is read through is not installed or set up.").to_owned(),
            Some("cli-unauthenticated") => text("requirement").unwrap_or("This host has no working credentials.").to_owned(),
            _ => "Change requests cannot be browsed for this project's host yet.".to_owned(),
        },
        Some("PullRequestOperationError") => match (text("operation"), text("detail")) {
            (Some(operation), Some(detail)) => format!("Pull request operation {operation} failed: {detail}"),
            (_, Some(detail)) => detail.to_owned(),
            _ => fallback.to_owned(),
        },
        Some("EnvironmentAuthorizationError") => "This device is not allowed to do that on this server.".to_owned(),
        _ => text("message").unwrap_or(fallback).to_owned(),
    }
}

/// `actionSuccessLabels`: a merge is only requested, since some hosts accept one before it lands.
pub fn action_done_label(action: Action, term: &str) -> String {
    let capitalized = detail::capitalize(term);
    match action {
        Action::Merge => "Merge requested".to_owned(),
        Action::Close => format!("{capitalized} closed"),
        Action::Reopen => format!("{capitalized} reopened"),
        Action::Ready => "Marked ready for review".to_owned(),
        Action::Draft => "Converted to draft".to_owned(),
        Action::UpdateBranch => "Branch updated with the base branch".to_owned(),
        Action::EnableAutoMerge => "Auto-merge enabled".to_owned(),
        Action::DisableAutoMerge => "Auto-merge disabled".to_owned(),
        Action::Revert => format!("Revert {term} opened"),
        Action::DeleteBranch => "Branch deleted".to_owned(),
        Action::ApproveWorkflows => "Workflows approved".to_owned(),
        Action::Unknown => format!("{capitalized} updated"),
    }
}

/// `actionFailureLabels`.
pub fn action_failed_label(action: Action, term: &str) -> String {
    match action {
        Action::Merge => format!("Could not merge this {term}"),
        Action::Ready => "Could not mark this ready for review".to_owned(),
        Action::Draft => "Could not convert this to a draft".to_owned(),
        Action::Close => format!("Could not close this {term}"),
        Action::Reopen => format!("Could not reopen this {term}"),
        Action::UpdateBranch => "Could not update this branch".to_owned(),
        Action::EnableAutoMerge => "Could not enable auto-merge".to_owned(),
        Action::DisableAutoMerge => "Could not disable auto-merge".to_owned(),
        Action::Revert => format!("Could not open a revert {term}"),
        Action::DeleteBranch => "Could not delete this branch".to_owned(),
        Action::ApproveWorkflows => "Could not approve workflows".to_owned(),
        Action::Unknown => format!("Could not update this {term}"),
    }
}

/// `pullRequestActionNeedsHostRefresh`: what the host only shows after its reads are dropped.
pub fn action_needs_host_refresh(action: Action) -> bool {
    matches!(action, Action::UpdateBranch | Action::ApproveWorkflows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_input_overlays_typed_filters_on_menu_filters() {
        let preferences = ListPreferences { draft: Some(DraftFilter::Hide), labels: vec!["bug".into()], project_id: Some("p".into()), ..Default::default() };
        let input = list_input(&preferences, "crash label:ui review:approved", PAGE_SIZE, None);
        assert_eq!(input["state"], "open");
        assert_eq!(input["involvement"], "all");
        assert_eq!(input["limit"], 99);
        assert_eq!(input["projectId"], "p");
        assert_eq!(input["query"], "crash");
        assert_eq!(input["filters"], json!({ "draft": "hide", "review": "approved", "labels": [["ui"]] }));
        let bare = list_input(&ListPreferences::default(), "", PAGE_SIZE, None);
        assert!(bare.get("filters").is_none() && bare.get("query").is_none());
    }

    #[test]
    fn preferences_round_trip_and_count_filters() {
        let preferences = ListPreferences { state: ListState::Merged, labels: vec!["a".into(), "b".into()], sort: Sort::Largest, ..Default::default() };
        let decoded: ListPreferences = serde_json::from_value(serde_json::to_value(&preferences).unwrap()).unwrap();
        assert_eq!(decoded, preferences);
        assert_eq!(preferences.filter_count(), 3);
        let partial: ListPreferences = serde_json::from_value(json!({ "state": "closed", "unknown": 1 })).unwrap();
        assert_eq!((partial.state, partial.involvement), (ListState::Closed, Involvement::All));
    }

    #[test]
    fn failure_messages_rebuild_tagged_errors() {
        let unavailable = json!({ "_tag": "PullRequestUnavailableError", "reason": "cli-unauthenticated" });
        assert_eq!(failure_message(Some("PullRequestUnavailableError"), &unavailable, "x"), "This host has no working credentials.");
        let operation = json!({ "operation": "runAction", "detail": "Not mergeable" });
        assert_eq!(failure_message(Some("PullRequestOperationError"), &operation, "x"), "Pull request operation runAction failed: Not mergeable");
        assert_eq!(failure_message(None, &json!({}), "Could not load"), "Could not load");
    }

    fn entry(number: u64) -> ListEntry {
        serde_json::from_value(json!({
            "provider": "github", "host": "github.com", "projectId": "p", "projectTitle": "P", "repository": "a/b", "number": number,
            "title": "t", "url": "u", "author": null, "headBranch": "h", "baseBranch": "main", "state": "open", "isDraft": false,
            "mergeability": "mergeable", "additions": 0, "deletions": 0, "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
            "viewerReviewRequested": false, "labels": []
        }))
        .unwrap()
    }

    #[test]
    fn stats_are_read_in_batches_of_five_hundred() {
        let entries: Vec<ListEntry> = (1..=1203).map(entry).collect();
        let batches = stats_inputs(&entries);
        let sizes: Vec<usize> = batches.iter().map(|batch| batch["refs"].as_array().unwrap().len()).collect();
        assert_eq!(sizes, vec![500, 500, 203]);
        assert_eq!(batches[2]["refs"][202]["number"], 1203);
        assert!(stats_inputs(&[]).is_empty());
    }

    #[test]
    fn list_query_stays_within_the_contract_in_utf16_units() {
        let input = list_input(&ListPreferences::default(), &"😀".repeat(150), PAGE_SIZE, None);
        assert_eq!(input["query"].as_str().map(|query| query.encode_utf16().count()), Some(200));
    }

    #[test]
    fn project_default_merge_method_reads_the_override_then_the_environment() {
        let settings = |value: Value| -> crate::server_config::ServerSettings { serde_json::from_value(value).unwrap() };
        let environment = settings(json!({ "pullRequestMergeMethod": "rebase" }));
        assert_eq!(project_default_merge_method(&environment, "p"), Some(MergeMethod::Rebase));
        let overridden = settings(json!({ "pullRequestMergeMethod": "rebase", "projectSettingsOverrides": { "p": { "pullRequestMergeMethod": "squash" }, "q": { "pullRequestMergeMethod": null } } }));
        assert_eq!(project_default_merge_method(&overridden, "p"), Some(MergeMethod::Squash));
        assert_eq!(project_default_merge_method(&overridden, "q"), None);
        assert_eq!(project_default_merge_method(&overridden, "r"), Some(MergeMethod::Rebase));
        assert_eq!(project_default_merge_method(&settings(json!({})), "p"), None);
    }

    #[test]
    fn stack_merge_scopes_the_layers_through_this_one() {
        let reference = PrRef { project_id: "p".into(), host: Some("git.cafe".into()), repository: "acme/app".into(), number: 3 };
        let layer = |number: u64, state: &str, sha: Option<&str>| json!({ "number": number, "headBranch": format!("b{number}"), "state": state, "headSha": sha });
        let stack = |layers: Vec<Value>| -> Stack { serde_json::from_value(json!({ "id": "s", "number": 9, "url": "u", "base": "main", "layers": layers })).unwrap() };
        let held = stack(vec![layer(1, "merged", Some("s1")), layer(2, "open", Some("s2")), layer(3, "open", Some("s3")), layer(4, "open", Some("s4"))]);
        let merge = stack_merge(&held, &reference, MergeMethod::Squash).unwrap();
        assert_eq!(merge.count, 2);
        assert_eq!(
            merge.input,
            json!({
                "projectId": "p", "host": "git.cafe", "repository": "acme/app", "number": 3, "action": "merge", "mergeMethod": "squash",
                "stackNumber": 9, "expectedStackHeads": [{ "number": 2, "headSha": "s2" }, { "number": 3, "headSha": "s3" }]
            })
        );
        assert!(stack_merge(&stack(vec![layer(2, "open", None), layer(3, "open", Some("s3"))]), &reference, MergeMethod::Merge).is_none());
        assert!(stack_merge(&stack(vec![layer(2, "closed", Some("s2")), layer(3, "open", Some("s3"))]), &reference, MergeMethod::Merge).is_none());
        assert!(stack_merge(&stack(vec![layer(3, "merged", Some("s3"))]), &reference, MergeMethod::Merge).is_none());
        assert!(stack_merge(&stack(vec![layer(4, "open", Some("s4"))]), &reference, MergeMethod::Merge).is_none());
        let mut draft = stack(vec![layer(3, "open", Some("s3"))]);
        draft.layers[0].is_draft = Some(true);
        assert!(stack_merge(&draft, &reference, MergeMethod::Merge).is_none());
    }

    #[test]
    fn invalidate_names_the_pull_request() {
        let reference = PrRef { project_id: "p".into(), host: Some("github.com".into()), repository: "a/b".into(), number: 3 };
        assert_eq!(invalidate_input(Some(&reference)), json!({ "reference": { "projectId": "p", "host": "github.com", "repository": "a/b", "number": 3 } }));
        assert_eq!(invalidate_input(None), json!({}));
    }

    #[test]
    fn action_input_carries_merge_method_only_for_merges() {
        let reference = PrRef { project_id: "p".into(), host: Some("github.com".into()), repository: "a/b".into(), number: 3 };
        let merge = action_input(&reference, Action::Merge, Some(MergeMethod::Squash));
        assert_eq!((merge["action"].as_str(), merge["mergeMethod"].as_str()), (Some("merge"), Some("squash")));
        assert!(action_input(&reference, Action::Close, Some(MergeMethod::Squash)).get("mergeMethod").is_none());
    }
}
