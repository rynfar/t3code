//! Sidebar: fixed-height rows in a `uniform_list`, so thousands of threads cost one viewport.
//! The pane is rendered `.cached()` by the workspace and only notifies on shell or selection
//! changes, so transcript streaming never repaints it. Rows use the slim 36px layout from
//! design-system §7.2; card rows come with the full section logic (ledger SB-ROW).

use std::sync::Arc;

use gpui::{Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, prelude::*, px, uniform_list};
use gpui_component::{Icon, IconName, Sizable, h_flex, v_flex};
use t3_core::shell::ThreadShell;
use t3_core::sidebar::SidebarRow;

use crate::app_state::{AppEvent, AppState};
use crate::theme::{Palette, hsla, palette};
use crate::workspace::TITLEBAR_HEIGHT;

const ROW_HEIGHT: f32 = 36.;

pub struct SidebarPane {
    state: Entity<AppState>,
    _subscription: Subscription,
}

/// `resolveSidebarThreadStatus` subset: the label and its token (design-system §7.3).
fn status(thread: &ThreadShell, p: &Palette) -> Option<(&'static str, u32)> {
    if let Some(request) = &thread.pending_runtime_request {
        let input = request.kind.contains("input") || request.kind.contains("question");
        return Some(if input { ("Input", p.status_input_icon) } else { ("Approval", p.warning_foreground) });
    }
    if thread.status == "failed" {
        return Some(("Failed", p.error_foreground));
    }
    if thread.activity_run_status.as_deref() == Some("waiting") || thread.status == "waiting" {
        return Some(("Waiting", p.muted_foreground));
    }
    if thread.is_live() {
        return Some(("Working", p.info_foreground));
    }
    None
}

impl SidebarPane {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Shell | AppEvent::Selection) {
                cx.notify();
            }
        });
        SidebarPane { state, _subscription: subscription }
    }

    fn render_row(&self, row: &SidebarRow, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = self.state.read(cx);
        let p = palette(cx);
        match row {
            SidebarRow::Section { title } => h_flex()
                .h(px(ROW_HEIGHT))
                .px(px(18.))
                .pt_2()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.sidebar_muted_foreground))
                .child(*title)
                .into_any_element(),
            SidebarRow::Project { project_id } => {
                let title: SharedString = state.shell.project(project_id).map(|p| p.title.clone()).unwrap_or_default().into();
                h_flex()
                    .h(px(ROW_HEIGHT))
                    .px(px(18.))
                    .gap(px(10.))
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.sidebar_foreground))
                    .child(Icon::new(IconName::Folder).small().text_color(hsla(p.sidebar_icon)))
                    .child(div().truncate().child(title))
                    .into_any_element()
            }
            SidebarRow::Thread { thread_id, depth } => {
                let Some(thread) = state.shell.thread(thread_id) else {
                    return div().h(px(ROW_HEIGHT)).into_any_element();
                };
                let selected = state.selected_thread.as_deref() == Some(thread_id.as_str());
                let status = status(thread, p);
                let title_color = if selected || status.is_some() { p.foreground } else { p.sidebar_secondary_label };
                let id = thread_id.clone();
                let title: SharedString = thread.title.clone().into();
                h_flex()
                    .id(SharedString::from(format!("thread-{thread_id}")))
                    .h(px(ROW_HEIGHT - 2.))
                    .my(px(1.))
                    .mx_2()
                    .pl(px(10. + f32::from(*depth) * 16.))
                    .pr(px(10.))
                    .gap(px(10.))
                    .rounded(px(6.))
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .text_color(hsla(title_color))
                    .when(selected, |this| this.bg(hsla(p.sidebar_row_active)))
                    .when(!selected, |this| this.hover(|this| this.bg(hsla(p.sidebar_row_hover))))
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .children(status.map(|(label, color)| {
                        div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(color)).child(label)
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.update(cx, |state, cx| state.select_thread(&id, cx));
                    }))
                    .into_any_element()
            }
        }
    }
}

impl Render for SidebarPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Arc<Vec<SidebarRow>> = self.state.read(cx).sidebar_rows.clone();
        let synchronized = self.state.read(cx).shell.synchronized;
        let p = palette(cx);
        v_flex()
            .size_full()
            .child(div().h(px(TITLEBAR_HEIGHT)).flex_none())
            .when(rows.is_empty(), |this| {
                this.child(
                    div()
                        .px(px(18.))
                        .text_sm()
                        .text_color(hsla(p.sidebar_muted_foreground))
                        .child(if synchronized { "No threads yet" } else { "Loading…" }),
                )
            })
            .child(
                uniform_list(
                    "sidebar-rows",
                    rows.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                        range.map(|ix| this.render_row(&rows[ix], cx)).collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .pb_2(),
            )
    }
}
