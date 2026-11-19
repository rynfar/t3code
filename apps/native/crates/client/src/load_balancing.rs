//! Load-balanced draft environment selection (`packages/client-runtime/src/load-balancing.ts`,
//! `apps/web/src/hooks/useLoadBalancedEnvironment.ts`,
//! `apps/web/src/components/chat/useDraftEnvironmentRouting.ts`,
//! `apps/web/src/components/chat/composerSend.logic.ts`).

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::supervisor::ClientHandle;

/// `WS_METHODS.serverGetHostResources` in `packages/contracts/src/rpc.ts`.
pub const SERVER_GET_HOST_RESOURCES: &str = "server.getHostResources";
/// Freshness window mirrored by `chooseLoadBalancedEnvironment` and `useLoadBalancedEnvironment`.
pub const HOST_RESOURCES_FRESH_MS: i64 = 15_000;
/// Maximum allowed future clock skew for a host resource sample.
pub const HOST_RESOURCES_FUTURE_SKEW_MS: i64 = 5_000;
/// Timeout for each candidate host resource query during send-time refresh.
pub const HOST_RESOURCES_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
/// Default machine weight when no explicit weight is configured.
pub const DEFAULT_LOAD_BALANCING_WEIGHT: f64 = 50.0;

/// `HostResourcesSnapshot` (`packages/contracts/src/resourceTelemetry.ts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostResourcesSnapshot {
    pub sampled_at: i64,
    #[serde(default)]
    pub cpu_utilization: Option<f64>,
    pub cpu_count: u32,
    pub available_memory_bytes: u64,
    pub total_memory_bytes: u64,
}

/// One candidate environment for `choose_load_balanced_environment`.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadBalanceCandidate {
    pub environment_id: String,
    pub resources: Option<HostResourcesSnapshot>,
    /// Client receipt time avoids comparing clocks across machines.
    pub received_at: Option<i64>,
    pub weight: f64,
}

/// `chooseLoadBalancedEnvironment` (`packages/client-runtime/src/load-balancing.ts`):
/// picks the connected candidate with the highest `weight * cpuCount * (1 - cpuUtilization) * memoryAvailable`.
pub fn choose_load_balanced_environment(candidates: &[LoadBalanceCandidate], now_ms: i64) -> Option<String> {
    let mut selected: Option<&str> = None;
    let mut best_score = 0.0_f64;
    for candidate in candidates {
        let Some(resources) = candidate.resources.as_ref() else {
            continue;
        };
        let sampled_at = candidate.received_at.unwrap_or(resources.sampled_at);
        let Some(cpu_utilization) = resources.cpu_utilization else {
            continue;
        };
        if !candidate.weight.is_finite()
            || candidate.weight <= 0.0
            || now_ms.saturating_sub(sampled_at) > HOST_RESOURCES_FRESH_MS
            || sampled_at > now_ms.saturating_add(HOST_RESOURCES_FUTURE_SKEW_MS)
            || !cpu_utilization.is_finite()
            || cpu_utilization >= 0.95
            || resources.total_memory_bytes == 0
            || resources.cpu_count == 0
        {
            continue;
        }
        let memory_available = resources.available_memory_bytes as f64 / resources.total_memory_bytes as f64;
        if memory_available <= 0.05 {
            continue;
        }
        let score = candidate.weight * (resources.cpu_count as f64) * (1.0 - cpu_utilization) * memory_available;
        if score > best_score {
            selected = Some(candidate.environment_id.as_str());
            best_score = score;
        }
    }
    selected.map(str::to_owned)
}

/// Provider readiness summary for `eligible_load_balancing_candidates`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateProvider {
    pub instance_id: String,
    pub driver: String,
    pub enabled: bool,
    pub installed: bool,
    pub status: String,
    pub auth_status: Option<String>,
    pub availability: Option<String>,
}

/// One environment hosting a logical project considered for draft load balancing.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftEnvironmentCandidate {
    pub saved_id: String,
    pub environment_id: String,
    pub project_id: Option<String>,
    pub connected: bool,
    pub weight: f64,
    pub providers: Vec<CandidateProvider>,
}

/// Filters logical project environments eligible for automatic load balancing
/// (`loadBalancingCandidates` in `apps/web/src/components/chat/useDraftEnvironmentRouting.ts`).
pub fn eligible_load_balancing_candidates(
    candidates: &[DraftEnvironmentCandidate],
    referenced_thread_environment_ids: &[String],
    selected_provider_driver: &str,
    active_provider_instance_id: Option<&str>,
) -> Vec<DraftEnvironmentCandidate> {
    candidates
        .iter()
        .filter(|candidate| {
            candidate.connected
                && candidate.project_id.is_some()
                && candidate.weight.is_finite()
                && candidate.weight > 0.0
                && referenced_thread_environment_ids.iter().all(|env_id| env_id == &candidate.environment_id)
                && candidate.providers.iter().any(|provider| {
                    active_provider_instance_id.is_none_or(|wanted| provider.instance_id == wanted)
                        && provider.driver == selected_provider_driver
                        && provider.enabled
                        && provider.installed
                        && provider.status != "error"
                        && provider.auth_status.as_deref() != Some("unauthenticated")
                        && provider.availability.as_deref() != Some("unavailable")
                })
        })
        .cloned()
        .collect()
}

/// `autoEnvironmentLabel` in `useDraftEnvironmentRouting.ts:245-253`.
pub fn auto_environment_label(
    automatic_environment: bool,
    resolved_environment_id: Option<&str>,
    pending: bool,
    failed: bool,
) -> Option<&'static str> {
    if !automatic_environment {
        return None;
    }
    Some(if resolved_environment_id.is_some() {
        "Auto balance"
    } else if pending {
        "Checking machines\u{2026}"
    } else if failed {
        "Auto balance unavailable"
    } else {
        "Auto balance"
    })
}

/// Warning toast shown when automatic load balancing cannot route a draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadBalancingWarning {
    pub title: &'static str,
    pub description: &'static str,
}

/// `loadBalancingSendWarning` in `apps/web/src/components/chat/composerSend.logic.ts`.
pub fn load_balancing_send_warning(
    pending: bool,
    referenced_thread_environment_ids: &[String],
    candidate_environment_ids: &[String],
) -> LoadBalancingWarning {
    if pending {
        return LoadBalancingWarning {
            title: "Checking machine resources",
            description: "Resource checks are still running. You can choose a machine in the composer.",
        };
    }
    let no_machine_reads_every_thread = !referenced_thread_environment_ids.is_empty()
        && !candidate_environment_ids.iter().any(|env_id| {
            referenced_thread_environment_ids.iter().all(|ctx_env| ctx_env == env_id)
        });
    LoadBalancingWarning {
        title: "Choose a machine to continue",
        description: if no_machine_reads_every_thread {
            "No machine can read every attached thread. Choose a machine in the composer or remove the threads."
        } else {
            "No eligible machine has available resources. Choose a machine in the composer to override."
        },
    }
}

/// Warning shown when choosing automatic routing while composer attachments are present
/// (`useDraftEnvironmentRouting.ts:224-232`).
pub const fn attachments_block_auto_routing_warning() -> LoadBalancingWarning {
    LoadBalancingWarning {
        title: "Keep attachments on this machine",
        description: "Remove attachments before choosing automatic routing, then attach them on the selected machine.",
    }
}

/// Cached resource reading per environment (`useLoadBalancedEnvironment.ts`).
#[derive(Debug, Clone, Default)]
pub struct CachedHostResources {
    pub resources: Option<HostResourcesSnapshot>,
    pub received_at_ms: i64,
    pub pending: bool,
    pub failed: bool,
}

/// Bounded in-memory cache of per-environment host resources for unresolved automatic drafts.
#[derive(Debug, Clone, Default)]
pub struct HostResourcesCache {
    entries: HashMap<String, CachedHostResources>,
}

impl HostResourcesCache {
    pub fn mark_pending(&mut self, environment_id: &str) {
        let entry = self.entries.entry(environment_id.to_owned()).or_default();
        entry.pending = true;
    }

    pub fn record(&mut self, environment_id: &str, resources: Option<HostResourcesSnapshot>, received_at_ms: i64) {
        let failed = resources.is_none();
        self.entries.insert(
            environment_id.to_owned(),
            CachedHostResources { resources, received_at_ms, pending: false, failed },
        );
    }

    pub fn get(&self, environment_id: &str) -> Option<&CachedHostResources> {
        self.entries.get(environment_id)
    }

    /// Evaluates the current best candidate and the earliest expiry timestamp (`useLoadBalancedEnvironment.ts`).
    pub fn evaluate(
        &self,
        environment_ids: &[String],
        weights: &HashMap<String, f64>,
        now_ms: i64,
    ) -> LoadBalancedEvaluation {
        let mut pending = false;
        let mut any_failed = false;
        let mut next_expiry_ms: Option<i64> = None;
        let mut candidates = Vec::with_capacity(environment_ids.len());
        for env_id in environment_ids {
            match self.entries.get(env_id) {
                Some(entry) => {
                    pending |= entry.pending;
                    any_failed |= entry.failed;
                    if entry.resources.is_some() {
                        let expires = entry.received_at_ms.saturating_add(HOST_RESOURCES_FRESH_MS);
                        if expires >= now_ms {
                            next_expiry_ms = Some(next_expiry_ms.map_or(expires, |cur| cur.min(expires)));
                        }
                    }
                    candidates.push(LoadBalanceCandidate {
                        environment_id: env_id.clone(),
                        resources: entry.resources.clone(),
                        received_at: Some(entry.received_at_ms),
                        weight: weights.get(env_id).copied().unwrap_or(DEFAULT_LOAD_BALANCING_WEIGHT),
                    });
                }
                None => {
                    pending = true;
                }
            }
        }
        let environment_id = choose_load_balanced_environment(&candidates, now_ms);
        let failed = !pending && environment_id.is_none() && any_failed;
        LoadBalancedEvaluation { environment_id, pending, failed, next_expiry_ms }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadBalancedEvaluation {
    pub environment_id: Option<String>,
    pub pending: bool,
    pub failed: bool,
    pub next_expiry_ms: Option<i64>,
}

fn wall_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Fetches `server.getHostResources` on one environment with the 5s refresh timeout.
pub async fn fetch_host_resources(client: &ClientHandle) -> Option<HostResourcesSnapshot> {
    let reply = tokio::time::timeout(
        HOST_RESOURCES_REFRESH_TIMEOUT,
        client.request(SERVER_GET_HOST_RESOURCES, json!({})),
    )
    .await
    .ok()?
    .ok()?;
    serde_json::from_value(reply).ok()
}

/// Concurrently refreshes host resources across all `candidates` `(environment_id, client, weight)`
/// through the shared `HostResourcesCache` and selects the best environment
/// (`refreshSelection` in `useLoadBalancedEnvironment.ts`).
pub async fn refresh_load_balanced_selection(
    candidates: Vec<(String, ClientHandle, f64)>,
) -> Option<String> {
    use std::sync::{Mutex, OnceLock};

    static CACHE: OnceLock<Mutex<HostResourcesCache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HostResourcesCache::default()));

    let mut env_ids = Vec::with_capacity(candidates.len());
    let mut weights = HashMap::with_capacity(candidates.len());
    {
        let mut guard = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for (environment_id, _, weight) in &candidates {
            guard.mark_pending(environment_id);
            env_ids.push(environment_id.clone());
            weights.insert(environment_id.clone(), *weight);
        }
    }

    let futures = candidates.into_iter().map(|(environment_id, client, _weight)| async move {
        let resources = fetch_host_resources(&client).await;
        (environment_id, resources, wall_now_ms())
    });
    let resolved = futures::future::join_all(futures).await;
    let mut guard = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for (environment_id, resources, received_at_ms) in resolved {
        guard.record(&environment_id, resources, received_at_ms);
    }
    guard.evaluate(&env_ids, &weights, wall_now_ms()).environment_id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(sampled_at: i64, cpu: Option<f64>, cores: u32, avail_mem: u64, total_mem: u64) -> HostResourcesSnapshot {
        HostResourcesSnapshot {
            sampled_at,
            cpu_utilization: cpu,
            cpu_count: cores,
            available_memory_bytes: avail_mem,
            total_memory_bytes: total_mem,
        }
    }

    #[test]
    fn choose_load_balanced_environment_matches_web_scoring_and_freshness_guards() {
        let now = 100_000;
        let busy = LoadBalanceCandidate {
            environment_id: "busy".into(),
            resources: Some(snapshot(now - 1_000, Some(0.8), 8, 8_000, 16_000)),
            received_at: Some(now - 1_000),
            weight: 50.0,
        };
        let idle = LoadBalanceCandidate {
            environment_id: "idle".into(),
            resources: Some(snapshot(now - 1_000, Some(0.1), 8, 12_000, 16_000)),
            received_at: Some(now - 1_000),
            weight: 50.0,
        };
        let preferred = LoadBalanceCandidate {
            environment_id: "preferred".into(),
            resources: Some(snapshot(now - 1_000, Some(0.2), 8, 12_000, 16_000)),
            received_at: Some(now - 1_000),
            weight: 100.0,
        };
        assert_eq!(choose_load_balanced_environment(&[busy.clone(), idle.clone(), preferred], now).as_deref(), Some("preferred"));
        assert_eq!(choose_load_balanced_environment(&[busy, idle], now).as_deref(), Some("idle"));

        // Different server clock still works when client receipt timestamp is fresh, and expires after 15_000ms.
        let skewed = LoadBalanceCandidate {
            environment_id: "different-clock".into(),
            resources: Some(snapshot(1_000, Some(0.2), 4, 8_000, 16_000)),
            received_at: Some(now),
            weight: 50.0,
        };
        assert_eq!(choose_load_balanced_environment(std::slice::from_ref(&skewed), now).as_deref(), Some("different-clock"));
        assert_eq!(choose_load_balanced_environment(std::slice::from_ref(&skewed), now + 15_001), None);

        // Overloaded CPU (>= 0.95) or low memory (<= 0.05) or zero weight is excluded.
        let hot = LoadBalanceCandidate {
            environment_id: "hot".into(),
            resources: Some(snapshot(now, Some(0.95), 8, 8_000, 16_000)),
            received_at: Some(now),
            weight: 50.0,
        };
        let oom = LoadBalanceCandidate {
            environment_id: "oom".into(),
            resources: Some(snapshot(now, Some(0.1), 8, 500, 16_000)),
            received_at: Some(now),
            weight: 50.0,
        };
        assert_eq!(choose_load_balanced_environment(&[hot, oom], now), None);
    }

    #[test]
    fn eligible_candidates_and_warnings_match_use_draft_environment_routing() {
        let ready_provider = CandidateProvider {
            instance_id: "claude".into(),
            driver: "claudeAgent".into(),
            enabled: true,
            installed: true,
            status: "ready".into(),
            auth_status: Some("authenticated".into()),
            availability: Some("available".into()),
        };
        let env_a = DraftEnvironmentCandidate {
            saved_id: "s1".into(),
            environment_id: "env-a".into(),
            project_id: Some("p1".into()),
            connected: true,
            weight: 50.0,
            providers: vec![ready_provider.clone()],
        };
        let env_b = DraftEnvironmentCandidate {
            saved_id: "s2".into(),
            environment_id: "env-b".into(),
            project_id: Some("p2".into()),
            connected: true,
            weight: 0.0, // disabled weight
            providers: vec![ready_provider],
        };
        let eligible = eligible_load_balancing_candidates(&[env_a, env_b], &["env-a".into()], "claudeAgent", Some("claude"));
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].environment_id, "env-a");

        assert_eq!(auto_environment_label(false, None, false, false), None);
        assert_eq!(auto_environment_label(true, None, true, false), Some("Checking machines\u{2026}"));
        assert_eq!(auto_environment_label(true, None, false, true), Some("Auto balance unavailable"));
        assert_eq!(auto_environment_label(true, Some("env-a"), false, false), Some("Auto balance"));

        let pending_warn = load_balancing_send_warning(true, &[], &[]);
        assert_eq!(pending_warn.title, "Checking machine resources");
        let thread_warn = load_balancing_send_warning(false, &["env-a".into(), "env-b".into()], &["env-a".into()]);
        assert!(thread_warn.description.contains("No machine can read every attached thread"));
        let res_warn = load_balancing_send_warning(false, &[], &["env-a".into()]);
        assert!(res_warn.description.contains("No eligible machine has available resources"));
    }

    #[test]
    fn host_resources_cache_tracks_pending_expiry_and_failures() {
        let mut cache = HostResourcesCache::default();
        let ids = vec!["a".to_owned(), "b".to_owned()];
        let weights = HashMap::from([("a".to_owned(), 50.0), ("b".to_owned(), 100.0)]);

        let eval = cache.evaluate(&ids, &weights, 10_000);
        assert!(eval.pending);
        assert_eq!(eval.environment_id, None);

        cache.record("a", Some(snapshot(10_000, Some(0.3), 4, 8_000, 16_000)), 10_000);
        cache.record("b", None, 10_000);
        let eval = cache.evaluate(&ids, &weights, 12_000);
        assert!(!eval.pending);
        assert!(!eval.failed);
        assert_eq!(eval.environment_id.as_deref(), Some("a"));
        assert_eq!(eval.next_expiry_ms, Some(25_000));

        // After expiry, no candidate is fresh -> failed becomes true because "b" failed.
        let expired = cache.evaluate(&ids, &weights, 30_000);
        assert_eq!(expired.environment_id, None);
        assert!(expired.failed);
    }
}
