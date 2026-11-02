//! Sidebar: fixed-height rows in a `uniform_list`, so thousands of threads cost one viewport.
//! The pane is rendered `.cached()` by the workspace and only notifies on shell or selection
//! changes, so transcript streaming never repaints it.

use std::sync::Arc;

use gpui::{Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, prelude::*, px, uniform_list};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};
use t3_core::sidebar::SidebarRow;

use crate::app_state::{AppEvent, AppState};
use crate::workspace::TITLEBAR_HEIGHT;

const ROW_HEIGHT: f32 = 30.;

pub struct SidebarPane {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl SidebarPane {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::ShellChanged | AppEvent::SelectionChanged) {
                cx.notify();
            }
        });
        SidebarPane { state, _subscription: subscription }
    }

    fn render_row(&self, row: &SidebarRow, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        match row {
            SidebarRow::Section { title } => div()
                .h(px(ROW_HEIGHT))
                .px_3()
                .pt_3()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(*title)
                .into_any_element(),
            SidebarRow::Project { project_id } => {
                let title: SharedString = state.shell.project(project_id).map(|p| p.title.clone()).unwrap_or_default().into();
                h_flex()
                    .h(px(ROW_HEIGHT))
                    .px_3()
                    .gap_2()
                    .text_sm()
                    .text_color(theme.sidebar_foreground)
                    .child(Icon::new(IconName::Folder).small().text_color(theme.muted_foreground))
                    .child(div().truncate().child(title))
                    .into_any_element()
            }
            SidebarRow::Thread { thread_id, depth } => {
                let Some(thread) = state.shell.thread(thread_id) else { return div().h(px(ROW_HEIGHT)).into_any_element() };
                let selected = state.selected_thread.as_deref() == Some(thread_id.as_str());
                let live = thread.is_live();
                let attention = thread.needs_attention();
                let failed = thread.status == "failed";
                let id = thread_id.clone();
                let title: SharedString = thread.title.clone().into();
                h_flex()
                    .id(SharedString::from(format!("thread-{thread_id}")))
                    .h(px(ROW_HEIGHT))
                    .mx_1()
                    .pl(px(12. + f32::from(*depth) * 14.))
                    .pr_2()
                    .gap_2()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .text_color(if selected { theme.sidebar_accent_foreground } else { theme.sidebar_foreground })
                    .when(selected, |this| this.bg(theme.sidebar_accent))
                    .hover(|this| this.bg(theme.sidebar_accent))
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .when(attention, |this| {
                        this.child(div().text_xs().text_color(theme.warning).child("Approval"))
                    })
                    .when(failed && !attention, |this| this.child(div().text_xs().text_color(theme.danger).child("Failed")))
                    .when(live && !attention, |this| {
                        this.child(Icon::new(IconName::LoaderCircle).xsmall().text_color(theme.muted_foreground))
                    })
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
        let theme = cx.theme();
        v_flex()
            .size_full()
            .child(div().h(px(TITLEBAR_HEIGHT)).flex_none())
            .when(rows.is_empty(), |this| {
                this.child(
                    div()
                        .px_3()
                        .text_sm()
                        .text_color(theme.muted_foreground)
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
