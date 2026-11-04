//! Chat markdown (`apps/web/src/components/ChatMarkdown.tsx`, `index.css` `.chat-markdown`) on
//! gpui-component's `TextView`. One `TextViewState` per message, created when its row first
//! renders; streamed text is appended with `push_str`, which reparses only the last block on a
//! background task. Code blocks are highlighted by tree-sitter and the styles are cached per block.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{App, AppContext, ClipboardItem, Entity, EntityId, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString, StyleRefinement, Styled, Subscription, Task, div, px, rems};
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
    selections: super::media::MediaSelections,
    _repaint: Subscription,
    updates: Option<MediaUpdates>,
}

struct MediaUpdates {
    source: tokio::sync::watch::Sender<Arc<str>>,
    _task: Task<()>,
}

impl MediaUpdates {
    fn new(state: &Entity<TextViewState>, document: super::media::MediaDocument, baseline: Arc<str>, requested: Arc<str>, cx: &App) -> Self {
        let (source, mut receiver) = tokio::sync::watch::channel(requested);
        let state = state.downgrade();
        let task = cx.spawn(async move |cx| {
            let mut analyzed = Arc::new(cx.background_executor().spawn(async move { t3_core::media::MarkdownMediaDocument::new(baseline).prepare() }).await);
            let mut displayed = analyzed.clone();
            loop {
                let source = receiver.borrow_and_update().clone();
                let previous = analyzed;
                let rendered = displayed.clone();
                let (next, delta) = cx.background_executor().spawn(async move {
                    let next = Arc::new(previous.updated(source));
                    let delta = rendered.append_delta(&next).map(str::to_owned);
                    (next, delta)
                }).await;
                analyzed = next;
                // One worker and one latest source prevent stale parses or a chunk backlog from reaching the view.
                if !Arc::ptr_eq(&receiver.borrow(), analyzed.source()) { continue; }
                let next = analyzed.clone();
                if state.update(cx, |state, cx| {
                    *document.write().unwrap() = next.clone();
                    match delta { Some(delta) => state.push_str(&delta, cx), None => state.set_text(next.source(), cx) }
                }).is_err() { return; }
                displayed = next;
                if receiver.changed().await.is_err() { return; }
            }
        });
        Self { source, _task: task }
    }
}

/// A message's markdown state, and whether it parses images as media (`super::media`). Only
/// texts with image syntax take the extension, since applying it reparses the text once.
#[derive(Clone)]
pub struct MarkdownState {
    pub entity: Entity<TextViewState>,
    media: bool,
    extensions: Option<gpui_component::text::MarkdownExtensions>,
    selections: super::media::MediaSelections,
    document: Option<super::media::MediaDocument>,
}

impl MarkdownState {
    pub(crate) fn selection_text(&self, source: &str, quote: &str, cx: &App) -> Option<String> {
        if quote.trim().is_empty() { return None; }
        let document = self.document.as_ref().filter(|_| self.media).map(|document| document.read().unwrap().clone());
        if document.as_ref().is_some_and(|document| &**document.source() != source) { return None; }
        let parent = self.entity.read(cx).selected_text();
        match document {
            Some(document) => self.selections.selection_text(&document, parent, quote, cx),
            None => (parent == quote).then_some(parent),
        }
    }
}

impl From<&MarkdownState> for MarkdownState {
    fn from(state: &MarkdownState) -> Self {
        state.clone()
    }
}

/// External states supply their own extensions, such as a file preview's media scope.
impl From<&Entity<TextViewState>> for MarkdownState {
    fn from(entity: &Entity<TextViewState>) -> Self {
        MarkdownState { entity: entity.clone(), media: false, extensions: None, selections: Default::default(), document: None }
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
    /// Background parsing changes measured row heights, so it also notifies the list owner.
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
                if cached.media {
                    if let Some(updates) = &cached.updates { updates.source.send_replace(text.clone()); }
                    else { cached.updates = Some(MediaUpdates::new(&cached.state, cached.document.clone(), previous, text.clone(), cx)); }
                } else {
                    cached.state.update(cx, |state, cx| match text.strip_prefix(&*previous) {
                        Some(delta) if !previous.is_empty() => state.push_str(delta, cx),
                        _ => state.set_text(text, cx),
                    });
                }
            }
            return MarkdownState { entity: cached.state.clone(), media: cached.media, extensions: Some(cached.extensions.clone()), selections: cached.selections.clone(), document: Some(cached.document.clone()) };
        }
        let media = has_image_syntax(text);
        let state = cx.new(|cx| TextViewState::markdown(if media { "" } else { text }, cx).selectable(true));
        let owner = self.owner;
        let repaint = cx.observe(&state, move |_, cx| cx.notify(owner));
        let document = super::media::media_document(text.clone());
        let selections = super::media::MediaSelections::default();
        let extensions = super::media::markdown_extensions_with_selections(document.clone(), None, selections.clone());
        let updates = media.then(|| MediaUpdates::new(&state, document.clone(), Arc::from(""), text.clone(), cx));
        self.states.insert(key.to_owned(), Cached { text: text.clone(), state: state.clone(), drawn: self.frame, media, document: document.clone(), extensions: extensions.clone(), selections: selections.clone(), _repaint: repaint, updates });
        MarkdownState { entity: state, media, extensions: Some(extensions), selections, document: Some(document) }
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

pub(super) const PARAGRAPH_GAP: f32 = 0.65;

pub(super) fn heading_font_size(level: u8) -> gpui::Pixels {
    px(match level { 1 => 20., 2 => 18., 3 => 16., _ => 14. })
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
        paragraph_gap: rems(PARAGRAPH_GAP),
        heading_base_font_size: px(14.),
        highlight_theme: theme.highlight_theme.clone(),
        code_block,
        inline_code: HighlightStyle { background_color: Some(hsla(p.inline_code_background)), ..HighlightStyle::default() },
        is_dark: theme.mode.is_dark(),
        ..TextViewStyle::default()
    }
    .heading_font_size(|level, _| heading_font_size(level))
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
            if t3_core::media::markdown_external_link(url) {
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
    #[gpui::test]
    fn heading_child_selection_reaches_the_message_citation_action(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let source: Arc<str> = "## left ![chart](a.png) **right**\n\nBody after.".into();
        let parent = cx.update(|cx| cx.new(|cx| TextViewState::markdown("", cx)));
        let child = cx.update(|cx| cx.new(|cx| TextViewState::markdown(" **right**", cx)));
        cx.run_until_parked();
        let selections = super::super::media::MediaSelections::default();
        selections.register((0, 2), &child, " **right**");
        let document = super::super::media::media_document(source.clone());
        let state = MarkdownState { entity: parent, media: true, extensions: None, selections, document: Some(document.clone()) };
        child.update(cx, |child, cx| child.select_all(cx));
        let cite = |state: &MarkdownState, source: &str, quote: &str, cx: &App| {
            crate::thread_view::ThreadView::citation_from_markdown_selection(state, source, quote, ("environment".into(), "thread".into(), "message".into()), cx)
        };
        cx.update(|cx| {
            assert!(state.entity.read(cx).selected_text().is_empty());
            let quote = child.read(cx).selected_text();
            let citation = cite(&state, &source, &quote, cx).expect("heading prose must reach Cite");
            assert_eq!((&*citation.environment_id, &*citation.thread_id, &*citation.message_id, &*citation.text), ("environment", "thread", "message", "right"));
            assert!(citation.valid());
            assert!(cite(&state, &source, "another message", cx).is_none());
            *document.write().unwrap() = Arc::new(t3_core::media::MarkdownMediaDocument::new("replacement".into()));
            assert!(cite(&state, &source, &quote, cx).is_none());
            let replacement = "## left ![chart](a.png) **replacement**";
            *document.write().unwrap() = Arc::new(t3_core::media::MarkdownMediaDocument::new(replacement.into()));
            assert!(cite(&state, replacement, &quote, cx).is_none());
            let unrelated = MarkdownState::from(&state.entity);
            assert!(cite(&unrelated, &source, &quote, cx).is_none());
            *document.write().unwrap() = Arc::new(t3_core::media::MarkdownMediaDocument::new(source.clone()));
        });
        state.entity.update(cx, |parent, cx| parent.set_text("Body after.", cx));
        cx.run_until_parked();
        state.entity.update(cx, |parent, cx| parent.select_all(cx));
        cx.update(|cx| {
            let parent = state.entity.read(cx).selected_text();
            let child = child.read(cx).selected_text();
            assert!(!parent.trim().is_empty() && !child.trim().is_empty());
            let quote = format!("{child}\n{parent}");
            let citation = cite(&state, &source, &quote, cx).expect("one message's parent and heading child must reach Cite together");
            assert_eq!(t3_core::assistant_citations::normalize(&citation.text), "right Body after.");
            assert_eq!((citation.start, citation.end), (5, 22));
            assert!(citation.valid());
            assert!(cite(&state, &source, &format!("{quote}\noutside this message"), cx).is_none());
        });
        state.entity.update(cx, |parent, cx| parent.clear_selection(cx));
        drop(child);
        cx.update(|cx| assert!(cite(&state, &source, "right\n", cx).is_none()));
        for (source, parent_text, parts, expected, range) in [
            ("Body before.\n\n## left ![chart](a.png) right", "Body before.", vec![("## left", 0, "left ")], "Body before. left", (0, 17)),
            ("🦀 before.\n\n## left ![chart](a.png) right", "🦀 before.", vec![("## left", 0, "left ")], "🦀 before. left", (0, 15)),
            ("left\n\n## left ![chart](a.png) right", "left", vec![("## left", 0, "left "), ("## left", 2, " right")], "left left right", (0, 15)),
            ("Before.\n\n## left ![chart](a.png) right\n\nAfter.", "Before.\n\nleft  right\n\nAfter.", vec![("## left", 0, "left "), ("## left", 2, " right")], "Before. left right After.", (0, 25)),
            ("## one ![chart](a.png) tail\n\nBody between.\n\n## next ![chart](a.png) end", "", vec![("## one", 2, " tail"), ("## next", 0, "next ")], "tail Body between. next", (4, 27)),
            ("## one ![chart](a.png) tail\n\nBody between.\n\n## next ![chart](a.png) end", "Body between.", vec![("## one", 2, " tail"), ("## next", 0, "next ")], "tail Body between. next", (4, 27)),
        ] {
            let source: Arc<str> = source.into();
            let parent = cx.update(|cx| cx.new(|cx| TextViewState::markdown(parent_text, cx)));
            let children: Vec<_> = parts.iter().map(|(_, _, text)| cx.update(|cx| cx.new(|cx| TextViewState::markdown(text, cx)))).collect();
            cx.run_until_parked();
            let selections = super::super::media::MediaSelections::default();
            for ((block, run, text), child) in parts.iter().zip(&children) {
                selections.register((source.find(block).unwrap(), *run), child, text);
                child.update(cx, |child, cx| child.select_all(cx));
            }
            parent.update(cx, |parent, cx| parent.select_all(cx));
            let document = super::super::media::media_document(source.clone());
            let state = MarkdownState { entity: parent, media: true, extensions: None, selections, document: Some(document) };
            cx.update(|cx| {
                let mut participants: Vec<_> = children.iter().map(|child| child.read(cx).selected_text()).collect();
                let parent = state.entity.read(cx).selected_text();
                if !parent.trim().is_empty() { participants.push(parent); }
                let quote = participants.join("\n");
                let citation = cite(&state, &source, &quote, cx).unwrap_or_else(|| panic!("Cite must restore source order: {source:?}; {quote:?}"));
                assert_eq!(t3_core::assistant_citations::normalize(&citation.text), expected);
                assert_eq!((citation.start, citation.end), range);
                assert!(citation.valid());
                assert_eq!((&*citation.environment_id, &*citation.thread_id, &*citation.message_id), ("environment", "thread", "message"));
                assert!(cite(&state, &source, &format!("{quote}\nother message"), cx).is_none());
            });
        }
    }
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
