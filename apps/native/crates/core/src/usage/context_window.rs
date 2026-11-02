//! The composer's context window meter: `web/src/lib/contextWindow.ts` and
//! `web/src/components/chat/ContextWindowMeter.logic.ts`. The newest provider turn's token usage
//! wins, then the active provider thread's reported context, then the last compaction. A fork's
//! inherited usage before its first turn (`contextTransfers`) is not read: the thread projection
//! does not decode `forkedFrom`.

use serde::Deserialize;
use serde_json::Value;

use crate::sidebar::time::parse_iso;
use crate::thread::{Entity, ThreadProjection};
use crate::turn_item::TurnItemKind;

/// `OrchestrationV2ProviderTurnTokenUsage`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnTokenUsage {
    used_tokens: f64,
    #[serde(default)]
    max_tokens: Option<f64>,
    #[serde(default)]
    auto_compact_threshold: Option<f64>,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct UsageCost {
    amount: f64,
    currency: String,
}

/// `ThreadTokenUsageSnapshot`, as a provider thread's `contextUsage`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContextUsage {
    used_tokens: f64,
    #[serde(default)]
    total_processed_tokens: Option<f64>,
    #[serde(default)]
    max_tokens: Option<f64>,
    #[serde(default)]
    compacts_automatically: Option<bool>,
    #[serde(default)]
    auto_compact_threshold: Option<f64>,
    #[serde(default)]
    cost: Option<UsageCost>,
}

/// `ContextWindowSnapshot`: the fields the meter shows.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextWindowSnapshot {
    pub used_tokens: f64,
    pub total_processed_tokens: Option<f64>,
    pub max_tokens: Option<f64>,
    pub used_percentage: Option<f64>,
    pub compacts_automatically: Option<bool>,
    pub auto_compact_threshold: Option<f64>,
    /// `(amount, currency)`.
    pub cost: Option<(f64, String)>,
    pub updated_at: String,
}

fn used_percentage(used: f64, max: Option<f64>) -> Option<f64> {
    max.filter(|max| *max > 0.).map(|max| (used / max * 100.).min(100.))
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite())
}

/// `latestProviderTurnTokenUsage`: the newest report by time; ties go to the later entry.
fn latest_turn_usage(turns: &[Entity]) -> Option<TurnTokenUsage> {
    let mut latest: Option<(TurnTokenUsage, Option<i64>)> = None;
    for turn in turns {
        let Some(usage) = turn.value.get("tokenUsage").filter(|usage| !usage.is_null()) else { continue };
        let Ok(usage) = serde_json::from_value::<TurnTokenUsage>(usage.clone()) else { continue };
        let at = parse_iso(&usage.updated_at);
        let newer = match &latest {
            None => true,
            Some((_, previous)) => matches!((at, previous), (Some(at), Some(previous)) if at >= *previous),
        };
        if newer {
            latest = Some((usage, at));
        }
    }
    latest.map(|(usage, _)| usage)
}

fn active_provider_thread(projection: &ThreadProjection) -> Option<&Value> {
    let id = projection.thread.active_provider_thread_id.as_deref()?;
    projection.provider_threads.iter().find(|thread| thread.id == id).map(|thread| &thread.value)
}

/// `deriveLatestContextWindowSnapshot` over the open thread.
pub fn context_window_snapshot(projection: &ThreadProjection) -> Option<ContextWindowSnapshot> {
    if let Some(live) = latest_turn_usage(&projection.provider_turns) {
        let used = live.used_tokens.max(0.);
        return Some(ContextWindowSnapshot {
            used_tokens: used,
            total_processed_tokens: None,
            max_tokens: live.max_tokens,
            used_percentage: used_percentage(used, live.max_tokens),
            compacts_automatically: Some(true),
            auto_compact_threshold: live.auto_compact_threshold,
            cost: None,
            updated_at: live.updated_at,
        });
    }
    let provider_thread = active_provider_thread(projection);
    let provider_usage = provider_thread
        .and_then(|thread| thread.get("contextUsage"))
        .filter(|usage| !usage.is_null())
        .and_then(|usage| serde_json::from_value::<ContextUsage>(usage.clone()).ok());
    let updated_at = provider_thread.and_then(|thread| thread.get("updatedAt")).and_then(Value::as_str);
    if let (Some(usage), Some(updated_at)) = (provider_usage, updated_at) {
        let max = finite(usage.max_tokens);
        return Some(ContextWindowSnapshot {
            used_tokens: usage.used_tokens,
            total_processed_tokens: finite(usage.total_processed_tokens),
            max_tokens: max,
            used_percentage: used_percentage(usage.used_tokens, max),
            compacts_automatically: usage.compacts_automatically,
            auto_compact_threshold: usage.auto_compact_threshold,
            cost: usage.cost.map(|cost| (cost.amount, cost.currency)),
            updated_at: updated_at.to_owned(),
        });
    }
    projection.visible_turn_items.iter().rev().find_map(|row| {
        let TurnItemKind::Compaction { before_token_count, after_token_count, .. } = &row.item.kind else { return None };
        let used = (*after_token_count)? as f64;
        Some(ContextWindowSnapshot {
            used_tokens: used,
            total_processed_tokens: before_token_count.map(|count| count as f64),
            max_tokens: None,
            used_percentage: None,
            compacts_automatically: Some(true),
            auto_compact_threshold: None,
            cost: None,
            updated_at: row.item.started_at.clone().unwrap_or_else(|| row.item.updated_at.clone()),
        })
    })
}

/// `formatContextWindowTokens`: `999`, `1.5k`, `12k`, `1.2m`.
pub fn format_context_tokens(value: Option<f64>) -> String {
    let Some(value) = value.filter(|value| value.is_finite()) else { return "0".to_owned() };
    let strip = |text: String| text.strip_suffix(".0").map(str::to_owned).unwrap_or(text);
    if value < 1_000. {
        format!("{}", (value + 0.5).floor())
    } else if value < 10_000. {
        format!("{}k", strip(super::format::to_fixed(value / 1_000., 1)))
    } else if value < 999_500. {
        format!("{}k", ((value / 1_000.) + 0.5).floor())
    } else {
        format!("{}m", strip(super::format::to_fixed(value / 1_000_000., 1)))
    }
}

/// The meter's percentage label: one decimal under 10%, whole numbers above.
pub fn format_meter_percentage(value: Option<f64>) -> Option<String> {
    let value = value.filter(|value| value.is_finite())?;
    if value < 10. {
        let fixed = super::format::to_fixed(value, 1);
        return Some(format!("{}%", fixed.strip_suffix(".0").unwrap_or(&fixed)));
    }
    Some(format!("{}%", (value + 0.5).floor()))
}

/// `formatContextWindowCompactionMessage`.
pub fn compaction_message(model_display_name: Option<&str>, auto_compact_threshold: Option<f64>) -> String {
    if let Some(threshold) = auto_compact_threshold.filter(|threshold| *threshold > 0.) {
        return format!("Compacts automatically at about {} tokens.", super::format::format_count(threshold));
    }
    match model_display_name {
        Some(name) => format!("Context for {name} compacts automatically when needed."),
        None => "Context compacts automatically when needed.".to_owned(),
    }
}

/// `formatContextWindowCost`: four decimals under a cent.
pub fn format_context_cost(amount: f64, currency: &str) -> String {
    let digits = if amount.abs() > 0. && amount.abs() < 0.01 { 4 } else { 2 };
    format!("{currency} {}", super::format::to_fixed(amount, digits))
}

/// The meter's accessible summary.
pub fn meter_label(snapshot: &ContextWindowSnapshot) -> String {
    match (snapshot.max_tokens, format_meter_percentage(snapshot.used_percentage)) {
        (Some(_), Some(percent)) => format!("Context window {percent} used"),
        _ => format!("Context window {} tokens used", format_context_tokens(Some(snapshot.used_tokens))),
    }
}
