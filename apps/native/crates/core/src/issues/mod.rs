//! Issue tracking domain foundation: forward-compatible wire contracts, multi-environment list
//! merging/deduplication/filtering/sorting/pagination, retry-failed-only trackers, saved views,
//! and picker selection helpers.
//!
//! Shared between `IS-PAGE` (`#438`) and Wave G2 `IS-DETAIL` (`#439`) / `PR-PAGE` saved views.

pub mod links;
pub mod list;
pub mod pager;
pub mod presentation;
pub mod saved_views;
pub mod selection;
pub mod wire;

/// Aggregates linked issue states for a sidebar thread row (`useSidebarLinkedIssueState`,
/// `pingdotgg/t3code#39351`): returns the first open state immediately, returns `None` while any
/// non-open state is still unloaded, otherwise returns the first non-completed closed state or the
/// first state.
pub fn aggregate_sidebar_issue_state(
    states: &[Option<&wire::IssueState>],
) -> Option<wire::IssueState> {
    if let Some(open) = states
        .iter()
        .flatten()
        .find(|state| state.category == wire::IssueStateCategory::Open)
    {
        return Some((*open).clone());
    }
    if states.iter().any(Option::is_none) {
        return None;
    }
    states
        .iter()
        .flatten()
        .find(|state| state.close_reason.as_deref() != Some("completed"))
        .or_else(|| states.iter().flatten().next())
        .map(|state| (*state).clone())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use serde_json::json;

    use super::aggregate_sidebar_issue_state;

    use super::list::{
        HeldIssuePage, ISSUE_LIST_MAX_EXCLUDED_PROJECTS, IssueCandidatesTarget,
        IssueCandidatesTracker, IssueListPreferences, IssueListReadTracker, IssueListRefreshMode,
        MAX_HELD_SEARCHES, PAGE_SIZE, ProjectDescriptor, continue_issue_page,
        every_project_hidden, filter_held_by_host, group_issues, hold_issue_pages,
        host_options_for, issue_cursors_for_host, issue_entry_key, issue_filter_projects,
        issue_filter_scopes, issue_list_targets, issues_search_from_url, merge_issue_list_page,
        repository_change_request_host, repository_issue_bindings, repository_issue_scope,
    };
    use super::saved_views::{
        SAVED_VIEW_NAME_MAX_LENGTH, SavedView, SavedViewNameError, create_saved_view,
        decode_saved_views, delete_saved_view, is_view_modified, issue_view_matches,
        issue_view_suggested_name, rename_saved_view, resolve_selected_view, saved_view_name_error,
        update_saved_view_preferences,
    };
    use super::selection::{
        IssuePicks, SelectionDraft, SelectionEntry, issue_assignees_after_picks,
        issue_labels_after_picks, issue_picks_after_failed_write, issue_picks_for,
        issue_write_after_picks,
    };
    use super::wire::{
        IssueActor, IssueAssigneeCandidate, IssueCapabilities, IssueCandidates, IssueDetail,
        IssueEventKind, IssueInvolvement, IssueLabel, IssueLabelCandidate, IssueListEntry,
        IssueListProjectError, IssueListResult, IssueListSort, IssueListState, IssueScopeRef,
        IssueState, IssueStateCategory, IssueTrackerConnectionList, IssueTrackerKind,
        IssueTrackerSummary, IssueViewerPermissions, SETTINGS_WRITE_SCOPE,
        SOURCE_CONTROL_WRITE_SCOPE, ThreadIssueKey, decode, failure_message,
        is_issues_unsupported_error, method, required_scope, thread_issue_keys_equal,
    };

    fn sample_entry(
        project_id: &str,
        tracker: IssueTrackerKind,
        host: &str,
        scope: &str,
        key: &str,
        title: &str,
        updated_at: &str,
    ) -> IssueListEntry {
        IssueListEntry {
            tracker,
            host: host.to_owned(),
            scope: scope.to_owned(),
            key: key.to_owned(),
            display_key: format!("#{key}"),
            title: title.to_owned(),
            url: format!("https://{host}/{scope}/issues/{key}"),
            author: Some(IssueActor {
                is_bot: Some(false),
                login: "alice".to_owned(),
                name: Some("Alice".to_owned()),
                avatar_url: None,
            }),
            state: IssueState {
                category: IssueStateCategory::Open,
                name: "Open".to_owned(),
                close_reason: None,
            },
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: updated_at.to_owned(),
            closed_at: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            comment_count: 2,
            project_id: project_id.to_owned(),
            project_title: format!("Project {project_id}"),
        }
    }

    fn github_tracker(host: &str, search: bool, configured: bool) -> IssueTrackerSummary {
        IssueTrackerSummary {
            tracker: IssueTrackerKind::GitHub,
            host: host.to_owned(),
            project_count: 1,
            capabilities: Some(IssueCapabilities {
                create: true,
                edit: true,
                comment: true,
                labels: true,
                assignees: true,
                reactions: true,
                templates: true,
                linked_pull_requests: true,
                events: true,
                search,
            }),
            configured,
            detail: (!configured).then(|| "Not signed in".to_owned()),
        }
    }

    #[test]
    fn wire_decoders_are_forward_compatible_and_preserve_unknown_trackers() {
        let tracker: IssueTrackerKind = decode(json!("youtrack")).unwrap();
        assert_eq!(tracker, IssueTrackerKind::Unknown("youtrack".to_owned()));
        assert_eq!(serde_json::to_value(&tracker).unwrap(), json!("youtrack"));
        assert_eq!(tracker.display_name(), "youtrack");

        let detail: IssueDetail = decode(json!({
            "tracker": "linear",
            "host": "linear.app",
            "scope": "ENG",
            "key": "ENG-42",
            "displayKey": "ENG-42",
            "title": "Forward-compatible detail",
            "url": "https://linear.app/team/issue/ENG-42",
            "state": { "category": "future-cat", "name": "In Review" },
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z",
            "projectId": "p1",
            "projectTitle": "Core",
            "events": [
                { "id": "e1", "kind": "future-event", "createdAt": "2026-01-02T00:00:00Z" },
                { "broken": true }
            ]
        }))
        .unwrap();
        assert_eq!(detail.tracker, IssueTrackerKind::Linear);
        assert_eq!(detail.state.category, IssueStateCategory::Unknown);
        assert_eq!(detail.viewer_permissions, IssueViewerPermissions::default());
        assert_eq!(detail.events.len(), 1);
        assert_eq!(detail.events[0].kind, IssueEventKind::Unknown);

        let trackers: IssueTrackerConnectionList = decode(json!({
            "trackers": [{
                "tracker": "linear",
                "displayName": "Linear",
                "host": "linear.app",
                "scopeNoun": "team",
                "credential": { "label": "Personal API key" },
                "connected": true,
                "account": "alice@example.com",
                "scopes": [{ "key": "ENG", "name": "Engineering" }]
            }],
            "bindings": { "p1": { "tracker": "linear", "scope": "ENG" } }
        }))
        .unwrap();
        assert_eq!(trackers.trackers.len(), 1);
        assert_eq!(trackers.bindings["p1"].scope, "ENG");

        assert_eq!(required_scope(method::CREATE), Some(SOURCE_CONTROL_WRITE_SCOPE));
        assert_eq!(required_scope(method::SET_STATE), Some(SOURCE_CONTROL_WRITE_SCOPE));
        assert_eq!(required_scope(method::TRACKERS_CONNECT), Some(SETTINGS_WRITE_SCOPE));
        assert_eq!(required_scope(method::TRACKERS_BIND_PROJECT), Some(SETTINGS_WRITE_SCOPE));
        assert_eq!(required_scope(method::LIST), None);
        assert_eq!(required_scope(method::DETAIL), None);

        assert!(is_issues_unsupported_error(Some("MethodNotFound"), "Method not found"));
        assert_eq!(
            failure_message(
                Some("IssueUnavailableError"),
                &json!({ "reason": "unauthenticated" }),
                "fallback",
            ),
            "This tracker has no working credentials."
        );
    }

    #[test]
    fn cross_environment_deduplication_lists_shared_issue_once() {
        // Mirrors `pingdotgg/t3code#38205`: two environments holding the same repo return the
        // same issue URL; only the first environment keeps it.
        let shared_a = sample_entry(
            "p1",
            IssueTrackerKind::GitHub,
            "github.com",
            "acme/app",
            "12",
            "Shared bug",
            "2026-01-03T00:00:00Z",
        );
        let shared_b = sample_entry(
            "p2",
            IssueTrackerKind::GitHub,
            "github.com",
            "acme/app",
            "12",
            "Shared bug",
            "2026-01-03T00:00:00Z",
        );
        let unique_b = sample_entry(
            "p2",
            IssueTrackerKind::GitHub,
            "github.com",
            "acme/app",
            "15",
            "Second bug",
            "2026-01-04T00:00:00Z",
        );

        let mut held = BTreeMap::new();
        held.insert(
            "env-a".to_owned(),
            HeldIssuePage::from_answer(IssueListResult {
                trackers: vec![github_tracker("github.com", true, true)],
                entries: vec![shared_a],
                ..Default::default()
            }),
        );
        held.insert(
            "env-b".to_owned(),
            HeldIssuePage::from_answer(IssueListResult {
                trackers: vec![github_tracker("github.com", true, true)],
                entries: vec![shared_b, unique_b],
                ..Default::default()
            }),
        );

        let groups = group_issues(&held, "", &[], IssueListSort::Updated);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].rows.len(), 2);
        assert_eq!(groups[0].rows[0].entry.key, "15");
        assert_eq!(groups[0].rows[0].environment_id, "env-b");
        assert_eq!(groups[0].rows[1].entry.key, "12");
        assert_eq!(groups[0].rows[1].environment_id, "env-a");
    }

    #[test]
    fn host_narrowing_and_invalid_host_guard() {
        // Mirrors `pingdotgg/t3code#38085` and `pingdotgg/t3code#38190`.
        let raw: serde_json::Map<String, serde_json::Value> = serde_json::from_value(json!({
            "host": "invalid host $$$",
            "key": 42,
            "status": "closed",
            "who": "assigned",
            "order": "created",
            "label": ["bug", "urgent"]
        }))
        .unwrap();
        let search = issues_search_from_url(&raw);
        assert_eq!(search.preferences.host.as_deref(), Some("invalid host $$$"));
        assert_eq!(search.key.as_deref(), Some("42"));
        assert_eq!(search.preferences.status, IssueListState::Closed);
        assert_eq!(search.preferences.who, IssueInvolvement::Assigned);
        assert_eq!(search.preferences.order, Some(IssueListSort::Created));

        let trackers = vec![
            github_tracker("github.com", true, true),
            IssueTrackerSummary {
                tracker: IssueTrackerKind::Linear,
                host: "linear.app".to_owned(),
                project_count: 1,
                capabilities: None,
                configured: false,
                detail: Some("Not connected".to_owned()),
            },
        ];
        let options = host_options_for(&trackers, Some("stale.example.com"));
        assert_eq!(options[0].host, None);
        assert_eq!(options[1].host.as_deref(), Some("stale.example.com"));
        assert_eq!(options[2].host.as_deref(), Some("github.com"));
        assert_eq!(options[3].host.as_deref(), Some("linear.app"));

        let mut held = BTreeMap::new();
        held.insert(
            "env-a".to_owned(),
            HeldIssuePage::from_answer(IssueListResult {
                trackers,
                ..Default::default()
            }),
        );
        let narrowed = filter_held_by_host(&held, Some("github.com"));
        assert_eq!(narrowed["env-a"].answer.trackers.len(), 1);
        assert_eq!(narrowed["env-a"].answer.trackers[0].host, "github.com");
    }

    #[test]
    fn project_filter_keeps_distinct_projects_and_hides_excluded_targets() {
        // Mirrors `pingdotgg/t3code#38256` and `#38799`.
        let projects = vec![
            ProjectDescriptor {
                environment_id: "env-a".into(),
                project_id: "p1".into(),
                title: "App".into(),
                is_scratch: false,
                provider: Some("github".into()),
                host: Some("github.com".into()),
                issue_scope: Some("acme/app".into()),
                issue_bindings: Vec::new(),
            },
            ProjectDescriptor {
                environment_id: "env-b".into(),
                project_id: "p2".into(),
                title: "App".into(),
                is_scratch: false,
                provider: Some("github".into()),
                host: Some("github.com".into()),
                issue_scope: Some("acme/app".into()),
                issue_bindings: Vec::new(),
            },
            ProjectDescriptor {
                environment_id: "env-a".into(),
                project_id: "scratch".into(),
                title: "Scratch".into(),
                is_scratch: true,
                provider: None,
                host: None,
                issue_scope: None,
                issue_bindings: Vec::new(),
            },
        ];
        let env_labels = HashMap::from([
            ("env-a".to_owned(), "Laptop".to_owned()),
            ("env-b".to_owned(), "Server".to_owned()),
        ]);
        let choices = issue_filter_projects(&projects, &env_labels);
        assert_eq!(choices.len(), 3);
        assert_eq!(choices[0].title, "App · Laptop");
        assert_eq!(choices[1].title, "App · Server");
        assert_eq!(choices[2].title, "Scratch");

        let base_input = IssueListPreferences::default().to_list_input(PAGE_SIZE, "", None, false);
        let envs = vec!["env-a".to_owned(), "env-b".to_owned()];
        // Hiding both env-a projects skips env-a completely while keeping env-b unfiltered.
        let hidden_env_a = vec![
            ("env-a".to_owned(), "p1".to_owned()),
            ("env-a".to_owned(), "scratch".to_owned()),
        ];
        assert!(!every_project_hidden(&choices, &hidden_env_a));
        let targets = issue_list_targets(&envs, &choices, &hidden_env_a, &base_input);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].environment_id, "env-b");
        assert!(targets[0].input.exclude_project_ids.is_empty());

        // Partial hide on env-a sets excludeProjectIds for env-a.
        let partial_hide = vec![("env-a".to_owned(), "scratch".to_owned())];
        let partial_targets = issue_list_targets(&envs, &choices, &partial_hide, &base_input);
        assert_eq!(partial_targets.len(), 2);
        assert_eq!(partial_targets[0].input.exclude_project_ids, vec!["scratch".to_owned()]);

        // Every project hidden produces zero targets.
        let all_hidden = vec![
            ("env-a".to_owned(), "p1".to_owned()),
            ("env-a".to_owned(), "scratch".to_owned()),
            ("env-b".to_owned(), "p2".to_owned()),
        ];
        assert!(every_project_hidden(&choices, &all_hidden));
        assert!(issue_list_targets(&envs, &choices, &all_hidden, &base_input).is_empty());

        // Before projects load on env-c, saved hide pairs still exclude env-c project IDs.
        let unloaded_targets = issue_list_targets(
            &["env-c".to_owned()],
            &[],
            &[("env-c".to_owned(), "p9".to_owned())],
            &base_input,
        );
        assert_eq!(unloaded_targets[0].input.exclude_project_ids, vec!["p9".to_owned()]);

        let mut held = BTreeMap::new();
        held.insert(
            "env-a".to_owned(),
            HeldIssuePage::from_answer(IssueListResult {
                trackers: vec![github_tracker("github.com", true, true)],
                scopes: Some(vec![IssueScopeRef {
                    project_id: "p1".into(),
                    tracker: IssueTrackerKind::GitHub,
                    host: Some("github.com".into()),
                    scope: "acme/app".into(),
                }]),
                ..Default::default()
            }),
        );
        let scopes = issue_filter_scopes(&held, &projects, &partial_hide);
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].input.project_id, "p1");
        let hidden_all_a = issue_filter_scopes(&held, &projects, &hidden_env_a);
        assert!(hidden_all_a.is_empty());
    }

    #[test]
    fn retry_failed_only_manual_refresh_and_closed_search_eviction() {
        // Mirrors `pingdotgg/t3code#38654`, `pingdotgg/t3code#38508`, `pingdotgg/t3code#38986`,
        // `pingdotgg/t3code#38493`, `pingdotgg/t3code#38423`.
        let mut tracker = IssueListReadTracker::default();
        let envs = vec!["env-a".to_owned(), "env-b".to_owned(), "env-c".to_owned()];
        tracker.set_search_key("k1".into(), &envs);

        let (g_a, refresh_a) = tracker.begin_read("env-a", IssueListRefreshMode::Background);
        let (g_b, _) = tracker.begin_read("env-b", IssueListRefreshMode::Background);
        let (_g_c, _) = tracker.begin_read("env-c", IssueListRefreshMode::Background);
        assert!(!refresh_a);
        assert!(!tracker.should_live_refresh());

        tracker.finish_read("env-a", g_a, Ok(IssueListResult::default()));
        tracker.finish_read("env-b", g_b, Err("timeout".into()));
        // env-c is still pending; retry must target ONLY env-b!
        assert_eq!(tracker.failed_environments(), vec!["env-b".to_owned()]);
        assert!(!tracker.should_live_refresh());

        // Manual refresh interrupted by a replacement read keeps `refresh: true` until a
        // non-interrupted completion finishes (`pingdotgg/t3code#38986`).
        let (m1, r1) = tracker.begin_read("env-a", IssueListRefreshMode::Manual);
        assert!(r1);
        let (m2, r2) = tracker.begin_read("env-a", IssueListRefreshMode::Background);
        assert!(r2, "replacement read after interrupted manual refresh still owes refresh=true");
        assert!(!tracker.finish_read("env-a", m1, Ok(IssueListResult::default())));
        assert!(tracker.finish_read("env-a", m2, Ok(IssueListResult::default())));
        let (_m3, r3) = tracker.begin_read("env-a", IssueListRefreshMode::Background);
        assert!(!r3);

        // Closed search cache stays bounded (`pingdotgg/t3code#38493`).
        for idx in 0..(MAX_HELD_SEARCHES + 8) {
            tracker.set_search_key(format!("search-{idx}"), &["env-a".to_owned()]);
            let (g, _) = tracker.begin_read("env-a", IssueListRefreshMode::Background);
            tracker.finish_read("env-a", g, Ok(IssueListResult::default()));
        }
        assert!(tracker.cached_closed_search_count() <= MAX_HELD_SEARCHES);
        tracker.prune_closed_searches();
        assert_eq!(tracker.cached_closed_search_count(), 0);

        // Candidate retry-failed-only (`pingdotgg/t3code#38423`).
        let mut candidates = IssueCandidatesTracker::default();
        let t1 = IssueCandidatesTarget {
            environment_id: "env-a".into(),
            input: IssueScopeRef {
                project_id: "p1".into(),
                tracker: IssueTrackerKind::GitHub,
                host: Some("github.com".into()),
                scope: "acme/one".into(),
            },
        };
        let t2 = IssueCandidatesTarget {
            environment_id: "env-a".into(),
            input: IssueScopeRef {
                project_id: "p2".into(),
                tracker: IssueTrackerKind::GitHub,
                host: Some("github.com".into()),
                scope: "acme/two".into(),
            },
        };
        let to_fetch = candidates.sync_targets(vec![t1.clone(), t2.clone()]);
        assert_eq!(to_fetch.len(), 2);
        let c1 = candidates.begin_read(&t1);
        let c2 = candidates.begin_read(&t2);
        candidates.finish_read(
            &t1.key(),
            c1,
            Ok(IssueCandidates {
                labels: vec![IssueLabelCandidate {
                    name: "bug".into(),
                    color: Some("ff0000".into()),
                    description: None,
                }],
                assignees: Vec::new(),
                truncated: false,
            }),
        );
        candidates.finish_read(&t2.key(), c2, Err("502".into()));
        assert_eq!(candidates.failed_targets(), vec![t2]);
        let merged = candidates.merged();
        assert_eq!(merged.labels.len(), 1);
        assert_eq!(merged.error.as_deref(), Some("502"));
    }

    #[test]
    fn pagination_merges_and_clears_recovered_continuation_errors() {
        let e1 = sample_entry(
            "p1",
            IssueTrackerKind::GitHub,
            "github.com",
            "acme/app",
            "1",
            "Old title",
            "2026-01-02T00:00:00Z",
        );
        let e2 = sample_entry(
            "p1",
            IssueTrackerKind::GitHub,
            "github.com",
            "acme/app",
            "1",
            "Updated title",
            "2026-01-04T00:00:00Z",
        );
        let e3 = sample_entry(
            "p1",
            IssueTrackerKind::GitHub,
            "github.com",
            "acme/app",
            "2",
            "Second",
            "2026-01-03T00:00:00Z",
        );
        let merged = merge_issue_list_page(std::slice::from_ref(&e1), &[e2, e3]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].title, "Updated title");
        assert_eq!(merged[1].key, "2");

        let answer = IssueListResult {
            trackers: vec![github_tracker("github.com", false, true)],
            entries: vec![e1],
            next_cursors: HashMap::from([
                ("github.com acme/app".to_owned(), "cur-1".to_owned()),
                ("linear.app ENG".to_owned(), "cur-lin".to_owned()),
            ]),
            ..Default::default()
        };
        let held = hold_issue_pages(&[("env-a".into(), answer)], &BTreeMap::new());
        let page = &held["env-a"];
        let gh_cursors = issue_cursors_for_host(&page.next_cursors, "github.com");
        assert_eq!(gh_cursors.len(), 1);

        let failed = continue_issue_page(page, Err("network".into()), &gh_cursors);
        assert_eq!(failed.more_failure.as_deref(), Some("network"));

        let next_page = IssueListResult {
            errors: vec![IssueListProjectError {
                project_id: "p1".into(),
                project_title: "App".into(),
                message: "partial".into(),
            }],
            ..Default::default()
        };
        let continued = continue_issue_page(&failed, Ok(&next_page), &gh_cursors);
        assert!(continued.more_failure.is_none());
        assert!(!continued.next_cursors.contains_key("github.com acme/app"));
        assert!(continued.next_cursors.contains_key("linear.app ENG"));
        assert_eq!(continued.errors.len(), 1);
    }

    #[test]
    fn saved_views_validation_matching_and_crud() {
        // Mirrors `pingdotgg/t3code#37963`.
        let mut views: Vec<SavedView<IssueListPreferences>> = Vec::new();
        assert_eq!(
            saved_view_name_error("   ", &views, None),
            Some(SavedViewNameError::Empty)
        );
        let too_long = "x".repeat(SAVED_VIEW_NAME_MAX_LENGTH + 1);
        assert_eq!(
            saved_view_name_error(&too_long, &views, None),
            Some(SavedViewNameError::TooLong { max: SAVED_VIEW_NAME_MAX_LENGTH })
        );

        let prefs = IssueListPreferences {
            who: IssueInvolvement::Assigned,
            status: IssueListState::Open,
            hide_projects: vec![("env-a".into(), "p2".into()), ("env-a".into(), "p1".into())],
            label: vec!["bug".into(), "alpha".into()],
            ..Default::default()
        };
        assert_eq!(issue_view_suggested_name(&prefs), "Issues Assigned to Me");
        let id = create_saved_view(&mut views, "v1".into(), "My Bugs", prefs.normalized()).unwrap();
        assert_eq!(id, "v1");
        assert_eq!(
            saved_view_name_error("my bugs", &views, None),
            Some(SavedViewNameError::Duplicate)
        );
        assert_eq!(saved_view_name_error("my bugs", &views, Some("v1")), None);

        // Label and hideProjects order do not break matching.
        let reordered = IssueListPreferences {
            label: vec!["alpha".into(), "bug".into()],
            hide_projects: vec![("env-a".into(), "p1".into()), ("env-a".into(), "p2".into())],
            ..prefs.clone()
        };
        assert!(issue_view_matches(&views[0], &reordered));
        let cleared_hidden = IssueListPreferences {
            hide_projects: Vec::new(),
            ..reordered.clone()
        };
        assert!(!issue_view_matches(&views[0], &cleared_hidden));
        let active = resolve_selected_view(&views, None, &reordered, issue_view_matches);
        assert_eq!(active.map(|v| v.id.as_str()), Some("v1"));
        assert!(!is_view_modified(active, &reordered, issue_view_matches));

        let modified_prefs = IssueListPreferences {
            status: IssueListState::Closed,
            ..reordered
        };
        assert!(is_view_modified(active, &modified_prefs, issue_view_matches));
        assert!(update_saved_view_preferences(
            &mut views,
            "v1",
            modified_prefs.normalized()
        ));
        assert!(rename_saved_view(&mut views, "v1", "Closed Bugs").unwrap());
        assert_eq!(views[0].name, "Closed Bugs");

        let encoded = serde_json::to_value(&views).unwrap();
        let decoded: Vec<SavedView<IssueListPreferences>> = decode_saved_views(Some(encoded));
        assert_eq!(decoded, views);

        assert!(delete_saved_view(&mut views, "v1"));
        assert!(views.is_empty());

        // `hideProjects` URL parsing & max-length cap.
        let mut raw_many = Vec::new();
        for i in 0..(ISSUE_LIST_MAX_EXCLUDED_PROJECTS + 25) {
            raw_many.push(json!(["env-a", format!("p-{i}")]));
        }
        let raw_search: serde_json::Map<String, serde_json::Value> = serde_json::from_value(json!({
            "hideProjects": [["env", "web"], ["env"], "api", ["env", 3]]
        }))
        .unwrap();
        let parsed = issues_search_from_url(&raw_search);
        assert_eq!(
            parsed.preferences.hide_projects,
            vec![("env".into(), "web".into()), ("env".into(), "3".into())]
        );
        let cap_search: serde_json::Map<String, serde_json::Value> = serde_json::from_value(json!({
            "hideProjects": raw_many
        }))
        .unwrap();
        assert_eq!(
            issues_search_from_url(&cap_search).preferences.hide_projects.len(),
            ISSUE_LIST_MAX_EXCLUDED_PROJECTS
        );
    }

    #[test]
    fn selection_helpers_and_key_equality() {
        let mut label_draft: SelectionDraft<IssueLabelCandidate> = SelectionDraft::new();
        label_draft.insert(
            "docs".into(),
            SelectionEntry {
                selected: true,
                item: IssueLabelCandidate {
                    name: "docs".into(),
                    color: None,
                    description: None,
                },
            },
        );
        let next = issue_labels_after_picks(
            &[IssueLabel { name: "bug".into(), color: None }],
            &label_draft,
        );
        assert_eq!(next, Some(vec!["bug".into(), "docs".into()]));

        let mut assignee_draft: SelectionDraft<IssueAssigneeCandidate> = SelectionDraft::new();
        assignee_draft.insert(
            "bob".into(),
            SelectionEntry {
                selected: true,
                item: IssueAssigneeCandidate {
                    is_bot: None,
                    login: "bob".into(),
                    name: None,
                    avatar_url: None,
                    id: "usr_bob".into(),
                },
            },
        );
        let assignees = issue_assignees_after_picks(&[], &assignee_draft);
        assert_eq!(assignees, Some(vec!["usr_bob".into()]));

        let picks = IssuePicks {
            basis: Some(1),
            draft: label_draft,
            written: Some(vec!["bug".into(), "docs".into()]),
        };
        let after_fail = issue_picks_after_failed_write(&picks, &picks);
        assert!(after_fail.written.is_none());
        assert!(after_fail.draft.is_empty());
        assert_eq!(
            issue_picks_for(&picks, 2, false).draft.len(),
            0
        );
        assert_eq!(
            issue_write_after_picks(
                Some(vec!["a".into()]),
                &["a".into()],
                None,
            ),
            None
        );

        let k1 = ThreadIssueKey {
            tracker: IssueTrackerKind::GitHub,
            host: "GitHub.com".into(),
            scope: "Acme/App".into(),
            key: "7".into(),
        };
        let k2 = ThreadIssueKey {
            tracker: IssueTrackerKind::GitHub,
            host: "github.com".into(),
            scope: "acme/app".into(),
            key: "7".into(),
        };
        assert!(thread_issue_keys_equal(&k1, &k2));
        assert_eq!(
            issue_entry_key("p1", "GitHub.com", "Acme/App", "7"),
            "p1\0github.com\0acme/app\x007"
        );
        let input = IssueListPreferences::default().to_list_input(PAGE_SIZE, "crash", None, true);
        assert_eq!(input.refresh, Some(true));
        assert_eq!(input.query.as_deref(), Some("crash"));
    }

    #[test]
    fn repository_identity_extracts_host_and_issue_scope_including_azure_devops_and_forgejo() {
        // GitHub with owner/name fallback (no displayName)
        let gh = json!({
            "canonicalKey": "github.com/acme/app",
            "provider": "github",
            "owner": "acme",
            "name": "app",
            "webUrl": "https://github.com/acme/app"
        });
        assert_eq!(
            repository_change_request_host(&gh, Some("github")).as_deref(),
            Some("github.com")
        );
        assert_eq!(repository_issue_scope(&gh).as_deref(), Some("acme/app"));

        // Forgejo preserves port from webUrl or HTTP locator.remoteUrl
        let forgejo_web = json!({
            "canonicalKey": "git.example.com/team/repo",
            "provider": "forgejo",
            "displayName": "team/repo",
            "webUrl": "https://git.example.com:3000/team/repo"
        });
        assert_eq!(
            repository_change_request_host(&forgejo_web, Some("forgejo")).as_deref(),
            Some("git.example.com:3000")
        );
        let forgejo_remote = json!({
            "canonicalKey": "git.example.com/team/repo",
            "provider": "forgejo",
            "displayName": "team/repo",
            "locator": { "remoteUrl": "http://git.example.com:8080/team/repo.git" }
        });
        assert_eq!(
            repository_change_request_host(&forgejo_remote, Some("forgejo")).as_deref(),
            Some("git.example.com:8080")
        );

        // Azure DevOps: org/project/_git/repo with percent-encoded project name
        let azdo_full = json!({
            "canonicalKey": "dev.azure.com/acme/My%20Project/_git/web",
            "provider": "azure-devops",
            "displayName": "acme/My%20Project/_git/web"
        });
        assert_eq!(
            repository_change_request_host(&azdo_full, Some("azure-devops")).as_deref(),
            Some("dev.azure.com")
        );
        assert_eq!(
            repository_issue_scope(&azdo_full).as_deref(),
            Some("acme/My Project")
        );

        // Azure DevOps short dev.azure.com: org/_git/repo -> org/repo
        let azdo_short = json!({
            "canonicalKey": "dev.azure.com/acme/_git/web",
            "provider": "azure-devops",
            "displayName": "acme/_git/web"
        });
        assert_eq!(
            repository_issue_scope(&azdo_short).as_deref(),
            Some("acme/web")
        );

        // Legacy *.visualstudio.com: /_git/web -> web, /DefaultCollection/_git/web -> web, /Proj/_git/repo -> Proj
        let azdo_legacy_bare = json!({
            "canonicalKey": "acme.visualstudio.com/_git/web",
            "provider": "azure-devops",
            "displayName": "_git/web"
        });
        assert_eq!(
            repository_issue_scope(&azdo_legacy_bare).as_deref(),
            Some("web")
        );
        let azdo_legacy_default_coll = json!({
            "canonicalKey": "acme.visualstudio.com/DefaultCollection/_git/web",
            "provider": "azure-devops",
            "displayName": "DefaultCollection/_git/web"
        });
        assert_eq!(
            repository_issue_scope(&azdo_legacy_default_coll).as_deref(),
            Some("web")
        );
        let azdo_legacy_proj = json!({
            "canonicalKey": "acme.visualstudio.com/Core%20Team/_git/repo",
            "provider": "azure-devops",
            "displayName": "Core%20Team/_git/repo"
        });
        assert_eq!(
            repository_issue_scope(&azdo_legacy_proj).as_deref(),
            Some("Core Team")
        );

        // ProjectDescriptor::from_repository_identity wires everything together
        let desc = ProjectDescriptor::from_repository_identity(
            "env-a".into(),
            "p1".into(),
            "Web".into(),
            false,
            Some(&azdo_full),
        );
        assert_eq!(desc.provider.as_deref(), Some("azure-devops"));
        assert_eq!(desc.host.as_deref(), Some("dev.azure.com"));
        assert_eq!(desc.issue_scope.as_deref(), Some("acme/My Project"));
    }

    #[test]
    fn presentation_events_write_access_and_timeline_sorting() {
        use super::presentation::{
            IssueStateTone, IssueTimelineItem, issue_author_label, issue_close_reason_label,
            issue_event_sentence, issue_state_tone, issue_write_access, merge_issue_timeline,
        };
        use super::wire::{IssueComment, IssueEvent};

        assert_eq!(issue_close_reason_label("not_planned"), "Not planned");
        assert_eq!(issue_close_reason_label("completed"), "Completed");
        assert_eq!(
            issue_state_tone(&IssueState {
                category: IssueStateCategory::Open,
                name: "In Progress".into(),
                close_reason: None,
            }),
            IssueStateTone::Open
        );
        assert_eq!(
            issue_state_tone(&IssueState {
                category: IssueStateCategory::Closed,
                name: "Done".into(),
                close_reason: Some("completed".into()),
            }),
            IssueStateTone::Completed
        );
        assert_eq!(
            issue_state_tone(&IssueState {
                category: IssueStateCategory::Closed,
                name: "Cancelled".into(),
                close_reason: Some("not_planned".into()),
            }),
            IssueStateTone::Dismissed
        );

        let closed_evt = IssueEvent {
            id: "e1".into(),
            kind: IssueEventKind::Closed,
            actor: None,
            created_at: "2026-01-02T00:00:00Z".into(),
            detail: Some("not_planned".into()),
            reference: None,
        };
        assert_eq!(issue_event_sentence(&closed_evt), "closed this as not planned");
        let moved_evt = IssueEvent {
            id: "e2".into(),
            kind: IssueEventKind::StateChanged,
            actor: None,
            created_at: "2026-01-01T12:00:00Z".into(),
            detail: Some("In Review".into()),
            reference: None,
        };
        assert_eq!(issue_event_sentence(&moved_evt), "moved this to In Review");

        let detail: IssueDetail = decode(json!({
            "tracker": "linear",
            "host": "linear.app",
            "scope": "ENG",
            "key": "ENG-5",
            "displayKey": "ENG-5",
            "title": "Workflow issue",
            "url": "https://linear.app/acme/issue/ENG-5",
            "state": { "category": "open", "name": "In Progress" },
            "states": [
                { "category": "open", "name": "Todo" },
                { "category": "open", "name": "In Progress" },
                { "category": "closed", "name": "Done", "closeReason": "completed" }
            ],
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z",
            "projectId": "p1",
            "projectTitle": "Core",
            "writable": true,
            "capabilities": {
                "create": true, "edit": true, "comment": true, "labels": true,
                "assignees": true, "reactions": false, "templates": false,
                "linkedPullRequests": false, "events": true, "search": true
            },
            "viewerPermissions": {
                "setState": true, "comment": true, "edit": false, "labels": true, "assignees": true
            }
        }))
        .unwrap();

        let access = issue_write_access(&detail, true);
        assert!(access.blocked.is_none());
        assert!(access.comment);
        assert!(!access.edit);
        assert!(!access.react);
        assert_eq!(
            access.states.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["Todo", "Done"]
        );

        let no_scope = issue_write_access(&detail, false);
        assert!(no_scope.blocked.is_some());
        assert!(!no_scope.comment);
        assert!(no_scope.states.is_empty());

        let c1 = IssueComment {
            id: "c1".into(),
            author: None,
            body: "Later comment".into(),
            created_at: "2026-01-03T00:00:00Z".into(),
            url: None,
            reactions: None,
        };
        let timeline = merge_issue_timeline(&[c1], &[closed_evt, moved_evt]);
        assert_eq!(timeline.len(), 3);
        assert!(matches!(&timeline[0], IssueTimelineItem::Event(e) if e.id == "e2"));
        assert!(matches!(&timeline[1], IssueTimelineItem::Event(e) if e.id == "e1"));
        assert!(matches!(&timeline[2], IssueTimelineItem::Comment(c) if c.id == "c1"));

        assert_eq!(issue_author_label(None), "ghost");
        assert_eq!(
            issue_author_label(Some(&IssueActor {
                is_bot: Some(false),
                login: "alice".into(),
                name: Some("Alice Smith".into()),
                avatar_url: None,
            })),
            "Alice Smith (@alice)"
        );
    }

    #[test]
    fn comment_pager_preserves_older_comments_on_failed_refresh_and_clears_on_success() {
        // Mirrors `pingdotgg/t3code#38489` and `pingdotgg/t3code#39000`.
        use super::pager::IssueCommentPager;
        use super::wire::{IssueComment, IssueCommentsPage};

        let make_comment = |id: &str| IssueComment {
            id: id.into(),
            author: None,
            body: format!("body {id}"),
            created_at: "2026-01-01T00:00:00Z".into(),
            url: None,
            reactions: None,
        };
        let detail: IssueDetail = decode(json!({
            "tracker": "github",
            "host": "github.com",
            "scope": "acme/web",
            "key": "1",
            "displayKey": "#1",
            "title": "Pager test",
            "url": "https://github.com/acme/web/issues/1",
            "state": { "category": "open", "name": "Open" },
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z",
            "projectId": "p1",
            "projectTitle": "Web",
            "comments": [{ "id": "c3", "body": "newest", "createdAt": "2026-01-03T00:00:00Z" }],
            "commentsCursor": "cur-2"
        }))
        .unwrap();

        let mut pager = IssueCommentPager::default();
        let req = pager.begin_load_older(Some(&detail), false).unwrap();
        assert_eq!(req.cursor, "cur-2");
        assert!(pager.finish_load_older(
            &req,
            Ok(IssueCommentsPage {
                comments: vec![make_comment("c2"), make_comment("c3")],
                next_cursor: Some("cur-1".into()),
            }),
        ));
        let view = pager.view(Some(&detail), false);
        assert_eq!(
            view.comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["c2", "c3"]
        );
        assert_eq!(view.next_cursor.as_deref(), Some("cur-1"));

        // Failed refresh preserves previously loaded older comments (`#39000`).
        assert!(pager.begin_refresh());
        pager.finish_refresh(false);
        let after_fail = pager.view(Some(&detail), false);
        assert_eq!(
            after_fail.comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["c2", "c3"]
        );
        assert_eq!(after_fail.next_cursor.as_deref(), Some("cur-1"));

        // In-flight loadOlder racing with a successful refresh is discarded (`#38489`).
        let stale_req = pager.begin_load_older(Some(&detail), false).unwrap();
        assert!(pager.begin_refresh());
        pager.finish_refresh(true);
        assert!(!pager.finish_load_older(
            &stale_req,
            Ok(IssueCommentsPage {
                comments: vec![make_comment("c1")],
                next_cursor: None,
            }),
        ));
        let after_success = pager.view(Some(&detail), false);
        assert_eq!(
            after_success.comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["c3"]
        );
        assert_eq!(after_success.next_cursor.as_deref(), Some("cur-2"));
    }

    #[test]
    fn issue_link_resolution_across_all_trackers_and_thread_commands() {
        // Mirrors `packages/client-runtime/src/issueLinks.test.ts` + `#38263`, `#38269`, `#38854`.
        use std::collections::HashSet;

        use super::links::{
            IssueLinkProject, is_standalone_issue_link,
            link_issue_command, linked_threads_label, may_contain_standalone_issue_link,
            resolve_issue_link_target, unlink_issue_command, visible_issue_linked_threads,
        };
        use super::wire::{IssueLinkedThread, ThreadIssueLinkSource};

        let gh_proj = |id: &str, host: &str, repo: &str| IssueLinkProject {
            id: id.into(),
            environment_id: "env-1".into(),
            repository_identity: Some(json!({
                "canonicalKey": format!("{host}/{repo}"),
                "provider": "github",
                "displayName": repo,
            })),
        };
        let projects = vec![
            gh_proj("p1", "github.com", "acme/api"),
            gh_proj("p2", "github.com", "acme/web"),
            gh_proj("ghe-bare", "github.example.com", "acme/web"),
            gh_proj("ghe-port", "github.example.com:8443", "acme/web"),
            gh_proj("ghe-custom", "code.acme.test", "acme/web"),
            IssueLinkProject {
                id: "gl".into(),
                environment_id: "env-1".into(),
                repository_identity: Some(json!({
                    "canonicalKey": "gitlab.corp.test/group/sub/app",
                    "provider": "gitlab",
                    "displayName": "group/sub/app",
                })),
            },
            IssueLinkProject {
                id: "azdo-legacy".into(),
                environment_id: "env-1".into(),
                repository_identity: Some(json!({
                    "canonicalKey": "acme.visualstudio.com/DefaultCollection/_git/Web%20App",
                    "provider": "azure-devops",
                    "displayName": "DefaultCollection/_git/Web%20App",
                })),
            },
            IssueLinkProject {
                id: "fj".into(),
                environment_id: "env-1".into(),
                repository_identity: Some(json!({
                    "canonicalKey": "codeberg.org/team/tool",
                    "provider": "forgejo",
                    "displayName": "team/tool",
                })),
            },
        ];

        // Exact repo match & lender fallback on github.com.
        let r_own = resolve_issue_link_target(
            "https://github.com/acme/web/issues/7",
            &projects,
            None,
            None,
        )
        .unwrap();
        assert_eq!(r_own.project_id, "p2");
        assert_eq!(r_own.reference.key, "7");
        let r_lend = resolve_issue_link_target(
            "https://github.com/other/repo/issues/9",
            &projects,
            None,
            None,
        )
        .unwrap();
        assert_eq!(r_lend.project_id, "p1");
        assert_eq!(r_lend.reference.scope, "other/repo");

        // Ported GHE & custom GHE hostname vouched by project (`#38269`).
        let r_port = resolve_issue_link_target(
            "https://github.example.com:8443/acme/web/issues/7",
            &projects,
            None,
            None,
        )
        .unwrap();
        assert_eq!(r_port.project_id, "ghe-port");
        let r_custom = resolve_issue_link_target(
            "https://code.acme.test/acme/web/issues/7",
            &projects,
            None,
            None,
        )
        .unwrap();
        assert_eq!(r_custom.project_id, "ghe-custom");
        assert_eq!(r_custom.reference.host.as_deref(), Some("code.acme.test"));

        // GitLab nested group work item.
        let r_gl = resolve_issue_link_target(
            "https://gitlab.corp.test/group/sub/app/-/work_items/14",
            &projects,
            None,
            None,
        )
        .unwrap();
        assert_eq!(r_gl.project_id, "gl");
        assert_eq!(r_gl.reference.tracker, IssueTrackerKind::GitLab);
        assert_eq!(r_gl.reference.key, "14");

        // Azure DevOps legacy host with DefaultCollection alias and encoded space (`#38263`).
        let r_az = resolve_issue_link_target(
            "https://acme.visualstudio.com/DefaultCollection/Web%20App/_workitems/edit/99",
            &projects,
            None,
            None,
        )
        .unwrap();
        assert_eq!(r_az.project_id, "azdo-legacy");
        assert_eq!(r_az.reference.scope, "web app");
        assert_eq!(r_az.reference.key, "99");

        // Forgejo / Codeberg.
        let r_fj = resolve_issue_link_target(
            "https://codeberg.org/team/tool/issues/3",
            &projects,
            None,
            None,
        )
        .unwrap();
        assert_eq!(r_fj.project_id, "fj");
        assert_eq!(r_fj.reference.tracker, IssueTrackerKind::Forgejo);

        // TanStack Source strips owner `@` and resolves against canonical project bindings (`#40741`).
        let ts_projects = [IssueLinkProject {
            id: "ts".into(),
            environment_id: "env-1".into(),
            repository_identity: Some(json!({
                "canonicalKey": "source.tanstack.com/tanstack/router",
                "provider": "tanstack-source",
                "displayName": "@TanStack/router",
            })),
        }];
        let r_ts = resolve_issue_link_target(
            "https://source.tanstack.com/@TanStack/router/issues/91",
            &ts_projects,
            None,
            None,
        )
        .unwrap();
        assert_eq!(r_ts.project_id, "ts");
        assert_eq!(r_ts.reference.tracker, IssueTrackerKind::TanStackSource);
        assert_eq!(r_ts.reference.scope, "tanstack/router");
        assert_eq!(r_ts.reference.key, "91");

        // Linear standalone tracker (`#38854`).
        let trackers: IssueTrackerConnectionList = decode(json!({
            "trackers": [{
                "tracker": "linear",
                "displayName": "Linear",
                "host": "linear.app",
                "scopeNoun": "team",
                "credential": { "label": "Personal API key" },
                "connected": true,
                "account": "alice",
                "workspace": "acme",
                "scopes": []
            }],
            "bindings": {
                "p1": { "tracker": "linear", "scope": "API", "additionalScopes": ["OPS"] },
                "p2": { "tracker": "linear", "scope": "WEB" }
            }
        }))
        .unwrap();
        assert!(is_standalone_issue_link("https://linear.app/acme/issue/WEB-17/fix-it"));
        assert!(may_contain_standalone_issue_link("See https://linear.app/acme/issue/WEB-17"));
        let r_lin = resolve_issue_link_target(
            "https://linear.app/acme/issue/WEB-17/fix-it",
            &projects,
            None,
            Some(&trackers),
        )
        .unwrap();
        assert_eq!(r_lin.project_id, "p2");
        assert_eq!(r_lin.reference.scope, "WEB");
        assert_eq!(r_lin.reference.key, "WEB-17");
        let r_lin_additional = resolve_issue_link_target(
            "https://linear.app/acme/issue/OPS-9/alert",
            &projects,
            Some("p1"),
            Some(&trackers),
        )
        .unwrap();
        assert_eq!(r_lin_additional.project_id, "p1");
        assert_eq!(r_lin_additional.reference.scope, "OPS");
        assert_eq!(r_lin_additional.reference.key, "OPS-9");

        // Foreign workspace, lookalike host, or PR URL stay ordinary links.
        assert!(resolve_issue_link_target(
            "https://linear.app/othercorp/issue/WEB-17",
            &projects,
            None,
            Some(&trackers),
        )
        .is_none());
        assert!(resolve_issue_link_target(
            "https://github.com.evil.example/acme/web/issues/7",
            &projects,
            None,
            Some(&trackers),
        )
        .is_none());
        assert!(resolve_issue_link_target(
            "https://github.com/acme/web/pull/7",
            &projects,
            None,
            Some(&trackers),
        )
        .is_none());

        // Commands + synced event + linked threads filter.
        let key = ThreadIssueKey {
            tracker: IssueTrackerKind::GitHub,
            host: "github.com".into(),
            scope: "acme/web".into(),
            key: "7".into(),
        };
        let link_cmd = link_issue_command(
            "cmd-1",
            "thr-1",
            &key,
            "p2",
            "https://github.com/acme/web/issues/7",
            "#7",
            "Bug",
            ThreadIssueLinkSource::Manual,
        );
        assert_eq!(link_cmd["type"], "thread.issue.link");
        assert_eq!(link_cmd["source"], "manual");
        let unlink_cmd = unlink_issue_command("cmd-2", "thr-1", &key);
        assert_eq!(unlink_cmd["type"], "thread.issue.unlink");

        let synced = json!({
            "id": "evt-1",
            "threadId": "thr-1",
            "occurredAt": "2026-01-01T00:00:00Z",
            "type": "thread.issue-synced",
            "payload": {
                "id": "thr-1",
                "projectId": "p2",
                "title": "Fix bug",
                "providerInstanceId": "codex",
                "modelSelection": { "provider": "codex", "model": "gpt-5.4" },
                "runtimeMode": "full-access",
                "interactionMode": "default",
                "branch": null,
                "worktreePath": null,
                "lineage": { "rootThreadId": "thr-1" },
                "createdAt": "2026-01-01T00:00:00Z",
                "updatedAt": "2026-01-01T00:00:01Z",
                "archivedAt": null,
                "issues": [{
                    "tracker": "github", "host": "github.com", "scope": "acme/web", "key": "7",
                    "projectId": "p2", "url": "https://github.com/acme/web/issues/7",
                    "source": "manual", "linkedAt": "2026-01-01T00:00:00Z",
                    "displayKey": "#7", "title": "Bug"
                }]
            }
        });
        let domain_event: crate::thread::DomainEvent = serde_json::from_value(synced).unwrap();
        assert!(matches!(
            domain_event.body,
            crate::thread::EventBody::ThreadReplaced(ref thread) if thread.id == "thr-1"
        ));

        let rows = vec![
            IssueLinkedThread { id: "t1".into(), project_id: "p1".into(), title: "A".into(), archived_at: None, settled: false },
            IssueLinkedThread { id: "t2".into(), project_id: "p1".into(), title: "B".into(), archived_at: None, settled: false },
        ];
        let loaded = HashSet::from(["t1".to_owned(), "t2".to_owned()]);
        let hidden = HashSet::from(["t2".to_owned()]);
        assert_eq!(visible_issue_linked_threads(&rows, &loaded, &hidden, false).len(), 2);
        let vis = visible_issue_linked_threads(&rows, &loaded, &hidden, true);
        assert_eq!(vis.len(), 1);
        assert_eq!(vis[0].id, "t1");
        assert_eq!(linked_threads_label(1), "Linked from 1 thread");
        assert_eq!(linked_threads_label(3), "Linked from 3 threads");
    }

    #[test]
    fn fork_upstream_issue_bindings_and_accent_folding_matcher() {
        use super::links::{
            IssueLinkProject, normalize_search_text, resolve_issue_link_target, thread_link_matches,
        };

        // `#39159`: fork resolves both its own and same-host upstream issues + filter scopes.
        let fork_identity = json!({
            "canonicalKey": "github.com/forkuser/repo",
            "provider": "github",
            "displayName": "forkuser/repo",
            "locator": { "remoteUrl": "https://github.com/forkuser/repo.git" },
            "origin": {
                "canonicalKey": "github.com/upstream/repo",
                "displayName": "upstream/repo",
                "remoteUrl": "https://github.com/upstream/repo.git"
            }
        });
        let bindings = repository_issue_bindings(&fork_identity);
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].scope, "forkuser/repo");
        assert_eq!(bindings[1].scope, "upstream/repo");

        let cross_host_fork = json!({
            "canonicalKey": "github.com/forkuser/repo",
            "provider": "github",
            "displayName": "forkuser/repo",
            "origin": {
                "canonicalKey": "gitlab.com/upstream/repo",
                "displayName": "upstream/repo"
            }
        });
        assert_eq!(repository_issue_bindings(&cross_host_fork).len(), 1);

        let desc = ProjectDescriptor::from_repository_identity(
            "env-a".into(),
            "fork-p".into(),
            "Fork".into(),
            false,
            Some(&fork_identity),
        );
        let mut held = BTreeMap::new();
        held.insert(
            "env-a".to_owned(),
            HeldIssuePage::from_answer(IssueListResult {
                trackers: vec![github_tracker("github.com", true, true)],
                ..Default::default()
            }),
        );
        let scopes = issue_filter_scopes(&held, &[desc], &[]);
        assert_eq!(scopes.len(), 2);
        assert_eq!(scopes[1].input.scope, "upstream/repo");

        let link_projects = vec![IssueLinkProject {
            id: "fork-p".into(),
            environment_id: "env-a".into(),
            repository_identity: Some(fork_identity),
        }];
        let resolved_upstream = resolve_issue_link_target(
            "https://github.com/upstream/repo/issues/88",
            &link_projects,
            None,
            None,
        )
        .unwrap();
        assert_eq!(resolved_upstream.project_id, "fork-p");
        assert_eq!(resolved_upstream.reference.scope, "upstream/repo");
        assert_eq!(resolved_upstream.reference.key, "88");

        // `#39557`: accent folding in ThreadLinkPicker.
        assert_eq!(normalize_search_text("  Café  "), "cafe");
        assert!(thread_link_matches("cafe", "Café layout bug"));
        assert!(thread_link_matches("resume", "Fix Résumé parser"));
        assert!(thread_link_matches("café", "Cafe notes"));
        assert!(!thread_link_matches("xyz", "Café layout bug"));

        // `#39351`: grouped sidebar issue state aggregation.
        let closed_completed = IssueState {
            category: IssueStateCategory::Closed,
            name: "Done".into(),
            close_reason: Some("completed".into()),
        };
        let closed_wontfix = IssueState {
            category: IssueStateCategory::Closed,
            name: "Canceled".into(),
            close_reason: Some("not_planned".into()),
        };
        let open_state = IssueState {
            category: IssueStateCategory::Open,
            name: "Open".into(),
            close_reason: None,
        };
        assert_eq!(
            aggregate_sidebar_issue_state(&[Some(&closed_completed), None, Some(&open_state)]),
            Some(open_state)
        );
        assert_eq!(
            aggregate_sidebar_issue_state(&[Some(&closed_completed), None]),
            None
        );
        assert_eq!(
            aggregate_sidebar_issue_state(&[Some(&closed_completed), Some(&closed_wontfix)]),
            Some(closed_wontfix)
        );
        assert_eq!(
            aggregate_sidebar_issue_state(&[Some(&closed_completed), Some(&closed_completed)]),
            Some(closed_completed)
        );
    }
}
