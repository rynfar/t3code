//! Archive: archived threads by project with inline Unarchive and Delete actions, and Unarchive
//! or Delete from the row's context menu (`web/src/components/settings/ArchivedThreadsPanel.tsx`).
//! Delete asks first while the delete confirmation setting is on, with the copy
//! `confirmAndDeleteThread` uses. The rows are a virtualized list over entries computed when the
//! archive loads or a row leaves, so a large archive costs only its visible rows on each render.

use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, App, Context, FontWeight, IntoElement, ListAlignment, ListOffset, ListState, MouseButton, ParentElement, Pixels, Point,
    SharedString, Styled, div, list, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::commands::{self, method};
use t3_core::auth::{ORCHESTRATION_OPERATE_SCOPE, ORCHESTRATION_READ_SCOPE};
use t3_core::sidebar::grouping::{EnvironmentOwner, ProjectRef};
use t3_core::settings::archive::{self as model, ArchiveEntry, ArchiveGroup, ArchiveVisibility, GET_ARCHIVED_SHELL_SNAPSHOT};
use t3_core::sidebar::tag_filter::ThreadKey;
use serde_json::json;

use super::controls::{Button, ButtonSize, ButtonVariant, Row, r, section};
use super::{Block, SettingsView, store};
use crate::app_state::{AppState, uuid_v4};
use crate::i18n::t;
use crate::project_icon::{self, ProjectVisual};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

#[derive(Default)]
enum Status {
    #[default]
    Loading,
    Loaded(Vec<ArchiveGroup>),
    Failed,
}

#[derive(Clone, PartialEq, Eq)]
struct ArchiveScope {
    owners: Vec<EnvironmentOwner>,
    projects: Option<HashSet<ProjectRef>>,
    active_connection: Option<u64>,
    active_fallback: bool,
}

#[derive(Clone)]
struct ArchiveOrigin {
    owner: EnvironmentOwner,
    thread_id: String,
    label: String,
}

pub(crate) struct ArchiveState {
    status: Status,
    entries: Vec<ArchiveEntry>,
    list: ListState,
    menu: Option<(String, Point<Pixels>)>,
    pending: HashSet<String>,
    scope: Option<ArchiveScope>,
    origins: HashMap<String, ArchiveOrigin>,
    projects: HashMap<String, ProjectRef>,
    show_environment: bool,
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
            scope: None,
            origins: HashMap::new(),
            projects: HashMap::new(),
            show_environment: false,
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
    fn archive_scope(&self, cx: &App) -> ArchiveScope {
        let owners = self.scoped_environment_owners(cx);
        let app = self.state.read(cx);
        let projects = self.scope.project.is_some().then(|| {
            self.scoped_project_members(cx).into_iter().map(|member| member.reference()).collect()
        });
        ArchiveScope {
            active_fallback: owners.is_empty() && self.scope.machine.is_none() && projects.is_none()
                && app.fleet.environments.is_empty(),
            owners,
            projects,
            active_connection: app.connection_id(),
        }
    }

    pub(super) fn load_archive(&mut self, cx: &mut Context<Self>) {
        let scope = self.archive_scope(cx);
        self.archive.scope = Some(scope.clone());
        self.archive.load_id += 1;
        let load_id = self.archive.load_id;
        self.archive.menu = None;
        self.archive.pending.clear();
        self.archive.action_error = None;
        self.archive.origins.clear();
        self.archive.projects.clear();
        self.archive.status = Status::Loading;
        self.archive.sync_entries();
        let app = self.state.read(cx);
        let fallback = scope.active_fallback.then(|| app.rpc(GET_ARCHIVED_SHELL_SNAPSHOT, json!({})));
        let destinations = scope.owners.iter().filter(|owner| app.scoped_ready(owner, ORCHESTRATION_READ_SCOPE))
            .map(|owner| app.connections.destination(owner, ORCHESTRATION_READ_SCOPE)).collect::<Vec<_>>();
        let runtime = app.runtime().clone();
        cx.spawn(async move |this, cx| {
            let mut snapshots = Vec::new();
            let mut failed = false;
            if let Some(task) = fallback {
                match task.await {
                    Ok(snapshot) => snapshots.push((None, snapshot)),
                    Err(_) => failed = true,
                }
            }
            for result in futures::future::join_all(destinations.into_iter().map(|destination| {
                let runtime = runtime.clone();
                async move {
                    let destination = destination.await.ok()?;
                    let response = runtime.spawn(async move {
                        destination.client.request(GET_ARCHIVED_SHELL_SNAPSHOT, json!({})).await.ok()
                            .map(|snapshot| (Some(destination.owner), snapshot))
                    }).await;
                    response.ok().flatten()
                }
            })).await {
                match result {
                    Some(snapshot) => snapshots.push(snapshot),
                    None => failed = true,
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.archive.load_id != load_id || this.archive_scope(cx) != scope {
                    return;
                }
                let logical_projects: HashMap<_, _> = this.project_groups(cx).into_iter().flat_map(|group| {
                    group.members.into_iter().map(move |member| (member.project.reference(), (group.key.clone(), group.label.clone())))
                }).collect();
                let app = this.state.read(cx);
                let scoped_groups = snapshots.iter().map(|(owner, snapshot)| {
                    (owner.as_ref(), archive_groups(app, owner.as_ref(), snapshot))
                }).collect::<Vec<_>>();
                let multiple = scoped_groups.iter().filter(|(_, groups)| !groups.is_empty()).count() > 1;
                let mut combined = Vec::<ArchiveGroup>::new();
                let mut index = HashMap::<String, usize>::new();
                for (owner, groups) in scoped_groups {
                    let environment = owner.and_then(|owner| app.scoped_environment(&owner.saved_id));
                    for mut group in groups {
                        let Some(owner) = owner else {
                            combined.push(group);
                            continue;
                        };
                        let project = ProjectRef { environment_id: owner.saved_id.clone(), project_id: group.project_id.clone() };
                        if scope.projects.as_ref().is_some_and(|projects| !projects.contains(&project)) {
                            continue;
                        }
                        let key = if let Some((key, title)) = logical_projects.get(&project).filter(|_| multiple) {
                            group.title = title.clone();
                            format!("group:{key}")
                        } else {
                            json!([owner.saved_id, group.project_id]).to_string()
                        };
                        this.archive.projects.entry(key.clone()).or_insert(project);
                        group.project_id = key.clone();
                        for thread in &mut group.threads {
                            let thread_key = json!([owner.saved_id, thread.id]).to_string();
                            let label = environment.and_then(|entry| entry.config.config.as_ref())
                                .map_or_else(|| owner.saved_id.clone(), |config| config.environment.label.clone());
                            this.archive.origins.insert(thread_key.clone(), ArchiveOrigin {
                                owner: owner.clone(), thread_id: std::mem::replace(&mut thread.id, thread_key), label,
                            });
                        }
                        if let Some(previous) = index.get(&key) {
                            combined[*previous].threads.extend(group.threads);
                        } else {
                            index.insert(key, combined.len());
                            combined.push(group);
                        }
                    }
                }
                if multiple {
                    for group in &mut combined {
                        group.threads.sort_by(|left, right| right.archived_or_created().cmp(left.archived_or_created()).then(right.id.cmp(&left.id)));
                    }
                }
                this.archive.show_environment = multiple;
                this.archive.status = if failed && combined.is_empty() { Status::Failed } else { Status::Loaded(combined) };
                if failed && matches!(&this.archive.status, Status::Loaded(_)) {
                    this.archive.action_error = Some(("Could not load all archived threads", t(cx, "One or more environments did not respond.").to_owned()));
                }
                this.archive.sync_entries();
                cx.notify();
            });
        }).detach();
    }

    fn may_change_archived_thread(&self, key: &str, cx: &App) -> bool {
        self.archive.origins.get(key).map_or_else(
            || self.archive.scope.as_ref().is_some_and(|scope| scope.active_fallback) && self.may(ORCHESTRATION_OPERATE_SCOPE, cx),
            |origin| self.state.read(cx).scoped_ready(&origin.owner, ORCHESTRATION_OPERATE_SCOPE),
        )
    }

    fn run_archive_action(&mut self, action: Action, thread_id: String, cx: &mut Context<Self>) {
        self.archive.menu = None;
        if self.archive.scope.as_ref() != Some(&self.archive_scope(cx)) || !self.may_change_archived_thread(&thread_id, cx)
            || self.archive.pending.contains(&thread_id) {
            return;
        }
        self.archive.action_error = None;
        let load_id = self.archive.load_id;
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
                            if this.archive.load_id != load_id || this.archive.scope.as_ref() != Some(&this.archive_scope(cx)) {
                                this.archive.pending.remove(&confirm_id);
                                cx.notify();
                                return;
                            }
                            this.dispatch_archive_action(action, confirm_id, load_id, cx);
                        });
                    });
                },
                move |_, cx| {
                    cx.defer(move |cx| {
                        let _ = cancel_view.update(cx, |this, cx| {
                            if this.archive.load_id == load_id {
                                this.archive.pending.remove(&cancel_id);
                                cx.notify();
                            }
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
        self.dispatch_archive_action(action, thread_id, load_id, cx);
    }

    fn dispatch_archive_action(&mut self, action: Action, thread_id: String, load_id: u64, cx: &mut Context<Self>) {
        if self.archive.load_id != load_id || self.archive.scope.as_ref() != Some(&self.archive_scope(cx)) || !self.may_change_archived_thread(&thread_id, cx) {
            self.archive.pending.remove(&thread_id);
            cx.notify();
            return;
        }
        let kind = if action == Action::Unarchive { "thread.unarchive" } else { "thread.delete" };
        let origin = self.archive.origins.get(&thread_id);
        let payload = commands::simple_thread_command(kind, &uuid_v4(), origin.map_or(thread_id.as_str(), |origin| &origin.thread_id));
        let app = self.state.read(cx);
        let destination = origin.map(|origin| app.connections.destination(&origin.owner, ORCHESTRATION_OPERATE_SCOPE));
        let fallback = destination.is_none().then(|| app.rpc(method::DISPATCH_COMMAND, payload.clone()));
        let runtime = app.runtime().clone();
        cx.spawn(async move |this, cx| {
            let result = if let Some(destination) = destination {
                match destination.await {
                    Ok(destination) => runtime.spawn(async move {
                        destination.client.request(method::DISPATCH_COMMAND, payload).await.map_err(|error| error.to_string())
                    }).await.unwrap_or_else(|error| Err(error.to_string())),
                    Err(error) => Err(format!("{error:?}")),
                }
            } else if let Some(task) = fallback {
                task.await.map_err(|error| error.to_string())
            } else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.archive.load_id != load_id || this.archive.scope.as_ref() != Some(&this.archive_scope(cx)) {
                    return;
                }
                this.archive.pending.remove(&thread_id);
                match result {
                    Ok(_) => {
                        if let Status::Loaded(groups) = &mut this.archive.status {
                            model::remove_thread(groups, &thread_id);
                        }
                        this.archive.sync_entries();
                    }
                    Err(error) => {
                        let title = if action == Action::Unarchive { "Failed to unarchive thread" } else { "Failed to delete thread" };
                        this.archive.action_error = Some((title, error));
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
        if self.archive.scope.as_ref() != Some(&self.archive_scope(cx)) {
            self.load_archive(cx);
        }
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
                let app = self.state.read(cx);
                let project = &groups[group];
                let visual = self.archive.projects.get(&project.project_id)
                    .map_or_else(|| ProjectVisual::for_project(app, &project.project_id), |key| ProjectVisual::for_scoped_project(app, key));
                let title = h_flex().gap(r(8.))
                    .child(visual.render(project_icon::size(16., cx), hsla(p.icon_muted), cx))
                    .child(project.title.clone());
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
                let pending = self.archive.pending.contains(&id) || !self.may_change_archived_thread(&id, cx);
                let mut description = model::description(thread, now);
                if self.archive.show_environment && let Some(origin) = self.archive.origins.get(&id) {
                    description = format!("{} · {description}", origin.label);
                }
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
                            if this.archive.pending.contains(&menu_id) || !this.may_change_archived_thread(&menu_id, cx) {
                                return;
                            }
                            this.archive.menu = Some((menu_id.clone(), event.position));
                            cx.notify();
                        }),
                    )
                    .child(
                        div().when(position > 0, |this| this.border_t_1().border_color(hsla(p.border).opacity(0.5))).child(
                            Row::new(SharedString::from(format!("archived-row-{id}")), thread.title.clone())
                                .description(description)
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

fn archive_groups(app: &AppState, owner: Option<&EnvironmentOwner>, snapshot: &serde_json::Value) -> Vec<ArchiveGroup> {
    let entry = owner.and_then(|owner| app.scoped_environment(&owner.saved_id));
    let config = if owner.is_some() { entry.and_then(|entry| entry.config.config.as_ref()) } else { app.server_config.config.as_ref() };
    let environment_id = config.map_or("", |config| config.environment.environment_id.as_str());
    let hidden_tags = config.and_then(|config| config.settings.hidden_tags());
    let active_env = app.server_config.config.as_ref().map_or("", |cfg| cfg.environment.environment_id.as_str());
    let already_hidden: HashSet<_> = app.sidebar.hidden_thread_ids.iter().map(|id| ThreadKey::new(active_env, id)).collect();
    let hiding_anywhere = app.hiding_thread_tags() || hidden_tags.is_some() || !already_hidden.is_empty();
    let loaded_elsewhere: HashSet<_> = if hiding_anywhere {
        app.fleet.environments.iter().flat_map(|entry| {
            let env = entry.summary.owner.canonical_id.as_deref().unwrap_or(&entry.summary.owner.saved_id);
            entry.summary.threads.iter().map(move |thread| ThreadKey::new(env, &thread.thread.id))
        }).chain(app.shell.snapshot.iter().flat_map(|snapshot| {
            snapshot.threads.iter().map(|thread| ThreadKey::new(active_env, &thread.id))
        })).collect()
    } else { HashSet::new() };
    let visibility = ArchiveVisibility { environment_id, hidden_tags: hidden_tags.as_deref(), already_hidden: &already_hidden, loaded_elsewhere: &loaded_elsewhere, hiding_anywhere };
    model::groups_filtered(snapshot, Some(&visibility))
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
                        lineage: None,
                        delegated_from: None,
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
