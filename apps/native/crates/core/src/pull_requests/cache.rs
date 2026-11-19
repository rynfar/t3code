//! Persisted cold-start pull request list and diff-stat cache. Ports
//! `packages/client-runtime/src/state/pullRequestStatsPersistence.ts` and the persisted list
//! query cache in `packages/client-runtime/src/state/pullRequests.ts`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::list::needs_stats;
use super::wire::{ListResult, ListStats};

pub const LIST_CACHE_KEY: &str = "t3.pullRequests.listCache.v1";
pub const STATS_CACHE_KEY: &str = "t3.pullRequests.statsCache.v1";

pub const MAX_CACHED_SCOPES: usize = 8;
pub const MAX_CACHED_STATS: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedListScope {
    pub saved_at_ms: i64,
    pub result: ListResult,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersistedListCache {
    pub version: u32,
    pub scopes: HashMap<String, CachedListScope>,
}

impl PersistedListCache {
    pub const VERSION: u32 = 1;

    pub fn from_value(value: Option<Value>) -> Self {
        value
            .and_then(|v| serde_json::from_value::<Self>(v).ok())
            .filter(|cache| cache.version == Self::VERSION)
            .unwrap_or_else(|| Self { version: Self::VERSION, scopes: HashMap::new() })
    }

    pub fn scope_key(environment_id: &str, scope: &str) -> String {
        format!("{environment_id}|{scope}")
    }

    pub fn get(&self, environment_id: &str, scope: &str) -> Option<&ListResult> {
        self.scopes.get(&Self::scope_key(environment_id, scope)).map(|entry| &entry.result)
    }

    pub fn put(&mut self, environment_id: &str, scope: &str, result: ListResult, now_ms: i64) {
        self.version = Self::VERSION;
        self.scopes.insert(Self::scope_key(environment_id, scope), CachedListScope { saved_at_ms: now_ms, result });
        while self.scopes.len() > MAX_CACHED_SCOPES {
            let oldest = self
                .scopes
                .iter()
                .min_by_key(|(_, entry)| entry.saved_at_ms)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    self.scopes.remove(&key);
                }
                None => break,
            }
        }
    }

    pub fn clear_environment(&mut self, environment_id: &str) {
        let prefix = format!("{environment_id}|");
        self.scopes.retain(|key, _| !key.starts_with(&prefix));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedDiffStat {
    pub additions: u64,
    pub deletions: u64,
    pub saved_at_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersistedStatsCache {
    pub version: u32,
    pub entries: HashMap<String, CachedDiffStat>,
}

impl PersistedStatsCache {
    pub const VERSION: u32 = 1;

    pub fn from_value(value: Option<Value>) -> Self {
        value
            .and_then(|v| serde_json::from_value::<Self>(v).ok())
            .filter(|cache| cache.version == Self::VERSION)
            .unwrap_or_else(|| Self { version: Self::VERSION, entries: HashMap::new() })
    }

    pub fn stat_key(environment_id: &str, project_id: &str, repository: &str, number: u64) -> String {
        format!("{environment_id}|{project_id}|{}|{number}", repository.to_lowercase())
    }

    pub fn get(&self, environment_id: &str, project_id: &str, repository: &str, number: u64) -> Option<(u64, u64)> {
        let entry = self.entries.get(&Self::stat_key(environment_id, project_id, repository, number))?;
        Some((entry.additions, entry.deletions))
    }

    pub fn record(&mut self, environment_id: &str, stats: &ListStats, now_ms: i64) {
        self.version = Self::VERSION;
        for stat in &stats.stats {
            let key = Self::stat_key(environment_id, &stat.project_id, &stat.repository, stat.number);
            self.entries.insert(key, CachedDiffStat { additions: stat.additions, deletions: stat.deletions, saved_at_ms: now_ms });
        }
        self.prune();
    }

    pub fn hydrate_list(&self, environment_id: &str, result: &mut ListResult) -> usize {
        let mut hydrated = 0;
        for entry in result.entries.iter_mut().filter(|entry| needs_stats(entry)) {
            if let Some((additions, deletions)) = self.get(environment_id, &entry.project_id, &entry.repository, entry.number) {
                entry.additions = additions;
                entry.deletions = deletions;
                hydrated += 1;
            }
        }
        hydrated
    }

    pub fn clear_environment(&mut self, environment_id: &str) {
        let prefix = format!("{environment_id}|");
        self.entries.retain(|key, _| !key.starts_with(&prefix));
    }

    fn prune(&mut self) {
        if self.entries.len() <= MAX_CACHED_STATS {
            return;
        }
        let mut by_age: Vec<(String, i64)> = self.entries.iter().map(|(key, entry)| (key.clone(), entry.saved_at_ms)).collect();
        by_age.sort_by_key(|(_, saved_at)| *saved_at);
        let remove = self.entries.len().saturating_sub(MAX_CACHED_STATS);
        for (key, _) in by_age.into_iter().take(remove) {
            self.entries.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::pull_requests::wire::{DiffStat, ListEntry};

    fn entry(number: u64, additions: u64, deletions: u64) -> ListEntry {
        serde_json::from_value(json!({
            "provider": "github", "host": "github.com", "projectId": "p1", "projectTitle": "App", "repository": "Acme/App", "number": number,
            "title": format!("PR #{number}"), "url": format!("https://github.com/Acme/App/pull/{number}"), "author": null,
            "headBranch": "feat", "baseBranch": "main", "state": "open", "isDraft": false, "mergeability": "mergeable",
            "additions": additions, "deletions": deletions, "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
            "viewerReviewRequested": false, "labels": []
        }))
        .unwrap()
    }

    #[test]
    fn list_cache_round_trips_evicts_oldest_and_isolates_environments() {
        let mut cache = PersistedListCache::from_value(None);
        for index in 0..10 {
            let list = ListResult { entries: vec![entry(index as u64 + 1, 0, 0)], ..Default::default() };
            cache.put("env-a", &format!("scope-{index}"), list, index as i64 * 10);
        }
        assert_eq!(cache.scopes.len(), MAX_CACHED_SCOPES);
        assert!(cache.get("env-a", "scope-0").is_none(), "oldest scope evicted");
        assert!(cache.get("env-a", "scope-9").is_some());
        assert!(cache.get("env-b", "scope-9").is_none(), "environments stay isolated");

        let serialized = serde_json::to_value(&cache).unwrap();
        let reloaded = PersistedListCache::from_value(Some(serialized));
        assert_eq!(reloaded, cache);
        assert!(PersistedListCache::from_value(Some(json!({ "version": 999 }))).scopes.is_empty());
        assert!(PersistedListCache::from_value(Some(json!("corrupt"))).scopes.is_empty());
    }

    #[test]
    fn stats_cache_hydrates_unmeasured_entries_case_insensitively() {
        let mut stats_cache = PersistedStatsCache::from_value(None);
        stats_cache.record(
            "env-a",
            &ListStats {
                stats: vec![DiffStat { project_id: "p1".into(), repository: "acme/app".into(), number: 42, additions: 19, deletions: 7 }],
            },
            1000,
        );
        let mut list = ListResult { entries: vec![entry(42, 0, 0), entry(43, 5, 1)], ..Default::default() };
        assert_eq!(stats_cache.hydrate_list("env-a", &mut list), 1);
        assert_eq!((list.entries[0].additions, list.entries[0].deletions), (19, 7));
        assert_eq!((list.entries[1].additions, list.entries[1].deletions), (5, 1));
        assert_eq!(stats_cache.hydrate_list("env-b", &mut list), 0);
    }
}
