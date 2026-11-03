use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Loading,
    Warning,
    Error,
    Success,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pill {
    pub key: String,
    pub tone: Tone,
    pub title: String,
    pub description: String,
    pub dismissible: bool,
    pub dismiss_after_ms: Option<u64>,
}
pub fn active(provider: &ServerProvider) -> bool {
    provider
        .update_state
        .as_ref()
        .is_some_and(|s| matches!(s.status.as_str(), "queued" | "running"))
}
pub fn candidate(provider: &ServerProvider) -> bool {
    provider.enabled
        && !provider
            .compatibility_advisory
            .as_ref()
            .and_then(|a| a.latest_version_status.as_deref())
            .is_some_and(|s| matches!(s, "broken" | "unsupported"))
        && provider
            .version_advisory
            .as_ref()
            .is_some_and(|a| a.status == "behind_latest" && a.latest_version.is_some())
}
fn dedupe(providers: Vec<&ServerProvider>) -> Vec<&ServerProvider> {
    let mut output: Vec<&ServerProvider> = Vec::new();
    for provider in providers {
        if let Some(index) = output.iter().position(|p| p.driver == provider.driver) {
            let old = output[index];
            if provider.instance_id == provider.driver
                || (old.instance_id != old.driver && provider.checked_at.0 >= old.checked_at.0)
            {
                output[index] = provider;
            }
        } else {
            output.push(provider);
        }
    }
    output
}
pub fn one_click_candidates(providers: &[ServerProvider]) -> Vec<&ServerProvider> {
    dedupe(providers.iter().filter(|p| candidate(p)).collect())
        .into_iter()
        .filter(|p| {
            if active(p) {
                return false;
            }
            let matching: Vec<_> = providers
                .iter()
                .filter(|other| other.driver == p.driver && candidate(other))
                .collect();
            let commands: HashSet<_> = matching
                .iter()
                .filter_map(|p| {
                    p.version_advisory
                        .as_ref()
                        .and_then(|a| a.update_command.as_deref())
                })
                .collect();
            commands.len() == 1
                && matching.iter().all(|p| {
                    p.version_advisory
                        .as_ref()
                        .is_some_and(|a| a.can_update && a.update_command.is_some())
                })
        })
        .collect()
}
fn version(value: &str) -> String {
    if value.starts_with('v') {
        value.into()
    } else {
        format!("v{value}")
    }
}
fn names(providers: &[&ServerProvider]) -> String {
    join_list(
        &providers
            .iter()
            .map(|p| driver_label(&p.driver))
            .collect::<Vec<_>>(),
    )
}

pub fn provider_pill(
    providers: &[ServerProvider],
    visible_after: Option<&str>,
    dismissed: &HashSet<String>,
) -> Option<Pill> {
    let active = dedupe(providers.iter().filter(|p| active(p)).collect());
    if !active.is_empty() {
        let mut keys: Vec<_> = active
            .iter()
            .map(|p| format!("{}:{}", p.driver, p.update_state.as_ref().unwrap().status))
            .collect();
        keys.sort();
        return Some(Pill {
            key: format!("loading:{}", keys.join("|")),
            tone: Tone::Loading,
            title: if active.len() == 1 {
                format!("Updating {}", driver_label(&active[0].driver))
            } else {
                format!("Updating {} providers", active.len())
            },
            description: format!(
                "{} {}",
                names(&active),
                if active.len() == 1 {
                    "update in progress."
                } else {
                    "updates are in progress."
                }
            ),
            dismissible: false,
            dismiss_after_ms: None,
        });
    }
    let mut terminal: Vec<&ServerProvider> = Vec::new();
    for provider in providers {
        let Some(state) = provider.update_state.as_ref() else {
            continue;
        };
        if !matches!(state.status.as_str(), "failed" | "unchanged" | "succeeded")
            || visible_after
                .is_some_and(|after| state.finished_at.as_deref().is_none_or(|at| at < after))
        {
            continue;
        }
        if let Some(index) = terminal.iter().position(|p| p.driver == provider.driver) {
            if state.finished_at >= terminal[index].update_state.as_ref().unwrap().finished_at {
                terminal[index] = provider;
            }
        } else {
            terminal.push(provider);
        }
    }
    if terminal.is_empty() {
        let available = dedupe(providers.iter().filter(|p| candidate(p)).collect());
        if let Some(first) = available.first() {
            let mut keys: Vec<_> = available
                .iter()
                .map(|p| {
                    format!(
                        "{}:{}",
                        p.driver,
                        p.version_advisory
                            .as_ref()
                            .and_then(|a| a.latest_version.as_deref())
                            .unwrap_or_default()
                    )
                })
                .collect();
            keys.sort();
            let key = format!("available:{}", keys.join("|"));
            if dismissed.contains(&key) {
                return None;
            }
            let detail = first
                .version_advisory
                .as_ref()
                .and_then(|a| a.message.clone())
                .unwrap_or_else(|| {
                    format!(
                        "Update available: install {}.",
                        version(
                            first
                                .version_advisory
                                .as_ref()
                                .and_then(|a| a.latest_version.as_deref())
                                .unwrap_or_default()
                        )
                    )
                });
            return Some(Pill {
                key,
                tone: Tone::Warning,
                title: if available.len() == 1 {
                    format!("{}: Update available", driver_label(&first.driver))
                } else {
                    format!("{} provider updates", available.len())
                },
                description: detail,
                dismissible: true,
                dismiss_after_ms: None,
            });
        }
    }
    let mut views = Vec::new();
    for (status, tone) in [
        ("failed", Tone::Error),
        ("unchanged", Tone::Warning),
        ("succeeded", Tone::Success),
    ] {
        let selected: Vec<_> = terminal
            .iter()
            .copied()
            .filter(|p| p.update_state.as_ref().unwrap().status == status)
            .collect();
        let Some(first) = selected.first() else {
            continue;
        };
        let n = selected.len();
        let name = driver_label(&first.driver);
        let title = match status {
            "failed" if n == 1 => first
                .version_advisory
                .as_ref()
                .and_then(|a| a.latest_version.as_deref())
                .map_or_else(
                    || format!("{name} update failed"),
                    |v| format!("{name} {} update failed", version(v)),
                ),
            "failed" => format!("{n} provider updates failed"),
            "unchanged" if n == 1 => format!("{name} still needs an update"),
            "unchanged" => format!("{n} providers still need updates"),
            _ if n == 1 => first.version.as_deref().map_or_else(
                || format!("{name} updated"),
                |v| format!("{name} updated: {}", version(v)),
            ),
            _ => format!("{n} providers updated"),
        };
        let description = match status {
            "failed" => if n == 1 {
                first
                    .update_state
                    .as_ref()
                    .unwrap()
                    .message
                    .clone()
                    .filter(|s| !s.is_empty())
            } else {
                None
            }
            .unwrap_or_else(|| {
                format!(
                    "{} failed to update. Check provider settings for details.",
                    names(&selected)
                )
            }),
            "unchanged" => format!(
                "{} {} outdated. Review provider settings for details.",
                names(&selected),
                if n == 1 {
                    "still appears"
                } else {
                    "still appear"
                }
            ),
            _ => format!(
                "New sessions will use the updated {}.",
                if n == 1 { "provider" } else { "providers" }
            ),
        };
        let mut keys: Vec<_> = selected
            .iter()
            .map(|p| {
                let s = p.update_state.as_ref().unwrap();
                format!(
                    "{}:{}:{}",
                    p.driver,
                    s.finished_at.as_deref().unwrap_or("pending"),
                    s.message.as_deref().unwrap_or_default()
                )
            })
            .collect();
        keys.sort();
        let key = format!("{status}:{}", keys.join("|"));
        if dismissed.contains(&key) {
            continue;
        }
        let latest = selected
            .iter()
            .filter_map(|p| p.update_state.as_ref().unwrap().finished_at.as_deref())
            .max()
            .unwrap_or_default();
        views.push((
            latest,
            Pill {
                key,
                tone,
                title,
                description,
                dismissible: status != "succeeded",
                dismiss_after_ms: (status == "succeeded").then_some(3000),
            },
        ));
    }
    views.sort_by(|a, b| b.0.cmp(a.0));
    views.into_iter().next().map(|(_, view)| view)
}
