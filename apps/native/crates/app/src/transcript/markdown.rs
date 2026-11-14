//! Chat markdown (`apps/web/src/components/ChatMarkdown.tsx`, `index.css` `.chat-markdown`) on
//! gpui-component's `TextView`. One `TextViewState` per message, created when its row first
//! renders; streamed text is appended with `push_str`, which reparses only the last block on a
//! background task. Code blocks are highlighted by tree-sitter and the styles are cached per block.

pub mod blocks;
pub mod links;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::prelude::FluentBuilder;
use gpui::{App, AppContext, ClipboardItem, Entity, EntityId, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString, StyleRefinement, Styled, Subscription, Task, div, px, rems};
use gpui_component::text::{TextView, TextViewState, TextViewStyle};
use gpui_component::{ActiveTheme, h_flex};
use t3_core::markdown::{DisplayMarkdownOptions, prepare_display_markdown};

use self::blocks::{
    CodeFenceMetaMap, NestedSelection, NestedViews, SharedActionContext, SharedCodeMeta, SharedMathEnabled, attach_rich_extensions,
};
use self::links::MarkdownActionContext;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// States kept regardless of when they were drawn; beyond this, states not drawn in the last
/// few frames (rows the list no longer renders) are dropped and rebuilt if scrolled back to.
const SOFT_LIMIT: usize = 48;
const KEEP_FRAMES: u64 = 2;

struct Cached {
    text: Arc<str>,
    prepared: Arc<str>,
    is_dark: bool,
    cwd: Option<String>,
    math_enabled: bool,
    math_enabled_flag: SharedMathEnabled,
    state: Entity<TextViewState>,
    drawn: u64,
    /// Set once the text holds image syntax; it stays on so the parse is not redone.
    media: bool,
    document: super::media::MediaDocument,
    extensions: gpui_component::text::MarkdownExtensions,
    selections: super::media::MediaSelections,
    nested_views: NestedViews,
    action_ctx: SharedActionContext,
    code_meta: SharedCodeMeta,
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

/// A message's markdown state, and whether it parses images or rich custom blocks (`super::media`,
/// `super::blocks`). Custom extensions stay attached on `MarkdownStore` entries so alerts,
/// `<details>`, math, Mermaid diagrams, tables, and media reuse one parsed state.
#[derive(Clone)]
pub struct MarkdownState {
    pub entity: Entity<TextViewState>,
    media: bool,
    rich: bool,
    extensions: Option<gpui_component::text::MarkdownExtensions>,
    selections: super::media::MediaSelections,
    document: Option<super::media::MediaDocument>,
    nested_views: NestedViews,
    action_ctx: SharedActionContext,
    code_meta: SharedCodeMeta,
}

impl MarkdownState {
    pub(crate) fn selection_text(&self, source: &str, quote: &str, cx: &App) -> Option<String> {
        if quote.trim().is_empty() { return None; }
        let document = self.document.as_ref().filter(|_| self.media).map(|document| document.read().unwrap().clone());
        let parent = self.entity.read(cx).selected_text();
        let matched = match document {
            Some(document) if &**document.source() == source => self.selections.selection_text(&document, parent.clone(), quote, cx),
            Some(_) => None,
            None => (parent == quote).then_some(parent),
        };
        if matched.is_some() {
            return matched;
        }
        let nested = self.nested_views.lock().unwrap();
        for child in nested.values() {
            match child {
                NestedSelection::Markdown(child) => {
                    let selected = child.read(cx).selected_text();
                    if !selected.trim().is_empty() && selected == quote {
                        return Some(selected);
                    }
                }
                NestedSelection::Selectable { handle, rendered, raw_fallback } => {
                    let Some(selected) = handle.selected_text(cx) else {
                        continue;
                    };
                    if !selected.trim().is_empty() && selected == quote {
                        if *selected == *rendered
                            && let Some(raw) = raw_fallback.as_ref().filter(|raw| !raw.trim().is_empty())
                        {
                            return Some(raw.clone());
                        }
                        return Some(selected);
                    }
                }
            }
        }
        None
    }

    #[cfg(test)]
    pub(crate) fn clear_nested_selections(&self, cx: &mut App) {
        let children: Vec<_> = self.nested_views.lock().unwrap().values().cloned().collect();
        for child in children {
            match child {
                NestedSelection::Markdown(child) => child.update(cx, |state, cx| state.clear_selection(cx)),
                NestedSelection::Selectable { handle, .. } => handle.clear_selection(cx),
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn with_nested_views_for_test(mut self, nested_views: NestedViews) -> Self {
        self.nested_views = nested_views;
        self
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
        MarkdownState {
            entity: entity.clone(),
            media: false,
            rich: false,
            extensions: None,
            selections: Default::default(),
            document: None,
            nested_views: Default::default(),
            action_ctx: Default::default(),
            code_meta: Default::default(),
        }
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
        self.state_in_context(key, text, None, cx)
    }

    /// The state for `key`, with an optional `MarkdownActionContext` for file/thread links and shell execution.
    pub fn state_in_context(
        &mut self,
        key: &str,
        text: &Arc<str>,
        action_ctx: Option<MarkdownActionContext>,
        cx: &mut App,
    ) -> MarkdownState {
        let is_dark = cx.theme().mode.is_dark();
        let cwd = action_ctx.as_ref().and_then(|c| c.cwd.clone());
        let math_enabled = crate::settings::client_settings(cx).math_rendering_enabled();

        if let Some(cached) = self.states.get_mut(key) {
            cached.drawn = self.frame;
            *cached.action_ctx.lock().unwrap() = action_ctx;
            let text_changed = !Arc::ptr_eq(&cached.text, text) && *cached.text != **text;
            let env_changed = cached.is_dark != is_dark || cached.cwd != cwd || cached.math_enabled != math_enabled;
            if text_changed || env_changed {
                let previous_prepared = cached.prepared.clone();
                let prepared: Arc<str> = Arc::from(prepare_display_markdown(
                    text,
                    DisplayMarkdownOptions { is_dark, cwd: cwd.as_deref(), math_enabled },
                ));
                cached.text = text.clone();
                cached.is_dark = is_dark;
                cached.cwd = cwd;
                cached.math_enabled = math_enabled;
                cached.math_enabled_flag.store(math_enabled, Ordering::Relaxed);
                cached.media |= has_image_syntax(&prepared);
                if *cached.prepared != *prepared {
                    cached.prepared = prepared.clone();
                    if prepared.contains("```") || prepared.contains("~~~") {
                        *cached.code_meta.lock().unwrap() = CodeFenceMetaMap::from_markdown(&prepared);
                    } else {
                        *cached.code_meta.lock().unwrap() = CodeFenceMetaMap::default();
                    }
                    if cached.media {
                        if let Some(updates) = &cached.updates {
                            updates.source.send_replace(prepared);
                        } else {
                            cached.updates = Some(MediaUpdates::new(
                                &cached.state,
                                cached.document.clone(),
                                previous_prepared,
                                prepared,
                                cx,
                            ));
                        }
                    } else {
                        cached.state.update(cx, |state, cx| match prepared.strip_prefix(&*previous_prepared) {
                            Some(delta) if !previous_prepared.is_empty() => state.push_str(delta, cx),
                            _ => state.set_text(&prepared, cx),
                        });
                    }
                }
            }
            return MarkdownState {
                entity: cached.state.clone(),
                media: cached.media,
                rich: true,
                extensions: Some(cached.extensions.clone()),
                selections: cached.selections.clone(),
                document: Some(cached.document.clone()),
                nested_views: cached.nested_views.clone(),
                action_ctx: cached.action_ctx.clone(),
                code_meta: cached.code_meta.clone(),
            };
        }

        let prepared: Arc<str> = Arc::from(prepare_display_markdown(
            text,
            DisplayMarkdownOptions { is_dark, cwd: cwd.as_deref(), math_enabled },
        ));
        let media = has_image_syntax(&prepared);
        let state = cx.new(|cx| TextViewState::markdown(if media { "" } else { &prepared }, cx).selectable(true));
        let owner = self.owner;
        let repaint = cx.observe(&state, move |_, cx| cx.notify(owner));
        let document = super::media::media_document(prepared.clone());
        let selections = super::media::MediaSelections::default();
        let nested_views = NestedViews::default();
        let shared_ctx: SharedActionContext = Arc::new(Mutex::new(action_ctx));
        let code_meta: SharedCodeMeta = Arc::new(Mutex::new(
            if prepared.contains("```") || prepared.contains("~~~") {
                CodeFenceMetaMap::from_markdown(&prepared)
            } else {
                CodeFenceMetaMap::default()
            },
        ));
        let math_enabled_flag: SharedMathEnabled = Arc::new(AtomicBool::new(math_enabled));
        let media_ext = super::media::markdown_extensions_with_selections(document.clone(), None, selections.clone());
        let extensions = attach_rich_extensions(
            media_ext,
            shared_ctx.clone(),
            nested_views.clone(),
            code_meta.clone(),
            math_enabled_flag.clone(),
        );
        let updates = media.then(|| MediaUpdates::new(&state, document.clone(), Arc::from(""), prepared.clone(), cx));
        self.states.insert(
            key.to_owned(),
            Cached {
                text: text.clone(),
                prepared,
                is_dark,
                cwd,
                math_enabled,
                math_enabled_flag,
                state: state.clone(),
                drawn: self.frame,
                media,
                document: document.clone(),
                extensions: extensions.clone(),
                selections: selections.clone(),
                nested_views: nested_views.clone(),
                action_ctx: shared_ctx.clone(),
                code_meta: code_meta.clone(),
                _repaint: repaint,
                updates,
            },
        );
        MarkdownState {
            entity: state,
            media,
            rich: true,
            extensions: Some(extensions),
            selections,
            document: Some(document),
            nested_views,
            action_ctx: shared_ctx,
            code_meta,
        }
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
    let mut table = StyleRefinement::default();
    table.overflow.x = Some(gpui::Overflow::Scroll);
    TextViewStyle {
        paragraph_gap: rems(PARAGRAPH_GAP),
        heading_base_font_size: px(14.),
        highlight_theme: theme.highlight_theme.clone(),
        code_block,
        table,
        inline_code: HighlightStyle { background_color: Some(hsla(p.inline_code_background)), ..HighlightStyle::default() },
        is_dark: theme.mode.is_dark(),
        ..TextViewStyle::default()
    }
    .heading_font_size(|level, _| heading_font_size(level))
}

/// The prose view: §7.6 rhythm, §7.8 code blocks with a language label, fence title, Run in terminal, and Copy.
pub fn markdown_view(state: impl Into<MarkdownState>, cx: &App) -> TextView {
    let state = state.into();
    let view = TextView::new(&state.entity);
    let view = if state.media || state.rich {
        if let Some(extensions) = state.extensions {
            view.markdown_extensions(extensions)
        } else {
            view
        }
    } else {
        view
    };
    let action_ctx = state.action_ctx.lock().unwrap().clone();
    style_markdown_internal(view, action_ctx, state.code_meta, cx)
}

pub fn style_markdown(view: TextView, cx: &App) -> TextView {
    style_markdown_internal(view, None, Default::default(), cx)
}

pub(crate) fn style_markdown_internal(
    view: TextView,
    action_ctx: Option<MarkdownActionContext>,
    code_meta: SharedCodeMeta,
    cx: &App,
) -> TextView {
    let link_ctx = action_ctx.clone();
    view.style(style(cx))
        .selectable(true)
        .code_block_actions(move |block, _window, cx| {
            let p = palette(cx);
            let code = block.code();
            let raw_lang = block.lang().filter(|lang| !lang.is_empty());
            let meta = code_meta.lock().unwrap().lookup(raw_lang.as_deref(), &code).cloned();
            let display_lang = meta
                .as_ref()
                .and_then(|m| m.display_lang.clone())
                .or_else(|| raw_lang.as_ref().map(|s| s.to_string()));
            let title = meta.as_ref().and_then(|m| m.title.clone());
            let can_run = t3_core::markdown::code::can_run_shell_command(
                raw_lang.as_deref(),
                &code,
                meta.as_ref().is_some_and(|m| m.closed),
            ) && action_ctx.as_ref().is_some_and(|c| c.cwd.is_some() && c.thread_id.is_some());
            let run_ctx = action_ctx.clone();
            let run_code = code.to_string();
            let copy_code = t3_core::markdown::normalize_clipboard_text(&code);
            let copy_app = action_ctx.as_ref().map(|c| c.state.clone());
            h_flex()
                .gap(px(4.))
                .items_center()
                .children(title.map(|title| {
                    div()
                        .px(px(6.))
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground).opacity(0.84))
                        .child(title)
                }))
                .children(display_lang.map(|lang| {
                    div()
                        .px(px(6.))
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(px(11.))
                        .text_color(hsla(p.foreground).opacity(0.72))
                        .child(lang)
                }))
                .when(can_run, |row| {
                    row.child(super::parts::ghost_icon_button(
                        "run-code",
                        Icon::Play,
                        move |_, window, cx| {
                            links::run_shell_code_block(&run_code, run_ctx.as_ref(), window, cx);
                        },
                        cx,
                    ))
                })
                .child(super::parts::ghost_icon_button(
                    "copy-code",
                    Icon::Copy,
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_code.clone()));
                        if let Some(app) = &copy_app {
                            app.update(cx, |a, cx| {
                                a.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Success, "Copied code"), cx);
                            });
                        }
                    },
                    cx,
                ))
        })
        .on_link_click(move |url: &SharedString, event, window, cx| {
            links::handle_markdown_link_click(url, event, link_ctx.as_ref(), window, cx);
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
        let state = MarkdownState {
            entity: parent,
            media: true,
            rich: false,
            extensions: None,
            selections,
            document: Some(document.clone()),
            nested_views: Default::default(),
            action_ctx: Default::default(),
            code_meta: Default::default(),
        };
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
            let state = MarkdownState {
                entity: parent,
                media: true,
                rich: false,
                extensions: None,
                selections,
                document: Some(document),
                nested_views: Default::default(),
                action_ctx: Default::default(),
                code_meta: Default::default(),
            };
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

    #[gpui::test]
    fn nested_view_selection_reaches_citation_action_and_clears(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let source: Arc<str> = "> [!NOTE]\n> Quoted inside alert".into();
        let parent = cx.update(|cx| cx.new(|cx| TextViewState::markdown("", cx)));
        let nested = cx.update(|cx| cx.new(|cx| TextViewState::markdown("Quoted inside alert", cx)));
        cx.run_until_parked();
        let nested_views = NestedViews::default();
        nested_views.lock().unwrap().insert("alert:0".into(), NestedSelection::Markdown(nested.clone()));
        let state = MarkdownState {
            entity: parent,
            media: false,
            rich: true,
            extensions: None,
            selections: Default::default(),
            document: None,
            nested_views,
            action_ctx: Default::default(),
            code_meta: Default::default(),
        };
        nested.update(cx, |view, cx| view.select_all(cx));
        cx.update(|cx| {
            let quote = nested.read(cx).selected_text();
            let citation = crate::thread_view::ThreadView::citation_from_markdown_selection(
                &state,
                &source,
                &quote,
                ("env".into(), "t1".into(), "m1".into()),
                cx,
            )
            .expect("nested view selection must resolve to citation");
            assert_eq!(citation.text.trim(), "Quoted inside alert");
        });
        cx.update(|cx| state.clear_nested_selections(cx));
        cx.update(|cx| assert!(nested.read(cx).selected_text().is_empty()));
    }

    #[gpui::test]
    fn selectable_text_nested_selection_reaches_citation_and_clears(cx: &mut gpui::TestAppContext) {
        use crate::selectable_text::{SelectableTextHandle, active_selected_text};

        cx.update(gpui_component::init);
        let parent = cx.update(|cx| cx.new(|cx| TextViewState::markdown("", cx)));
        cx.run_until_parked();

        // Math block: rendered unicode maps back to raw TeX source when selected in full.
        let math_source: Arc<str> = "Before\n\n$$\nx^2 + y^2 = z^2\n$$\n\nAfter".into();
        let rendered_math = t3_core::markdown::math::render_tex_unicode("x^2 + y^2 = z^2");
        let math_handle = cx.update(|cx| SelectableTextHandle::new_for_test(rendered_math.clone(), cx));
        let math_views = NestedViews::default();
        math_views.lock().unwrap().insert(
            "math:0".into(),
            NestedSelection::Selectable {
                handle: math_handle.clone(),
                rendered: rendered_math.clone(),
                raw_fallback: Some("x^2 + y^2 = z^2".into()),
            },
        );
        let math_state = MarkdownState {
            entity: parent.clone(),
            media: false,
            rich: true,
            extensions: None,
            selections: Default::default(),
            document: None,
            nested_views: math_views,
            action_ctx: Default::default(),
            code_meta: Default::default(),
        };
        cx.update(|cx| math_handle.select_all(cx));
        cx.update(|cx| {
            let quote = active_selected_text(cx).expect("active selectable text must be exposed");
            assert_eq!(quote, rendered_math);
            let citation = crate::thread_view::ThreadView::citation_from_markdown_selection(
                &math_state,
                &math_source,
                &quote,
                ("env".into(), "t1".into(), "m1".into()),
                cx,
            )
            .expect("math SelectableText selection must resolve to raw TeX citation");
            assert_eq!(citation.text.trim(), "x^2 + y^2 = z^2");
        });
        cx.update(|cx| math_state.clear_nested_selections(cx));
        cx.update(|cx| {
            assert!(math_handle.selected_text(cx).is_none());
            assert!(active_selected_text(cx).is_none());
        });

        // Mermaid code fallback: selected source code resolves to citation cleanly.
        let mermaid_code = "graph TD\n  A --> B";
        let mermaid_source: Arc<str> = format!("```mermaid\n{mermaid_code}\n```").into();
        let mermaid_handle = cx.update(|cx| SelectableTextHandle::new_for_test(mermaid_code, cx));
        let mermaid_views = NestedViews::default();
        mermaid_views.lock().unwrap().insert(
            "mermaid:0".into(),
            NestedSelection::Selectable {
                handle: mermaid_handle.clone(),
                rendered: mermaid_code.into(),
                raw_fallback: None,
            },
        );
        let mermaid_state = MarkdownState {
            entity: parent,
            media: false,
            rich: true,
            extensions: None,
            selections: Default::default(),
            document: None,
            nested_views: mermaid_views,
            action_ctx: Default::default(),
            code_meta: Default::default(),
        };
        cx.update(|cx| mermaid_handle.select_all(cx));
        cx.update(|cx| {
            let quote = active_selected_text(cx).expect("mermaid selectable text must be active");
            let citation = crate::thread_view::ThreadView::citation_from_markdown_selection(
                &mermaid_state,
                &mermaid_source,
                &quote,
                ("env".into(), "t1".into(), "m2".into()),
                cx,
            )
            .expect("mermaid SelectableText selection must resolve to citation");
            assert_eq!(citation.text.trim(), mermaid_code);
        });
        cx.update(|cx| mermaid_state.clear_nested_selections(cx));
        cx.update(|cx| assert!(active_selected_text(cx).is_none()));
    }

    #[gpui::test]
    fn math_rendering_setting_defaults_false_and_respects_toggle(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let app_state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| crate::app_state::AppState::new(runtime.handle().clone(), cx))
        });
        let mut store = MarkdownStore::new(EntityId::from(1u64));
        let source: Arc<str> = "Before\n\n$$\nx^2 + y^2\n$$\n\nAfter".into();

        // Commit explicit false first so ambient settings cannot affect default behavior, then toggle true.
        cx.update(|cx| {
            assert!(!t3_core::client_settings::ClientSettingsDoc::default().math_rendering_enabled());
            crate::settings::store::install_client_commit(
                &app_state,
                t3_client::store::ClientCommit {
                    sequence: 1,
                    patch: serde_json::Map::from_iter([("mathRenderingEnabled".into(), serde_json::Value::Bool(false))]),
                },
                cx,
            );
            let _state = store.state_in_context("math-row", &source, None, cx);
            let cached = store.states.get("math-row").unwrap();
            assert!(!cached.prepared.contains("```t3-math"));
            assert!(!cached.math_enabled);
            assert!(!cached.math_enabled_flag.load(Ordering::Relaxed));

            crate::settings::store::install_client_commit(
                &app_state,
                t3_client::store::ClientCommit {
                    sequence: 2,
                    patch: serde_json::Map::from_iter([("mathRenderingEnabled".into(), serde_json::Value::Bool(true))]),
                },
                cx,
            );
            store.begin_frame();
            let _state = store.state_in_context("math-row", &source, None, cx);
            let cached = store.states.get("math-row").unwrap();
            assert!(cached.prepared.contains("```t3-math"));
            assert!(cached.math_enabled);
            assert!(cached.math_enabled_flag.load(Ordering::Relaxed));

            crate::settings::store::install_client_commit(
                &app_state,
                t3_client::store::ClientCommit {
                    sequence: 3,
                    patch: serde_json::Map::from_iter([("mathRenderingEnabled".into(), serde_json::Value::Bool(false))]),
                },
                cx,
            );
            store.begin_frame();
            let _state = store.state_in_context("math-row", &source, None, cx);
            let cached = store.states.get("math-row").unwrap();
            assert!(!cached.prepared.contains("```t3-math"));
            assert!(!cached.math_enabled);
            assert!(!cached.math_enabled_flag.load(Ordering::Relaxed));
        });
    }

    #[gpui::test]
    fn compact_details_citation_matches_rendered_markdown(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let source: Arc<str> = "<details><summary>Reasoning</summary>\nQuoted inside compact details\n</details>".into();
        let parent = cx.update(|cx| cx.new(|cx| TextViewState::markdown("", cx)));
        let nested = cx.update(|cx| cx.new(|cx| TextViewState::markdown("Quoted inside compact details", cx)));
        cx.run_until_parked();
        let nested_views = NestedViews::default();
        nested_views.lock().unwrap().insert("details:0".into(), NestedSelection::Markdown(nested.clone()));
        let state = MarkdownState {
            entity: parent,
            media: false,
            rich: true,
            extensions: None,
            selections: Default::default(),
            document: None,
            nested_views,
            action_ctx: Default::default(),
            code_meta: Default::default(),
        };
        nested.update(cx, |view, cx| view.select_all(cx));
        cx.update(|cx| {
            let quote = nested.read(cx).selected_text();
            let citation = crate::thread_view::ThreadView::citation_from_markdown_selection(
                &state,
                &source,
                &quote,
                ("env".into(), "t1".into(), "m3".into()),
                cx,
            )
            .expect("compact <details> block selection must resolve to citation");
            assert_eq!(citation.text.trim(), "Quoted inside compact details");
        });
    }
}
