//! `AssistantSelectionToolbar.tsx` and `citationProcedure.ts`, using TextView's shared
//! selection (including links and highlighted code) rather than flattening assistant markdown.

use super::ThreadView;
use crate::app_state::AppEvent;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};
use crate::transcript::markdown::{MarkdownState, links::{CitationPopoverGlobal, close_sent_citation_popover, navigate_to_citation_source}};
use gpui::{AnyElement, Bounds, ClipboardItem, Context, FocusHandle, IntoElement, ParentElement, Pixels, Point, Styled, Window, div, prelude::*, px};
use gpui_base::TextSelection;
use std::sync::Arc;
use t3_core::assistant_citations::{self as rules, Citation, CitedMessage, HistoryAction, SelectionHint};
use t3_core::timeline::Role;

fn current_selection_quote(window: &mut Window, cx: &mut gpui::App) -> String {
    let tv = TextSelection::selected_text(window, cx);
    if tv.trim().is_empty() {
        crate::selectable_text::active_selected_text(cx).unwrap_or_default()
    } else {
        tv
    }
}

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
    #[cfg(test)]
    pub(crate) fn citation_from_markdown_selection(selected: &MarkdownState, source: &str, quote: &str, identity: (String, String, String), cx: &gpui::App) -> Option<Citation> {
        let quote = selected.selection_text(source, quote, cx)?;
        Citation::from_rendered_selection(identity.0, identity.1, identity.2, &rules::markdown_text(source), &quote)
    }

    pub(crate) fn citation_from_markdown_selection_with_hint(
        selected: &MarkdownState,
        source: &str,
        quote: &str,
        identity: (String, String, String),
        hint: &SelectionHint<'_>,
        cx: &gpui::App,
    ) -> Option<Citation> {
        let quote = selected.selection_text(source, quote, cx)?;
        Citation::from_rendered_selection_with_hint(identity.0, identity.1, identity.2, &rules::markdown_text(source), &quote, hint)
    }

    pub(crate) fn capture_citation_selection(
        &mut self,
        selected: MarkdownState,
        message: String,
        source: Arc<str>,
        (pointer, hint_bounds): (Point<Pixels>, Option<Bounds<Pixels>>),
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
                    t3_core::timeline::EntryKind::Message(current) if current.id == message && (*current.text == *source || current.text.contains(&*source))));
                if !current { return; }
                let quote = current_selection_quote(window, cx);
                let (vertical_fraction, horizontal_fraction) = match hint_bounds {
                    Some(bounds) if f32::from(bounds.size.height) > 1.0 => (
                        Some(((f32::from(pointer.y) - f32::from(bounds.origin.y)) / f32::from(bounds.size.height)).clamp(0.0, 1.0)),
                        Some(((f32::from(pointer.x) - f32::from(bounds.origin.x)) / f32::from(bounds.size.width).max(1.0)).clamp(0.0, 1.0)),
                    ),
                    _ => (None, None),
                };
                let hint = SelectionHint {
                    byte_range: None,
                    source_quote: Some(&quote),
                    vertical_fraction,
                    horizontal_fraction,
                };
                let Some(citation) = Self::citation_from_markdown_selection_with_hint(&selected, &source, &quote, (environment, thread.unwrap_or_default(), message), &hint, cx) else { return };
                this.citation_ui.selection = Some((citation, pointer));
                this.citation_ui.selection_connection = connection;
                window.focus(&this.citation_ui.selection_focus, cx);
                cx.notify();
            });
        });
    }

    pub(super) fn dismiss_citation_selection(&mut self, cx: &mut Context<Self>) {
        close_sent_citation_popover(cx);
        if self.citation_ui.selection.take().is_some() || self.citation_ui.highlighted.take().is_some() {
            cx.notify();
        }
        self.citation_ui.pending = None;
    }

    pub(super) fn render_citation_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let popover = self.render_sent_citation_popover(window, cx);
        let toolbar = self.render_selection_toolbar(window, cx);
        match (toolbar, popover) {
            (None, None) => None,
            (Some(a), None) | (None, Some(a)) => Some(a),
            (Some(a), Some(b)) => Some(div().child(a).child(b).into_any_element()),
        }
    }

    fn render_sent_citation_popover(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let active = cx
            .try_global::<CitationPopoverGlobal>()
            .and_then(|g| g.active.as_ref())
            .filter(|a| a.owner == cx.entity().entity_id())?
            .clone();
        let p = palette(cx);
        let bounds = window.viewport_size();
        let x = f32::from(active.position.x).clamp(8., (f32::from(bounds.width) - 368.).max(8.));
        let y = (f32::from(active.position.y) + 12.).clamp(8., (f32::from(bounds.height) - 220.).max(8.));
        let copy_text = active.citation.text.clone();
        let copy_state = active.state.clone();
        let go_citation = active.citation.clone();
        let go_state = active.state.clone();
        let comment = active.citation.comment.clone().filter(|c| !c.trim().is_empty());

        Some(
            gpui::deferred(
                gpui::anchored()
                    .position_mode(gpui::AnchoredPositionMode::Window)
                    .position(gpui::point(px(x), px(y)))
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        gpui_component::v_flex()
                            .id("sent-citation-popover")
                            .w(px(340.))
                            .max_w(px(360.))
                            .p(px(12.))
                            .gap(px(8.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(hsla(p.border))
                            .bg(hsla(p.popover))
                            .shadow_sm()
                            .occlude()
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_mouse_down_out(|_, _, cx| close_sent_citation_popover(cx))
                            .on_key_down(|event: &gpui::KeyDownEvent, _, cx| {
                                if event.keystroke.key == "escape" {
                                    close_sent_citation_popover(cx);
                                    cx.stop_propagation();
                                }
                            })
                            .child(
                                gpui_component::h_flex()
                                    .justify_between()
                                    .items_center()
                                    .child(
                                        gpui_component::h_flex()
                                            .gap(px(6.))
                                            .items_center()
                                            .text_size(px(11.))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(hsla(p.muted_foreground))
                                            .child(Icon::Quote.render(px(12.), hsla(p.context_chip_citation)))
                                            .child("Cited excerpt"),
                                    )
                                    .child(crate::transcript::ghost_icon_button(
                                        "close-sent-citation",
                                        Icon::X,
                                        |_, _, cx| close_sent_citation_popover(cx),
                                        cx,
                                    )),
                            )
                            .child(
                                div()
                                    .id("sent-citation-quote-body")
                                    .max_h(px(180.))
                                    .overflow_y_scroll()
                                    .pl(px(10.))
                                    .border_l_2()
                                    .border_color(hsla(p.context_chip_citation).opacity(0.65))
                                    .text_size(px(12.))
                                    .line_height(px(18.))
                                    .text_color(hsla(p.foreground).opacity(0.9))
                                    .child(active.citation.text),
                            )
                            .when_some(comment, |this, comment| {
                                this.child(
                                    gpui_component::v_flex()
                                        .gap(px(2.))
                                        .pt(px(6.))
                                        .border_t_1()
                                        .border_color(hsla(p.border).opacity(0.6))
                                        .child(
                                            div()
                                                .text_size(px(11.))
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .text_color(hsla(p.muted_foreground))
                                                .child("Comment"),
                                        )
                                        .child(div().text_size(px(12.)).text_color(hsla(p.foreground)).child(comment)),
                                )
                            })
                            .child(
                                gpui_component::h_flex()
                                    .justify_end()
                                    .gap(px(6.))
                                    .pt(px(2.))
                                    .child(crate::transcript::ghost_text_button(
                                        "copy-sent-citation",
                                        "Copy quote",
                                        Some(Icon::Copy),
                                        move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                                            copy_state.update(cx, |app, cx| {
                                                app.show_toast(Toast::new(ToastKind::Success, "Copied quote"), cx);
                                            });
                                            close_sent_citation_popover(cx);
                                        },
                                        cx,
                                    ))
                                    .child(crate::transcript::ghost_text_button(
                                        "goto-sent-citation",
                                        "Go to source",
                                        Some(Icon::ArrowUpRight),
                                        move |_, _, cx| {
                                            navigate_to_citation_source(go_citation.clone(), &go_state, cx);
                                        },
                                        cx,
                                    )),
                            ),
                    ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    fn render_selection_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
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
        if self.citation_ui.pending.is_none() {
            if self.state.read(cx).citation_source.as_ref().is_some_and(|c| self.thread_id.as_deref() == Some(&c.thread_id)) {
                self.open_citation_source(cx);
            }
            return;
        }
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
                let target_entry_id = citation.message_id.clone();
                self.citation_ui.pending = Some((citation, procedure));
                self.load_earlier_through(target_entry_id, cx);
            }
            HistoryAction::Found => {
                self.citation_ui.pending = None;
                self.citation_ui.highlighted = Some(citation.message_id.clone());
                let matched_excerpt = self.entries_cache.entries().iter().find_map(|entry| match &entry.kind {
                    t3_core::timeline::EntryKind::Message(msg) if msg.id == citation.message_id => {
                        let rendered = rules::markdown_text(&msg.text);
                        Some(rules::find_citation_range(&rendered, &citation).is_some())
                    }
                    _ => None,
                });
                if matched_excerpt == Some(false) {
                    self.state.update(cx, |state, cx| {
                        state.show_toast(
                            Toast::new(ToastKind::Info, "The quoted text has changed")
                                .description("Showing the source response. The saved quote is unchanged."),
                            cx,
                        );
                    });
                }
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
        crate::selectable_text::clear_active_selection(cx);
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
            crate::selectable_text::clear_active_selection(cx);
            cx.stop_propagation();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::markdown::blocks::NestedSelection;
    use gpui::AppContext;
    use gpui_component::text::TextViewState;

    #[gpui::test]
    fn repeated_quotes_disambiguate_using_hint_fraction(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let source = "alpha token beta\n\nmiddle body prose here\n\ngamma token delta";
        let tv = cx.update(|cx| cx.new(|cx| TextViewState::markdown(source, cx)));
        let nested = cx.update(|cx| cx.new(|cx| TextViewState::markdown("token", cx)));
        cx.run_until_parked();
        let nested_views = crate::transcript::markdown::blocks::NestedViews::default();
        nested_views.lock().unwrap().insert("repeat:0".into(), NestedSelection::Markdown(nested.clone()));
        let state = MarkdownState::from(&tv).with_nested_views_for_test(nested_views);
        nested.update(cx, |s, cx| s.select_all(cx));
        cx.update(|cx| {
            let quote = nested.read(cx).selected_text();
            let bottom_hint = SelectionHint {
                byte_range: None,
                source_quote: Some(&quote),
                vertical_fraction: Some(0.9),
                horizontal_fraction: Some(0.4),
            };
            let cited = ThreadView::citation_from_markdown_selection_with_hint(
                &state,
                source,
                &quote,
                ("env".into(), "t1".into(), "m1".into()),
                &bottom_hint,
                cx,
            )
            .expect("hint must disambiguate repeated quote");
            assert!(cited.start > 30);
        });
    }

    #[gpui::test]
    fn active_selectable_text_fallback_reaches_citation_when_text_selection_empty(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let cx = cx.add_empty_window();
        let handle = cx.update(|_, cx| crate::selectable_text::SelectableTextHandle::new_for_test("graph LR\n  A --> B", cx));
        cx.update(|_, cx| handle.select_all(cx));
        let quote = cx.update(current_selection_quote);
        assert_eq!(quote, "graph LR\n  A --> B");
        cx.update(|_, cx| crate::selectable_text::clear_active_selection(cx));
        let cleared = cx.update(current_selection_quote);
        assert!(cleared.is_empty());
    }
}
