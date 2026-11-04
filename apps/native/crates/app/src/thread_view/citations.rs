//! `AssistantSelectionToolbar.tsx` and `citationProcedure.ts`, using TextView's shared
//! selection (including links and highlighted code) rather than flattening assistant markdown.

use super::ThreadView;
use crate::app_state::AppEvent;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};
use gpui::{AnyElement, Context, FocusHandle, IntoElement, ParentElement, Pixels, Point, Styled, Window, div, prelude::*, px};
use gpui_base::TextSelection;
use crate::transcript::markdown::MarkdownState;
use std::sync::Arc;
use t3_core::assistant_citations::{self as rules, Citation, CitedMessage, HistoryAction};
use t3_core::timeline::Role;

pub(super) struct CitationUi {
    selection: Option<(Citation, Point<Pixels>)>,
    selection_connection: Option<u64>,
    focus: FocusHandle,
    pub(super) selection_focus: FocusHandle,
    pending: Option<(Citation, rules::CitationProcedure)>,
    pub(super) highlighted: Option<String>,
}
impl CitationUi {
    pub fn new(cx: &mut Context<ThreadView>) -> Self {
        Self { selection: None, selection_connection: None, focus: cx.focus_handle(), selection_focus: cx.focus_handle(), pending: None, highlighted: None }
    }
}

impl ThreadView {
    pub(crate) fn citation_from_markdown_selection(selected: &MarkdownState, source: &str, quote: &str, identity: (String, String, String), cx: &gpui::App) -> Option<Citation> {
        let quote = selected.selection_text(source, quote, cx)?;
        Citation::from_rendered_selection(identity.0, identity.1, identity.2, &rules::markdown_text(source), &quote)
    }

    pub(crate) fn capture_citation_selection(
        &mut self,
        selected: MarkdownState,
        message: String,
        source: Arc<str>,
        pointer: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity().downgrade();
        let thread = self.thread_id.clone();
        let connection = self.state.read(cx).connection_id();
        let environment = self.state.read(cx).server_config.config.as_ref().map(|config| config.environment.environment_id.clone()).unwrap_or_default();
        // Selection ranges settle during paint. Read once on the next frame, never on a timer.
        window.on_next_frame(move |window, cx| {
            let _ = owner.update(cx, |this, cx| {
                if thread != this.thread_id || connection.is_none() || connection != this.state.read(cx).connection_id() {
                    return;
                }
                let current = this.entries_cache.entries().iter().any(|entry| matches!(&entry.kind,
                    t3_core::timeline::EntryKind::Message(current) if current.id == message && *current.text == *source));
                if !current { return; }
                let quote = TextSelection::selected_text(window, cx);
                let Some(citation) = Self::citation_from_markdown_selection(&selected, &source, &quote, (environment, thread.unwrap_or_default(), message), cx) else { return };
                this.citation_ui.selection = Some((citation, pointer));
                this.citation_ui.selection_connection = connection;
                window.focus(&this.citation_ui.selection_focus, cx);
                cx.notify();
            });
        });
    }

    pub(super) fn dismiss_citation_selection(&mut self, cx: &mut Context<Self>) {
        if self.citation_ui.selection.take().is_some() || self.citation_ui.highlighted.take().is_some() {
            cx.notify();
        }
        self.citation_ui.pending = None;
    }

    pub(super) fn render_citation_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (citation, pointer) = self.citation_ui.selection.as_ref()?;
        if self.thread_id.as_deref() != Some(&citation.thread_id) || self.citation_ui.selection_connection != self.state.read(cx).connection_id() {
            return None;
        }
        let too_long = rules::utf16_len(&citation.text) > rules::MAX_TEXT;
        let p = palette(cx);
        let cite = citation.clone();
        let bounds = window.viewport_size();
        let x = f32::from(pointer.x).clamp(8., (f32::from(bounds.width) - 136.).max(8.));
        let y = (f32::from(pointer.y) + 12.).clamp(8., (f32::from(bounds.height) - 32.).max(8.));
        Some(
            gpui::deferred(
                gpui::anchored().position_mode(gpui::AnchoredPositionMode::Window).position(gpui::point(px(x), px(y))).snap_to_window_with_margin(px(8.)).child(
                    div()
                        .id("assistant-selection-toolbar")
                        .occlude()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_citation_selection(cx)))
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                this.dismiss_citation_selection(cx);
                                TextSelection::clear(window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(
                            gpui_component::h_flex()
                                .id("cite-selection")
                                .track_focus(&self.citation_ui.focus)
                                .tab_index(if too_long { -1 } else { 0 })
                                .h(px(24.))
                                .px(px(7.))
                                .gap(px(4.))
                                .rounded(px(8.))
                                .border_1()
                                .border_color(hsla(p.border))
                                .bg(hsla(p.popover))
                                .shadow_sm()
                                .text_size(px(12.))
                                .text_color(hsla(p.foreground))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .when(too_long, |this| this.opacity(0.64))
                                .when(!too_long, |this| {
                                    this.cursor_pointer().hover(|style| style.bg(hsla(p.accent))).on_click(cx.listener(move |this, _, window, cx| {
                                        this.cite_selection(&cite, window, cx);
                                    }))
                                })
                                .child(Icon::Quote.render(px(14.), hsla(p.foreground)))
                                .child(if too_long { "Shorten selection" } else { "Cite" }),
                        ),
                ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    pub(super) fn open_citation_source(&mut self, cx: &mut Context<Self>) {
        let request = self.state.read(cx).citation_source.clone();
        if let Some(citation) = request.filter(|c| self.thread_id.as_deref() == Some(&c.thread_id)) {
            self.disclosure.cancel();
            self.state.update(cx, |state, _| state.citation_source = None);
            self.citation_ui.pending = Some((citation, rules::CitationProcedure::default()));
            self.advance_citation_source(cx);
        }
    }

    pub(super) fn advance_citation_source(&mut self, cx: &mut Context<Self>) {
        let Some((citation, mut procedure)) = self.citation_ui.pending.clone() else {
            return;
        };
        if self.thread_id.as_deref() != Some(&citation.thread_id) {
            self.citation_ui.pending = None;
            return;
        }
        let thread = self.thread_state(self.state.read(cx));
        if (thread.projection.is_none() && !thread.deleted) || thread.history.loading || thread.is_merging_history() {
            return;
        }
        let message = self.entries_cache.entries().iter().find_map(|entry| match &entry.kind {
            t3_core::timeline::EntryKind::Message(message) if message.id == citation.message_id => Some(message.role),
            _ => None,
        });
        let message = match message {
            Some(Role::Assistant) => CitedMessage::Assistant,
            Some(_) => CitedMessage::Other,
            None => CitedMessage::Missing,
        };
        match procedure.step(message, thread.history.has_more_history && thread.history.history_cursor.is_some(), thread.history.error.is_some()) {
            HistoryAction::Load => {
                self.citation_ui.pending = Some((citation, procedure));
                self.load_earlier(cx);
            }
            HistoryAction::Found => {
                self.citation_ui.pending = None;
                self.citation_ui.highlighted = Some(citation.message_id.clone());
                self.reveal_entry(&citation.message_id, cx);
            }
            HistoryAction::Failed(failure) => {
                self.citation_ui.pending = None;
                let (title, description) = failure.copy();
                self.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Warning, title).description(description), cx));
            }
            HistoryAction::Aborted => self.citation_ui.pending = None,
        }
    }

    fn cite_selection(&mut self, citation: &Citation, window: &mut Window, cx: &mut Context<Self>) {
        if !citation.valid() || self.thread_id.as_deref() != Some(&citation.thread_id)
            || self.citation_ui.selection_connection.is_none() || self.citation_ui.selection_connection != self.state.read(cx).connection_id() { return; }
        self.state.update(cx, |state, cx| { state.composer_citation = Some(citation.clone()); cx.emit(AppEvent::ComposerCitation); });
        TextSelection::clear(window, cx);
        self.dismiss_citation_selection(cx);
    }

    pub(super) fn citation_key(&mut self, event: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        if event.key == "escape" && (self.citation_ui.pending.as_mut().is_some_and(|(_, procedure)| procedure.escape(false))
            || self.citation_ui.highlighted.is_some()) {
            self.dismiss_citation_selection(cx);
            return;
        }
        if self.citation_ui.selection.is_none()
            || !(self.citation_ui.selection_focus.is_focused(window) || self.citation_ui.focus.is_focused(window)) {
            return;
        }
        if event.key == "tab" && !event.modifiers.shift && !self.citation_ui.focus.is_focused(window)
            && self.citation_ui.selection.as_ref().is_some_and(|(c, _)| rules::utf16_len(&c.text) <= rules::MAX_TEXT) {
            window.focus(&self.citation_ui.focus, cx);
            cx.stop_propagation();
        }
        if event.key == "enter" && self.citation_ui.focus.is_focused(window)
            && let Some((citation, _)) = self.citation_ui.selection.clone() {
            self.cite_selection(&citation, window, cx);
            cx.stop_propagation();
        }
        if event.key == "escape" {
            self.dismiss_citation_selection(cx);
            TextSelection::clear(window, cx);
            cx.stop_propagation();
        }
    }
}
