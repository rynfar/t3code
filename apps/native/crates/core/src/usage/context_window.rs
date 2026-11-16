//! The thread's context window snapshot (`packages/client-runtime/src/state/contextWindow.ts`).
//! The newest provider turn's token usage wins, then the active provider thread's reported
//! context, then a fork's inherited usage before its first turn (`contextTransfers`), then the
//! last compaction.

use serde::Deserialize;
use serde_json::Value;

use crate::model::ModelSelection;
use crate::server_config::{ServerProvider, ServerSettings};
use crate::sidebar::time::parse_iso;
use crate::thread::{Entity, ProjectedTurnItem, ThreadProjection};
use crate::turn_item::TurnItemKind;

pub const COMPACTION_SUGGESTION_MIN_TOKENS: f64 = 200_000.0;
pub const CLAUDE_RESUME_COMPACTION_FALLBACK_MS: i64 = 70 * 60_000;
pub const CLAUDE_RESUME_COMPACTION_NEVER_ANSWER: &str = "Don't ask again";

/// `OrchestrationV2ProviderTurnTokenUsage`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnTokenUsage {
    used_tokens: f64,
    #[serde(default)]
    max_tokens: Option<f64>,
    #[serde(default)]
    auto_compact_threshold: Option<f64>,
    #[serde(default)]
    prompt_cache_ttl_ms: Option<f64>,
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
    pub prompt_cache_ttl_ms: Option<f64>,
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
fn latest_turn_usage(turns: &[Entity], provider_thread_id: Option<&str>) -> Option<TurnTokenUsage> {
    let mut latest: Option<(TurnTokenUsage, Option<i64>)> = None;
    for turn in turns {
        if let Some(expected_thread) = provider_thread_id
            && turn.value.get("providerThreadId").and_then(Value::as_str) != Some(expected_thread)
        {
            continue;
        }
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

fn inherited_fork_usage(projection: &ThreadProjection) -> Option<TurnTokenUsage> {
    let forked_from = projection.thread.forked_from.as_ref()?;
    if forked_from.get("type").and_then(Value::as_str) != Some("run") {
        return None;
    }
    let source_thread_id = forked_from.get("threadId").and_then(Value::as_str)?;
    let source_run_id = forked_from.get("runId").and_then(Value::as_str)?;
    let target_thread_id = projection.thread.id.as_str();
    let instance_id = projection.thread.model_selection.instance_id.as_str();
    projection.context_transfers.iter().find_map(|transfer| {
        let value = &transfer.value;
        let matches_transfer = value.get("type").and_then(Value::as_str) == Some("fork")
            && value.get("targetThreadId").and_then(Value::as_str) == Some(target_thread_id)
            && value.get("sourceThreadId").and_then(Value::as_str) == Some(source_thread_id)
            && value.get("sourcePoint").and_then(|p| p.get("runId")).and_then(Value::as_str) == Some(source_run_id)
            && value.get("sourceProviderInstanceId").and_then(Value::as_str) == Some(instance_id)
            && (value.get("status").and_then(Value::as_str) == Some("pending")
                || value.get("resolution").and_then(|r| r.get("strategy")).and_then(Value::as_str) == Some("native_fork"));
        if !matches_transfer {
            return None;
        }
        let usage = value.get("sourceTokenUsage").filter(|usage| !usage.is_null())?;
        serde_json::from_value::<TurnTokenUsage>(usage.clone()).ok()
    })
}

fn build_context_window_snapshot(
    visible_turn_items: &[ProjectedTurnItem],
    live: Option<TurnTokenUsage>,
    provider_thread: Option<&Value>,
    provider_usage: Option<ContextUsage>,
) -> Option<ContextWindowSnapshot> {
    if let Some(live) = live {
        let used = live.used_tokens.max(0.);
        return Some(ContextWindowSnapshot {
            used_tokens: used,
            total_processed_tokens: None,
            max_tokens: live.max_tokens,
            used_percentage: used_percentage(used, live.max_tokens),
            compacts_automatically: Some(true),
            auto_compact_threshold: live.auto_compact_threshold,
            prompt_cache_ttl_ms: finite(live.prompt_cache_ttl_ms),
            cost: None,
            updated_at: live.updated_at,
        });
    }
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
            prompt_cache_ttl_ms: None,
            cost: usage.cost.map(|cost| (cost.amount, cost.currency)),
            updated_at: updated_at.to_owned(),
        });
    }
    visible_turn_items.iter().rev().find_map(|row| {
        let TurnItemKind::Compaction { before_token_count, after_token_count, .. } = &row.item.kind else { return None };
        let used = (*after_token_count)? as f64;
        Some(ContextWindowSnapshot {
            used_tokens: used,
            total_processed_tokens: before_token_count.map(|count| count as f64),
            max_tokens: None,
            used_percentage: None,
            compacts_automatically: Some(true),
            auto_compact_threshold: None,
            prompt_cache_ttl_ms: None,
            cost: None,
            updated_at: row.item.started_at.clone().unwrap_or_else(|| row.item.updated_at.clone()),
        })
    })
}

/// `deriveLatestContextWindowSnapshot` over the open thread.
pub fn context_window_snapshot(projection: &ThreadProjection) -> Option<ContextWindowSnapshot> {
    let provider_thread = active_provider_thread(projection);
    let provider_usage = provider_thread
        .and_then(|thread| thread.get("contextUsage"))
        .filter(|usage| !usage.is_null())
        .and_then(|usage| serde_json::from_value::<ContextUsage>(usage.clone()).ok());
    let live = latest_turn_usage(&projection.provider_turns, None)
        .or_else(|| if provider_usage.is_none() { inherited_fork_usage(projection) } else { None });
    build_context_window_snapshot(&projection.visible_turn_items, live, provider_thread, provider_usage)
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

/// `resolveIdleCompaction` (`packages/contracts/src/settings.ts`): checks the per-instance override
/// before falling back to the environment-wide setting.
pub fn resolve_idle_compaction(settings: &ServerSettings, instance_id: &str) -> &'static str {
    let instance_override = settings
        .provider_instances
        .get(instance_id)
        .and_then(|instance| instance.config.as_ref())
        .and_then(|config| config.get("idleCompaction"))
        .and_then(Value::as_str)
        .and_then(crate::settings::providers::background::known_idle_compaction_mode);
    instance_override.unwrap_or_else(|| settings.provider_panel.idle_compaction())
}

/// `isClaudeResumeCompactionQuestion` (`packages/shared/src/claudeCompaction.ts`).
pub fn is_claude_resume_compaction_question(question: &str) -> bool {
    let Some(rest) = question.strip_prefix("This session is ") else { return false };
    let Some(body) = rest.strip_suffix(" tokens. Compact it before continuing?") else { return false };
    let Some((age, count)) = body.split_once(" old and uses ") else { return false };
    let valid_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let valid_age = if let Some((hours, mins_part)) = age.split_once("h ") {
        valid_digits(hours) && mins_part.strip_suffix('m').is_some_and(valid_digits)
    } else {
        age.strip_suffix('m').is_some_and(valid_digits)
    };
    if !valid_age {
        return false;
    }
    let mut groups = count.split(',');
    let Some(first) = groups.next() else { return false };
    if !(1..=3).contains(&first.len()) || !valid_digits(first) {
        return false;
    }
    groups.all(|group| group.len() == 3 && valid_digits(group))
}

/// `hasDismissedResumeCompaction` (`contextWindow.ts`): checks resolved user-input requests on
/// the projection for the `"Don't ask again"` answer to a resume-compaction prompt.
pub fn has_dismissed_resume_compaction(projection: &ThreadProjection) -> bool {
    projection.runtime_requests.iter().any(|request| {
        let v = &request.value;
        if v.get("kind").and_then(Value::as_str) != Some("user_input")
            || v.get("status").and_then(Value::as_str) != Some("resolved")
        {
            return false;
        }
        let Some(answers) = v.get("answers").and_then(Value::as_object) else { return false };
        answers.iter().any(|(question, answer)| {
            is_claude_resume_compaction_question(question)
                && answer.as_str() == Some(CLAUDE_RESUME_COMPACTION_NEVER_ANSWER)
        })
    })
}

/// `shouldOfferResumeCompaction` (`contextWindow.ts`).
pub fn should_offer_resume_compaction(
    provider_driver: &str,
    used_tokens: Option<f64>,
    updated_at: Option<&str>,
    prompt_cache_ttl_ms: Option<f64>,
    now_ms: i64,
) -> bool {
    if provider_driver != "claudeAgent" || used_tokens.unwrap_or(0.0) < COMPACTION_SUGGESTION_MIN_TOKENS {
        return false;
    }
    let Some(updated_ms) = updated_at.and_then(parse_iso) else { return false };
    let ttl_ms = prompt_cache_ttl_ms
        .filter(|ttl| ttl.is_finite() && *ttl >= 0.0)
        .map(|ttl| ttl as i64)
        .unwrap_or(CLAUDE_RESUME_COMPACTION_FALLBACK_MS);
    now_ms.saturating_sub(updated_ms) >= ttl_ms
}

/// `resolvePendingHandoff(...) !== null` (`packages/client-runtime/src/handoff.ts`).
pub fn has_pending_handoff(
    projection: &ThreadProjection,
    selection: &ModelSelection,
    providers: &[ServerProvider],
) -> bool {
    if projection.visible_turn_items.is_empty() {
        return false;
    }
    let Some(from) = projection.runs.iter().rev().find_map(|run| {
        (run.status != "rolled_back"
            && (run.started_at.is_some()
                || (run.status == "starting" && run.context_handoff_id.is_some())))
        .then_some(&run.model_selection)
    }) else {
        return false;
    };
    if from.instance_id == selection.instance_id {
        return false;
    }
    let source = providers.iter().find(|provider| provider.instance_id == from.instance_id);
    let target = providers.iter().find(|provider| provider.instance_id == selection.instance_id);
    match (source.and_then(|s| s.continuation.as_ref()), target.and_then(|t| t.continuation.as_ref())) {
        (Some(src_cont), Some(tgt_cont)) => {
            source.map(|s| &s.driver) != target.map(|t| &t.driver) || src_cont.group_key != tgt_cont.group_key
        }
        _ => true,
    }
}

/// `compactBeforeSendTokens` (`packages/client-runtime/src/state/contextWindow.ts`): returns the
/// token count a plain send compacts first for an idle stale Claude session, or `None`.
pub fn compact_before_send_tokens(
    projection: &ThreadProjection,
    selection: &ModelSelection,
    providers: &[ServerProvider],
    settings: &ServerSettings,
    text: &str,
    thread_idle: bool,
    now_ms: i64,
) -> Option<f64> {
    let provider = providers.iter().find(|entry| entry.instance_id == selection.instance_id)?;
    if !thread_idle
        || crate::composer::is_compact_command(text)
        || resolve_idle_compaction(settings, &provider.instance_id) == "off"
        || !provider.slash_commands.iter().any(|command| command.name == "compact")
        || has_pending_handoff(projection, selection, providers)
        || projection
            .runtime_requests
            .iter()
            .any(|request| request.value.get("status").and_then(Value::as_str) == Some("pending"))
        || projection
            .runs
            .iter()
            .any(|run| run.status == "queued" && run.queue_held == Some(true))
    {
        return None;
    }
    let has_compactable_user_message = projection.visible_turn_items.iter().any(|row| {
        matches!(
            &row.item.kind,
            TurnItemKind::UserMessage { text, attachments, .. }
                if !text.trim().eq_ignore_ascii_case("/compact") || !attachments.is_empty()
        )
    });
    let has_unprojected_started_run = projection.runs.iter().any(|run| {
        run.status != "rolled_back"
            && run.started_at.is_some()
            && !projection.visible_turn_items.iter().any(|row| {
                matches!(&row.item.kind, TurnItemKind::UserMessage { .. })
                    && row.item.run_id.as_deref() == Some(run.id.as_str())
            })
    });
    if (!has_compactable_user_message && !has_unprojected_started_run)
        || has_dismissed_resume_compaction(projection)
    {
        return None;
    }
    let active_provider_thread_id = projection.thread.active_provider_thread_id.as_deref();
    let provider_thread = active_provider_thread(projection);
    let provider_usage = provider_thread
        .and_then(|thread| thread.get("contextUsage"))
        .filter(|usage| !usage.is_null())
        .and_then(|usage| serde_json::from_value::<ContextUsage>(usage.clone()).ok());
    let live_usage = latest_turn_usage(&projection.provider_turns, active_provider_thread_id).or_else(|| {
        (provider_usage.is_none() && selection.instance_id == projection.thread.model_selection.instance_id)
            .then(|| inherited_fork_usage(projection))
            .flatten()
    });
    let ttl_ms = live_usage.as_ref().and_then(|usage| usage.prompt_cache_ttl_ms);
    let snapshot =
        build_context_window_snapshot(&projection.visible_turn_items, live_usage, provider_thread, provider_usage)?;
    should_offer_resume_compaction(
        &provider.driver,
        Some(snapshot.used_tokens),
        Some(&snapshot.updated_at),
        ttl_ms,
        now_ms,
    )
    .then_some(snapshot.used_tokens)
}
