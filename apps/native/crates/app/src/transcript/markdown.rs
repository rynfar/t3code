//! Chat markdown (`apps/web/src/components/ChatMarkdown.tsx`, `index.css` `.chat-markdown`) on
//! gpui-component's `TextView`. One `TextViewState` per message, created when its row first
//! renders; streamed text is appended with `push_str`, which reparses only the last block on a
//! background task. Code blocks are highlighted by tree-sitter and the styles are cached per block.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{App, AppContext, ClipboardItem, Entity, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString, StyleRefinement, Styled, div, px, rems};
use gpui_component::text::{TextView, TextViewState, TextViewStyle};
use gpui_component::{ActiveTheme, h_flex};

use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// States kept regardless of when they were drawn; beyond this, states not drawn in the last
/// few frames (rows the list no longer renders) are dropped and rebuilt if scrolled back to.
const SOFT_LIMIT: usize = 48;
const KEEP_FRAMES: u64 = 2;

struct Cached {
    text: Arc<str>,
    state: Entity<TextViewState>,
    drawn: u64,
}

/// Markdown states keyed by the row that shows them, created lazily when a row renders.
#[derive(Default)]
pub struct MarkdownStore {
    states: HashMap<String, Cached>,
    frame: u64,
}

impl MarkdownStore {
    /// Called once per transcript render, before rows draw.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        if self.states.len() > SOFT_LIMIT {
            let frame = self.frame;
            self.states.retain(|_, cached| cached.drawn + KEEP_FRAMES >= frame);
        }
    }

    /// The state for `key`, brought up to `text`. Appends stream; anything else replaces.
    pub fn state(&mut self, key: &str, text: &Arc<str>, cx: &mut App) -> Entity<TextViewState> {
        if let Some(cached) = self.states.get_mut(key) {
            cached.drawn = self.frame;
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
        let state = cx.new(|cx| TextViewState::markdown(text, cx).selectable(true));
        self.states.insert(key.to_owned(), Cached { text: text.clone(), state: state.clone(), drawn: self.frame });
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
                .child(super::parts::ghost_icon_button(
                    "copy-code",
                    Icon::Copy,
                    move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(code.to_string())),
                    cx,
                ))
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
