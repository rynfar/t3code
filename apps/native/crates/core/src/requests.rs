//! What the agent is waiting on: approvals and questions, derived from a thread projection's
//! runtime requests and their turn items. Ports `derivePendingThreadRequests` from
//! `client-runtime/src/state/threadRequests.ts`, the answer drafting in
//! `client-runtime/src/state/pendingUserInput.ts`, and the progress in `web/src/pendingUserInput.ts`.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::commands::ApprovalDecision;
use crate::thread::ThreadProjection;
use crate::turn_item::TurnItemKind;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponseCapability {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    supports_feedback: Option<bool>,
}

/// `OrchestrationV2RuntimeRequest`: the fields deciding what is pending and how to answer it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeRequest {
    id: String,
    kind: String,
    status: String,
    response_capability: ResponseCapability,
    #[serde(default)]
    created_at: String,
}

/// `ProviderApprovalOption`.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalOption {
    pub decision: ApprovalDecision,
    pub label: String,
    pub warning: Option<String>,
}

/// `DEFAULT_APPROVAL_OPTIONS` in `ComposerPendingApprovalActions.tsx`.
pub fn default_approval_options() -> Vec<ApprovalOption> {
    [
        (ApprovalDecision::Cancel, "Cancel"),
        (ApprovalDecision::Decline, "Decline"),
        (ApprovalDecision::AcceptForSession, "Always allow this session"),
        (ApprovalDecision::Accept, "Approve"),
    ]
    .into_iter()
    .map(|(decision, label)| ApprovalOption { decision, label: label.to_owned(), warning: None })
    .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingApproval {
    pub request_id: String,
    /// `ProviderRequestKind`: command, file-read, file-change, mcp-elicitation, permission.
    pub request_kind: String,
    pub created_at: String,
    pub detail: Option<String>,
    pub reason: Option<String>,
    /// Why a full-access thread still asks: `ask_rule` or `provider_check`.
    pub full_access_override: Option<String>,
    pub app_name: Option<String>,
    pub options: Vec<ApprovalOption>,
    /// The provider can still take an answer (`responseCapability.type === "live"`).
    pub live: bool,
    pub supports_feedback: bool,
}

impl PendingApproval {
    /// `APPROVAL_PRESENTATION` in `TimelinePendingApprovalRow.tsx`: label and detail label.
    pub fn presentation(&self) -> (&'static str, &'static str) {
        match self.request_kind.as_str() {
            "command" => ("Command approval", "Command"),
            "file-change" => ("File change approval", "File change"),
            "file-read" => ("File read approval", "File to read"),
            "mcp-elicitation" => ("App access approval", "App access request"),
            _ => ("App permission approval", "Permission request"),
        }
    }

    /// The buttons beside the request (Decline, Approve) and the rest behind "More".
    pub fn split_options(&self) -> (Vec<&ApprovalOption>, Vec<&ApprovalOption>) {
        self.options
            .iter()
            .partition(|option| matches!(option.decision, ApprovalDecision::Decline | ApprovalDecision::Accept))
    }
}

pub const FULL_ACCESS_OVERRIDE_QUESTION: &str = "Why am I being asked this?";

/// `fullAccessOverrideExplanation`.
pub fn full_access_override_explanation(full_access_override: &str) -> &'static str {
    if full_access_override == "ask_rule" {
        "Full access skips approvals, but an ask rule in the agent's own settings matches this action, so it still asks. Remove or narrow that rule to stop these prompts."
    } else {
        "Full access skips approvals, but the agent still stops for a few safety checks of its own that no setting can skip, such as a delete whose path it can't verify. T3 Code can't turn these off, so check the request and approve it if it's what you expect."
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub value: Option<String>,
}

impl QuestionOption {
    /// What a pick records: the provider's value, else the label.
    pub fn answer_value(&self) -> &str {
        self.value.as_deref().unwrap_or(&self.label)
    }
}

/// `OrchestrationV2UserInputQuestion` with `multiSelect` defaulted.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub allow_custom_answer: Option<bool>,
    #[serde(default)]
    pub initial_answer: Option<String>,
}

impl Question {
    pub fn allows_custom_answer(&self) -> bool {
        self.allow_custom_answer != Some(false)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingUserInput {
    pub request_id: String,
    pub created_at: String,
    pub questions: Vec<Question>,
    /// `live`, `message` (answerable after the turn ends), or `not_resumable`.
    pub response_capability: String,
    pub dismissible: bool,
}

impl PendingUserInput {
    pub fn can_respond(&self) -> bool {
        self.response_capability != "not_resumable"
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PendingRequests {
    pub approvals: Vec<PendingApproval>,
    pub user_inputs: Vec<PendingUserInput>,
}

impl PendingRequests {
    pub fn is_empty(&self) -> bool {
        self.approvals.is_empty() && self.user_inputs.is_empty()
    }
}

fn approval_options(raw: &[Value]) -> Vec<ApprovalOption> {
    raw.iter()
        .filter_map(|option| {
            Some(ApprovalOption {
                decision: ApprovalDecision::parse(option.get("decision")?.as_str()?)?,
                label: option.get("label")?.as_str()?.to_owned(),
                warning: option.get("warning").and_then(Value::as_str).map(str::to_owned),
            })
        })
        .collect()
}

/// `derivePendingThreadRequests`, then sorted oldest first as `derivePendingApprovals` and
/// `derivePendingUserInputs` sort them. Runs on every streamed event, so settled requests are
/// skipped before anything is decoded.
pub fn pending_requests(projection: &ThreadProjection) -> PendingRequests {
    let mut pending = PendingRequests::default();
    for entity in &projection.runtime_requests {
        if entity.value.get("status").and_then(Value::as_str) != Some("pending") {
            continue;
        }
        let Ok(request) = RuntimeRequest::deserialize(&entity.value) else { continue };
        let capability = request.response_capability.kind.clone();
        if request.kind == "user_input" {
            let found = projection.turn_items.iter().rev().find_map(|item| match &item.kind {
                TurnItemKind::UserInputRequest { request_id, questions, response_mode, .. } if *request_id == request.id => {
                    Some((questions, response_mode))
                }
                _ => None,
            });
            let Some((questions, response_mode)) = found else { continue };
            pending.user_inputs.push(PendingUserInput {
                request_id: request.id,
                created_at: request.created_at,
                questions: questions.iter().filter_map(|q| Question::deserialize(q).ok()).collect(),
                dismissible: response_mode.as_deref() == Some("message") || capability == "message",
                response_capability: capability,
            });
            continue;
        }
        if request.kind == "auth_refresh" || request.kind == "dynamic_tool_call" {
            continue;
        }
        let item = projection.turn_items.iter().rev().find_map(|item| match &item.kind {
            TurnItemKind::ApprovalRequest { request_id, prompt, reason, options, full_access_override, app_name, .. }
                if *request_id == request.id =>
            {
                Some((prompt, reason, options, full_access_override, app_name))
            }
            _ => None,
        });
        let non_empty = |text: &Option<String>| text.clone().filter(|text| !text.is_empty());
        pending.approvals.push(PendingApproval {
            detail: item.and_then(|(prompt, ..)| non_empty(prompt)),
            reason: item.and_then(|(_, reason, ..)| non_empty(reason)),
            full_access_override: item.and_then(|(.., full_access, _)| full_access.clone()),
            app_name: item.and_then(|(.., app_name)| non_empty(app_name)),
            options: item
                .and_then(|(_, _, options, ..)| options.as_deref())
                .map(approval_options)
                .unwrap_or_else(default_approval_options),
            live: capability == "live",
            supports_feedback: capability == "live" && request.response_capability.supports_feedback == Some(true),
            request_id: request.id,
            request_kind: request.kind,
            created_at: request.created_at,
        });
    }
    pending.approvals.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    pending.user_inputs.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    pending
}

/// `PendingUserInputDraftAnswer` without attachments. `custom` keeps "typed nothing" (`Some("")`)
/// apart from "never typed" (`None`), which matters for questions with a suggested answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftAnswer {
    pub selected: Vec<String>,
    pub custom: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Text(String),
    Choices(Vec<String>),
}

impl Answer {
    pub fn to_json(&self) -> Value {
        match self {
            Answer::Text(text) => json!(text),
            Answer::Choices(choices) => json!(choices),
        }
    }
}

fn resolve_option_value(question: &Question, value: &str) -> Option<String> {
    if question.options.iter().any(|option| option.value.as_deref() == Some(value)) {
        return Some(value.to_owned());
    }
    let label = value.trim();
    (!label.is_empty() && question.options.iter().any(|option| option.value.is_none() && option.label.trim() == label))
        .then(|| label.to_owned())
}

/// `normalizeSelectedOptionValues`.
pub fn selected_values(question: &Question, draft: Option<&DraftAnswer>) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    for entry in draft.map(|draft| draft.selected.as_slice()).unwrap_or_default() {
        if let Some(value) = resolve_option_value(question, entry)
            && !values.contains(&value)
        {
            values.push(value);
        }
    }
    values
}

fn trimmed_custom(draft: Option<&DraftAnswer>) -> Option<String> {
    draft?.custom.as_deref().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned)
}

/// `resolvePendingUserInputAnswer`: the answer to submit, or `None` while unanswered.
pub fn resolve_answer(question: &Question, draft: Option<&DraftAnswer>) -> Option<Answer> {
    let custom = if !question.allows_custom_answer() {
        None
    } else if question.initial_answer.is_some() {
        draft.and_then(|draft| draft.custom.clone())
    } else {
        trimmed_custom(draft)
    };
    if let Some(custom) = custom.as_ref().filter(|custom| !custom.is_empty()) {
        return Some(Answer::Text(custom.clone()));
    }
    let selected = selected_values(question, draft);
    if question.multi_select {
        return if selected.is_empty() { custom.map(Answer::Text) } else { Some(Answer::Choices(selected)) };
    }
    selected.into_iter().next().or(custom).map(Answer::Text)
}

/// `setPendingUserInputCustomAnswer`.
pub fn set_custom_answer(question: &Question, draft: Option<&DraftAnswer>, text: &str) -> DraftAnswer {
    if !question.allows_custom_answer() {
        return draft.cloned().unwrap_or_default();
    }
    let keep_selection = question.initial_answer.is_none() && text.trim().is_empty();
    DraftAnswer {
        custom: Some(text.to_owned()),
        selected: if keep_selection { selected_values(question, draft) } else { Vec::new() },
    }
}

/// `isPendingUserInputOptionSelected`.
pub fn is_option_selected(question: &Question, draft: Option<&DraftAnswer>, value: &str) -> bool {
    if question.allows_custom_answer() && trimmed_custom(draft).is_some() {
        return false;
    }
    resolve_option_value(question, value).is_some_and(|value| selected_values(question, draft).contains(&value))
}

/// `togglePendingUserInputOptionSelection`. An unknown option leaves the draft as it was.
pub fn toggle_option(question: &Question, draft: Option<&DraftAnswer>, value: &str) -> DraftAnswer {
    let Some(value) = resolve_option_value(question, value) else { return draft.cloned().unwrap_or_default() };
    if !question.multi_select {
        return DraftAnswer { custom: Some(String::new()), selected: vec![value] };
    }
    let mut selected = selected_values(question, draft);
    match selected.iter().position(|existing| *existing == value) {
        Some(index) => {
            selected.remove(index);
        }
        None => selected.push(value),
    }
    DraftAnswer { custom: Some(String::new()), selected }
}

/// `seedUserInputDraftAnswers`: suggested answers show until the user edits them.
pub fn seed_drafts(questions: &[Question], drafts: &mut HashMap<String, DraftAnswer>) {
    for question in questions {
        if let Some(initial) = &question.initial_answer
            && question.allows_custom_answer()
        {
            drafts
                .entry(question.id.clone())
                .or_insert_with(|| DraftAnswer { custom: Some(initial.clone()), selected: Vec::new() });
        }
    }
}

/// `buildPendingUserInputAnswers`: every answer keyed by question id, or `None` while any is open.
pub fn build_answers(questions: &[Question], drafts: &HashMap<String, DraftAnswer>) -> Option<Map<String, Value>> {
    let mut answers = Map::new();
    for question in questions {
        answers.insert(question.id.clone(), resolve_answer(question, drafts.get(&question.id))?.to_json());
    }
    Some(answers)
}

/// `derivePendingUserInputProgress`, the parts the panel and the primary action read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub question_index: usize,
    pub is_last_question: bool,
    pub is_complete: bool,
    pub can_advance: bool,
    pub custom_answer: String,
}

pub fn progress(questions: &[Question], drafts: &HashMap<String, DraftAnswer>, question_index: usize) -> Progress {
    let index = question_index.min(questions.len().saturating_sub(1));
    let active = questions.get(index);
    let draft = active.and_then(|question| drafts.get(&question.id));
    Progress {
        question_index: index,
        is_last_question: questions.is_empty() || index + 1 >= questions.len(),
        is_complete: build_answers(questions, drafts).is_some(),
        can_advance: active.is_some_and(|question| resolve_answer(question, draft).is_some()),
        custom_answer: match active {
            Some(question) if question.allows_custom_answer() => draft.and_then(|draft| draft.custom.clone()).unwrap_or_default(),
            _ => String::new(),
        },
    }
}

/// `carryDisplacedCustomAnswerIntoPrompt`: text typed as an answer that an option then replaced
/// goes back to the thread's draft after a blank line.
pub fn carry_displaced_answer(prompt: &str, displaced: &str) -> String {
    let displaced = displaced.trim();
    match (prompt.trim().is_empty(), displaced.is_empty()) {
        (_, true) => prompt.to_owned(),
        (true, false) => displaced.to_owned(),
        (false, false) => format!("{}\n\n{displaced}", prompt.trim_end()),
    }
}

/// `formatPendingPrimaryActionLabel` for the full-width footer.
pub fn primary_action_label(progress: &Progress, responding: bool) -> &'static str {
    if responding {
        "Submitting..."
    } else if !progress.is_last_question {
        "Next question"
    } else if progress.question_index > 0 {
        "Submit answers"
    } else {
        "Submit answer"
    }
}

#[cfg(test)]
mod tests;
