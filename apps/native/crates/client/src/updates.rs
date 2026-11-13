//! Ports `useUpdateEverything.ts` owner routing and `client-runtime/src/state/server.ts`
//! prepare/commit/lifecycle handoff; the server remains authoritative for authorization.
use crate::environments::{Destination, DestinationError, EnvironmentView, FleetView};
use crate::session::RpcError;
use crate::supervisor::ConnectionStatus;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use t3_core::server_config::{ServerConfig, ServerProvider};
use t3_core::sidebar::grouping::EnvironmentOwner;
use t3_core::updates::{
    self, ChannelSwitchPlan, Plan, ProviderOutcome, ProviderTarget, ServerOutcome, ServerTarget,
    ServerUpdateStage, UpdateEverythingResult, UpdateTrack,
};
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateEvent {
    ProvidersFinished {
        updated_providers: usize,
        failures: Vec<String>,
    },
    ServerStage {
        environment_id: String,
        stage: ServerUpdateStage,
    },
    ServerHandedOff {
        environment_id: String,
        target_version: String,
    },
    MachineSettled {
        environment_id: String,
        failure: Option<String>,
        server_updated: bool,
        provider_failure: Option<String>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct ServerRequestOptions {
    pub channel: Option<UpdateTrack>,
    pub release_channel: Option<UpdateTrack>,
    pub target_database_migration: Option<u64>,
    pub track_only: bool,
}

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
    capture_targets(&plan.providers, &plan.servers, view)
}

pub fn capture_channel_switch(
    switch: &ChannelSwitchPlan,
    view: &FleetView,
) -> Result<Vec<CapturedUpdateOwner>, String> {
    let servers: Vec<_> = switch.targets.iter().map(|t| t.server.clone()).collect();
    capture_targets(&switch.plan.providers, &servers, view)
}

fn capture_targets(
    providers: &[ProviderTarget],
    servers: &[ServerTarget],
    view: &FleetView,
) -> Result<Vec<CapturedUpdateOwner>, String> {
    let mut ids: Vec<_> = providers.iter().map(|p| p.environment_id.as_str())
        .chain(servers.iter().map(|s| s.environment_id.as_str())).collect();
    ids.sort_unstable(); ids.dedup();
    ids.into_iter().map(|id| {
        let entry = view.environment(id).filter(|_| view.environments.iter().filter(|e| e.summary.owner.saved_id == id).count() == 1)
            .filter(|e| ready(e)).ok_or_else(|| format!("{id}: environment is not ready"))?;
        let config = entry.config.config.as_ref().unwrap();
        let mut drivers: Vec<_> = providers.iter().filter(|p| p.environment_id == id).map(|p| p.driver.as_str()).collect();
        drivers.sort_unstable(); drivers.dedup();
        for target in providers.iter().filter(|p| p.environment_id == id) {
            if config.providers.iter().filter(|p| p.driver == target.driver && p.instance_id == target.instance_id).count() != 1 {
                return Err(format!("{id}: provider changed"));
            }
        }
        Ok(CapturedUpdateOwner {
            owner: entry.summary.owner.clone(), label: config.environment.label.clone(),
            providers: drivers.iter().flat_map(|driver| provider_facts(&config.providers, driver)).collect(),
            representatives: drivers.iter().map(|driver| ((*driver).to_owned(), representative(&config.providers, driver))).collect(),
            server: servers.iter().any(|s| s.environment_id == id).then(|| server_facts(config)),
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

pub async fn run(
    plan: Plan,
    destinations: Vec<UpdateDestination>,
    view: watch::Receiver<Arc<FleetView>>,
) -> Vec<String> {
    run_with_progress(plan, destinations, view, None).await.failures
}

pub async fn run_with_progress(
    plan: Plan,
    destinations: Vec<UpdateDestination>,
    view: watch::Receiver<Arc<FleetView>>,
    events: Option<mpsc::UnboundedSender<UpdateEvent>>,
) -> UpdateEverythingResult {
    let mut provider_outcomes = Vec::new();
    let mut provider_failures_by_env: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut unresolved_provider = false;
    let mut machine_ids = Vec::new();
    for p in &plan.providers {
        if !machine_ids.contains(&p.environment_id) {
            machine_ids.push(p.environment_id.clone());
        }
    }
    for target in plan.providers {
        let packet = match packet(&destinations, &target.environment_id, "providers:manage") {
            Ok(p) => p,
            Err(e) => {
                provider_outcomes.push(ProviderOutcome::Failed(e.clone()));
                provider_failures_by_env.entry(target.environment_id).or_insert(e);
                continue;
            }
        };
        let label = format!(
            "{} / {}",
            packet.capture.label,
            t3_core::providers::driver_label(&target.driver)
        );
        let (result, enqueued) = provider_attempt(packet, &target, &view).await;
        match result {
            Ok(value) => {
                if !provider_terminal(&value, &target.instance_id) {
                    unresolved_provider = true;
                    let line = format!("{label}: remote update is unresolved");
                    provider_outcomes.push(ProviderOutcome::Failed(line.clone()));
                    provider_failures_by_env
                        .entry(target.environment_id)
                        .or_insert(line);
                } else if let Some(error) = provider_failure(&value, &target.instance_id) {
                    let line = format!("{label}: {error}");
                    provider_outcomes.push(ProviderOutcome::Failed(line.clone()));
                    provider_failures_by_env
                        .entry(target.environment_id)
                        .or_insert(line);
                } else {
                    provider_outcomes.push(ProviderOutcome::Updated);
                }
            }
            Err(error) => {
                unresolved_provider |= enqueued;
                let line = format!(
                    "{label}: {error}{}",
                    if enqueued {
                        " (remote update is unresolved)"
                    } else {
                        ""
                    }
                );
                provider_outcomes.push(ProviderOutcome::Failed(line.clone()));
                provider_failures_by_env
                    .entry(target.environment_id)
                    .or_insert(line);
            }
        }
    }
    if let Some(tx) = &events {
        for id in &machine_ids {
            if !plan.servers.iter().any(|s| &s.environment_id == id) {
                let failure = provider_failures_by_env.get(id).cloned();
                let _ = tx.send(UpdateEvent::MachineSettled {
                    environment_id: id.clone(),
                    failure: failure.clone(),
                    server_updated: false,
                    provider_failure: failure,
                });
            }
        }
        let updated_providers = provider_outcomes
            .iter()
            .filter(|o| matches!(o, ProviderOutcome::Updated))
            .count();
        let provider_failures = provider_outcomes
            .iter()
            .filter_map(|o| match o {
                ProviderOutcome::Failed(line) => Some(line.clone()),
                _ => None,
            })
            .collect();
        let _ = tx.send(UpdateEvent::ProvidersFinished {
            updated_providers,
            failures: provider_failures,
        });
    }
    let targets = plan
        .servers
        .into_iter()
        .map(|target| (target, ServerRequestOptions::default()))
        .collect();
    execute_server_targets(
        targets,
        unresolved_provider,
        &provider_outcomes,
        &provider_failures_by_env,
        &destinations,
        &view,
        events.as_ref(),
    )
    .await
}

pub async fn run_channel_switch_with_progress(
    switch: ChannelSwitchPlan,
    release_channel: UpdateTrack,
    destinations: Vec<UpdateDestination>,
    view: watch::Receiver<Arc<FleetView>>,
    events: Option<mpsc::UnboundedSender<UpdateEvent>>,
) -> UpdateEverythingResult {
    if let Some(tx) = &events {
        let _ = tx.send(UpdateEvent::ProvidersFinished {
            updated_providers: 0,
            failures: Vec::new(),
        });
    }
    let targets = switch
        .targets
        .into_iter()
        .map(|target| {
            let options = ServerRequestOptions {
                channel: target.channel,
                release_channel: Some(release_channel),
                target_database_migration: target.target_database_migration,
                track_only: target.track_only,
            };
            (target.server, options)
        })
        .collect();
    let provider_failures_by_env = std::collections::HashMap::new();
    execute_server_targets(
        targets,
        false,
        &[],
        &provider_failures_by_env,
        &destinations,
        &view,
        events.as_ref(),
    )
    .await
}

async fn execute_server_targets(
    targets: Vec<(ServerTarget, ServerRequestOptions)>,
    unresolved_provider: bool,
    provider_outcomes: &[ProviderOutcome],
    provider_failures_by_env: &std::collections::HashMap<String, String>,
    destinations: &[UpdateDestination],
    view: &watch::Receiver<Arc<FleetView>>,
    events: Option<&mpsc::UnboundedSender<UpdateEvent>>,
) -> UpdateEverythingResult {
    let mut server_outcomes: Vec<(String, ServerOutcome)> = Vec::new();
    for (target, options) in targets {
        let provider_failure = provider_failures_by_env.get(&target.environment_id).cloned();
        let outcome = if unresolved_provider {
            ServerOutcome::Failed(
                "server restart withheld because a provider update is unresolved".into(),
            )
        } else {
            match packet(destinations, &target.environment_id, "environment:maintain") {
                Ok(packet) => match update_server_with_events(
                    packet,
                    &target,
                    view,
                    &options,
                    events,
                )
                .await
                {
                    Ok(()) => ServerOutcome::Updated,
                    Err(error) => ServerOutcome::Failed(error),
                },
                Err(error) => ServerOutcome::Failed(error),
            }
        };
        if let Some(tx) = events
            && !options.track_only
        {
            let (failure, server_updated) = match &outcome {
                ServerOutcome::Updated | ServerOutcome::Skipped => (provider_failure.clone(), true),
                ServerOutcome::Failed(message) => (Some(message.clone()), false),
            };
            let _ = tx.send(UpdateEvent::MachineSettled {
                environment_id: target.environment_id.clone(),
                failure,
                server_updated,
                provider_failure,
            });
        }
        server_outcomes.push((target.label.clone(), outcome));
    }
    let refs: Vec<(&str, &ServerOutcome)> = server_outcomes
        .iter()
        .map(|(label, outcome)| (label.as_str(), outcome))
        .collect();
    updates::collect_update_everything_result(&refs, provider_outcomes)
}

pub async fn retry_servers_with_progress(
    targets: Vec<(ServerTarget, Option<String>)>,
    destinations: Vec<UpdateDestination>,
    view: watch::Receiver<Arc<FleetView>>,
    events: Option<mpsc::UnboundedSender<UpdateEvent>>,
) -> Vec<String> {
    let mut failures = Vec::new();
    for (target, provider_failure) in targets {
        let outcome = match packet(&destinations, &target.environment_id, "environment:maintain") {
            Ok(packet) => update_server_with_events(
                packet,
                &target,
                &view,
                &ServerRequestOptions::default(),
                events.as_ref(),
            )
            .await,
            Err(error) => Err(error),
        };
        let (settled_failure, server_updated) = match &outcome {
            Ok(()) => (provider_failure.clone(), true),
            Err(error) => {
                failures.push(format!("{}: {error}", target.label));
                (Some(error.clone()), false)
            }
        };
        if let Some(tx) = &events {
            let _ = tx.send(UpdateEvent::MachineSettled {
                environment_id: target.environment_id,
                failure: settled_failure,
                server_updated,
                provider_failure,
            });
        }
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

#[cfg(test)]
async fn update_with(
    transport: &impl UpdateTransport,
    target: &str,
    capability: &str,
) -> Result<(), String> {
    update_with_options(
        transport,
        target,
        capability,
        &ServerRequestOptions::default(),
        |_, _| {},
    )
    .await
}

async fn update_with_options(
    transport: &impl UpdateTransport,
    target: &str,
    capability: &str,
    options: &ServerRequestOptions,
    mut on_stage: impl FnMut(ServerUpdateStage, Option<&str>),
) -> Result<(), String> {
    let mut input = json!({"targetVersion": target});
    if let Some(channel) = options.channel {
        input["channel"] = json!(channel.as_str());
    }
    if let Some(migration) = options.target_database_migration {
        input["targetDatabaseMigration"] = json!(migration);
    }
    on_stage(ServerUpdateStage::Downloading, None);
    let generation = transport.generation();
    let result = match transport
        .request("server.updateServer", input)
        .await
    {
        Ok(result) => result,
        Err(error) if matches!(capability, "boot-service" | "respawn") && handoff_loss(&error) => {
            on_stage(ServerUpdateStage::Resuming, Some(target));
            transport
                .reconnected(generation)
                .await
                .map_err(|e| e.to_string())?;
            json!({"targetVersion":target,"method":capability})
        }
        Err(error) => return Err(error.to_string()),
    };
    let response_version = result["targetVersion"]
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or("The update host returned no target version.")?
        .to_owned();
    if options.track_only && result["updateId"].is_null() {
        return Ok(());
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
        on_stage(ServerUpdateStage::Resuming, Some(&response_version));
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
    on_stage(ServerUpdateStage::Resuming, Some(&response_version));
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
async fn update_server_with_events(
    packet: &UpdateDestination,
    target: &ServerTarget,
    view: &watch::Receiver<Arc<FleetView>>,
    options: &ServerRequestOptions,
    events: Option<&mpsc::UnboundedSender<UpdateEvent>>,
) -> Result<(), String> {
    let destination = packet.receipt.as_ref().map_err(|e| format!("Destination unavailable: {e:?}"))?;
    authorized(&packet.capture, destination, &view.borrow(), "environment:maintain").map_err(|e| e.to_string())?;
    let eligible = {
        let view = view.borrow();
        let entry = view.environment(&target.environment_id).ok_or("server removed")?;
        let machines = [updates::Machine { id: &target.environment_id, label: &target.label,
            config: entry.config.config.as_ref(), connected: ready(entry), hosted_by_this_app: false,
            can_maintain: true, can_manage_providers: false, updating: false }];
        if let Some(channel) = options.release_channel.or(options.channel) {
            let release = updates::ChannelSwitchRelease {
                channel,
                version: target.target_version.clone(),
                database_migration: options.target_database_migration,
            };
            updates::collect_channel_switch(&machines, &release)
                .targets
                .iter()
                .any(|t| &t.server == target && t.track_only == options.track_only)
        } else {
            updates::collect(&machines, &target.target_version).servers.iter().any(|s| s == target)
        }
    };
    if !eligible { return Err("server target is no longer eligible".into()); }
    let guard = GuardedUpdate {
        capture: &packet.capture, destination, target, view: view.clone(), continuation: AtomicBool::new(false),
        generations: Mutex::new((packet.capture.owner.connection_generation, packet.capture.owner.shell_generation)),
    };
    guard.check().map_err(|e| e.to_string())?;
    let timeout_secs = (updates::HANDOFF_TIMEOUT_MS / 1000) as u64;
    tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
        update_with_options(&guard, &target.target_version, &target.self_update, options, |stage, response_version| {
            if let Some(tx) = events {
                let _ = tx.send(UpdateEvent::ServerStage {
                    environment_id: target.environment_id.clone(),
                    stage,
                });
                if stage == ServerUpdateStage::Resuming {
                    let _ = tx.send(UpdateEvent::ServerHandedOff {
                        environment_id: target.environment_id.clone(),
                        target_version: response_version
                            .unwrap_or(&target.target_version)
                            .to_owned(),
                    });
                }
            }
        })
        .await?;
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

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn unresolved_provider_reply_reports_single_unresolved_failure() {
        let session = serde_json::from_value(json!({"authenticated":true,"permissions":["providers:manage"]})).unwrap();
        let (client, mut fixture) = crate::supervisor::ClientFixture::handle(session);
        let grants_rx = client.session_state();
        let mut config = t3_core::server_config::ServerConfigState::default();
        config.apply(
            serde_json::from_value(json!({
                "type": "snapshot",
                "config": {
                    "environment": { "environmentId": "env-1", "label": "Work" },
                    "providers": [{
                        "instanceId": "codex",
                        "driver": "codex",
                        "enabled": true,
                        "installed": true,
                        "status": "ready",
                        "versionAdvisory": {
                            "status": "behind_latest",
                            "latestVersion": "2.0",
                            "canUpdate": true,
                            "updateCommand": "update"
                        }
                    }]
                }
            }))
            .unwrap(),
        );
        let owner = EnvironmentOwner {
            saved_id: "env-1".into(),
            canonical_id: Some("env-1".into()),
            generation: 1,
            connection_generation: 1,
            shell_generation: 0,
        };
        let mut shell = t3_core::shell::ShellState::default();
        shell.synchronized = true;
        let summary = Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
            owner.clone(),
            &shell,
            &config,
        ));
        let entry = Arc::new(EnvironmentView {
            summary,
            status: ConnectionStatus::Connected,
            config,
            grants: grants_rx.borrow().clone(),
            settings_revision: 1,
            keybindings_revision: 1,
        });
        let (_view_tx, view_rx) = watch::channel(Arc::new(FleetView {
            revision: 1,
            environments: vec![entry],
        }));
        let plan = Plan {
            providers: vec![ProviderTarget {
                environment_id: "env-1".into(),
                driver: "codex".into(),
                instance_id: "codex".into(),
            }],
            ..Default::default()
        };
        let captures = capture(&plan, &view_rx.borrow()).unwrap();
        let destinations = vec![UpdateDestination {
            capture: captures.into_iter().next().unwrap(),
            scope: "providers:manage",
            receipt: Ok(Destination { owner, client }),
        }];
        let respond = tokio::spawn(async move {
            let (method, _payload, reply) = fixture.next_request().await;
            assert_eq!(method, "server.updateProvider");
            let _ = reply.send(Ok(json!({
                "providers": [{ "instanceId": "codex", "updateState": { "status": "running" } }]
            })));
            fixture
        });
        let failures = run(plan, destinations, view_rx).await;
        let _fixture = respond.await.unwrap();
        assert_eq!(failures, vec!["Work / Codex: remote update is unresolved"]);
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn run_with_progress_emits_stages_and_retries_only_failed_servers() {
        let session: t3_core::auth::AuthSessionState = serde_json::from_value(json!({
            "authenticated": true,
            "permissions": ["providers:manage", "environment:maintain"]
        }))
        .unwrap();
        let (client_a, mut fixture_a) = crate::supervisor::ClientFixture::handle(session.clone());
        let (client_b, mut fixture_b) = crate::supervisor::ClientFixture::handle(session);
        let grants_a = client_a.session_state();
        let grants_b = client_b.session_state();
        let make_entry = |id: &str,
                          label: &str,
                          with_provider: bool,
                          client: &crate::supervisor::ClientHandle,
                          grants_rx: &watch::Receiver<Option<t3_core::auth::AuthSessionState>>| {
            let mut config = t3_core::server_config::ServerConfigState::default();
            let providers = if with_provider {
                json!([{
                    "instanceId": "codex",
                    "driver": "codex",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "versionAdvisory": {
                        "status": "behind_latest",
                        "latestVersion": "2.0",
                        "canUpdate": true,
                        "updateCommand": "update"
                    }
                }])
            } else {
                json!([])
            };
            config.apply(
                serde_json::from_value(json!({
                    "type": "snapshot",
                    "config": {
                        "environment": {
                            "environmentId": id,
                            "label": label,
                            "serverVersion": "0.1.0",
                            "capabilities": { "serverSelfUpdate": "npm" }
                        },
                        "providers": providers
                    }
                }))
                .unwrap(),
            );
            let owner = EnvironmentOwner {
                saved_id: id.into(),
                canonical_id: Some(id.into()),
                generation: 1,
                connection_generation: 1,
                shell_generation: 0,
            };
            let mut shell = t3_core::shell::ShellState::default();
            shell.synchronized = true;
            let summary = Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
                owner.clone(),
                &shell,
                &config,
            ));
            let entry = Arc::new(EnvironmentView {
                summary,
                status: ConnectionStatus::Connected,
                config,
                grants: grants_rx.borrow().clone(),
                settings_revision: 1,
                keybindings_revision: 1,
            });
            (owner, entry, client.clone())
        };
        let (owner_a, entry_a, handle_a) = make_entry("env-a", "Alpha", true, &client_a, &grants_a);
        let (owner_b, entry_b, handle_b) = make_entry("env-b", "Beta", false, &client_b, &grants_b);
        let (_view_tx, view_rx) = watch::channel(Arc::new(FleetView {
            revision: 1,
            environments: vec![entry_a, entry_b],
        }));
        let plan = Plan {
            providers: vec![ProviderTarget {
                environment_id: "env-a".into(),
                driver: "codex".into(),
                instance_id: "codex".into(),
            }],
            servers: vec![ServerTarget {
                environment_id: "env-b".into(),
                label: "Beta".into(),
                target_version: "0.2.0".into(),
                self_update: "npm".into(),
            }],
            ..Default::default()
        };
        let captures = capture(&plan, &view_rx.borrow()).unwrap();
        let destinations = captures
            .into_iter()
            .map(|capture| {
                let (owner, scope, client) = if capture.owner.saved_id == "env-a" {
                    (owner_a.clone(), "providers:manage", handle_a.clone())
                } else {
                    (owner_b.clone(), "environment:maintain", handle_b.clone())
                };
                UpdateDestination {
                    capture,
                    scope,
                    receipt: Ok(Destination { owner, client }),
                }
            })
            .collect();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let server_a = tokio::spawn(async move {
            let (method, _, reply) = fixture_a.next_request().await;
            assert_eq!(method, "server.updateProvider");
            let _ = reply.send(Ok(json!({
                "providers": [{ "instanceId": "codex", "updateState": { "status": "succeeded" } }]
            })));
            fixture_a
        });
        let server_b = tokio::spawn(async move {
            let (method, _, reply) = fixture_b.next_request().await;
            assert_eq!(method, "server.updateServer");
            let _ = reply.send(Err(crate::session::RpcError::Failure(
                t3_core::rpc::RpcFailure {
                    tag: None,
                    message: "network timeout".into(),
                    error: Value::Null,
                },
            )));
            // Second request is the retry targeting only env-b.
            let (method2, _, reply2) = fixture_b.next_request().await;
            assert_eq!(method2, "server.updateServer");
            let _ = reply2.send(Err(crate::session::RpcError::Failure(
                t3_core::rpc::RpcFailure {
                    tag: None,
                    message: "still offline".into(),
                    error: Value::Null,
                },
            )));
            fixture_b
        });

        let result = run_with_progress(plan.clone(), destinations, view_rx.clone(), Some(tx)).await;
        assert_eq!(result.updated_providers, 1);
        assert_eq!(result.updated_servers, 0);
        assert_eq!(result.failures, vec!["Beta: network timeout"]);
        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            seen.push(ev);
        }
        assert!(seen.contains(&UpdateEvent::ProvidersFinished {
            updated_providers: 1,
            failures: Vec::new(),
        }));
        assert!(seen.contains(&UpdateEvent::ServerStage {
            environment_id: "env-b".into(),
            stage: ServerUpdateStage::Downloading,
        }));
        assert!(seen.contains(&UpdateEvent::MachineSettled {
            environment_id: "env-a".into(),
            failure: None,
            server_updated: false,
            provider_failure: None,
        }));
        assert!(seen.contains(&UpdateEvent::MachineSettled {
            environment_id: "env-b".into(),
            failure: Some("network timeout".into()),
            server_updated: false,
            provider_failure: None,
        }));

        let retry_capture = capture(
            &Plan {
                servers: vec![plan.servers[0].clone()],
                ..Default::default()
            },
            &view_rx.borrow(),
        )
        .unwrap();
        let retry_dest = vec![UpdateDestination {
            capture: retry_capture.into_iter().next().unwrap(),
            scope: "environment:maintain",
            receipt: Ok(Destination {
                owner: owner_b,
                client: handle_b,
            }),
        }];
        let (rtx, mut rrx) = mpsc::unbounded_channel();
        let retry_failures = retry_servers_with_progress(
            vec![(plan.servers[0].clone(), None)],
            retry_dest,
            view_rx,
            Some(rtx),
        )
        .await;
        assert_eq!(retry_failures, vec!["Beta: still offline"]);
        let mut retry_events = Vec::new();
        while let Ok(ev) = rrx.try_recv() {
            retry_events.push(ev);
        }
        assert!(retry_events.contains(&UpdateEvent::MachineSettled {
            environment_id: "env-b".into(),
            failure: Some("still offline".into()),
            server_updated: false,
            provider_failure: None,
        }));
        let mut fa = server_a.await.unwrap();
        assert!(fa.take_commands().is_empty());
        let _fb = server_b.await.unwrap();
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn run_channel_switch_with_progress_dispatches_boot_service_and_track_only_hosts() {
        let session_json = json!({"authenticated":true,"permissions":["environment:maintain"]});
        let (handle_boot, mut fixture_boot) =
            crate::supervisor::ClientFixture::handle(serde_json::from_value(session_json.clone()).unwrap());
        let (handle_track, mut fixture_track) =
            crate::supervisor::ClientFixture::handle(serde_json::from_value(session_json).unwrap());

        let make_entry = |id: &str, config: Value, grants| {
            let mut cfg = t3_core::server_config::ServerConfigState::default();
            cfg.apply(serde_json::from_value(json!({"type":"snapshot","config":config})).unwrap());
            let owner = EnvironmentOwner {
                saved_id: id.into(),
                canonical_id: Some(id.into()),
                generation: 1,
                connection_generation: 1,
                shell_generation: 0,
            };
            let mut shell = t3_core::shell::ShellState::default();
            shell.synchronized = true;
            let summary = Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
                owner.clone(),
                &shell,
                &cfg,
            ));
            (
                owner,
                Arc::new(EnvironmentView {
                    summary,
                    status: ConnectionStatus::Connected,
                    config: cfg,
                    grants,
                    settings_revision: 1,
                    keybindings_revision: 1,
                }),
            )
        };
        let (owner_boot, entry_boot) = make_entry(
            "env-boot",
            json!({
                "environment": {
                    "environmentId": "env-boot",
                    "label": "Boot",
                    "serverVersion": "0.1.3-nightly.20261001.3500",
                    "databaseMigration": 69,
                    "capabilities": { "serverSelfUpdate": "boot-service" }
                },
                "providers": []
            }),
            handle_boot.session_state().borrow().clone(),
        );
        let (owner_track, entry_track) = make_entry(
            "env-track",
            json!({
                "environment": {
                    "environmentId": "env-track",
                    "label": "TrackOnly",
                    "serverVersion": "0.1.3",
                    "databaseMigration": 69,
                    "platform": { "os": "darwin", "machine": "arm64" },
                    "capabilities": {
                        "serverSelfUpdate": "desktop-managed",
                        "desktopAppUpdate": true,
                        "desktopAppChannelSwitch": true
                    }
                },
                "providers": []
            }),
            handle_track.session_state().borrow().clone(),
        );
        let (_tx, view_rx) = watch::channel(Arc::new(FleetView {
            revision: 1,
            environments: vec![entry_boot, entry_track],
        }));

        let release = updates::ChannelSwitchRelease {
            channel: UpdateTrack::Latest,
            version: "0.1.3".into(),
            database_migration: Some(69),
        };
        let cfg_boot = view_rx.borrow().environment("env-boot").unwrap().config.config.clone();
        let cfg_track = view_rx.borrow().environment("env-track").unwrap().config.config.clone();
        let machines = [
            updates::Machine {
                id: "env-boot",
                label: "Boot",
                config: cfg_boot.as_ref(),
                connected: true,
                hosted_by_this_app: false,
                can_maintain: true,
                can_manage_providers: false,
                updating: false,
            },
            updates::Machine {
                id: "env-track",
                label: "TrackOnly",
                config: cfg_track.as_ref(),
                connected: true,
                hosted_by_this_app: false,
                can_maintain: true,
                can_manage_providers: false,
                updating: false,
            },
        ];
        let switch = updates::collect_channel_switch(&machines, &release);
        assert_eq!(switch.targets.len(), 2);
        let captures = capture_channel_switch(&switch, &view_rx.borrow()).unwrap();
        let destinations = captures
            .into_iter()
            .map(|capture| {
                let (owner, client) = if capture.owner.saved_id == "env-boot" {
                    (owner_boot.clone(), handle_boot.clone())
                } else {
                    (owner_track.clone(), handle_track.clone())
                };
                UpdateDestination {
                    capture,
                    scope: "environment:maintain",
                    receipt: Ok(Destination { owner, client }),
                }
            })
            .collect();

        let boot_task = tokio::spawn(async move {
            let (method, input, reply) = fixture_boot.next_request().await;
            assert_eq!(method, "server.updateServer");
            assert!(
                input.get("channel").is_none(),
                "boot-service channel switch must omit RPC channel field"
            );
            assert_eq!(input["targetVersion"], "0.1.3");
            assert_eq!(input["targetDatabaseMigration"], 69);
            let _ = reply.send(Err(crate::session::RpcError::Failure(
                t3_core::rpc::RpcFailure {
                    tag: None,
                    message: "restart failed".into(),
                    error: Value::Null,
                },
            )));
            fixture_boot
        });
        let track_task = tokio::spawn(async move {
            let (method, input, reply) = fixture_track.next_request().await;
            assert_eq!(method, "server.updateServer");
            assert_eq!(input["channel"], "latest");
            assert_eq!(input["targetVersion"], "0.1.3");
            assert_eq!(input["targetDatabaseMigration"], 69);
            let _ = reply.send(Ok(json!({
                "targetVersion": "0.1.3",
                "method": "desktop-app"
            })));
            fixture_track
        });

        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let result = run_channel_switch_with_progress(
            switch,
            UpdateTrack::Latest,
            destinations,
            view_rx,
            Some(ev_tx),
        )
        .await;
        assert_eq!(result.updated_servers, 1);
        assert_eq!(result.failures, vec!["Boot: restart failed"]);
        let mut seen = Vec::new();
        while let Ok(ev) = ev_rx.try_recv() {
            seen.push(ev);
        }
        assert!(seen.iter().any(|ev| matches!(
            ev,
            UpdateEvent::MachineSettled {
                environment_id,
                server_updated: false,
                ..
            } if environment_id == "env-boot"
        )));
        assert!(!seen.iter().any(|ev| matches!(
            ev,
            UpdateEvent::MachineSettled { environment_id, .. } if environment_id == "env-track"
        )));
        let _ = boot_task.await.unwrap();
        let _ = track_task.await.unwrap();
    }
}

#[cfg(test)]
mod handoff_tests;
