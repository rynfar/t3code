//! Chat markdown (`apps/web/src/components/ChatMarkdown.tsx`, `index.css` `.chat-markdown`) on
//! gpui-component's `TextView`. One `TextViewState` per message, created when its row first
//! renders; streamed text is appended with `push_str`, which reparses only the last block on a
//! background task. Code blocks are highlighted by tree-sitter and the styles are cached per block.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{App, AppContext, ClipboardItem, Entity, FontWeight, HighlightStyle, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, StyleRefinement, Styled, div, px, rems};
use gpui_component::text::{TextView, TextViewState, TextViewStyle};
use gpui_component::{ActiveTheme, h_flex};

use crate::icons::Icon;
use crate::theme::{hsla, palette};

const CACHE_LIMIT: usize = 400;

struct Cached {
    text: Arc<str>,
    state: Entity<TextViewState>,
    used: u64,
}

/// Markdown states keyed by the row that shows them. Bounded: past the limit the least
/// recently drawn half is dropped and rebuilt if scrolled back into view.
#[derive(Default)]
pub struct MarkdownStore {
    states: HashMap<String, Cached>,
    clock: u64,
}

impl MarkdownStore {
    /// The state for `key`, brought up to `text`. Appends stream; anything else replaces.
    pub fn state(&mut self, key: &str, text: &Arc<str>, cx: &mut App) -> Entity<TextViewState> {
        self.clock += 1;
        if let Some(cached) = self.states.get_mut(key) {
            cached.used = self.clock;
            if !Arc::ptr_eq(&cached.text, text) && *cached.text != **text {
                let previous = cached.text.clone();
                cached.text = text.clone();
                cached.state.update(cx, |state, cx| match text.strip_prefix(&*previous) {
                    Some(delta) if !previous.is_empty() => state.push_str(delta, cx),
                    _ => state.set_text(text, cx),
                });
            }
            return cached.state.clone();
        }
        if self.states.len() >= CACHE_LIMIT {
            let mut used: Vec<u64> = self.states.values().map(|cached| cached.used).collect();
            used.sort_unstable();
            let cutoff = used[used.len() / 2];
            self.states.retain(|_, cached| cached.used > cutoff);
        }
        let state = cx.new(|cx| TextViewState::markdown(text, cx).selectable(true));
        self.states.insert(key.to_owned(), Cached { text: text.clone(), state: state.clone(), used: self.clock });
        state
    }

    pub fn clear(&mut self) {
        self.states.clear();
    }
}

fn style(cx: &App) -> TextViewStyle {
    let p = palette(cx);
    let theme = cx.theme();
    let code_block = StyleRefinement::default()
        .bg(hsla(p.code_block_background))
        .border_1()
        .border_color(hsla(p.code_block_border))
        .rounded(px(8.))
        .pt(px(34.))
        .pb(px(12.8))
        .px(px(14.4))
        .text_size(px(crate::theme::metrics::DEFAULT_CODE_FONT_SIZE))
        .line_height(rems(1.375));
    TextViewStyle {
        paragraph_gap: rems(0.65),
        heading_base_font_size: px(14.),
        highlight_theme: theme.highlight_theme.clone(),
        code_block,
        inline_code: HighlightStyle { background_color: Some(hsla(p.inline_code_background)), ..HighlightStyle::default() },
        is_dark: theme.mode.is_dark(),
        ..TextViewStyle::default()
    }
    .heading_font_size(|level, _| px(match level {
        1 => 20.,
        2 => 18.,
        3 => 16.,
        _ => 14.,
    }))
}

/// The prose view: §7.6 rhythm, §7.8 code blocks with a language label and Copy.
pub fn markdown_view(state: &Entity<TextViewState>, cx: &App) -> TextView {
    TextView::new(state)
        .style(style(cx))
        .selectable(true)
        .code_block_actions(|block, _window, cx| {
            let p = palette(cx);
            let code = block.code();
            let label = block.lang().filter(|lang| !lang.is_empty());
            h_flex()
                .gap(px(2.))
                .items_center()
                .children(label.map(|lang| {
                    div()
                        .px(px(6.))
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(px(11.))
                        .text_color(hsla(p.foreground).opacity(0.72))
                        .child(lang)
                }))
                .child(
                    div()
                        .id("copy-code")
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(hsla(p.accent)))
                        .child(Icon::Copy.render(px(12.), hsla(p.muted_foreground)))
                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()))),
                )
        })
        .on_link_click(|url: &SharedString, _, _, cx| {
            if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
                cx.open_url(url);
            }
        })
}

/// Assistant prose: 14px on a 22.75px line, `foreground` at 90% (§3.3).
pub fn prose(state: &Entity<TextViewState>, cx: &App) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .text_size(px(14.))
        .line_height(px(22.75))
        .font_weight(FontWeight::NORMAL)
        .text_color(hsla(palette(cx).foreground).opacity(0.9))
        .child(markdown_view(state, cx))
}
