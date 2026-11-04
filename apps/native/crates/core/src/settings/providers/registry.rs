//! `AcpRegistrySearchStep.tsx`, `AddProviderInstanceDialog.tsx` and contracts/acpRegistry.ts.
use super::{ACP_REGISTRY, auth::AuthState};
use crate::server_config::{ProviderInstanceConfig, ServerProvider};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;

pub const SEARCH: &str = "server.searchAcpRegistry";
pub const PREPARE: &str = "server.prepareAcpRegistryAgent";
pub const DEBOUNCE_MS: u64 = 300;
pub const MAX_AGENTS: usize = 512;
// providerClients.ts and provider-{cursor,grok,opencode,pi}/src/client.ts metadata.
pub const FIRST_CLASS_IDS: &[&str] = &[
    "codex-acp",
    "claude-acp",
    "cursor",
    "grok-build",
    "opencode",
    "opencode-v2",
    "antigravity-acp",
    "pi-acp",
];

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub website: Option<String>,
    pub repository: Option<String>,
    pub icon: Option<String>,
    pub distribution: String,
}
#[derive(Debug, Deserialize)]
pub struct SearchResult {
    pub agents: Vec<Agent>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    pub agent_id: String,
    pub version: String,
    pub distribution: String,
    pub prepared: bool,
}
impl Agent {
    pub fn prepared(mut self, result: Prepared) -> Result<Self, String> {
        if !result.prepared {
            return Err("The ACP could not be prepared.".into());
        }
        self.id = result.agent_id;
        self.version = result.version;
        self.distribution = result.distribution;
        Ok(self)
    }
    pub fn config(&self) -> Value {
        let mut config = json!({"agentId":self.id,"distribution":"auto"});
        if let Some(icon) = self.icon.as_deref().and_then(official_icon) {
            config["registryIconUrl"] = json!(icon);
        }
        config
    }
}
pub fn official_icon(icon: &str) -> Option<String> {
    let url = url::Url::parse(icon).ok()?;
    (url.scheme() == "https"
        && url.host_str() == Some("cdn.agentclientprotocol.com")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.into())
}
pub fn icon_url(agent: &Agent) -> Option<String> {
    agent.icon.as_deref().and_then(official_icon).or_else(|| {
        let id = agent.id.trim();
        let mut chars = id.chars();
        (chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && chars.all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            }))
        .then(|| format!("https://cdn.agentclientprotocol.com/registry/v1/latest/{id}.svg"))
    })
}
/// Registry website/repository are HTTP(S) links, not executable OS URL schemes.
pub fn external_url(value: Option<&str>) -> Option<String> {
    let url = url::Url::parse(value?).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.into())
}
pub fn configured<'a>(
    mut instances: impl Iterator<Item = &'a ProviderInstanceConfig>,
    id: &str,
) -> bool {
    instances.any(|i| {
        i.driver == ACP_REGISTRY
            && i.config
                .as_ref()
                .is_some_and(|c| c["source"] != "local" && c["agentId"] == id)
    })
}
pub fn instance_id(label: &str, existing: &[String]) -> String {
    super::available_instance_id(super::derive_instance_id(ACP_REGISTRY, label), existing)
}

pub fn search_payload(query: &str) -> Value {
    if query.trim().is_empty() {
        json!({"query":"","limit":MAX_AGENTS})
    } else {
        json!({"query":query.trim()})
    }
}

pub const ICON_RETRY_MS: u64 = 30_000;
#[derive(Default)]
pub struct IconRetry {
    retry_at: Option<u64>,
}
impl IconRetry {
    pub fn failed(&mut self, now: u64) {
        self.retry_at = Some(now.saturating_add(ICON_RETRY_MS));
    }
    pub fn due(&self, now: u64) -> bool {
        self.retry_at.is_some_and(|at| now >= at)
    }
}

// server.ts `searchAcpRegistry` and runtime.ts: five-minute SWR and idle retention.
pub const SEARCH_TTL_MS: u64 = 5 * 60_000;
const RECENT_QUERIES: usize = 16;
const RECENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchScope {
    pub environment: String,
    pub connection: u64,
    pub session: u64,
    pub permitted: bool,
}
#[derive(Default)]
pub struct RecentSearches {
    scope: Option<SearchScope>,
    entries: VecDeque<RecentSearch>,
}
struct RecentSearch {
    query: String,
    agents: Option<Vec<Agent>>,
    error: Option<String>,
    answered: Option<u64>,
    used: u64,
    active: bool,
    bytes: usize,
}
impl RecentSearches {
    pub fn scope(&mut self, scope: SearchScope) -> bool {
        if self.scope.as_ref() == Some(&scope) {
            return false;
        }
        let changed = self.scope.is_some();
        self.entries.clear();
        self.scope = Some(scope);
        changed
    }
    fn expire(&mut self, now: u64) {
        self.entries
            .retain(|e| e.active || now.saturating_sub(e.used) < SEARCH_TTL_MS);
    }
    // Atom's idle TTL starts on unsubscribe; answer freshness remains independent.
    pub fn release(&mut self, search: &Search, now: u64) {
        self.expire(now);
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.query == search.submitted)
            && entry.active
        {
            entry.active = false;
            entry.used = now;
        }
    }
    pub fn restore(&mut self, search: &mut Search, now: u64) -> bool {
        self.expire(now);
        let query = search.query.trim();
        if query != search.submitted {
            search.agents = None;
        }
        search.submitted = query.to_owned();
        search.error = None;
        search.retire();
        if self.scope.as_ref().is_none_or(|s| !s.permitted) {
            search.agents = None;
            return false;
        }
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.query == search.submitted)
        else {
            search.agents = None;
            return false;
        };
        entry.active = true;
        entry.used = now;
        search.agents = entry.agents.clone();
        search.error = entry.error.clone();
        entry
            .answered
            .is_some_and(|at| now.saturating_sub(at) < SEARCH_TTL_MS)
    }
    pub fn remember(&mut self, search: &Search, now: u64) {
        if search.pending || self.scope.as_ref().is_none_or(|s| !s.permitted) {
            return;
        }
        let answered = if search.error.is_none() {
            Some(now)
        } else {
            self.entries
                .iter()
                .find(|e| e.query == search.submitted)
                .and_then(|e| e.answered)
        };
        self.entries.retain(|e| e.query != search.submitted);
        let bytes = search.submitted.len()
            + search.error.as_ref().map_or(0, String::len)
            + search
                .agents
                .iter()
                .flatten()
                .map(|a| {
                    std::mem::size_of::<Agent>()
                        + a.id.len()
                        + a.name.len()
                        + a.description.len()
                        + a.version.len()
                        + a.distribution.len()
                        + [&a.icon, &a.website, &a.repository]
                            .into_iter()
                            .flatten()
                            .map(String::len)
                            .sum::<usize>()
                })
                .sum::<usize>();
        if bytes > RECENT_BYTES {
            return;
        }
        self.entries.push_back(RecentSearch {
            query: search.submitted.clone(),
            agents: search.agents.clone(),
            error: search.error.clone(),
            answered,
            used: now,
            active: true,
            bytes,
        });
        while self.entries.len() > RECENT_QUERIES
            || self.entries.iter().map(|e| e.bytes).sum::<usize>() > RECENT_BYTES
        {
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.used)
                .map(|(i, _)| i)
                .unwrap();
            self.entries.remove(oldest);
        }
    }
}

/// Epochs belong to a single dialog/environment. Hiding retires in-flight work but keeps results.
#[derive(Default)]
pub struct Search {
    pub query: String,
    pub submitted: String,
    pub agents: Option<Vec<Agent>>,
    pub pending: bool,
    pub error: Option<String>,
    pub preparing: Option<String>,
    pub prepare_error: Option<String>,
    epoch: u64,
}
impl Search {
    pub fn retire(&mut self) {
        self.epoch += 1;
        self.pending = false;
        self.preparing = None;
    }
    pub fn typed(&mut self, text: String) {
        self.query = text;
        self.prepare_error = None;
    }
    pub fn submit(&mut self) -> bool {
        self.query = self.query.trim().to_owned();
        self.prepare_error = None;
        self.query == self.submitted
    }
    pub fn begin_search(&mut self) -> u64 {
        self.retire();
        self.submitted = self.query.trim().into();
        self.pending = true;
        self.error = None;
        self.prepare_error = None;
        self.epoch
    }
    pub fn searched(&mut self, epoch: u64, result: Result<SearchResult, String>) -> bool {
        if epoch != self.epoch {
            return false;
        }
        self.pending = false;
        match result {
            Ok(result) => {
                self.agents = Some(
                    result
                        .agents
                        .into_iter()
                        .take(MAX_AGENTS)
                        .filter(|a| !FIRST_CLASS_IDS.contains(&a.id.as_str()))
                        .collect(),
                );
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
    pub fn begin_prepare(&mut self, id: &str) -> Option<u64> {
        if self.preparing.is_some() {
            return None;
        }
        self.retire();
        self.preparing = Some(id.into());
        self.prepare_error = None;
        Some(self.epoch)
    }
    pub fn prepared(
        &mut self,
        epoch: u64,
        agent: Agent,
        result: Result<Prepared, String>,
    ) -> Option<Agent> {
        if epoch != self.epoch || self.preparing.as_deref() != Some(&agent.id) {
            return None;
        }
        self.preparing = None;
        match result.and_then(|p| agent.prepared(p)) {
            Ok(agent) => Some(agent),
            Err(error) => {
                self.prepare_error = Some(error);
                None
            }
        }
    }
}

/// ProviderWizardAuthenticationStep.tsx: discovery is independent of the live setup flags.
pub fn signed_in(provider: Option<&ServerProvider>, auth: Option<&AuthState>) -> bool {
    provider.is_some_and(|p| {
        p.auth.status == "authenticated"
            || (p.auth.status == "unknown" && auth.is_some_and(|a| a.phase == "succeeded"))
    })
}
pub fn discovering(signed_in: bool, auth: Option<&AuthState>, error: Option<&str>) -> bool {
    !signed_in && error.is_none() && auth.is_none_or(|a| a.methods.is_none())
}

#[cfg(test)]
mod tests;
