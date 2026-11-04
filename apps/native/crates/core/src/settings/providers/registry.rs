//! `AcpRegistrySearchStep.tsx`, `AddProviderInstanceDialog.tsx` and contracts/acpRegistry.ts.
use super::{ACP_REGISTRY, auth::AuthState};
use crate::server_config::{ProviderInstanceConfig, ServerProvider};
use serde::Deserialize;
use serde_json::{Value, json};

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
