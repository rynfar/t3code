//! Chat markdown (`apps/web/src/components/ChatMarkdown.tsx`, `index.css` `.chat-markdown`) on
//! gpui-component's `TextView`. One `TextViewState` per message, created when its row first
//! renders; streamed text is appended with `push_str`, which reparses only the last block on a
//! background task. Code blocks are highlighted by tree-sitter and the styles are cached per block.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{App, AppContext, ClipboardItem, Entity, EntityId, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString, StyleRefinement, Styled, Subscription, div, px, rems};
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
    /// Set once the text holds image syntax; it stays on so the parse is not redone.
    media: bool,
    document: super::media::MediaDocument,
    extensions: gpui_component::text::MarkdownExtensions,
    _repaint: Subscription,
}

/// A message's markdown state, and whether it parses images as media (`super::media`). Only
/// texts with image syntax take the extension, since applying it reparses the text once.
#[derive(Clone)]
pub struct MarkdownState {
    pub entity: Entity<TextViewState>,
    media: bool,
    extensions: Option<gpui_component::text::MarkdownExtensions>,
}

impl From<&MarkdownState> for MarkdownState {
    fn from(state: &MarkdownState) -> Self {
        state.clone()
    }
}

/// External states supply their own extensions, such as a file preview's media scope.
impl From<&Entity<TextViewState>> for MarkdownState {
    fn from(entity: &Entity<TextViewState>) -> Self {
        MarkdownState { entity: entity.clone(), media: false, extensions: None }
    }
}

fn has_image_syntax(text: &str) -> bool {
    text.contains("![")
}

struct CachedContext {
    text: Arc<str>,
    presentation: Arc<t3_core::composer::records::SentPresentation>,
    drawn: u64,
}

/// Markdown states keyed by the row that shows them, created lazily when a row renders.
pub struct MarkdownStore {
    states: HashMap<String, Cached>,
    contexts: HashMap<String, CachedContext>,
    frame: u64,
    owner: EntityId,
}

impl MarkdownStore {
    /// `owner` is the view rendering these states. It is drawn `.cached()`, and a state's
    /// background reparse notifies only the state, so each state re-notifies the owner.
    pub fn new(owner: EntityId) -> Self {
        Self { states: HashMap::new(), contexts: HashMap::new(), frame: 0, owner }
    }

    /// Called once per transcript render, before rows draw.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        if self.states.len() > SOFT_LIMIT {
            let frame = self.frame;
            self.states.retain(|_, cached| cached.drawn + KEEP_FRAMES >= frame);
        }
        if self.contexts.len() > SOFT_LIMIT {
            let frame = self.frame;
            self.contexts.retain(|_, cached| cached.drawn + KEEP_FRAMES >= frame);
        }
    }

    /// The state for `key`, brought up to `text`. Appends stream; anything else replaces.
    pub fn state(&mut self, key: &str, text: &Arc<str>, cx: &mut App) -> MarkdownState {
        if let Some(cached) = self.states.get_mut(key) {
            cached.drawn = self.frame;
            cached.media |= has_image_syntax(text);
            if !Arc::ptr_eq(&cached.text, text) && *cached.text != **text {
                let previous = cached.text.clone();
                cached.text = text.clone();
                *cached.document.write().unwrap() = Arc::new(t3_core::media::MarkdownMediaDocument::new(text.clone()));
                cached.state.update(cx, |state, cx| match text.strip_prefix(&*previous) {
                    Some(delta) if !previous.is_empty() => state.push_str(delta, cx),
                    _ => state.set_text(text, cx),
                });
            }
            return MarkdownState { entity: cached.state.clone(), media: cached.media, extensions: Some(cached.extensions.clone()) };
        }
        let state = cx.new(|cx| TextViewState::markdown(text, cx).selectable(true));
        let owner = self.owner;
        let repaint = cx.observe(&state, move |_, cx| cx.notify(owner));
        let media = has_image_syntax(text);
        let document = super::media::media_document(text.clone());
        let extensions = super::media::markdown_extensions(document.clone(), None);
        self.states.insert(key.to_owned(), Cached { text: text.clone(), state: state.clone(), drawn: self.frame, media, document, extensions: extensions.clone(), _repaint: repaint });
        MarkdownState { entity: state, media, extensions: Some(extensions) }
    }

    pub fn user_context(&mut self, key: &str, text: &Arc<str>) -> Arc<t3_core::composer::records::SentPresentation> {
        if let Some(cached) = self.contexts.get_mut(key)
            && (Arc::ptr_eq(&cached.text, text) || *cached.text == **text)
        {
            cached.drawn = self.frame;
            return cached.presentation.clone();
        }
        let presentation = Arc::new(t3_core::composer::records::SentPresentation::new(text));
        self.contexts.insert(key.into(), CachedContext { text: text.clone(), presentation: presentation.clone(), drawn: self.frame });
        presentation
    }

    pub fn clear(&mut self) {
        self.states.clear();
        self.contexts.clear();
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
        .text_size(theme.mono_font_size)
        .line_height(theme.mono_font_size * 1.375);
    let mut code_block = if crate::theme::preferences(cx).typography.word_wrap { code_block.whitespace_normal() } else { code_block.whitespace_nowrap() };
    code_block.overflow.x = Some(gpui::Overflow::Scroll);
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
pub fn markdown_view(state: impl Into<MarkdownState>, cx: &App) -> TextView {
    let state = state.into();
    let view = TextView::new(&state.entity);
    let view = if state.media { view.markdown_extensions(state.extensions.unwrap()) } else { view };
    style_markdown(view, cx)
}

pub fn style_markdown(view: TextView, cx: &App) -> TextView {
    view.style(style(cx))
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
pub fn prose(state: &MarkdownState, cx: &App) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .text_size(px(14.))
        .line_height(px(22.75))
        .font_weight(FontWeight::NORMAL)
        .text_color(hsla(palette(cx).foreground).opacity(0.9))
        .child(markdown_view(state, cx))
}

#[cfg(test)]
mod context_tests {
    use super::*;
    #[test]
    fn user_rows_reuse_context_parsing_across_renders_and_refresh_record_payloads() {
        let mut store=MarkdownStore::new(EntityId::from(1u64));
        let source:Arc<str>="Use [Mac](t3-context://v1/environment/environment_mac) and `[Mac](t3-context://v1/environment/environment_mac)`".into();
        let first=store.user_context("user:1",&source);
        store.begin_frame();
        let same_text:Arc<str>=source.to_string().into();
        let again=store.user_context("user:1",&same_text);
        assert!(Arc::ptr_eq(&first,&again));
        assert_eq!(&*first.display_text,"Use Mac and `[Mac](t3-context://v1/environment/environment_mac)`");
        let record=t3_core::composer::records::environment_record("mac","New title");
        assert_eq!(again.records(&[record])[0]["label"],"New title");
        let changed:Arc<str>="Plain message".into();
        let replaced=store.user_context("user:1",&changed);
        assert!(!Arc::ptr_eq(&first,&replaced));
        assert!(Arc::ptr_eq(&changed,&replaced.display_text));
        store.clear();
        let reset=store.user_context("user:1",&changed);
        assert!(!Arc::ptr_eq(&replaced,&reset));
        let kept=store.user_context("keep",&changed);
        for n in 0..SOFT_LIMIT { store.user_context(&n.to_string(),&changed); }
        for _ in 0..=KEEP_FRAMES { store.begin_frame(); store.user_context("keep",&changed); }
        assert!(Arc::ptr_eq(&kept,&store.user_context("keep",&changed)));
        assert!(!Arc::ptr_eq(&reset,&store.user_context("user:1",&changed)));
    }
}
