//! Saved named pull request filter views (`pingdotgg/t3code#29387`), porting
//! `apps/web/src/components/pullRequest/pullRequestSavedViews.ts` on top of the shared
//! `t3_core::issues::saved_views` primitives.

use super::ListPreferences;
use super::list::Involvement;
use crate::issues::saved_views::SavedView;

/// Storage key for pull request saved views (`PULL_REQUEST_SAVED_VIEWS.storageKey`).
pub const PR_SAVED_VIEWS_KEY: &str = "t3.pullRequests.savedViews";

/// Placeholder for the PR saved-view name input (`PULL_REQUEST_SAVED_VIEWS.placeholder`).
pub const PR_SAVED_VIEW_PLACEHOLDER: &str = "PRs Requesting My Review";

pub type PrSavedView = SavedView<ListPreferences>;

/// Canonical comparison (`pullRequestViewMatches` in `pullRequestSavedViews.ts`): normalizes
/// both sides (excluding device-level `merge_method`) and sorts `labels` so label order does not
/// trigger false modified indicators.
pub fn pull_request_view_matches(view: &PrSavedView, preferences: &ListPreferences) -> bool {
    let canonical = |prefs: &ListPreferences| {
        let mut normalized = prefs.normalized();
        normalized.labels.sort();
        normalized
    };
    canonical(&view.preferences) == canonical(preferences)
}

/// Default name suggested when saving the current PR view (`PULL_REQUEST_SAVED_VIEWS.suggestedName`).
pub fn pull_request_view_suggested_name(preferences: &ListPreferences) -> &'static str {
    if preferences.involvement == Involvement::Reviewing {
        "PRs Requesting My Review"
    } else {
        ""
    }
}

/// Applies a saved view's normalized list scope onto `current`, explicitly clearing unset filter
/// fields while preserving the device's remembered `merge_method` (`pullRequestSavedViewPatch`).
pub fn apply_saved_view_patch(current: &ListPreferences, saved: &ListPreferences) -> ListPreferences {
    let mut next = saved.normalized();
    next.merge_method = current.merge_method;
    next
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::issues::saved_views::{
        SavedViewNameError, create_saved_view, decode_saved_views, delete_saved_view,
        is_view_modified, rename_saved_view, resolve_selected_view, update_saved_view_preferences,
    };
    use crate::pull_requests::list::{ListState, Sort};
    use crate::pull_requests::wire::MergeMethod;

    #[test]
    fn matches_views_ignoring_label_order_and_merge_method() {
        let view = PrSavedView {
            id: "v1".into(),
            name: "Review queue".into(),
            preferences: ListPreferences {
                involvement: Involvement::Reviewing,
                labels: vec!["ui".into(), "bug".into()],
                sort: Sort::Updated,
                merge_method: Some(MergeMethod::Squash),
                ..Default::default()
            }
            .normalized(),
        };
        let current = ListPreferences {
            involvement: Involvement::Reviewing,
            labels: vec!["bug".into(), "ui".into()],
            sort: Sort::Updated,
            merge_method: Some(MergeMethod::Rebase),
            ..Default::default()
        };
        assert!(pull_request_view_matches(&view, &current));
        assert!(!is_view_modified(Some(&view), &current, pull_request_view_matches));

        let changed = ListPreferences { state: ListState::Merged, ..current };
        assert!(is_view_modified(Some(&view), &changed, pull_request_view_matches));
    }

    #[test]
    fn suggested_name_and_patch_preserve_merge_method_and_clear_old_filters() {
        let reviewing = ListPreferences { involvement: Involvement::Reviewing, ..Default::default() };
        assert_eq!(pull_request_view_suggested_name(&reviewing), "PRs Requesting My Review");
        assert_eq!(pull_request_view_suggested_name(&ListPreferences::default()), "");

        let current = ListPreferences {
            state: ListState::Closed,
            author: Some("alice".into()),
            labels: vec!["bug".into()],
            q: Some("crash".into()),
            merge_method: Some(MergeMethod::Squash),
            ..Default::default()
        };
        let patched = apply_saved_view_patch(&current, &reviewing);
        assert_eq!(patched.involvement, Involvement::Reviewing);
        assert_eq!(patched.state, ListState::Open);
        assert_eq!(patched.author, None);
        assert!(patched.labels.is_empty());
        assert_eq!(patched.q, None);
        assert_eq!(patched.merge_method, Some(MergeMethod::Squash));
    }

    #[test]
    fn saved_view_crud_and_lenient_decoding_reuse_shared_primitives() {
        let mut views: Vec<PrSavedView> = Vec::new();
        let prefs = ListPreferences { involvement: Involvement::Reviewing, ..Default::default() }.normalized();
        let id = create_saved_view(&mut views, "pr-1".into(), "  My Reviews ", prefs.clone()).unwrap();
        assert_eq!(views[0].name, "My Reviews");
        assert_eq!(
            create_saved_view(&mut views, "pr-2".into(), "my reviews", prefs.clone()),
            Err(SavedViewNameError::Duplicate)
        );
        assert_eq!(
            resolve_selected_view(&views, None, &prefs, pull_request_view_matches).map(|v| v.id.as_str()),
            Some("pr-1")
        );
        assert_eq!(rename_saved_view(&mut views, &id, "Urgent Reviews"), Ok(true));
        let updated_prefs = ListPreferences { state: ListState::All, ..prefs }.normalized();
        assert!(update_saved_view_preferences(&mut views, &id, updated_prefs.clone()));

        let persisted = json!([
            serde_json::to_value(&views[0]).unwrap(),
            { "id": "", "name": "Bad id", "preferences": {} },
            { "id": "bad-name", "name": "   ", "preferences": {} }
        ]);
        let decoded: Vec<PrSavedView> = decode_saved_views(Some(persisted));
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].name, "Urgent Reviews");
        assert_eq!(decoded[0].preferences, updated_prefs);

        assert!(delete_saved_view(&mut views, &id));
        assert!(views.is_empty());
    }
}
