//! The unsent draft cards and discard Undo (`SidebarDraftBlock.tsx`, `discardComposerDraft.ts`).

use super::SidebarPane;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use gpui::{AnyElement, Context, FontWeight, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use t3_core::thread_actions::{UndoAction, UndoKind, UndoStep};

impl SidebarPane {
    pub(super) fn discard_draft(&mut self, project: &str, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.update(cx, |composer, cx| composer.discard_sidebar_draft(project, window, cx));
        if let Some(draft) = draft {
            let claim = self.state.update(cx, |state, cx| {
                let claim = state.sidebar.undo.begin(UndoKind::Discard, project);
                state.sidebar.undo.show(UndoAction::Discarded, claim.clone(), vec![UndoStep::RestoreDraft], super::controller::now_ms());
                state.after_undo_change(cx);
                claim
            });
            if let Some((_, previous)) = self.discarded.insert(project.into(), (claim, draft)) {
                self.composer.update(cx, |composer, cx| composer.commit_sidebar_discard(previous, cx));
            }
        }
        self.sync_rows(cx);
        cx.notify();
    }

    pub(super) fn render_draft_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.drafts.rows[index];
        let p = palette(cx);
        let state = self.state.read(cx);
        let active = state.draft_project.as_deref() == Some(&row.project_id);
        let project = state.sidebar.model.project_names.get(&row.project_id).cloned();
        let open = row.project_id.clone();
        let discard = open.clone();
        let menu = open.clone();
        div()
            .px(px(8.))
            .py(px(2.))
            .child(
                div()
                    .id(SharedString::from(format!("draft-row-{open}")))
                    .group("draft-row")
                    .w_full()
                    .h(px(78.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .bg(if active { hsla(p.sidebar_row_active) } else { hsla(p.warning).opacity(0.04) })
                    .when(!active, |this| this.hover(|this| this.bg(hsla(p.warning).opacity(0.08))))
                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Unsent draft").build(window, cx))
                    .child(
                        div()
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(Icon::SquarePen.render(px(12.), hsla(p.warning_foreground)))
                            .child(self.favicon(&row.project_id, false, cx))
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
                            .child(
                                div()
                                    .id(SharedString::from(format!("discard-draft-{discard}")))
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
                                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Discard draft").build(window, cx))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.discard_draft(&discard, window, cx);
                                    })),
                            ),
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
                    .on_click(cx.listener(move |this, _, _, cx| this.state.update(cx, |state, cx| state.start_draft(&open, cx))))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.menu.update(cx, |menu_view, cx| menu_view.open_for_draft(&menu, event.position, window, cx));
                        }),
                    ),
            )
            .into_any_element()
    }
}
