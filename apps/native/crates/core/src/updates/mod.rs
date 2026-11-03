//! Ports `sidebar/updateEverything.logic.ts`, `UpdateEverythingDialog.tsx` and
//! `ProviderUpdateLaunchNotification.logic.ts`; no desktop bundle self-updater.
use crate::providers::driver_label;
use crate::server_config::{ServerConfig, ServerProvider};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

mod provider;
#[cfg(test)]
mod tests;
pub use provider::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerTarget {
    pub environment_id: String,
    pub label: String,
    pub target_version: String,
    pub self_update: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub environment_id: String,
    pub label: String,
    pub app: Option<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    pub skipped: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartingMachine {
    pub environment_id: String,
    pub continues_threads: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub servers: Vec<ServerTarget>,
    pub providers: Vec<ProviderTarget>,
    pub rows: Vec<Row>,
    pub restarts: Vec<RestartingMachine>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
