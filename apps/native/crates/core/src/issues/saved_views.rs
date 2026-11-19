//! Named saved filter views (`packages/client-runtime/src/savedViews.ts` and
//! `apps/web/src/components/issue/issueSavedViews.ts`).
//!
//! Kept generic over `P` so both the Issues page (`IS-PAGE`) and Pull Requests page (`PR-PAGE`)
//! can share the same validation, persistence shape, and CRUD transitions.

use serde::{Deserialize, Serialize};

use super::list::IssueListPreferences;
use super::wire::IssueInvolvement;

/// `SAVED_VIEW_NAME_MAX_LENGTH` (`packages/client-runtime/src/savedViews.ts`).
pub const SAVED_VIEW_NAME_MAX_LENGTH: usize = 80;

/// Storage key for issue saved views (`ISSUE_SAVED_VIEWS.storageKey`).
pub const ISSUE_SAVED_VIEWS_KEY: &str = "t3.issues.savedViews";

/// One saved filter view persisted on this device (`SavedView<Preferences>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedView<P> {
    pub id: String,
    pub name: String,
    pub preferences: P,
}

/// Validation errors returned by `saved_view_name_error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedViewNameError {
    Empty,
    TooLong { max: usize },
    Duplicate,
}

impl SavedViewNameError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "Enter a name for this view.",
            Self::TooLong { .. } => "Use 80 characters or fewer.",
            Self::Duplicate => "A view with this name already exists.",
        }
    }
}

/// Validates a candidate saved-view name (`savedViewNameError` in `savedViews.ts`).
///
/// Rejects blank names, names longer than `SAVED_VIEW_NAME_MAX_LENGTH` UTF-16/char units, and
/// case-insensitive duplicates among other saved views (ignoring `editing_id`).
pub fn saved_view_name_error<P>(
    name: &str,
    views: &[SavedView<P>],
    editing_id: Option<&str>,
) -> Option<SavedViewNameError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some(SavedViewNameError::Empty);
    }
    if trimmed.encode_utf16().count() > SAVED_VIEW_NAME_MAX_LENGTH {
        return Some(SavedViewNameError::TooLong { max: SAVED_VIEW_NAME_MAX_LENGTH });
    }
    let lowered = trimmed.to_lowercase();
    if views
        .iter()
        .any(|view| editing_id != Some(view.id.as_str()) && view.name.to_lowercase() == lowered)
    {
        return Some(SavedViewNameError::Duplicate);
    }
    None
}

/// Decodes a persisted array of saved views leniently, dropping malformed items or views with
/// invalid names/ids so corrupted storage never breaks the menu.
pub fn decode_saved_views<P>(value: Option<serde_json::Value>) -> Vec<SavedView<P>>
where
    P: serde::de::DeserializeOwned,
{
    let Some(serde_json::Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| serde_json::from_value::<SavedView<P>>(item).ok())
        .filter(|view| {
            !view.id.is_empty()
                && !view.name.trim().is_empty()
                && view.name.trim().encode_utf16().count() <= SAVED_VIEW_NAME_MAX_LENGTH
        })
        .collect()
}

/// Resolves the currently active saved view: prefers the explicitly selected id if it still
/// exists, otherwise falls back to the first saved view matching `preferences`.
pub fn resolve_selected_view<'a, P>(
    views: &'a [SavedView<P>],
    selected_id: Option<&str>,
    preferences: &P,
    matches: impl Fn(&SavedView<P>, &P) -> bool,
) -> Option<&'a SavedView<P>> {
    selected_id
        .and_then(|id| views.iter().find(|view| view.id == id))
        .or_else(|| views.iter().find(|view| matches(view, preferences)))
}

/// Returns `true` when `selected` is active and its stored preferences no longer match `current`.
pub fn is_view_modified<P>(
    selected: Option<&SavedView<P>>,
    current: &P,
    matches: impl Fn(&SavedView<P>, &P) -> bool,
) -> bool {
    selected.is_some_and(|view| !matches(view, current))
}

/// Saves a new view with normalized preferences, returning its id on success.
pub fn create_saved_view<P>(
    views: &mut Vec<SavedView<P>>,
    id: String,
    name: &str,
    preferences: P,
) -> Result<String, SavedViewNameError> {
    if let Some(error) = saved_view_name_error(name, views, None) {
        return Err(error);
    }
    views.push(SavedView { id: id.clone(), name: name.trim().to_owned(), preferences });
    Ok(id)
}

/// Renames an existing saved view. Returns `Ok(false)` when `id` was already removed.
pub fn rename_saved_view<P>(
    views: &mut [SavedView<P>],
    id: &str,
    name: &str,
) -> Result<bool, SavedViewNameError> {
    if let Some(error) = saved_view_name_error(name, views, Some(id)) {
        return Err(error);
    }
    let Some(view) = views.iter_mut().find(|view| view.id == id) else {
        return Ok(false);
    };
    view.name = name.trim().to_owned();
    Ok(true)
}

/// Updates an existing saved view's stored preferences in place.
pub fn update_saved_view_preferences<P>(views: &mut [SavedView<P>], id: &str, preferences: P) -> bool {
    let Some(view) = views.iter_mut().find(|view| view.id == id) else {
        return false;
    };
    view.preferences = preferences;
    true
}

/// Deletes a saved view by id, returning `true` if one was removed.
pub fn delete_saved_view<P>(views: &mut Vec<SavedView<P>>, id: &str) -> bool {
    let before = views.len();
    views.retain(|view| view.id != id);
    views.len() != before
}

/// Canonical comparison key for `IssueListPreferences` (`issueViewMatches` in `issueSavedViews.ts`):
/// normalizes both sides and sorts labels so label order does not produce false mismatches.
pub fn issue_view_matches(
    view: &SavedView<IssueListPreferences>,
    preferences: &IssueListPreferences,
) -> bool {
    let canonical = |prefs: &IssueListPreferences| {
        let mut normalized = prefs.normalized();
        normalized.label.sort();
        normalized
    };
    canonical(&view.preferences) == canonical(preferences)
}

/// Default name suggested when saving the current issue view (`ISSUE_SAVED_VIEWS.suggestedName`).
pub fn issue_view_suggested_name(preferences: &IssueListPreferences) -> &'static str {
    if preferences.who == IssueInvolvement::Assigned {
        "Issues Assigned to Me"
    } else {
        ""
    }
}

/// Placeholder for the issue saved-view name input (`ISSUE_SAVED_VIEWS.placeholder`).
pub const ISSUE_SAVED_VIEW_PLACEHOLDER: &str = "Issues Assigned to Me";
