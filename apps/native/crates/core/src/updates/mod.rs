//! Ports `sidebar/updateEverything.logic.ts`, `UpdateEverythingDialog.tsx` and
//! `ProviderUpdateLaunchNotification.logic.ts`; no desktop bundle self-updater.
use crate::providers::driver_label;
use crate::server_config::{ServerConfig, ServerProvider};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

mod provider;
#[cfg(test)]
mod tests;
pub use provider::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerTarget {
    pub environment_id: String,
    pub label: String,
    pub target_version: String,
    pub self_update: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub environment_id: String,
    pub label: String,
    pub app: Option<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    pub skipped: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartingMachine {
    pub environment_id: String,
    pub continues_threads: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub servers: Vec<ServerTarget>,
    pub providers: Vec<ProviderTarget>,
    pub rows: Vec<Row>,
    pub restarts: Vec<RestartingMachine>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTarget {
    pub environment_id: String,
    pub driver: String,
    pub instance_id: String,
}

pub struct Machine<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub config: Option<&'a ServerConfig>,
    pub connected: bool,
    pub hosted_by_this_app: bool,
    pub can_maintain: bool,
    pub can_manage_providers: bool,
    pub updating: bool,
}

pub fn channel(version: &str) -> &str {
    let Some((core, prerelease)) = version.split_once('-') else {
        return "stable";
    };
    if core.is_empty() || core.contains('+') {
        return "stable";
    }
    let parts: Vec<_> = prerelease.split('.').collect();
    if let [track @ ("nightly" | "preview"), day, build] = parts.as_slice()
        && day.len() == 8
        && day.bytes().all(|c| c.is_ascii_digit())
        && !build.is_empty()
        && build.bytes().all(|c| c.is_ascii_digit())
    {
        return track;
    }
    "stable"
}
fn nightly(version: &str) -> bool {
    version
        .split_once('-')
        .is_some_and(|(_, p)| p.split('.').next() == Some("nightly"))
}

fn version_parts(version: &str) -> Option<Vec<u64>> {
    version
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()?
        .split('.')
        .map(str::parse)
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .filter(|p| p.len() == 3)
}
fn prerelease_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |s: &str| {
        s.split_once('-')
            .map(|(_, p)| {
                p.split('+')
                    .next()
                    .unwrap_or_default()
                    .split('.')
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let (a, b) = (parts(a), parts(b));
    for (a, b) in a.iter().zip(&b) {
        let order = match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(a), Ok(b)) => a.cmp(&b),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            _ => a.cmp(b),
        };
        if !order.is_eq() {
            return order;
        }
    }
    a.len().cmp(&b.len())
}
pub fn behind(server: &str, target: &str) -> bool {
    if server.trim().is_empty() || target.trim().is_empty() {
        return false;
    }
    match (version_parts(server), version_parts(target)) {
        (Some(a), Some(b)) => {
            a < b
                || (a == b
                    && nightly(server)
                    && nightly(target)
                    && prerelease_cmp(server, target).is_lt())
        }
        _ => server != target,
    }
}

pub fn managed_denial(policy: Option<&Value>, target: Option<&str>) -> Option<String> {
    let policy = policy?;
    let limits = &policy["limits"]["updates"];
    let organization = policy["organization"]
        .as_str()
        .unwrap_or("Your organization");
    if limits["mode"] == "off" {
        return Some(format!("{organization} manages T3 Code updates."));
    }
    if limits["allowNightly"] == false && target.is_some_and(|v| channel(v) != "stable") {
        return Some(format!(
            "{organization} manages T3 Code updates. Only stable releases can be installed."
        ));
    }
    None
}

pub fn collect(machines: &[Machine<'_>], target_version: &str) -> Plan {
    let mut plan = Plan::default();
    // Server rows precede provider-only rows, matching describeUpdateEverything's Map order.
    let mut skipped = Vec::new();
    for machine in machines {
        if machine.hosted_by_this_app {
            continue;
        }
        let config = machine.config;
        if config.is_some_and(|c| !behind(&c.environment.server_version, target_version)) {
            continue;
        }
        let capability =
            config.and_then(|c| c.environment.capabilities.server_self_update.as_deref());
        let reason = if config.is_none() || !machine.connected {
            Some("not connected".into())
        } else if !machine.can_maintain {
            Some("no permission".into())
        } else if machine.updating {
            Some("already updating".into())
        } else if let Some(denial) = managed_denial(
            config.and_then(|c| c.managed_policy.as_ref()),
            if capability == Some("desktop-managed") {
                None
            } else {
                Some(target_version)
            },
        ) {
            Some(denial)
        } else if capability.is_none() {
            Some("needs a manual update".into())
        } else if capability == Some("desktop-managed")
            && !config.is_some_and(|c| c.environment.capabilities.desktop_app_update == Some(true))
        {
            Some("needs a newer desktop app".into())
        } else if capability != Some("desktop-managed")
            && config
                .is_some_and(|c| channel(&c.environment.server_version) != channel(target_version))
        {
            Some(format!(
                "on the {} channel",
                channel(
                    config
                        .map(|c| c.environment.server_version.as_str())
                        .unwrap_or_default()
                )
            ))
        } else {
            None
        };
        if let Some(reason) = reason {
            skipped.push((machine, reason));
            continue;
        }
        let capability = capability.unwrap_or_default();
        plan.servers.push(ServerTarget {
            environment_id: machine.id.into(),
            label: machine.label.into(),
            target_version: target_version.into(),
            self_update: capability.into(),
        });
        plan.rows.push(Row {
            environment_id: machine.id.into(),
            label: machine.label.into(),
            app: Some(
                if capability == "desktop-managed" {
                    "Desktop app"
                } else {
                    "Server"
                }
                .into(),
            ),
            providers: vec![],
            skipped: None,
        });
    }
    for machine in machines {
        if !machine.connected || !machine.can_manage_providers {
            continue;
        }
        let Some(config) = machine.config else {
            continue;
        };
        let candidates = one_click_candidates(&config.providers);
        if candidates.is_empty() {
            continue;
        }
        let providers = candidates.iter().map(|p| driver_label(&p.driver)).collect();
        let row = row_mut(&mut plan.rows, machine.id, machine.label);
        row.providers = providers;
        plan.providers
            .extend(candidates.into_iter().map(|p| ProviderTarget {
                environment_id: machine.id.into(),
                driver: p.driver.clone(),
                instance_id: p.instance_id.clone(),
            }));
    }
    for (machine, reason) in skipped {
        row_mut(&mut plan.rows, machine.id, machine.label).skipped = Some(reason);
    }
    plan.restarts = collect_restarting(machines, &plan.servers, false);
    plan
}
fn row_mut<'a>(rows: &'a mut Vec<Row>, id: &str, label: &str) -> &'a mut Row {
    let index = rows
        .iter()
        .position(|r| r.environment_id == id)
        .unwrap_or_else(|| {
            rows.push(Row {
                environment_id: id.into(),
                label: label.into(),
                app: None,
                providers: vec![],
                skipped: None,
            });
            rows.len() - 1
        });
    &mut rows[index]
}
pub fn collect_restarting(
    machines: &[Machine<'_>],
    servers: &[ServerTarget],
    installs_local: bool,
) -> Vec<RestartingMachine> {
    machines
        .iter()
        .filter(|m| {
            m.connected
                && m.config.is_some()
                && (servers.iter().any(|s| s.environment_id == m.id)
                    || (installs_local && m.hosted_by_this_app))
        })
        .map(|m| RestartingMachine {
            environment_id: m.id.into(),
            continues_threads: m.config.unwrap().settings.update_continuation_advertised,
        })
        .collect()
}
impl Plan {
    pub fn has_updates(&self) -> bool {
        !self.servers.is_empty() || !self.providers.is_empty()
    }
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (count, noun) in [
            (self.servers.len(), "server"),
            (self.providers.len(), "provider"),
        ] {
            if count > 0 {
                parts.push(format!(
                    "{count} {noun}{}",
                    if count == 1 { "" } else { "s" }
                ));
            }
        }
        format!("Update {}", join_list(&parts))
    }
    pub fn updating_rows(&self) -> impl Iterator<Item = &Row> {
        self.rows
            .iter()
            .filter(|r| r.app.is_some() || !r.providers.is_empty())
    }
    pub fn title(&self) -> String {
        let n = self.updating_rows().count();
        format!(
            "Update {n} {}?",
            if n == 1 { "machine" } else { "machines" }
        )
    }
    pub fn version(&self) -> Option<&str> {
        self.servers
            .iter()
            .find(|s| s.self_update != "desktop-managed")
            .map(|s| s.target_version.as_str())
    }
}
pub fn join_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        _ => format!(
            "{}, and {}",
            names[..names.len() - 1].join(", "),
            names.last().unwrap()
        ),
    }
}
pub fn running(status: &str) -> bool {
    matches!(status, "preparing" | "starting" | "running")
}
pub fn running_counts<'a>(
    restarts: &[RestartingMachine],
    threads: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<(String, usize)> {
    let mut counts: Vec<_> = restarts
        .iter()
        .map(|r| (r.environment_id.clone(), 0))
        .collect();
    for (environment, status) in threads {
        if running(status)
            && let Some((_, count)) = counts.iter_mut().find(|(id, _)| id == environment)
        {
            *count += 1;
        }
    }
    counts
}
pub fn thread_warning(stopped: usize, continuing: usize) -> Option<String> {
    let count = stopped + continuing;
    if count == 0 {
        return None;
    }
    let threads = format!(
        "{count} running {}",
        if count == 1 { "thread" } else { "threads" }
    );
    Some(if stopped == 0 {
        format!("{threads} will be interrupted and can resume after the restart.")
    } else if continuing == 0 {
        format!("{threads} will be interrupted.")
    } else {
        format!("{threads} will be interrupted. {continuing} can resume after the restart.")
    })
}

/// A service acknowledgement is a handoff, never proof that the trial committed.
pub fn ready_outcome(result: &Value, event: &Value) -> Option<Result<(), String>> {
    if event["type"] != "ready" || result["targetVersion"].as_str().is_none_or(str::is_empty) {
        return None;
    }
    let payload = &event["payload"];
    if let Some(id) = result["updateId"].as_str() {
        let outcome = &payload["updateOutcome"];
        if outcome["id"] != id {
            return None;
        }
        return Some(
            if outcome["status"] == "committed"
                && outcome["targetVersion"] == result["targetVersion"]
                && payload["environment"]["serverVersion"] == result["targetVersion"]
            {
                Ok(())
            } else {
                Err(outcome["reason"].as_str().unwrap_or("The service launcher resumed without committing the requested server version.").into())
            },
        );
    }
    (payload["environment"]["serverVersion"] == result["targetVersion"]).then_some(Ok(()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateTrack {
    Latest,
    Nightly,
}

impl UpdateTrack {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Latest => "latest",
            Self::Nightly => "nightly",
        }
    }

    pub fn release_channel(self) -> &'static str {
        match self {
            Self::Latest => "stable",
            Self::Nightly => "nightly",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Latest => "Stable",
            Self::Nightly => "Nightly",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSwitchRelease {
    pub channel: UpdateTrack,
    pub version: String,
    #[serde(default)]
    pub database_migration: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSwitchTarget {
    pub server: ServerTarget,
    #[serde(default)]
    pub channel: Option<UpdateTrack>,
    #[serde(default)]
    pub target_database_migration: Option<u64>,
    #[serde(default)]
    pub track_only: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSwitchPlan {
    pub plan: Plan,
    pub targets: Vec<ChannelSwitchTarget>,
}

fn has_prerelease(version: &str) -> bool {
    version.trim().split_once('-').is_some_and(|(_, p)| {
        !p.split('+').next().unwrap_or_default().is_empty()
    })
}

fn semver_cmp(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let core = version_parts(a)?.cmp(&version_parts(b)?);
    if !core.is_eq() {
        return Some(core);
    }
    Some(match (has_prerelease(a), has_prerelease(b)) {
        (false, false) => std::cmp::Ordering::Equal,
        (false, true) => std::cmp::Ordering::Greater,
        (true, false) => std::cmp::Ordering::Less,
        (true, true) => prerelease_cmp(a, b),
    })
}

pub fn is_on_track(version: &str, track: UpdateTrack) -> bool {
    let ch = channel(version);
    ch != "preview" && ch == track.release_channel()
}

pub fn awaits_track_switch(machine: &Machine<'_>, track: UpdateTrack) -> bool {
    !machine.hosted_by_this_app
        && machine.config.is_some_and(|config| {
            !is_on_track(&config.environment.server_version, track)
                && track_switch_move(config, false).is_ok()
        })
}

pub fn channel_switch_block_reason(
    current_version: &str,
    release: &ChannelSwitchRelease,
    server_database_migration: Option<u64>,
) -> Option<String> {
    if is_on_track(current_version, release.channel) {
        return None;
    }
    let target_migration = release.database_migration?;
    let Some(current_migration) = server_database_migration else {
        return Some("its server predates database checks; update it on Nightly first".into());
    };
    if current_migration > target_migration {
        let stay_on = if release.channel == UpdateTrack::Latest {
            "Nightly"
        } else {
            "Stable"
        };
        return Some(format!(
            "its database has changes {} does not support yet; stay on {stay_on} until {} has them",
            release.version,
            release.channel.label()
        ));
    }
    None
}

fn track_switch_move(config: &ServerConfig, same_track: bool) -> Result<&str, String> {
    if channel(&config.environment.server_version) == "preview" {
        return Err("runs a preview build".into());
    }
    let Some(self_update) = config.environment.capabilities.server_self_update.as_deref() else {
        return Err("needs a manual update".into());
    };
    if self_update != "desktop-managed" {
        return Ok(self_update);
    }
    if config.environment.capabilities.desktop_app_channel_switch != Some(true) {
        return Err("needs a newer desktop app".into());
    }
    if !same_track
        && config
            .environment
            .platform
            .as_ref()
            .is_some_and(|p| p.os == "darwin")
    {
        return Err("switch the Mac app on that Mac".into());
    }
    Ok(self_update)
}

fn machine_unavailable_reason(machine: &Machine<'_>) -> Option<&'static str> {
    if machine.config.is_none() || !machine.connected {
        Some("not connected")
    } else if !machine.can_maintain {
        Some("no permission")
    } else if machine.updating {
        Some("already updating")
    } else {
        None
    }
}

pub fn collect_channel_switch(
    machines: &[Machine<'_>],
    release: &ChannelSwitchRelease,
) -> ChannelSwitchPlan {
    let mut plan = Plan::default();
    let mut targets = Vec::new();
    let mut skipped = Vec::new();
    for machine in machines {
        if machine.hosted_by_this_app {
            continue;
        }
        let config = machine.config;
        let server_version = config
            .map(|c| c.environment.server_version.as_str())
            .filter(|v| !v.is_empty());
        let same_track = server_version.is_some_and(|v| is_on_track(v, release.channel));
        if let Some(config) = config
            && let Some(sv) = server_version
            && same_track
            && !behind(sv, &release.version)
        {
            if let Ok(self_update) = track_switch_move(config, true)
                && self_update == "desktop-managed"
                && machine_unavailable_reason(machine).is_none()
                && managed_denial(config.managed_policy.as_ref(), Some(&release.version)).is_none()
            {
                let server = ServerTarget {
                    environment_id: machine.id.into(),
                    label: machine.label.into(),
                    target_version: sv.into(),
                    self_update: self_update.into(),
                };
                targets.push(ChannelSwitchTarget {
                    server,
                    channel: Some(release.channel),
                    target_database_migration: release.database_migration,
                    track_only: true,
                });
            }
            continue;
        }
        if let Some(reason) = machine_unavailable_reason(machine) {
            skipped.push((machine, reason.to_owned()));
            continue;
        }
        let config = config.unwrap();
        let sv = server_version.unwrap_or_default();
        if let Some(denial) = managed_denial(config.managed_policy.as_ref(), Some(&release.version))
        {
            skipped.push((machine, denial));
            continue;
        }
        let self_update = match track_switch_move(config, same_track) {
            Ok(capability) => capability,
            Err(reason) => {
                skipped.push((machine, reason));
                continue;
            }
        };
        if let Some(blocked) =
            channel_switch_block_reason(sv, release, config.environment.database_migration)
        {
            skipped.push((machine, blocked));
            continue;
        }
        if self_update == "boot-service"
            && semver_cmp(&release.version, sv).is_some_and(|ord| ord.is_le())
        {
            skipped.push((
                machine,
                format!(
                    "run `t3 update --channel {} --allow-downgrade` there",
                    release.channel.label().to_lowercase()
                ),
            ));
            continue;
        }
        let server = ServerTarget {
            environment_id: machine.id.into(),
            label: machine.label.into(),
            target_version: release.version.clone(),
            self_update: self_update.into(),
        };
        plan.servers.push(server.clone());
        plan.rows.push(Row {
            environment_id: machine.id.into(),
            label: machine.label.into(),
            app: Some(
                if self_update == "desktop-managed" {
                    "Desktop app"
                } else {
                    "Server"
                }
                .into(),
            ),
            providers: vec![],
            skipped: None,
        });
        targets.push(ChannelSwitchTarget {
            server,
            channel: (self_update == "desktop-managed").then_some(release.channel),
            target_database_migration: release.database_migration,
            track_only: false,
        });
    }
    for (machine, reason) in skipped {
        row_mut(&mut plan.rows, machine.id, machine.label).skipped = Some(reason);
    }
    let moving: Vec<_> = targets
        .iter()
        .filter(|t| !t.track_only)
        .map(|t| t.server.clone())
        .collect();
    plan.restarts = collect_restarting(machines, &moving, false);
    ChannelSwitchPlan { plan, targets }
}

pub const UPDATE_SLOW_AFTER_MS: i64 = 3 * 60 * 1000;
pub const HANDOFF_TIMEOUT_MS: i64 = 32 * 60 * 1000;
pub const SAVED_RESULT_MAX_AGE_MS: i64 = 30 * 60 * 1000;
pub const SAVED_RESULT_STORE_KEY: &str = "update-everything-result";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerUpdateStage {
    Downloading,
    Installing,
    Resuming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateRunMachineStatus {
    Providers,
    Waiting,
    Downloading,
    Restarting,
    Done,
    Failed,
}

impl UpdateRunMachineStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Providers => "updating providers",
            Self::Waiting => "waiting",
            Self::Downloading => "downloading",
            Self::Restarting => "restarting",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub fn is_active(self) -> bool {
        !matches!(self, Self::Done | Self::Failed)
    }

    pub fn is_working(self) -> bool {
        !matches!(self, Self::Done | Self::Failed | Self::Waiting)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MachineResult {
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateRunPhase {
    Providers,
    Servers,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRunMachine {
    pub environment_id: String,
    pub label: String,
    pub updates_providers: bool,
    pub server: Option<ServerTarget>,
    pub handed_off_version: Option<String>,
    pub started_at_ms: i64,
    pub result: Option<MachineResult>,
    pub failure: Option<String>,
    pub provider_failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateEverythingRun {
    #[serde(default)]
    pub track_switch: bool,
    pub phase: UpdateRunPhase,
    pub machines: Vec<UpdateRunMachine>,
}

impl UpdateEverythingRun {
    pub fn from_plan(plan: &Plan, track_switch: bool, started_at_ms: i64) -> Self {
        let mut machines: Vec<UpdateRunMachine> = Vec::new();
        for row in &plan.rows {
            let updates_providers = plan
                .providers
                .iter()
                .any(|p| p.environment_id == row.environment_id);
            let server = plan
                .servers
                .iter()
                .find(|s| s.environment_id == row.environment_id)
                .cloned();
            if !updates_providers && server.is_none() {
                continue;
            }
            machines.push(UpdateRunMachine {
                environment_id: row.environment_id.clone(),
                label: row.label.clone(),
                updates_providers,
                server,
                handed_off_version: None,
                started_at_ms,
                result: None,
                failure: None,
                provider_failure: None,
            });
        }
        for server in &plan.servers {
            if !machines.iter().any(|m| m.environment_id == server.environment_id) {
                machines.push(UpdateRunMachine {
                    environment_id: server.environment_id.clone(),
                    label: server.label.clone(),
                    updates_providers: false,
                    server: Some(server.clone()),
                    handed_off_version: None,
                    started_at_ms,
                    result: None,
                    failure: None,
                    provider_failure: None,
                });
            }
        }
        let phase = if machines.iter().any(|m| m.updates_providers) {
            UpdateRunPhase::Providers
        } else {
            UpdateRunPhase::Servers
        };
        Self {
            track_switch,
            phase,
            machines,
        }
    }

    pub fn has_active_machine(&self, stages: &[(String, Option<ServerUpdateStage>)]) -> bool {
        self.machines
            .iter()
            .any(|m| resolve_machine_status(m, self.phase, stage_for(stages, &m.environment_id)).is_working())
    }

    pub fn is_updating_environment(&self, environment_id: &str) -> bool {
        self.machines
            .iter()
            .any(|m| m.environment_id == environment_id && m.result.is_none())
    }
}

fn stage_for(
    stages: &[(String, Option<ServerUpdateStage>)],
    environment_id: &str,
) -> Option<ServerUpdateStage> {
    stages
        .iter()
        .find(|(id, _)| id == environment_id)
        .and_then(|(_, stage)| *stage)
}

pub fn resolve_machine_status(
    machine: &UpdateRunMachine,
    phase: UpdateRunPhase,
    server_stage: Option<ServerUpdateStage>,
) -> UpdateRunMachineStatus {
    match machine.result {
        Some(MachineResult::Done) => return UpdateRunMachineStatus::Done,
        Some(MachineResult::Failed) => return UpdateRunMachineStatus::Failed,
        None => {}
    }
    if machine.handed_off_version.is_some() {
        return UpdateRunMachineStatus::Restarting;
    }
    if phase == UpdateRunPhase::Providers && machine.updates_providers {
        return UpdateRunMachineStatus::Providers;
    }
    if machine.server.is_some()
        && let Some(stage) = server_stage
    {
        return match stage {
            ServerUpdateStage::Resuming => UpdateRunMachineStatus::Restarting,
            ServerUpdateStage::Downloading | ServerUpdateStage::Installing => {
                UpdateRunMachineStatus::Downloading
            }
        };
    }
    UpdateRunMachineStatus::Waiting
}

pub fn format_update_elapsed(ms: i64) -> String {
    let minutes = ms.max(0) / 60_000;
    if minutes < 1 {
        "<1m".into()
    } else if minutes < 60 {
        format!("{minutes}m")
    } else {
        format!("{}h {}m", minutes / 60, minutes % 60)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsuleTone {
    Default,
    Slow,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsuleAction {
    Retry,
    Dismiss,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleView {
    pub tone: CapsuleTone,
    pub label: String,
    pub slow: Option<(String, String)>,
    pub action: Option<CapsuleAction>,
}

pub fn describe_update_capsule(
    run: &UpdateEverythingRun,
    stages: &[(String, Option<ServerUpdateStage>)],
    now_ms: i64,
) -> CapsuleView {
    let statuses: Vec<_> = run
        .machines
        .iter()
        .map(|m| (m, resolve_machine_status(m, run.phase, stage_for(stages, &m.environment_id))))
        .collect();
    if let Some((slowest, status)) = statuses
        .iter()
        .filter(|(_, s)| s.is_working())
        .min_by_key(|(m, _)| m.started_at_ms)
        && now_ms - slowest.started_at_ms >= UPDATE_SLOW_AFTER_MS
    {
        return CapsuleView {
            tone: CapsuleTone::Slow,
            label: slowest.label.clone(),
            slow: Some((
                status.label().into(),
                format_update_elapsed(now_ms - slowest.started_at_ms),
            )),
            action: None,
        };
    }
    let done = statuses
        .iter()
        .filter(|(_, s)| *s == UpdateRunMachineStatus::Done)
        .count();
    if statuses.iter().any(|(_, s)| s.is_active()) {
        return CapsuleView {
            tone: CapsuleTone::Default,
            label: format!("{done} of {} updated", statuses.len()),
            slow: None,
            action: None,
        };
    }
    let failed: Vec<_> = statuses
        .iter()
        .filter(|(_, s)| *s == UpdateRunMachineStatus::Failed)
        .collect();
    if !failed.is_empty() {
        let can_retry = !run.track_switch && failed.iter().any(|(m, _)| m.server.is_some());
        return CapsuleView {
            tone: CapsuleTone::Failed,
            label: if failed.len() == 1 {
                format!("{} failed", failed[0].0.label)
            } else {
                format!("{} failed", failed.len())
            },
            slow: None,
            action: Some(if can_retry {
                CapsuleAction::Retry
            } else {
                CapsuleAction::Dismiss
            }),
        };
    }
    CapsuleView {
        tone: CapsuleTone::Default,
        label: format!("{done} of {} updated", statuses.len()),
        slow: None,
        action: None,
    }
}

pub fn close_finished_run(run: &UpdateEverythingRun) -> bool {
    run.phase == UpdateRunPhase::Servers
        && run
            .machines
            .iter()
            .all(|m| m.result == Some(MachineResult::Done))
}

pub fn settle_machine(
    run: &mut Option<UpdateEverythingRun>,
    environment_id: &str,
    failure: Option<String>,
    server_updated: bool,
    provider_failure: Option<Option<String>>,
) {
    let Some(current) = run.as_mut() else {
        return;
    };
    for machine in &mut current.machines {
        if machine.environment_id == environment_id {
            machine.result = Some(if failure.is_none() {
                MachineResult::Done
            } else {
                MachineResult::Failed
            });
            machine.failure = failure.clone();
            machine.handed_off_version = None;
            if server_updated {
                machine.server = None;
            }
            if let Some(provider_failure) = provider_failure.clone() {
                machine.provider_failure = provider_failure;
            }
        }
    }
    if close_finished_run(current) {
        *run = None;
    }
}

pub fn begin_servers_phase(run: &mut Option<UpdateEverythingRun>, now_ms: i64) {
    let Some(current) = run.as_mut() else {
        return;
    };
    current.phase = UpdateRunPhase::Servers;
    for machine in &mut current.machines {
        if machine.server.is_some() {
            machine.started_at_ms = now_ms;
        }
    }
    if close_finished_run(current) {
        *run = None;
    }
}

pub fn retry_failed_servers(
    run: &mut Option<UpdateEverythingRun>,
    now_ms: i64,
) -> Vec<(ServerTarget, Option<String>)> {
    retry_failed_servers_with_versions(run, now_ms, |_| None)
}

/// Retries failed server machines in `run`, skipping any server that has
/// already reconnected on the target version (`!behind(live_version, target)`).
pub fn retry_failed_servers_with_versions<'a>(
    run: &mut Option<UpdateEverythingRun>,
    now_ms: i64,
    mut live_server_version: impl FnMut(&str) -> Option<&'a str>,
) -> Vec<(ServerTarget, Option<String>)> {
    let Some(current) = run.as_mut() else {
        return Vec::new();
    };
    if current.track_switch {
        return Vec::new();
    }
    let mut already_updated = Vec::new();
    let mut targets = Vec::new();
    for machine in &mut current.machines {
        if machine.result == Some(MachineResult::Failed)
            && let Some(server) = machine.server.clone()
        {
            if live_server_version(&server.environment_id)
                .is_some_and(|version| !behind(version, &server.target_version))
            {
                already_updated.push((
                    machine.environment_id.clone(),
                    machine.provider_failure.clone(),
                ));
                continue;
            }
            machine.result = None;
            machine.failure = None;
            machine.handed_off_version = None;
            machine.started_at_ms = now_ms;
            targets.push((server, machine.provider_failure.clone()));
        }
    }
    if !targets.is_empty() || !already_updated.is_empty() {
        current.phase = UpdateRunPhase::Servers;
    }
    for (environment_id, provider_failure) in already_updated {
        settle_machine(run, &environment_id, provider_failure, true, None);
    }
    targets
}

pub fn dismiss_settled_machines(run: &mut Option<UpdateEverythingRun>) {
    let Some(current) = run.as_mut() else {
        return;
    };
    current.machines.retain(|m| m.result.is_none());
    if current.machines.is_empty() || close_finished_run(current) {
        *run = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerOutcome {
    Updated,
    Skipped,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderOutcome {
    Interrupted,
    Updated,
    Failed(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateEverythingResult {
    pub updated_servers: usize,
    pub updated_providers: usize,
    pub failures: Vec<String>,
    pub restarting: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastView {
    pub is_error: bool,
    pub title: String,
    pub description: Option<String>,
}

fn count_label(count: usize, noun: &str) -> Option<String> {
    (count > 0).then(|| format!("{count} {noun}{}", if count == 1 { "" } else { "s" }))
}

pub fn collect_update_everything_result(
    servers: &[(&str, &ServerOutcome)],
    providers: &[ProviderOutcome],
) -> UpdateEverythingResult {
    let updated_servers = servers
        .iter()
        .filter(|(_, outcome)| matches!(outcome, ServerOutcome::Updated))
        .count();
    let updated_providers = providers
        .iter()
        .filter(|outcome| matches!(outcome, ProviderOutcome::Updated))
        .count();
    let mut failures = Vec::new();
    for (label, outcome) in servers {
        if let ServerOutcome::Failed(message) = outcome {
            failures.push(format!("{label}: {message}"));
        }
    }
    for outcome in providers {
        if let ProviderOutcome::Failed(line) = outcome {
            failures.push(line.clone());
        }
    }
    UpdateEverythingResult {
        updated_servers,
        updated_providers,
        failures,
        restarting: Vec::new(),
    }
}

pub fn update_everything_toast_view(result: &UpdateEverythingResult) -> Option<ToastView> {
    let parts: Vec<String> = [
        count_label(result.updated_servers, "server"),
        count_label(result.updated_providers, "provider"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut lines = result.failures.clone();
    if !result.restarting.is_empty() {
        lines.push(format!(
            "{} {} still restarting.",
            join_list(&result.restarting),
            if result.restarting.len() == 1 {
                "is"
            } else {
                "are"
            }
        ));
    }
    if result.failures.is_empty() {
        if parts.is_empty() {
            if result.restarting.is_empty() {
                return None;
            }
            return Some(ToastView {
                is_error: false,
                title: lines.join("\n"),
                description: None,
            });
        }
        return Some(ToastView {
            is_error: false,
            title: format!("Updated {}", join_list(&parts)),
            description: (!lines.is_empty()).then(|| lines.join("\n")),
        });
    }
    Some(ToastView {
        is_error: true,
        title: if parts.is_empty() {
            "Update failed".into()
        } else {
            format!("Updated {}", join_list(&parts))
        },
        description: Some(lines.join("\n")),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandedOffServer {
    pub environment_id: String,
    pub label: String,
    pub target_version: String,
    pub started_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedUpdateEverythingResult {
    pub saved_at_ms: i64,
    pub updated_servers: usize,
    pub updated_providers: usize,
    pub failures: Vec<String>,
    #[serde(default)]
    pub handed_off: Vec<HandedOffServer>,
}

impl SavedUpdateEverythingResult {
    pub fn from_run(
        run: Option<&UpdateEverythingRun>,
        stages: &[(String, Option<ServerUpdateStage>)],
        result: &UpdateEverythingResult,
        saved_at_ms: i64,
    ) -> Self {
        let handed_off = run
            .map(|run| {
                run.machines
                    .iter()
                    .filter(|m| m.result.is_none())
                    .filter_map(|m| {
                        let target_version = m.handed_off_version.clone().or_else(|| {
                            stage_for(stages, &m.environment_id)
                                .is_some()
                                .then(|| m.server.as_ref().map(|s| s.target_version.clone()))
                                .flatten()
                        })?;
                        Some(HandedOffServer {
                            environment_id: m.environment_id.clone(),
                            label: m.label.clone(),
                            target_version,
                            started_at_ms: m.started_at_ms,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            saved_at_ms,
            updated_servers: result.updated_servers,
            updated_providers: result.updated_providers,
            failures: result.failures.clone(),
            handed_off,
        }
    }

    pub fn into_toast_result(&self) -> UpdateEverythingResult {
        UpdateEverythingResult {
            updated_servers: self.updated_servers,
            updated_providers: self.updated_providers,
            failures: self.failures.clone(),
            restarting: self.handed_off.iter().map(|s| s.label.clone()).collect(),
        }
    }

    pub fn into_resuming_run(&self) -> Option<UpdateEverythingRun> {
        if self.handed_off.is_empty() {
            return None;
        }
        Some(UpdateEverythingRun {
            track_switch: false,
            phase: UpdateRunPhase::Servers,
            machines: self
                .handed_off
                .iter()
                .map(|server| UpdateRunMachine {
                    environment_id: server.environment_id.clone(),
                    label: server.label.clone(),
                    updates_providers: false,
                    server: None,
                    handed_off_version: Some(server.target_version.clone()),
                    started_at_ms: server.started_at_ms,
                    result: None,
                    failure: None,
                    provider_failure: None,
                })
                .collect(),
        })
    }
}

pub fn decode_fresh_saved_result(
    value: Value,
    now_ms: i64,
) -> Option<SavedUpdateEverythingResult> {
    let saved: SavedUpdateEverythingResult = serde_json::from_value(value).ok()?;
    (now_ms - saved.saved_at_ms < SAVED_RESULT_MAX_AGE_MS).then_some(saved)
}
