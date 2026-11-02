//! Archive: archived threads by project with Unarchive, and Unarchive or Delete from the row's
//! context menu (`web/src/components/settings/ArchivedThreadsPanel.tsx`). Delete asks first while
//! the delete confirmation setting is on, with the copy `confirmAndDeleteThread` uses. The rows
//! are a virtualized list over entries computed when the archive loads or a row leaves, so a
//! large archive costs only its visible rows on each render (a settings search keystroke).

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ListAlignment, ListOffset, ListState, MouseButton, ParentElement, Pixels, Point,
    PromptLevel, SharedString, Styled, Window, div, list, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::commands::{self, method};
use t3_core::settings::archive::{self as model, ArchiveEntry, ArchiveGroup, GET_ARCHIVED_SHELL_SNAPSHOT};
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

pub(crate) struct ArchiveState {
    status: Status,
    entries: Vec<ArchiveEntry>,
    list: ListState,
    menu: Option<(String, Point<Pixels>)>,
    action_error: Option<(&'static str, String)>,
}

impl Default for ArchiveState {
    fn default() -> Self {
        ArchiveState {
            status: Status::default(),
            entries: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            menu: None,
            action_error: None,
        }
    }
}

impl ArchiveState {
    /// Re-derives the rows after the groups change, re-measuring only from the first that moved.
    fn sync_entries(&mut self) {
        let next = match &self.status {
            Status::Loaded(groups) => model::entries(groups),
            _ => Vec::new(),
        };
        let first = self.entries.iter().zip(&next).take_while(|(old, new)| old == new).count();
        self.list.splice(first..self.entries.len(), next.len() - first);
        self.entries = next;
    }

    /// Whether the archive shows as the virtualized list rather than a placeholder row.
    fn has_rows(&self) -> bool {
        !self.entries.is_empty()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Unarchive,
    Delete,
}

impl SettingsView {
    pub(super) fn load_archive(&mut self, cx: &mut Context<Self>) {
        self.archive.status = Status::Loading;
        self.archive.sync_entries();
        let task = self.state.read(cx).rpc(GET_ARCHIVED_SHELL_SNAPSHOT, json!({}));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.archive.status = match result {
                    Ok(snapshot) => Status::Loaded(model::groups(&snapshot)),
                    Err(_) => Status::Failed,
                };
                this.archive.entries.clear();
                this.archive.list.reset(0);
                this.archive.sync_entries();
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
                        this.archive.sync_entries();
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

    fn archive_error(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let p = palette(cx);
        self.archive.action_error.clone().map(|(title, message)| {
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
        })
    }

    /// The loading, failed and empty states; loaded rows render through `archive_list`.
    pub(super) fn archive_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        if self.archive.has_rows() {
            return Vec::new();
        }
        let (title, description) = match &self.archive.status {
            Status::Loading => ("Loading archived threads", "Checking connected environments."),
            Status::Failed => ("Could not load archived threads", "Failed to load archived threads."),
            Status::Loaded(_) => ("No archived threads", "Archived threads will appear here."),
        };
        let empty = section("Archived threads", vec![Row::new("archive", title).description(description).into_any_element()], cx);
        self.archive_error(cx).map(|error| Block::new(&[], error)).into_iter().chain([Block::new(&["archive"], empty)]).collect()
    }

    /// The archive's rows as a virtualized list filling the page, with the row menu over it.
    pub(super) fn archive_list(&mut self, scroll_to_top: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.archive.has_rows() {
            return None;
        }
        if scroll_to_top {
            self.archive.list.scroll_to(ListOffset::default());
        }
        let p = palette(cx);
        let error = self.archive_error(cx).map(|error| div().w_full().max_w(r(896.)).px(r(24.)).pt(r(24.)).child(error));
        let popup = self.archive.menu.clone().map(|(thread_id, position)| {
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
            gpui::deferred(
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
            .with_priority(2)
        });
        Some(
            v_flex()
                .w_full()
                .flex_1()
                .min_h_0()
                .items_center()
                .children(error)
                .child(list(self.archive.list.clone(), cx.processor(|this, index: usize, _, cx| this.render_archive_entry(index, cx))).w_full().flex_1())
                .children(popup)
                .into_any_element(),
        )
    }

    fn render_archive_entry(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let (Some(entry), Status::Loaded(groups)) = (self.archive.entries.get(index).copied(), &self.archive.status) else {
            return div().into_any_element();
        };
        let p = palette(cx);
        let last = index + 1 == self.archive.entries.len();
        let content = match entry {
            ArchiveEntry::Project(group) => {
                let title = h_flex().gap(r(8.)).child(icon(Icon::Folder, 16., hsla(p.muted_foreground))).child(groups[group].title.clone());
                div()
                    .id(SharedString::from(format!("archive-project-{}", groups[group].project_id)))
                    .pt(r(if group == 0 { 0. } else { 24. }))
                    .pb(r(10.))
                    .child(h_flex().min_h(r(28.)).px(r(16.)).text_base().font_weight(FontWeight::SEMIBOLD).child(title))
                    .into_any_element()
            }
            ArchiveEntry::Thread(group, position) => {
                let count = groups[group].threads.len();
                let thread = &groups[group].threads[position];
                let now = chrono::Utc::now().timestamp_millis();
                let id = thread.id.clone();
                let (unarchive_id, menu_id) = (id.clone(), id.clone());
                // One segment of `SettingsGroup`: the card's sides on every row, its ends on the first and last.
                div()
                    .id(SharedString::from(format!("archived-{id}")))
                    .border_l_1()
                    .border_r_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .bg(hsla(p.card).opacity(0.4))
                    .when(position == 0, |this| this.border_t_1().rounded_t(r(12.)))
                    .when(position + 1 == count, |this| this.border_b_1().rounded_b(r(12.)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            this.archive.menu = Some((menu_id.clone(), event.position));
                            cx.notify();
                        }),
                    )
                    .child(
                        div().when(position > 0, |this| this.border_t_1().border_color(hsla(p.border).opacity(0.5))).child(
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
                        ),
                    )
                    .into_any_element()
            }
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .when(index == 0, |this| this.pt(r(24.)))
            .when(last, |this| this.pb(r(48.)))
            .child(div().w_full().max_w(r(896.)).px(r(24.)).child(content))
            .into_any_element()
    }
}
