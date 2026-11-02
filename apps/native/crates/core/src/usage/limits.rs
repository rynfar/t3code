//! Subscription limits pooled by provider: `packages/shared/src/usageLimits.ts`. Logins to one
//! account merge, whichever environment or limit source reported them; each pool window then
//! averages its accounts and judges pace from the windows that have a reset clock.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::server_config::{ServerProvider, UsageLimitSource, UsageLimitSourceAccount, UsageLimits, UsageWindow, UsageWindowKind};
use crate::sidebar::time::{DAY_MS, HOUR_MS, MINUTE_MS, Ms, parse_iso};

mod report;

pub use report::{ReportAccount, UsageLimitsReport, collect_provider_usage_limits, has_provider_usage_limits};

pub const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/#settings/Usage";

/// One environment's limit reporters.
#[derive(Debug, Clone, Copy)]
pub struct LimitEnvironment<'a> {
    pub environment_id: &'a str,
    pub label: &'a str,
    pub providers: &'a [ServerProvider],
    pub sources: &'a [UsageLimitSource],
}

/// `UsageLimitsSection`: providers and source accounts the page hides drop out; a source whose
/// every account is hidden drops out too.
pub fn hide_drivers(providers: &[ServerProvider], sources: &[UsageLimitSource], hidden: &[&str]) -> (Vec<ServerProvider>, Vec<UsageLimitSource>) {
    let visible = |driver: &str| !hidden.contains(&driver);
    let providers = providers.iter().filter(|provider| visible(&provider.driver)).cloned().collect();
    let sources = sources
        .iter()
        .filter_map(|source| {
            let accounts: Vec<UsageLimitSourceAccount> = source.accounts.iter().filter(|account| visible(&account.driver)).cloned().collect();
            (!accounts.is_empty() || source.accounts.is_empty()).then(|| UsageLimitSource { accounts, ..source.clone() })
        })
        .collect();
    (providers, sources)
}

fn providers_with_limits(providers: &[ServerProvider]) -> impl Iterator<Item = &ServerProvider> {
    providers.iter().filter(|provider| provider.enabled && provider.installed && provider.is_available() && provider.usage_limits.is_some())
}

/// `usesChatGptSharing`.
pub fn uses_chatgpt_sharing(provider: &ServerProvider) -> bool {
    provider.auth.status == "authenticated" && provider.auth.subscription_sharing == Some(true)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalUsageLink {
    pub label: String,
    pub url: String,
    pub message: Option<String>,
    pub accounts: Vec<String>,
}

/// `collectExternalUsageLinks`.
pub fn collect_external_usage_links(environments: &[LimitEnvironment]) -> Vec<ExternalUsageLink> {
    let mut links: Vec<ExternalUsageLink> = Vec::new();
    for environment in environments {
        for provider in providers_with_limits(environment.providers) {
            let Some(limits) = &provider.usage_limits else { continue };
            let Some(external) = limits.external_usage.as_ref().filter(|_| provider.auth.status == "authenticated") else { continue };
            let account = format!("{} on {}", provider.display_name.clone().unwrap_or_else(|| provider.instance_id.clone()), environment.label);
            let message = limits.unavailable.as_ref().and_then(|unavailable| unavailable.message.clone());
            match links.iter_mut().find(|link| link.url == external.url) {
                Some(link) => {
                    link.label = external.label.clone();
                    link.message = message;
                    if !link.accounts.contains(&account) {
                        link.accounts.push(account);
                    }
                }
                None => links.push(ExternalUsageLink { label: external.label.clone(), url: external.url.clone(), message, accounts: vec![account] }),
            }
        }
    }
    links
}

fn account_key(driver: &str, email: Option<&str>, limits: Option<&UsageLimits>) -> Option<String> {
    if let Some(email) = email.map(|email| email.trim().to_lowercase()).filter(|email| !email.is_empty()) {
        return Some(format!("{driver}:{email}"));
    }
    limits.and_then(|limits| limits.credential_fingerprint.as_ref()).map(|fingerprint| format!("{driver}:credential:{fingerprint}"))
}

struct Report<'a> {
    driver: &'a str,
    email: Option<&'a str>,
    workspace_id: Option<&'a str>,
    limits: Option<&'a UsageLimits>,
}

fn native_report(provider: &ServerProvider) -> Report<'_> {
    Report {
        driver: &provider.driver,
        email: provider.auth.email.as_deref(),
        workspace_id: provider.auth.workspace_id.as_deref(),
        limits: provider.usage_limits.as_ref(),
    }
}

fn source_report(account: &UsageLimitSourceAccount) -> Report<'_> {
    Report { driver: &account.driver, email: account.email.as_deref(), workspace_id: account.workspace_id.as_deref(), limits: Some(&account.usage_limits) }
}

/// `accountKeyResolver`: one key per login, telling workspaces of one email apart.
struct KeyResolver {
    workspaces: HashMap<String, Vec<String>>,
}

impl KeyResolver {
    fn new<'a>(reports: impl IntoIterator<Item = Report<'a>>) -> Self {
        let mut workspaces: HashMap<String, Vec<String>> = HashMap::new();
        for report in reports {
            if let (Some(key), Some(workspace)) = (account_key(report.driver, report.email, report.limits), report.workspace_id) {
                let known = workspaces.entry(key).or_default();
                if !known.iter().any(|seen| seen == workspace) {
                    known.push(workspace.to_owned());
                }
            }
        }
        KeyResolver { workspaces }
    }

    fn resolve(&self, report: &Report) -> Option<String> {
        let key = account_key(report.driver, report.email, report.limits)?;
        if let Some(workspace) = report.workspace_id {
            return Some(format!("{key}:{workspace}"));
        }
        match self.workspaces.get(&key) {
            None => Some(key),
            Some(known) if known.len() == 1 => Some(format!("{key}:{}", known[0])),
            Some(_) => None,
        }
    }
}

fn resolver<'a>(environments: &[LimitEnvironment<'a>]) -> KeyResolver {
    KeyResolver::new(environments.iter().flat_map(|environment| {
        environment
            .providers
            .iter()
            .filter(|provider| provider.enabled)
            .map(native_report)
            .chain(environment.sources.iter().flat_map(|source| source.accounts.iter().map(source_report)))
    }))
}

/// What redeeming a reset credit sends to `provider.consumeResetCredit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetCreditInput {
    Instance { instance_id: String },
    Source { source_id: String, account_id: String, credit_id: String },
}

impl ResetCreditInput {
    pub fn payload(&self) -> Value {
        match self {
            ResetCreditInput::Instance { instance_id } => json!({ "instanceId": instance_id }),
            ResetCreditInput::Source { source_id, account_id, credit_id } => {
                json!({ "sourceId": source_id, "accountId": account_id, "creditId": credit_id })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redeem {
    pub environment_id: String,
    pub input: ResetCreditInput,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitAccount {
    pub key: String,
    pub driver: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub accent_color: Option<String>,
    /// Environments signed in to this account, as `(id, label)`.
    pub environments: Vec<(String, String)>,
    pub source_label: Option<String>,
    pub redeem: Option<Redeem>,
    pub limits: UsageLimits,
}

impl LimitAccount {
    /// Where the account was read: its environments, else the limit source.
    pub fn location(&self) -> Option<String> {
        if self.environments.is_empty() {
            self.source_label.clone()
        } else {
            Some(self.environments.iter().map(|(_, label)| label.as_str()).collect::<Vec<_>>().join(", "))
        }
    }

    pub fn reset_credit_count(&self) -> u64 {
        self.limits.reset_credits.as_ref().map_or(0, |credits| credits.available_count)
    }
}

fn checked(account: &LimitAccount) -> Option<Ms> {
    parse_iso(&account.limits.checked_at)
}

/// `Date.parse(a) > Date.parse(b)`, false when either is not a date.
fn fresher(next: &LimitAccount, previous: &LimitAccount) -> bool {
    matches!((checked(next), checked(previous)), (Some(next), Some(previous)) if next > previous)
}

#[derive(Default)]
struct AccountMerge {
    order: Vec<String>,
    accounts: HashMap<String, LimitAccount>,
    credit_sources: HashMap<String, LimitAccount>,
    hub_redeems: HashMap<String, LimitAccount>,
    plan_sources: HashMap<String, LimitAccount>,
}

impl AccountMerge {
    fn merge(&mut self, key: String, next: LimitAccount) {
        // Prefer hub redemption, which also clears its routing cooldown; balances follow the freshest read.
        let hub = next.redeem.as_ref().is_some_and(|redeem| matches!(redeem.input, ResetCreditInput::Source { .. }));
        if hub && self.hub_redeems.get(&key).is_none_or(|previous| fresher(&next, previous)) {
            self.hub_redeems.insert(key.clone(), next.clone());
        }
        if next.limits.reset_credits.is_some() && self.credit_sources.get(&key).is_none_or(|previous| fresher(&next, previous)) {
            self.credit_sources.insert(key.clone(), next.clone());
        }
        if next.plan.is_some() && self.plan_sources.get(&key).is_none_or(|previous| fresher(&next, previous)) {
            self.plan_sources.insert(key.clone(), next.clone());
        }
        let Some(previous) = self.accounts.get(&key) else {
            self.order.push(key.clone());
            self.accounts.insert(key, next);
            return;
        };
        let mut environments = previous.environments.clone();
        for environment in &next.environments {
            if !environments.iter().any(|(id, _)| *id == environment.0) {
                environments.push(environment.clone());
            }
        }
        let winner = if fresher(&next, previous) { &next } else { previous };
        let credit_source = self.credit_sources.get(&key);
        let redeem = match (self.hub_redeems.get(&key).and_then(|hub| hub.redeem.clone()), credit_source) {
            (Some(hub), _) => Some(hub),
            (None, Some(source)) => source.redeem.clone(),
            (None, None) => winner.redeem.clone().or_else(|| previous.redeem.clone()).or_else(|| next.redeem.clone()),
        };
        let merged = LimitAccount {
            key: previous.key.clone(),
            driver: previous.driver.clone(),
            display_name: previous.display_name.clone().or_else(|| next.display_name.clone()),
            email: previous.email.clone(),
            plan: self.plan_sources.get(&key).and_then(|source| source.plan.clone()).or_else(|| previous.plan.clone()).or_else(|| next.plan.clone()),
            accent_color: previous.accent_color.clone().or_else(|| next.accent_color.clone()),
            source_label: if environments.is_empty() { previous.source_label.clone().or_else(|| next.source_label.clone()) } else { None },
            environments,
            redeem,
            limits: UsageLimits { reset_credits: credit_source.and_then(|source| source.limits.reset_credits.clone()), ..winner.limits.clone() },
        };
        self.accounts.insert(key, merged);
    }

    fn finish(mut self) -> Vec<LimitAccount> {
        self.order.iter().filter_map(|key| self.accounts.remove(key)).collect()
    }
}

fn strip_json_suffix(id: &str) -> String {
    if id.len() >= 5 && id[id.len() - 5..].eq_ignore_ascii_case(".json") { id[..id.len() - 5].to_owned() } else { id.to_owned() }
}

/// `collectLimitAccounts`.
pub fn collect_limit_accounts(environments: &[LimitEnvironment]) -> Vec<LimitAccount> {
    let keys = resolver(environments);
    let mut merge = AccountMerge::default();
    for environment in environments {
        for provider in providers_with_limits(environment.providers) {
            let Some(limits) = provider.usage_limits.as_ref().filter(|limits| limits_notice(limits).is_none()) else { continue };
            let key = format!("{}:{}", environment.environment_id, provider.instance_id);
            merge.merge(
                keys.resolve(&native_report(provider)).unwrap_or_else(|| key.clone()),
                LimitAccount {
                    key,
                    driver: provider.driver.clone(),
                    display_name: provider.display_name.as_deref().map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned),
                    email: provider.auth.email.clone(),
                    plan: provider.auth.label.clone(),
                    accent_color: provider.accent_color.clone(),
                    environments: vec![(environment.environment_id.to_owned(), environment.label.to_owned())],
                    source_label: None,
                    redeem: Some(Redeem {
                        environment_id: environment.environment_id.to_owned(),
                        input: ResetCreditInput::Instance { instance_id: provider.instance_id.clone() },
                    }),
                    limits: limits.clone(),
                },
            );
        }
    }
    let label_environment = environments.len() > 1;
    for environment in environments {
        for source in environment.sources {
            let source_label = if label_environment { format!("{} · {}", environment.label, source.label) } else { source.label.clone() };
            for account in source.accounts.iter().filter(|account| limits_notice(&account.usage_limits).is_none()) {
                let key = match &account.workspace_id {
                    Some(workspace) => format!("{}:{}:{workspace}", source.id, account.id),
                    None => format!("{}:{}", source.id, account.id),
                };
                let redeem = account.usage_limits.reset_credits.as_ref().and_then(|credits| credits.next_credit_id.clone()).map(|credit_id| Redeem {
                    environment_id: environment.environment_id.to_owned(),
                    input: ResetCreditInput::Source { source_id: source.id.clone(), account_id: account.id.clone(), credit_id },
                });
                merge.merge(
                    keys.resolve(&source_report(account)).unwrap_or_else(|| key.clone()),
                    LimitAccount {
                        key,
                        driver: account.driver.clone(),
                        display_name: if account.email.is_some() { None } else { Some(strip_json_suffix(&account.id)) },
                        email: account.email.clone(),
                        plan: account.plan.clone(),
                        accent_color: None,
                        environments: Vec::new(),
                        source_label: Some(source_label.clone()),
                        redeem,
                        limits: account.usage_limits.clone(),
                    },
                );
            }
        }
    }
    merge.finish()
}

/// `collectLimitNotices`.
pub fn collect_limit_notices(environments: &[LimitEnvironment]) -> Vec<String> {
    let labelled = |environment: &str, subject: &str| if environments.len() > 1 { format!("{environment} · {subject}") } else { subject.to_owned() };
    let mut notices = Vec::new();
    for environment in environments {
        for provider in providers_with_limits(environment.providers) {
            let Some(limits) = &provider.usage_limits else { continue };
            if limits.unavailable.as_ref().is_some_and(|unavailable| unavailable.reason == "unsupported") {
                continue;
            }
            if let Some(notice) = limits_notice(limits) {
                let name = provider.display_name.as_deref().map(str::trim).filter(|name| !name.is_empty()).unwrap_or(&provider.driver);
                notices.push(format!("{}: {notice}", labelled(environment.label, name)));
            }
        }
        for source in environment.sources {
            if let Some(error) = &source.error {
                notices.push(format!("{}: {error}", labelled(environment.label, &source.label)));
            } else if source.accounts.is_empty() {
                notices.push(format!("{}: No accounts reported.", labelled(environment.label, &source.label)));
            }
        }
    }
    notices
}

/// `limitsNotice`: why an account shows no bars, if it shows none.
pub fn limits_notice(limits: &UsageLimits) -> Option<String> {
    match limits.unavailable.as_ref().map(|unavailable| (unavailable.reason.as_str(), unavailable.message.clone())) {
        Some(("unsupported", message)) => Some(message.unwrap_or_else(|| "This account has no subscription limits.".to_owned())),
        Some(("probeFailed", message)) => Some(message.unwrap_or_else(|| "Could not read limits.".to_owned())),
        _ => limits.windows.is_empty().then(|| "No limits reported.".to_owned()),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitPoolMember {
    pub account: Arc<LimitAccount>,
    pub window: UsageWindow,
    pub refill_percent: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PoolReset {
    pub account_key: String,
    pub at: Ms,
    pub restores_percent: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitPoolWindow {
    pub id: String,
    pub kind: UsageWindowKind,
    pub label: String,
    pub members: Vec<LimitPoolMember>,
    /// Every account in pool order; `None` where an account lacks this window.
    pub columns: Vec<(Arc<LimitAccount>, Option<LimitPoolMember>)>,
    pub remaining_percent: i64,
    pub used_percent: i64,
    pub pace: Option<LimitPace>,
    pub resets: Vec<PoolReset>,
}

impl LimitPoolWindow {
    pub fn reset_for(&self, account_key: &str) -> Option<&PoolReset> {
        self.resets.iter().find(|reset| reset.account_key == account_key)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitPool {
    pub driver: String,
    pub accounts: Vec<Arc<LimitAccount>>,
    pub windows: Vec<LimitPoolWindow>,
}

fn kind_order(kind: UsageWindowKind) -> u8 {
    match kind {
        UsageWindowKind::Session => 0,
        UsageWindowKind::Weekly => 1,
        UsageWindowKind::Monthly => 2,
        UsageWindowKind::Other => 3,
    }
}

fn kind_name(kind: UsageWindowKind) -> &'static str {
    match kind {
        UsageWindowKind::Session => "session",
        UsageWindowKind::Weekly => "weekly",
        UsageWindowKind::Monthly => "monthly",
        UsageWindowKind::Other => "other",
    }
}

/// Codex ids name slots, so it pools by kind; other drivers' ids name distinct allowances.
fn pool_key(driver: &str, window: &UsageWindow) -> String {
    if driver == "codex" { kind_name(window.kind).to_owned() } else { format!("{}:{}", kind_name(window.kind), window.id) }
}

pub struct CursorWindow {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

pub const CURSOR_USAGE_WINDOWS: [CursorWindow; 3] = [
    CursorWindow { id: "totalPercentUsed", label: "Overall", description: "Combined usage across both allowances, not a third quota." },
    CursorWindow { id: "autoPercentUsed", label: "Cursor Models", description: "Grok and Composer use this first. Auto can use either pool." },
    CursorWindow {
        id: "apiPercentUsed",
        label: "Other Models",
        description: "Claude, GPT, and Gemini use this pool. Grok and Composer fall back here.",
    },
];

pub fn cursor_usage_window_details(id: &str) -> Option<&'static CursorWindow> {
    CURSOR_USAGE_WINDOWS.iter().find(|window| window.id == id)
}

fn cursor_rank(id: &str) -> usize {
    CURSOR_USAGE_WINDOWS.iter().position(|window| window.id == id).unwrap_or(CURSOR_USAGE_WINDOWS.len())
}

fn antigravity_base(id: &str) -> &str {
    ["-5h", "-weekly", "-quota"].iter().find_map(|suffix| id.strip_suffix(suffix)).unwrap_or(id)
}

/// `displayLimitWindows`: Antigravity lists Gemini first by family; Cursor drops Overall when
/// both of its pools report.
pub fn display_limit_windows(pool: &LimitPool) -> Vec<&LimitPoolWindow> {
    let mut windows: Vec<&LimitPoolWindow> = pool.windows.iter().collect();
    match pool.driver.as_str() {
        "antigravity" => windows.sort_by(|left, right| {
            (!left.id.starts_with("gemini-"))
                .cmp(&!right.id.starts_with("gemini-"))
                .then_with(|| antigravity_base(&left.id).cmp(antigravity_base(&right.id)))
                .then_with(|| kind_order(left.kind).cmp(&kind_order(right.kind)))
        }),
        "cursor" => {
            let both = ["autoPercentUsed", "apiPercentUsed"].iter().all(|id| windows.iter().any(|window| window.id == *id));
            windows.retain(|window| !both || window.id != "totalPercentUsed");
            windows.sort_by_key(|window| cursor_rank(&window.id));
        }
        _ => {}
    }
    windows
}

fn reset_millis(window: &UsageWindow) -> Option<Ms> {
    window.resets_at.as_deref().and_then(parse_iso)
}

fn js_round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

/// A session window under a spent weekly quota cannot be used until the weekly resets.
fn pool_member(account: &Arc<LimitAccount>, reported: &UsageWindow, now: Ms) -> LimitPoolMember {
    let spent_weekly = (reported.kind == UsageWindowKind::Session)
        .then(|| {
            account.limits.windows.iter().find(|window| {
                window.used_percent >= 100.
                    && match account.driver.as_str() {
                        "claudeAgent" => window.id == "seven_day",
                        "codex" => window.kind == UsageWindowKind::Weekly,
                        _ => false,
                    }
            })
        })
        .flatten();
    let Some(spent_weekly) = spent_weekly else {
        return LimitPoolMember { account: account.clone(), window: reported.clone(), refill_percent: reported.used_percent };
    };
    let cutoff = now.max(parse_iso(&account.limits.checked_at).unwrap_or(Ms::MIN));
    let (mut resets_at, mut refill_percent) = (None, 0.);
    // A stale or missing clock cannot tell when blocked quota becomes usable.
    if let (Some(weekly), Some(session)) = (reset_millis(spent_weekly), reset_millis(reported))
        && weekly > cutoff
        && session > cutoff
    {
        if session <= weekly {
            (resets_at, refill_percent) = (spent_weekly.resets_at.clone(), 100.);
        } else if reported.used_percent >= 100. {
            (resets_at, refill_percent) = (reported.resets_at.clone(), 100.);
        } else {
            (resets_at, refill_percent) = (spent_weekly.resets_at.clone(), 100. - reported.used_percent.clamp(0., 100.));
        }
    }
    LimitPoolMember {
        account: account.clone(),
        // Suppression is not session spending, so it has no session pace clock.
        window: UsageWindow { used_percent: 100., resets_at, window_duration_mins: None, ..reported.clone() },
        refill_percent,
    }
}

fn pool_windows(accounts: &[Arc<LimitAccount>], now: Ms) -> Vec<LimitPoolWindow> {
    let mut keys: Vec<String> = Vec::new();
    let mut by_key: HashMap<String, Vec<LimitPoolMember>> = HashMap::new();
    for account in accounts {
        for reported in &account.limits.windows {
            let member = pool_member(account, reported, now);
            let pooled = pool_key(&account.driver, &member.window);
            let taken = by_key.get(&pooled).is_some_and(|members| members.iter().any(|existing| existing.account.key == account.key));
            let key = if taken { format!("{}:{}", kind_name(member.window.kind), member.window.id) } else { pooled };
            if !by_key.contains_key(&key) {
                keys.push(key.clone());
            }
            by_key.entry(key).or_default().push(member);
        }
    }
    let mut pools: Vec<LimitPoolWindow> = keys
        .into_iter()
        .filter_map(|key| {
            let members = by_key.remove(&key)?;
            let first = members.first()?.window.clone();
            let count = members.len() as f64;
            let id = if members[0].account.driver == "codex" { key } else { first.id.clone() };
            let used = members.iter().map(|member| member.window.used_percent).sum::<f64>() / count;
            // Judge pace only for windows with reset clocks; others add spend without elapsed time.
            let timed: Vec<(f64, f64)> =
                members.iter().filter_map(|member| elapsed_share(&member.window, now).map(|share| (member.window.used_percent, share))).collect();
            let pace = (!timed.is_empty()).then(|| {
                let used = timed.iter().map(|(used, _)| used).sum::<f64>() / timed.len() as f64;
                let elapsed = timed.iter().map(|(_, elapsed)| elapsed).sum::<f64>() / timed.len() as f64;
                pace_of_shares(used, elapsed)
            });
            let mut resets: Vec<PoolReset> = members
                .iter()
                .filter_map(|member| {
                    reset_millis(&member.window).map(|at| PoolReset {
                        account_key: member.account.key.clone(),
                        at,
                        restores_percent: js_round(member.refill_percent / count),
                    })
                })
                .collect();
            resets.sort_by_key(|reset| reset.at);
            let columns = accounts
                .iter()
                .map(|account| (account.clone(), members.iter().rev().find(|member| member.account.key == account.key).cloned()))
                .collect();
            Some(LimitPoolWindow {
                id,
                kind: first.kind,
                label: first.label.clone(),
                members,
                columns,
                used_percent: js_round(used),
                remaining_percent: js_round(100. - used),
                pace,
                resets,
            })
        })
        .collect();
    pools.sort_by_key(|pool| kind_order(pool.kind));
    pools
}

fn sort_name(account: &LimitAccount) -> String {
    account.display_name.as_ref().or(account.email.as_ref()).unwrap_or(&account.key).to_lowercase()
}

/// `collectLimitPools`: accounts grouped by driver, ordered by when their shortest window resets.
pub fn collect_limit_pools(accounts: Vec<LimitAccount>, now: Ms) -> Vec<LimitPool> {
    let mut drivers: Vec<String> = Vec::new();
    let mut by_driver: HashMap<String, Vec<Arc<LimitAccount>>> = HashMap::new();
    for account in accounts {
        if !by_driver.contains_key(&account.driver) {
            drivers.push(account.driver.clone());
        }
        by_driver.entry(account.driver.clone()).or_default().push(Arc::new(account));
    }
    drivers
        .into_iter()
        .filter_map(|driver| {
            let mut members = by_driver.remove(&driver)?;
            let order_window =
                members.iter().flat_map(|account| account.limits.windows.iter()).min_by_key(|window| kind_order(window.kind)).cloned();
            let order_reset = |account: &Arc<LimitAccount>| -> f64 {
                let Some(order) = &order_window else { return f64::INFINITY };
                let order_key = pool_key(&driver, order);
                account
                    .limits
                    .windows
                    .iter()
                    .find(|window| pool_key(&driver, window) == order_key)
                    .and_then(|window| reset_millis(&pool_member(account, window, now).window))
                    .map_or(f64::INFINITY, |at| at as f64)
            };
            members.sort_by(|left, right| {
                order_reset(left)
                    .total_cmp(&order_reset(right))
                    .then_with(|| sort_name(left).cmp(&sort_name(right)))
                    .then_with(|| left.key.cmp(&right.key))
            });
            let windows = pool_windows(&members, now);
            Some(LimitPool { driver, accounts: members, windows })
        })
        .collect()
}

/// `remainingPercent`.
pub fn remaining_percent(window: &UsageWindow) -> i64 {
    js_round(100. - window.used_percent.clamp(0., 100.))
}

/// `elapsedShare`: how much of a timed window has passed.
pub fn elapsed_share(window: &UsageWindow, now: Ms) -> Option<f64> {
    let resets_at = reset_millis(window)?;
    let length = window.window_duration_mins? as f64 * MINUTE_MS as f64;
    (length > 0.).then(|| ((length - (resets_at - now) as f64) / length).clamp(0., 1.))
}

/// `paceRemainingPercent`: bars show quota left, so even pace tracks time left.
pub fn pace_remaining_percent(window: &UsageWindow, now: Ms) -> Option<i64> {
    elapsed_share(window, now).map(|elapsed| js_round((1. - elapsed) * 100.))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitPace {
    Ahead,
    On,
    Under,
}

impl LimitPace {
    pub fn label(self) -> &'static str {
        match self {
            LimitPace::Ahead => "Ahead of pace: spending faster than the window elapses",
            LimitPace::On => "On pace with the window",
            LimitPace::Under => "Under pace: headroom left for the rest of the window",
        }
    }
}

pub fn pace_of(window: &UsageWindow, now: Ms) -> Option<LimitPace> {
    elapsed_share(window, now).map(|elapsed| pace_of_shares(window.used_percent, elapsed))
}

fn pace_of_shares(used_percent: f64, elapsed: f64) -> LimitPace {
    let gap = used_percent - elapsed * 100.;
    if gap > 5. {
        LimitPace::Ahead
    } else if gap < -5. {
        LimitPace::Under
    } else {
        LimitPace::On
    }
}

/// `formatDuration`: `2d 3h`, `3h 20m`, `20m`.
pub fn format_duration(ms: Ms) -> String {
    let remaining = ms.max(0);
    let (days, hours, minutes) = (remaining / DAY_MS, remaining % DAY_MS / HOUR_MS, remaining % HOUR_MS / MINUTE_MS);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

/// `formatResetsIn`.
pub fn format_resets_in(window: &UsageWindow, now: Ms) -> Option<String> {
    let resets_at = reset_millis(window)?;
    Some(if resets_at <= now {
        "resets now".to_owned()
    } else if resets_at - now < MINUTE_MS {
        "Resets momentarily".to_owned()
    } else {
        format!("resets in {}", format_duration(resets_at - now))
    })
}

/// `resetCreditsSummary`.
pub fn reset_credits_summary(available: u64, next_expires_at: Option<&str>, now: Ms, compact: bool) -> String {
    let expires_in = next_expires_at.and_then(parse_iso).map(|at| format_duration(at - now));
    if available == 0 {
        return "No reset credits banked".to_owned();
    }
    if compact {
        return format!("{available} banked{}", expires_in.map(|time| format!(" · expires in {time}")).unwrap_or_default());
    }
    let noun = if available == 1 { "reset credit" } else { "reset credits" };
    format!("{available} {noun} banked{}", expires_in.map(|time| format!(" · next expires in {time}")).unwrap_or_default())
}

/// `OUTCOME_TEXT` for `provider.consumeResetCredit`.
pub fn reset_outcome_text(reply: &Value) -> String {
    if let Some(warning) = reply.get("warning").and_then(Value::as_str) {
        return warning.to_owned();
    }
    match reply.get("outcome").and_then(Value::as_str) {
        Some("reset") => "Reset applied. Your windows have cleared.",
        Some("nothingToReset") => "Nothing to reset right now.",
        Some("noCredit") => "No reset credit left.",
        Some("alreadyRedeemed") => "That credit was already redeemed.",
        _ => "Could not use the reset credit.",
    }
    .to_owned()
}

/// `normalizeProviderAccentColor`: a configured `#rrggbb` accent as `0xrrggbbff`.
pub fn accent_rgba(accent: Option<&str>) -> Option<u32> {
    let hex = accent?.trim().strip_prefix('#')?;
    (hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| u32::from_str_radix(hex, 16).ok().map(|rgb| (rgb << 8) | 0xff)).flatten()
}

/// The name an account shows: its display name, else its driver's label.
pub fn account_title(account: &LimitAccount) -> String {
    account.display_name.clone().unwrap_or_else(|| crate::providers::driver_label(&account.driver))
}

/// A segment's accessible summary, as its button announces it.
pub fn segment_summary(account: &LimitAccount, window: &UsageWindow, now: Ms) -> String {
    let name = account.display_name.clone().or_else(|| account.email.as_deref().map(account_initials)).unwrap_or_else(|| account.driver.clone());
    let mut summary = format!("{name}: {}% left", remaining_percent(window));
    if let Some(resets) = format_resets_in(window, now) {
        summary.push_str(&format!(", {resets}"));
    }
    if let Some(time_left) = pace_remaining_percent(window, now) {
        summary.push_str(&format!(", {time_left}% of the window left"));
    }
    let credits = account.reset_credit_count();
    if credits > 0 {
        summary.push_str(&format!(", {credits} reset {} banked", if credits == 1 { "credit" } else { "credits" }));
    }
    summary
}

/// `accountInitials`.
pub fn account_initials(email: &str) -> String {
    let (local, domain) = email.split_once('@').unwrap_or((email, ""));
    let initials: String = local.chars().take(1).chain(domain.chars().take(1)).collect::<String>().to_uppercase();
    if initials.is_empty() { "?".to_owned() } else { initials }
}

/// `accountHue`: a stable hue per email for its chip.
pub fn account_hue(email: &str) -> u32 {
    let hash = email.encode_utf16().fold(0i32, |hash, unit| hash.wrapping_mul(31).wrapping_add(i32::from(unit)));
    (i64::from(hash).unsigned_abs() % 360) as u32
}
