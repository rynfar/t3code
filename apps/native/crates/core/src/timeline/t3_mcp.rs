//! T3's own MCP tool presentation, bounded name cache (`#229`), output failure detection, and
//! summary grouping: `packages/shared/src/t3McpToolPresentation.ts` and
//! `packages/client-runtime/src/t3ToolSummary.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use serde_json::{Map, Value};

use crate::pull_requests::link::{change_request_provider, parse_change_request_url};

const TOOL_NAME_CACHE_LIMIT: usize = 10_000;
const TOOL_NAME_CACHE_MAX_KEY_LENGTH: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum T3McpToolIcon {
    T3Code,
    Browser,
    Device,
    PullRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum T3McpSummaryAction {
    Capabilities,
    Delegate,
    TaskStatus,
    TaskCancel,
    ScheduleRun,
    ScheduleCreate,
    ScheduleList,
    ScheduleUpdate,
    ScheduleDelete,
    ThreadCreate,
    ThreadList,
    ThreadRead,
    ThreadSend,
    ThreadWait,
    ThreadInterrupt,
    WorkflowStop,
    BackgroundWait,
    ThreadConfiguration,
    ThreadConfigure,
    ThreadFork,
    ThreadMerge,
    ThreadSearch,
    ThreadTransfers,
    ThreadOrganize,
    ThreadUpdate,
    QueueList,
    QueueRead,
    QueueEdit,
    QueueCancel,
    QueueReorder,
    QueueSteer,
    QuestionList,
    QuestionRead,
    QuestionRespond,
    SecretRequest,
    WorktreeHandoff,
    WorktreeList,
    WorktreeStatus,
    ProjectList,
    ProjectRead,
    ProjectCreate,
    ProjectUpdate,
    ProjectDelete,
    ProjectClone,
    EnvironmentRead,
    EnvironmentLinks,
    EnvironmentLink,
    EnvironmentUnlink,
    EnvironmentUpdate,
    ThemeRead,
    ThemeSet,
    AttachmentPrepare,
    AttachmentDiscard,
    AttachmentSend,
    LinkPr,
    UnlinkPr,
    ListPrs,
    WatchPr,
    UnwatchPr,
    WritePrGuides,
    ReadPrGuide,
    Browser,
    Device,
    HtmlPreview,
    HtmlRender,
    SendToUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct T3McpToolDefinition {
    pub name: &'static str,
    pub action: &'static str,
    pub running: &'static str,
    pub completed: &'static str,
    pub detail: &'static str,
    pub icon: T3McpToolIcon,
    pub summary_action: T3McpSummaryAction,
}

const fn def(
    name: &'static str,
    action: &'static str,
    running: &'static str,
    completed: &'static str,
    detail: &'static str,
    summary_action: T3McpSummaryAction,
    icon: T3McpToolIcon,
) -> T3McpToolDefinition {
    T3McpToolDefinition { name, action, running, completed, detail, icon, summary_action }
}

const fn t3_tool(
    name: &'static str,
    action: &'static str,
    running: &'static str,
    completed: &'static str,
    detail: &'static str,
    summary_action: T3McpSummaryAction,
) -> T3McpToolDefinition {
    def(name, action, running, completed, detail, summary_action, T3McpToolIcon::T3Code)
}

const fn browser_tool(name: &'static str, action: &'static str, running: &'static str, completed: &'static str, detail: &'static str) -> T3McpToolDefinition {
    def(name, action, running, completed, detail, T3McpSummaryAction::Browser, T3McpToolIcon::Browser)
}

const fn device_tool(name: &'static str, action: &'static str, running: &'static str, completed: &'static str, detail: &'static str) -> T3McpToolDefinition {
    def(name, action, running, completed, detail, T3McpSummaryAction::Device, T3McpToolIcon::Device)
}

const fn pr_tool(
    name: &'static str,
    action: &'static str,
    running: &'static str,
    completed: &'static str,
    detail: &'static str,
    summary_action: T3McpSummaryAction,
) -> T3McpToolDefinition {
    def(name, action, running, completed, detail, summary_action, T3McpToolIcon::PullRequest)
}

const T3_MCP_TOOLS: &[T3McpToolDefinition] = &[
    pr_tool("link_pull_request", "Link", "Linking", "Linked", "a pull request", T3McpSummaryAction::LinkPr),
    pr_tool("link_pull_requests", "Link", "Linking", "Linked", "pull requests", T3McpSummaryAction::LinkPr),
    pr_tool("unlink_pull_request", "Unlink", "Unlinking", "Unlinked", "a pull request", T3McpSummaryAction::UnlinkPr),
    pr_tool("unlink_pull_requests", "Unlink", "Unlinking", "Unlinked", "pull requests by state", T3McpSummaryAction::UnlinkPr),
    pr_tool("list_thread_pull_requests", "Check", "Checking", "Checked", "linked pull requests", T3McpSummaryAction::ListPrs),
    pr_tool("watch_pull_request", "Watch", "Watching", "Watching", "a pull request", T3McpSummaryAction::WatchPr),
    pr_tool("unwatch_pull_request", "Stop watching", "Unwatching", "Stopped watching", "a pull request", T3McpSummaryAction::UnwatchPr),
    pr_tool("write_pull_request_guides", "Write", "Writing", "Started writing", "review guides", T3McpSummaryAction::WritePrGuides),
    pr_tool("read_pull_request_guide", "Read", "Reading", "Read", "a review guide", T3McpSummaryAction::ReadPrGuide),
    t3_tool("orchestrator_capabilities", "Get", "Getting", "Got", "orchestration capabilities", T3McpSummaryAction::Capabilities),
    t3_tool("delegate_task", "Delegate", "Delegating", "Delegated", "a child task", T3McpSummaryAction::Delegate),
    t3_tool("task_status", "Get", "Getting", "Got", "delegated task status", T3McpSummaryAction::TaskStatus),
    t3_tool("task_cancel", "Cancel", "Canceling", "Requested cancellation of", "a delegated task", T3McpSummaryAction::TaskCancel),
    t3_tool("schedule_task", "Schedule", "Scheduling", "Scheduled", "a task", T3McpSummaryAction::ScheduleCreate),
    t3_tool("list_scheduled_tasks", "List", "Listing", "Listed", "scheduled tasks", T3McpSummaryAction::ScheduleList),
    t3_tool("update_scheduled_task", "Update", "Updating", "Updated", "a scheduled task", T3McpSummaryAction::ScheduleUpdate),
    t3_tool("delete_scheduled_task", "Delete", "Deleting", "Requested deletion of", "a scheduled task", T3McpSummaryAction::ScheduleDelete),
    t3_tool("request_secret", "Ask for", "Asking for", "Asked for", "a secret", T3McpSummaryAction::SecretRequest),
    t3_tool("create_threads", "Create", "Creating", "Created", "T3 threads", T3McpSummaryAction::ThreadCreate),
    t3_tool("t3_thread_start", "Start", "Starting", "Started", "a T3 thread", T3McpSummaryAction::ThreadCreate),
    t3_tool("t3_thread_list", "List", "Listing", "Listed", "T3 threads", T3McpSummaryAction::ThreadList),
    t3_tool("t3_thread_read", "Read", "Reading", "Read", "a T3 thread", T3McpSummaryAction::ThreadRead),
    t3_tool("t3_thread_send", "Send", "Sending", "Sent", "to a T3 thread", T3McpSummaryAction::ThreadSend),
    t3_tool("t3_thread_wait", "Wait", "Waiting", "Waited", "for a T3 thread", T3McpSummaryAction::ThreadWait),
    t3_tool("t3_workflow_stop", "Stop", "Stopping", "Requested a stop for", "a workflow", T3McpSummaryAction::WorkflowStop),
    t3_tool("t3_background_wait", "Update", "Updating", "Updated", "background waits", T3McpSummaryAction::BackgroundWait),
    t3_tool("t3_thread_interrupt", "Interrupt", "Interrupting", "Requested an interrupt of", "a T3 thread", T3McpSummaryAction::ThreadInterrupt),
    t3_tool("t3_worktree_handoff", "Hand off", "Handing off", "Handed off", "thread to a git worktree", T3McpSummaryAction::WorktreeHandoff),
    t3_tool("t3_worktree_status", "Get", "Getting", "Got", "thread worktree status", T3McpSummaryAction::WorktreeStatus),
    browser_tool("preview_status", "Get", "Getting", "Got", "preview browser status"),
    browser_tool("preview_open", "Open", "Opening", "Opened", "a page in the preview browser"),
    browser_tool("preview_navigate", "Navigate", "Navigating", "Navigated", "the preview browser"),
    browser_tool("preview_dialog", "Respond", "Responding", "Responded", "to a preview browser dialog"),
    browser_tool("preview_snapshot", "Take a snapshot of", "Taking a snapshot of", "Took a snapshot of", "the preview page"),
    browser_tool("preview_act", "Use", "Using", "Used", "the preview browser"),
    browser_tool("preview_click", "Click", "Clicking", "Clicked", "in the preview browser"),
    browser_tool("preview_press", "Press", "Pressing", "Pressed", "a key in the preview browser"),
    browser_tool("preview_type", "Type", "Typing", "Typed", "in the preview browser"),
    browser_tool("preview_hover", "Hover", "Hovering", "Hovered", "in the preview browser"),
    browser_tool("preview_select", "Choose", "Choosing", "Chose", "an option in the preview browser"),
    browser_tool("preview_drag", "Drag", "Dragging", "Dragged", "in the preview browser"),
    browser_tool("preview_upload", "Upload", "Uploading", "Uploaded", "files to the preview browser"),
    browser_tool("preview_scroll", "Scroll", "Scrolling", "Scrolled", "the preview browser"),
    browser_tool("preview_resize", "Resize", "Resizing", "Resized", "the preview browser"),
    browser_tool("preview_evaluate", "Evaluate", "Evaluating", "Evaluated", "script in the preview browser"),
    browser_tool("preview_wait_for", "Wait", "Waiting", "Waited", "for the preview page"),
    browser_tool("preview_set_appearance", "Set", "Setting", "Set", "preview browser appearance"),
    browser_tool("preview_recording_start", "Start", "Starting", "Started", "recording the preview browser"),
    browser_tool("preview_recording_stop", "Stop", "Stopping", "Stopped", "recording the preview browser"),
    device_tool("device_list", "List", "Listing", "Listed", "simulators and emulators"),
    device_tool("device_open", "Open", "Opening", "Opened", "a device in the Device panel"),
    device_tool("device_screenshot", "Take a screenshot of", "Taking a screenshot of", "Took a screenshot of", "the device"),
    device_tool("device_close", "Close", "Closing", "Closed", "a device"),
    t3_tool("run_scheduled_task_now", "Run", "Running", "Requested a run of", "a scheduled task", T3McpSummaryAction::ScheduleRun),
    t3_tool("t3_queue", "Manage", "Managing", "Managed", "queued messages", T3McpSummaryAction::QueueEdit),
    t3_tool("t3_pending_request", "Handle", "Handling", "Handled", "pending questions", T3McpSummaryAction::QuestionRespond),
    t3_tool("t3_queue_list", "List", "Listing", "Listed", "queued messages", T3McpSummaryAction::QueueList),
    t3_tool("t3_queue_read", "Read", "Reading", "Read", "a queued message", T3McpSummaryAction::QueueRead),
    t3_tool("t3_queue_edit", "Edit", "Editing", "Edited", "a queued message", T3McpSummaryAction::QueueEdit),
    t3_tool("t3_queue_cancel", "Cancel", "Canceling", "Requested cancellation of", "a queued run", T3McpSummaryAction::QueueCancel),
    t3_tool("t3_queue_reorder", "Reorder", "Reordering", "Reordered", "a queued run", T3McpSummaryAction::QueueReorder),
    t3_tool("t3_queue_promote_to_steer", "Steer with", "Steering with", "Requested steering with", "a queued message", T3McpSummaryAction::QueueSteer),
    t3_tool("t3_pending_request_list", "List", "Listing", "Listed", "pending questions", T3McpSummaryAction::QuestionList),
    t3_tool("t3_pending_request_read", "Read", "Reading", "Read", "pending questions", T3McpSummaryAction::QuestionRead),
    t3_tool("t3_pending_request_respond", "Answer", "Answering", "Answered", "pending questions", T3McpSummaryAction::QuestionRespond),
    t3_tool("t3_thread_configuration", "Read", "Reading", "Read", "thread configuration", T3McpSummaryAction::ThreadConfiguration),
    t3_tool("t3_thread_configure", "Use", "Using", "Used", "thread model settings", T3McpSummaryAction::ThreadConfigure),
    t3_tool("t3_thread_transfer", "Use", "Using", "Used", "thread transfers", T3McpSummaryAction::ThreadFork),
    t3_tool("t3_thread_fork", "Fork", "Forking", "Requested a fork of", "this thread", T3McpSummaryAction::ThreadFork),
    t3_tool("t3_thread_merge_back", "Merge", "Merging", "Requested a merge of", "thread context", T3McpSummaryAction::ThreadMerge),
    t3_tool("t3_thread_search", "Search", "Searching", "Searched", "thread content", T3McpSummaryAction::ThreadSearch),
    t3_tool("t3_thread_transfers", "Read", "Reading", "Read", "thread transfers", T3McpSummaryAction::ThreadTransfers),
    t3_tool("t3_thread_organize", "Organize", "Organizing", "Organized", "a thread", T3McpSummaryAction::ThreadOrganize),
    t3_tool("t3_thread_update", "Update", "Updating", "Updated", "T3 thread metadata", T3McpSummaryAction::ThreadUpdate),
    t3_tool("t3_worktree_list", "List", "Listing", "Listed", "workspace branches", T3McpSummaryAction::WorktreeList),
    browser_tool("t3_preview_list", "List", "Listing", "Listed", "preview tabs"),
    browser_tool("t3_preview_close", "Close", "Closing", "Closed", "a preview tab"),
    t3_tool("t3_environment_read", "Read", "Reading", "Read", "environment preferences", T3McpSummaryAction::EnvironmentRead),
    t3_tool("t3_environment_links", "List", "Listing", "Listed", "linked environments", T3McpSummaryAction::EnvironmentLinks),
    t3_tool("t3_environment_link", "Ask to link", "Asking to link", "Asked to link", "an environment", T3McpSummaryAction::EnvironmentLink),
    t3_tool("t3_environment_unlink", "Forget", "Forgetting", "Forgot", "a linked environment", T3McpSummaryAction::EnvironmentUnlink),
    t3_tool("t3_environment_preferences_update", "Update", "Updating", "Updated", "environment preferences", T3McpSummaryAction::EnvironmentUpdate),
    t3_tool("t3_theme_read", "Read", "Reading", "Read", "themes", T3McpSummaryAction::ThemeRead),
    t3_tool("t3_theme_set", "Set", "Setting", "Set", "the theme", T3McpSummaryAction::ThemeSet),
    t3_tool("t3_thread_launch", "Launch", "Launching", "Launched", "a project thread", T3McpSummaryAction::ThreadCreate),
    t3_tool("t3_project_list", "List", "Listing", "Listed", "projects", T3McpSummaryAction::ProjectList),
    t3_tool("t3_project_read", "Read", "Reading", "Read", "a project", T3McpSummaryAction::ProjectRead),
    t3_tool("t3_project_create", "Register", "Registering", "Registered", "a project", T3McpSummaryAction::ProjectCreate),
    t3_tool("t3_project_update", "Update", "Updating", "Updated", "a project", T3McpSummaryAction::ProjectUpdate),
    t3_tool("t3_project_delete", "Delete", "Deleting", "Deleted", "a project", T3McpSummaryAction::ProjectDelete),
    t3_tool("t3_project_clone", "Clone", "Cloning", "Cloned", "a repository", T3McpSummaryAction::ProjectClone),
    t3_tool("t3_attachment_prepare_upload", "Prepare", "Preparing", "Prepared", "an attachment upload", T3McpSummaryAction::AttachmentPrepare),
    t3_tool("t3_attachment_discard", "Discard", "Discarding", "Discarded", "a pending attachment", T3McpSummaryAction::AttachmentDiscard),
    t3_tool("t3_thread_send_attachments", "Send", "Sending", "Sent", "attachments", T3McpSummaryAction::AttachmentSend),
    t3_tool("html_preview", "Preview", "Previewing", "Previewed", "an HTML page", T3McpSummaryAction::HtmlPreview),
    t3_tool("html_render", "Render", "Rendering", "Rendered", "an HTML page", T3McpSummaryAction::HtmlRender),
    t3_tool("send_to_user", "Send", "Sending", "Sent", "a message", T3McpSummaryAction::SendToUser),
];

fn lookup_tool(name: &str) -> Option<&'static T3McpToolDefinition> {
    T3_MCP_TOOLS.iter().find(|entry| entry.name == name)
}

fn is_t3_server_alias(server: &str) -> bool {
    matches!(server.to_ascii_lowercase().as_str(), "t3-code" | "t3_code" | "t3code" | "t3-code-direct" | "t3_code_direct")
}

fn normalize_label(value: &str) -> &str {
    let trimmed = value.trim_end();
    let suffix_len = if trimmed.len() >= 9 && trimmed.is_char_boundary(trimmed.len() - 9) && trimmed[trimmed.len() - 9..].eq_ignore_ascii_case("completed") {
        Some(9)
    } else if trimmed.len() >= 8 && trimmed.is_char_boundary(trimmed.len() - 8) && trimmed[trimmed.len() - 8..].eq_ignore_ascii_case("complete") {
        Some(8)
    } else {
        None
    };
    if let Some(len) = suffix_len {
        let before = &trimmed[..trimmed.len() - len];
        if before.ends_with(char::is_whitespace) {
            return before.trim();
        }
    }
    value.trim()
}

fn strip_namespace_prefix(label: &str) -> Option<&str> {
    let bytes = label.as_bytes();
    if bytes.len() < 7 || !bytes[..2].eq_ignore_ascii_case(b"t3") {
        return None;
    }
    let sep = bytes[2];
    let rest_offset = if sep == b'-' || sep == b'_' { 3 } else { 2 };
    let rest = &bytes[rest_offset..];
    if rest.len() < 5 || !rest[..4].eq_ignore_ascii_case(b"code") {
        return None;
    }
    let after_code_offset = rest_offset + 4;
    let after_code = &bytes[after_code_offset..];
    let after_server_offset = if (sep == b'-' || sep == b'_')
        && after_code.len() >= 8
        && matches!(after_code[0], b'-' | b'_')
        && after_code[1..7].eq_ignore_ascii_case(b"direct")
    {
        after_code_offset + 7
    } else if sep == b'-' || sep == b'_' || (bytes.len() >= 6 && bytes[2..6].eq_ignore_ascii_case(b"code")) {
        after_code_offset
    } else {
        return None;
    };
    let after_server = &label[after_server_offset..];
    if let Some(tool) = after_server.strip_prefix(['.', ':', '/']) {
        return (!tool.is_empty() && !tool.contains('\n')).then_some(tool);
    }
    let ws_stripped = after_server.trim_start_matches(char::is_whitespace);
    if let Some(after_dot) = ws_stripped.strip_prefix('·') {
        let tool = after_dot.trim_start_matches(char::is_whitespace);
        return (!tool.is_empty() && !tool.contains('\n')).then_some(tool);
    }
    None
}

fn strip_loose_t3_prefix(label: &str) -> Option<&str> {
    let mut rest = label;
    if rest.as_bytes().get(..3).is_some_and(|b| b.eq_ignore_ascii_case(b"mcp")) {
        let after_mcp = &rest[3..];
        let sep_count = after_mcp.bytes().take(2).take_while(|b| *b == b'-' || *b == b'_').count();
        if sep_count == 0 {
            return None;
        }
        rest = &after_mcp[sep_count..];
    }
    if !rest.as_bytes().get(..2).is_some_and(|b| b.eq_ignore_ascii_case(b"t3")) {
        return None;
    }
    rest = &rest[2..];
    if matches!(rest.as_bytes().first(), Some(b'-' | b'_' | b' ')) {
        rest = &rest[1..];
    }
    if !rest.as_bytes().get(..4).is_some_and(|b| b.eq_ignore_ascii_case(b"code")) {
        return None;
    }
    rest = &rest[4..];
    let tool = if let Some(after_double) = rest.strip_prefix("__") {
        after_double
    } else {
        rest.strip_prefix(['-', '_', '.', ':', '/', ' '])?
    };
    (!tool.is_empty() && !tool.contains('\n')).then_some(tool)
}

fn parse_t3_mcp_tool_name(value: &str) -> Option<String> {
    let label = normalize_label(value);
    if label.as_bytes().get(..5).is_some_and(|b| b.eq_ignore_ascii_case(b"mcp__")) && !label.contains('\n') {
        let after = &label[5..];
        if let Some(split) = after.char_indices().skip(1).find_map(|(i, _)| after[i..].starts_with("__").then_some(i)) {
            let server = &after[..split];
            let tool = &after[split + 2..];
            if !tool.is_empty() && is_t3_server_alias(server) {
                return Some(tool.to_owned());
            }
            return None;
        }
    }

    if let Some(tool) = strip_namespace_prefix(label) {
        return Some(tool.to_owned());
    }

    let candidate = strip_loose_t3_prefix(label).unwrap_or(label);
    if lookup_tool(candidate).is_some() {
        return Some(candidate.to_owned());
    }

    if label.as_bytes().get(..8).is_some_and(|b| b.eq_ignore_ascii_case(b"t3-code-")) {
        let mut longest: Option<&'static str> = None;
        for entry in T3_MCP_TOOLS {
            if let Some(prefix) = label.strip_suffix(entry.name)
                && prefix.ends_with('_')
                && longest.is_none_or(|best| entry.name.len() > best.len())
            {
                longest = Some(entry.name);
            }
        }
        return longest.map(str::to_owned);
    }

    None
}

fn tool_name_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn resolve_t3_mcp_tool_name(value: &str) -> Option<String> {
    if value.len() > TOOL_NAME_CACHE_MAX_KEY_LENGTH {
        return parse_t3_mcp_tool_name(value);
    }
    let cache = tool_name_cache();
    if let Ok(guard) = cache.lock()
        && let Some(cached) = guard.get(value)
    {
        return cached.clone();
    }
    let parsed = parse_t3_mcp_tool_name(value);
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= TOOL_NAME_CACHE_LIMIT {
            guard.clear();
        }
        guard.insert(value.to_owned(), parsed.clone());
    }
    parsed
}

#[cfg(test)]
fn resolve_t3_mcp_tool_id(tool_name: Option<&str>) -> Option<&'static str> {
    let name = resolve_t3_mcp_tool_name(tool_name?)?;
    lookup_tool(&name).map(|entry| entry.name)
}

fn action_override(tool_name: &str, action: &str) -> Option<&'static str> {
    let resolved = match tool_name {
        "preview_act" => lookup_tool(&format!("preview_{action}")),
        "t3_queue" => lookup_tool(&format!("t3_queue_{action}")),
        "t3_pending_request" => lookup_tool(&format!("t3_pending_request_{action}")),
        "t3_thread_transfer" => match action {
            "fork" => lookup_tool("t3_thread_fork"),
            "merge_back" => lookup_tool("t3_thread_merge_back"),
            "status" => lookup_tool("t3_thread_transfers"),
            _ => None,
        },
        _ => None,
    };
    resolved.map(|entry| entry.name)
}

pub fn resolve_t3_mcp_tool_definition(tool_name: Option<&str>, input: Option<&Value>) -> Option<&'static T3McpToolDefinition> {
    let name = resolve_t3_mcp_tool_name(tool_name?)?;
    let base = lookup_tool(&name)?;
    if let Some(action) = input.and_then(Value::as_object).and_then(|obj| obj.get("action")).and_then(Value::as_str)
        && let Some(overridden) = action_override(base.name, action)
        && let Some(def) = lookup_tool(overridden)
    {
        return Some(def);
    }
    Some(base)
}

pub fn resolve_t3_mcp_summary_action(tool_name: Option<&str>, input: Option<&Value>) -> Option<T3McpSummaryAction> {
    let raw_name = tool_name?;
    let definition = resolve_t3_mcp_tool_definition(Some(raw_name), input)?;
    let parsed_name = resolve_t3_mcp_tool_name(raw_name);
    if let Some(fields) = input.and_then(Value::as_object) {
        match parsed_name.as_deref() {
            Some("t3_thread_configure") => {
                return Some(if fields.contains_key("modelSelection") {
                    T3McpSummaryAction::ThreadConfigure
                } else {
                    T3McpSummaryAction::ThreadConfiguration
                });
            }
            Some("schedule_task") => {
                return Some(if fields.contains_key("scheduledTaskId") {
                    T3McpSummaryAction::ScheduleUpdate
                } else {
                    T3McpSummaryAction::ScheduleCreate
                });
            }
            _ => {}
        }
    }
    Some(definition.summary_action)
}

pub fn pr_short_label_and_number(input: Option<&Map<String, Value>>) -> Option<(&'static str, u64)> {
    let input = input?;
    if let Some(url) = input.get("url").and_then(Value::as_str)
        && let Some(parsed) = parse_change_request_url(url)
    {
        let short_label = if change_request_provider(url) == Some("gitlab") { "MR" } else { "PR" };
        return Some((short_label, parsed.number));
    }
    let number = input.get("number").and_then(|value| {
        value.as_u64().filter(|n| *n > 0).or_else(|| {
            value.as_f64().filter(|n| n.is_finite() && *n > 0.0 && n.fract() == 0.0 && *n <= 9_007_199_254_740_991.0).map(|n| n as u64)
        })
    })?;
    Some(("PR", number))
}

fn tag_indicates_failure(tag: &str) -> bool {
    tag.ends_with("Error") || tag.ends_with("Failure")
}

fn record_failed(map: &Map<String, Value>) -> bool {
    map.get("isError") == Some(&Value::Bool(true))
        || map.get("is_error") == Some(&Value::Bool(true))
        || map.get("_tag").and_then(Value::as_str).is_some_and(tag_indicates_failure)
        || map.get("error").is_some_and(|v| !v.is_null())
}

enum ParsedResult {
    None,
    FailedOnly,
    Map(Map<String, Value>, bool),
}

fn read_result_owned(value: &Value, depth: usize) -> ParsedResult {
    if depth > 4 {
        return ParsedResult::None;
    }
    match value {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(parsed) => read_result_owned(&parsed, depth + 1),
            Err(_) => ParsedResult::None,
        },
        Value::Array(items) => {
            let mut found_map: Option<Map<String, Value>> = None;
            let mut failed = false;
            for block in items {
                let Some(obj) = block.as_object() else { continue };
                let text = obj.get("text").or_else(|| obj.get("content").and_then(Value::as_object).and_then(|c| c.get("text")));
                let inner = text.and_then(|v| v.as_object().and_then(|o| o.get("text")).or(Some(v)));
                if let Some(target) = inner {
                    match read_result_owned(target, depth + 1) {
                        ParsedResult::None => {}
                        ParsedResult::FailedOnly => failed = true,
                        ParsedResult::Map(map, f) => {
                            if found_map.is_none() {
                                found_map = Some(map);
                            }
                            failed |= f;
                        }
                    }
                }
            }
            match found_map {
                Some(map) => ParsedResult::Map(map, failed),
                None if failed => ParsedResult::FailedOnly,
                None => ParsedResult::None,
            }
        }
        Value::Object(map) => {
            let failed = record_failed(map);
            if let Some(content) = map.get("structuredContent").or_else(|| map.get("content")) {
                match read_result_owned(content, depth + 1) {
                    ParsedResult::None => {
                        if failed {
                            ParsedResult::FailedOnly
                        } else {
                            ParsedResult::None
                        }
                    }
                    ParsedResult::FailedOnly => ParsedResult::FailedOnly,
                    ParsedResult::Map(inner, inner_failed) => ParsedResult::Map(inner, failed || inner_failed),
                }
            } else {
                ParsedResult::Map(map.clone(), failed)
            }
        }
        _ => ParsedResult::None,
    }
}

pub fn unwrap_t3_tool_input(value: Option<&Value>) -> Option<Map<String, Value>> {
    let input = match value.map(|v| read_result_owned(v, 0)) {
        Some(ParsedResult::Map(map, _)) => map,
        _ => return None,
    };
    if input.get("toolName").is_some_and(Value::is_string) {
        return input.get("args").and_then(Value::as_object).cloned();
    }
    Some(input)
}

/// MCP errors can be returned as data even when the provider completed the tool call.
pub fn t3_tool_result_indicates_failure(output: Option<&Value>) -> bool {
    let Some(output) = output else { return false };
    match read_result_owned(output, 0) {
        ParsedResult::None => false,
        ParsedResult::FailedOnly => true,
        ParsedResult::Map(_, failed) => failed,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum T3ToolOutcome {
    Completed,
    Failed,
    Unfinished,
}

pub struct T3ToolSummaryCall<'a> {
    pub input: Option<&'a Value>,
    pub output: Option<&'a Value>,
    pub outcome: T3ToolOutcome,
}

struct EvaluatedCall {
    input: Option<Map<String, Value>>,
    output: Option<Map<String, Value>>,
    outcome: T3ToolOutcome,
}

fn non_empty_id(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

fn count_entities<'a>(ids: impl Iterator<Item = Option<&'a str>>) -> usize {
    let mut unique = HashSet::new();
    let mut missing = 0;
    for item in ids {
        match item {
            Some(id) => {
                unique.insert(id);
            }
            None => missing += 1,
        }
    }
    unique.len() + missing
}

fn quantity(count: usize, noun: &str) -> String {
    format!("{count} {}", if count == 1 { noun.to_owned() } else { format!("{noun}s") })
}

fn quantity_plural(count: usize, noun: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { noun } else { plural })
}

fn pull_request_quantity(calls: &[&EvaluatedCall], count_key: &str) -> String {
    let mut count: u64 = 0;
    for call in calls {
        if let Some(reported) = call.output.as_ref().and_then(|o| o.get(count_key)).and_then(Value::as_u64) {
            count += reported;
        } else if call.input.as_ref().is_some_and(|i| i.get("url").is_some_and(|v| !v.is_null()) || i.get("number").is_some_and(|v| !v.is_null())) {
            count += 1;
        } else {
            return "pull requests".to_owned();
        }
    }
    format!("{count} {}", if count == 1 { "pull request" } else { "pull requests" })
}

fn collect_entity_ids<'a>(selected: &'a [&'a EvaluatedCall], key: &'static str) -> Vec<Option<&'a str>> {
    selected
        .iter()
        .map(|call| {
            non_empty_id(call.output.as_ref().and_then(|o| o.get(key)))
                .or_else(|| non_empty_id(call.input.as_ref().and_then(|i| i.get(key))))
        })
        .collect()
}

pub fn summarize_t3_tool_calls(action: T3McpSummaryAction, calls: &[T3ToolSummaryCall<'_>]) -> (String, usize) {
    let evaluated: Vec<EvaluatedCall> = calls
        .iter()
        .map(|call| {
            let (output, output_failed) = match call.output.map(|v| read_result_owned(v, 0)) {
                Some(ParsedResult::Map(map, failed)) => (Some(map), failed),
                Some(ParsedResult::FailedOnly) => (None, true),
                _ => (None, false),
            };
            EvaluatedCall {
                input: unwrap_t3_tool_input(call.input),
                output,
                outcome: if output_failed { T3ToolOutcome::Failed } else { call.outcome },
            }
        })
        .collect();

    let completed: Vec<&EvaluatedCall> = evaluated.iter().filter(|c| c.outcome == T3ToolOutcome::Completed).collect();
    let mut failed_count = evaluated.iter().filter(|c| c.outcome == T3ToolOutcome::Failed).count();
    let all_refs: Vec<&EvaluatedCall> = evaluated.iter().collect();
    let selected: &[&EvaluatedCall] = if !completed.is_empty() { &completed } else { &all_refs };
    let times = quantity(selected.len(), "time");
    let any_completed = !completed.is_empty();
    let phrase = |past: &str, infinitive: &str, object: &str| {
        if any_completed { format!("{past} {object}") } else { format!("Tried to {infinitive} {object}") }
    };
    let entity_ids = |key: &'static str| collect_entity_ids(selected, key).into_iter();
    let project_ids = || {
        selected.iter().map(|call| {
            non_empty_id(call.output.as_ref().and_then(|o| o.get("id")))
                .or_else(|| non_empty_id(call.output.as_ref().and_then(|o| o.get("projectId"))))
                .or_else(|| non_empty_id(call.input.as_ref().and_then(|i| i.get("projectId"))))
        })
    };
    let thread_ids = || -> Vec<Option<&str>> {
        selected
            .iter()
            .map(|call| {
                non_empty_id(call.output.as_ref().and_then(|o| o.get("threadId")))
                    .or_else(|| non_empty_id(call.output.as_ref().and_then(|o| o.get("thread")).and_then(Value::as_object).and_then(|t| t.get("threadId"))))
                    .or_else(|| non_empty_id(call.input.as_ref().and_then(|i| i.get("threadId"))))
            })
            .collect()
    };

    use T3McpSummaryAction::*;
    let label = match action {
        ThreadSend => {
            let messages = count_entities(selected.iter().map(|c| non_empty_id(c.output.as_ref().and_then(|o| o.get("messageId")))));
            let tids = thread_ids();
            let targets_known = tids.iter().all(Option::is_some);
            let threads = tids.into_iter().collect::<HashSet<_>>().len();
            let object = if targets_known {
                if messages == threads && messages > 1 {
                    format!("messages to {}", quantity(threads, "thread"))
                } else {
                    format!("{} to {}", quantity(messages, "message"), quantity(threads, "thread"))
                }
            } else {
                quantity(messages, "message")
            };
            phrase("Sent", "send", &object)
        }
        ThreadCreate => {
            let mut created_ids = HashSet::new();
            let results_known = !completed.is_empty()
                && completed.iter().all(|call| {
                    let threads = call.output.as_ref().and_then(|o| o.get("threads")).and_then(Value::as_array);
                    let check_thread = |val: Option<&Map<String, Value>>, ids: &mut HashSet<String>| {
                        let Some(rec) = val else { return false };
                        if rec.get("status").and_then(Value::as_str) == Some("rolled_back") {
                            return true;
                        }
                        let Some(tid) = non_empty_id(rec.get("threadId")) else { return false };
                        ids.insert(tid.to_owned());
                        true
                    };
                    match threads {
                        Some(list) => list.iter().all(|t| check_thread(t.as_object(), &mut created_ids)),
                        None => check_thread(call.output.as_ref(), &mut created_ids),
                    }
                });
            if results_known {
                format!("Created {}", quantity(created_ids.len(), "thread"))
            } else {
                format!("Requested thread creation {times}")
            }
        }
        Delegate => phrase("Delegated", "delegate", &quantity(count_entities(entity_ids("taskId")), "task")),
        ThreadRead | ThreadWait => {
            let tids = thread_ids();
            let targets = if tids.iter().all(Option::is_some) {
                quantity(tids.into_iter().collect::<HashSet<_>>().len(), "thread")
            } else {
                format!("threads {times}")
            };
            if action == ThreadRead {
                phrase("Read", "read", &targets)
            } else {
                phrase("Waited on", "wait on", &targets)
            }
        }
        ThreadList => phrase("Listed", "list", &format!("threads {times}")),
        ThreadInterrupt => phrase("Requested interrupts for", "interrupt", &quantity(count_entities(thread_ids().into_iter()), "thread")),
        WorkflowStop => phrase("Requested stops for", "stop", &quantity(count_entities(entity_ids("subagentId")), "workflow")),
        TaskStatus => phrase("Checked", "check", &format!("task status {times}")),
        BackgroundWait => phrase("Updated", "update", &format!("background waits {times}")),
        TaskCancel => phrase("Requested cancellation of", "cancel", &quantity(count_entities(entity_ids("taskId")), "task")),
        ScheduleCreate => phrase("Scheduled", "schedule", &quantity(count_entities(entity_ids("scheduledTaskId")), "task")),
        ScheduleList => phrase("Listed", "list", &format!("scheduled tasks {times}")),
        ScheduleUpdate => phrase("Updated", "update", &quantity(count_entities(entity_ids("scheduledTaskId")), "scheduled task")),
        ScheduleDelete => phrase("Requested deletion of", "delete", &quantity(count_entities(entity_ids("scheduledTaskId")), "scheduled task")),
        ScheduleRun => phrase("Requested", "request", &quantity(selected.len(), "scheduled task run")),
        ThreadConfiguration => phrase("Checked", "check", &format!("thread configuration {times}")),
        ThreadConfigure => phrase("Set", "set", &format!("thread model {times}")),
        ThreadFork => phrase("Requested", "request", &quantity(selected.len(), "thread fork")),
        ThreadMerge => phrase("Requested", "request", &quantity(selected.len(), "context merge")),
        ThreadSearch => phrase("Searched", "search", &format!("threads {times}")),
        ThreadTransfers => phrase("Checked", "check", &format!("thread transfers {times}")),
        ThreadOrganize => phrase("Organized", "organize", &format!("threads {times}")),
        ThreadUpdate => phrase("Updated", "update", &quantity(count_entities(thread_ids().into_iter()), "thread")),
        QueueList => phrase("Listed", "list", &format!("queued messages {times}")),
        QueueRead => phrase("Read", "read", &quantity(count_entities(entity_ids("queuedRunId")), "queued message")),
        QueueEdit => phrase("Edited", "edit", &quantity(count_entities(entity_ids("queuedRunId")), "queued message")),
        QueueCancel => phrase("Requested cancellation of", "cancel", &quantity(count_entities(entity_ids("queuedRunId")), "queued run")),
        QueueReorder => phrase("Reordered", "reorder", &quantity(count_entities(entity_ids("queuedRunId")), "queued run")),
        QueueSteer => phrase("Requested steering with", "steer with", &quantity(count_entities(entity_ids("queuedRunId")), "queued message")),
        QuestionList => phrase("Listed", "list", &format!("pending questions {times}")),
        QuestionRead => phrase("Read", "read", &quantity(count_entities(entity_ids("requestId")), "pending question request")),
        QuestionRespond => phrase("Answered", "answer", &quantity(count_entities(entity_ids("requestId")), "pending question request")),
        SecretRequest => phrase("Asked for", "ask for", &quantity(selected.len(), "secret")),
        WorktreeHandoff => phrase("Handed off to", "hand off to", &quantity(count_entities(entity_ids("worktreePath")), "worktree")),
        WorktreeList => phrase("Listed", "list", &format!("workspace branches {times}")),
        WorktreeStatus => phrase("Checked", "check", &format!("worktree status {times}")),
        ProjectList => phrase("Listed", "list", &format!("projects {times}")),
        ProjectRead => phrase("Read", "read", &quantity(count_entities(project_ids()), "project")),
        ProjectCreate => phrase("Registered", "register", &quantity(count_entities(project_ids()), "project")),
        ProjectUpdate => phrase("Updated", "update", &quantity(count_entities(project_ids()), "project")),
        ProjectDelete => phrase("Deleted", "delete", &quantity(count_entities(project_ids()), "project")),
        ProjectClone => phrase("Cloned", "clone", &quantity_plural(count_entities(entity_ids("cwd")), "repository", "repositories")),
        EnvironmentRead => phrase("Checked", "check", &format!("environment preferences {times}")),
        EnvironmentUpdate => phrase("Updated", "update", &format!("environment preferences {times}")),
        ThemeRead => phrase("Checked", "check", &format!("themes {times}")),
        ThemeSet => phrase("Set", "set", &format!("the theme {times}")),
        EnvironmentLinks => phrase("Listed", "list", &format!("linked environments {times}")),
        EnvironmentLink => phrase("Asked to link", "ask to link", &quantity(selected.len(), "environment")),
        EnvironmentUnlink => phrase("Forgot", "forget", &quantity(count_entities(entity_ids("environmentId")), "linked environment")),
        AttachmentPrepare => phrase("Prepared", "prepare", &quantity(count_entities(entity_ids("attachmentId")), "attachment upload")),
        AttachmentDiscard => phrase("Discarded", "discard", &quantity(count_entities(entity_ids("attachmentId")), "pending attachment")),
        AttachmentSend => {
            let mut by_message: HashMap<String, &EvaluatedCall> = HashMap::new();
            for (index, call) in selected.iter().enumerate() {
                let key = non_empty_id(call.output.as_ref().and_then(|o| o.get("messageId")))
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("call-{index}"));
                by_message.insert(key, call);
            }
            let attachment_count: usize = by_message
                .values()
                .map(|call| call.input.as_ref().and_then(|i| i.get("attachments")).and_then(Value::as_array).map_or(0, Vec::len))
                .sum();
            let counts_known = by_message
                .values()
                .all(|call| call.input.as_ref().and_then(|i| i.get("attachments")).and_then(Value::as_array).is_some_and(|a| !a.is_empty()));
            let tids = thread_ids();
            let targets = if tids.iter().all(Option::is_some) {
                format!(" to {}", quantity(tids.into_iter().collect::<HashSet<_>>().len(), "thread"))
            } else {
                String::new()
            };
            let object = if counts_known {
                format!("{}{targets}", quantity(attachment_count, "attachment"))
            } else {
                format!("attachments{targets} {times}")
            };
            phrase("Sent", "send", &object)
        }
        LinkPr => phrase("Linked", "link", &pull_request_quantity(selected, "linkedCount")),
        UnlinkPr => phrase("Unlinked", "unlink", &pull_request_quantity(selected, "unlinkedCount")),
        WatchPr => phrase("Watching", "watch", &quantity(selected.len(), "pull request")),
        UnwatchPr => phrase("Stopped watching", "stop watching", &quantity(selected.len(), "pull request")),
        ListPrs => {
            let suffix = if selected.len() == 1 { String::new() } else { format!(" {times}") };
            phrase("Checked", "check", &format!("linked pull requests{suffix}"))
        }
        WritePrGuides => {
            let outcomes: Vec<Option<&str>> = selected
                .iter()
                .flat_map(|call| call.output.as_ref().and_then(|o| o.get("guides")).and_then(Value::as_array).into_iter().flatten())
                .map(|guide| guide.as_object().and_then(|g| g.get("outcome")).and_then(Value::as_str))
                .collect();
            let started = outcomes.iter().filter(|outcome| **outcome != Some("failed")).count();
            let failed = outcomes.len() - started;
            failed_count += failed;
            if outcomes.is_empty() {
                let suffix = if selected.len() == 1 { String::new() } else { format!(" {times}") };
                phrase("Started writing", "start writing", &format!("review guides{suffix}"))
            } else if started == 0 {
                format!("Could not start {}", quantity(failed, "review guide"))
            } else {
                let tail = if failed > 0 { format!(", {failed} failed") } else { String::new() };
                phrase("Started", "start", &format!("{}{tail}", quantity(started, "review guide")))
            }
        }
        ReadPrGuide => phrase("Read", "read", &quantity(selected.len(), "review guide")),
        Browser => phrase("Used", "use", &format!("browser {times}")),
        Device => phrase("Used", "use", &format!("device controls {times}")),
        HtmlPreview => phrase("Previewed", "preview", &quantity(selected.len(), "HTML page")),
        HtmlRender => phrase("Rendered", "render", &quantity(selected.len(), "HTML page")),
        SendToUser => phrase("Sent", "send", &quantity(selected.len(), "message")),
        Capabilities => phrase("Checked", "check", &format!("orchestration capabilities {times}")),
    };
    (label, failed_count)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn resolves_aliases_namespaces_and_opencode_prefixes() {
        assert_eq!(resolve_t3_mcp_tool_id(Some("mcp__t3_code__preview_open")), Some("preview_open"));
        assert_eq!(resolve_t3_mcp_tool_id(Some("mcp__t3-code-direct__t3_thread_read complete")), Some("t3_thread_read"));
        assert_eq!(resolve_t3_mcp_tool_id(Some("t3_code_direct · device_screenshot completed")), Some("device_screenshot"));
        assert_eq!(resolve_t3_mcp_tool_id(Some("t3-code-abc_123_link_pull_request")), Some("link_pull_request"));
        assert_eq!(resolve_t3_mcp_tool_id(Some("mcp__github__list_issues")), None);
        assert_eq!(resolve_t3_mcp_tool_id(Some("x".repeat(300).as_str())), None);
    }

    #[test]
    fn resolves_action_tools_and_dynamic_summary_actions() {
        assert_eq!(resolve_t3_mcp_tool_definition(Some("preview_act"), Some(&json!({ "action": "snapshot" }))).map(|d| d.name), Some("preview_snapshot"));
        assert_eq!(resolve_t3_mcp_tool_definition(Some("t3_thread_transfer"), Some(&json!({ "action": "status" }))).map(|d| d.name), Some("t3_thread_transfers"));
        assert_eq!(resolve_t3_mcp_summary_action(Some("t3_thread_configure"), Some(&json!({}))), Some(T3McpSummaryAction::ThreadConfiguration));
        assert_eq!(resolve_t3_mcp_summary_action(Some("t3_thread_configure"), Some(&json!({ "modelSelection": null }))), Some(T3McpSummaryAction::ThreadConfigure));
        assert_eq!(resolve_t3_mcp_summary_action(Some("schedule_task"), Some(&json!({ "scheduledTaskId": "s1" }))), Some(T3McpSummaryAction::ScheduleUpdate));
    }

    #[test]
    fn detects_mcp_json_and_envelope_failures() {
        assert!(t3_tool_result_indicates_failure(Some(&json!({ "isError": true }))));
        assert!(t3_tool_result_indicates_failure(Some(&json!({ "content": [{ "type": "text", "text": "{\"_tag\":\"ScheduleValidationError\"}" }] }))));
        assert!(t3_tool_result_indicates_failure(Some(&json!({ "structuredContent": { "error": "boom" } }))));
        assert!(!t3_tool_result_indicates_failure(Some(&json!({ "content": [{ "type": "text", "text": "{\"ok\":true}" }] }))));
    }

    #[test]
    fn summarizes_t3_tool_calls_with_parity() {
        let send_a = json!({ "threadId": "t1" });
        let out_a = json!({ "messageId": "m1", "threadId": "t1" });
        let (label, failed) = summarize_t3_tool_calls(T3McpSummaryAction::ThreadSend, &[T3ToolSummaryCall {
            input: Some(&send_a),
            output: Some(&out_a),
            outcome: T3ToolOutcome::Completed,
        }]);
        assert_eq!(label, "Sent 1 message to 1 thread");
        assert_eq!(failed, 0);

        let guides_out = json!({ "guides": [{ "outcome": "started" }, { "outcome": "failed" }] });
        let (label, failed) = summarize_t3_tool_calls(T3McpSummaryAction::WritePrGuides, &[T3ToolSummaryCall {
            input: None,
            output: Some(&guides_out),
            outcome: T3ToolOutcome::Completed,
        }]);
        assert_eq!(label, "Started 1 review guide, 1 failed");
        assert_eq!(failed, 1);

        assert_eq!(resolve_t3_mcp_tool_definition(Some("t3_theme_read"), None).map(|d| d.summary_action), Some(T3McpSummaryAction::ThemeRead));
        assert_eq!(resolve_t3_mcp_tool_definition(Some("t3_theme_set"), None).map(|d| d.summary_action), Some(T3McpSummaryAction::ThemeSet));
        assert_eq!(resolve_t3_mcp_tool_definition(Some("send_to_user"), None).map(|d| d.summary_action), Some(T3McpSummaryAction::SendToUser));
        assert_eq!(summarize_t3_tool_calls(T3McpSummaryAction::ThemeSet, &[T3ToolSummaryCall { input: None, output: None, outcome: T3ToolOutcome::Completed }]).0, "Set the theme 1 time");
        assert_eq!(summarize_t3_tool_calls(T3McpSummaryAction::SendToUser, &[T3ToolSummaryCall { input: None, output: None, outcome: T3ToolOutcome::Completed }]).0, "Sent 1 message");
    }
}
