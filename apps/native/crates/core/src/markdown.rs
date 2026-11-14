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
    let with_html_and_files = alerts_details::preprocess_markdown_for_display(source, options.is_dark, options.cwd);
    if options.math_enabled {
        math::preprocess_math_for_display(&with_html_and_files)
    } else {
        with_html_and_files
    }
}

/// Returns true when the Markdown text contains syntax handled by custom extensions or display
/// preprocessing (code blocks, tables, blockquotes/alerts, math, details, pictures, or file paths).
pub fn has_rich_markdown_syntax(source: &str) -> bool {
    source.contains("```")
        || source.contains("~~~")
        || source.contains('>')
        || source.contains('|')
        || source.contains("![")
        || source.contains("<details")
        || source.contains("<picture")
        || math::might_contain_math(source)
        || source.contains('`')
}

/// Formats copied text cleanly (normalizing CRLF and trimming excess trailing blank lines).
pub fn normalize_clipboard_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end_matches('\n').to_owned()
}
