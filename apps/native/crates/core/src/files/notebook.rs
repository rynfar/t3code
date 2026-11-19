//! Jupyter Notebook (`.ipynb`) parser and inert markdown sanitizer.
//! Ports `openNotebook` in `apps/web/src/components/files/extendedDocumentPreview.ts` and
//! `inertNotebookMarkdown` / pagination constants in `apps/web/src/components/files/NotebookPreview.tsx`.

use base64::Engine as _;
use serde_json::Value;

pub const NOTEBOOK_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const NOTEBOOK_MAX_CELLS: usize = 500;
pub const NOTEBOOK_MAX_OUTPUTS_PER_CELL: usize = 100;
pub const NOTEBOOK_TEXT_LIMIT: usize = 256_000;
pub const NOTEBOOK_IMAGE_BASE64_LIMIT: usize = 4 * 1024 * 1024;
pub const NOTEBOOK_INITIAL_VISIBLE_CELLS: usize = 20;
pub const NOTEBOOK_CELL_PAGE_STEP: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotebookCellKind {
    Markdown,
    Code,
    Raw,
}

impl NotebookCellKind {
    pub fn label(self) -> &'static str {
        match self {
            NotebookCellKind::Markdown => "markdown",
            NotebookCellKind::Code => "code",
            NotebookCellKind::Raw => "raw",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotebookOutput {
    Text(String),
    Html(String),
    Image { mime_type: String, bytes: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotebookCell {
    pub kind: NotebookCellKind,
    pub execution_count: Option<u64>,
    pub source: String,
    pub outputs: Vec<NotebookOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotebookDocument {
    pub language: String,
    pub cells: Vec<NotebookCell>,
    pub byte_length: u64,
}

impl NotebookDocument {
    /// Maps the notebook language name to a `SyntaxHighlighter` grammar name if supported.
    pub fn syntax_language(&self) -> Option<&'static str> {
        match self.language.to_ascii_lowercase().as_str() {
            "python" | "python3" | "py" => Some("python"),
            "javascript" | "js" | "node" => Some("javascript"),
            "typescript" | "ts" => Some("typescript"),
            "rust" | "rs" => Some("rust"),
            "go" | "golang" => Some("go"),
            "sql" => Some("sql"),
            "bash" | "sh" | "zsh" | "shell" => Some("bash"),
            "ruby" | "rb" => Some("ruby"),
            "html" => Some("html"),
            "css" => Some("css"),
            "json" => Some("json"),
            "yaml" | "yml" => Some("yaml"),
            "toml" => Some("toml"),
            _ => None,
        }
    }
}

/// Parses an `.ipynb` JSON document with the same safety bounds as `openNotebook`.
pub fn parse_notebook(bytes: &[u8]) -> Result<NotebookDocument, String> {
    if bytes.len() > NOTEBOOK_MAX_BYTES {
        return Err("Notebook preview limited to files under 8 MB.".to_owned());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "This notebook is not valid UTF-8 JSON.".to_owned())?;
    let root: Value = serde_json::from_str(text)
        .map_err(|_| "Could not parse notebook JSON.".to_owned())?;
    let Some(obj) = root.as_object() else {
        return Err("Preview supports version 4 notebooks with up to 500 cells.".to_owned());
    };
    let nbformat = obj.get("nbformat").and_then(Value::as_u64);
    let cells_arr = obj.get("cells").and_then(Value::as_array);
    let Some(cells_arr) = cells_arr.filter(|arr| nbformat == Some(4) && arr.len() <= NOTEBOOK_MAX_CELLS) else {
        return Err("Preview supports version 4 notebooks with up to 500 cells.".to_owned());
    };

    let raw_lang = obj
        .get("metadata")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("language_info"))
        .and_then(Value::as_object)
        .and_then(|info| info.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("text");
    let language: String = raw_lang
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '#' | '-'))
        .take(40)
        .collect();
    let language = if language.is_empty() { "text".to_owned() } else { language };

    let mut cells = Vec::with_capacity(cells_arr.len());
    for cell_val in cells_arr {
        let cell_obj = cell_val.as_object();
        let kind = match cell_obj.and_then(|c| c.get("cell_type")).and_then(Value::as_str) {
            Some("markdown") => NotebookCellKind::Markdown,
            Some("code") => NotebookCellKind::Code,
            _ => NotebookCellKind::Raw,
        };
        let execution_count = cell_obj.and_then(|c| c.get("execution_count")).and_then(Value::as_u64);
        let source = notebook_text(cell_obj.and_then(|c| c.get("source")), NOTEBOOK_TEXT_LIMIT)?;
        let raw_outputs = cell_obj
            .and_then(|c| c.get("outputs"))
            .and_then(Value::as_array);
        if let Some(arr) = raw_outputs
            && arr.len() > NOTEBOOK_MAX_OUTPUTS_PER_CELL
        {
            return Err("A notebook cell has too many outputs to preview.".to_owned());
        }
        let mut outputs = Vec::new();
        for output_val in raw_outputs.into_iter().flatten() {
            if let Some(parsed) = parse_output(output_val)? {
                outputs.push(parsed);
            }
        }
        cells.push(NotebookCell { kind, execution_count, source, outputs });
    }

    Ok(NotebookDocument {
        language,
        cells,
        byte_length: bytes.len() as u64,
    })
}

fn notebook_text(value: Option<&Value>, limit: usize) -> Result<String, String> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    let result = match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                if let Some(part) = item.as_str() {
                    if out.len().saturating_add(part.len()) > limit {
                        return Err("A notebook cell is too large to preview.".to_owned());
                    }
                    out.push_str(part);
                }
            }
            out
        }
        _ => String::new(),
    };
    if result.len() > limit {
        return Err("A notebook cell is too large to preview.".to_owned());
    }
    Ok(result)
}

fn parse_output(value: &Value) -> Result<Option<NotebookOutput>, String> {
    let Some(obj) = value.as_object() else {
        return Ok(None);
    };
    let data = obj.get("data").and_then(Value::as_object);
    if let Some(data) = data {
        for mime in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
            if let Some(img_val) = data.get(mime) {
                let raw = notebook_text(Some(img_val), NOTEBOOK_IMAGE_BASE64_LIMIT)?;
                let cleaned: String = raw.chars().filter(|ch| !ch.is_whitespace()).collect();
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(&cleaned)
                    && !decoded.is_empty()
                {
                    return Ok(Some(NotebookOutput::Image {
                        mime_type: mime.to_owned(),
                        bytes: decoded,
                    }));
                }
            }
        }
        if let Some(html_val) = data.get("text/html") {
            let html = notebook_text(Some(html_val), NOTEBOOK_TEXT_LIMIT)?;
            if !html.trim().is_empty() {
                return Ok(Some(NotebookOutput::Html(strip_html_to_text(&html))));
            }
        }
    }

    let text = if let Some(stream_text) = obj.get("text") {
        notebook_text(Some(stream_text), NOTEBOOK_TEXT_LIMIT)?
    } else if let Some(plain) = data.and_then(|d| d.get("text/plain")) {
        notebook_text(Some(plain), NOTEBOOK_TEXT_LIMIT)?
    } else if let Some(traceback) = obj.get("traceback") {
        format_traceback(traceback)?
    } else {
        String::new()
    };

    let cleaned = strip_ansi(&text);
    if cleaned.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(NotebookOutput::Text(cleaned)))
    }
}

/// IPython traceback frames may omit trailing newlines or end with ANSI SGR sequences.
fn format_traceback(value: &Value) -> Result<String, String> {
    let Some(lines) = value.as_array() else {
        return notebook_text(Some(value), NOTEBOOK_TEXT_LIMIT);
    };
    let mut out = String::new();
    for line in lines {
        let Some(part) = line.as_str() else { continue };
        if out.len().saturating_add(part.len() + 1) > NOTEBOOK_TEXT_LIMIT {
            return Err("A notebook cell is too large to preview.".to_owned());
        }
        out.push_str(part);
        let stripped = strip_ansi(part);
        if !stripped.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

/// Strips ANSI CSI escape sequences so error tracebacks read cleanly in plain `<pre>` output.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Converts saved notebook HTML outputs into safe, readable plain text (never loads remote URLs or scripts).
pub fn strip_html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len().min(4096));
    let mut in_tag = false;
    let mut skip_block = false;
    let mut tag_buf = String::new();
    for ch in html.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                let tag = tag_buf.trim().to_ascii_lowercase();
                let name = tag
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                if tag.starts_with("script") || tag.starts_with("style") {
                    skip_block = true;
                } else if tag.starts_with("/script") || tag.starts_with("/style") {
                    skip_block = false;
                } else if matches!(name, "br" | "tr" | "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4") {
                    if !out.ends_with('\n') && !out.is_empty() {
                        out.push('\n');
                    }
                } else if matches!(name, "td" | "th") && tag.starts_with('/') {
                    out.push('\t');
                }
                tag_buf.clear();
            } else {
                tag_buf.push(ch);
            }
        } else if ch == '<' {
            in_tag = true;
            tag_buf.clear();
        } else if !skip_block {
            out.push(ch);
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_owned()
}

/// Makes notebook markdown inert (`inertNotebookMarkdown` in `NotebookPreview.tsx`):
/// strips raw HTML tags, replaces images `![alt](url)` with `alt`, strips link destinations `[label](url)` -> `label`,
/// and downgrades ```mermaid fences to ```text so saved notebooks cannot trigger network requests or app actions.
pub fn sanitize_notebook_markdown(markdown: &str) -> String {
    let mut lines_out = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            if !in_fence {
                in_fence = true;
                let lang = rest.trim().to_ascii_lowercase();
                if lang == "mermaid" {
                    lines_out.push("```text".to_owned());
                    continue;
                }
            } else {
                in_fence = false;
            }
            lines_out.push(line.to_owned());
            continue;
        }
        if in_fence {
            lines_out.push(line.to_owned());
            continue;
        }
        lines_out.push(strip_markdown_links_and_html(line));
    }
    lines_out.join("\n")
}

fn strip_markdown_links_and_html(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        // Strip inline HTML tags `<...>`
        if chars[i] == '<'
            && let Some(end) = chars[i + 1..].iter().position(|&c| c == '>')
            && chars.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic() || *c == '/' || *c == '!')
        {
            i += end + 2;
            continue;
        }
        // Replace `![alt](url)` or `[label](url)` with just `alt` / `label`
        let is_image = chars[i] == '!' && chars.get(i + 1) == Some(&'[');
        if is_image || chars[i] == '[' {
            let bracket_start = if is_image { i + 1 } else { i };
            if let Some(close_rel) = chars[bracket_start + 1..].iter().position(|&c| c == ']') {
                let close_idx = bracket_start + 1 + close_rel;
                if chars.get(close_idx + 1) == Some(&'(')
                    && let Some(paren_rel) = chars[close_idx + 2..].iter().position(|&c| c == ')')
                {
                    let label: String = chars[bracket_start + 1..close_idx].iter().collect();
                    out.push_str(&label);
                    i = close_idx + 2 + paren_rel + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}
