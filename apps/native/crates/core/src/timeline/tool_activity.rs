//! Tool classification and labels: `packages/shared/src/toolActivity.ts`,
//! `toolOutputIndicatesFailure` (`packages/shared/src/toolOutput.ts`) and the common paths of
//! `packages/client-runtime/src/work-log/commandLabel.ts` (PowerShell parsing is not ported).

use serde_json::{Map, Value};

fn record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn trimmed(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolActivityAction {
    Command,
    Read,
    FileChange,
    Search,
    Other,
}

fn tool_name_token(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value.contains("__") || value.contains('.') || value.contains('/') {
        return None;
    }
    Some(value.chars().filter(|c| !matches!(c, '_' | '-') && !c.is_whitespace()).collect::<String>().to_lowercase())
}

/// `classifyToolActivity`.
pub fn classify_tool_activity(item_type: Option<&str>, request_kind: Option<&str>, data: Option<&Value>) -> ToolActivityAction {
    use ToolActivityAction::*;
    let data = data.and_then(record);
    let request_kind = request_kind.map(|kind| kind.trim().to_lowercase());
    let kind = trimmed(data.and_then(|d| d.get("kind"))).map(str::to_lowercase);
    let tool_name = tool_name_token(
        trimmed(data.and_then(|d| d.get("toolName")))
            .or_else(|| trimmed(data.and_then(|d| d.get("item")).and_then(record).and_then(|i| i.get("tool")))),
    );
    match item_type {
        Some("command_execution") => return Command,
        Some("image_view") => return Read,
        Some("file_change") => return FileChange,
        Some("web_search") => return Search,
        _ => {}
    }
    let (request_kind, kind, tool_name) = (request_kind.as_deref(), kind.as_deref(), tool_name.as_deref());
    if request_kind == Some("command") || kind == Some("execute") {
        return Command;
    }
    if request_kind == Some("file-change") || matches!(kind, Some("edit" | "move" | "delete" | "write")) {
        return FileChange;
    }
    if kind == Some("search") || matches!(tool_name, Some("find" | "grep" | "glob" | "rg" | "ls")) {
        return Search;
    }
    if request_kind == Some("file-read") || kind == Some("read") {
        return Read;
    }
    if matches!(tool_name, Some("terminal" | "bash" | "shell")) {
        return Command;
    }
    if matches!(tool_name, Some("read" | "readfile")) {
        return Read;
    }
    Other
}

const PATH_KEYS: [&str; 8] = ["path", "filePath", "file_path", "relativePath", "filename", "fileName", "newPath", "oldPath"];
const NESTED_KEYS: [&str; 7] = ["locations", "item", "input", "result", "rawInput", "data", "changes"];

fn collect_paths(value: &Value, paths: &mut Vec<String>, depth: usize) {
    if depth > 4 || paths.len() >= 8 {
        return;
    }
    if let Value::Array(entries) = value {
        for entry in entries {
            collect_paths(entry, paths, depth + 1);
            if paths.len() >= 8 {
                return;
            }
        }
        return;
    }
    let Some(object) = record(value) else { return };
    for key in PATH_KEYS {
        let Some(candidate) = trimmed(object.get(key)) else { continue };
        if paths.iter().any(|path| path == candidate) {
            continue;
        }
        paths.push(candidate.to_owned());
        if paths.len() >= 8 {
            return;
        }
    }
    for key in NESTED_KEYS {
        if let Some(nested) = object.get(key) {
            collect_paths(nested, paths, depth + 1);
            if paths.len() >= 8 {
                return;
            }
        }
    }
}

/// `collectToolFilePaths`.
pub fn collect_tool_file_paths(data: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    collect_paths(data, &mut paths, 0);
    paths
}

const SEARCH_QUERY_KEYS: [&str; 6] = ["pattern", "query", "searchTerm", "regex", "grep", "needle"];
const SEARCH_GLOB_KEYS: [&str; 6] = ["glob", "globPattern", "glob_pattern", "include", "filePattern", "file_pattern"];
const SEARCH_TARGET_KEYS: [&str; 6] = ["path", "target_directory", "targetDirectory", "directory", "cwd", "root"];

/// `formatSearchToolLabel`.
pub fn format_search_tool_label(data: &Value) -> Option<String> {
    let data_record = record(data);
    let input = [
        data_record.and_then(|d| d.get("rawInput")),
        data_record.and_then(|d| d.get("input")),
        data_record.and_then(|d| d.get("item")).and_then(record).and_then(|i| i.get("input")),
    ]
    .into_iter()
    .flatten()
    .filter_map(record)
    .find(|r| !r.is_empty())
    .or(data_record);
    let first = |keys: &[&str]| keys.iter().find_map(|key| trimmed(input.and_then(|i| i.get(*key))));
    let query = first(&SEARCH_QUERY_KEYS);
    let glob = first(&SEARCH_GLOB_KEYS);
    let target = first(&SEARCH_TARGET_KEYS)
        .and_then(|value| value.split(['/', '\\']).rfind(|part| !part.is_empty() && *part != "."));
    match (query, glob, target) {
        (Some(query), _, Some(target)) => Some(format!("Searched {query} in {target}")),
        (None, Some(glob), Some(target)) => Some(format!("Searched files {glob} in {target}")),
        (None, Some(glob), None) => Some(format!("Searched files {glob}")),
        (Some(query), _, None) => Some(format!("Searched {query}")),
        (None, None, Some(target)) => Some(format!("Searched in {target}")),
        (None, None, None) => None,
    }
}

/// `formatReadToolLabel`.
pub fn format_read_tool_label(path: &str, extra: usize) -> String {
    let suffix = if extra > 0 { format!(" +{extra} more") } else { String::new() };
    match path.trim() {
        "" => format!("Read file{suffix}"),
        path => format!("Read {path}{suffix}"),
    }
}

fn is_javascript_repl(tool_name: Option<&str>) -> bool {
    matches!(tool_name, Some("js" | "cua_repl.js" | "node_repl.js"))
}

pub fn is_javascript_repl_tool(tool_name: Option<&str>) -> bool {
    is_javascript_repl(tool_name)
}

/// `claudeSkillInvocation`: (name, args).
pub fn claude_skill_invocation(tool_name: Option<&str>, input: &Value) -> Option<(String, Option<String>)> {
    if tool_name != Some("Skill") {
        return None;
    }
    let input = record(input)?;
    let name = trimmed(input.get("skill"))?;
    Some((name.to_owned(), trimmed(input.get("args")).map(str::to_owned)))
}

/// `dynamicToolSurface`.
pub fn dynamic_tool_surface(tool_name: Option<&str>, input: &Value) -> Option<&'static str> {
    if tool_name == Some("cua_repl.js") {
        return Some("computer");
    }
    let code = record(input).and_then(|i| i.get("code")).and_then(Value::as_str)?;
    let uses_sky = code.contains("sky.") || code.contains("sky .") || code.contains("\"@oai/sky\"") || code.contains("'@oai/sky'");
    (is_javascript_repl(tool_name) && uses_sky).then_some("computer")
}

/// `dynamicToolTitle`.
pub fn dynamic_tool_title(tool_name: Option<&str>, input: &Value, retained_title: Option<&str>) -> Option<String> {
    if is_javascript_repl(tool_name) {
        let meaningful = retained_title.map(str::trim).filter(|title| {
            !title.is_empty() && !title.eq_ignore_ascii_case("js") && !tool_name.is_some_and(|name| title.eq_ignore_ascii_case(name))
        });
        let from_input = trimmed(record(input).and_then(|i| i.get("title")));
        return Some(
            from_input
                .or(meaningful)
                .map(str::to_owned)
                .unwrap_or_else(|| if dynamic_tool_surface(tool_name, input) == Some("computer") { "Computer use".into() } else { "Run JavaScript".into() }),
        );
    }
    claude_skill_invocation(tool_name, input).map(|(name, _)| format!("Skill: {name}"))
}

/// `toolOutputIndicatesFailure`.
pub fn tool_output_indicates_failure(text: &str) -> bool {
    let lower = text.to_lowercase();
    const PHRASES: [&str; 8] = [
        "file not found",
        "no files found",
        "enoent",
        "no such file",
        "commandnotfoundexception",
        "command not found",
        "is not recognized as the name of a cmdlet",
        "a parameter cannot be found that matches parameter name",
    ];
    if PHRASES.iter().any(|phrase| lower.contains(phrase)) {
        return true;
    }
    if lower.contains("cannot find path") && lower.contains("because it does not exist") {
        return true;
    }
    if lower.contains("is not recognized") && lower.contains("the term '") {
        return true;
    }
    nonzero_after(&lower, "exit code")
}

/// Matches `exit code 2`, `exit code: 2` and `<exited with exit code 2>`, but not code 0.
fn nonzero_after(text: &str, marker: &str) -> bool {
    let mut rest = text;
    while let Some(index) = rest.find(marker) {
        let after = &rest[index + marker.len()..];
        let digits = after.trim_start_matches([' ', '\t', ':']);
        let separated = digits.len() < after.len();
        let number: String = digits.chars().take_while(char::is_ascii_digit).collect();
        if separated && !number.is_empty() && !number.starts_with('0') {
            return true;
        }
        rest = after;
    }
    false
}

const SHELL_PROGRAMS: [&str; 7] = ["sh", "bash", "zsh", "dash", "ash", "ksh", "fish"];
const SETUP_PROGRAMS: [&str; 10] = ["cd", "pushd", "popd", "source", ".", "export", "unset", "set", "alias", "setopt"];
const SYNTAX_PROGRAMS: [&str; 16] =
    ["if", "then", "for", "while", "until", "case", "function", "[[", "[", "test", "{", "(", "((", "true", "return", "repeat"];
const WRAPPERS: [&str; 8] = ["sudo", "env", "timeout", "nice", "nohup", "time", "command", "exec"];

/// Shell-style tokens. Operators (`&&`, `||`, `;`, `|`, newline) are their own tokens.
/// `None` when quoting does not balance.
fn tokenize(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                has_token = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        other => current.push(other),
                    }
                }
            }
            '"' => {
                has_token = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => current.push(chars.next()?),
                        other => current.push(other),
                    }
                }
            }
            '\\' => {
                has_token = true;
                match chars.next() {
                    Some('\n') | None => {}
                    Some(next) => current.push(next),
                }
            }
            '`' => {
                has_token = true;
                current.push(c);
                loop {
                    let next = chars.next()?;
                    current.push(next);
                    if next == '`' {
                        break;
                    }
                }
            }
            '$' | '<' | '>' if matches!(chars.peek(), Some('(' | '{')) && (c == '$' || chars.peek() == Some(&'(')) => {
                has_token = true;
                current.push(c);
                let open = chars.next()?;
                let close = if open == '(' { ')' } else { '}' };
                current.push(open);
                let mut level = 1;
                while level > 0 {
                    let next = chars.next()?;
                    if next == open {
                        level += 1;
                    } else if next == close {
                        level -= 1;
                    }
                    current.push(next);
                }
            }
            '#' if !has_token => {
                while chars.peek().is_some_and(|next| *next != '\n') {
                    chars.next();
                }
            }
            ' ' | '\t' | '\r' => {
                if has_token {
                    tokens.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            '\n' | ';' | '|' | '&' => {
                if has_token {
                    tokens.push(std::mem::take(&mut current));
                    has_token = false;
                }
                let mut operator = c.to_string();
                if matches!(c, '|' | '&') && chars.peek() == Some(&c) {
                    chars.next();
                    operator.push(c);
                }
                tokens.push(operator);
            }
            other => {
                has_token = true;
                current.push(other);
            }
        }
    }
    if has_token {
        tokens.push(current);
    }
    Some(tokens)
}

fn is_operator(token: &str) -> bool {
    matches!(token, "\n" | ";" | "|" | "||" | "&" | "&&")
}

fn is_assignment(token: &str) -> bool {
    let Some(index) = token.find('=') else { return false };
    let name = token[..index].trim_end_matches('+');
    !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn program_basename(token: &str) -> String {
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
    base.strip_suffix(".exe").or_else(|| base.strip_suffix(".EXE")).unwrap_or(base).to_owned()
}

/// The script of `bash -lc '<script>'` when it is the last argument.
fn shell_script(tokens: &[String]) -> Option<&str> {
    let mut index = 1;
    while let Some(token) = tokens.get(index) {
        if token == "-o" || token == "-O" || token == "--rcfile" || token == "--init-file" {
            index += 2;
            continue;
        }
        if token.starts_with('-') && !token.starts_with("--") && token.contains('c') {
            return (index + 2 == tokens.len()).then(|| tokens[index + 1].as_str());
        }
        if !token.starts_with('-') {
            return None;
        }
        index += 1;
    }
    None
}

fn first_program(tokens: &[String], depth: usize) -> Option<String> {
    let mut segment_start = 0;
    while segment_start < tokens.len() {
        let end = tokens[segment_start..].iter().position(|t| is_operator(t)).map_or(tokens.len(), |i| segment_start + i);
        let segment = &tokens[segment_start..end];
        segment_start = end + 1;
        let mut index = 0;
        while let Some(token) = segment.get(index) {
            if is_assignment(token) {
                index += 1;
                continue;
            }
            let name = program_basename(token);
            if WRAPPERS.contains(&name.as_str()) {
                index += 1;
                while let Some(arg) = segment.get(index) {
                    if arg.starts_with('-') {
                        index += if matches!(arg.as_str(), "-u" | "-g" | "-n" | "-s" | "-k") { 2 } else { 1 };
                    } else if is_assignment(arg) || (name == "timeout" && arg.chars().next().is_some_and(|c| c.is_ascii_digit())) {
                        index += 1;
                    } else {
                        break;
                    }
                }
                continue;
            }
            if name == "arch" {
                index += 1;
                while segment.get(index).is_some_and(|arg| arg.starts_with('-')) {
                    index += 2;
                }
                continue;
            }
            if name == "script" {
                index += 1;
                while segment.get(index).is_some_and(|arg| arg.starts_with('-')) {
                    index += 1;
                }
                index += 1;
                continue;
            }
            if SHELL_PROGRAMS.contains(&name.as_str()) && segment.len() > index + 1 {
                if depth < 3 {
                    return shell_script(&segment[index..]).and_then(|script| command_program_name_at(script, depth + 1));
                }
                return None;
            }
            if SETUP_PROGRAMS.contains(&name.as_str()) {
                break;
            }
            if SYNTAX_PROGRAMS.contains(&token.as_str()) || token.ends_with("()") {
                return None;
            }
            let variable_path = token.starts_with('$') && token.contains('/');
            if token.starts_with(['<', '>', '(', ')', '{', '}', '[', ']', '$', '`', '!', '%', '@', ':']) && !variable_path {
                return None;
            }
            return Some(name).filter(|name| !name.is_empty());
        }
    }
    None
}

fn command_program_name_at(command: &str, depth: usize) -> Option<String> {
    first_program(&tokenize(command)?, depth)
}

/// `commandProgramName`: the first program a shell command runs, without executing it.
pub fn command_program_name(command: &str) -> Option<String> {
    command_program_name_at(command, 0)
}

/// `commandDisplayText`: drops a `bash -lc '…'` wrapper and shows control characters.
pub fn command_display_text(command: &str) -> String {
    let trimmed = command.trim();
    let unwrapped = tokenize(trimmed)
        .filter(|tokens| !tokens.iter().any(|t| is_operator(t)))
        .and_then(|tokens| {
            let program = program_basename(tokens.first()?);
            if !SHELL_PROGRAMS.contains(&program.as_str()) {
                return None;
            }
            shell_script(&tokens).map(|script| script.trim().to_owned()).filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| trimmed.to_owned());
    with_visible_control_characters(&unwrapped)
}

/// `withVisibleControlCharacters`: control characters other than tab and line breaks as their
/// Control Pictures glyphs.
pub fn with_visible_control_characters(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{7f}' => '␡',
            '\t' | '\n' | '\r' => c,
            c if (c as u32) < 0x20 => char::from_u32(0x2400 + c as u32).unwrap_or(c),
            c => c,
        })
        .collect()
}
