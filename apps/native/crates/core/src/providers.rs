//! Provider instances and models as the composer offers them. Ports entries, readiness and default
//! models from `web/src/providerInstances.ts`, per-project enablement from `contracts/src/settings.ts`,
//! model options and ordering from `web/src/modelSelection.ts` and `web/src/modelOrdering.ts`,
//! display names from `client-runtime/src/state/providerInstanceDisplay.ts` and
//! `web/src/components/chat/providerIconUtils.ts`, and search ranking from
//! `web/src/components/chat/modelPickerSearch.ts`.

use crate::model::ModelSelection;
use crate::server_config::{ProviderModel, ServerConfig, ServerProvider, ServerSettings, UsageWindow};
use crate::sidebar::time::parse_iso;

/// Drivers whose instances are off until configured (`DEFAULT_DISABLED_PROVIDER_DRIVERS`).
const DEFAULT_DISABLED_DRIVERS: [&str; 7] = ["cursor", "grok", "fx", "muse", "pi", "opencode", "antigravity"];
/// Instances on without any config (`DEFAULT_ENABLED_DEFAULT_INSTANCES`).
const DEFAULT_ENABLED_INSTANCES: [&str; 2] = ["codex", "claudeAgent"];

/// `PROVIDER_DISPLAY_NAMES`.
pub fn driver_label(driver: &str) -> String {
    match driver {
        "antigravity" => "Antigravity".into(),
        "codex" => "Codex".into(),
        "claudeAgent" => "Claude".into(),
        "cursor" => "Cursor".into(),
        "grok" => "Grok".into(),
        "fx" => "fx".into(),
        "muse" => "Muse Code".into(),
        "acpRegistry" => "ACP Registry".into(),
        "pi" => "Pi".into(),
        "opencode" => "OpenCode".into(),
        other => humanize_slug(other),
    }
}

fn humanize_slug(slug: &str) -> String {
    let mut spaced = String::with_capacity(slug.len() + 4);
    let mut previous_lower = false;
    for ch in slug.chars() {
        if ch.is_ascii_uppercase() && previous_lower {
            spaced.push(' ');
        }
        previous_lower = ch.is_ascii_lowercase();
        spaced.push(if ch == '_' || ch == '-' { ' ' } else { ch });
    }
    spaced
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `resolveProviderInstanceDisplayName`.
pub fn display_name(provider: &ServerProvider) -> String {
    let kind = driver_label(&provider.driver);
    let named = provider.display_name.as_deref().map(str::trim).filter(|name| !name.is_empty());
    if let Some(name) = named.filter(|name| *name != kind) {
        return name.to_owned();
    }
    if provider.instance_id != provider.driver {
        let humanized = humanize_slug(&provider.instance_id);
        if !humanized.is_empty() {
            return humanized;
        }
    }
    named.map(str::to_owned).unwrap_or(kind)
}

/// `providerInstanceInitials`: the monogram drawn when a provider has no logo. Punctuation splits
/// words; apostrophes inside a word stay.
pub fn initials(label: &str) -> String {
    let spaced: String = label.chars().map(|ch| if ch.is_ascii_punctuation() && ch != '\'' { ' ' } else { ch }).collect();
    let words: Vec<&str> =
        spaced.split_whitespace().map(|word| word.trim_matches('\'')).filter(|word| !word.is_empty()).collect();
    match words.as_slice() {
        [] => String::new(),
        [word] => word.chars().take(2).collect::<String>().to_uppercase(),
        [first, second, ..] => [first, second].iter().filter_map(|word| word.chars().next()).collect::<String>().to_uppercase(),
    }
}

/// `normalizeProviderAccentColor`: a configured `#rrggbb`, as `0xrrggbbff`.
pub fn accent_color(provider: &ServerProvider) -> Option<u32> {
    let hex = provider.accent_color.as_deref()?.trim().strip_prefix('#')?;
    (hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(hex, 16).ok().map(|rgb| (rgb << 8) | 0xff))
        .flatten()
}

/// `providerAccentForegroundColor`: whether black reads better than white on the accent.
pub fn accent_wants_black(rgba: u32) -> bool {
    let channel = |shift: u32| {
        let value = ((rgba >> shift) & 0xff) as f64 / 255.0;
        if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    };
    let luminance = 0.2126 * channel(24) + 0.7152 * channel(16) + 0.0722 * channel(8);
    (luminance + 0.05) / 0.05 >= 1.05 / (luminance + 0.05)
}

/// `shouldShowInstanceBadge`: an accent, or another enabled instance of the same driver, earns the
/// instance a badge so the two can be told apart.
pub fn shows_instance_badge(entry: &ProviderEntry, entries: &[ProviderEntry]) -> bool {
    if accent_color(entry.provider).is_some() {
        return true;
    }
    entries
        .iter()
        .filter(|candidate| candidate.driver() == entry.driver())
        .filter(|candidate| !entry.enabled || candidate.enabled)
        .count()
        > 1
}

/// `resolveProjectProviderInstanceEnabled`.
pub fn instance_enabled(settings: &ServerSettings, project_id: Option<&str>, instance_id: &str) -> bool {
    let project_override = project_id
        .and_then(|id| settings.project_settings_overrides.get(id))
        .and_then(|overrides| overrides.provider_instances.get(instance_id))
        .and_then(|instance| instance.enabled);
    if let Some(enabled) = project_override {
        return enabled;
    }
    match settings.provider_instances.get(instance_id) {
        Some(instance) => {
            let config_enabled = instance.config.as_ref().and_then(|config| config.get("enabled")?.as_bool());
            if instance.enabled == Some(false) || config_enabled == Some(false) {
                return false;
            }
            instance.enabled.or(config_enabled).unwrap_or(!DEFAULT_DISABLED_DRIVERS.contains(&instance.driver.as_str()))
        }
        None => DEFAULT_ENABLED_INSTANCES.contains(&instance_id),
    }
}

/// `ProviderInstanceEntry` with project settings applied.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderEntry<'a> {
    pub provider: &'a ServerProvider,
    pub enabled: bool,
    pub display_name: String,
}

impl ProviderEntry<'_> {
    pub fn instance_id(&self) -> &str {
        &self.provider.instance_id
    }

    pub fn driver(&self) -> &str {
        &self.provider.driver
    }

    /// `isProviderInstancePickerReady`.
    pub fn is_ready(&self) -> bool {
        self.enabled && self.provider.is_available() && self.provider.status == "ready"
    }

    fn is_selectable(&self) -> bool {
        self.enabled && self.provider.is_available()
    }
}

/// `providerInstanceEntriesFor`: each driver's default instance first, then its custom ones,
/// drivers in the order the server first lists them.
pub fn entries<'a>(config: &'a ServerConfig, project_id: Option<&str>) -> Vec<ProviderEntry<'a>> {
    let mut drivers: Vec<&str> = Vec::new();
    for provider in &config.providers {
        if !drivers.contains(&provider.driver.as_str()) {
            drivers.push(&provider.driver);
        }
    }
    let mut sorted = Vec::with_capacity(config.providers.len());
    for driver in drivers {
        let bucket = config.providers.iter().filter(|provider| provider.driver == driver);
        let (defaults, customs): (Vec<_>, Vec<_>) = bucket.partition(|provider| provider.instance_id == provider.driver);
        sorted.extend(defaults.into_iter().chain(customs).map(|provider| ProviderEntry {
            provider,
            enabled: instance_enabled(&config.settings, project_id, &provider.instance_id),
            display_name: display_name(provider),
        }));
    }
    sorted
}

/// `resolveSelectableProviderInstanceEntry`.
pub fn selectable_entry<'e, 'a>(entries: &'e [ProviderEntry<'a>], instance_id: Option<&str>) -> Option<&'e ProviderEntry<'a>> {
    if let Some(requested) = instance_id.and_then(|id| entries.iter().find(|entry| entry.instance_id() == id))
        && requested.is_selectable()
    {
        return Some(requested);
    }
    entries
        .iter()
        .find(|entry| entry.is_ready())
        .or_else(|| entries.iter().find(|entry| entry.is_selectable() && entry.provider.status != "error"))
}

/// `getDefaultProviderInstanceModel`.
pub fn default_model(provider: &ServerProvider) -> Option<&str> {
    provider
        .models
        .iter()
        .find(|model| model.is_default && !model.is_custom)
        .or_else(|| provider.models.iter().find(|model| !model.is_custom))
        .or_else(|| provider.models.first())
        .map(|model| model.slug.as_str())
}

/// `resolveProjectDefaultProviderModelSelection`: the first driver group's best instance wins.
pub fn project_default_selection(entries: &[ProviderEntry]) -> Option<ModelSelection> {
    let first_driver = entries.first()?.driver();
    let group: Vec<ProviderEntry> = entries.iter().filter(|entry| entry.driver() == first_driver).cloned().collect();
    let entry = selectable_entry(&group, None).or_else(|| selectable_entry(entries, None))?;
    let model = default_model(entry.provider)?;
    Some(ModelSelection { instance_id: entry.instance_id().to_owned(), model: model.to_owned(), options: None })
}

/// `getAppModelOptionsForInstance` without custom models: hidden models drop out (custom ones
/// never hide) and the user's model order leads, then the server's order.
pub fn model_options<'a>(settings: &ServerSettings, provider: &'a ServerProvider) -> Vec<&'a ProviderModel> {
    let preferences = settings.provider_model_preferences.as_ref().and_then(|prefs| prefs.get(&provider.instance_id));
    let hidden = |model: &ProviderModel| preferences.is_some_and(|prefs| prefs.hidden_models.contains(&model.slug));
    let mut options: Vec<&ProviderModel> = provider.models.iter().filter(|model| model.is_custom || !hidden(model)).collect();
    if let Some(order) = preferences.map(|prefs| &prefs.model_order).filter(|order| !order.is_empty()) {
        let rank = |model: &ProviderModel| order.iter().position(|slug| *slug == model.slug).unwrap_or(usize::MAX);
        options.sort_by_key(|model| rank(model));
    }
    options
}

/// The favorites list: server settings once migrated, else none (the native client never held
/// the web's local favorites).
pub fn favorites(settings: &ServerSettings) -> &[crate::server_config::ModelFavorite] {
    settings.favorites.as_deref().unwrap_or(&[])
}

pub fn is_favorite(settings: &ServerSettings, instance_id: &str, slug: &str) -> bool {
    favorites(settings).iter().any(|favorite| favorite.provider == instance_id && favorite.model == slug)
}

fn family<'a>(id: &'a str, families: &[&'a str]) -> Option<&'a str> {
    families.iter().copied().find(|family| id.strip_prefix(family).is_some_and(|rest| rest.starts_with('-')))
}

/// `exhaustedUsageWindow`: a spent quota that has not reset, matched to the model's pool for
/// providers that bill models from separate pools.
pub fn exhausted_window<'a>(provider: &'a ServerProvider, now_ms: i64, model: &str) -> Option<&'a UsageWindow> {
    let windows = provider.usage_limits.as_ref().map(|limits| limits.windows.as_slice()).unwrap_or_default();
    let mut exhausted = windows.iter().filter(|window| {
        window.used_percent >= 100.0
            && window.resets_at.as_deref().and_then(parse_iso).is_none_or(|resets| resets > now_ms)
    });
    match provider.driver.as_str() {
        "cursor" => {
            let windows: Vec<&UsageWindow> = exhausted.collect();
            let api = windows.iter().copied().find(|window| window.id == "apiPercentUsed");
            let auto = windows.iter().copied().find(|window| window.id == "autoPercentUsed");
            let cursor_pool = ["auto", "default", "grok", "composer"].iter().any(|prefix| model.starts_with(prefix));
            if cursor_pool { auto.and(api) } else { api }
        }
        "antigravity" => {
            let model_family = family(model, &["gemini", "claude", "gpt"]);
            exhausted.find(|window| {
                let pool = family(&window.id, &["gemini", "claude", "gpt", "3p"]);
                match (pool, model_family) {
                    (None, _) | (_, None) => true,
                    (Some(pool), Some(model_family)) => {
                        pool == model_family || (pool == "3p" && matches!(model_family, "claude" | "gpt"))
                    }
                }
            })
        }
        "opencode" => exhausted.find(|window| !window.id.starts_with("go_") || model.starts_with("opencode-go/")),
        _ => exhausted.next(),
    }
}

/// `resolveNewThreadModelSelection`: a default whose quota is spent gives way to the configured
/// fallback, when the fallback can take work. Returns the selection and whether it fell back.
pub fn with_usage_fallback(
    config: &ServerConfig,
    project_id: Option<&str>,
    default: Option<ModelSelection>,
    now_ms: i64,
) -> (Option<ModelSelection>, bool) {
    let (Some(selection), Some(fallback)) = (default.as_ref(), config.settings.fallback_model_selection.as_ref()) else {
        return (default, false);
    };
    let exhausted = config.provider(&selection.instance_id).and_then(|provider| exhausted_window(provider, now_ms, &selection.model));
    let usable = config.provider(&fallback.instance_id).is_some_and(|provider| {
        provider.enabled
            && provider.installed
            && provider.auth.status != "unauthenticated"
            && provider.is_available()
            && exhausted_window(provider, now_ms, &fallback.model).is_none()
            && instance_enabled(&config.settings, project_id, &provider.instance_id)
    });
    if exhausted.is_some() && usable { (Some(fallback.clone()), true) } else { (default, false) }
}

fn strip_leading_qualifier<'a>(value: &'a str, qualifier: Option<&str>) -> &'a str {
    let Some(qualifier) = qualifier.map(str::trim).filter(|q| !q.is_empty()) else { return value };
    let Some(prefix) = value.get(..qualifier.len()).filter(|prefix| prefix.eq_ignore_ascii_case(qualifier)) else {
        return value;
    };
    let rest = &value[prefix.len()..];
    let trimmed = rest.trim_start();
    let separated = trimmed.len() < rest.len();
    let after = match trimmed.chars().next() {
        Some('.' | ':' | '/' | '-') => trimmed[1..].trim_start(),
        _ if separated => trimmed,
        _ => return value,
    };
    let after = after.trim();
    if after.is_empty() { value } else { after }
}

/// `getDisplayModelName`.
pub fn model_name(model: &ProviderModel) -> &str {
    strip_leading_qualifier(&model.name, model.sub_provider.as_deref())
}

/// `getTriggerDisplayModelName`: the short name when the provider gives one.
pub fn trigger_model_name(model: &ProviderModel) -> &str {
    strip_leading_qualifier(model.short_name.as_deref().unwrap_or(&model.name), model.sub_provider.as_deref())
}

fn normalize(value: &str) -> String {
    value.trim().to_lowercase()
}

/// `scoreQueryMatch` with every base given and `ignorePosition`.
fn score_field(value: &str, token: &str, base: i64) -> Option<i64> {
    if value.is_empty() || token.is_empty() {
        return None;
    }
    if value == token {
        return Some(base);
    }
    if value.starts_with(token) {
        return Some(base + 2);
    }
    if [" ", "-", "_", "/"].iter().any(|marker| value.contains(&format!("{marker}{token}"))) {
        return Some(base + 4);
    }
    if value.contains(token) {
        return Some(base + 6);
    }
    if token.chars().count() >= 3 {
        return subsequence_score(value, token).map(|score| base + 100 + score);
    }
    None
}

/// `scoreSubsequenceMatch` with `ignorePosition`.
fn subsequence_score(value: &str, query: &str) -> Option<i64> {
    let query: Vec<char> = query.chars().collect();
    let (mut index, mut first, mut previous, mut gaps) = (0usize, None::<usize>, None::<usize>, 0i64);
    for (position, ch) in value.chars().enumerate() {
        if ch != query[index] {
            continue;
        }
        first.get_or_insert(position);
        if let Some(previous) = previous {
            gaps += (position - previous - 1) as i64;
        }
        previous = Some(position);
        index += 1;
        if index == query.len() {
            let span = (position - first.unwrap_or(position) + 1 - query.len()) as i64;
            return Some(gaps * 3 + span);
        }
    }
    None
}

/// `scoreModelPickerSearch`: lower is better, `None` when a token matches no field.
pub fn search_score(model: &ProviderModel, entry: &ProviderEntry, favorite: bool, query: &str) -> Option<i64> {
    let normalized_query = normalize(query);
    let tokens: Vec<&str> = normalized_query.split_whitespace().collect();
    if tokens.is_empty() {
        return Some(0);
    }
    let collapse = |value: &str| normalize(value).split_whitespace().collect::<Vec<_>>().join(" ");
    let joined = tokens.join(" ");
    if joined == collapse(&model.name) || model.short_name.as_deref().is_some_and(|short| joined == collapse(short)) {
        return Some(-1000);
    }
    let mut fields = vec![normalize(&model.name)];
    fields.extend(model.short_name.as_deref().map(normalize));
    fields.extend(model.sub_provider.as_deref().map(normalize));
    fields.push(normalize(entry.driver()));
    fields.push(normalize(&entry.display_name));
    let all = [Some(model.name.as_str()), model.short_name.as_deref(), model.sub_provider.as_deref(), Some(entry.driver()), Some(&entry.display_name)];
    fields.push(normalize(&all.iter().flatten().copied().filter(|value| !value.is_empty()).collect::<Vec<_>>().join(" ")));
    let mut score = 0;
    for token in tokens {
        score += fields
            .iter()
            .enumerate()
            .filter_map(|(index, field)| score_field(field, token, index as i64 * 10))
            .min()?;
    }
    Some(if favorite { score - 24 } else { score })
}

#[cfg(test)]
mod tests;
