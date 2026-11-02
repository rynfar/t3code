//! `OrchestrationV2TurnItem`: base fields shared by every item, plus a `type`-tagged body.
//! Unknown `type`s decode to `TurnItemKind::Unknown` and are filtered out, as the web's
//! `ForwardCompatibleUnionArray` does. Nested shapes the native UI does not render yet stay
//! as `Value`.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnItem {
    pub id: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub parent_item_id: Option<String>,
    pub ordinal: u64,
    pub status: String,
    pub title: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub updated_at: String,
    #[serde(flatten)]
    pub kind: TurnItemKind,
}

impl TurnItem {
    pub fn is_known(&self) -> bool {
        !matches!(self.kind, TurnItemKind::Unknown)
    }

    pub fn is_queued_user_message(&self) -> bool {
        matches!(&self.kind, TurnItemKind::UserMessage { input_intent, .. } if input_intent == "queued_turn")
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status.as_str(), "pending" | "running" | "waiting")
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub id: String,
    pub text: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointFileSummary {
    pub path: String,
    pub kind: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFailure {
    pub class: String,
    pub message: String,
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum TurnItemKind {
    Notification {
        source: Value,
        outcome: String,
        summary: String,
        #[serde(default)]
        detail: Option<String>,
    },
    UserMessage {
        message_id: String,
        text: String,
        input_intent: String,
        #[serde(default)]
        attachments: Vec<Value>,
        #[serde(default)]
        model_switch: Option<Value>,
        #[serde(default)]
        scheduled_task_id: Option<String>,
        #[serde(default)]
        sender_thread_id: Option<String>,
    },
    AssistantMessage {
        message_id: String,
        text: String,
        streaming: bool,
        #[serde(default)]
        attachments: Option<Vec<Value>>,
    },
    Reasoning {
        text: String,
        streaming: bool,
    },
    ProposedPlan {
        plan_id: String,
        markdown: String,
        streaming: bool,
    },
    TodoList {
        plan_id: String,
        steps: Vec<PlanStep>,
        #[serde(default)]
        explanation: Option<String>,
    },
    UserInputRequest {
        request_id: String,
        questions: Vec<Value>,
        #[serde(default)]
        question_answer: Option<Value>,
    },
    FileChange {
        file_name: String,
        #[serde(default)]
        additions: Option<u64>,
        #[serde(default)]
        deletions: Option<u64>,
        #[serde(default)]
        diff_str: Option<String>,
        #[serde(default)]
        old_str: Option<String>,
        #[serde(default)]
        new_str: Option<String>,
    },
    CommandExecution {
        input: String,
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        output_indicates_failure: Option<bool>,
        #[serde(default)]
        exit_code: Option<i64>,
    },
    FileSearch {
        #[serde(default)]
        pattern: Option<String>,
        #[serde(default)]
        results: Option<Vec<Value>>,
    },
    WebSearch {
        #[serde(default)]
        patterns: Option<Vec<String>>,
        #[serde(default)]
        results: Option<Vec<Value>>,
    },
    ApprovalRequest {
        request_id: String,
        request_kind: String,
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        options: Option<Vec<Value>>,
    },
    Checkpoint {
        checkpoint_id: String,
        scope_id: String,
        files: Vec<CheckpointFileSummary>,
    },
    RunInterruptRequest {
        message: String,
    },
    RunInterruptResult {
        message: String,
    },
    SystemNotice {
        message: String,
    },
    Error {
        failure: ProviderFailure,
    },
    Compaction {
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        before_token_count: Option<u64>,
        #[serde(default)]
        after_token_count: Option<u64>,
    },
    Handoff {
        #[serde(default)]
        to_model: Option<String>,
        strategy: String,
        #[serde(default)]
        summary: Option<String>,
    },
    Fork {
        target_thread_id: String,
    },
    ThreadCreated {
        target_thread_id: String,
        target_model: String,
    },
    SecretRequest {
        label: String,
        reason: String,
        secret_status: String,
    },
    LinkRequest {
        reason: String,
        link_status: String,
        #[serde(default)]
        label: Option<String>,
    },
    Subagent {
        subagent_id: String,
        child_thread_id: Option<String>,
        prompt: String,
        #[serde(default)]
        progress: Option<String>,
        result: Option<String>,
    },
    DynamicTool {
        tool_name: Option<String>,
        #[serde(default)]
        input: Value,
        #[serde(default)]
        output: Option<Value>,
    },
    #[serde(other)]
    Unknown,
}
