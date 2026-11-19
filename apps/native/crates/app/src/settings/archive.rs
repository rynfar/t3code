//! Archive: archived threads by project with inline Unarchive and Delete actions, and Unarchive
//! or Delete from the row's context menu (`web/src/components/settings/ArchivedThreadsPanel.tsx`).
//! Delete asks first while the delete confirmation setting is on, with the copy
//! `confirmAndDeleteThread` uses. The rows are a virtualized list over entries computed when the
//! archive loads or a row leaves, so a large archive costs only its visible rows on each render.

use std::collections::HashSet;

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ListAlignment, ListOffset, ListState, MouseButton, ParentElement, Pixels, Point,
    SharedString, Styled, div, list, prelude::*, px,
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
    pending: HashSet<String>,
    connection: Option<u64>,
    load_id: u64,
    action_error: Option<(&'static str, String)>,
}

impl Default for ArchiveState {
    fn default() -> Self {
        ArchiveState {
            status: Status::default(),
            entries: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            menu: None,
            pending: HashSet::new(),
            connection: None,
            load_id: 0,
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

    pub(super) fn dismiss(&mut self) -> bool {
        self.menu.take().is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Unarchive,
    Delete,
}

impl SettingsView {
    pub(super) fn load_archive(&mut self, cx: &mut Context<Self>) {
        let connection = self.state.read(cx).connection_id();
        self.archive.connection = connection;
        self.archive.load_id += 1;
        let load_id = self.archive.load_id;
        self.archive.menu = None;
        self.archive.pending.clear();
        self.archive.action_error = None;
        self.archive.status = Status::Loading;
        self.archive.sync_entries();
        let hidden_tags = self
            .state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .and_then(|config| config.settings.hidden_tags());
        let task = self.state.read(cx).rpc(GET_ARCHIVED_SHELL_SNAPSHOT, json!({}));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.archive.load_id != load_id || this.archive.connection != connection {
                    return;
                }
                this.archive.status = match result {
                    Ok(snapshot) => Status::Loaded(model::groups_filtered(&snapshot, hidden_tags.as_deref())),
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

    fn run_archive_action(&mut self, action: Action, thread_id: String, cx: &mut Context<Self>) {
        self.archive.menu = None;
        if self.archive.pending.contains(&thread_id) {
            return;
        }
        self.archive.action_error = None;
        let connection = self.archive.connection;
        let title = self.archived_title(&thread_id).unwrap_or_else(|| "this thread".to_owned());
        self.archive.pending.insert(thread_id.clone());
        cx.notify();
        if action == Action::Delete && store::client(cx).confirm_thread_delete() {
            let (message, detail) = model::delete_prompt(&title);
            let confirm_view = cx.entity().downgrade();
            let cancel_view = cx.entity().downgrade();
            let confirm_id = thread_id.clone();
            let cancel_id = thread_id.clone();
            let opened = crate::dialog::ask_available_cancellable(
                message,
                detail,
                "Delete",
                move |_, cx| {
                    // Defer so `AppState` is no longer borrowed by `ConfirmDialog::resolve`.
                    cx.defer(move |cx| {
                        let _ = confirm_view.update(cx, |this, cx| {
                            if this.archive.connection != connection {
                                this.archive.pending.remove(&confirm_id);
                                cx.notify();
                                return;
                            }
                            this.dispatch_archive_action(action, confirm_id, connection, cx);
                        });
                    });
                },
                move |_, cx| {
                    cx.defer(move |cx| {
                        let _ = cancel_view.update(cx, |this, cx| {
                            this.archive.pending.remove(&cancel_id);
                            cx.notify();
                        });
                    });
                },
                cx,
            );
            if !opened {
                self.archive.pending.remove(&thread_id);
                cx.notify();
            }
            return;
        }
        self.dispatch_archive_action(action, thread_id, connection, cx);
    }

    fn dispatch_archive_action(&mut self, action: Action, thread_id: String, connection: Option<u64>, cx: &mut Context<Self>) {
        if self.archive.connection != connection {
            self.archive.pending.remove(&thread_id);
            cx.notify();
            return;
        }
        let kind = if action == Action::Unarchive { "thread.unarchive" } else { "thread.delete" };
        let task = self.state.read(cx).rpc(method::DISPATCH_COMMAND, commands::simple_thread_command(kind, &uuid_v4(), &thread_id));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.archive.pending.remove(&thread_id);
                if this.archive.connection != connection {
                    return;
                }
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
                        .child(item("Unarchive", false).on_click(cx.listener(move |this, _, _, cx| {
                            this.run_archive_action(Action::Unarchive, unarchive_id.clone(), cx)
                        })))
                        .child(item("Delete", true).on_click(cx.listener(move |this, _, _, cx| {
                            this.run_archive_action(Action::Delete, delete_id.clone(), cx)
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
                let pending = self.archive.pending.contains(&id);
                let (unarchive_id, delete_id, menu_id) = (id.clone(), id.clone(), id.clone());
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
                            if this.archive.pending.contains(&menu_id) {
                                return;
                            }
                            this.archive.menu = Some((menu_id.clone(), event.position));
                            cx.notify();
                        }),
                    )
                    .child(
                        div().when(position > 0, |this| this.border_t_1().border_color(hsla(p.border).opacity(0.5))).child(
                            Row::new(SharedString::from(format!("archived-row-{id}")), thread.title.clone())
                                .description(model::description(thread, now))
                                .control(
                                    h_flex()
                                        .gap(r(8.))
                                        .child(
                                            Button::new(SharedString::from(format!("unarchive-{id}")), ButtonVariant::Outline, ButtonSize::IconSm)
                                                .icon(Icon::ArchiveX)
                                                .tooltip("Unarchive")
                                                .disabled(pending)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.run_archive_action(Action::Unarchive, unarchive_id.clone(), cx)
                                                })),
                                        )
                                        .child(
                                            Button::new(SharedString::from(format!("delete-archived-{id}")), ButtonVariant::DestructiveOutline, ButtonSize::IconSm)
                                                .icon(Icon::Trash2)
                                                .tooltip("Delete")
                                                .disabled(pending)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.run_archive_action(Action::Delete, delete_id.clone(), cx)
                                                })),
                                        ),
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

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use gpui::{Focusable, Render, Window, div, prelude::*};
    use serde_json::json;
    use t3_core::settings::archive::{ArchiveGroup, ArchivedThread};
    use t3_core::settings::search::Section;

    use super::*;

    struct TestFrame {
        dialog: gpui::Entity<crate::dialog::ConfirmDialog>,
    }
    impl Render for TestFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .relative()
                .size_full()
                .children(crate::settings::overlay(window, cx))
                .child(self.dialog.clone())
        }
    }

    #[gpui::test]
    fn archive_unarchive_and_confirmed_delete_dispatch_and_guard_pending(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let drive = |vcx: &mut gpui::VisualTestContext| {
            vcx.run_until_parked();
            runtime.block_on(async {
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            });
            vcx.run_until_parked();
        };
        cx.executor().allow_parking();
        let (state, mut fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "orchestration:operate"]
                }))
                .unwrap(),
            ))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
        });
        let dialog = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
        let frame_dialog = dialog.clone();
        let window = cx.add_window(|_, _| TestFrame { dialog: frame_dialog });
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|_, cx| crate::settings::open(Section::Archived, cx));
        drive(&mut vcx);
        let _ = fixture.take_requests();

        let view = vcx.read(|cx| cx.global::<crate::settings::Host>().view.clone().expect("settings view open"));
        vcx.update(|_, cx| {
            view.update(cx, |view, _| {
                view.archive.status = Status::Loaded(vec![ArchiveGroup {
                    project_id: "proj-1".into(),
                    title: "App".into(),
                    threads: vec![ArchivedThread {
                        id: "thread-1".into(),
                        project_id: "proj-1".into(),
                        title: "Old migration".into(),
                        created_at: "2026-11-18T00:00:00Z".into(),
                        archived_at: Some("2026-11-18T00:00:00Z".into()),
                        tags: vec![],
                        parent_thread_id: None,
                    }],
                }]);
                view.archive.sync_entries();
            });
        });

        // Unarchive dispatches immediately and blocks duplicate actions while pending.
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.run_archive_action(Action::Unarchive, "thread-1".into(), cx);
                assert!(view.archive.pending.contains("thread-1"));
                view.run_archive_action(Action::Unarchive, "thread-1".into(), cx);
            });
        });
        drive(&mut vcx);
        let (requests, _) = fixture.take_requests();
        let unarchives: Vec<_> = requests.into_iter().filter(|(m, _, _)| *m == "orchestration.dispatchCommand").collect();
        assert_eq!(unarchives.len(), 1);
        assert_eq!(unarchives[0].1["type"], json!("thread.unarchive"));
        assert_eq!(unarchives[0].1["threadId"], json!("thread-1"));
        let _ = unarchives.into_iter().next().unwrap().2.send(Ok(json!({})));
        drive(&mut vcx);
        view.read_with(&vcx, |view, _| assert!(!view.archive.pending.contains("thread-1")));

        // Delete opens confirmation first; cancelling sends nothing and releases pending.
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.run_archive_action(Action::Delete, "thread-1".into(), cx);
            });
        });
        drive(&mut vcx);
        assert!(vcx.read(|cx| crate::dialog::is_open(cx)));
        assert!(fixture.take_requests().0.into_iter().all(|(m, _, _)| m != "orchestration.dispatchCommand"));
        vcx.update(|window, cx| {
            let focus = dialog.focus_handle(cx);
            window.focus(&focus, cx);
            window.refresh();
            window.draw(cx).clear();
        });
        vcx.simulate_keystrokes("escape");
        drive(&mut vcx);
        assert!(!vcx.read(|cx| crate::dialog::is_open(cx)));
        assert!(fixture.take_requests().0.into_iter().all(|(m, _, _)| m != "orchestration.dispatchCommand"));
        view.read_with(&vcx, |view, _| assert!(!view.archive.pending.contains("thread-1")));

        // Confirming Delete dispatches thread.delete.
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.run_archive_action(Action::Delete, "thread-1".into(), cx);
            });
        });
        drive(&mut vcx);
        vcx.update(|window, cx| {
            let focus = dialog.focus_handle(cx);
            window.focus(&focus, cx);
            window.refresh();
            window.draw(cx).clear();
        });
        vcx.simulate_keystrokes("enter");
        drive(&mut vcx);
        let (requests, _) = fixture.take_requests();
        let (_, payload, reply) = requests.into_iter().find(|(m, _, _)| *m == "orchestration.dispatchCommand").expect("thread.delete sent");
        assert_eq!(payload["type"], json!("thread.delete"));
        assert_eq!(payload["threadId"], json!("thread-1"));
        let _ = reply.send(Ok(json!({})));
        drive(&mut vcx);
        view.read_with(&vcx, |view, _| assert!(!view.archive.pending.contains("thread-1")));

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(vcx);
        drop(fixture);
        drop(runtime);
    }
}
