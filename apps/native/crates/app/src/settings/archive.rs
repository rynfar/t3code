//! Archive: archived threads by project with Unarchive, and Unarchive or Delete from the row's
//! context menu (`web/src/components/settings/ArchivedThreadsPanel.tsx`). Delete asks first while
//! the delete confirmation setting is on, with the copy `confirmAndDeleteThread` uses.

use gpui::{
    Context, FontWeight, IntoElement, MouseButton, ParentElement, Pixels, Point, PromptLevel, SharedString, Styled, Window,
    div, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::commands::{self, method};
use t3_core::settings::archive::{self as model, ArchiveGroup, GET_ARCHIVED_SHELL_SNAPSHOT};
use serde_json::json;

use super::controls::{Button, ButtonSize, ButtonVariant, Row, icon, r, section};
use super::{Block, SettingsView, store};
use crate::app_state::uuid_v4;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

#[derive(Default)]
enum Status {
    #[default]
    Loading,
    Loaded(Vec<ArchiveGroup>),
    Failed,
}

#[derive(Default)]
pub(crate) struct ArchiveState {
    status: Status,
    menu: Option<(String, Point<Pixels>)>,
    action_error: Option<(&'static str, String)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Unarchive,
    Delete,
}

impl SettingsView {
    pub(super) fn load_archive(&mut self, cx: &mut Context<Self>) {
        self.archive.status = Status::Loading;
        let task = self.state.read(cx).rpc(GET_ARCHIVED_SHELL_SNAPSHOT, json!({}));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.archive.status = match result {
                    Ok(snapshot) => Status::Loaded(model::groups(&snapshot)),
                    Err(_) => Status::Failed,
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn run_archive_action(&mut self, action: Action, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.archive.menu = None;
        self.archive.action_error = None;
        let title = self.archived_title(&thread_id).unwrap_or_else(|| "this thread".to_owned());
        if action == Action::Delete && store::client(cx).confirm_thread_delete() {
            let (message, detail) = model::delete_prompt(&title);
            let answer = window.prompt(PromptLevel::Warning, &message, Some(detail), &["Delete", "Cancel"], cx);
            cx.spawn_in(window, async move |this, cx| {
                if answer.await.ok() == Some(0) {
                    let _ = this.update(cx, |this, cx| this.dispatch_archive_action(action, thread_id, cx));
                }
            })
            .detach();
            return;
        }
        self.dispatch_archive_action(action, thread_id, cx);
    }

    fn dispatch_archive_action(&mut self, action: Action, thread_id: String, cx: &mut Context<Self>) {
        let kind = if action == Action::Unarchive { "thread.unarchive" } else { "thread.delete" };
        let task = self.state.read(cx).rpc(method::DISPATCH_COMMAND, commands::simple_thread_command(kind, &uuid_v4(), &thread_id));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        if let Status::Loaded(groups) = &mut this.archive.status {
                            model::remove_thread(groups, &thread_id);
                        }
                    }
                    Err(error) => {
                        let title = if action == Action::Unarchive { "Failed to unarchive thread" } else { "Failed to delete thread" };
                        this.archive.action_error = Some((title, error.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn archived_title(&self, thread_id: &str) -> Option<String> {
        let Status::Loaded(groups) = &self.archive.status else { return None };
        groups.iter().flat_map(|group| &group.threads).find(|thread| thread.id == thread_id).map(|thread| thread.title.clone())
    }

    pub(super) fn archive_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let p = palette(cx);
        let error = self.archive.action_error.clone().map(|(title, message)| {
            v_flex()
                .px(r(14.))
                .py(r(12.))
                .gap(r(2.))
                .rounded(r(12.))
                .border_1()
                .border_color(hsla(p.error).opacity(0.32))
                .bg(hsla(p.error_surface))
                .text_sm()
                .text_color(hsla(p.error_foreground))
                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().opacity(0.8).child(message))
        });
        let groups = match &self.archive.status {
            Status::Loaded(groups) if !groups.is_empty() => groups.clone(),
            status => {
                let (title, description) = match status {
                    Status::Loading => ("Loading archived threads", "Checking connected environments."),
                    Status::Failed => ("Could not load archived threads", "Failed to load archived threads."),
                    Status::Loaded(_) => ("No archived threads", "Archived threads will appear here."),
                };
                let empty = section("Archived threads", vec![Row::new("archive", title).description(description).into_any_element()], cx);
                return error.map(|error| Block::new(&[], error)).into_iter().chain([Block::new(&["archive"], empty)]).collect();
            }
        };
        let now = chrono::Utc::now().timestamp_millis();
        let menu = self.archive.menu.clone();
        let mut blocks: Vec<Block> = error.map(|error| Block::new(&[], error)).into_iter().collect();
        for (index, group) in groups.into_iter().enumerate() {
            let rows = group
                .threads
                .iter()
                .map(|thread| {
                    let id = thread.id.clone();
                    let unarchive_id = id.clone();
                    let menu_id = id.clone();
                    div()
                        .id(SharedString::from(format!("archived-{id}")))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                                this.archive.menu = Some((menu_id.clone(), event.position));
                                cx.notify();
                            }),
                        )
                        .child(
                            Row::new(SharedString::from(format!("archived-row-{id}")), thread.title.clone())
                                .description(model::description(thread, now))
                                .control(
                                    Button::new(SharedString::from(format!("unarchive-{id}")), ButtonVariant::Outline, ButtonSize::Sm)
                                        .icon(Icon::ArchiveX)
                                        .label("Unarchive")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.run_archive_action(Action::Unarchive, unarchive_id.clone(), window, cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                })
                .collect();
            let title = h_flex().gap(r(8.)).child(icon(Icon::Folder, 16., hsla(p.muted_foreground))).child(group.title.clone());
            let anchors: &[&'static str] = if index == 0 { &["archive"] } else { &[] };
            blocks.push(Block::new(
                anchors,
                v_flex()
                    .gap(r(10.))
                    .child(h_flex().min_h(r(28.)).px(r(16.)).text_base().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(super::controls::group(rows, cx)),
            ));
        }
        if let Some((thread_id, position)) = menu {
            let (unarchive_id, delete_id) = (thread_id.clone(), thread_id);
            let item = |label: &'static str, destructive: bool| {
                h_flex()
                    .id(label)
                    .min_h(r(28.))
                    .px(r(8.))
                    .py(r(4.))
                    .rounded(r(4.))
                    .text_sm()
                    .cursor_pointer()
                    .text_color(if destructive { hsla(p.destructive_foreground) } else { hsla(p.foreground) })
                    .hover(|this| this.bg(hsla(p.accent)))
                    .child(label)
            };
            let popup = gpui::deferred(
                gpui::anchored().position(position).snap_to_window_with_margin(px(8.)).child(
                    v_flex()
                        .occlude()
                        .min_w(r(160.))
                        .p(r(4.))
                        .rounded(r(8.))
                        .border_1()
                        .border_color(hsla(p.dropdown_border))
                        .bg(hsla(p.popover))
                        .shadow(crate::composer::controls::floating_shadow(cx.theme().is_dark()))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.archive.menu = None;
                            cx.notify();
                        }))
                        .child(item("Unarchive", false).on_click(cx.listener(move |this, _, window, cx| {
                            this.run_archive_action(Action::Unarchive, unarchive_id.clone(), window, cx)
                        })))
                        .child(item("Delete", true).on_click(cx.listener(move |this, _, window, cx| {
                            this.run_archive_action(Action::Delete, delete_id.clone(), window, cx)
                        }))),
                ),
            )
            .with_priority(2);
            blocks.push(Block::new(&[], div().size_0().child(popup)));
        }
        blocks
    }
}
