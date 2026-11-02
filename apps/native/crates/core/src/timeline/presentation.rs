//! Work-log labels, actions and status: `packages/client-runtime/src/work-log/presentation.ts`,
//! the label helpers of `MessagesTimeline.logic.ts` and icons from
//! `apps/web/src/components/chat/TimelineWorkEntryRow.logic.ts`. T3's own MCP tool labels
//! (`t3McpToolPresentation.ts`) are not ported yet; those calls use the generic labels.

use std::collections::BTreeSet;

use serde_json::Value;

use super::tool_activity::{
    ToolActivityAction, classify_tool_activity, claude_skill_invocation, collect_tool_file_paths, command_display_text,
    command_program_name, dynamic_tool_title, format_read_tool_label, format_search_tool_label, is_javascript_repl_tool,
    tool_output_indicates_failure,
};
use super::{SourceActivity, Tone, ToolStatus, WorkLogEntry};
use crate::turn_item::TurnItemKind;

const TOOL_LIFECYCLE_ITEM_TYPES: [&str; 7] =
    ["command_execution", "file_change", "mcp_tool_call", "dynamic_tool_call", "collab_agent_tool_call", "web_search", "image_view"];

/// `workLogEntryIsToolLike` (`session-logic.ts`).
pub fn work_log_entry_is_tool_like(entry: &WorkLogEntry) -> bool {
    matches!(entry.tone, Tone::Tool | Tone::Thinking | Tone::Error) || entry.command.is_some() || entry.request_kind.is_some()
}

/// The stricter variant `presentation.ts` uses internally.
fn presentation_tool_like(entry: &WorkLogEntry) -> bool {
    matches!(entry.tone, Tone::Tool | Tone::Thinking | Tone::Error)
        || entry.command.as_deref().is_some_and(|c| !c.trim().is_empty())
        || entry.request_kind.is_some()
        || entry.item_type.is_some_and(|t| TOOL_LIFECYCLE_ITEM_TYPES.contains(&t))
}

fn indicates_failure(entry: &WorkLogEntry, include_command: bool) -> bool {
    if entry.tone == Tone::Error || matches!(entry.status, Some(ToolStatus::Failed | ToolStatus::Declined)) {
        return true;
    }
    if !presentation_tool_like(entry) || entry.item_type == Some("reasoning") {
        return false;
    }
    if let Some(TurnItemKind::CommandExecution { exit_code, output_indicates_failure, output, .. }) = entry.item.as_ref().map(|i| &i.kind) {
        if let Some(code) = exit_code {
            return *code != 0;
        }
        if *output_indicates_failure == Some(true) {
            return true;
        }
        if let Some(output) = output {
            let end = output.char_indices().nth(32_768).map_or(output.len(), |(i, _)| i);
            if tool_output_indicates_failure(&output[..end]) {
                return true;
            }
        }
    }
    let mut text = entry.detail.clone().unwrap_or_default();
    if include_command && let Some(command) = &entry.command {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(command);
    }
    !text.is_empty() && tool_output_indicates_failure(&text)
}

pub fn work_entry_indicates_tool_failure(entry: &WorkLogEntry) -> bool {
    indicates_failure(entry, true)
}

pub fn work_entry_display_indicates_tool_failure(entry: &WorkLogEntry) -> bool {
    indicates_failure(entry, false)
}

pub fn work_entry_indicates_tool_success(entry: &WorkLogEntry) -> bool {
    if !work_log_entry_is_tool_like(entry)
        || work_entry_indicates_tool_failure(entry)
        || (entry.tone == Tone::Thinking && entry.item_type != Some("reasoning"))
    {
        return false;
    }
    !matches!(
        entry.status,
        Some(ToolStatus::Failed | ToolStatus::Declined | ToolStatus::InProgress | ToolStatus::Stopped | ToolStatus::Idle)
    )
}

pub fn work_entry_indicates_tool_neutral_status(entry: &WorkLogEntry) -> bool {
    work_log_entry_is_tool_like(entry) && !work_entry_indicates_tool_failure(entry) && !work_entry_indicates_tool_success(entry)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ToolGroupAction {
    Read,
    Edit,
    Command,
    ThreadCreate,
    Browser,
    Device,
    CodeSearch,
    Search,
    Other,
    Update,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryKind {
    Action(ToolGroupAction),
    DynamicTool,
    Reasoning,
    AgentTool,
    ToneTool,
    Mixed,
}

pub fn normalize_compact_tool_label(value: &str) -> String {
    let trimmed = value.trim_end();
    let lower = trimmed.to_lowercase();
    for suffix in ["completed", "complete"] {
        if lower.ends_with(suffix) {
            let head = &trimmed[..trimmed.len() - suffix.len()];
            if head.ends_with(char::is_whitespace) {
                return head.trim().to_owned();
            }
        }
    }
    value.trim().to_owned()
}

fn is_local_code_search(entry: &WorkLogEntry) -> bool {
    entry.item_type == Some("file_search")
        || (entry.item_type == Some("web_search")
            && normalize_compact_tool_label(entry.tool_title.as_deref().unwrap_or(&entry.label))
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|word| word == "grep"))
}

fn dynamic_tool(entry: &WorkLogEntry) -> Option<(Option<&str>, &Value)> {
    match entry.item.as_ref().map(|i| &i.kind) {
        Some(TurnItemKind::DynamicTool { tool_name, input, .. }) => Some((tool_name.as_deref(), input)),
        _ => None,
    }
}

/// `toolGroupAction`.
pub fn tool_group_action(entry: &WorkLogEntry) -> ToolGroupAction {
    use ToolGroupAction::*;
    if entry.item_type == Some("thread_created") {
        return ThreadCreate;
    }
    if entry.request_kind.as_deref() == Some("file-read") || entry.viewed_image_path.is_some() {
        return Read;
    }
    if matches!(entry.item_type, Some("approval_request" | "user_input_request")) {
        return if presentation_tool_like(entry) { Other } else { Update };
    }
    let item_type = match entry.item_type {
        Some(t @ ("command_execution" | "file_change" | "web_search")) => Some(t),
        Some("dynamic_tool") => Some("dynamic_tool_call"),
        _ => None,
    };
    let tool_name = dynamic_tool(entry).and_then(|(name, _)| name).or(entry.tool_title.as_deref());
    let data = serde_json::json!({ "toolName": tool_name });
    let classified = classify_tool_activity(item_type, None, Some(&data));
    if classified == ToolActivityAction::Read {
        return Read;
    }
    if classified == ToolActivityAction::FileChange || entry.item_type == Some("file_change") {
        return Edit;
    }
    if classified == ToolActivityAction::Command || entry.item_type == Some("command_execution") || entry.command.is_some() {
        return Command;
    }
    if classified == ToolActivityAction::Search {
        return if entry.item_type == Some("web_search") && !is_local_code_search(entry) { Search } else { CodeSearch };
    }
    if is_local_code_search(entry) {
        return CodeSearch;
    }
    if entry.item_type == Some("web_search") {
        return Search;
    }
    if !entry.changed_files.is_empty() {
        return Edit;
    }
    if presentation_tool_like(entry) { Other } else { Update }
}

fn action_count(action: ToolGroupAction, entries: &[&WorkLogEntry]) -> usize {
    if action != ToolGroupAction::Edit {
        return entries.len();
    }
    let mut files = BTreeSet::new();
    let mut without_files = 0;
    for entry in entries {
        if entry.changed_files.is_empty() {
            without_files += 1;
        } else {
            files.extend(entry.changed_files.iter().map(String::as_str));
        }
    }
    files.len() + without_files
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn action_label(action: ToolGroupAction, count: usize) -> String {
    use ToolGroupAction::*;
    let times = |count| plural(count, "time", "times");
    match action {
        Read => format!("Read {}", plural(count, "file", "files")),
        Edit => format!("Changed {}", plural(count, "file", "files")),
        Command => format!("Ran {}", plural(count, "command", "commands")),
        ThreadCreate => format!("Created {}", plural(count, "thread", "threads")),
        Device => format!("Used device controls {}", times(count)),
        Browser => format!("Used browser {}", times(count)),
        Search => format!("Searched the web {}", times(count)),
        CodeSearch => format!("Searched code {}", times(count)),
        Other => format!("Used {}", plural(count, "tool", "tools")),
        Update => format!("Received {}", plural(count, "update", "updates")),
    }
}

fn action_priority(action: ToolGroupAction) -> u8 {
    match action {
        ToolGroupAction::Command | ToolGroupAction::Edit | ToolGroupAction::ThreadCreate => 0,
        ToolGroupAction::Other | ToolGroupAction::Update => 2,
        _ => 1,
    }
}

fn join_sentence(labels: &[String]) -> String {
    let labels: Vec<String> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| if index == 0 { label.clone() } else { lowercase_first(label) })
        .collect();
    match labels.len() {
        0 => String::new(),
        1 | 2 => labels.join(" and "),
        n => format!("{}, and {}", labels[..n - 1].join(", "), labels[n - 1]),
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_lowercase().chain(chars).collect()).unwrap_or_default()
}

/// `summarizeToolGroup`: (summary, has_failure).
pub fn summarize_tool_group(entries: &[&WorkLogEntry]) -> (String, bool) {
    let tools: Vec<&WorkLogEntry> = entries.iter().copied().filter(|e| e.item_type != Some("reasoning")).collect();
    if !entries.is_empty() && tools.is_empty() {
        let summary = if entries.len() == 1 { "Thought".to_owned() } else { format!("Thought (×{})", entries.len()) };
        return (summary, false);
    }
    let mut groups: Vec<(ToolGroupAction, Vec<&WorkLogEntry>)> = Vec::new();
    let mut sources: Vec<(String, String, String)> = Vec::new();
    for entry in &tools {
        if let Some(source) = &entry.tool_source {
            if !sources.iter().any(|(key, _, _)| *key == source.key) {
                sources.push((source.key.clone(), source.name.clone(), source.kind.clone()));
            }
            continue;
        }
        let action = tool_group_action(entry);
        match groups.iter_mut().find(|(candidate, _)| *candidate == action) {
            Some((_, group)) => group.push(entry),
            None => groups.push((action, vec![entry])),
        }
    }
    let summaries: Vec<(usize, usize, u8, String, usize)> = groups
        .iter()
        .enumerate()
        .map(|(index, (action, group))| {
            let failed = group.iter().filter(|e| work_entry_display_indicates_tool_failure(e)).count();
            (index, group.len(), action_priority(*action), action_label(*action, action_count(*action, group)), failed)
        })
        .collect();
    let mut selected: Vec<&(usize, usize, u8, String, usize)> = summaries.iter().collect();
    selected.sort_by_key(|(index, _, priority, _, _)| (*priority, *index));
    selected.truncate(2);
    selected.sort_by_key(|(index, ..)| *index);
    let mut labels: Vec<String> = selected.iter().map(|(_, _, _, label, _)| label.clone()).collect();
    if !sources.is_empty() {
        let mut names: Vec<&str> = Vec::new();
        for (_, name, _) in &sources {
            if !names.contains(&name.as_str()) {
                names.push(name);
            }
        }
        let formatted = match names.len() {
            0 | 1 => names.first().copied().unwrap_or_default().to_owned(),
            2 => names.join(" and "),
            n => format!("{}, and {}", names[..n - 1].join(", "), names[n - 1]),
        };
        let all_integrations = sources.iter().all(|(_, _, kind)| kind == "integration");
        let suffix = if all_integrations { if names.len() == 1 { " integration" } else { " integrations" } } else { "" };
        labels.insert(0, format!("Used {formatted}{suffix}"));
    }
    let sourced = tools.iter().filter(|e| e.tool_source.is_some()).count();
    let covered: usize = selected.iter().map(|(_, count, ..)| *count).sum();
    let remaining = tools.len().saturating_sub(sourced + covered);
    if remaining > 0 {
        labels.push(format!("Performed {remaining} other {}", if remaining == 1 { "action" } else { "actions" }));
    }
    (join_sentence(&labels), summaries.iter().any(|s| s.4 > 0))
}

/// `toolGroupSummaryKind`.
pub fn tool_group_summary_kind(entries: &[&WorkLogEntry]) -> SummaryKind {
    let tools: Vec<&WorkLogEntry> = entries.iter().copied().filter(|e| e.item_type != Some("reasoning")).collect();
    if !entries.is_empty() && tools.is_empty() {
        return SummaryKind::Reasoning;
    }
    let actions: BTreeSet<ToolGroupAction> = tools.iter().map(|e| tool_group_action(e)).collect();
    if actions.len() != 1 {
        return SummaryKind::Mixed;
    }
    let Some(action) = actions.into_iter().next() else { return SummaryKind::Mixed };
    if action != ToolGroupAction::Other {
        return SummaryKind::Action(action);
    }
    let kinds: Vec<SummaryKind> = tools
        .iter()
        .map(|entry| match entry.item_type {
            Some("dynamic_tool") => SummaryKind::DynamicTool,
            Some("subagent") => SummaryKind::AgentTool,
            _ if entry.tone == Tone::Thinking => SummaryKind::AgentTool,
            _ if entry.tone == Tone::Tool => SummaryKind::ToneTool,
            _ => SummaryKind::Action(ToolGroupAction::Other),
        })
        .collect();
    match kinds.first() {
        Some(first) if kinds.iter().all(|k| k == first) => *first,
        _ => SummaryKind::Mixed,
    }
}

/// `formatWorkspaceRelativePath`, without line positions.
pub fn format_workspace_relative_path(path: &str, workspace_root: Option<&str>) -> String {
    let normalized = path.replace('\\', "/");
    let Some(root) = workspace_root.map(|r| r.replace('\\', "/").trim_end_matches('/').to_owned()).filter(|r| !r.is_empty()) else {
        return normalized;
    };
    let label = root.rsplit('/').next().unwrap_or(&root).to_owned();
    if normalized == root {
        return label;
    }
    if let Some(rest) = normalized.strip_prefix(&format!("{root}/")) {
        return format!("{label}/{rest}");
    }
    if !normalized.starts_with('/') {
        let relative = normalized.trim_start_matches("./");
        return if normalized.starts_with(&format!("{label}/")) { normalized.clone() } else { format!("{label}/{relative}") };
    }
    normalized
}

fn read_raw_paths(entry: &WorkLogEntry) -> Vec<String> {
    if !entry.changed_files.is_empty() {
        return entry.changed_files.clone();
    }
    if let Some((_, input)) = dynamic_tool(entry) {
        let paths = collect_tool_file_paths(&serde_json::json!({ "input": input }));
        if !paths.is_empty() {
            return paths;
        }
    }
    Vec::new()
}

/// `workEntryReadOutput`: absolute paths of a read, one per line.
pub fn work_entry_read_output(entry: &WorkLogEntry, workspace_root: Option<&str>) -> Option<String> {
    let mut paths: Vec<String> = Vec::new();
    for path in read_raw_paths(entry) {
        let trimmed = path.trim().replace('\\', "/");
        let full = match workspace_root {
            Some(root) if !trimmed.starts_with('/') => {
                format!("{}/{}", root.replace('\\', "/").trim_end_matches('/'), trimmed.trim_start_matches("./").trim_start_matches('/'))
            }
            _ => trimmed,
        };
        if !full.is_empty() && !paths.contains(&full) {
            paths.push(full);
        }
    }
    (!paths.is_empty()).then(|| paths.join("\n"))
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `workEntryDisplayLabel`.
pub fn work_entry_display_label(entry: &WorkLogEntry, workspace_root: Option<&str>) -> String {
    if entry.item_type == Some("system_notice") {
        return entry.label.clone();
    }
    if entry.item_type == Some("reasoning") || entry.tone == Tone::Thinking {
        let thought = entry.detail.as_deref().map(collapse_whitespace).unwrap_or_default();
        return if thought.is_empty() { entry.label.clone() } else { thought };
    }
    if let Some(command) = entry.command.as_deref().filter(|c| !c.trim().is_empty()) {
        return command_display_text(command);
    }
    let action = tool_group_action(entry);
    if matches!(action, ToolGroupAction::CodeSearch | ToolGroupAction::Search) {
        let label = if entry.item_type == Some("file_search") {
            Some(entry.label.clone())
        } else {
            entry.item.as_ref().and_then(|item| match &item.kind {
                TurnItemKind::DynamicTool { input, .. } => format_search_tool_label(&serde_json::json!({ "input": input })),
                _ => None,
            })
        };
        if let Some(label) = label {
            return label;
        }
    }
    if action == ToolGroupAction::Read {
        let paths: Vec<String> = read_raw_paths(entry).iter().map(|p| format_workspace_relative_path(p, workspace_root)).collect();
        if let Some(first) = paths.first() {
            return format_read_tool_label(first, paths.len() - 1);
        }
    }
    let provider_retry = matches!(entry.item.as_ref().map(|i| &i.kind), Some(TurnItemKind::Error { retry: Some(_), .. }));
    if let Some((tool_name, input)) = dynamic_tool(entry)
        && let Some(title) = dynamic_tool_title(tool_name, input, entry.item.as_ref().and_then(|i| i.title.as_deref()))
    {
        return title;
    }
    let detail = entry.detail.as_deref().map(str::trim).unwrap_or_default();
    let search_output = matches!(action, ToolGroupAction::CodeSearch | ToolGroupAction::Search) && detail.contains(['\r', '\n']);
    if !detail.is_empty() && !provider_retry && action != ToolGroupAction::Read && !search_output {
        return detail.to_owned();
    }
    if let Some(first) = entry.changed_files.first() {
        let path = format_workspace_relative_path(first, workspace_root);
        return if entry.changed_files.len() == 1 { path } else { format!("{path} +{} more", entry.changed_files.len() - 1) };
    }
    if action == ToolGroupAction::Read && entry.viewed_image_path.is_none() {
        return "Read file".into();
    }
    let heading = normalize_compact_tool_label(entry.tool_title.as_deref().filter(|t| !t.is_empty()).unwrap_or(&entry.label));
    let mut chars = heading.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// `singleToolCallLabel`.
pub fn single_tool_call_label(entry: &WorkLogEntry) -> String {
    if entry.item_type == Some("reasoning") {
        let thought = entry.detail.as_deref().map(collapse_whitespace).unwrap_or_default();
        return if thought.is_empty() { "Thought".into() } else { thought };
    }
    if let Some((tool_name, input)) = dynamic_tool(entry)
        && let Some(title) = dynamic_tool_title(tool_name, input, entry.item.as_ref().and_then(|i| i.title.as_deref()))
    {
        return title;
    }
    if entry.item_type == Some("web_search") {
        return entry.tool_title.clone().unwrap_or_else(|| "Web search".into());
    }
    work_entry_display_label(entry, None)
}

fn live_status(status: Option<ToolStatus>, present: bool) -> ToolStatus {
    match status {
        Some(s @ (ToolStatus::Failed | ToolStatus::Declined | ToolStatus::Stopped | ToolStatus::Idle)) => s,
        _ if present || status == Some(ToolStatus::InProgress) => ToolStatus::InProgress,
        _ => ToolStatus::Completed,
    }
}

/// `liveWorkEntryLabel`.
pub fn live_work_entry_label(entry: &WorkLogEntry, workspace_root: Option<&str>, active: bool) -> String {
    let status = live_status(entry.status, active);
    if entry.item_type == Some("reasoning") {
        let thought = entry.detail.as_deref().map(collapse_whitespace).unwrap_or_default();
        if !thought.is_empty() {
            return thought;
        }
        return if status == ToolStatus::InProgress { "Thinking".into() } else { "Thought".into() };
    }
    if let Some(command) = entry.command.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        let verb = match status {
            ToolStatus::InProgress => "Running",
            ToolStatus::Failed => "Failed",
            ToolStatus::Declined => "Declined",
            ToolStatus::Stopped => "Stopped",
            _ => "Ran",
        };
        return format!("{verb} {}", command_program_name(command).unwrap_or_else(|| "command".into()));
    }
    work_entry_display_label(entry, workspace_root)
}

/// `workEntryIsVisibleInGroup`.
pub fn work_entry_is_visible_in_group(entry: &WorkLogEntry, expanded_group: bool) -> bool {
    if entry.item_type == Some("reasoning") {
        return entry.detail.as_deref().is_some_and(|d| !d.trim().is_empty());
    }
    (expanded_group && (entry.status == Some(ToolStatus::InProgress) || entry.source_activity == Some(SourceActivity::TaskProgress)))
        || entry.status == Some(ToolStatus::Stopped)
        || !work_entry_indicates_tool_neutral_status(entry)
}

/// `liveThoughtLine`: the first bolded heading, or the first sentence, as plain text.
pub fn live_thought_line(markdown: &str) -> String {
    let first_line = markdown.trim_start_matches([' ', '\t']).lines().next().unwrap_or_default();
    let heading = first_line
        .strip_prefix("**")
        .and_then(|rest| rest.find("**").map(|end| (&rest[..end], rest[end + 2..].trim())))
        .filter(|(inner, tail)| !inner.is_empty() && !inner.contains('*') && tail.is_empty())
        .map(|(inner, _)| inner);
    let text = collapse_whitespace(&strip_inline_markdown(heading.unwrap_or(markdown)));
    if heading.is_some() {
        return text;
    }
    let chars: Vec<char> = text.chars().collect();
    for (index, c) in chars.iter().enumerate() {
        if matches!(c, '.' | '?' | '!') {
            let mut end = index + 1;
            if chars.get(end).is_some_and(|c| matches!(c, '"' | '\'' | '”' | '’' | ')')) {
                end += 1;
            }
            if chars.get(end).is_some_and(|c| c.is_whitespace()) {
                return chars[..end].iter().collect();
            }
        }
    }
    text
}

fn strip_inline_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut line = line.trim_start_matches([' ', '\t']);
        let marker_end = line.find([' ', '\t']).unwrap_or(0);
        let marker = &line[..marker_end];
        if !marker.is_empty()
            && (marker.chars().all(|c| c == '#') && marker.len() <= 6
                || matches!(marker, "-" | "*" | "+")
                || (marker.ends_with('.') && marker[..marker.len() - 1].chars().all(|c| c.is_ascii_digit()) && marker.len() > 1))
        {
            line = line[marker_end..].trim_start();
        }
        out.push_str(line);
        out.push('\n');
    }
    let mut result = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == '!' && chars.get(index + 1) == Some(&'[') {
            index += 1;
            continue;
        }
        if c == '['
            && let Some(close) = chars[index..].iter().position(|c| *c == ']').map(|p| p + index)
            && chars.get(close + 1) == Some(&'(')
            && let Some(end) = chars[close..].iter().position(|c| *c == ')').map(|p| p + close)
        {
            result.extend(&chars[index + 1..close]);
            index = end + 1;
            continue;
        }
        if matches!(c, '`' | '*' | '_' | '~') {
            index += 1;
            continue;
        }
        result.push(c);
        index += 1;
    }
    result
}

/// `contextCompactionLabel` / `contextCompactionTokenCounts`.
pub fn compaction_tokens(status: &str, before: Option<u64>, after: Option<u64>) -> Option<super::CompactionTokens> {
    if status == "running" {
        return None;
    }
    Some(super::CompactionTokens { before: super::format_tokens(before?), after: super::format_tokens(after?) })
}

pub fn context_compaction_label(status: &str, before: Option<u64>, after: Option<u64>) -> String {
    match status {
        "failed" => "Context compaction failed".into(),
        "cancelled" | "interrupted" => "Context compaction stopped".into(),
        "pending" | "running" | "waiting" => "Compacting context".into(),
        _ => match compaction_tokens(status, before, after) {
            Some(tokens) => format!("Context compacted {} → {} tokens", tokens.before, tokens.after),
            None => "Context compacted".into(),
        },
    }
}

/// The icon a work row shows: `WorkEntryIconName`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkIcon {
    Brain,
    Terminal,
    Eye,
    SquarePen,
    Globe,
    Search,
    Wrench,
    Hammer,
    Bot,
    Zap,
    Check,
    CircleAlert,
    MessageCircle,
    ListTodo,
    Code,
    Computer,
    T3,
}

pub fn summary_icon(kind: SummaryKind) -> WorkIcon {
    use ToolGroupAction as A;
    match kind {
        SummaryKind::Action(A::Read) => WorkIcon::Eye,
        SummaryKind::Action(A::Edit) => WorkIcon::SquarePen,
        SummaryKind::Action(A::Command) => WorkIcon::Terminal,
        SummaryKind::Action(A::ThreadCreate) => WorkIcon::T3,
        SummaryKind::Action(A::Browser | A::Device) => WorkIcon::Computer,
        SummaryKind::Action(A::Search) => WorkIcon::Globe,
        SummaryKind::Action(A::CodeSearch) => WorkIcon::Search,
        SummaryKind::Action(A::Other) => WorkIcon::Wrench,
        SummaryKind::DynamicTool => WorkIcon::Hammer,
        SummaryKind::Reasoning => WorkIcon::Brain,
        SummaryKind::AgentTool => WorkIcon::Bot,
        SummaryKind::ToneTool => WorkIcon::Zap,
        SummaryKind::Action(A::Update) | SummaryKind::Mixed => WorkIcon::Hammer,
    }
}

fn tone_icon(tone: Tone) -> WorkIcon {
    match tone {
        Tone::Error => WorkIcon::CircleAlert,
        Tone::Thinking => WorkIcon::Brain,
        Tone::Info => WorkIcon::Check,
        Tone::Tool => WorkIcon::Zap,
    }
}

/// `workEntryIconName`.
pub fn work_entry_icon(entry: &WorkLogEntry) -> WorkIcon {
    if let Some(TurnItemKind::Notification { outcome, source, .. }) = entry.item.as_ref().map(|i| &i.kind) {
        if outcome == "failed" {
            return WorkIcon::CircleAlert;
        }
        return match source.get("kind").and_then(Value::as_str) {
            Some("subagent" | "delegated_task") => WorkIcon::Bot,
            Some("command") => WorkIcon::Terminal,
            Some("monitor") => WorkIcon::Eye,
            Some("system") => WorkIcon::T3,
            _ => WorkIcon::Zap,
        };
    }
    if matches!(entry.item_type, Some("user_input_request" | "approval_request")) {
        return WorkIcon::MessageCircle;
    }
    if entry.item_type == Some("todo_list") {
        return WorkIcon::ListTodo;
    }
    if entry.tool_surface.is_some() {
        return WorkIcon::Computer;
    }
    if dynamic_tool(entry).is_some_and(|(name, _)| is_javascript_repl_tool(name)) {
        return WorkIcon::Code;
    }
    let action = tool_group_action(entry);
    if action != ToolGroupAction::Other {
        return summary_icon(SummaryKind::Action(action));
    }
    match entry.item_type {
        Some("dynamic_tool") => WorkIcon::Wrench,
        Some("subagent") => WorkIcon::Bot,
        _ => tone_icon(entry.tone),
    }
}

/// `skill.args` for a Claude skill call, which the row shows as its plain output.
pub fn skill_args(entry: &WorkLogEntry) -> Option<Option<String>> {
    dynamic_tool(entry).and_then(|(name, input)| claude_skill_invocation(name, input)).map(|(_, args)| args)
}
