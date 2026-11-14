//! Rich Markdown helpers for `apps/native` (`TR-MARKDOWN`): fenced code metadata & shell run
//! detection, GFM table serialization, GitHub Alerts, `<details>`/`<picture>` preprocessing,
//! LaTeX math formatting, Mermaid SVG rendering, and file/thread/citation link classification.

pub mod alerts_details;
pub mod code;
pub mod links;
pub mod math;
pub mod mermaid;
pub mod table;

/// Options controlling display preprocessing of Markdown text.
#[derive(Debug, Clone, Default)]
pub struct DisplayMarkdownOptions<'a> {
    pub is_dark: bool,
    pub cwd: Option<&'a str>,
    pub math_enabled: bool,
}

/// Applies `<picture>` theme selection, `<details>` disclosure extraction, optional LaTeX math
/// formatting, and inline-code file-path linking to `source` for rendering in `TextView`.
pub fn prepare_display_markdown(source: &str, options: DisplayMarkdownOptions<'_>) -> String {
    prepare_display_markdown_with_raw_html(source, options, true)
}

/// Same as [`prepare_display_markdown`], letting user/work messages disable raw HTML expansion
/// (`parseRawHtml={false}`) while keeping inline file linking and optional math.
pub fn prepare_display_markdown_with_raw_html(source: &str, options: DisplayMarkdownOptions<'_>, parse_raw_html: bool) -> String {
    let with_html_and_files = alerts_details::preprocess_markdown_for_display_with_html(source, options.is_dark, options.cwd, parse_raw_html);
    if options.math_enabled {
        math::preprocess_math_for_display(&with_html_and_files)
    } else {
        with_html_and_files
    }
}

/// Returns true when the Markdown text contains syntax handled by custom extensions or display
/// preprocessing (code blocks, tables, blockquotes/alerts, math, details, HTML/markdown media, or file paths).
pub fn has_rich_markdown_syntax(source: &str) -> bool {
    source.contains("```")
        || source.contains("~~~")
        || source.contains('>')
        || source.contains('|')
        || source.contains("![")
        || contains_ascii_case_insensitive_tag(source)
        || math::might_contain_math(source)
        || source.contains('`')
}

fn contains_ascii_case_insensitive_tag(source: &str) -> bool {
    if !source.contains('<') {
        return false;
    }
    let lower = source.to_ascii_lowercase();
    ["<details", "<picture", "<img", "<video", "<table", "<h1", "<h2", "<h3", "<h4", "<h5", "<h6"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// Formats copied text cleanly (normalizing CRLF and trimming excess trailing blank lines).
pub fn normalize_clipboard_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end_matches('\n').to_owned()
}
