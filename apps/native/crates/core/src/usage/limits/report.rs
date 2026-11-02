//! The `/usage-limits` report for the composer's provider: `collectProviderUsageLimits` and
//! `hasProviderUsageLimits` in `packages/shared/src/usageLimits.ts`. Native logins merge by
//! account, preferring a usable and fresher read; limit-source accounts fill in the rest.

use std::collections::HashMap;

use super::{KeyResolver, ResetCreditInput, limits_notice, native_report, providers_with_limits, source_report};
use crate::server_config::{ServerProvider, UsageLimitSource, UsageLimitSourceAccount, UsageLimits};
use crate::sidebar::time::{Ms, format_iso, parse_iso};

/// `hasProviderUsageLimits`: whether `/usage-limits` has anything to show for a driver.
pub fn has_provider_usage_limits(driver: &str, providers: &[ServerProvider], sources: &[UsageLimitSource]) -> bool {
    providers_with_limits(providers).any(|provider| provider.driver == driver)
        || sources.iter().any(|source| source.accounts.iter().any(|account| account.driver == driver) || (source.error.is_some() && source.accounts.is_empty()))
}

/// One account in a `/usage-limits` report (`UsageLimitsReport`).
#[derive(Debug, Clone, PartialEq)]
pub struct ReportAccount {
    pub id: String,
    pub driver: String,
    pub label: String,
    pub plan: Option<String>,
    pub email: Option<String>,
    pub source_label: Option<String>,
    pub instance_id: Option<String>,
    pub reset_credit_input: Option<ResetCreditInput>,
    pub display_name: Option<String>,
    pub accent_color: Option<String>,
    pub limits: UsageLimits,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageLimitsReport {
    pub created_at: String,
    pub accounts: Vec<ReportAccount>,
    pub notices: Vec<String>,
}

struct NativeSnapshot<'a> {
    usage: &'a ServerProvider,
    credits: Option<&'a ServerProvider>,
}

fn provider_checked(provider: &ServerProvider) -> Option<Ms> {
    provider.usage_limits.as_ref().and_then(|limits| parse_iso(&limits.checked_at))
}

fn later(next: Option<Ms>, previous: Option<Ms>) -> bool {
    matches!((next, previous), (Some(next), Some(previous)) if next > previous)
}

/// `collectProviderUsageLimits`: the `/usage-limits` report for the composer's provider.
pub fn collect_provider_usage_limits(instance_id: &str, providers: &[ServerProvider], sources: &[UsageLimitSource], now: Ms) -> Option<UsageLimitsReport> {
    let selected = providers.iter().find(|provider| provider.instance_id == instance_id)?;
    if !has_provider_usage_limits(&selected.driver, providers, sources) {
        return None;
    }
    let keys = KeyResolver::new(
        providers.iter().filter(|provider| provider.enabled).map(native_report).chain(sources.iter().flat_map(|source| source.accounts.iter().map(source_report))),
    );
    let mut order: Vec<String> = Vec::new();
    let mut native: HashMap<String, NativeSnapshot> = HashMap::new();
    for provider in providers_with_limits(providers).filter(|provider| provider.driver == selected.driver) {
        let Some(limits) = &provider.usage_limits else { continue };
        let key = keys.resolve(&native_report(provider)).unwrap_or_else(|| format!("instance:{}", provider.instance_id));
        let usable = limits_notice(limits).is_none();
        let snapshot = match native.get(&key) {
            None => {
                order.push(key.clone());
                NativeSnapshot { usage: provider, credits: limits.reset_credits.as_ref().map(|_| provider) }
            }
            Some(previous) => {
                let previous_usable = previous.usage.usage_limits.as_ref().is_some_and(|limits| limits_notice(limits).is_none());
                let wins = (usable && !previous_usable) || (usable == previous_usable && later(provider_checked(provider), provider_checked(previous.usage)));
                let credits = if limits.reset_credits.is_some() && previous.credits.is_none_or(|credits| later(provider_checked(provider), provider_checked(credits))) {
                    Some(provider)
                } else {
                    previous.credits
                };
                NativeSnapshot { usage: if wins { provider } else { previous.usage }, credits }
            }
        };
        native.insert(key, snapshot);
    }
    let snapshots: Vec<&NativeSnapshot> = order.iter().filter_map(|key| native.get(key)).collect();
    let native_keys: Vec<String> = snapshots
        .iter()
        .filter(|snapshot| snapshot.usage.usage_limits.as_ref().is_some_and(|limits| !limits.windows.is_empty() && limits.unavailable.is_none()))
        .filter_map(|snapshot| keys.resolve(&native_report(snapshot.usage)))
        .collect();
    let mut accounts = Vec::new();
    for snapshot in &snapshots {
        let provider = snapshot.usage;
        let Some(limits) = &provider.usage_limits else { continue };
        let key = keys.resolve(&native_report(provider));
        let hub = sources
            .iter()
            .flat_map(|source| source.accounts.iter().map(move |account| (source, account)))
            .filter(|(_, account)| {
                key.is_some() && keys.resolve(&source_report(account)) == key && account.usage_limits.reset_credits.is_some() && limits_notice(&account.usage_limits).is_none()
            })
            .fold(None, |best: Option<(&UsageLimitSource, &UsageLimitSourceAccount)>, candidate| match best {
                Some((_, kept)) if !later(parse_iso(&candidate.1.usage_limits.checked_at), parse_iso(&kept.usage_limits.checked_at)) => best,
                _ => Some(candidate),
            });
        let credits_checked = snapshot.credits.and_then(provider_checked);
        let show_hub = hub.is_some_and(|(_, account)| snapshot.credits.is_none() || later(parse_iso(&account.usage_limits.checked_at), credits_checked));
        let shown_credits = if show_hub {
            hub.and_then(|(_, account)| account.usage_limits.reset_credits.clone())
        } else {
            snapshot.credits.and_then(|credits| credits.usage_limits.as_ref()).and_then(|limits| limits.reset_credits.clone())
        };
        let hub_input = hub.and_then(|(source, account)| {
            account.usage_limits.reset_credits.as_ref()?.next_credit_id.clone().map(|credit_id| ResetCreditInput::Source {
                source_id: source.id.clone(),
                account_id: account.id.clone(),
                credit_id,
            })
        });
        let name = provider.display_name.as_deref().map(str::trim).filter(|name| !name.is_empty()).unwrap_or(&provider.driver);
        accounts.push(ReportAccount {
            id: provider.instance_id.clone(),
            driver: provider.driver.clone(),
            label: format!("{name} [{}]", provider.instance_id),
            plan: provider.auth.label.clone(),
            email: provider.auth.email.clone(),
            source_label: None,
            instance_id: Some(provider.instance_id.clone()),
            reset_credit_input: Some(hub_input.unwrap_or_else(|| ResetCreditInput::Instance {
                instance_id: snapshot.credits.map_or_else(|| provider.instance_id.clone(), |credits| credits.instance_id.clone()),
            })),
            display_name: provider.display_name.clone(),
            accent_color: provider.accent_color.clone(),
            limits: match shown_credits {
                Some(credits) => UsageLimits { reset_credits: Some(credits), ..limits.clone() },
                None => limits.clone(),
            },
        });
    }
    let mut notices = Vec::new();
    for source in sources {
        let matching: Vec<&UsageLimitSourceAccount> = source.accounts.iter().filter(|account| account.driver == selected.driver).collect();
        for account in &matching {
            if keys.resolve(&source_report(account)).is_some_and(|key| native_keys.contains(&key)) {
                continue;
            }
            accounts.push(ReportAccount {
                id: format!("{}:{}", source.id, account.id),
                driver: account.driver.clone(),
                label: format!("{} · {}", source.label, account.id),
                plan: account.plan.clone(),
                email: account.email.clone(),
                source_label: Some("CLI Proxy".to_owned()),
                instance_id: None,
                reset_credit_input: account.usage_limits.reset_credits.as_ref().and_then(|credits| credits.next_credit_id.clone()).map(|credit_id| {
                    ResetCreditInput::Source { source_id: source.id.clone(), account_id: account.id.clone(), credit_id }
                }),
                display_name: None,
                accent_color: None,
                limits: account.usage_limits.clone(),
            });
        }
        if let Some(error) = &source.error
            && (!matching.is_empty() || source.accounts.is_empty())
        {
            notices.push(format!("{}: {error}", source.label));
        }
    }
    Some(UsageLimitsReport { created_at: format_iso(now), accounts, notices })
}
