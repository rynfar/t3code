//! Parent-aware tag visibility from `packages/client-runtime/src/state/threadTagFilter.ts`,
//! with route guards from `apps/web/src/components/ThreadRouteView.tsx`.

use std::collections::{HashMap, HashSet};

use crate::shell::ThreadShell;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThreadKey {
    pub environment_id: String,
    pub thread_id: String,
}

impl ThreadKey {
    pub fn new(environment_id: &str, thread_id: &str) -> Self {
        Self { environment_id: environment_id.into(), thread_id: thread_id.into() }
    }
}

pub struct Environment<'a> {
    pub environment_id: &'a str,
    pub threads: &'a [&'a ThreadShell],
    pub hidden_tags: Option<&'a [String]>,
}

pub fn visibility_parent(environment_id: &str, thread: &ThreadShell) -> Option<ThreadKey> {
    if !thread.lineage.is_listed()
        && let Some(parent) = &thread.lineage.parent_thread_id
    {
        return Some(ThreadKey::new(environment_id, parent));
    }
    let parent = thread.delegated_from.as_ref()?;
    Some(ThreadKey::new(parent.get("environmentId")?.as_str()?, parent.get("threadId")?.as_str()?))
}

pub fn hidden_thread_keys(
    environments: &[Environment<'_>],
    already_hidden: &HashSet<ThreadKey>,
    loaded_elsewhere: &HashSet<ThreadKey>,
    hiding_anywhere: bool,
) -> HashSet<ThreadKey> {
    if already_hidden.is_empty() && !hiding_anywhere {
        return HashSet::new();
    }
    let mut entries = HashMap::new();
    for environment in environments {
        for thread in environment.threads {
            entries.insert(ThreadKey::new(environment.environment_id, &thread.id), (*thread, environment));
        }
    }
    fn is_hidden(
        key: &ThreadKey,
        entries: &HashMap<ThreadKey, (&ThreadShell, &Environment<'_>)>,
        known: &mut HashMap<ThreadKey, bool>,
        already_hidden: &HashSet<ThreadKey>,
        loaded_elsewhere: &HashSet<ThreadKey>,
        hiding_anywhere: bool,
    ) -> bool {
        if already_hidden.contains(key) {
            return true;
        }
        if let Some(hidden) = known.get(key) {
            return *hidden;
        }
        let Some((thread, environment)) = entries.get(key) else { return false };
        let parent = visibility_parent(environment.environment_id, thread);
        let follows = parent.as_ref().is_some_and(|parent| entries.contains_key(parent) || already_hidden.contains(parent));
        let hidden = if follows {
            // Seed before following a malformed cycle, as the TS selector does.
            known.insert(key.clone(), false);
            is_hidden(parent.as_ref().unwrap(), entries, known, already_hidden, loaded_elsewhere, hiding_anywhere)
        } else if parent.as_ref().is_some_and(|parent| loaded_elsewhere.contains(parent)) {
            false
        } else if parent.is_some() && hiding_anywhere {
            true
        } else {
            environment.hidden_tags.is_some_and(|tags| tags.iter().any(|tag| thread.tags.contains(tag)))
        };
        known.insert(key.clone(), hidden);
        hidden
    }
    let mut known = HashMap::new();
    entries.keys().filter(|key| is_hidden(key, &entries, &mut known, already_hidden, loaded_elsewhere, hiding_anywhere)).cloned().collect()
}

pub fn visible_linked_threads<'a, Row>(
    rows: &'a [Row],
    environment_id: &str,
    id: impl Fn(&Row) -> &str,
    loaded: &HashSet<ThreadKey>,
    hidden: &HashSet<ThreadKey>,
    hiding_anywhere: bool,
) -> Vec<&'a Row> {
    rows.iter()
        .filter(|row| {
            let key = ThreadKey::new(environment_id, id(row));
            if loaded.contains(&key) { !hidden.contains(&key) } else { !hiding_anywhere }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteVisibility {
    Visible,
    Hidden,
    Loading,
}

pub fn route_visibility(id: &str, loaded: bool, hidden: &HashSet<String>, hiding: bool) -> RouteVisibility {
    if hidden.contains(id) {
        RouteVisibility::Hidden
    } else if hiding && !loaded {
        RouteVisibility::Loading
    } else {
        RouteVisibility::Visible
    }
}

/// Only a known visible thread can open a new route; an existing route waits while loading.
pub fn route_visible(id: &str, loaded: bool, hidden: &HashSet<String>, hiding: bool) -> bool {
    route_visibility(id, loaded, hidden, hiding) == RouteVisibility::Visible
}

#[cfg(test)]
mod tests;
