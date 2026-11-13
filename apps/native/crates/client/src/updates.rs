//! Ports `useUpdateEverything.ts` owner routing and `client-runtime/src/state/server.ts`
//! prepare/commit/lifecycle handoff; the server remains authoritative for authorization.
use crate::environments::{Destination, DestinationError, EnvironmentView, FleetView};
use crate::session::RpcError;
use crate::supervisor::ConnectionStatus;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use t3_core::server_config::{ServerConfig, ServerProvider};
use t3_core::sidebar::grouping::EnvironmentOwner;
use t3_core::updates::{self, Plan, ProviderTarget, ServerTarget};
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq)]
struct AdvisoryFacts {
    status: String,
    latest_version: Option<String>,
    can_update: bool,
    update_command: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
struct ProviderFacts {
    driver: String,
    instance_id: String,
    enabled: bool,
    compatibility_latest_version_status: Option<String>,
    advisory: Option<AdvisoryFacts>,
    update_active: bool,
}
#[derive(Debug, Clone, PartialEq)]
struct ServerFacts {
    from_version: String,
    self_update: Option<String>,
    desktop_app_update: Option<bool>,
    update_policy: Option<Value>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedUpdateOwner {
    pub owner: EnvironmentOwner,
    label: String,
    providers: Vec<ProviderFacts>,
    representatives: Vec<(String, Option<String>)>,
    server: Option<ServerFacts>,
}
pub struct UpdateDestination {
    pub capture: CapturedUpdateOwner,
    pub scope: &'static str,
    pub receipt: Result<Destination, DestinationError>,
}

fn provider_facts(providers: &[ServerProvider], driver: &str) -> Vec<ProviderFacts> {
    let mut facts: Vec<_> = providers.iter().filter(|p| p.driver == driver).map(|p| ProviderFacts {
        driver: p.driver.clone(), instance_id: p.instance_id.clone(), enabled: p.enabled,
        compatibility_latest_version_status: p.compatibility_advisory.as_ref().and_then(|a| a.latest_version_status.clone()),
        advisory: p.version_advisory.as_ref().map(|a| AdvisoryFacts { status: a.status.clone(), latest_version: a.latest_version.clone(), can_update: a.can_update, update_command: a.update_command.clone() }),
        update_active: updates::active(p),
    }).collect();
    facts.sort_by(|a,b| (&a.driver, &a.instance_id).cmp(&(&b.driver, &b.instance_id)));
    facts
}
fn representative(providers: &[ServerProvider], driver: &str) -> Option<String> {
    updates::one_click_candidates(providers).into_iter().find(|p| p.driver == driver).map(|p| p.instance_id.clone())
}
fn server_facts(config: &ServerConfig) -> ServerFacts {
    ServerFacts {
        from_version: config.environment.server_version.clone(),
        self_update: config.environment.capabilities.server_self_update.clone(),
        desktop_app_update: config.environment.capabilities.desktop_app_update,
        update_policy: config.managed_policy.as_ref().and_then(|p| p.get("limits")).and_then(|p| p.get("updates")).cloned(),
    }
}
fn ready(entry: &EnvironmentView) -> bool {
    entry.status == ConnectionStatus::Connected && entry.summary.synchronized
        && entry.summary.config_loaded && entry.config.config.is_some()
}

pub fn capture(plan: &Plan, view: &FleetView) -> Result<Vec<CapturedUpdateOwner>, String> {
    let mut ids: Vec<_> = plan.providers.iter().map(|p| p.environment_id.as_str())
        .chain(plan.servers.iter().map(|s| s.environment_id.as_str())).collect();
    ids.sort_unstable(); ids.dedup();
    ids.into_iter().map(|id| {
        let entry = view.environment(id).filter(|_| view.environments.iter().filter(|e| e.summary.owner.saved_id == id).count() == 1)
            .filter(|e| ready(e)).ok_or_else(|| format!("{id}: environment is not ready"))?;
        let config = entry.config.config.as_ref().unwrap();
        let mut drivers: Vec<_> = plan.providers.iter().filter(|p| p.environment_id == id).map(|p| p.driver.as_str()).collect();
        drivers.sort_unstable(); drivers.dedup();
        for target in plan.providers.iter().filter(|p| p.environment_id == id) {
            if config.providers.iter().filter(|p| p.driver == target.driver && p.instance_id == target.instance_id).count() != 1 {
                return Err(format!("{id}: provider changed"));
            }
        }
        Ok(CapturedUpdateOwner {
            owner: entry.summary.owner.clone(), label: config.environment.label.clone(),
            providers: drivers.iter().flat_map(|driver| provider_facts(&config.providers, driver)).collect(),
            representatives: drivers.iter().map(|driver| ((*driver).to_owned(), representative(&config.providers, driver))).collect(),
            server: plan.servers.iter().any(|s| s.environment_id == id).then(|| server_facts(config)),
        })
    }).collect()
}

fn refused(message: impl Into<String>) -> RpcError {
    RpcError::Failure(t3_core::rpc::RpcFailure { tag: Some("UpdateDestinationUnavailable".into()), message: message.into(), error: Value::Null })
}
fn authorized<'a>(capture: &CapturedUpdateOwner, destination: &Destination, view: &'a FleetView, scope: &str) -> Result<&'a EnvironmentView, RpcError> {
    let entry = view.environment(&capture.owner.saved_id).ok_or_else(|| refused("environment removed"))?;
    if destination.owner != capture.owner || !destination.is_current(view) || !ready(entry) {
        return Err(refused("environment owner or readiness changed"));
    }
    if !entry.grants.as_ref().is_some_and(|s| s.grants(scope))
        || !destination.client.session_state().borrow().as_ref().is_some_and(|s| s.grants(scope)) {
        return Err(refused("no permission"));
    }
    Ok(entry)
}
fn provider_current(capture: &CapturedUpdateOwner, destination: &Destination, view: &FleetView, target: &ProviderTarget, opening: bool) -> Result<(), RpcError> {
    let entry = authorized(capture, destination, view, "providers:manage")?;
    let providers = &entry.config.config.as_ref().unwrap().providers;
    let original: Vec<_> = capture.providers.iter().filter(|p| p.driver == target.driver).cloned().collect();
    let expected = original.iter().find(|p| p.instance_id == target.instance_id).ok_or_else(|| refused("provider was not captured"))?;
    let mut matching = providers.iter().filter(|p| p.driver == target.driver && p.instance_id == target.instance_id);
    let current = matching.next().ok_or_else(|| refused("provider removed"))?;
    if matching.next().is_some() || current.enabled != expected.enabled { return Err(refused("provider changed")); }
    if opening && (provider_facts(providers, &target.driver) != original
        || capture.representatives.iter().find(|(driver,_)| driver == &target.driver).map(|(_,id)| id)
            != Some(&representative(providers, &target.driver))) {
        return Err(refused("provider update facts changed"));
    }
    Ok(())
}
async fn provider_attempt(packet: &UpdateDestination, target: &ProviderTarget, view: &watch::Receiver<Arc<FleetView>>) -> (Result<Value, RpcError>, bool) {
    let destination = match &packet.receipt { Ok(d) => d, Err(e) => return (Err(refused(format!("Destination unavailable: {e:?}"))), false) };
    if let Err(error) = view.has_changed().map_err(|_| refused("environment view closed")).and_then(|_| provider_current(&packet.capture, destination, &view.borrow(), target, true)) {
        return (Err(error), false);
    }
    let response = destination.client.request("server.updateProvider", json!({"provider":target.driver,"instanceId":target.instance_id}));
    tokio::pin!(response);
    let mut current = view.clone();
    let mut grants = destination.client.session_state();
    let mut generation = destination.client.connection_generation();
    let result = loop {
        if let Err(e) = current.has_changed().map_err(|_| refused("environment view closed")).and_then(|_| provider_current(&packet.capture, destination, &current.borrow(), target, false)) { break Err(e); }
        tokio::select! {
            result = &mut response => break result.and_then(|value| {
                current.has_changed().map_err(|_| refused("environment view closed"))?;
                provider_current(&packet.capture, destination, &current.borrow(), target, false)?;
                Ok(value)
            }),
            changed = current.changed() => if changed.is_err() { break Err(refused("environment view closed")); },
            changed = grants.changed() => if changed.is_err() { break Err(refused("session grants closed")); },
            changed = generation.changed() => if changed.is_err() { break Err(refused("connection closed")); },
        }
    };
    (result, true)
}
fn packet<'a>(packets: &'a [UpdateDestination], id: &str, scope: &str) -> Result<&'a UpdateDestination, String> {
    let mut matches = packets.iter().filter(|p| p.capture.owner.saved_id == id && p.scope == scope);
    let packet = matches.next().ok_or_else(|| format!("{id}: destination was not captured"))?;
    if matches.next().is_some() { return Err(format!("{id}: ambiguous update destination")); }
    Ok(packet)
}
fn provider_terminal(response: &Value, instance: &str) -> bool {
    response["providers"].as_array().and_then(|ps| ps.iter().find(|p| p["instanceId"] == instance))
        .and_then(|p| p["updateState"]["status"].as_str()).is_some_and(|s| matches!(s, "succeeded" | "failed" | "unchanged"))
}

pub async fn run(plan: Plan, destinations: Vec<UpdateDestination>, view: watch::Receiver<Arc<FleetView>>) -> Vec<String> {
    let mut failures = Vec::new();
    let mut unresolved_provider = false;
    for target in plan.providers {
        let packet = match packet(&destinations, &target.environment_id, "providers:manage") { Ok(p) => p, Err(e) => { failures.push(e); continue; } };
        let label = format!("{} / {}", packet.capture.label, t3_core::providers::driver_label(&target.driver));
        let (result, enqueued) = provider_attempt(packet, &target, &view).await;
        match result {
            Ok(value) => {
                if !provider_terminal(&value, &target.instance_id) { unresolved_provider = true; failures.push(format!("{label}: remote update is unresolved")); }
                if let Some(error) = provider_failure(&value, &target.instance_id) { failures.push(format!("{label}: {error}")); }
            }
            Err(error) => {
                unresolved_provider |= enqueued;
                failures.push(format!("{label}: {error}{}", if enqueued { " (remote update is unresolved)" } else { "" }));
            }
        }
    }
    for target in plan.servers {
        if unresolved_provider { failures.push(format!("{}: server restart withheld because a provider update is unresolved", target.label)); continue; }
        let result = match packet(&destinations, &target.environment_id, "environment:maintain") {
            Ok(packet) => update_server(packet, &target, &view).await,
            Err(error) => Err(error),
        };
        if let Err(error) = result { failures.push(format!("{}: {error}", target.label)); }
    }
    failures
}

trait UpdateTransport {
    fn generation(&self) -> u64;
    fn request(
        &self,
        method: &'static str,
        input: Value,
    ) -> impl std::future::Future<Output = Result<Value, crate::session::RpcError>> + Send;
    fn reconnected(
        &self,
        after: u64,
    ) -> impl std::future::Future<Output = Result<(), crate::session::RpcError>> + Send;
    fn next_ready(
        &self,
        result: &Value,
    ) -> impl std::future::Future<Output = Result<Value, crate::session::RpcError>> + Send;
}

fn handoff_loss(error: &crate::session::RpcError) -> bool {
    match error {
        crate::session::RpcError::Closed => true,
        crate::session::RpcError::Failure(error) => {
            error.tag.is_none() && error.message == "Interrupted" && error.error.is_null()
        }
    }
}

async fn update_with(
    transport: &impl UpdateTransport,
    target: &str,
    capability: &str,
) -> Result<(), String> {
    let generation = transport.generation();
    let result = match transport
        .request("server.updateServer", json!({"targetVersion":target}))
        .await
    {
        Ok(result) => result,
        Err(error) if matches!(capability, "boot-service" | "respawn") && handoff_loss(&error) => {
            transport
                .reconnected(generation)
                .await
                .map_err(|e| e.to_string())?;
            json!({"targetVersion":target,"method":capability})
        }
        Err(error) => return Err(error.to_string()),
    };
    if result["targetVersion"].as_str().is_none_or(|v| v.trim().is_empty()) {
        return Err("The update host returned no target version.".into());
    }
    if result["method"] == "desktop-app" {
        let token = result["desktopUpdateToken"].as_str().filter(|token| !token.trim().is_empty())
            .ok_or("The update host returned no prepared token.")?;
        let generation = transport.generation();
        match transport
            .request("server.commitDesktopUpdate", json!({"requestId":token}))
            .await
        {
            Ok(_) => {}
            Err(error) if handoff_loss(&error) => transport
                .reconnected(generation)
                .await
                .map_err(|e| e.to_string())?,
            Err(error) => return Err(error.to_string()),
        }
        // Match waitForDesktopUpdateTarget: an old-version reconnect retries the same token,
        // with at most three readiness checks, and never consumes the pre-commit connection.
        for attempt in 1..=3 {
            let ready = transport
                .next_ready(&result)
                .await
                .map_err(|e| e.to_string())?;
            if ready["payload"]["environment"]["serverVersion"] == result["targetVersion"] {
                return t3_core::updates::ready_outcome(&result, &ready).unwrap_or_else(|| {
                    Err("The desktop app resumed without installing the prepared update.".into())
                });
            }
            if attempt == 3 {
                break;
            }
            let generation = transport.generation();
            match transport
                .request("server.commitDesktopUpdate", json!({"requestId":token}))
                .await
            {
                Ok(_) => break,
                Err(error) if handoff_loss(&error) => transport
                    .reconnected(generation)
                    .await
                    .map_err(|e| e.to_string())?,
                Err(error) => return Err(error.to_string()),
            }
        }
        return Err("The desktop app resumed without installing the prepared update.".into());
    }
    loop {
        let ready = transport
            .next_ready(&result)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(outcome) = t3_core::updates::ready_outcome(&result, &ready) {
            return outcome;
        }
    }
}

struct GuardedUpdate<'a> {
    capture: &'a CapturedUpdateOwner,
    destination: &'a Destination,
    target: &'a ServerTarget,
    view: watch::Receiver<Arc<FleetView>>,
    continuation: AtomicBool,
    generations: Mutex<(u64, u64)>,
}
impl GuardedUpdate<'_> {
    fn check(&self) -> Result<bool, RpcError> {
        self.view.has_changed().map_err(|_| refused("environment view closed"))?;
        let view = self.view.borrow();
        let entry = view.environment(&self.capture.owner.saved_id).ok_or_else(|| refused("environment removed"))?;
        let current = &entry.summary.owner;
        let original = &self.capture.owner;
        if self.destination.owner != *original || current.saved_id != original.saved_id
            || current.canonical_id != original.canonical_id || current.generation != original.generation {
            return Err(refused("server owner changed"));
        }
        let continuation = self.continuation.load(Ordering::Relaxed);
        if !continuation && current != original { return Err(refused("server connection changed")); }
        let previous = *self.generations.lock().unwrap();
        if current.connection_generation < previous.0 || current.shell_generation < previous.1 {
            return Err(refused("server generation regressed"));
        }
        let live = self.destination.client.session_state();
        if entry.grants.as_ref().is_some_and(|s| !s.grants("environment:maintain"))
            || live.borrow().as_ref().is_some_and(|s| !s.grants("environment:maintain")) {
            return Err(refused("no permission to maintain server"));
        }
        if let Some(config) = &entry.config.config {
            let captured = self.capture.server.as_ref().ok_or_else(|| refused("server facts missing"))?;
            let facts = server_facts(config);
            if facts.self_update != captured.self_update || facts.desktop_app_update != captured.desktop_app_update
                || facts.update_policy != captured.update_policy
                || updates::managed_denial(config.managed_policy.as_ref(), if self.target.self_update == "desktop-managed" { None } else { Some(&self.target.target_version) }).is_some() {
                return Err(refused("server capability or policy changed"));
            }
            if !continuation && facts != *captured { return Err(refused("server update facts changed")); }
        }
        let aligned = *self.destination.client.connection_generation().borrow() == current.connection_generation;
        let ready = ready(entry) && aligned && entry.grants.as_ref().is_some_and(|s| s.grants("environment:maintain"))
            && live.borrow().as_ref().is_some_and(|s| s.grants("environment:maintain"));
        if ready { *self.generations.lock().unwrap() = (current.connection_generation, current.shell_generation); }
        if !ready && !continuation { return Err(refused("server not ready")); }
        Ok(ready)
    }
    async fn until_ready(&self, after: Option<u64>) -> Result<(), RpcError> {
        let mut view = self.view.clone();
        let mut grants = self.destination.client.session_state();
        let mut generation = self.destination.client.connection_generation();
        loop {
            if self.check()? && after.is_none_or(|after| self.generation() > after) { return Ok(()); }
            tokio::select! {
                changed = view.changed() => changed.map_err(|_| refused("environment view closed"))?,
                changed = grants.changed() => changed.map_err(|_| refused("session grants closed"))?,
                changed = generation.changed() => changed.map_err(|_| refused("connection closed"))?,
            }
        }
    }
    async fn observe<T>(&self, future: impl std::future::Future<Output = Result<T, RpcError>> + Send) -> Result<T, RpcError> {
        tokio::pin!(future);
        let mut view = self.view.clone();
        let mut grants = self.destination.client.session_state();
        let mut generation = self.destination.client.connection_generation();
        loop {
            self.check()?;
            tokio::select! {
                result = &mut future => {
                    self.check()?;
                    return result;
                },
                changed = view.changed() => changed.map_err(|_| refused("environment view closed"))?,
                changed = grants.changed() => changed.map_err(|_| refused("session grants closed"))?,
                changed = generation.changed() => changed.map_err(|_| refused("connection closed"))?,
            }
        }
    }
}
impl UpdateTransport for GuardedUpdate<'_> {
    fn generation(&self) -> u64 { *self.destination.client.connection_generation().borrow() }
    async fn request(&self, method: &'static str, input: Value) -> Result<Value, RpcError> {
        self.until_ready(None).await?;
        let response = self.destination.client.request(method, input);
        if method == "server.commitDesktopUpdate" || (method == "server.updateServer" && matches!(self.target.self_update.as_str(), "boot-service" | "respawn")) {
            self.continuation.store(true, Ordering::Relaxed);
        }
        let value = self.observe(response).await?;
        self.until_ready(None).await?;
        Ok(value)
    }
    async fn reconnected(&self, after: u64) -> Result<(), RpcError> { self.until_ready(Some(after)).await }
    async fn next_ready(&self, result: &Value) -> Result<Value, RpcError> {
        loop {
            self.until_ready(None).await?;
            let generation = self.generation();
            match self.observe(self.destination.client.stream("subscribeServerLifecycle", json!({}))).await {
                Ok(mut stream) => {
                    loop {
                        match self.observe(async { stream.receiver.recv().await.unwrap_or(Err(RpcError::Closed)) }).await {
                            Ok(event) if event["type"] == "ready"
                                && ((result["method"] == "desktop-app" && result["desktopUpdateToken"].is_string())
                                    || updates::ready_outcome(result, &event).is_some()) => {
                                self.until_ready(None).await?;
                                return Ok(event);
                            }
                            Ok(_) => {},
                            Err(error) if handoff_loss(&error) => break,
                            Err(error) => return Err(error),
                        }
                    }
                }
                Err(error) if handoff_loss(&error) => {},
                Err(error) => return Err(error),
            }
            self.reconnected(generation).await?;
        }
    }
}
async fn update_server(packet: &UpdateDestination, target: &ServerTarget, view: &watch::Receiver<Arc<FleetView>>) -> Result<(), String> {
    let destination = packet.receipt.as_ref().map_err(|e| format!("Destination unavailable: {e:?}"))?;
    authorized(&packet.capture, destination, &view.borrow(), "environment:maintain").map_err(|e| e.to_string())?;
    let eligible = {
        let view = view.borrow();
        let entry = view.environment(&target.environment_id).ok_or("server removed")?;
        let machines = [updates::Machine { id: &target.environment_id, label: &target.label,
            config: entry.config.config.as_ref(), connected: ready(entry), hosted_by_this_app: false,
            can_maintain: true, can_manage_providers: false, updating: false }];
        updates::collect(&machines, &target.target_version).servers.iter().any(|s| s == target)
    };
    if !eligible { return Err("server target is no longer eligible".into()); }
    let guard = GuardedUpdate {
        capture: &packet.capture, destination, target, view: view.clone(), continuation: AtomicBool::new(false),
        generations: Mutex::new((packet.capture.owner.connection_generation, packet.capture.owner.shell_generation)),
    };
    guard.check().map_err(|e| e.to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(32 * 60), async {
        update_with(&guard, &target.target_version, &target.self_update).await?;
        guard.until_ready(None).await.map_err(|e| e.to_string())
    }).await.map_err(|_| "Timed out waiting for the server to resume after updating.".to_owned())?
}

pub fn provider_failure(response: &Value, instance_id: &str) -> Option<String> {
    let state = response["providers"]
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["instanceId"] == instance_id))
        .map(|p| &p["updateState"]);
    if state.is_some_and(|s| s["status"] == "succeeded") {
        None
    } else {
        Some(
            state
                .and_then(|s| s["message"].as_str())
                .unwrap_or("Provider update did not finish.")
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_provider_rpc_success_can_contain_a_failed_or_unchanged_update() {
        for status in ["failed", "unchanged", "running", "succeeded"] {
            let response = json!({"providers":[{"instanceId":"codex","updateState":{"status":status,"message":"detail"}}]});
            assert_eq!(
                provider_failure(&response, "codex").is_none(),
                status == "succeeded"
            );
        }
        assert!(provider_failure(&json!({}), "codex").is_some());
        for status in ["queued", "running", "succeeded", "failed", "unchanged", "unknown"] {
            assert_eq!(provider_terminal(&json!({"providers":[{"instanceId":"codex","updateState":{"status":status}}]}), "codex"), matches!(status,"succeeded" | "failed" | "unchanged"));
        }
        assert!(!provider_terminal(&json!({}), "codex"));
    }
}

#[cfg(test)]
mod handoff_tests;
