//! Issue list state, multi-environment merging, deduplication, filtering, sorting, pagination,
//! host/project/candidate scope resolution, retry-failed-only tracking, and search query parsing.
//!
//! Ports:
//! - `packages/client-runtime/src/issueList.ts`
//! - `packages/client-runtime/src/state/issues.ts`
//! - `packages/client-runtime/src/state/issueTrackers.ts`
//! - `apps/web/src/components/issue/issueListPreferences.ts`
//! - `apps/web/src/components/issue/issuesSearch.logic.ts`

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::wire::{
    IssueAssigneeCandidate, IssueCandidates, IssueInvolvement, IssueLabelCandidate,
    IssueListCursors, IssueListEntry, IssueListInput, IssueListProjectError, IssueListResult,
    IssueListSort, IssueListState, IssueScopeRef, IssueTrackerKind, IssueTrackerSummary,
};
use crate::timeline::parse_iso_ms;

/// Default rows per scope (`PAGE_SIZE` in `IssuesPage.tsx`).
pub const PAGE_SIZE: u32 = 50;
/// Search debounce delay in milliseconds (`useDebouncedValue(search.q ?? "", 300)`).
pub const SEARCH_DEBOUNCE_MS: u64 = 300;
/// Local persistence key for issue list preferences (`STORAGE_KEY` in `issueListPreferences.ts`).
pub const PREFERENCES_KEY: &str = "t3.issues.preferences";
/// Maximum length for bounded preference strings (`BoundedPreference`).
pub const MAX_BOUNDED_PREFERENCE_LEN: usize = 200;
/// Maximum number of label filters (`Schema.isMaxLength(20)`).
pub const MAX_LABELS: usize = 20;
/// Maximum number of excluded project pairs (`ISSUE_LIST_MAX_EXCLUDED_PROJECTS`).
pub const ISSUE_LIST_MAX_EXCLUDED_PROJECTS: usize = 1000;
/// Upper bound for tracked inactive search entries (`pingdotgg/t3code#38493`).
pub const MAX_HELD_SEARCHES: usize = 16;

/// Persisted list controls (`IssueListPreferences` in `issueListPreferences.ts`).
///
/// Selection state stays out of preferences so saved views and visits never pin a stale issue.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IssueListPreferences {
    pub who: IssueInvolvement,
    pub status: IssueListState,
    /// `(environmentId, projectId)` pairs hidden from the list (`hideProjects`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hide_projects: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub label: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// `None` means `updated`; `Some(IssueListSort::Created)` means `created`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<IssueListSort>,
}

fn bound_opt(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_BOUNDED_PREFERENCE_LEN).collect())
}

fn normalize_hidden_projects(pairs: &[(String, String)]) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (env_id, project_id) in pairs {
        if out.len() >= ISSUE_LIST_MAX_EXCLUDED_PROJECTS {
            break;
        }
        let (Some(env), Some(proj)) = (bound_opt(Some(env_id)), bound_opt(Some(project_id))) else {
            continue;
        };
        if seen.insert((env.clone(), proj.clone())) {
            out.push((env, proj));
        }
    }
    out
}

impl IssueListPreferences {
    /// Normalizes preferences in the exact shape `issueListPreferences()` writes:
    /// deduplicates and caps `hideProjects`, bounds strings, normalizes `order` (`updated`
    /// stored as `None`), and caps labels at `MAX_LABELS`.
    pub fn normalized(&self) -> Self {
        let hide_projects = normalize_hidden_projects(&self.hide_projects);
        let label: Vec<String> = self
            .label
            .iter()
            .filter_map(|name| bound_opt(Some(name.as_str())))
            .take(MAX_LABELS)
            .collect();
        let order = match self.order {
            Some(IssueListSort::Created) => Some(IssueListSort::Created),
            _ => None,
        };
        let who = match self.who {
            IssueInvolvement::Unknown => IssueInvolvement::All,
            other => other,
        };
        let status = match self.status {
            IssueListState::Unknown => IssueListState::Open,
            other => other,
        };
        Self {
            who,
            status,
            hide_projects,
            host: bound_opt(self.host.as_deref()),
            q: bound_opt(self.q.as_deref()),
            label,
            assignee: bound_opt(self.assignee.as_deref()),
            order,
        }
    }

    pub fn effective_sort(&self) -> IssueListSort {
        match self.order {
            Some(IssueListSort::Created) => IssueListSort::Created,
            _ => IssueListSort::Updated,
        }
    }

    /// Number of active filters shown in the `Filters` button pill (`IssueListFilterPickers.tsx`).
    pub fn filter_count(&self) -> usize {
        usize::from(self.status != IssueListState::Open)
            + usize::from(self.who != IssueInvolvement::All)
            + usize::from(!self.hide_projects.is_empty())
            + usize::from(self.assignee.as_deref().is_some_and(|a| !a.trim().is_empty()))
            + self.label.len()
    }

    /// Whether any filter narrows the default listing.
    pub fn is_filtered(&self) -> bool {
        self.filter_count() > 0 || self.host.as_deref().is_some_and(|h| !h.trim().is_empty())
    }

    /// Builds the wire `IssueListInput` for these preferences.
    pub fn to_list_input(
        &self,
        limit: u32,
        debounced_query: &str,
        cursors: Option<IssueListCursors>,
        refresh: bool,
    ) -> IssueListInput {
        let normalized = self.normalized();
        let query = bound_opt(Some(debounced_query));
        let labels = (!normalized.label.is_empty()).then_some(normalized.label);
        IssueListInput {
            state: normalized.status,
            involvement: Some(normalized.who),
            project_id: None,
            exclude_project_ids: Vec::new(),
            host: normalized.host,
            limit: Some(limit.clamp(1, 200)),
            cursors: cursors.filter(|c| !c.is_empty()),
            refresh: refresh.then_some(true),
            query,
            labels,
            assignee: normalized.assignee,
            sort: normalized.order,
        }
    }
}

fn project_pairs_from_json(value: Option<&Value>) -> Vec<(String, String)> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    let raw_pairs: Vec<(String, String)> = items
        .iter()
        .filter_map(|pair| {
            let arr = pair.as_array()?;
            let env = bounded_json_value(arr.first())?;
            let proj = bounded_json_value(arr.get(1))?;
            Some((env, proj))
        })
        .collect();
    normalize_hidden_projects(&raw_pairs)
}

/// Full route search parameters (`IssuesSearch` in `IssuesPage.tsx` / `issuesSearch.logic.ts`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssuesSearch {
    pub preferences: IssueListPreferences,
    pub selected_environment_id: Option<String>,
    pub selected_project_id: Option<String>,
    pub selected_host: Option<String>,
    pub tracker: Option<String>,
    pub scope: Option<String>,
    pub key: Option<String>,
}

fn bounded_json_value(value: Option<&Value>) -> Option<String> {
    let text = match value? {
        Value::String(s) => s.as_str().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_BOUNDED_PREFERENCE_LEN).collect())
}

fn labels_from_json(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else { return Vec::new() };
    let items: Vec<&Value> = match value {
        Value::Array(arr) => arr.iter().collect(),
        single => vec![single],
    };
    items
        .into_iter()
        .filter_map(|item| bounded_json_value(Some(item)))
        .take(MAX_LABELS)
        .collect()
}

/// Parses raw URL / route query parameters safely (`issuesSearchFromUrl` in `issuesSearch.logic.ts`).
///
/// Accepts numeric `key` values, bounds all strings to 200 chars, caps labels at 20, and accepts
/// any host string (including unknown/invalid hosts) without crashing (`pingdotgg/t3code#38190`).
pub fn issues_search_from_url(raw: &Map<String, Value>) -> IssuesSearch {
    let who = match raw.get("who").and_then(Value::as_str) {
        Some("assigned") => IssueInvolvement::Assigned,
        Some("authored") => IssueInvolvement::Authored,
        Some("mentioned") => IssueInvolvement::Mentioned,
        _ => IssueInvolvement::All,
    };
    let status = match raw.get("status").and_then(Value::as_str) {
        Some("closed") => IssueListState::Closed,
        Some("all") => IssueListState::All,
        _ => IssueListState::Open,
    };
    let order = match raw.get("order").and_then(Value::as_str) {
        Some("created") => Some(IssueListSort::Created),
        _ => None,
    };
    IssuesSearch {
        preferences: IssueListPreferences {
            who,
            status,
            hide_projects: project_pairs_from_json(raw.get("hideProjects")),
            host: bounded_json_value(raw.get("host")),
            q: bounded_json_value(raw.get("q")),
            label: labels_from_json(raw.get("label")),
            assignee: bounded_json_value(raw.get("assignee")),
            order,
        },
        selected_environment_id: bounded_json_value(raw.get("selectedEnvironmentId")),
        selected_project_id: bounded_json_value(raw.get("selectedProjectId")),
        selected_host: bounded_json_value(raw.get("selectedHost")),
        tracker: bounded_json_value(raw.get("tracker")),
        scope: bounded_json_value(raw.get("scope")),
        key: bounded_json_value(raw.get("key")),
    }
}

/// One row's identity across list pages and detail reads (`issueEntryKey`).
pub fn issue_entry_key(project_id: &str, host: &str, scope: &str, key: &str) -> String {
    format!(
        "{project_id}\0{}\0{}\0{key}",
        host.to_lowercase(),
        scope.to_lowercase()
    )
}

/// Stable grouping key for one tracker account (`issueTrackerKey`).
pub fn issue_tracker_key(tracker: &IssueTrackerKind, host: &str) -> String {
    format!("{} {}", tracker.as_str(), host.to_lowercase())
}

fn compare_iso_desc(left: &str, right: &str) -> Ordering {
    match (parse_iso_ms(left), parse_iso_ms(right)) {
        (Some(l), Some(r)) => r.cmp(&l),
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

/// Compares two entries newest-first by `sort` (`issueOrder` in `issueList.ts`).
pub fn compare_issues(left: &IssueListEntry, right: &IssueListEntry, sort: IssueListSort) -> Ordering {
    match sort {
        IssueListSort::Created => compare_iso_desc(&left.created_at, &right.created_at),
        IssueListSort::Updated | IssueListSort::Unknown => {
            compare_iso_desc(&left.updated_at, &right.updated_at)
        }
    }
}

/// Lays a continuation page over held rows (`mergeIssueListPage` in `state/issues.ts`).
///
/// Rows from other scopes stay; repeated rows replace the held version; order remains newest
/// `updatedAt` first.
pub fn merge_issue_list_page(
    held: &[IssueListEntry],
    page: &[IssueListEntry],
) -> Vec<IssueListEntry> {
    if held.is_empty() {
        return page.to_vec();
    }
    if page.is_empty() {
        return held.to_vec();
    }
    let mut order: Vec<String> = Vec::with_capacity(held.len() + page.len());
    let mut by_key: HashMap<String, IssueListEntry> = HashMap::with_capacity(held.len() + page.len());
    for entry in held {
        let key = issue_entry_key(&entry.project_id, &entry.host, &entry.scope, &entry.key);
        if !by_key.contains_key(&key) {
            order.push(key.clone());
        }
        by_key.insert(key, entry.clone());
    }
    for entry in page {
        let key = issue_entry_key(&entry.project_id, &entry.host, &entry.scope, &entry.key);
        if !by_key.contains_key(&key) {
            order.push(key.clone());
        }
        by_key.insert(key, entry.clone());
    }
    let mut merged: Vec<IssueListEntry> = order
        .into_iter()
        .filter_map(|key| by_key.remove(&key))
        .collect();
    merged.sort_by(|a, b| compare_iso_desc(&a.updated_at, &b.updated_at));
    merged
}

/// Continuation errors remembered beside the cursor keys that produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationError {
    pub asked: Vec<String>,
    pub errors: Vec<IssueListProjectError>,
}

/// Rows from one environment with any continuation pages laid over them (`HeldIssuePage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldIssuePage {
    pub answer: IssueListResult,
    pub entries: Vec<IssueListEntry>,
    pub errors: Vec<IssueListProjectError>,
    pub more_errors: Vec<ContinuationError>,
    pub next_cursors: IssueListCursors,
    pub more_failure: Option<String>,
}

impl HeldIssuePage {
    pub fn from_answer(answer: IssueListResult) -> Self {
        let entries = answer.entries.clone();
        let errors = answer.errors.clone();
        let next_cursors = answer.next_cursors.clone();
        Self {
            answer,
            entries,
            errors,
            more_errors: Vec::new(),
            next_cursors,
            more_failure: None,
        }
    }
}

/// Reconciles first-page answers with held continuation pages (`holdIssuePages`).
///
/// When an environment's first-page answer is unchanged, its held continuation pages survive;
/// a new first-page answer resets continuations for that environment.
pub fn hold_issue_pages(
    answers: &[(String, IssueListResult)],
    pages: &BTreeMap<String, HeldIssuePage>,
) -> BTreeMap<String, HeldIssuePage> {
    let mut next = BTreeMap::new();
    for (env_id, answer) in answers {
        let page = match pages.get(env_id) {
            Some(previous) if previous.answer == *answer => previous.clone(),
            _ => HeldIssuePage::from_answer(answer.clone()),
        };
        next.insert(env_id.clone(), page);
    }
    next
}

/// Filters `cursors` to those belonging to `host` (`issueCursorsForHost`).
pub fn issue_cursors_for_host(cursors: &IssueListCursors, host: &str) -> IssueListCursors {
    let prefix = format!("{host} ");
    cursors
        .iter()
        .filter(|(key, _)| key.starts_with(&prefix))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Applies the result of a continuation read to `page` (`continueIssuePage`).
pub fn continue_issue_page(
    page: &HeldIssuePage,
    next: Result<&IssueListResult, String>,
    asked: &IssueListCursors,
) -> HeldIssuePage {
    match next {
        Err(failure) => HeldIssuePage {
            more_failure: Some(failure),
            ..page.clone()
        },
        Ok(result) => {
            let mut more_errors: Vec<ContinuationError> = page
                .more_errors
                .iter()
                .filter(|more| !more.asked.iter().any(|key| asked.contains_key(key)))
                .cloned()
                .collect();
            if !result.errors.is_empty() {
                more_errors.push(ContinuationError {
                    asked: asked.keys().cloned().collect(),
                    errors: result.errors.clone(),
                });
            }
            let mut errors = page.answer.errors.clone();
            for more in &more_errors {
                errors.extend(more.errors.iter().cloned());
            }
            let mut next_cursors: IssueListCursors = page
                .next_cursors
                .iter()
                .filter(|(key, _)| !asked.contains_key(*key))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (k, v) in &result.next_cursors {
                next_cursors.insert(k.clone(), v.clone());
            }
            HeldIssuePage {
                answer: page.answer.clone(),
                entries: merge_issue_list_page(&page.entries, &result.entries),
                errors,
                more_errors,
                next_cursors,
                more_failure: None,
            }
        }
    }
}

/// An issue row tagged with the environment it was read from (`EnvironmentIssueEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentIssueEntry {
    pub environment_id: String,
    pub entry: IssueListEntry,
}

/// Project-scoped error tagged with its environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentProjectError {
    pub environment_id: String,
    pub project_id: String,
    pub project_title: String,
    pub message: String,
}

/// One repository binding (primary repo or its fork upstream) (`sourceControlClients.issueBindings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryIssueBinding {
    pub tracker: String,
    pub host: String,
    pub scope: String,
}

/// Minimal project info used for error attribution, project filtering, and candidate scopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDescriptor {
    pub environment_id: String,
    pub project_id: String,
    pub title: String,
    pub is_scratch: bool,
    pub provider: Option<String>,
    pub host: Option<String>,
    pub issue_scope: Option<String>,
    pub issue_bindings: Vec<RepositoryIssueBinding>,
}

fn non_empty_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn http_url_host_with_port(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_lowercase();
    match url.port() {
        Some(port) => Some(format!("{host}:{port}")),
        None => Some(host),
    }
}

fn http_url_origin_and_host(raw: &str) -> Option<(String, String)> {
    let url = url::Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = http_url_host_with_port(raw)?;
    Some((format!("{}://{host}", url.scheme()), host))
}

/// Decodes percent-encoded UTF-8 URL path segments (`decodeUrlPathSegment`), returning the
/// original segment unchanged when percent escapes or UTF-8 bytes are invalid.
fn decode_url_path_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return segment.to_owned();
            }
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            let (Some(hi), Some(lo)) = (hi, lo) else {
                return segment.to_owned();
            };
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| segment.to_owned())
}

/// Extracts the change-request / issue tracker host from a `RepositoryIdentity` JSON value
/// (`sourceControlClients.changeRequestHost` in `packages/source-control-core` and
/// `packages/source-control-forgejo`).
pub fn repository_change_request_host(
    identity: &Value,
    provider: Option<&str>,
) -> Option<String> {
    if provider == Some("forgejo") {
        if let Some(web_url) = non_empty_str(identity, "webUrl")
            && let Some(host) = http_url_host_with_port(web_url)
        {
            return Some(host);
        }
        if let Some(remote_url) = identity
            .get("locator")
            .and_then(|locator| non_empty_str(locator, "remoteUrl"))
            && let Some(host) = http_url_host_with_port(remote_url)
        {
            return Some(host);
        }
    }
    if let Some(canonical_key) = non_empty_str(identity, "canonicalKey")
        && let Some(host) = canonical_key
            .split('/')
            .next()
            .map(str::trim)
            .filter(|h| !h.is_empty())
    {
        return Some(host.to_lowercase());
    }
    if let Some(web_url) = non_empty_str(identity, "webUrl")
        && let Some(host) = http_url_host_with_port(web_url)
    {
        return Some(host);
    }
    provider
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_lowercase)
}

/// Extracts the issue container scope from a `RepositoryIdentity` JSON value
/// (`sourceControlClients.issueScope` in `packages/source-control-core` and
/// `packages/source-control-azure-devops/src/shared/definition.ts:179-196`).
pub fn repository_issue_scope(identity: &Value) -> Option<String> {
    let provider = non_empty_str(identity, "provider");
    if provider == Some("azure-devops") {
        let display_name = identity
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or("");
        let segments: Vec<String> = display_name
            .split('/')
            .filter(|part| !part.is_empty())
            .map(decode_url_path_segment)
            .collect();
        let at = segments.iter().position(|part| part == "_git")?;
        if at == 2 {
            return Some(segments[..2].join("/"));
        }
        let repository = segments.get(at + 1)?;
        if at != 0 && at != 1 {
            return None;
        }
        let host = identity
            .get("canonicalKey")
            .and_then(Value::as_str)
            .and_then(|key| key.split('/').next())
            .unwrap_or("")
            .to_lowercase();
        let legacy = host.ends_with(".visualstudio.com");
        let first = segments.first().map(String::as_str).unwrap_or("");
        if at == 0 {
            return legacy.then(|| repository.clone());
        }
        if host == "dev.azure.com" {
            return Some(format!("{first}/{repository}"));
        }
        if !legacy {
            return None;
        }
        return if first.eq_ignore_ascii_case("defaultcollection") {
            Some(repository.clone())
        } else {
            Some(first.to_owned())
        };
    }

    let strip_owner_mark = |value: String| {
        if provider == Some("tanstack-source") {
            value
                .strip_prefix('@')
                .or_else(|| {
                    value
                        .get(..3)
                        .filter(|p| p.eq_ignore_ascii_case("%40"))
                        .map(|_| &value[3..])
                })
                .unwrap_or(&value)
                .to_owned()
        } else {
            value
        }
    };

    if let Some(display_name) = non_empty_str(identity, "displayName") {
        return Some(strip_owner_mark(display_name.to_owned()));
    }
    if let (Some(owner), Some(name)) = (
        non_empty_str(identity, "owner"),
        non_empty_str(identity, "name"),
    ) {
        return Some(strip_owner_mark(format!("{owner}/{name}")));
    }
    if let Some(web_url) = non_empty_str(identity, "webUrl")
        && let Ok(url) = url::Url::parse(web_url)
        && matches!(url.scheme(), "http" | "https")
    {
        let segments: Vec<&str> = url
            .path()
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        if segments.len() >= 2 {
            return Some(strip_owner_mark(segments.join("/")));
        }
    }
    None
}

/// Returns primary plus same-host fork upstream bindings (`sourceControlClients.issueBindings`,
/// `pingdotgg/t3code#39159`).
pub fn repository_issue_bindings(identity: &Value) -> Vec<RepositoryIssueBinding> {
    let Some(tracker) = non_empty_str(identity, "provider").filter(|p| *p != "unknown") else {
        return Vec::new();
    };
    let Some(scope) = repository_issue_scope(identity) else {
        return Vec::new();
    };
    let Some(host) = repository_change_request_host(identity, Some(tracker)) else {
        return Vec::new();
    };
    let primary = RepositoryIssueBinding {
        tracker: tracker.to_owned(),
        host: host.clone(),
        scope: scope.clone(),
    };

    let Some(origin) = identity.get("origin").filter(|v| v.is_object()) else {
        return vec![primary];
    };
    if repository_change_request_host(origin, None) != repository_change_request_host(identity, None) {
        return vec![primary];
    }
    let origin_url = non_empty_str(origin, "remoteUrl").and_then(http_url_origin_and_host);
    let primary_url = identity
        .get("locator")
        .and_then(|locator| non_empty_str(locator, "remoteUrl"))
        .and_then(http_url_origin_and_host)
        .or_else(|| non_empty_str(identity, "webUrl").and_then(http_url_origin_and_host));
    if let Some((ref o_origin, ref o_host)) = origin_url {
        let matches_host = match primary_url {
            Some((ref p_origin, _)) => o_origin == p_origin,
            None => o_host == &host,
        };
        if !matches_host {
            return vec![primary];
        }
    }

    let origin_key = non_empty_str(origin, "canonicalKey").unwrap_or("");
    let origin_display = non_empty_str(origin, "displayName")
        .map(str::to_owned)
        .unwrap_or_else(|| {
            origin_key
                .split('/')
                .skip(1)
                .collect::<Vec<_>>()
                .join("/")
        });
    let synthetic = serde_json::json!({
        "provider": tracker,
        "canonicalKey": origin_key,
        "displayName": origin_display,
    });
    let Some(origin_scope) = repository_issue_scope(&synthetic) else {
        return vec![primary];
    };
    if origin_scope.eq_ignore_ascii_case(&scope) {
        return vec![primary];
    }
    vec![
        primary,
        RepositoryIssueBinding {
            tracker: tracker.to_owned(),
            host,
            scope: origin_scope,
        },
    ]
}

impl ProjectDescriptor {
    /// Builds a `ProjectDescriptor` from a project's metadata and optional `RepositoryIdentity`.
    pub fn from_repository_identity(
        environment_id: String,
        project_id: String,
        title: String,
        is_scratch: bool,
        identity: Option<&Value>,
    ) -> Self {
        let provider = identity
            .and_then(|id| non_empty_str(id, "provider"))
            .map(str::to_owned);
        let host = identity
            .and_then(|id| repository_change_request_host(id, provider.as_deref()));
        let issue_scope = identity.and_then(repository_issue_scope);
        let issue_bindings = identity.map(repository_issue_bindings).unwrap_or_default();
        Self {
            environment_id,
            project_id,
            title,
            is_scratch,
            provider,
            host,
            issue_scope,
            issue_bindings,
        }
    }
}

/// One tracker section on the Issues page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueTrackerGroup {
    pub tracker: IssueTrackerSummary,
    pub rows: Vec<EnvironmentIssueEntry>,
    pub errors: Vec<EnvironmentProjectError>,
    /// Environments that still have continuation cursors for this tracker's host.
    pub more: Vec<String>,
}

/// Narrows `held` pages to trackers matching `chosen_host` (case-insensitively) while keeping
/// the full tracker list available separately for the host switcher (`pingdotgg/t3code#38085`).
pub fn filter_held_by_host(
    held: &BTreeMap<String, HeldIssuePage>,
    chosen_host: Option<&str>,
) -> BTreeMap<String, HeldIssuePage> {
    let Some(host) = chosen_host.map(str::trim).filter(|h| !h.is_empty()) else {
        return held.clone();
    };
    let lower = host.to_lowercase();
    held.iter()
        .map(|(env_id, page)| {
            let mut narrowed = page.clone();
            narrowed
                .answer
                .trackers
                .retain(|tracker| tracker.host.to_lowercase() == lower);
            (env_id.clone(), narrowed)
        })
        .collect()
}

/// Groups issues across environments by tracker account (`groupIssues` in `issueList.ts`).
///
/// - Deduplicates identical issues held by multiple environments by `entry.url`
///   (`pingdotgg/t3code#38205`), keeping the first environment's row.
/// - Locally filters by `query` (title or displayKey) when a tracker lacks `capabilities.search`.
/// - Routes project errors to their tracker group (falling back to the first tracker group).
/// - Sorts rows within each group newest-first by `sort`, and sorts groups by `issue_tracker_key`.
pub fn group_issues(
    held: &BTreeMap<String, HeldIssuePage>,
    query: &str,
    projects: &[ProjectDescriptor],
    sort: IssueListSort,
) -> Vec<IssueTrackerGroup> {
    let mut by_tracker: BTreeMap<String, IssueTrackerGroup> = BTreeMap::new();
    let mut listed_urls: HashSet<String> = HashSet::new();
    let needle = query.trim().to_lowercase();

    for (env_id, page) in held {
        for tracker in &page.answer.trackers {
            let key = issue_tracker_key(&tracker.tracker, &tracker.host);
            let group = by_tracker.entry(key).or_insert_with(|| IssueTrackerGroup {
                tracker: tracker.clone(),
                rows: Vec::new(),
                errors: Vec::new(),
                more: Vec::new(),
            });
            if !group.tracker.configured && tracker.configured {
                group.tracker = tracker.clone();
            }
            let cursor_prefix = format!("{} ", tracker.host);
            if page
                .next_cursors
                .keys()
                .any(|cursor_key| cursor_key.starts_with(&cursor_prefix))
                && !group.more.contains(env_id)
            {
                group.more.push(env_id.clone());
            }
        }

        for entry in &page.entries {
            let key = issue_tracker_key(&entry.tracker, &entry.host);
            let Some(group) = by_tracker.get_mut(&key) else { continue };
            let searches_on_host = group
                .tracker
                .capabilities
                .is_some_and(|caps| caps.search);
            if !needle.is_empty()
                && !searches_on_host
                && !entry.title.to_lowercase().contains(&needle)
                && !entry.display_key.to_lowercase().contains(&needle)
            {
                continue;
            }
            // Environments holding the same repository answer the same issue; the first keeps it.
            if !listed_urls.insert(entry.url.clone()) {
                continue;
            }
            group.rows.push(EnvironmentIssueEntry {
                environment_id: env_id.clone(),
                entry: entry.clone(),
            });
        }

        for error in &page.errors {
            let project = projects
                .iter()
                .find(|p| p.environment_id == *env_id && p.project_id == error.project_id);
            let group_key = project
                .and_then(|p| match (p.provider.as_deref(), p.host.as_deref()) {
                    (Some(kind), Some(host)) => {
                        Some(format!("{kind} {}", host.to_lowercase()))
                    }
                    _ => None,
                })
                .filter(|key| by_tracker.contains_key(key))
                .or_else(|| by_tracker.keys().next().cloned());
            if let Some(key) = group_key
                && let Some(group) = by_tracker.get_mut(&key)
            {
                group.errors.push(EnvironmentProjectError {
                    environment_id: env_id.clone(),
                    project_id: error.project_id.clone(),
                    project_title: error.project_title.clone(),
                    message: error.message.clone(),
                });
            }
        }
    }

    let mut groups: Vec<IssueTrackerGroup> = by_tracker.into_values().collect();
    for group in &mut groups {
        group
            .rows
            .sort_by(|left, right| compare_issues(&left.entry, &right.entry, sort));
    }
    groups.sort_by(|a, b| {
        issue_tracker_key(&a.tracker.tracker, &a.tracker.host)
            .cmp(&issue_tracker_key(&b.tracker.tracker, &b.tracker.host))
    });
    groups
}

/// One choice in the Tracker/Host switcher (`hostOptionsFor` in `IssuesPage.tsx`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOption {
    /// `None` means "All" hosts.
    pub host: Option<String>,
    pub label: String,
    pub tracker: Option<IssueTrackerKind>,
    pub unavailable: Option<String>,
}

/// Builds the host switcher choices (`hostOptionsFor`), preserving any remembered or invalid
/// `chosen` host so the user can always clear it (`pingdotgg/t3code#38085`, `pingdotgg/t3code#38190`).
pub fn host_options_for(
    trackers: &[IssueTrackerSummary],
    chosen: Option<&str>,
) -> Vec<HostOption> {
    let mut ordered: Vec<&IssueTrackerSummary> = trackers.iter().collect();
    ordered.sort_by(|a, b| {
        issue_tracker_key(&a.tracker, &a.host).cmp(&issue_tracker_key(&b.tracker, &b.host))
    });

    let mut hosts: Vec<&IssueTrackerSummary> = Vec::new();
    let mut index_by_host: HashMap<String, usize> = HashMap::new();
    for tracker in ordered {
        let host_key = tracker.host.clone();
        if let Some(&idx) = index_by_host.get(&host_key) {
            if !hosts[idx].configured && tracker.configured {
                hosts[idx] = tracker;
            }
        } else {
            index_by_host.insert(host_key, hosts.len());
            hosts.push(tracker);
        }
    }

    let mut options = vec![HostOption {
        host: None,
        label: "All".to_owned(),
        tracker: None,
        unavailable: None,
    }];

    if let Some(chosen_host) = chosen.map(str::trim).filter(|h| !h.is_empty())
        && !index_by_host.contains_key(chosen_host)
    {
        options.push(HostOption {
            host: Some(chosen_host.to_owned()),
            label: chosen_host.to_owned(),
            tracker: None,
            unavailable: None,
        });
    }

    for tracker in &hosts {
        let provider_name = tracker.tracker.display_name();
        let label = if hosts.len() > 1 && provider_name == tracker.host {
            tracker.host.clone()
        } else {
            format!("{provider_name} ({})", tracker.host)
        };
        let unavailable = (!tracker.configured).then(|| {
            tracker
                .detail
                .as_deref()
                .map(|d| issue_tracker_detail_text(d).to_owned())
                .unwrap_or_else(|| "Unavailable".to_owned())
        });
        options.push(HostOption {
            host: Some(tracker.host.clone()),
            label,
            tracker: Some(tracker.tracker.clone()),
            unavailable,
        });
    }

    options
}

/// One choice in the Project filter submenu (`issueFilterProjects` in `issuesSearch.logic.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterProject {
    pub environment_id: String,
    pub project_id: String,
    pub title: String,
}

/// Builds project filter choices (`pingdotgg/t3code#38256`, `#38799`): keeps scratch projects so
/// Linear bindings on them can still be filtered, keeps one entry per project even when two
/// projects share a repository, and disambiguates duplicate titles across environments.
pub fn issue_filter_projects(
    projects: &[ProjectDescriptor],
    environment_labels: &HashMap<String, String>,
) -> Vec<FilterProject> {
    let eligible: Vec<&ProjectDescriptor> = projects.iter().collect();
    let mut title_counts: HashMap<String, usize> = HashMap::new();
    for project in &eligible {
        *title_counts.entry(project.title.to_lowercase()).or_default() += 1;
    }
    let mut out: Vec<FilterProject> = eligible
        .into_iter()
        .map(|project| {
            let duplicate = title_counts
                .get(&project.title.to_lowercase())
                .copied()
                .unwrap_or(0)
                > 1;
            let title = if duplicate
                && let Some(env_label) = environment_labels.get(&project.environment_id)
                && !env_label.trim().is_empty()
            {
                format!("{} · {}", project.title, env_label)
            } else {
                project.title.clone()
            };
            FilterProject {
                environment_id: project.environment_id.clone(),
                project_id: project.project_id.clone(),
                title,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.project_id.cmp(&b.project_id))
    });
    out
}

/// Planned per-environment `issues.list` RPC target (`IssueListTarget`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueListTarget {
    pub environment_id: String,
    pub input: IssueListInput,
}

pub fn is_project_hidden(
    hidden: &[(String, String)],
    environment_id: &str,
    project_id: &str,
) -> bool {
    hidden
        .iter()
        .any(|(env, proj)| env == environment_id && proj == project_id)
}

/// Returns `true` when at least one project exists and every project is hidden
/// (`everyProjectHidden` in `IssuesPage.tsx`).
pub fn every_project_hidden(projects: &[FilterProject], hidden: &[(String, String)]) -> bool {
    !projects.is_empty()
        && projects
            .iter()
            .all(|project| is_project_hidden(hidden, &project.environment_id, &project.project_id))
}

/// Computes per-environment `issues.list` requests leaving out hidden projects and skipping any
/// environment whose own projects are all hidden (`issueListTargets` in `issuesSearch.logic.ts`).
pub fn issue_list_targets(
    environment_ids: &[String],
    projects: &[FilterProject],
    hidden: &[(String, String)],
    base_input: &IssueListInput,
) -> Vec<IssueListTarget> {
    if every_project_hidden(projects, hidden) {
        return Vec::new();
    }
    let mut targets = Vec::new();
    for environment_id in environment_ids {
        let own: Vec<&FilterProject> = projects
            .iter()
            .filter(|project| project.environment_id == *environment_id)
            .collect();
        if !own.is_empty()
            && own
                .iter()
                .all(|project| is_project_hidden(hidden, &project.environment_id, &project.project_id))
        {
            continue;
        }
        let mut exclude_project_ids: Vec<String> = if !own.is_empty() {
            own.into_iter()
                .filter(|project| {
                    is_project_hidden(hidden, &project.environment_id, &project.project_id)
                })
                .map(|project| project.project_id.clone())
                .collect()
        } else {
            hidden
                .iter()
                .filter(|(env, _)| env == environment_id)
                .map(|(_, project_id)| project_id.clone())
                .collect()
        };
        exclude_project_ids.truncate(ISSUE_LIST_MAX_EXCLUDED_PROJECTS);
        let mut input = base_input.clone();
        input.exclude_project_ids = exclude_project_ids;
        targets.push(IssueListTarget {
            environment_id: environment_id.clone(),
            input,
        });
    }
    targets
}

/// Target for an `issues.candidates` RPC call (`IssueCandidatesTarget`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IssueCandidatesTarget {
    pub environment_id: String,
    pub input: IssueScopeRef,
}

impl IssueCandidatesTarget {
    pub fn key(&self) -> String {
        format!(
            "{}\0{}\0{}\0{}",
            self.environment_id,
            self.input.tracker.as_str(),
            self.input.host.as_deref().unwrap_or("").to_lowercase(),
            self.input.scope.to_lowercase()
        )
    }
}

/// Computes the candidate scopes to query for Labels and Assignees pickers (`issueFilterScopes`).
pub fn issue_filter_scopes(
    held: &BTreeMap<String, HeldIssuePage>,
    projects: &[ProjectDescriptor],
    hidden: &[(String, String)],
) -> Vec<IssueCandidatesTarget> {
    let mut scopes: Vec<IssueCandidatesTarget> = Vec::new();
    let already_has = |scopes: &[IssueCandidatesTarget], scope: &str, host: Option<&str>| {
        let scope_lower = scope.to_lowercase();
        let host_lower = host.map(str::to_lowercase);
        scopes.iter().any(|existing| {
            existing.input.scope.to_lowercase() == scope_lower
                && existing.input.host.as_deref().map(str::to_lowercase) == host_lower
        })
    };

    for (env_id, page) in held {
        for project in projects {
            if project.environment_id != *env_id {
                continue;
            }
            if is_project_hidden(hidden, env_id, &project.project_id) {
                continue;
            }
            let bindings: Vec<(String, String, String)> = if !project.issue_bindings.is_empty() {
                project
                    .issue_bindings
                    .iter()
                    .map(|b| (b.tracker.clone(), b.host.clone(), b.scope.clone()))
                    .collect()
            } else if let (Some(kind), Some(host), Some(scope)) = (
                project.provider.as_deref(),
                project.host.as_deref(),
                project.issue_scope.as_deref(),
            ) {
                vec![(kind.to_owned(), host.to_owned(), scope.to_owned())]
            } else {
                Vec::new()
            };
            for (kind, host, scope) in bindings {
                let tracker = page.answer.trackers.iter().find(|t| {
                    t.tracker.as_str() == kind && t.host.eq_ignore_ascii_case(&host)
                });
                let Some(tracker) = tracker else { continue };
                if !tracker.configured || tracker.capabilities.is_none() {
                    continue;
                }
                if already_has(&scopes, &scope, Some(&host)) {
                    continue;
                }
                scopes.push(IssueCandidatesTarget {
                    environment_id: env_id.clone(),
                    input: IssueScopeRef {
                        project_id: project.project_id.clone(),
                        tracker: tracker.tracker.clone(),
                        host: Some(host),
                        scope,
                    },
                });
            }
        }

        // Standalone trackers (e.g. Linear team bound in settings) appear in `answer.scopes`;
        // older servers omit `scopes`, so rows stand in.
        let covered: Vec<IssueScopeRef> = match &page.answer.scopes {
            Some(list) => list.clone(),
            None => page
                .entries
                .iter()
                .map(|entry| IssueScopeRef {
                    project_id: entry.project_id.clone(),
                    tracker: entry.tracker.clone(),
                    host: Some(entry.host.clone()),
                    scope: entry.scope.clone(),
                })
                .collect(),
        };
        for input in covered {
            if is_project_hidden(hidden, env_id, &input.project_id) {
                continue;
            }
            let host_lower = input.host.as_deref().map(str::to_lowercase);
            let tracker = page.answer.trackers.iter().find(|t| {
                t.tracker == input.tracker
                    && host_lower.as_deref() == Some(t.host.to_lowercase().as_str())
            });
            let Some(tracker) = tracker else { continue };
            if !tracker.configured {
                continue;
            }
            let supports_pickers = tracker
                .capabilities
                .is_some_and(|caps| caps.labels || caps.assignees);
            if !supports_pickers {
                continue;
            }
            if already_has(&scopes, &input.scope, input.host.as_deref()) {
                continue;
            }
            scopes.push(IssueCandidatesTarget {
                environment_id: env_id.clone(),
                input,
            });
        }
    }

    scopes
}

/// How an issue list read was requested (`IssueListRefreshMode` in `state/issues.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueListRefreshMode {
    /// Explicit user refresh: sets `refresh: true` so Azure DevOps re-reads each project's
    /// finished states and tracker metadata (`pingdotgg/t3code#38986`).
    Manual,
    /// Automatic/background refresh: omits `refresh: true` and skips when a read is already
    /// in flight (`pingdotgg/t3code#38508`).
    Background,
}

/// Per-environment or per-scope read lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadSlotState {
    Idle,
    Pending,
    Loaded,
    Failed(String),
}

/// Per-environment list read state supporting:
/// - retry-failed-only without restarting pending or already-loaded environments
///   (`pingdotgg/t3code#38654`)
/// - manual `refresh: true` intent surviving interruption until a non-interrupted completion
///   (`pingdotgg/t3code#38986`)
/// - skipping background live refreshes while any read is in flight (`pingdotgg/t3code#38508`)
/// - evicting closed search keys so memory never grows unboundedly (`pingdotgg/t3code#38493`).
#[derive(Debug, Clone, Default)]
pub struct IssueListReadTracker {
    active_search_key: Option<String>,
    slots: BTreeMap<String, EnvironmentReadSlot>,
    held_searches: HashMap<String, BTreeMap<String, IssueListResult>>,
    search_order: Vec<String>,
}

#[derive(Debug, Clone)]
struct EnvironmentReadSlot {
    state: ReadSlotState,
    generation: u64,
    pending_manual_refresh: bool,
    answer: Option<IssueListResult>,
}

impl Default for EnvironmentReadSlot {
    fn default() -> Self {
        Self {
            state: ReadSlotState::Idle,
            generation: 0,
            pending_manual_refresh: false,
            answer: None,
        }
    }
}

impl IssueListReadTracker {
    /// Switches to a new search/filter scope key, pruning stale closed-search caches beyond
    /// `MAX_HELD_SEARCHES` (`pingdotgg/t3code#38493`).
    pub fn set_search_key(&mut self, key: String, environments: &[String]) -> bool {
        if self.active_search_key.as_deref() == Some(key.as_str()) {
            let desired: HashSet<&str> = environments.iter().map(String::as_str).collect();
            self.slots.retain(|env_id, _| desired.contains(env_id.as_str()));
            for env_id in environments {
                self.slots.entry(env_id.clone()).or_default();
            }
            return false;
        }
        if let Some(prev_key) = self.active_search_key.take() {
            let snapshot: BTreeMap<String, IssueListResult> = self
                .slots
                .iter()
                .filter_map(|(env, slot)| slot.answer.clone().map(|ans| (env.clone(), ans)))
                .collect();
            if !snapshot.is_empty() {
                self.held_searches.insert(prev_key.clone(), snapshot);
                self.search_order.retain(|k| k != &prev_key);
                self.search_order.push(prev_key);
                while self.search_order.len() > MAX_HELD_SEARCHES {
                    let evicted = self.search_order.remove(0);
                    self.held_searches.remove(&evicted);
                }
            }
        }
        self.active_search_key = Some(key.clone());
        let restored = self.held_searches.remove(&key).unwrap_or_default();
        self.search_order.retain(|k| k != &key);
        self.slots.clear();
        for env_id in environments {
            let mut slot = EnvironmentReadSlot::default();
            if let Some(answer) = restored.get(env_id) {
                slot.answer = Some(answer.clone());
                slot.state = ReadSlotState::Loaded;
            }
            self.slots.insert(env_id.clone(), slot);
        }
        true
    }

    /// Drops every cached closed search immediately (e.g. when leaving the Issues page or
    /// switching environments) (`pingdotgg/t3code#38493`).
    pub fn prune_closed_searches(&mut self) {
        self.held_searches.clear();
        self.search_order.clear();
    }

    pub fn cached_closed_search_count(&self) -> usize {
        self.held_searches.len()
    }

    pub fn is_pending(&self) -> bool {
        self.slots
            .values()
            .any(|slot| matches!(slot.state, ReadSlotState::Pending))
    }

    /// Returns `false` while any environment read is in flight so live refresh never cancels
    /// an active read and discards scopes already read (`pingdotgg/t3code#38508`).
    pub fn should_live_refresh(&self) -> bool {
        !self.is_pending()
    }

    /// Marks `env_id` as starting a read. Returns `(generation, send_refresh_flag)`.
    ///
    /// If `mode == Manual` (or a previous manual refresh was interrupted before completing),
    /// `send_refresh_flag` is `true` so AZDO re-reads project finished states (`pingdotgg/t3code#38986`).
    pub fn begin_read(&mut self, env_id: &str, mode: IssueListRefreshMode) -> (u64, bool) {
        let slot = self.slots.entry(env_id.to_owned()).or_default();
        if mode == IssueListRefreshMode::Manual {
            slot.pending_manual_refresh = true;
        }
        slot.generation += 1;
        slot.state = ReadSlotState::Pending;
        (slot.generation, slot.pending_manual_refresh)
    }

    /// Completes a read for `env_id`. Stale generations (interrupted/superseded reads) are
    /// ignored AND keep `pending_manual_refresh` intact for the replacement read (`pingdotgg/t3code#38986`).
    pub fn finish_read(
        &mut self,
        env_id: &str,
        generation: u64,
        result: Result<IssueListResult, String>,
    ) -> bool {
        let Some(slot) = self.slots.get_mut(env_id) else {
            return false;
        };
        if slot.generation != generation {
            // Superseded/interrupted: leave pending_manual_refresh for the active replacement.
            return false;
        }
        slot.pending_manual_refresh = false;
        match result {
            Ok(answer) => {
                slot.answer = Some(answer);
                slot.state = ReadSlotState::Loaded;
            }
            Err(error) => {
                slot.state = ReadSlotState::Failed(error);
            }
        }
        true
    }

    /// Returns ONLY environments currently in `Failed` state, skipping both `Loaded` and
    /// `Pending` environments (`pingdotgg/t3code#38654`).
    pub fn failed_environments(&self) -> Vec<String> {
        self.slots
            .iter()
            .filter(|(_, slot)| matches!(slot.state, ReadSlotState::Failed(_)))
            .map(|(env_id, _)| env_id.clone())
            .collect()
    }

    /// First environment-level failure, if any.
    pub fn first_error(&self) -> Option<&str> {
        self.slots.values().find_map(|slot| match &slot.state {
            ReadSlotState::Failed(message) => Some(message.as_str()),
            _ => None,
        })
    }

    /// Answers from every environment that has one, in deterministic environment order.
    pub fn answers(&self) -> Vec<(String, IssueListResult)> {
        self.slots
            .iter()
            .filter_map(|(env_id, slot)| {
                slot.answer.as_ref().map(|ans| (env_id.clone(), ans.clone()))
            })
            .collect()
    }
}

/// Aggregated filter picker candidates (`MergedIssueCandidates` in `state/issues.ts`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergedIssueCandidates {
    pub labels: Vec<IssueLabelCandidate>,
    pub assignees: Vec<IssueAssigneeCandidate>,
    pub error: Option<String>,
    pub is_pending: bool,
    pub truncated: bool,
}

/// Per-scope candidate read tracker supporting retry-failed-only (`pingdotgg/t3code#38423`):
/// retrying skips scopes that already answered or still have a request in flight.
#[derive(Debug, Clone, Default)]
pub struct IssueCandidatesTracker {
    targets: Vec<IssueCandidatesTarget>,
    slots: HashMap<String, CandidateSlot>,
}

#[derive(Debug, Clone)]
struct CandidateSlot {
    state: ReadSlotState,
    generation: u64,
    value: Option<IssueCandidates>,
}

impl Default for CandidateSlot {
    fn default() -> Self {
        Self {
            state: ReadSlotState::Idle,
            generation: 0,
            value: None,
        }
    }
}

impl IssueCandidatesTracker {
    /// Updates the desired target scopes and returns the scopes that have not yet been requested.
    pub fn sync_targets(&mut self, targets: Vec<IssueCandidatesTarget>) -> Vec<IssueCandidatesTarget> {
        let keep: HashSet<String> = targets.iter().map(IssueCandidatesTarget::key).collect();
        self.slots.retain(|key, _| keep.contains(key));
        let mut to_fetch = Vec::new();
        for target in &targets {
            let key = target.key();
            let slot = self.slots.entry(key).or_default();
            if matches!(slot.state, ReadSlotState::Idle) {
                to_fetch.push(target.clone());
            }
        }
        self.targets = targets;
        to_fetch
    }

    pub fn begin_read(&mut self, target: &IssueCandidatesTarget) -> u64 {
        let slot = self.slots.entry(target.key()).or_default();
        slot.generation += 1;
        slot.state = ReadSlotState::Pending;
        slot.generation
    }

    pub fn finish_read(
        &mut self,
        target_key: &str,
        generation: u64,
        result: Result<IssueCandidates, String>,
    ) -> bool {
        let Some(slot) = self.slots.get_mut(target_key) else {
            return false;
        };
        if slot.generation != generation {
            return false;
        }
        match result {
            Ok(candidates) => {
                slot.value = Some(candidates);
                slot.state = ReadSlotState::Loaded;
            }
            Err(error) => {
                slot.state = ReadSlotState::Failed(error);
            }
        }
        true
    }

    /// Returns ONLY scopes currently in `Failed` state, skipping loaded and in-flight scopes
    /// (`pingdotgg/t3code#38423`).
    pub fn failed_targets(&self) -> Vec<IssueCandidatesTarget> {
        self.targets
            .iter()
            .filter(|target| {
                self.slots
                    .get(&target.key())
                    .is_some_and(|slot| matches!(slot.state, ReadSlotState::Failed(_)))
            })
            .cloned()
            .collect()
    }

    /// Merges all loaded candidates across targets, deduplicating labels by name and assignees
    /// by login, sorted alphabetically (`createMergedIssueCandidatesAtoms`).
    pub fn merged(&self) -> MergedIssueCandidates {
        let mut labels: BTreeMap<String, IssueLabelCandidate> = BTreeMap::new();
        let mut assignees: BTreeMap<String, IssueAssigneeCandidate> = BTreeMap::new();
        let mut error: Option<String> = None;
        let mut is_pending = false;
        let mut truncated = false;

        for target in &self.targets {
            let Some(slot) = self.slots.get(&target.key()) else { continue };
            match &slot.state {
                ReadSlotState::Pending => is_pending = true,
                ReadSlotState::Failed(msg) if error.is_none() => error = Some(msg.clone()),
                _ => {}
            }
            if let Some(value) = &slot.value {
                truncated |= value.truncated;
                for label in &value.labels {
                    labels.entry(label.name.clone()).or_insert_with(|| label.clone());
                }
                for actor in &value.assignees {
                    assignees.entry(actor.login.clone()).or_insert_with(|| actor.clone());
                }
            }
        }

        let mut labels: Vec<IssueLabelCandidate> = labels.into_values().collect();
        labels.sort_by_key(|a| a.name.to_lowercase());
        let mut assignees: Vec<IssueAssigneeCandidate> = assignees.into_values().collect();
        assignees.sort_by_key(|a| a.login.to_lowercase());

        MergedIssueCandidates {
            labels,
            assignees,
            error,
            is_pending,
            truncated,
        }
    }

    pub fn clear(&mut self) {
        self.targets.clear();
        self.slots.clear();
    }
}

/// Known server tracker detail strings (`issueTrackerDetailText` in `state/issueTrackers.ts`).
pub fn issue_tracker_detail_text(detail: &str) -> &str {
    detail.trim()
}
