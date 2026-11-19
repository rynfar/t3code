//! Reviewer candidate merging, search gating, and request payloads.
//! Ports `packages/client-runtime/src/pullRequestReviewers.ts`.

use std::collections::HashSet;

use serde_json::{Value, json};

use super::PrRef;
use super::wire::{ReviewerCandidate, ReviewerCandidates, ReviewerKind};

fn candidate_key(candidate: &ReviewerCandidate) -> String {
    format!("{}:{}", candidate.kind.as_str(), candidate.id)
}

/// `mergeReviewerCandidates`: loaded rows retain mutation updates; host search matches append.
pub fn merge_reviewer_candidates(loaded: &[ReviewerCandidate], searched: &[ReviewerCandidate], query: &str) -> Vec<ReviewerCandidate> {
    let needle = query.trim().to_lowercase();
    let known: HashSet<String> = loaded.iter().map(candidate_key).collect();
    let host_matches: HashSet<String> = searched.iter().map(candidate_key).collect();
    let mut merged: Vec<ReviewerCandidate> = loaded
        .iter()
        .filter(|candidate| {
            host_matches.contains(&candidate_key(candidate))
                || needle.is_empty()
                || candidate.login.to_lowercase().contains(&needle)
                || candidate.name.as_deref().unwrap_or("").to_lowercase().contains(&needle)
        })
        .cloned()
        .collect();
    merged.extend(searched.iter().filter(|candidate| !known.contains(&candidate_key(candidate))).cloned());
    merged
}

/// What to ask the host for, or `None` when the loaded page already answers the query.
pub fn host_reviewer_query<'a>(loaded: Option<&ReviewerCandidates>, searchable: bool, debounced_query: &'a str) -> Option<&'a str> {
    let trimmed = debounced_query.trim();
    (searchable && loaded.is_some_and(|list| list.truncated) && !trimmed.is_empty()).then_some(trimmed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerTruncation {
    Unsearchable,
    Searchable,
    SearchFailed,
    SearchTruncated,
}

impl ReviewerTruncation {
    pub fn label(self, search_error: Option<&str>) -> String {
        match self {
            ReviewerTruncation::Unsearchable => "This repository has more people with access than are listed here. Ask for the rest on the host.".into(),
            ReviewerTruncation::Searchable => "This repository has more people with access than are listed here. Search to find them.".into(),
            ReviewerTruncation::SearchFailed => format!("Only the people already listed are matched: the host search failed. {}", search_error.unwrap_or("")).trim_end().to_owned(),
            ReviewerTruncation::SearchTruncated => "More people match than the host returned. Keep typing to narrow it down.".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerView {
    pub candidates: Vec<ReviewerCandidate>,
    pub search_pending: bool,
    pub search_error: Option<String>,
    pub truncation: Option<ReviewerTruncation>,
}

/// `reviewerCandidateView`.
pub fn reviewer_candidate_view(
    loaded: Option<&ReviewerCandidates>,
    searchable: bool,
    query: &str,
    debounced_query: &str,
    search_data: Option<&ReviewerCandidates>,
    search_is_pending: bool,
    search_error_in: Option<&str>,
) -> ReviewerView {
    let trimmed = query.trim();
    let host_query = host_reviewer_query(loaded, searchable, debounced_query);
    let searched = host_query.is_some_and(|hq| hq == trimmed);
    let candidates = merge_reviewer_candidates(
        loaded.map_or(&[], |list| list.candidates.as_slice()),
        if searched { search_data.map_or(&[], |list| list.candidates.as_slice()) } else { &[] },
        trimmed,
    );
    let searching = searchable && loaded.is_some_and(|list| list.truncated) && !trimmed.is_empty();
    let search_error = if searched { search_error_in.map(str::to_owned) } else { None };
    let truncation = if !loaded.is_some_and(|list| list.truncated) {
        None
    } else if !searchable {
        Some(ReviewerTruncation::Unsearchable)
    } else if trimmed.is_empty() {
        Some(ReviewerTruncation::Searchable)
    } else if !searched {
        None
    } else if search_error.is_some() {
        Some(ReviewerTruncation::SearchFailed)
    } else if search_data.is_some_and(|list| list.truncated) {
        Some(ReviewerTruncation::SearchTruncated)
    } else {
        None
    };
    ReviewerView {
        search_pending: candidates.is_empty() && searching && (!searched || search_is_pending),
        candidates,
        search_error,
        truncation,
    }
}

/// `PullRequestReviewerCandidatesInput`.
pub fn reviewer_candidates_input(reference: &PrRef, query: Option<&str>) -> Value {
    let mut payload = reference.payload();
    if let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) {
        let clamped: String = query.chars().take(100).collect();
        payload["query"] = json!(clamped);
    }
    payload
}

/// `PullRequestReviewerRequestInput`.
pub fn request_reviewers_input(reference: &PrRef, id: &str, kind: ReviewerKind, requested: bool) -> Value {
    let mut payload = reference.payload();
    payload["reviewers"] = json!([{ "id": id, "kind": kind.as_str() }]);
    payload["requested"] = json!(requested);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, login: &str, name: Option<&str>, requested: bool) -> ReviewerCandidate {
        ReviewerCandidate {
            id: id.into(),
            kind: ReviewerKind::User,
            is_requested: requested,
            is_bot: None,
            login: login.into(),
            name: name.map(str::to_owned),
            avatar_url: None,
        }
    }

    #[test]
    fn merges_loaded_and_host_searched_candidates_without_duplicates() {
        let loaded = vec![candidate("1", "alice", Some("Alice A"), true), candidate("2", "bob", None, false)];
        let searched = vec![candidate("1", "alice", Some("Alice A"), false), candidate("3", "carol", Some("Carol"), false)];
        let merged = merge_reviewer_candidates(&loaded, &searched, "ali");
        assert_eq!(merged.len(), 2);
        assert_eq!((merged[0].login.as_str(), merged[0].is_requested), ("alice", true), "loaded request state wins");
        assert_eq!(merged[1].login, "carol");
    }

    #[test]
    fn truncation_and_host_search_gating_follow_web_rules() {
        let truncated = ReviewerCandidates { candidates: vec![candidate("1", "alice", None, false)], truncated: true };
        assert_eq!(host_reviewer_query(Some(&truncated), true, "bob"), Some("bob"));
        assert_eq!(host_reviewer_query(Some(&truncated), false, "bob"), None);

        let view = reviewer_candidate_view(Some(&truncated), true, "bob", "bob", None, true, None);
        assert!(view.search_pending);
        assert_eq!(view.truncation, None);

        let failed = reviewer_candidate_view(Some(&truncated), true, "bob", "bob", None, false, Some("timeout"));
        assert_eq!(failed.truncation, Some(ReviewerTruncation::SearchFailed));
        assert!(failed.truncation.unwrap().label(failed.search_error.as_deref()).contains("timeout"));
    }
}
