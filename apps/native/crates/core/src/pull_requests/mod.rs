//! Pull requests: wire types, the list's search syntax and ranking, the detail panel's derived
//! state, and thread links. Ports `packages/client-runtime/src/pullRequestQuery.ts`,
//! `apps/web/src/components/pullRequest/pullRequestList.logic.ts`,
//! `pullRequestListPreferences.ts`, `pullRequestDetail.logic.ts` and
//! `client-runtime/src/pullRequestLinkInput.ts`. One environment, so nothing is routed.

pub mod detail;
pub mod link;
pub mod list;
pub mod query;
pub mod wire;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use list::{Involvement, ListState, Sort};
use query::{ChecksFilter, DraftFilter, ListFilters, ReviewFilter};
use wire::{Action, ListEntry, MergeMethod};

pub mod method {
    pub const LIST: &str = "pullRequests.list";
    pub const LIST_STATS: &str = "pullRequests.listStats";
    pub const FILTER_OPTIONS: &str = "pullRequests.filterOptions";
    pub const DETAIL: &str = "pullRequests.detail";
    pub const CHECKS: &str = "pullRequests.checks";
    pub const ACTIVITY: &str = "pullRequests.activity";
    pub const RUN_ACTION: &str = "pullRequests.runAction";
    pub const INVALIDATE: &str = "pullRequests.invalidate";
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
        input.insert("query".into(), json!(parsed.text.chars().take(200).collect::<String>()));
    }
    if let Some(cursors) = cursors.filter(|cursors| !cursors.is_empty()) {
        input.insert("cursors".into(), json!(cursors));
    }
    Value::Object(input)
}

/// `pullRequests.listStats` for the rows still without line counts.
pub fn stats_input<'a>(entries: impl IntoIterator<Item = &'a ListEntry>) -> Option<Value> {
    let refs: Vec<Value> = entries.into_iter().filter(|entry| list::needs_stats(entry)).take(MAX_STATS_REFS).map(|entry| PrRef::of(entry).payload()).collect();
    (!refs.is_empty()).then(|| json!({ "refs": refs }))
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

/// `pullRequestActionToast`-style past tense for a finished action.
pub fn action_done_label(action: Action, term: &str) -> String {
    let capitalized = detail::capitalize(term);
    match action {
        Action::Merge => format!("{capitalized} merged"),
        Action::Close => format!("{capitalized} closed"),
        Action::Reopen => format!("{capitalized} reopened"),
        Action::Ready => format!("{capitalized} ready for review"),
        Action::Draft => format!("{capitalized} converted to draft"),
        Action::EnableAutoMerge => "Auto-merge enabled".to_owned(),
        Action::DisableAutoMerge => "Auto-merge disabled".to_owned(),
        Action::ApproveWorkflows => "Workflows approved".to_owned(),
        _ => format!("{capitalized} updated"),
    }
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

    #[test]
    fn action_input_carries_merge_method_only_for_merges() {
        let reference = PrRef { project_id: "p".into(), host: Some("github.com".into()), repository: "a/b".into(), number: 3 };
        let merge = action_input(&reference, Action::Merge, Some(MergeMethod::Squash));
        assert_eq!((merge["action"].as_str(), merge["mergeMethod"].as_str()), (Some("merge"), Some("squash")));
        assert!(action_input(&reference, Action::Close, Some(MergeMethod::Squash)).get("mergeMethod").is_none());
    }
}
