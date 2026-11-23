//! The unsent draft cards and discard Undo (`SidebarDraftBlock.tsx`, `discardComposerDraft.ts`).

use super::SidebarPane;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use gpui::{AnyElement, Context, ElementId, FontWeight, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use t3_core::composer::drafts::DraftRef;
use t3_core::thread_actions::{UndoAction, UndoStep};

impl SidebarPane {
    fn open_draft_menu(&mut self, draft: &DraftRef, position: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.draft_menu_target = Some(draft.clone());
        self.menu.update(cx, |menu, cx| menu.open_for_draft(&draft.project_id, position, window, cx));
    }

    pub(super) fn discard_draft(&mut self, target: &DraftRef, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.update(cx, |composer, cx| composer.discard_sidebar_draft(target, window, cx));
        if let Some(draft) = draft {
            let key = target.key();
            let claim = self.state.update(cx, |state, cx| {
                let claim = state.sidebar.undo.begin_draft_session(&key);
                state.sidebar.undo.show(UndoAction::Discarded, claim.clone(), vec![UndoStep::RestoreDraft], super::controller::now_ms());
                state.sidebar.history.forget_draft(target);
                if state.is_active_draft(target) {
                    state.draft_project = None;
                    state.clear_selection(cx);
                }
                state.after_undo_change(cx);
                claim
            });
            if let Some((_, previous)) = self.discarded.insert(key, (claim, draft)) {
                self.composer.update(cx, |composer, cx| composer.commit_sidebar_discard(previous, cx));
            }
        }
        self.sync_rows(cx);
        cx.notify();
    }

    pub(super) fn discard_thread_draft(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.update(cx, |composer, cx| composer.discard_thread_draft(thread_id, window, cx));
        if let Some(draft) = draft {
            let claim = self.state.update(cx, |state, cx| {
                let claim = state.sidebar.undo.begin_thread_draft(thread_id);
                state.sidebar.undo.show(UndoAction::Discarded, claim.clone(), vec![UndoStep::RestoreDraft], super::controller::now_ms());
                state.after_undo_change(cx);
                claim
            });
            if let Some((_, previous)) = self.discarded_threads.insert(thread_id.into(), (claim, draft)) {
                self.composer.update(cx, |composer, cx| composer.commit_sidebar_discard(previous, cx));
            }
        }
        cx.notify();
    }

    pub(super) fn render_draft_row(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.drafts.rows[index];
        let p = palette(cx);
        let state = self.state.read(cx);
        let active = state.is_active_draft(&row.draft);
        let project_id = &row.draft.project_id;
        let project = row.draft.environment_id.as_ref()
            .and_then(|env| state.sidebar.fleet.project_names.get(&t3_core::sidebar::grouping::ProjectRef {
                environment_id: env.clone(),
                project_id: project_id.clone(),
            }))
            .or_else(|| state.sidebar.model.project_names.get(project_id))
            .cloned();
        let open = row.draft.clone();
        let discard = open.clone();
        let menu = open.clone();
        let keyboard_menu = open.clone();
        let id = ElementId::Name(SharedString::from(format!("draft-row-{}", row.key)));
        let discard_id = SharedString::from(format!("discard-draft-{}", row.key));
        let focus = self.tree.focus(&super::tree::TreeRow::Draft(row.key.clone()))
            .unwrap_or_else(|| crate::settings::controls::focus_handle(&id, window, cx));
        let keyboard_focus = focus.clone();
        let (bounds, measure) = super::measure_trigger();
        div()
            .px(px(8.))
            .py(px(2.))
            .child(
                div()
                    .id(id)
                    .debug_selector(|| format!("draft-row-{}", row.key))
                    .relative()
                    .track_focus(&focus)
                    .role(gpui::Role::Button)
                    .focus_visible(|this| this.bg(hsla(p.sidebar_row_hover)))
                    .child(measure)
                    .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                        if keyboard_focus.is_focused(window) && super::tree::context_menu_key(&event.keystroke) {
                            window.prevent_default();
                            cx.stop_propagation();
                            if !event.is_held && let Some(bounds) = bounds.get() {
                                this.open_draft_menu(&keyboard_menu, bounds.bottom_left(), window, cx);
                            }
                        }
                    }))
                    .group("draft-row")
                    .w_full()
                    .h(px(78.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .bg(if active { hsla(p.sidebar_row_active) } else { hsla(p.warning).opacity(0.04) })
                    .when(!active, |this| this.hover(|this| this.bg(hsla(p.warning).opacity(0.08))))
                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(crate::i18n::t(cx, "Unsent draft")).build(window, cx))
                    .child(
                        div()
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(Icon::SquarePen.render(px(12.), hsla(p.warning_foreground)))
                            .child(self.favicon(project_id, false, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.sidebar_secondary_label))
                                    .children(project),
                            )
                            .child(if self.jump_hints_visible() {
                                gpui::div()
                                    .h(px(20.))
                                    .min_w(px(20.))
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .children((index < 9).then(|| super::row::render_jump_badge(index, cx)).flatten())
                                    .into_any_element()
                            } else {
                                div()
                                    .id(discard_id)
                                    .size(px(20.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.))
                                    .invisible()
                                    .group_hover("draft-row", |this| this.visible())
                                    .text_color(hsla(p.muted_foreground))
                                    .hover(|this| this.text_color(hsla(p.foreground)))
                                    .child(Icon::X.sized(px(12.)))
                                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(crate::i18n::t(cx, "Discard draft")).build(window, cx))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.discard_draft(&discard, window, cx);
                                    }))
                                    .into_any_element()
                            }),
                    )
                    .child(
                        div()
                            .mt(px(2.))
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground).opacity(0.9))
                            .child(row.preview.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.composer.read(cx).sidebar_drafts().get(&open.key()).is_some()
                            || this.drafts.rows.iter().any(|candidate| candidate.key == open.key())
                        {
                            this.state.update(cx, |state, cx| {
                                state.clear_thread_selection(cx);
                                state.open_saved_draft(open.clone(), cx);
                            });
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            window.focus(&focus, cx);
                            this.open_draft_menu(&menu, event.position, window, cx);
                        }),
                    ),
            )
            .into_any_element()
    }
}
