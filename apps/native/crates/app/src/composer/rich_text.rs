//! Rich-text decorations, editable Markdown source markers (`#277`), fenced code block
//! highlighting & indentation, list continuation, and formatting toggles (`CO-RICH`, `#387`).

use std::ops::Range;

use gpui::{App, Context, FontStyle, FontWeight, HighlightStyle, StrikethroughStyle, Window, px};
use gpui_base::input::{TextDecoration, TextDecorationCollection};
use gpui_component::ActiveTheme;
use gpui_component::highlighter::SyntaxHighlighter;
use t3_core::client_settings::typography::FontRole;
use t3_core::composer::rich_text::{self as rules, InlineFormat, InlineTokenKind};
use t3_core::lists;

use super::Composer;
use crate::theme::{hsla, palette};

pub(super) struct RichTextState {
    pub(super) decorations: TextDecorationCollection,
    cache_key: Option<(String, Range<usize>, usize, bool)>,
}

impl RichTextState {
    pub(super) fn new(decorations: TextDecorationCollection) -> Self {
        Self { decorations, cache_key: None }
    }
}

impl Composer {
    /// Syncs inline Markdown decorations, editable source-marker visibility (`#277`), fenced code
    /// block syntax highlighting, and inline chip/mention styling onto the `EditorState`.
    pub(super) fn sync_rich_decorations(&mut self, cx: &mut Context<Self>) {
        let (text, selection) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.selected_range())
        };
        let rev = crate::theme::revision(cx);
        let dark = cx.theme().is_dark();
        if self.rich_text.cache_key.as_ref() == Some(&(text.clone(), selection.clone(), rev, dark)) {
            return;
        }
        self.rich_text.cache_key = Some((text.clone(), selection.clone(), rev, dark));

        if text.is_empty() {
            self.rich_text.decorations.clear(cx);
            return;
        }

        let p = palette(cx);
        let doc = rules::parse_composer_document(&text);
        let mut decos: Vec<TextDecoration> = Vec::new();
        let revealed_marker_color = hsla(p.muted_foreground);
        let collapsed_marker_color = hsla(p.muted_foreground).opacity(0.24);
        let code_bg = hsla(p.code_block_background);
        let inline_code_bg = hsla(p.inline_code_background);

        for block in &doc.code_blocks {
            if !block.code_range.is_empty() {
                decos.push(TextDecoration::new(
                    block.code_range.clone(),
                    HighlightStyle { background_color: Some(code_bg), ..Default::default() },
                ));
                if let Some(lang) = block.language.as_deref() {
                    let code = &text[block.code_range.clone()];
                    let mut syntax = SyntaxHighlighter::new(lang);
                    syntax.update(None, &gpui_component::Rope::from_str(code), None);
                    for (range, style) in syntax.styles(&(0..code.len()), &*cx.theme().highlight_theme) {
                        let abs = (block.code_range.start + range.start)..(block.code_range.start + range.end);
                        if abs.start < abs.end {
                            decos.push(TextDecoration::new(abs, style));
                        }
                    }
                }
            }
            let fence_color = if rules::is_marker_revealed(&block.full_range, &selection) {
                revealed_marker_color
            } else {
                collapsed_marker_color
            };
            decos.push(TextDecoration::new(
                block.open_fence.clone(),
                HighlightStyle { color: Some(fence_color), background_color: Some(code_bg.opacity(0.6)), ..Default::default() },
            ));
            if let Some(close) = &block.close_fence {
                decos.push(TextDecoration::new(
                    close.clone(),
                    HighlightStyle { color: Some(fence_color), background_color: Some(code_bg.opacity(0.6)), ..Default::default() },
                ));
            }
        }

        for prefix in &doc.prefixes {
            let revealed = rules::is_marker_revealed(&prefix.full_range, &selection);
            let marker_color = if revealed { revealed_marker_color } else { hsla(p.muted_foreground).opacity(0.45) };
            decos.push(TextDecoration::new(
                prefix.marker_range.clone(),
                HighlightStyle { color: Some(marker_color), font_weight: Some(FontWeight::MEDIUM), ..Default::default() },
            ));
            if prefix.heading_level.is_some() && !prefix.content_range.is_empty() {
                decos.push(TextDecoration::new(
                    prefix.content_range.clone(),
                    HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), color: Some(hsla(p.foreground)), ..Default::default() },
                ));
            }
        }

        for span in &doc.marks {
            let content_style = match span.mark {
                InlineFormat::Bold => HighlightStyle { font_weight: Some(FontWeight::BOLD), ..Default::default() },
                InlineFormat::Italic => HighlightStyle { font_style: Some(FontStyle::Italic), ..Default::default() },
                InlineFormat::Strike => HighlightStyle {
                    strikethrough: Some(StrikethroughStyle { thickness: px(1.), color: Some(hsla(p.muted_foreground)) }),
                    color: Some(hsla(p.muted_foreground)),
                    ..Default::default()
                },
                InlineFormat::Code => HighlightStyle { background_color: Some(inline_code_bg), color: Some(hsla(p.foreground)), ..Default::default() },
            };
            if !span.content_range.is_empty() {
                decos.push(TextDecoration::new(span.content_range.clone(), content_style));
            }
            let marker_color = if rules::is_marker_revealed(&span.full_range, &selection) {
                revealed_marker_color
            } else {
                collapsed_marker_color
            };
            decos.push(TextDecoration::new(
                span.open_marker.clone(),
                HighlightStyle { color: Some(marker_color), ..Default::default() },
            ));
            decos.push(TextDecoration::new(
                span.close_marker.clone(),
                HighlightStyle { color: Some(marker_color), ..Default::default() },
            ));
        }

        for token in &doc.tokens {
            let pill_bg = hsla(p.accent).opacity(if dark { 0.36 } else { 0.45 });
            let pill_fg = match token.kind {
                InlineTokenKind::ContextLink | InlineTokenKind::CitationLink | InlineTokenKind::ModelMention => hsla(p.info),
                InlineTokenKind::FileMention | InlineTokenKind::SkillMention => hsla(p.primary),
            };
            decos.push(TextDecoration::new(
                token.range.clone(),
                HighlightStyle { color: Some(pill_fg), background_color: Some(pill_bg), font_weight: Some(FontWeight::MEDIUM), ..Default::default() },
            ));
        }

        self.rich_text.decorations.set(decos, cx);
    }

    /// Computes auto-growing editor height (2..=9 rows) matching the `TextareaState::auto_grow(2, 9)` behavior.
    pub(super) fn composer_editor_height(&self, cx: &App) -> f32 {
        let font_size = crate::theme::font_size(FontRole::Prompt, cx);
        let line_height = (font_size * 1.625).ceil();
        let input = self.input.read(cx);
        let text = input.value();
        let width = f32::from(input.input_bounds().size.width);
        let rows: usize = if width <= 16.0 {
            text.split('\n').count()
        } else {
            let approx_char_w = (font_size * 0.56).max(4.0);
            text.split('\n')
                .map(|line| {
                    let line_px = (line.chars().count().max(1) as f32) * approx_char_w;
                    (line_px / width).ceil().max(1.0) as usize
                })
                .sum()
        };
        // Extra 4px keeps the top ascenders unclipped when `Editor` fits exact line counts.
        rows.clamp(2, 9) as f32 * line_height + 4.0
    }

    pub(super) fn apply_rich_edit(&mut self, edit: rules::RichTextEdit, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_selected_range(edit.start..edit.end, cx);
            input.replace(edit.replacement, window, cx);
            input.set_selected_range(edit.selection_after, cx);
        });
    }

    pub(super) fn indent_list_or_code(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.is_rewinding(cx) || self.answering.is_some() {
            return false;
        }
        let (prompt, sel) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.selected_range())
        };
        if let Some(edit) = rules::code_block_indent(&prompt, sel.start, sel.end, false) {
            self.apply_rich_edit(edit, window, cx);
            return true;
        }
        if let Some(edit) = lists::indent_for_tab(&prompt, sel.start, sel.end) {
            self.input.update(cx, |input, cx| {
                input.set_selected_range(edit.start..edit.end, cx);
                input.replace(edit.replacement, window, cx);
                input.set_selected_range(edit.cursor_after..edit.cursor_after, cx);
            });
            return true;
        }
        false
    }

    pub(super) fn toggle_inline_format(&mut self, format: InlineFormat, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) || self.answering.is_some() {
            return;
        }
        let (prompt, sel) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.selected_range())
        };
        let edit = rules::toggle_inline_format(&prompt, sel.start, sel.end, format);
        self.apply_rich_edit(edit, window, cx);
    }
}
