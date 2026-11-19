//! Issue tracking domain foundation: forward-compatible wire contracts, multi-environment list
//! merging/deduplication/filtering/sorting/pagination, retry-failed-only trackers, saved views,
//! and picker selection helpers.
//!
//! Shared between `IS-PAGE` (`#438`) and Wave G2 `IS-DETAIL` (`#439`) / `PR-PAGE` saved views.

pub mod list;
pub mod saved_views;
pub mod selection;
pub mod wire;

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use serde_json::json;

    use super::list::{
        HeldIssuePage, IssueCandidatesTarget, IssueCandidatesTracker, IssueListPreferences,
        IssueListReadTracker, IssueListRefreshMode, MAX_HELD_SEARCHES, PAGE_SIZE,
        ProjectDescriptor, ScopedProject, continue_issue_page, filter_held_by_host, group_issues,
        hold_issue_pages, host_options_for, issue_cursors_for_host, issue_entry_key,
        issue_filter_projects, issue_filter_scopes, issues_search_from_url, merge_issue_list_page,
        repository_change_request_host, repository_issue_scope,
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
    fn project_filter_keeps_distinct_projects_and_excludes_scratch() {
        // Mirrors `pingdotgg/t3code#38256`.
        let projects = vec![
            ProjectDescriptor {
                environment_id: "env-a".into(),
                project_id: "p1".into(),
                title: "App".into(),
                is_scratch: false,
                provider: Some("github".into()),
                host: Some("github.com".into()),
                issue_scope: Some("acme/app".into()),
            },
            ProjectDescriptor {
                environment_id: "env-b".into(),
                project_id: "p2".into(),
                title: "App".into(),
                is_scratch: false,
                provider: Some("github".into()),
                host: Some("github.com".into()),
                issue_scope: Some("acme/app".into()),
            },
            ProjectDescriptor {
                environment_id: "env-a".into(),
                project_id: "scratch".into(),
                title: "Scratch".into(),
                is_scratch: true,
                provider: None,
                host: None,
                issue_scope: None,
            },
        ];
        let env_labels = HashMap::from([
            ("env-a".to_owned(), "Laptop".to_owned()),
            ("env-b".to_owned(), "Server".to_owned()),
        ]);
        let choices = issue_filter_projects(&projects, &env_labels);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].title, "App · Laptop");
        assert_eq!(choices[1].title, "App · Server");

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
        let scopes = issue_filter_scopes(
            &held,
            &projects,
            Some(&ScopedProject {
                environment_id: "env-a".into(),
                project_id: "p1".into(),
            }),
        );
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].input.project_id, "p1");
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

        // Label order does not break matching.
        let reordered = IssueListPreferences {
            label: vec!["alpha".into(), "bug".into()],
            ..prefs.clone()
        };
        assert!(issue_view_matches(&views[0], &reordered));
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
}
