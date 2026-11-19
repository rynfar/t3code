//! Standalone Mermaid file preview guard and renderer.
//! Ports `apps/web/src/components/files/MermaidFilePreview.tsx` and `MermaidViewer.tsx`.

use std::sync::OnceLock;

use base64::Engine as _;
use regex::Regex;

use crate::markdown::mermaid::{RenderedMermaidSvg, render_mermaid_svg};

/// Encodes an SVG document as a `data:image/svg+xml;base64,...` URI for the media lightbox.
pub fn svg_data_url(svg: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(svg.as_bytes());
    format!("data:image/svg+xml;base64,{encoded}")
}

/// Exact safety notice from `MermaidViewer.tsx`.
pub const MERMAID_UNSAFE_NOTICE: &str = "This diagram contains resource URLs, markup, node metadata, or custom configuration. Show Mermaid source to inspect it safely.";

/// Refuses resource URLs, markup, YAML frontmatter/node metadata, or directive config blocks
/// (`MermaidViewer.tsx` line 12).
pub fn contains_unsafe_mermaid_directives(source: &str) -> bool {
    static UNSAFE_RE: OnceLock<Regex> = OnceLock::new();
    let re = UNSAFE_RE.get_or_init(|| {
        Regex::new(
            r"(?im)%%\s*\{|^\s*---|</?[A-Za-z]|<[!?]|@\s*\{|\b(?:img|image)\s*:|url\s*\(|@import|(?:https?|data|blob|file):",
        )
        .expect("mermaid safety regex must compile")
    });
    re.is_match(source)
}

#[derive(Debug, Clone, PartialEq)]
pub enum MermaidFileOutcome {
    Rendered(RenderedMermaidSvg),
    UnsafeDirective(&'static str),
    RenderError(String),
}

/// Validates and renders a standalone `.mmd`/`.mermaid` file to SVG.
pub fn render_mermaid_file(source: &str, is_dark: bool) -> MermaidFileOutcome {
    if contains_unsafe_mermaid_directives(source) {
        return MermaidFileOutcome::UnsafeDirective(MERMAID_UNSAFE_NOTICE);
    }
    match render_mermaid_svg(source, is_dark) {
        Ok(svg) => MermaidFileOutcome::Rendered(svg),
        Err(err) => MermaidFileOutcome::RenderError(err),
    }
}
