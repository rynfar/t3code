//! Thread-level inputs for row derivation, from `packages/client-runtime/src/state/threadExecution.ts`
//! (`deriveThreadActivityRun`, `deriveThreadStructureRuntime`), `threadRequests.ts`
//! (pending approvals), plus `shouldCollapseUserMessage` (`MessagesTimeline.logic.ts`),
//! `resolveUserMessagePresentation` (`userMessage.ts`) and `proposedPlanText.ts`.

use std::collections::HashSet;

use serde_json::Value;

use super::ChatMessage;
use crate::thread::{Entity, Run, ThreadProjection};

/// `TimelineLatestRun`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LatestRun {
    pub run_id: String,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    /// The run's latest provider turn finished while the run itself is still open.
    pub turn_ended: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThreadActivity {
    pub latest_run: Option<LatestRun>,
    pub running_run_id: Option<String>,
    pub is_working: bool,
    /// A provider-native subagent thread working through its runless root turn.
    pub runless_work_active: bool,
    pub active_turn_started_at: Option<String>,
    pub pending_approval_ids: HashSet<String>,
}

const ACTIVITY: [&str; 4] = ["preparing", "starting", "running", "waiting"];
const INTERRUPTIBLE: [&str; 3] = ["preparing", "starting", "running"];

fn latest_matching(runs: &[Run], predicate: impl Fn(&Run) -> bool) -> Option<&Run> {
    runs.iter().filter(|run| predicate(run)).max_by_key(|run| run.ordinal)
}

/// `summarizeThreadRun`: an open run whose latest provider turn has stopped reports `turnEnded`.
fn summarize(run: &Run, provider_turns: &[Entity]) -> LatestRun {
    let turn_status = ACTIVITY.contains(&run.status.as_str()).then(|| {
        let attempt = run.active_attempt_id.as_deref()?;
        provider_turns
            .iter()
            .filter(|turn| turn.value.get("runAttemptId").and_then(Value::as_str) == Some(attempt))
            .max_by_key(|turn| turn.value.get("ordinal").and_then(Value::as_u64).unwrap_or(0))
            .and_then(|turn| turn.value.get("status").and_then(Value::as_str))
    });
    LatestRun {
        run_id: run.id.clone(),
        status: run.status.clone(),
        started_at: run.started_at.clone(),
        completed_at: run.completed_at.clone(),
        turn_ended: matches!(turn_status.flatten(), Some(status) if status != "pending" && status != "running"),
    }
}

/// `deriveRunlessWorkStartedAt`: provider-native subagent threads have no app runs; their work is
/// a runless root turn whose status follows the subagent.
fn runless_work_started_at(projection: &ThreadProjection) -> Option<String> {
    let thread = &projection.thread;
    if thread.lineage.relationship_to_parent.as_deref() != Some("subagent") || thread.creation_source.as_deref() != Some("provider") {
        return None;
    }
    let node = projection.nodes.iter().rev().find(|node| {
        node.value.get("kind").and_then(Value::as_str) == Some("root_turn") && node.value.get("runId").is_none_or(Value::is_null)
    })?;
    let active = matches!(node.value.get("status").and_then(Value::as_str), Some("pending" | "running" | "waiting"));
    active.then(|| node.value.get("startedAt").and_then(Value::as_str).unwrap_or_default().to_owned())
}

/// The run the timeline treats as current and whether the thread is working. Held queues are
/// not on the native wire model yet, so every run counts as unheld.
pub fn derive_activity(projection: &ThreadProjection) -> ThreadActivity {
    let runs = &projection.runs;
    let activity_run = latest_matching(runs, |run| ACTIVITY.contains(&run.status.as_str()))
        .or_else(|| latest_matching(runs, |_| true));
    let running_run_id = latest_matching(runs, |run| INTERRUPTIBLE.contains(&run.status.as_str())).map(|run| run.id.clone());
    let live = latest_matching(runs, |run| ACTIVITY.contains(&run.status.as_str()));
    let runless = runless_work_started_at(projection);
    let is_working = live.is_some() || runless.is_some();
    let active_turn_started_at =
        live.map(|run| run.started_at.clone().unwrap_or_else(|| run.requested_at.clone())).or_else(|| runless.clone().filter(|at| !at.is_empty()));
    let pending_approval_ids = projection
        .runtime_requests
        .iter()
        .filter(|request| {
            let get = |key: &str| request.value.get(key).and_then(Value::as_str);
            get("status") == Some("pending") && !matches!(get("kind"), Some("user_input" | "auth_refresh" | "dynamic_tool_call"))
        })
        .map(|request| request.id.clone())
        .collect();
    ThreadActivity {
        latest_run: activity_run.map(|run| summarize(run, &projection.provider_turns)),
        running_run_id,
        is_working,
        runless_work_active: runless.is_some(),
        active_turn_started_at,
        pending_approval_ids,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    Automation,
    Agent,
    T3Code,
    Guest,
}

/// `resolveUserMessagePresentation(...).attribution`.
pub fn user_message_attribution(message: &ChatMessage) -> Option<Attribution> {
    if message.meta.guest_name.is_some() || message.meta.guest_text.is_some() {
        return Some(Attribution::Guest);
    }
    if scheduled_task_id_from_message(message).is_some() || has_legacy_automation_prefix(&message.text) {
        return Some(Attribution::Automation);
    }
    match (message.meta.created_by.as_deref(), message.meta.creation_source.as_deref()) {
        (Some("agent"), Some("server")) => Some(Attribution::T3Code),
        (Some("agent"), _) => Some(Attribution::Agent),
        _ => None,
    }
}

const MAX_COLLAPSED_LINES: usize = 8;
const MAX_COLLAPSED_LENGTH: usize = 600;

/// Markdown links count by their label: `visibleUserMessageText`.
fn visible_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let label_start = open + 1;
        let Some(close) = rest[label_start..].find(']').map(|i| label_start + i) else { break };
        let label = &rest[label_start..close];
        let after = &rest[close + 1..];
        let target_end = after.strip_prefix('(').and_then(|target| target.find(')').filter(|end| !target[..*end].contains([' ', '\n'])));
        match target_end {
            Some(end) if !label.contains('\n') => {
                let prefix = rest[..open].strip_suffix('!').unwrap_or(&rest[..open]);
                out.push_str(prefix);
                out.push_str(label);
                rest = &after[end + 2..];
            }
            _ => {
                out.push_str(&rest[..label_start]);
                rest = &rest[label_start..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `shouldCollapseUserMessage`.
pub fn should_collapse_user_message(text: &str) -> bool {
    let visible = visible_text(text);
    !visible.trim().is_empty() && (visible.chars().count() > MAX_COLLAPSED_LENGTH || visible.split('\n').count() > MAX_COLLAPSED_LINES)
}

fn heading_text(line: &str) -> Option<&str> {
    let trimmed = line.strip_prefix("   ").or_else(|| line.strip_prefix("  ")).or_else(|| line.strip_prefix(' ')).unwrap_or(line);
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &trimmed[hashes..];
    rest.starts_with([' ', '\t']).then(|| rest.trim()).filter(|text| !text.is_empty())
}

/// `proposedPlanTitle`.
pub fn plan_title(markdown: &str) -> Option<String> {
    markdown.lines().find_map(heading_text).map(str::to_owned)
}

/// `stripDisplayedPlanMarkdown`: drops the title heading and a leading "Summary" heading.
pub fn strip_displayed_plan_markdown(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.trim_end().lines().collect();
    let mut start = usize::from(lines.first().is_some_and(|line| heading_text(line).is_some()));
    while lines.get(start).is_some_and(|line| line.trim().is_empty()) {
        start += 1;
    }
    if lines.get(start).and_then(|line| heading_text(line)).is_some_and(|text| text.eq_ignore_ascii_case("summary")) {
        start += 1;
        while lines.get(start).is_some_and(|line| line.trim().is_empty()) {
            start += 1;
        }
    }
    lines.get(start..).map(|rest| rest.join("\n")).unwrap_or_default()
}

/// Resolved presentation of a user message (`resolveUserMessagePresentation` in `userMessage.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessagePresentation {
    pub text: String,
    pub attribution: Option<Attribution>,
    pub scheduled_task_id: Option<String>,
}

/// Parse `scheduled-task-message:<id>:<ts>:(scheduled|manual)` when `meta.scheduled_task_id` is absent.
pub fn scheduled_task_id_from_message(message: &ChatMessage) -> Option<String> {
    if let Some(id) = message.meta.scheduled_task_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return Some(id.to_owned());
    }
    let rest = message.id.strip_prefix("scheduled-task-message:")?;
    let suffix_start = rest.rfind(':')?;
    let trigger = &rest[suffix_start + 1..];
    if !matches!(trigger, "scheduled" | "manual") {
        return None;
    }
    let before_trigger = &rest[..suffix_start];
    let ts_start = before_trigger.rfind(':')?;
    let ts = &before_trigger[ts_start + 1..];
    if ts.is_empty() || !ts.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id = &before_trigger[..ts_start];
    (!id.is_empty()).then(|| id.to_owned())
}

fn has_legacy_automation_prefix(text: &str) -> bool {
    strip_legacy_automation_prefix(text).is_some()
}

fn strip_legacy_automation_prefix(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("[Triggered by schedule task: ")?;
    let close = rest.find(']')?;
    if rest[..close].contains(['\r', '\n']) {
        return None;
    }
    let after_bracket = &rest[close + 1..];
    if let Some(after) = after_bracket.strip_prefix("\r\n\r\n") {
        Some(after)
    } else if let Some(after) = after_bracket.strip_prefix("\n\n") {
        Some(after)
    } else {
        None
    }
}

/// Strip automation trigger prefix and attachment-only bootstrap prompt (`resolveUserMessagePresentation`).
pub fn resolve_user_message_presentation(message: &ChatMessage) -> UserMessagePresentation {
    let scheduled_task_id = scheduled_task_id_from_message(message);
    let attribution = user_message_attribution(message);
    let mut text = message.text.as_ref();
    if attribution == Some(Attribution::Automation)
        && let Some(stripped) = strip_legacy_automation_prefix(text)
    {
        text = stripped;
    }
    let display = if crate::composer::attachments::is_attachment_only_prompt(text) {
        String::new()
    } else {
        text.to_owned()
    };
    UserMessagePresentation { text: display, attribution, scheduled_task_id }
}

/// Partitioned attachments and file-card flags for a user message (`resolveUserMessageFileCards`).
#[derive(Debug, Clone, PartialEq)]
pub struct UserFileCard {
    pub attachment: serde_json::Value,
    pub label: String,
    pub size_bytes: Option<u64>,
    pub downloadable: bool,
    pub pasted_text: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserAttachmentPartition {
    pub images: Vec<serde_json::Value>,
    pub audios: Vec<serde_json::Value>,
    pub files: Vec<UserFileCard>,
    pub file_cards_only: bool,
    pub audio_only: bool,
}

pub fn is_audio_attachment(attachment: &serde_json::Value) -> bool {
    attachment.get("kind").and_then(serde_json::Value::as_str) == Some("audio")
        || attachment
            .get("mimeType")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|mime| mime.to_ascii_lowercase().starts_with("audio/"))
}

pub fn user_message_file_label(attachment: &serde_json::Value) -> String {
    if attachment.get("pastedText").and_then(serde_json::Value::as_bool) == Some(true) {
        return "Pasted text".to_owned();
    }
    attachment
        .get("name")
        .or_else(|| attachment.get("filename"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("File")
        .to_owned()
}

pub fn partition_user_attachments(attachments: &[serde_json::Value], display_text: &str) -> UserAttachmentPartition {
    let mut out = UserAttachmentPartition::default();
    for att in attachments {
        let kind = att.get("kind").and_then(serde_json::Value::as_str);
        let ty = att.get("type").and_then(serde_json::Value::as_str);
        let mime = att.get("mimeType").and_then(serde_json::Value::as_str).unwrap_or("");
        if is_audio_attachment(att) {
            out.audios.push(att.clone());
        } else if ty == Some("image") || kind == Some("image") || mime.to_ascii_lowercase().starts_with("image/") {
            out.images.push(att.clone());
        } else {
            let pasted_text = att.get("pastedText").and_then(serde_json::Value::as_bool) == Some(true);
            let downloadable = att.get("downloadable").and_then(serde_json::Value::as_bool) != Some(false)
                && att.get("id").and_then(serde_json::Value::as_str).is_some_and(|id| !id.is_empty());
            let size_bytes = att.get("sizeBytes").and_then(serde_json::Value::as_u64);
            out.files.push(UserFileCard {
                attachment: att.clone(),
                label: user_message_file_label(att),
                size_bytes,
                downloadable,
                pasted_text,
            });
        }
    }
    let text_empty = display_text.trim().is_empty();
    out.file_cards_only = text_empty && out.images.is_empty() && out.audios.is_empty() && !out.files.is_empty();
    out.audio_only = text_empty && out.images.is_empty() && out.files.is_empty() && !out.audios.is_empty();
    out
}

/// Resolved same-harness model/effort switch (`resolveModelSwitchChange` in `handoff.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSwitchDisplay {
    pub from_summary: String,
    pub to_summary: String,
    pub model_changed: bool,
    pub effort_changed: bool,
}

const FALLBACK_EFFORT_KEYS: [&str; 7] =
    ["reasoningEffort", "effort", "reasoning", "variant", "reasoning_effort", "thinking_level", "thinking"];

fn resolve_effort(
    selection: &serde_json::Value,
    providers: &[crate::providers::ProviderEntry<'_>],
) -> Option<(String, String)> {
    let instance_id = selection.get("instanceId").and_then(serde_json::Value::as_str).unwrap_or("");
    let slug = selection.get("model").and_then(serde_json::Value::as_str).unwrap_or("");
    let opts = selection.get("options");
    let model = providers
        .iter()
        .find(|p| p.instance_id() == instance_id)
        .and_then(|p| p.provider.models.iter().find(|m| m.slug == slug));
    let Some(model) = model else {
        let value = FALLBACK_EFFORT_KEYS.iter().find_map(|key| {
            opts.and_then(|o| o.get(*key))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })?;
        return Some((value.to_owned(), value.to_owned()));
    };
    let capabilities = model.capabilities.as_ref()?;
    let crate::server_config::OptionDescriptor::Select(select) =
        capabilities.option_descriptors.iter().find(|d| crate::traits::is_reasoning_control(d))?
    else {
        return None;
    };
    let raw = opts
        .and_then(|o| o.get(&select.id))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            crate::traits::current_value(&crate::server_config::OptionDescriptor::Select(select.clone()))
                .and_then(|v| match v {
                    crate::traits::OptionValue::Str(s) => Some(s),
                    crate::traits::OptionValue::Bool(_) => None,
                })
        })?;
    let label = select
        .options
        .iter()
        .find(|opt| opt.id == raw)
        .map(|opt| opt.label.clone())
        .unwrap_or_else(|| raw.clone());
    Some((raw, label))
}

fn describe_switch_side(
    selection: &serde_json::Value,
    providers: &[crate::providers::ProviderEntry<'_>],
    model_changed: bool,
    effort: Option<&str>,
) -> String {
    let instance_id = selection.get("instanceId").and_then(serde_json::Value::as_str).unwrap_or("");
    let slug = selection.get("model").and_then(serde_json::Value::as_str).unwrap_or("");
    let model_name = providers
        .iter()
        .find(|p| p.instance_id() == instance_id)
        .and_then(|p| p.provider.models.iter().find(|m| m.slug == slug))
        .map(|m| crate::providers::trigger_model_name(m).to_owned())
        .unwrap_or_else(|| slug.to_owned());
    match (model_changed.then_some(model_name).filter(|s| !s.is_empty()), effort) {
        (Some(m), Some(e)) => format!("{m} {e}"),
        (Some(m), None) => m,
        (None, Some(e)) => e.to_owned(),
        (None, None) => String::new(),
    }
}

pub fn resolve_model_switch_change(
    model_switch: &serde_json::Value,
    providers: &[crate::providers::ProviderEntry],
) -> Option<ModelSwitchDisplay> {
    let from = model_switch.get("from")?;
    let to = model_switch.get("to")?;
    let from_slug = from.get("model").and_then(serde_json::Value::as_str).unwrap_or("");
    let to_slug = to.get("model").and_then(serde_json::Value::as_str).unwrap_or("");
    let from_effort = resolve_effort(from, providers);
    let to_effort = resolve_effort(to, providers);
    let model_changed = from_slug != to_slug;
    let effort_labels = from_effort
        .as_ref()
        .zip(to_effort.as_ref())
        .filter(|((fv, _), (tv, _))| fv != tv)
        .map(|((_, fl), (_, tl))| (fl.as_str(), tl.as_str()));
    let effort_changed = effort_labels.is_some();
    if !model_changed && !effort_changed {
        return None;
    }
    Some(ModelSwitchDisplay {
        from_summary: describe_switch_side(from, providers, model_changed, effort_labels.map(|(f, _)| f)),
        to_summary: describe_switch_side(to, providers, model_changed, effort_labels.map(|(_, t)| t)),
        model_changed,
        effort_changed,
    })
}

/// Whether a user message is a provider-native workflow opening script (`isWorkflowScriptMessage`).
pub fn is_workflow_script_message(role: super::Role, text: &str, workflow_script: Option<bool>) -> bool {
    if role != super::Role::User {
        return false;
    }
    workflow_script.unwrap_or_else(|| starts_with_workflow_script(text))
}

fn starts_with_workflow_script(text: &str) -> bool {
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
        } else {
            break;
        }
    }
    let Some(after_export) = rest.strip_prefix("export") else { return false };
    let after_ws1 = after_export.trim_start();
    if after_ws1.len() == after_export.len() {
        return false;
    }
    let Some(after_const) = after_ws1.strip_prefix("const") else { return false };
    let after_ws2 = after_const.trim_start();
    if after_ws2.len() == after_const.len() {
        return false;
    }
    let Some(after_meta) = after_ws2.strip_prefix("meta") else { return false };
    after_meta.chars().next().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use serde_json::json;

    use super::*;
    use crate::composer::attachments::ATTACHMENT_ONLY_PROMPT;
    use crate::timeline::{MessageMeta, Role};

    fn user_msg(id: &str, text: &str, meta: MessageMeta) -> ChatMessage {
        ChatMessage {
            id: id.to_owned(),
            role: Role::User,
            text: Arc::from(text),
            run_id: None,
            streaming: false,
            created_at: String::new(),
            updated_at: String::new(),
            meta,
            item: None,
        }
    }

    #[test]
    fn user_message_presentation_strips_automation_and_bootstrap_prompts() {
        let auto = user_msg(
            "scheduled-task-message:task-42:1700000000000:scheduled",
            "[Triggered by schedule task: Daily check]\n\nRun checks",
            MessageMeta::default(),
        );
        let pres = resolve_user_message_presentation(&auto);
        assert_eq!(pres.attribution, Some(Attribution::Automation));
        assert_eq!(pres.scheduled_task_id.as_deref(), Some("task-42"));
        assert_eq!(pres.text, "Run checks");

        let boot = user_msg("m1", &format!("Ultrathink:\n{ATTACHMENT_ONLY_PROMPT}"), MessageMeta::default());
        assert_eq!(resolve_user_message_presentation(&boot).text, "");
    }

    #[test]
    fn partitions_user_attachments_for_pasted_text_audio_and_files() {
        let atts = vec![
            json!({ "id": "a1", "kind": "file", "name": "notes.txt", "pastedText": true, "sizeBytes": 42 }),
            json!({ "id": "a2", "kind": "audio", "name": "voice.webm", "mimeType": "audio/webm" }),
        ];
        let part = partition_user_attachments(&atts, "");
        assert_eq!(part.files.len(), 1);
        assert_eq!(part.files[0].label, "Pasted text");
        assert!(part.files[0].pasted_text);
        assert_eq!(part.audios.len(), 1);
        assert!(!part.file_cards_only);

        let files_only = partition_user_attachments(&atts[..1], "");
        assert!(files_only.file_cards_only);
    }

    #[test]
    fn detects_workflow_script_messages() {
        assert!(is_workflow_script_message(Role::User, "// comment\nexport const meta = {};", None));
        assert!(!is_workflow_script_message(Role::User, "Explain `export const meta`.", None));
        assert!(!is_workflow_script_message(Role::Assistant, "export const meta = {};", None));
    }

    #[test]
    fn resolves_model_switch_thinking_and_omits_half_effort_without_default() {
        let same_model_thinking = json!({
            "from": { "instanceId": "pi", "model": "claude-sonnet", "options": { "thinking": "low" } },
            "to": { "instanceId": "pi", "model": "claude-sonnet", "options": { "thinking": "high" } },
        });
        let display = resolve_model_switch_change(&same_model_thinking, &[]).expect("effort switch");
        assert!(!display.model_changed);
        assert!(display.effort_changed);
        assert_eq!((display.from_summary.as_str(), display.to_summary.as_str()), ("low", "high"));

        // Without a catalog default, an omitted effort on one side must not produce " → high".
        let missing_left = json!({
            "from": { "instanceId": "codex", "model": "gpt-5" },
            "to": { "instanceId": "codex", "model": "gpt-5", "options": { "reasoningEffort": "high" } },
        });
        assert!(resolve_model_switch_change(&missing_left, &[]).is_none());

        // Same model across instances does not count as a model change.
        let cross_inst = json!({
            "from": { "instanceId": "a", "model": "gpt-5" },
            "to": { "instanceId": "b", "model": "gpt-5" },
        });
        assert!(resolve_model_switch_change(&cross_inst, &[]).is_none());
    }
}


