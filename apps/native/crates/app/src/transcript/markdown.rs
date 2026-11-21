//! Chat markdown (`apps/web/src/components/ChatMarkdown.tsx`, `index.css` `.chat-markdown`) on
//! gpui-component's `TextView`. One `TextViewState` per message, created when its row first
//! renders; streamed text is appended with `push_str`, which reparses only the last block on a
//! background task. Code blocks are highlighted by tree-sitter and the styles are cached per block.

pub mod blocks;
pub mod links;

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, App, AppContext, ClipboardItem, Entity, EntityId, FontWeight, HighlightStyle, InteractiveElement, IntoElement, ParentElement, SharedString, StyleRefinement, Styled, Subscription, Task, div, px, rems};
use gpui_component::text::{TextView, TextViewState, TextViewStyle};
use gpui_component::{ActiveTheme, h_flex};
use t3_core::markdown::mentions::{collect_composer_model_mentions, link_model_mentions};
use t3_core::markdown::{
    DisplayMarkdownOptions, prepare_display_markdown_for_surface, prepare_display_markdown_for_user,
    strip_code_fence_trailing_newline,
};

use self::blocks::{
    CodeFenceMetaMap, NestedSelection, NestedViews, RichMarkdownMedia, SharedActionContext, SharedCodeMeta, SharedMathEnabled,
    attach_rich_extensions_with_media_options,
};
use self::links::MarkdownActionContext;
use crate::icons::Icon;
use crate::theme::{ThemeSlot, ThemeSlotHoverExt, apply_slot_override, hsla, palette, slot_style, theme_slot_background_layer};

/// States kept regardless of when they were drawn; beyond this, states not drawn in the last
/// few frames (rows the list no longer renders) are dropped and rebuilt if scrolled back to.
const SOFT_LIMIT: usize = 48;
const KEEP_FRAMES: u64 = 2;

struct Cached {
    source_key: Arc<str>,
    text: Arc<str>,
    prepared: Arc<str>,
    is_dark: bool,
    cwd: Option<String>,
    math_enabled: bool,
    mentions_hash: u64,
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
    media_scope: Arc<Mutex<Option<crate::media::MediaScope>>>,
    _repaint: Subscription,
    updates: Option<MediaUpdates>,
}

impl Drop for Cached {
    fn drop(&mut self) {
        // Nested child TextViewStates hold extensions referencing `nested_views`; clear to break the cycle.
        self.nested_views.lock().unwrap().clear();
    }
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
    source_key: Option<Arc<str>>,
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
        let is_user = self.source_key.as_deref().is_some_and(|key| key.starts_with("user:"));
        if let Some(matched) = matched {
            return Some(if is_user {
                t3_core::markdown::clipboard::restore_preserved_leading_whitespace(&matched, source)
            } else {
                matched
            });
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
                NestedSelection::Alert { entity, plain_body, clipboard_markdown } => {
                    let selected = entity.read(cx).selected_text();
                    if !selected.trim().is_empty() && selected == quote {
                        if selected.trim() == plain_body.trim() && !clipboard_markdown.trim().is_empty() {
                            return Some(clipboard_markdown.clone());
                        }
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
                NestedSelection::Alert { entity, .. } => entity.update(cx, |state, cx| state.clear_selection(cx)),
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
            source_key: None,
        }
    }
}

fn model_mention_display_label(instance_id: &str, model: &str, providers: Option<&[t3_core::server_config::ServerProvider]>) -> Option<String> {
    let provider = providers?.iter().find(|p| p.instance_id == instance_id)?;
    let option = provider
        .models
        .iter()
        .find(|m| m.slug == model)
        .or_else(|| provider.models.iter().find(|m| m.slug.eq_ignore_ascii_case(model)))?;
    let name = option.short_name.as_deref().filter(|s| !s.is_empty()).unwrap_or(&option.name);
    Some(format!("@{name}"))
}

fn find_markdown_link_spans(text: &str) -> Vec<(usize, usize, usize, usize, usize, usize)> {
    // Returns (full_start, full_end, label_start, label_end, url_start, url_end)
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'[' && (i == 0 || bytes[i - 1] != b'!') {
            let label_start = i + 1;
            let mut j = label_start;
            while j < bytes.len() && bytes[j] != b']' && bytes[j] != b'\n' && bytes[j] != b'\r' {
                j += 1;
            }
            if j + 1 < bytes.len() && bytes[j] == b']' && bytes[j + 1] == b'(' {
                let label_end = j;
                let url_start = j + 2;
                let mut k = url_start;
                let mut depth = 1usize;
                while k < bytes.len() && depth > 0 && bytes[k] != b'\n' && bytes[k] != b'\r' {
                    match bytes[k] {
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }
                if depth == 0 {
                    out.push((i, k + 1, label_start, label_end, url_start, k));
                    i = k + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

fn rewrite_thread_links_and_prefetch(text: &str, action_ctx: Option<&MarkdownActionContext>, cx: &mut App) -> (Option<String>, u64) {
    if !text.contains("t3-thread://") && !text.contains("app.t3.codes") {
        return (None, 0);
    }
    let Some(ctx) = action_ctx else {
        return (None, 0);
    };
    let spans = find_markdown_link_spans(text);
    if spans.is_empty() {
        return (None, 0);
    }
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    let mut missing_ids: Vec<String> = Vec::new();
    let mut hasher = DefaultHasher::new();

    {
        let app = ctx.state.read(cx);
        let meta_global = cx.try_global::<links::ThreadLinkMetadataGlobal>();
        let active_canonical = app.server_config.config.as_ref().map(|c| c.environment.environment_id.as_str());
        let active_saved = app.environments.active.as_deref();

        for (full_start, full_end, label_start, label_end, url_start, url_end) in spans {
            let label = &text[label_start..label_end];
            let url = &text[url_start..url_end];
            let Some(target) = t3_core::markdown::links::parse_thread_link(url.trim()) else {
                continue;
            };
            target.thread_id.hash(&mut hasher);
            let title = app
                .thread_shell(&target.thread_id)
                .map(|t| t.title.trim().to_owned())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    meta_global
                        .and_then(|g| g.by_thread_id.get(&target.thread_id))
                        .map(|m| m.title.trim().to_owned())
                        .filter(|s| !s.is_empty())
                });
            let is_active_env = target.environment_id.as_deref().is_none_or(|env_id| {
                active_canonical == Some(env_id)
                    || active_saved == Some(env_id)
                    || app
                        .scoped_environment(env_id)
                        .is_some_and(|entry| app.scoped_ready(&entry.summary.owner, t3_core::auth::ORCHESTRATION_READ_SCOPE))
            });
            title.hash(&mut hasher);
            is_active_env.hash(&mut hasher);

            if let Some(resolved_title) = title {
                if label.is_empty() || label == url || label.eq_ignore_ascii_case("Thread") {
                    replacements.push((full_start, full_end, format!("[{resolved_title}]({url})")));
                }
            } else if !is_active_env {
                if label.is_empty() || label == url {
                    let fallback = t3_core::markdown::links::unknown_thread_link_label(false);
                    replacements.push((full_start, full_end, format!("[{fallback}]({url})")));
                }
            } else if meta_global.is_none_or(|g| !g.by_thread_id.contains_key(&target.thread_id))
                && !missing_ids.contains(&target.thread_id)
            {
                missing_ids.push(target.thread_id);
            }
        }
    }

    if !missing_ids.is_empty() {
        links::prefetch_thread_link_metadata(&ctx.state, ctx.owner, missing_ids, cx);
    }

    if replacements.is_empty() {
        return (None, hasher.finish());
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for (start, end, replacement) in replacements {
        out.push_str(&text[cursor..start]);
        out.push_str(&replacement);
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    (Some(out), hasher.finish())
}

fn prepare_surface_markdown(
    key: &str,
    text: &str,
    action_ctx: Option<&MarkdownActionContext>,
    options: DisplayMarkdownOptions<'_>,
    cx: &mut App,
) -> (Arc<str>, u64) {
    let parse_raw_html = !key.starts_with("user:");
    let render_insights = key.starts_with("assistant:");
    let rewrite_mentions = key.starts_with("user:") || key.starts_with("assistant:");

    let (with_mentions, mentions_hash) = if rewrite_mentions {
        let occurrences = collect_composer_model_mentions(text);
        if occurrences.is_empty() {
            (None, 0)
        } else {
            let providers = action_ctx
                .and_then(|c| c.state.read(cx).server_config.config.as_ref())
                .map(|cfg| cfg.providers.as_slice());
            let mut hasher = DefaultHasher::new();
            for occ in &occurrences {
                occ.instance_id.hash(&mut hasher);
                occ.model.hash(&mut hasher);
                model_mention_display_label(occ.instance_id, occ.model, providers).hash(&mut hasher);
            }
            let hash = hasher.finish();
            let linked = link_model_mentions(text, |instance_id, model| {
                model_mention_display_label(instance_id, model, providers)
            });
            (Some(linked), hash)
        }
    } else {
        (None, 0)
    };
    let after_mentions = with_mentions.as_deref().unwrap_or(text);
    let (with_threads, threads_hash) = rewrite_thread_links_and_prefetch(after_mentions, action_ctx, cx);
    let effective = with_threads.as_deref().unwrap_or(after_mentions);
    let prepared = Arc::from(if key.starts_with("user:") {
        prepare_display_markdown_for_user(effective, options)
    } else {
        prepare_display_markdown_for_surface(effective, options, parse_raw_html, render_insights)
    });
    (prepared, mentions_hash ^ threads_hash.rotate_left(17))
}

fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let needle = needle.as_bytes();
    !needle.is_empty() && bytes.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

pub(super) fn has_image_syntax(text: &str) -> bool {
    text.contains("![")
        || contains_ascii_ci(text, "<img")
        || contains_ascii_ci(text, "<video")
        || contains_ascii_ci(text, "<picture")
}

fn has_media_syntax(prepared: &str, parse_raw_html: bool) -> bool {
    prepared.contains("![") || (parse_raw_html && has_image_syntax(prepared))
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
        self.state_with_media_scope(key, text, action_ctx, None, cx)
    }

    /// Like `state_in_context`, plus an optional surface-local `MediaScope` override (for
    /// example, PR markdown enabling `SourceControlMedia` resolution).
    pub fn state_with_media_scope(
        &mut self,
        key: &str,
        text: &Arc<str>,
        action_ctx: Option<MarkdownActionContext>,
        media_scope: Option<crate::media::MediaScope>,
        cx: &mut App,
    ) -> MarkdownState {
        let is_dark = cx.theme().mode.is_dark();
        let cwd = action_ctx.as_ref().and_then(|c| c.cwd.clone());
        let math_enabled = crate::settings::client_settings(cx).math_rendering_enabled();
        let parse_raw_html = !key.starts_with("user:");
        let keep_ordered_list_numbers = key.starts_with("user:");

        if let Some(cached) = self.states.get_mut(key) {
            cached.drawn = self.frame;
            let (prepared_next, next_mentions_hash) = {
                let text_changed = !Arc::ptr_eq(&cached.text, text) && *cached.text != **text;
                let env_changed = cached.is_dark != is_dark || cached.cwd != cwd || cached.math_enabled != math_enabled;
                if text_changed || env_changed || text.contains("model:") || text.contains("t3-thread://") || text.contains("app.t3.codes") {
                    let (prepared, mentions_hash) = prepare_surface_markdown(
                        key,
                        text,
                        action_ctx.as_ref(),
                        DisplayMarkdownOptions { is_dark, cwd: cwd.as_deref(), math_enabled },
                        cx,
                    );
                    if text_changed || env_changed || cached.mentions_hash != mentions_hash {
                        (Some(prepared), mentions_hash)
                    } else {
                        (None, cached.mentions_hash)
                    }
                } else {
                    (None, cached.mentions_hash)
                }
            };
            *cached.action_ctx.lock().unwrap() = action_ctx;
            *cached.media_scope.lock().unwrap() = media_scope;
            if let Some(prepared) = prepared_next {
                let previous_prepared = cached.prepared.clone();
                cached.text = text.clone();
                cached.is_dark = is_dark;
                cached.cwd = cwd;
                cached.math_enabled = math_enabled;
                cached.mentions_hash = next_mentions_hash;
                cached.math_enabled_flag.store(math_enabled, Ordering::Relaxed);
                cached.media |= has_media_syntax(&prepared, parse_raw_html);
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
                source_key: Some(cached.source_key.clone()),
            };
        }

        let source_key: Arc<str> = Arc::from(key);
        let (prepared, mentions_hash) = prepare_surface_markdown(
            key,
            text,
            action_ctx.as_ref(),
            DisplayMarkdownOptions { is_dark, cwd: cwd.as_deref(), math_enabled },
            cx,
        );
        let media = has_media_syntax(&prepared, parse_raw_html);
        let state = cx.new(|cx| TextViewState::markdown(if media { "" } else { &prepared }, cx).selectable(true));
        let owner = self.owner;
        let repaint = cx.observe(&state, move |_, cx| cx.notify(owner));
        let document = super::media::media_document(prepared.clone());
        let selections = super::media::MediaSelections::default();
        let nested_views = NestedViews::default();
        let shared_ctx: SharedActionContext = Arc::new(Mutex::new(action_ctx));
        let shared_media_scope = Arc::new(Mutex::new(media_scope));
        let scope_source = super::media::MediaScopeSource::Override(shared_media_scope.clone());
        let code_meta: SharedCodeMeta = Arc::new(Mutex::new(
            if prepared.contains("```") || prepared.contains("~~~") {
                CodeFenceMetaMap::from_markdown(&prepared)
            } else {
                CodeFenceMetaMap::default()
            },
        ));
        let math_enabled_flag: SharedMathEnabled = Arc::new(AtomicBool::new(math_enabled));
        let media_ext = super::media::markdown_extensions_with_selections(document.clone(), scope_source.clone(), selections.clone());
        let rich_media = RichMarkdownMedia {
            parse_document: document.clone(),
            gallery_document: document.clone(),
            scope: scope_source,
            selections: selections.clone(),
        };
        let extensions = attach_rich_extensions_with_media_options(
            media_ext,
            shared_ctx.clone(),
            nested_views.clone(),
            code_meta.clone(),
            math_enabled_flag.clone(),
            rich_media,
            keep_ordered_list_numbers,
        );
        let updates = media.then(|| MediaUpdates::new(&state, document.clone(), Arc::from(""), prepared.clone(), cx));
        self.states.insert(
            key.to_owned(),
            Cached {
                source_key: source_key.clone(),
                text: text.clone(),
                prepared,
                is_dark,
                cwd,
                math_enabled,
                mentions_hash,
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
                media_scope: shared_media_scope,
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
            source_key: Some(source_key),
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
    if let Some(set) = slot_style(ThemeSlot::ChatMarkdownCodeblock, cx) {
        if !set.base.is_empty() {
            apply_slot_override(&mut code_block, &set.base, cx);
        }
        if matches!(
            set.base.background,
            Some(t3_core::themes::slots::SlotBackground::ThemeBackgroundImage { .. })
        ) && crate::theme::background_image(cx).is_some()
        {
            code_block.background = Some(gpui::Fill::Color(gpui::solid_background(gpui::transparent_black())));
        }
    }
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
    style_markdown_with_source(view, action_ctx, state.code_meta, state.source_key, cx)
}

/// Renders a user-authored markdown message (`user:*`) while intercepting `Copy` (`Cmd+C`)
/// so NBSP indentation injected by `preserve_user_leading_whitespace` is restored back to
/// ASCII spaces/tabs while preserving any literal user-typed NBSPs (`pingdotgg/t3code#38079`).
pub fn user_markdown_view(state: impl Into<MarkdownState>, raw_source: &Arc<str>, cx: &App) -> AnyElement {
    let state = state.into();
    let entity = state.entity.clone();
    let raw_source = raw_source.clone();
    div()
        .w_full()
        .capture_action(move |_: &gpui_component::input::Copy, window, cx| {
            let own = entity.read(cx).selected_text();
            if own.trim().is_empty() {
                cx.propagate();
                return;
            }
            let cleaned_own = t3_core::markdown::clipboard::restore_preserved_leading_whitespace(&own, &raw_source);
            let window_selected = gpui_base::TextSelection::selected_text(window, cx);
            let copied = if window_selected.is_empty() || window_selected.trim() == own.trim() {
                cleaned_own
            } else if window_selected.contains(&own) {
                window_selected.replacen(&own, &cleaned_own, 1)
            } else {
                t3_core::markdown::clipboard::restore_preserved_leading_whitespace(&window_selected, &raw_source)
            };
            let copied = copied.trim_end_matches(['\r', '\n']).to_owned();
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(copied));
            cx.stop_propagation();
        })
        .child(markdown_view(state, cx))
        .into_any_element()
}

pub fn style_markdown(view: TextView, cx: &App) -> TextView {
    style_markdown_with_source(view, None, Default::default(), None, cx)
}

pub(crate) fn style_markdown_internal(
    view: TextView,
    action_ctx: Option<MarkdownActionContext>,
    code_meta: SharedCodeMeta,
    cx: &App,
) -> TextView {
    style_markdown_with_source(view, action_ctx, code_meta, None, cx)
}

fn style_markdown_with_source(
    view: TextView,
    action_ctx: Option<MarkdownActionContext>,
    code_meta: SharedCodeMeta,
    source_key: Option<Arc<str>>,
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
            let copy_code = strip_code_fence_trailing_newline(&code);
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
                .child(super::parts::CopyButton::new("copy-code", "Copy code", move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_code.clone()));
                }))
        })
        .on_link_click(move |url: &SharedString, event, window, cx| {
            links::handle_markdown_link_click_for_source(url, event, link_ctx.as_ref(), source_key.as_deref(), window, cx);
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
        .theme_slot_hover(ThemeSlot::ChatMarkdown, cx)
        .when_some(theme_slot_background_layer(ThemeSlot::ChatMarkdown, cx), |this, layer| {
            this.relative().overflow_hidden().child(layer)
        })
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
            source_key: None,
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
                source_key: None,
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
            source_key: None,
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
            source_key: None,
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
            source_key: None,
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
            source_key: None,
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

    #[gpui::test]
    fn user_messages_skip_raw_html_while_assistant_normalizes_media_tables_and_headings(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let _app_state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| crate::app_state::AppState::new(runtime.handle().clone(), cx))
        });
        let mut store = MarkdownStore::new(EntityId::from(1u64));
        let user_src: Arc<str> = "Look at <img src=\"a.png\" width=\"120\">".into();
        let assistant_src: Arc<str> = concat!(
            "<h1>Shot <img src=\"head.png\" width=\"24\"></h1>\n\n",
            "> [!NOTE]\n> <video src=\"https://example.com/clip\" width=\"320\" height=\"180\"></video>\n\n",
            "<table><tr><th>Item</th></tr><tr><td><img src=\"cell.png\" alt=\"Cell\"></td></tr></table>"
        )
        .into();

        cx.update(|cx| {
            let user_state = store.state_in_context("user:m1", &user_src, None, cx);
            assert!(!user_state.media);
            assert_eq!(store.states.get("user:m1").unwrap().prepared.as_ref(), "Look at <img src=\"a.png\" width=\"120\">");

            let scope = crate::media::MediaScope {
                owner: EntityId::from(1u64),
                thread_id: Some("t1".into()),
                workspace_root: Some("/repo".into()),
                workspace_mutation_id: None,
                source_control_media: true,
                source_control_media_repository: Some("org/repo".into()),
                viewer: gpui::WeakEntity::new_invalid(),
            };
            let assistant_state = store.state_with_media_scope("assistant:m2", &assistant_src, None, Some(scope), cx);
            assert!(assistant_state.media);
            let doc = assistant_state.document.as_ref().unwrap().read().unwrap().clone();
            let gallery = doc.gallery_images();
            let urls: Vec<_> = gallery.iter().map(|(u, _, _)| u.as_str()).collect();
            assert_eq!(urls, vec!["head.png", "https://example.com/clip", "cell.png"]);
            assert!(gallery[1].2.explicit_video);
            assert_eq!(gallery[1].2.width, Some(320));
            assert_eq!(gallery[1].2.height, Some(180));

            let updated_scope = crate::media::MediaScope {
                owner: EntityId::from(1u64),
                thread_id: Some("t1".into()),
                workspace_root: Some("/repo".into()),
                workspace_mutation_id: Some("rev-2".into()),
                source_control_media: true,
                source_control_media_repository: Some("org/other".into()),
                viewer: gpui::WeakEntity::new_invalid(),
            };
            let reused = store.state_with_media_scope("assistant:m2", &assistant_src, None, Some(updated_scope), cx);
            assert_eq!(reused.entity.entity_id(), assistant_state.entity.entity_id());
            let guard = store.states.get("assistant:m2").unwrap().media_scope.lock().unwrap();
            assert_eq!(guard.as_ref().and_then(|s| s.source_control_media_repository.as_deref()), Some("org/other"));
        });
    }

    #[gpui::test]
    fn assistant_insight_callout_preserves_clipboard_markdown_on_full_selection(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
        });

        let mut store = MarkdownStore::new(EntityId::from(1u64));
        let raw_insight = "`★ Insight ─────────────────────────────────────`\nKey finding here.\n`─────────────────────────────────────────────────`";
        let assistant_src: Arc<str> = raw_insight.into();
        let user_src: Arc<str> = raw_insight.into();

        cx.update(|cx| {
            let _user_state = store.state_in_context("user:u-ins", &user_src, None, cx);
            assert!(!store.states.get("user:u-ins").unwrap().prepared.contains("[!INSIGHT]"));

            let _assistant_state = store.state_in_context("assistant:a-ins", &assistant_src, None, cx);
            assert!(store.states.get("assistant:a-ins").unwrap().prepared.contains("[!INSIGHT]"));
        });

        let parent = cx.update(|cx| cx.new(|cx| TextViewState::markdown("", cx)));
        let alert_body = cx.update(|cx| cx.new(|cx| TextViewState::markdown("Key finding here.", cx)));
        cx.run_until_parked();
        let nested_views = NestedViews::default();
        nested_views.lock().unwrap().insert(
            "alert:0".into(),
            NestedSelection::Alert {
                entity: alert_body.clone(),
                plain_body: "Key finding here.".into(),
                clipboard_markdown: raw_insight.into(),
            },
        );
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
            source_key: None,
        };
        alert_body.update(cx, |view, cx| view.select_all(cx));
        cx.update(|cx| {
            let quote = alert_body.read(cx).selected_text();
            let copied = state.selection_text(raw_insight, &quote, cx).expect("alert selection must resolve");
            assert_eq!(copied, raw_insight);
        });
    }

    #[gpui::test]
    fn model_mentions_link_when_server_config_loads(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let app_state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| crate::app_state::AppState::new(runtime.handle().clone(), cx))
        });

        let mut store = MarkdownStore::new(EntityId::from(1u64));
        let src: Arc<str> = "Ask model:claude/claude-sonnet-4 to review.".into();
        let ctx = MarkdownActionContext {
            state: app_state.clone(),
            owner: EntityId::from(1u64),
            cwd: None,
            thread_id: None,
            thread_view: None,
        };

        cx.update(|cx| {
            let _ = store.state_in_context("user:m-mention", &src, Some(ctx.clone()), cx);
            assert!(
                !store.states.get("user:m-mention").unwrap().prepared.contains("t3-model://"),
                "unloaded config keeps raw mention token"
            );

            let cfg: t3_core::server_config::ServerConfig = serde_json::from_value(serde_json::json!({
                "providers": [{
                    "instanceId": "claude",
                    "driver": "claudeAgent",
                    "displayName": "Claude Code",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "models": [{
                        "slug": "claude-sonnet-4",
                        "name": "Claude Sonnet 4",
                        "shortName": "Sonnet 4"
                    }]
                }]
            }))
            .expect("valid ServerConfig");
            app_state.update(cx, |app, _| {
                app.server_config.config = Some(cfg);
            });

            store.begin_frame();
            let _ = store.state_in_context("user:m-mention", &src, Some(ctx), cx);
            let prepared = &store.states.get("user:m-mention").unwrap().prepared;
            assert!(prepared.contains("[@Sonnet 4](t3-model://claude/claude-sonnet-4)"), "got: {prepared}");
        });
    }

    #[gpui::test]
    fn thread_links_resolve_cached_metadata_and_unreachable_machine_fallback(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let app_state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| crate::app_state::AppState::new(runtime.handle().clone(), cx))
        });

        let mut store = MarkdownStore::new(EntityId::from(1u64));
        let src: Arc<str> = "See [](t3-thread://v1/thr-unloaded) and [](t3-thread://v1/env-offline/thr-remote).".into();
        let ctx = MarkdownActionContext {
            state: app_state,
            owner: EntityId::from(1u64),
            cwd: None,
            thread_id: None,
            thread_view: None,
        };

        cx.update(|cx| {
            let mut global = links::ThreadLinkMetadataGlobal::default();
            global.by_thread_id.insert(
                "thr-unloaded".into(),
                t3_core::markdown::links::ThreadLinkMetadata {
                    title: "Fix diff viewer scroll".into(),
                    project_id: Some("p-1".into()),
                },
            );
            cx.set_global(global);
            let _ = store.state_in_context("assistant:a-threads", &src, Some(ctx), cx);
            let prepared = &store.states.get("assistant:a-threads").unwrap().prepared;
            assert!(prepared.contains("[Fix diff viewer scroll](t3-thread://v1/thr-unloaded)"), "got: {prepared}");
            assert!(prepared.contains("[Thread on another machine](t3-thread://v1/env-offline/thr-remote)"), "got: {prepared}");
        });
    }

    #[gpui::test]
    fn user_markdown_copy_restores_leading_spaces_and_preserves_literal_nbsp(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
        });
        struct UserCopyHost {
            state: MarkdownState,
            raw: Arc<str>,
        }
        impl gpui::Render for UserCopyHost {
            fn render(&mut self, _window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
                div().size_full().child(user_markdown_view(&self.state, &self.raw, cx))
            }
        }
        let raw: Arc<str> = Arc::from("  indented line\n\tsecond line\n\u{00A0}\u{00A0}literal nbsp");
        let (view, visual_cx) = cx.add_window_view(|_, cx| {
            let mut store = MarkdownStore::new(cx.entity_id());
            let state = store.state_in_context("user:copy-test", &raw, None, cx);
            UserCopyHost { state, raw: raw.clone() }
        });
        visual_cx.run_until_parked();
        visual_cx.update(|window, cx| {
            window.resize(gpui::size(px(420.), px(200.)));
            window.draw(cx).clear();
        });
        visual_cx.run_until_parked();
        visual_cx.simulate_click(gpui::point(px(12.), px(12.)), gpui::Modifiers::default());
        visual_cx.update(|window, cx| {
            let entity = view.read(cx).state.entity.clone();
            entity.update(cx, |tv, cx| tv.select_all(cx));
            window.dispatch_action(Box::new(gpui_component::input::Copy), cx);
        });
        let copied = visual_cx
            .update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
            .expect("Copy on user_markdown_view must populate clipboard");
        assert_eq!(copied, "  indented line\n\tsecond line\n\u{00A0}\u{00A0}literal nbsp");
    }

    #[gpui::test]
    fn markdown_prose_and_codeblock_apply_theme_css_slot_overrides_cleanly(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
        });
        struct ProseHost {
            state: MarkdownState,
        }
        impl gpui::Render for ProseHost {
            fn render(&mut self, _window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
                div().size_full().child(prose(&self.state, cx))
            }
        }
        let source: Arc<str> = Arc::from("Hello markdown\n\n```rust\nlet x = 42;\n```\n");
        let (_view, vcx) = cx.add_window_view(|_, cx| {
            let mut store = MarkdownStore::new(cx.entity_id());
            let state = store.state("assistant:css-slot-test", &source, cx);
            ProseHost { state }
        });
        vcx.run_until_parked();

        let colors = t3_core::themes::standard(true);
        let css = r#"
            .chat-markdown { font-size: 16px; color: #f0f4ff; }
            .chat-markdown:hover { opacity: 0.98; }
            .chat-markdown-codeblock { background: #141824; border: 2px solid #ff55aa; border-radius: 14px; }
            .chat-markdown-codeblock:hover { border-color: #55ccff; }
            .chat-markdown-codeblock:active { opacity: 0.92; }
        "#;
        vcx.update(|window, cx| {
            crate::theme::preview_with_css(Some((&colors, Some(css), true)), cx);
            let tv_style = style(cx);
            assert_eq!(
                tv_style.code_block.background,
                Some(gpui::Fill::Color(gpui::solid_background(hsla(0x1418_24ff))))
            );
            assert_eq!(tv_style.code_block.border_color, Some(hsla(0xff55_aaff)));
            assert_eq!(tv_style.code_block.corner_radii.top_left, Some(px(14.).into()));
            window.resize(gpui::size(px(480.), px(320.)));
            window.draw(cx).clear();
            window.draw(cx).clear();
        });
        vcx.run_until_parked();

        // Scoped background image makes inner TextView code_block transparent so the outer card layer shows through.
        let bg_image = Arc::new(gpui::RenderImage::new(vec![image::Frame::new(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([0x33, 0x66, 0x99, 0xff]),
        ))]));
        let bg_css = r#"
            .chat-markdown-codeblock { background: var(--theme-background-image) center / cover no-repeat; border-radius: 12px; }
        "#;
        vcx.update(|window, cx| {
            crate::theme::set_background_image(Some(bg_image), cx);
            crate::theme::preview_with_css(Some((&colors, Some(bg_css), true)), cx);
            let tv_style = style(cx);
            assert_eq!(
                tv_style.code_block.background,
                Some(gpui::Fill::Color(gpui::solid_background(gpui::transparent_black())))
            );
            assert!(theme_slot_background_layer(ThemeSlot::ChatMarkdownCodeblock, cx).is_some());
            window.draw(cx).clear();
            crate::theme::preview(None, cx);
            crate::theme::set_background_image(None, cx);
            window.draw(cx).clear();
        });
    }
}
