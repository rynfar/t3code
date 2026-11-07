//! Sidebar state on `AppState`: the derived model, shelf toggles, the Working-shelf tracker,
//! Undo, the open thread's visit watermark, and the thread actions the menu, row buttons,
//! keybindings and palette share (`apps/web/src/hooks/useThreadActions.ts`).

use std::sync::Arc;
use std::time::Duration;

use gpui::{ClipboardItem, Context, Task};
use serde_json::Value;
use t3_client::session::RpcError;
use t3_core::sidebar::inbox::InboxReturnTracker;
use t3_core::settings::snooze::menu_settings;
use t3_core::sidebar::settled::{self, DEFAULT_SNOOZE_PRESETS, SnoozePreset, SnoozePresetSetting};
use t3_core::sidebar::sort::top_of_pinned_order_key;
use t3_core::sidebar::time::{LocalTime, Ms, Zone};
use t3_core::families::MountedReads;
use t3_core::shell::{ShellState, ShellStreamItem};
use t3_core::sidebar::history::NavEntry;
use t3_core::sidebar::thread::same_apart_from_streaming;
use t3_core::sidebar::visits::{OpenEnvironment, OpenHistory, VisitDecision, VisitTracker, VisitedShell};
use t3_core::sidebar::{ModelInput, OlderSettledShelf, Shelf, ShelfState, SidebarModel, SidebarSettings};
use t3_core::thread_actions::{
    self as actions, PlanFor, RenameOutcome, ThreadAction, ThreadUndoStore, UndoAction, UndoKind, UndoStep,
};

use crate::app_state::{AppEvent, AppState, Phase};
use crate::toast::{Toast, ToastKind};
use t3_client::supervisor::ConnectionStatus;

/// A thread delta that only moves streaming fields; the sidebar skips its rebuild.
pub fn is_streaming_tick(shell: &ShellState, item: &ShellStreamItem) -> bool {
    match item {
        ShellStreamItem::ThreadUpdated { location, thread, .. } if location == "active" => {
            shell.thread(&thread.id).is_some_and(|previous| same_apart_from_streaming(previous, thread))
        }
        _ => false,
    }
}

/// What this environment lets the menu offer. The web reads these from the server config and the
/// session's scopes (`useThreadActionMenu.ts`); until the native client subscribes to the server
/// config, the defaults assume a current server and an operator session, and keep auto-settle
/// opt-out (a newer capability) hidden. The server-config subscription should set this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadCapabilities {
    pub can_operate: bool,
    pub config_loaded: bool,
    pub auto_settle_opt_out: bool,
}

impl Default for ThreadCapabilities {
    fn default() -> Self {
        ThreadCapabilities { can_operate: true, config_loaded: true, auto_settle_opt_out: false }
    }
}

pub fn now_ms() -> Ms {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as Ms).unwrap_or(0)
}

/// The OS time zone, including DST for any instant.
pub struct SystemZone;

impl Zone for SystemZone {
    #[cfg(unix)]
    fn offset_seconds_at(&self, utc_ms: Ms) -> i64 {
        let seconds = utc_ms.div_euclid(1000) as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: `localtime_r` writes only into `tm`, which outlives the call.
        let ok = unsafe { !libc::localtime_r(&seconds, &mut tm).is_null() };
        if ok { tm.tm_gmtoff as i64 } else { 0 }
    }

    #[cfg(not(unix))]
    fn offset_seconds_at(&self, _utc_ms: Ms) -> i64 {
        0
    }
}

pub fn time_label(time: LocalTime) -> String {
    settled::default_time_label(time)
}

#[derive(Clone, PartialEq)]
pub(crate) enum UndoContext {
    Thread(Box<crate::context_menu::ThreadMenuOwner>),
    Draft { environment: Option<String>, canonical: Option<String>, connection: Option<u64>, generation: u64, project: Box<t3_core::shell::ProjectShell> },
    Unavailable { id: String, draft: bool },
}

#[derive(Default)]
pub struct UndoContexts {
    store: ThreadUndoStore,
    contexts: Vec<(String, UndoContext)>,
    pending: Vec<actions::Claim>,
    active_threads: std::collections::HashMap<String, UndoContext>,
    active_drafts: std::collections::HashMap<String, UndoContext>,
}

impl UndoContexts {
    fn token(&mut self, context: UndoContext) -> String {
        if let Some((token, _)) = self.contexts.iter().find(|(_, current)| *current == context) { return token.clone(); }
        let token = uuid(); self.contexts.push((token.clone(), context)); token
    }
    fn active_token(&mut self, kind: UndoKind, id: &str) -> String {
        let draft = kind == UndoKind::Discard;
        let context = if draft { &self.active_drafts } else { &self.active_threads }.get(id).cloned().unwrap_or_else(|| UndoContext::Unavailable { id:id.into(), draft });
        self.token(context)
    }
    pub fn begin(&mut self, kind: UndoKind, id: &str) -> actions::Claim { let token = self.active_token(kind,id); let claim=self.store.begin(kind,&token); self.pending.push(claim.clone()); claim }
    fn begin_owned(&mut self, kind: UndoKind, owner: crate::context_menu::ThreadMenuOwner) -> actions::Claim { let token = self.token(UndoContext::Thread(Box::new(owner))); let claim=self.store.begin(kind,&token); self.pending.push(claim.clone()); claim }
    pub fn invalidate(&mut self, kind: UndoKind, id: &str) { let token = self.active_token(kind,id); self.store.invalidate(kind,&token); }
    fn invalidate_owned(&mut self, kind: UndoKind, owner: &crate::context_menu::ThreadMenuOwner) { let token = self.token(UndoContext::Thread(Box::new(owner.clone()))); self.store.invalidate(kind,&token); }
    pub fn expire_placement(&mut self, id: &str, archive: bool) { let token = self.active_token(UndoKind::Pin,id); self.store.expire_placement(&token,archive); }
    fn expire_owned(&mut self, owner: &crate::context_menu::ThreadMenuOwner, archive: bool) { let token = self.token(UndoContext::Thread(Box::new(owner.clone()))); self.store.expire_placement(&token,archive); }
    pub fn is_current(&self, claim: &actions::Claim) -> bool { self.store.is_current(claim) }
    pub fn finish(&mut self, claim: &actions::Claim) { self.pending.retain(|pending| pending != claim); self.store.finish(claim); self.prune(); }
    pub fn show(&mut self, action: UndoAction, claim: actions::Claim, plan: Vec<UndoStep>, now: Ms) { self.pending.retain(|pending| *pending != claim); self.store.show(action,claim,plan,now); }
    pub fn notice(&self) -> Option<actions::UndoNotice> { self.store.notice() }
    pub fn expires_at(&self) -> Option<Ms> { self.store.expires_at() }
    pub fn expire(&mut self, now: Ms) -> bool { let expired=self.store.expire(now); self.prune(); expired }
    fn prune(&mut self) { self.pending.retain(|claim| self.store.is_current(claim)); if self.pending.is_empty() && self.store.notice().is_none() { self.contexts.clear(); } }
    pub(crate) fn take_latest_group(&mut self) -> Vec<(UndoContext,Vec<UndoStep>)> {
        self.store.take_latest_group().into_iter().filter_map(|(token,plan)| self.contexts.iter().find(|(current,_)| *current == token).map(|(_,context)| (context.clone(),plan))).collect()
    }
}

#[derive(Default)]
pub struct SidebarController {
    pub model: Arc<SidebarModel>,
    pub fleet: Arc<t3_core::sidebar::fleet::ScopedSidebarModel>,
    pub fleet_state: t3_core::sidebar::fleet::ScopedSidebarState,
    pub owner_extra_threads: std::collections::HashMap<t3_core::sidebar::grouping::EnvironmentOwner, Vec<t3_core::sidebar::fleet::ScopedThread>>,
    pub owner_favicons: std::collections::HashMap<t3_core::sidebar::grouping::EnvironmentOwner, super::favicons::FaviconStore>,
    pub owner_omitted: std::collections::HashMap<t3_core::sidebar::grouping::EnvironmentOwner, crate::omitted_rows::OmittedRows>,
    pub fleet_selection: std::collections::HashSet<t3_core::sidebar::tag_filter::ThreadKey>,
    pub fleet_anchor: Option<t3_core::sidebar::tag_filter::ThreadKey>,
    pub(crate) route_observed: Option<t3_core::sidebar::tag_filter::ThreadKey>,
    pub(crate) route_pending: Option<t3_core::sidebar::tag_filter::ThreadKey>,
    pub(crate) search_input: Option<gpui::WeakEntity<gpui_component::input::InputState>>,
    pub grouping_projects: Vec<t3_core::sidebar::grouping::ScopedProject>,
    pub grouping_threads: Vec<t3_core::sidebar::fleet::ScopedThread>,
    pub fleet_drag: Option<(t3_core::sidebar::tag_filter::ThreadKey, t3_core::sidebar::Section)>,
    pub shelves: ShelfState,
    pub(crate) settled_scope_keys: Vec<String>,
    pub settings: SidebarSettings,
    pub undo: UndoContexts,
    pub capabilities: ThreadCapabilities,
    pub collapsed: bool,
    pub history: t3_core::sidebar::history::NavHistory,
    /// Project ids the thread list is filtered to, or every project.
    pub project_scope: Option<std::collections::HashSet<String>>,
    pub project_filter_scope: t3_core::sidebar::grouping::ProjectScope,
    pub selection: t3_core::sidebar::selection::ThreadSelection,
    /// The thread being dragged and the section it left.
    pub drag: Option<(String, t3_core::sidebar::Section)>,
    pub favicons: super::favicons::FaviconStore,
    pub drafts_to_restore: Vec<String>,
    pub hidden_thread_ids: std::collections::HashSet<String>,
    hiding_tags: bool,
    tracker: InboxReturnTracker,
    visit: Option<VisitTracker>,
    visit_target: Option<VisitTarget>,
    visit_revision: u64,
    wake_timer: Option<Task<()>>,
    visit_timer: Option<Task<()>>,
    pub(crate) undo_timer: Option<Task<()>>,
    // The delete stream can precede its reply; only that request may hold the selected mount.
    pending_deletion: Option<PendingDeletion>,
    deletion_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VisitTarget {
    environment: OpenEnvironment,
    thread: String,
    connection: Option<u64>,
    mount: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeleteTarget {
    environment: Option<String>,
    canonical: Option<String>,
    connection: Option<u64>,
    selected: Option<String>,
    visit: Option<VisitTarget>,
}

#[derive(Clone)]
pub(super) struct Deletion {
    pub target: DeleteTarget,
    pub doomed: Arc<std::collections::HashSet<String>>,
}

struct PendingDeletion {
    token: u64,
    target: DeleteTarget,
}

struct DeletionReply {
    owner: Option<u64>,
    target: DeleteTarget,
    id: String,
    project: String,
    fallback: Option<String>,
}

impl DeleteTarget {
    fn same_scope(&self, current: &Self) -> bool {
        self.environment == current.environment
            && self.canonical == current.canonical
            && self.connection == current.connection
    }

    fn route(
        &self,
        current: &Self,
        deleted: &str,
        ok: bool,
        fallback: Option<String>,
    ) -> Option<Option<String>> {
        (ok && self.selected.as_deref() == Some(deleted) && self == current).then_some(fallback)
    }
}

#[derive(Clone)]
struct VisitTimer {
    target: VisitTarget,
    revision: u64,
    visited_at: Ms,
}

impl SidebarController {
    fn visit_decision(
        &mut self,
        history: &mut OpenHistory,
        target: Option<(OpenEnvironment, Option<u64>, &str)>,
        shell: Option<VisitedShell>,
        now: Ms,
    ) -> VisitDecision {
        self.visit_timer = None;
        self.visit_revision += 1;
        let Some((environment, connection, id)) = target else {
            self.visit = None;
            self.visit_target = None;
            return VisitDecision::Skip;
        };
        if self.visit_target.as_ref().is_none_or(|current| {
            current.environment != environment
                || current.thread != id
                || current.connection != connection
        }) {
            let opened = history.claim(&environment, id, now);
            self.visit = Some(VisitTracker::new(id, opened));
            self.visit_target = Some(VisitTarget {
                environment,
                thread: id.to_owned(),
                connection,
                mount: history.next_mount(),
            });
        }
        shell.map_or(VisitDecision::Skip, |shell| {
            self.visit.as_mut().unwrap().on_shell(shell, now)
        })
    }

    fn pending_visit(&self, visited_at: Ms) -> Option<VisitTimer> {
        Some(VisitTimer {
            target: self.visit_target.clone()?,
            revision: self.visit_revision,
            visited_at,
        })
    }

    fn fire_visit(
        &mut self,
        timer: &VisitTimer,
        environment: Option<&OpenEnvironment>,
        connection: Option<u64>,
        selected: Option<&str>,
        now: Ms,
    ) -> VisitDecision {
        if self.visit_target.as_ref() != Some(&timer.target)
            || self.visit_revision != timer.revision
            || environment != Some(&timer.target.environment)
            || connection != timer.target.connection
            || selected != Some(timer.target.thread.as_str())
        {
            return VisitDecision::Skip;
        }
        self.visit_timer = None;
        self.visit_revision += 1;
        self.visit.as_mut().map_or(VisitDecision::Skip, |tracker| {
            tracker.fire(timer.visited_at, now)
        })
    }
}

fn history_entry_exists(shell: &ShellState, mounts: &MountedReads, hidden: &std::collections::HashSet<String>, hiding: bool, entry: &NavEntry) -> bool {
    match entry {
        NavEntry::Thread(id) => {
            let loaded = mounts.thread(shell, id).is_some();
            loaded && t3_core::sidebar::tag_filter::route_visible(id, loaded, hidden, hiding)
        }
        NavEntry::Draft(project_id) => shell.project(project_id).is_some(),
    }
}

fn connected(phase: &Phase) -> bool {
    matches!(phase, Phase::Online(ConnectionStatus::Connected))
}

impl AppState {
    /// Re-derives the sidebar from the shell. Cheap enough per batch; rows notify only the sidebar.
    pub(crate) fn rebuild_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar.settings.working_shelf_enabled = crate::settings::store::client(cx).working_shelf();
        self.sync_undo_contexts();
        self.release_stale_deletion(cx);
        let now = now_ms();
        let extras = self.omitted.mounts.extras(&self.shell);
        let tags = self.server_config.config.as_ref().and_then(|config| config.settings.hidden_tags());
        self.sidebar.hiding_tags = tags.is_some();
        let environment_id = self.server_config.config.as_ref().map(|config| config.environment.environment_id.as_str()).unwrap_or("local");
        self.sidebar.settings.grouping = crate::settings::store::client(cx).project_grouping_settings();
        self.sidebar.settings.grouping.environment_id = Some(environment_id.to_owned());
        self.sidebar.project_scope = if self.sidebar.project_filter_scope.is_empty() {
            None
        } else {
            let groups = self.shell.snapshot.as_ref().map(|snapshot| {
                t3_core::sidebar::grouping::build_project_groups(environment_id, &snapshot.projects, &self.sidebar.settings.grouping)
            }).unwrap_or_default();
            self.sidebar.project_filter_scope.members(&groups, self.shell.synchronized && self.server_config.config.is_some())
        };
        let threads: Vec<_> = self.shell.snapshot.iter().flat_map(|snapshot| snapshot.threads.iter()).chain(extras.iter().copied()).collect();
        self.sidebar.hidden_thread_ids = t3_core::sidebar::tag_filter::hidden_thread_keys(
            &[t3_core::sidebar::tag_filter::Environment { environment_id, threads: &threads, hidden_tags: tags.as_deref() }],
            &Default::default(), &Default::default(), tags.is_some(),
        ).into_iter().map(|key| key.thread_id).collect();
        let older = self.omitted.older();
        let frontier = self.omitted.pages.listing_frontier(older.as_ref());
        let older_settled = older.as_ref().map(|older| OlderSettledShelf { older, frontier });
        let model = match &self.shell.snapshot {
            Some(snapshot) => SidebarModel::build(
                &ModelInput {
                    snapshot,
                    config_loaded: self.server_config.config.is_some(),
                    extra_threads: &extras,
                    older_settled,
                    settings: &self.sidebar.settings,
                    shelves: &self.sidebar.shelves,
                    selected_thread_id: self.selected_thread.as_deref(),
                    project_scope: self.sidebar.project_scope.as_ref(),
                    hidden_thread_ids: Some(&self.sidebar.hidden_thread_ids),
                    now,
                    dragging: self.sidebar.drag.as_ref().map(|(_, from)| *from),
                },
                &mut self.sidebar.tracker,
            ),
            None => SidebarModel::default(),
        };
        let next = model.next_change_at;
        let listed_settled = model.listed_settled_count;
        self.sidebar.model = Arc::new(model);
        self.retire_hidden_notifications(cx);
        if self.selected_thread.as_deref().is_some_and(|id| {
            t3_core::sidebar::tag_filter::route_visibility(id, self.thread_shell(id).is_some(), &self.sidebar.hidden_thread_ids, self.hiding_thread_tags())
                == t3_core::sidebar::tag_filter::RouteVisibility::Hidden
        }) {
            self.clear_selection(cx);
            return;
        }
        self.grow_settled_shelf(listed_settled, cx);
        self.sidebar.wake_timer = next.map(|at| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis((at - now).max(0) as u64 + 1)).await;
                let _ = this.update(cx, |state, cx| {
                    state.rebuild_sidebar(cx);
                    cx.emit(AppEvent::Shell);
                });
            })
        });
        self.track_visit(cx);
        self.rebuild_fleet_sidebar(cx);
        if connected(&self.phase) {
            self.ensure_favicons(cx);
        }
    }

    fn rebuild_fleet_sidebar(&mut self, cx: &mut Context<Self>) {
        if self.fleet.environments.is_empty() { return; }
        self.reveal_scoped_route(cx);
        let groups = self.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx);
        let scope_keys = self.scoped_project_scope(&groups);
        if self.sidebar.settled_scope_keys != scope_keys {
            self.sidebar.settled_scope_keys = scope_keys;
            self.sidebar.shelves.settled_visible = t3_core::sidebar::SETTLED_TAIL_INITIAL_COUNT;
        }
        self.reconcile_scoped_settled_scopes(&groups);
        let environments: Vec<_> = self.fleet.environments.iter().map(|entry| entry.summary.as_ref()).collect();
        let selected = self.selected_thread.as_deref().and_then(|id| self.active_thread_key(id));
        let hidden = self.notifications.visibility.hidden.union(&self.notifications.visibility.delegated_under_parent).cloned().collect();
        let owners: std::collections::HashSet<_> = environments.iter().map(|entry| entry.owner.clone()).collect();
        self.sidebar.owner_omitted.retain(|owner, _| owners.contains(owner));
        let older_values: Vec<_> = self.sidebar.owner_omitted.iter().filter_map(|(owner, omitted)| omitted.older().map(|older| (owner, older))).collect();
        let older: Vec<_> = older_values.iter().map(|(owner, older)| t3_core::sidebar::fleet::ScopedOlderSettled {
            owner, older, frontier: self.sidebar.owner_omitted.get(*owner).and_then(|omitted| omitted.pages.listing_frontier(Some(older))),
        }).collect();
        let extra_values = environments.iter().filter_map(|summary| {
            let omitted = self.sidebar.owner_omitted.get(&summary.owner)?;
            let ids = summary.threads.iter().map(|row| row.thread.id.clone()).collect();
            let cut = summary.older_settled.as_ref().map_or(0, |cut| cut.as_of_sequence);
            let threads = omitted.mounts.extras_for(summary.owner.shell_generation, cut, &ids, &summary.removed_at).into_iter().map(|thread| t3_core::sidebar::fleet::ScopedThread::from_shell(&summary.owner, thread, None)).collect::<Vec<_>>();
            Some((&summary.owner, threads))
        }).collect::<Vec<_>>();
        let extras = extra_values.iter().map(|(owner, threads)| t3_core::sidebar::fleet::ScopedExtraThreads { owner, threads }).collect::<Vec<_>>();
        let model = t3_core::sidebar::fleet::ScopedSidebarModel::build(&t3_core::sidebar::fleet::ScopedSidebarInput {
            environments: &environments, groups: &groups, preferences: &self.project_preferences,
            settings: &self.sidebar.settings, shelves: &self.sidebar.shelves, selected: selected.as_ref(), hidden: &hidden,
            older: &older, extra_threads: &extras, now: now_ms(), folder_mode: crate::settings::store::client(cx).boring_sidebar_enabled(),
            dragging: self.sidebar.fleet_drag.as_ref().map(|(_, section)| *section),
        }, &mut self.sidebar.fleet_state);
        self.sidebar.owner_extra_threads = extra_values.into_iter().map(|(owner, threads)| (owner.clone(), threads)).collect();
        self.sidebar.fleet_selection.retain(|key| model.thread(key).is_some());
        let next = model.next_change_at;
        let listed = model.listed_settled_count;
        self.sidebar.fleet = Arc::new(model);
        self.grow_scoped_settled_shelf(listed, cx);
        if let Some(at) = next {
            self.sidebar.wake_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis((at - now_ms()).max(0) as u64 + 1)).await;
                let _ = this.update(cx, |state, cx| { state.rebuild_sidebar(cx); cx.emit(AppEvent::Shell); cx.emit(AppEvent::Notice); });
            }));
        }
    }


    pub(crate) fn project_groups_for(&self, consumer: t3_core::sidebar::grouping::GroupConsumer, cx: &gpui::App) -> Vec<t3_core::sidebar::grouping::ScopedProjectGroup> {
        let doc = crate::settings::store::client(cx);
        t3_core::sidebar::grouping::build_scoped_project_groups(&t3_core::sidebar::grouping::GroupInput {
            projects: &self.sidebar.grouping_projects, threads: &self.sidebar.grouping_threads, settings: &doc.project_grouping_settings(), preferences: &self.project_preferences,
            preferred_environment: self.environments.active.as_deref(), consumer,
            sort: match doc.project_sort_order() {
                t3_core::client_settings::ProjectSortOrder::CreatedAt => t3_core::sidebar::grouping::ProjectSortOrder::CreatedAt,
                t3_core::client_settings::ProjectSortOrder::Manual => t3_core::sidebar::grouping::ProjectSortOrder::Manual,
                _ => t3_core::sidebar::grouping::ProjectSortOrder::UpdatedAt,
            },
        })
    }

    pub(crate) fn run_scoped_thread_action(&mut self, key: &t3_core::sidebar::tag_filter::ThreadKey, action: ThreadAction, shown: &[SnoozePreset], cx: &mut Context<Self>) {
        let Some(owner) = crate::context_menu::ThreadMenuOwner::capture_scoped(self, key) else { return; };
        if !owner.allows(self, &action) { return; }
        let id = &key.thread_id;
        let command = match action {
            ThreadAction::Settle => actions::settle(&uuid(), id),
            ThreadAction::Unsettle => actions::unsettle(&uuid(), id),
            ThreadAction::Pin => actions::pin(&uuid(), id, None),
            ThreadAction::Unpin => actions::unpin(&uuid(), id),
            ThreadAction::Unsnooze => actions::unsnooze(&uuid(), id),
            ThreadAction::Archive => actions::archive(&uuid(), id),
            ThreadAction::Delete => { self.delete_scoped_threads(vec![owner], cx); return; }
            ThreadAction::MarkUnread => actions::mark_unread(&uuid(), id),
            ThreadAction::RegenerateTitle => actions::regenerate_title(&uuid(), id),
            ThreadAction::AutoSettle | ThreadAction::AutoSettleEnabled => actions::set_auto_settle(&uuid(), id, true),
            ThreadAction::AutoSettleDisabled => actions::set_auto_settle(&uuid(), id, false),
            ThreadAction::SnoozePreset(preset) => {
                let Some(environment) = self.scoped_environment(&key.environment_id) else { return; };
                let settings = environment.config.config.as_ref().map(|config| menu_settings(&config.settings.threads.snooze_presets)).unwrap_or_default();
                let Some(preset) = settled::pick_snooze_preset(shown, &preset, now_ms(), &settings, &SystemZone, &time_label) else { return; };
                actions::snooze(&uuid(), id, preset.snoozed_until)
            }
            ThreadAction::NewThreadOnBranch => {
                self.navigate_scoped(owner.destination.clone().unwrap(), crate::app_state::ScopedRouteAction::Branch(id.clone()), cx);
                return;
            }
            ThreadAction::ProjectSettings => {
                let project = owner.project_ref().unwrap();
                self.navigate_scoped(owner.destination.clone().unwrap(), crate::app_state::ScopedRouteAction::ProjectSettings(project.project_id), cx);
                return;
            }
            ThreadAction::FilterByProject => {
                let groups = self.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx);
                if let Some(reference) = owner.project_ref() && let Some(group) = groups.iter().find(|group| group.member_refs.contains(&reference)) {
                    let keys = if self.scoped_project_scope(&groups).contains(&group.key) { Vec::new() } else { vec![group.key.clone()] };
                    self.set_scoped_project_scope(keys, cx);
                }
                return;
            }
            ThreadAction::TurnIntoProject => { self.promote_scoped(owner, cx); return; }
            ThreadAction::CopyThreadId => { self.copy(id.clone(), "Thread ID copied", Some(id.clone()), cx); return; }
            ThreadAction::CopyPath => {
                let path = self.scoped_thread(key).and_then(|row| row.thread.worktree_path.clone()).or_else(|| owner.project_ref().and_then(|key| self.scoped_project(&key)).map(|project| project.project.workspace_root.clone()));
                if let Some(path) = path { self.copy(path.clone(), "Path copied", Some(path), cx); }
                return;
            }
            ThreadAction::CopyBranch => {
                if let Some(branch) = self.scoped_thread(key).and_then(|row| row.thread.branch.clone()) { self.copy(branch, "Copied branch name", None, cx); }
                return;
            }
            ThreadAction::CopyThread => {
                if let Some(environment) = self.scoped_environment(&key.environment_id)
                    && let Some(row) = self.scoped_thread(key)
                    && let Some(reference) = actions::thread_reference(&row.thread.title, environment.summary.owner.canonical_id.as_deref(), id) {
                    crate::composer::clipboard::write(reference.text, reference.fragment, cx);
                    self.show_toast(Toast::new(ToastKind::Success, "Thread copied").description(reference.description), cx);
                }
                return;
            }
            _ => return,
        };
        self.run_scoped_command(owner, command, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE, "Thread action failed", cx);
    }

    pub(crate) fn observe_scoped_route(&mut self, key: t3_core::sidebar::tag_filter::ThreadKey) {
        if self.sidebar.route_observed.as_ref() != Some(&key) {
            self.sidebar.route_observed = Some(key.clone());
            self.sidebar.route_pending = Some(key);
        }
    }

    fn reveal_scoped_route(&mut self, cx: &mut Context<Self>) {
        let Some(current) = self.selected_thread.as_deref().and_then(|id| self.active_thread_key(id)) else { return; };
        self.observe_scoped_route(current.clone());
        let Some(key) = self.sidebar.route_pending.clone().filter(|key| *key == current) else { return; };
        if self.sidebar.search_input.as_ref().and_then(gpui::WeakEntity::upgrade).is_some_and(|input| !input.read(cx).value().trim().is_empty())
            || !self.fleet.environments.iter().all(|entry| entry.summary.synchronized && entry.config.config.is_some()) { return; }
        let Some(thread) = self.scoped_thread(&key) else { return; };
        if self.notifications.visibility.hidden.contains(&key) || self.notifications.visibility.delegated_under_parent.contains(&key)
            || (self.environments.active.as_deref() == Some(&key.environment_id) && self.sidebar.hidden_thread_ids.contains(&key.thread_id)) { return; }
        let reference = thread.project_ref();
        let groups = self.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx);
        let Some(group) = groups.iter().find(|group| !group.archived && group.member_refs.contains(&reference)) else { return; };
        let scope = self.scoped_project_scope(&groups);
        self.sidebar.route_pending = None;
        if !scope.is_empty() && !scope.contains(&group.key) {
            self.project_preferences.scope_keys.clear();
            self.sidebar.fleet_selection.clear();
            self.save_project_preferences(cx);
        }
    }

    pub(crate) fn scoped_project_scope(&self, groups: &[t3_core::sidebar::grouping::ScopedProjectGroup]) -> Vec<String> {
        let ready = self.fleet.environments.iter().all(|entry| entry.summary.synchronized && entry.config.config.is_some());
        self.project_preferences.resolved_scope(groups, ready)
    }

    pub(crate) fn set_scoped_project_scope(&mut self, keys: Vec<String>, cx: &mut Context<Self>) {
        self.project_preferences.scope_keys = keys;
        self.sidebar.fleet_selection.clear();
        self.sidebar.fleet_anchor = None;
        self.save_project_preferences(cx);
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Shell);
    }

    pub(crate) fn save_project_preferences(&mut self, cx: &mut Context<Self>) {
        let save = t3_client::store::save_project_preferences(&self.project_preferences);
        cx.spawn(async move |this, cx| {
            if let Err(error) = save.await {
                let _ = this.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Could not save sidebar preferences").description(error), cx));
            }
        }).detach();
    }

    fn sync_undo_contexts(&mut self) {
        let ids = self.shell.snapshot.as_ref().map(|snapshot| snapshot.threads.iter().chain(&snapshot.archived_threads).chain(self.omitted.mounts.extras(&self.shell)).map(|thread| thread.id.clone()).collect::<Vec<_>>()).unwrap_or_default();
        let threads = ids.into_iter().filter_map(|id| crate::context_menu::ThreadMenuOwner::capture_undo(self,&id).map(|owner| (id,UndoContext::Thread(Box::new(owner))))).collect();
        let drafts = self.shell.snapshot.as_ref().map(|snapshot| snapshot.projects.iter().map(|project| (project.id.clone(),UndoContext::Draft { environment:self.environments.active.clone(),canonical:self.server_config.config.as_ref().map(|config| config.environment.environment_id.clone()),connection:self.connection_id(),generation:self.shell.generation,project:Box::new(project.clone()) })).collect()).unwrap_or_default();
        self.sidebar.undo.active_threads = threads;
        self.sidebar.undo.active_drafts = drafts;
    }

    pub(crate) fn scoped_undo_claim(&mut self, owner: &crate::context_menu::ThreadMenuOwner, command: &str) -> Option<(actions::Claim, UndoAction, Vec<UndoStep>)> {
        let thread = self.scoped_thread(&owner.key()?)?.thread.clone();
        let store = &mut self.sidebar.undo;
        for kind in match command { "thread.pin" => vec![UndoKind::Pin, UndoKind::Settle, UndoKind::Snooze], "thread.unsettle" => vec![UndoKind::Settle], "thread.unsnooze" => vec![UndoKind::Snooze], "thread.settle" => vec![UndoKind::Pin, UndoKind::Snooze], _ => Vec::new() } { store.invalidate_owned(kind, owner); }
        if matches!(command, "thread.archive" | "thread.delete") { store.expire_owned(owner, command == "thread.delete"); }
        let (kind, action, plan) = match command {
            "thread.settle" => (UndoKind::Settle, UndoAction::Settled, actions::plan_undo(PlanFor::Settle, thread.pinned, thread.pin_order_key.as_deref(), thread.snoozed_until)),
            "thread.unpin" => (UndoKind::Pin, UndoAction::Unpinned, actions::plan_undo(PlanFor::Unpin, thread.pinned, thread.pin_order_key.as_deref(), thread.snoozed_until)),
            "thread.snooze" => (UndoKind::Snooze, UndoAction::Snoozed, vec![UndoStep::Unsnooze]),
            "thread.archive" => (UndoKind::Archive, UndoAction::Archived, vec![UndoStep::Unarchive]), _ => return None,
        };
        Some((store.begin_owned(kind, owner.clone()), action, plan))
    }

    fn scoped_deletion_missing(&self, owner: &crate::context_menu::ThreadMenuOwner) -> bool {
        owner.response_after_deletion(self)
            && owner.key().is_some_and(|key| self.scoped_thread(&key).is_none_or(|row| row.thread.deleted))
    }

    fn deletion_interrupted(error: &RpcError) -> bool {
        match error {
            RpcError::Closed => true,
            RpcError::Failure(failure) => failure.tag.is_none() && failure.message == "Interrupted" && failure.error.is_null(),
        }
    }

    pub(crate) fn delete_scoped_threads(&mut self, owners: Vec<crate::context_menu::ThreadMenuOwner>, cx: &mut Context<Self>) {
        if owners.is_empty() { return; }
        if owners.iter().any(|owner| !(owner.current(self) || self.scoped_deletion_missing(owner)) || !owner.can_operate_owner(self)) {
            self.show_toast(Toast::new(ToastKind::Error, "Thread deletion incomplete").description("0 thread(s) deleted; stopped: destination or permission changed"), cx);
            return;
        }
        let target = self.delete_target();
        let resolves = owners.iter().map(|owner| self.connections.destination(owner.destination.as_ref().unwrap(), t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)).collect::<Vec<_>>();
        cx.spawn(async move |this, cx| {
            let mut destinations = Vec::new();
            for resolve in resolves {
                let Ok(destination) = resolve.await else {
                    let _ = this.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Thread deletion incomplete").description("0 thread(s) deleted; stopped: destination or permission changed"), cx));
                    return;
                };
                destinations.push(destination);
            }
            if !this.read_with(cx, |state, _| owners.iter().zip(&destinations).all(|(owner, destination)| {
                destination.is_current(&state.fleet) && (owner.current(state) || state.scoped_deletion_missing(owner)) && owner.can_operate_owner(state)
                    && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))
            })).unwrap_or(false) {
                let _ = this.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Thread deletion incomplete").description("0 thread(s) deleted; stopped: destination or permission changed"), cx));
                return;
            }
            let mut completed = 0;
            let mut failures = 0;
            let mut interruption = None;
            for (owner, destination) in owners.iter().zip(&destinations) {
                let current = |state: &AppState| destination.is_current(&state.fleet) && owner.current(state) && owner.can_operate_owner(state)
                    && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
                let missing = this.read_with(cx, |state, _| destination.is_current(&state.fleet) && state.scoped_deletion_missing(owner) && owner.can_operate_owner(state)
                    && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))).unwrap_or(false);
                if missing { continue; }
                if !this.read_with(cx, |state, _| current(state)).unwrap_or(false) {
                    interruption = Some("destination or permission changed".to_owned()); break;
                }
                let stop = this.read_with(cx, |state, _| owner.key().and_then(|key| state.scoped_thread(&key)).is_some_and(|row| row.thread.runtime.is_some())).unwrap_or(false);
                if stop {
                    if let Err(error) = destination.client.dispatch(t3_core::commands::stop_thread(&uuid(), &owner.thread)).await {
                        if Self::deletion_interrupted(&error) { interruption = Some(error.to_string()); break; }
                        failures += 1;
                        continue;
                    }
                    if !this.read_with(cx, |state, _| current(state)).unwrap_or(false) {
                        interruption = Some("destination or permission changed".to_owned()); break;
                    }
                }
                let reply = this.update(cx, |state, cx| {
                    if !current(state) { return None; }
                    let reply = if target.environment.as_deref() == owner.destination.as_ref().map(|destination| destination.saved_id.as_str()) {
                        let doomed = owners.iter().filter(|candidate| candidate.destination == owner.destination).map(|candidate| candidate.thread.clone()).collect();
                        state.prepare_deletion(owner.thread.clone(), &Deletion { target: target.clone(), doomed: Arc::new(doomed) }, crate::settings::store::client(cx).thread_sort_order(), now_ms())
                    } else { None };
                    Some(reply)
                }).ok().flatten();
                let Some(reply) = reply else { interruption = Some("destination or permission changed".to_owned()); break; };
                let result = destination.client.dispatch(actions::delete(&uuid(), &owner.thread)).await;
                let interrupted = result.as_ref().err().is_some_and(Self::deletion_interrupted);
                let ok = result.is_ok();
                let valid = this.update(cx, |state, cx| {
                    let valid = destination.is_current(&state.fleet) && owner.response_after_deletion(state) && owner.can_operate_owner(state)
                        && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
                    if let Some(reply) = reply
                        && let Some(route) = state.finish_deletion(reply, ok && valid, cx) {
                        match route { Some(next) => state.select_thread(&next, cx), None => state.clear_selection(cx) }
                    }
                    if ok && valid {
                        state.sidebar.undo.expire_owned(owner, true);
                        if let Some(key) = owner.key() { state.sidebar.fleet_selection.remove(&key); }
                        cx.emit(AppEvent::Notice);
                        cx.emit(AppEvent::SidebarSelection);
                    }
                    valid
                }).unwrap_or(false);
                if ok { completed += 1; } else { failures += 1; }
                if interrupted || !valid {
                    interruption = Some(if interrupted { "connection closed or interrupted" } else { "destination or permission changed" }.to_owned()); break;
                }
            }
            let _ = this.update(cx, |state, cx| {
                for owner in &owners {
                    if state.scoped_deletion_missing(owner) && owner.can_operate_owner(state)
                        && let Some(key) = owner.key() {
                        state.sidebar.fleet_selection.remove(&key);
                    }
                }
                if failures > 0 || interruption.is_some() {
                    state.show_toast(Toast::new(ToastKind::Error, "Thread deletion incomplete").description(format!("{completed} thread(s) deleted, {failures} failed{}", interruption.map(|reason| format!("; stopped: {reason}")).unwrap_or_default())), cx);
                }
                cx.emit(AppEvent::SidebarSelection);
            });
        }).detach();
    }

    pub(crate) fn run_scoped_command(&mut self, owner: crate::context_menu::ThreadMenuOwner, command: Value, scope: &'static str, failure: &'static str, cx: &mut Context<Self>) {
        let Some(destination) = owner.destination.clone() else { return; };
        if !owner.current(self) || !self.scoped_ready(&destination, scope) { return; }
        let kind = command.get("type").and_then(Value::as_str).unwrap_or_default();
        let removes = matches!(kind, "thread.archive" | "thread.settle" | "thread.unpin" | "thread.delete");
        let stop_first = kind == "thread.delete" && owner.key().and_then(|key| self.scoped_thread(&key)).is_some_and(|row| row.thread.runtime.is_some());
        let undo = self.scoped_undo_claim(&owner, kind);
        let resolve = self.connections.destination(&destination, scope);
        cx.spawn(async move |this, cx| {
            let Ok(target) = resolve.await else { return; };
            let valid = this.read_with(cx, |state, _| target.is_current(&state.fleet) && owner.current(state) && state.scoped_ready(&destination, scope)
                && target.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(scope))).unwrap_or(false);
            if !valid { return; }
            if stop_first {
                if let Err(error) = target.client.dispatch(t3_core::commands::stop_thread(&uuid(), &owner.thread)).await {
                    let _ = this.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, failure).description(error.to_string()), cx)); return;
                }
                if !this.read_with(cx, |state, _| target.is_current(&state.fleet) && owner.current(state) && owner.can_operate_owner(state)
                    && target.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(scope))).unwrap_or(false) { return; }
            }
            let result = target.client.dispatch(command).await;
            let _ = this.update(cx, |state, cx| {
                if !(if removes { owner.response_after_placement(state) } else { owner.response_current(state) }) || !state.scoped_ready(&destination, scope) { return; }
                if let Err(error) = result {
                    if let Some((claim, _, _)) = &undo { state.sidebar.undo.finish(claim); }
                    state.show_toast(Toast::new(ToastKind::Error, failure).description(error.to_string()), cx);
                } else if let Some((claim, action, plan)) = undo {
                    state.sidebar.undo.show(action, claim, plan, now_ms());
                    state.after_undo_change(cx);
                    cx.emit(AppEvent::Notice);
                }
            });
        }).detach();
    }

    /// Applies the server config's keybindings and capabilities (`subscribeServerConfig`).
    pub(crate) fn sync_server_config(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.server_config.config.as_ref() else { return };
        self.sidebar.capabilities.auto_settle_opt_out = config.environment.capabilities.thread_auto_settle_opt_out == Some(true);
        crate::theme::set_environment_themes(&config.environment_theme_definitions, cx);
        let keybindings = config.keybindings.clone();
        crate::keymap::install(&keybindings, cx);
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Shell);
    }

    pub fn toggle_shelf(&mut self, shelf: Shelf, cx: &mut Context<Self>) {
        self.sidebar.shelves.toggle(shelf);
        if shelf == Shelf::Settled && !self.sidebar.shelves.settled_expanded {
            self.suspend_settled_pages();
        }
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Shell);
    }

    pub fn show_more_settled(&mut self, cx: &mut Context<Self>) {
        self.retry_settled_pages(cx);
        self.sidebar.shelves.show_more_settled();
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Shell);
    }

    fn hiding_thread_tags(&self) -> bool {
        self.sidebar.hiding_tags
    }

    pub(crate) fn thread_route_visible(&self, id: &str) -> bool {
        t3_core::sidebar::tag_filter::route_visible(id, self.thread_shell(id).is_some(), &self.sidebar.hidden_thread_ids, self.hiding_thread_tags())
    }

    pub fn can_navigate(&self, back: bool) -> bool {
        let (shell, mounts) = (&self.shell, &self.omitted.mounts);
        self.sidebar.history.can_go(back, |entry| history_entry_exists(shell, mounts, &self.sidebar.hidden_thread_ids, self.hiding_thread_tags(), entry))
    }

    /// `navigation.back` / `navigation.forward`.
    pub fn navigate_history(&mut self, back: bool, cx: &mut Context<Self>) {
        let (shell, mounts) = (&self.shell, &self.omitted.mounts);
        let hiding = self.hiding_thread_tags();
        let hidden = &self.sidebar.hidden_thread_ids;
        match self.sidebar.history.go(back, |entry| history_entry_exists(shell, mounts, hidden, hiding, entry)).cloned() {
            Some(NavEntry::Thread(id)) => self.select_thread(&id, cx),
            Some(NavEntry::Draft(project_id)) => self.start_draft(&project_id, cx),
            None => {}
        }
    }

    pub fn set_project_scope(&mut self, scope: t3_core::sidebar::grouping::ProjectScope, cx: &mut Context<Self>) {
        // A selection made under one scope can hold threads the next scope hides.
        self.sidebar.selection.clear();
        self.sidebar.project_filter_scope = scope;
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Shell);
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar.collapsed = !self.sidebar.collapsed;
        cx.emit(AppEvent::Selection);
    }

    pub(crate) fn visit_environment(&mut self) -> Option<OpenEnvironment> {
        let saved = self.environments.active();
        let canonical = self
            .server_config
            .config
            .as_ref()
            .map(|config| config.environment.environment_id.as_str())
            .filter(|id| !id.is_empty())
            .or_else(|| saved.and_then(|saved| saved.environment_id.as_deref()));
        self.open_history
            .environment(saved.map(|saved| saved.id.as_str()), canonical)
    }

    pub(crate) fn track_visit(&mut self, cx: &mut Context<Self>) {
        let id = self.selected_thread.clone();
        let now = now_ms();
        let environment = self.visit_environment();
        let shell = id.as_deref().and_then(|id| {
            let thread = self.thread_shell(id)?;
            Some(VisitedShell {
                updated_at: t3_core::sidebar::time::parse_iso(&thread.updated_at)?,
                last_visited_at: thread
                    .last_visited_at
                    .as_deref()
                    .and_then(t3_core::sidebar::time::parse_iso),
                completed_at: self
                    .sidebar
                    .model
                    .thread(id)
                    .and_then(|t| t.latest_run.as_ref())
                    .and_then(|run| run.completed_at),
                woke_at: self
                    .sidebar
                    .model
                    .thread(id)
                    .and_then(|t| settled::woke_at(t, now)),
            })
        });
        let connection = self.connection_id();
        let decision = self.sidebar.visit_decision(
            &mut self.open_history,
            environment
                .zip(id.as_deref())
                .map(|(environment, id)| (environment, connection, id)),
            shell,
            now,
        );
        let Some(id) = id else { return };
        match decision {
            VisitDecision::Skip => {}
            VisitDecision::Now { visited_at, opened } => {
                self.send_visit(&id, visited_at, opened, cx)
            }
            VisitDecision::At { due, visited_at } => {
                let Some(timer) = self.sidebar.pending_visit(visited_at) else {
                    return;
                };
                self.sidebar.visit_timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis((due - now).max(0) as u64))
                        .await;
                    let _ = this.update(cx, |state, cx| {
                        let environment = state.visit_environment();
                        let connection = state.connection_id();
                        if let VisitDecision::Now { visited_at, opened } = state.sidebar.fire_visit(
                            &timer,
                            environment.as_ref(),
                            connection,
                            state.selected_thread.as_deref(),
                            now_ms(),
                        ) {
                            state.send_visit(&id, visited_at, opened, cx);
                        }
                    });
                }));
            }
        }
    }

    fn send_visit(&mut self, thread_id: &str, visited_at: Ms, opened: bool, cx: &mut Context<Self>) {
        let mut command = actions::visit(&uuid(), thread_id, visited_at);
        if opened {
            command["opened"] = Value::Bool(true);
        }
        self.dispatch_then(command, None, cx, |_, _, _| {});
    }

    /// Dispatches a command; on failure shows `failure_title` (when given) as an error toast.
    pub(crate) fn dispatch_then(
        &mut self,
        command: Value,
        failure_title: Option<&'static str>,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut AppState, bool, &mut Context<Self>) + 'static,
    ) {
        let removes_row = matches!(command.get("type").and_then(Value::as_str), Some("thread.archive" | "thread.settle" | "thread.unpin"));
        let guarded = matches!(command.get("type").and_then(Value::as_str), Some("thread.settle" | "thread.unsettle" | "thread.pin" | "thread.unpin" | "thread.archive" | "thread.metadata.update" | "thread.auto-settle.set"));
        let owner = guarded.then(|| command.get("threadId").and_then(Value::as_str).and_then(|id| crate::context_menu::ThreadMenuOwner::capture(self, id))).flatten();
        let Some(client) = self.client_handle() else { return };
        let task = self.runtime().spawn(client.dispatch(command));
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or(Err(RpcError::Closed));
            let _ = this.update(cx, |state, cx| {
                if owner.as_ref().is_some_and(|owner| !(if removes_row { owner.response_after_placement(state) } else { owner.response_current(state) }) || !crate::context_menu::ThreadMenuOwner::can_operate(state)) { return; }
                if let (Err(error), Some(title)) = (&result, failure_title) {
                    state.show_toast(Toast::new(ToastKind::Error, title).description(error.to_string()), cx);
                }
                then(state, result.is_ok(), cx);
            });
        })
        .detach();
    }

    pub(crate) fn after_undo_change(&mut self, cx: &mut Context<Self>) {
        self.announce_undo_notice(cx);
        cx.emit(AppEvent::Notice);
        let Some(at) = self.sidebar.undo.expires_at() else {
            self.sidebar.undo_timer = None;
            return;
        };
        let wait = (at - now_ms()).max(0) as u64;
        self.sidebar.undo_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(wait)).await;
            let _ = this.update(cx, |state, cx| {
                if state.sidebar.undo.expire(now_ms()) {
                    state.announce_undo_notice(cx);
                    cx.emit(AppEvent::Notice);
                }
            });
        }));
    }

    fn snapshot_thread(&self, thread_id: &str) -> Option<t3_core::sidebar::thread::SidebarThread> {
        self.sidebar.model.thread(thread_id).cloned()
    }

    pub fn settle_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.snapshot_thread(thread_id) else { return };
        let now = now_ms();
        let woke = settled::woke_at(&thread, now);
        let plan = actions::plan_undo(PlanFor::Settle, thread.pinned, thread.pin_order_key.as_deref(), thread.snoozed_until);
        self.sidebar.undo.invalidate(UndoKind::Pin, thread_id);
        self.sidebar.undo.invalidate(UndoKind::Snooze, thread_id);
        let claim = self.sidebar.undo.begin(UndoKind::Settle, thread_id);
        let id = thread_id.to_owned();
        self.dispatch_then(actions::settle(&uuid(), thread_id), Some("Failed to settle thread"), cx, move |state, ok, cx| {
            if !ok {
                state.sidebar.undo.finish(&claim);
                return;
            }
            if let Some(woke) = woke.filter(|_| state.thread_shell(&id).is_some()) {
                state.send_visit(&id, woke, false, cx);
            }
            state.sidebar.undo.show(UndoAction::Settled, claim, plan, now_ms());
            state.after_undo_change(cx);
        });
    }

    pub fn unsettle_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.sidebar.undo.invalidate(UndoKind::Settle, thread_id);
        self.dispatch_then(actions::unsettle(&uuid(), thread_id), Some("Failed to un-settle thread"), cx, |_, _, _| {});
    }

    pub fn pin_thread(&mut self, thread_id: &str, order_key: Option<String>, cx: &mut Context<Self>) {
        let command = self.pin_command(thread_id, order_key);
        self.dispatch_then(command, Some("Failed to pin thread"), cx, |_, _, _| {});
    }

    /// `pinThread`: tops the pinned run by default, and supersedes older pin, settle and snooze
    /// undos, since pinning clears snooze and un-settles.
    pub(crate) fn pin_command(&mut self, thread_id: &str, order_key: Option<String>) -> Value {
        let order_key = order_key.or_else(|| top_of_pinned_order_key(self.sidebar.model.threads.iter()));
        for kind in [UndoKind::Pin, UndoKind::Settle, UndoKind::Snooze] {
            self.sidebar.undo.invalidate(kind, thread_id);
        }
        actions::pin(&uuid(), thread_id, order_key.as_deref())
    }

    pub fn unpin_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.snapshot_thread(thread_id) else { return };
        let plan = actions::plan_undo(PlanFor::Unpin, thread.pinned, thread.pin_order_key.as_deref(), thread.snoozed_until);
        let claim = self.sidebar.undo.begin(UndoKind::Pin, thread_id);
        self.dispatch_then(actions::unpin(&uuid(), thread_id), Some("Failed to unpin thread"), cx, move |state, ok, cx| {
            if ok && state.sidebar.undo.is_current(&claim) {
                state.sidebar.undo.show(UndoAction::Unpinned, claim, plan, now_ms());
                state.after_undo_change(cx);
            } else {
                state.sidebar.undo.finish(&claim);
            }
        });
    }

    pub fn snooze_thread(&mut self, thread_id: &str, snoozed_until: Ms, cx: &mut Context<Self>) {
        let Some(thread) = self.snapshot_thread(thread_id) else { return };
        if !settled::can_snooze(&thread, now_ms()) {
            self.show_toast(
                Toast::new(ToastKind::Error, "Failed to snooze thread")
                    .description("This thread is waiting on you. Respond to the pending request before snoozing it."),
                cx,
            );
            return;
        }
        let claim = self.sidebar.undo.begin(UndoKind::Snooze, thread_id);
        self.dispatch_then(actions::snooze(&uuid(), thread_id, snoozed_until), Some("Failed to snooze thread"), cx, move |state, ok, cx| {
            if !ok {
                state.sidebar.undo.finish(&claim);
                return;
            }
            state.sidebar.undo.show(UndoAction::Snoozed, claim, actions::plan_undo(PlanFor::Snooze, false, None, None), now_ms());
            state.after_undo_change(cx);
        });
    }

    pub fn unsnooze_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.sidebar.undo.invalidate(UndoKind::Snooze, thread_id);
        self.dispatch_then(actions::unsnooze(&uuid(), thread_id), Some("Failed to wake thread"), cx, |_, _, _| {});
    }

    pub fn archive_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.snapshot_thread(thread_id) else { return };
        if !thread.can_archive() {
            self.show_toast(Toast::new(ToastKind::Error, "Failed to archive thread").description("Stop the thread before archiving it."), cx);
            return;
        }
        let claim = self.sidebar.undo.begin(UndoKind::Archive, thread_id);
        let id = thread_id.to_owned();
        self.dispatch_then(actions::archive(&uuid(), thread_id), Some("Failed to archive thread"), cx, move |state, ok, cx| {
            if !ok {
                state.sidebar.undo.finish(&claim);
                return;
            }
            state.sidebar.undo.expire_placement(&id, false);
            state.sidebar.undo.show(UndoAction::Archived, claim, vec![UndoStep::Unarchive], now_ms());
            state.after_undo_change(cx);
            if state.selected_thread.as_deref() == Some(id.as_str()) {
                state.select_adjacent_after_removal(&id, cx);
            }
        });
    }

    pub(super) fn delete_target(&self) -> DeleteTarget {
        DeleteTarget {
            environment: self.environments.active.clone(),
            canonical: self
                .server_config
                .config
                .as_ref()
                .map(|config| config.environment.environment_id.clone()),
            connection: self.connection_id(),
            selected: self.selected_thread.clone(),
            visit: self.sidebar.visit_target.clone(),
        }
    }

    fn deletion_fallback(
        &self,
        thread_id: &str,
        doomed: &std::collections::HashSet<String>,
        order: t3_core::sidebar::sort::ThreadSortOrder,
        now: Ms,
    ) -> Option<String> {
        let snapshot = self.shell.snapshot.as_ref()?;
        let extras = self.omitted.mounts.extras(&self.shell);
        let requests = t3_core::sidebar::thread::pending_subagent_requests(&snapshot.threads);
        let threads: Vec<_> = snapshot
            .threads
            .iter()
            .chain(extras.iter().copied())
            .filter(|thread| thread.archived_at.is_none() && thread.deleted_at.is_none())
            .map(|thread| {
                t3_core::sidebar::thread::present_thread(thread, requests.get(&thread.id).copied())
            })
            .collect();
        t3_core::sidebar::fallback_after_delete(&threads, thread_id, doomed, order, now)
            .map(str::to_owned)
    }

    fn deletion_route(
        &self,
        target: &DeleteTarget,
        id: &str,
        project: &str,
        fallback: Option<String>,
        ok: bool,
    ) -> Option<Option<String>> {
        let fallback = fallback.filter(|next| {
            self.thread_shell(next).is_some_and(|thread| {
                thread.project_id == project
                    && thread.deleted_at.is_none()
                    && thread.archived_at.is_none()
            })
        });
        target.route(&self.delete_target(), id, ok, fallback)
    }

    /// Deletes after the caller confirmed. Stops a live runtime first, as the web does.
    pub fn delete_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let target = self.delete_target();
        self.delete_thread_then(
            thread_id,
            Deletion {
                target,
                doomed: Arc::default(),
            },
            "Failed to delete thread",
            cx,
            |_, _, _| {},
        );
    }

    pub(super) fn delete_thread_then(
        &mut self,
        thread_id: &str,
        deletion: Deletion,
        failure_title: &'static str,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut AppState, bool, &mut Context<Self>) + 'static,
    ) {
        if !deletion.target.same_scope(&self.delete_target()) {
            return;
        }
        let Some(thread) = self.thread_shell(thread_id) else {
            return then(self, false, cx);
        };
        let thread = t3_core::sidebar::thread::present_thread(thread, None);
        let id = thread_id.to_owned();
        if thread.runtime.is_some() {
            self.dispatch_then(
                t3_core::commands::stop_thread(&uuid(), thread_id),
                None,
                cx,
                move |state, _, cx| {
                    if deletion.target.same_scope(&state.delete_target()) {
                        state.delete_stopped_thread(id, deletion, failure_title, cx, then);
                    }
                },
            );
        } else {
            self.delete_stopped_thread(id, deletion, failure_title, cx, then);
        }
    }

    pub(crate) fn hold_pending_thread_deletion(&self) -> bool {
        connected(&self.phase)
            && self
                .sidebar
                .pending_deletion
                .as_ref()
                .is_some_and(|pending| pending.target == self.delete_target())
    }

    fn release_stale_deletion(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.pending_deletion.is_none() || self.hold_pending_thread_deletion() {
            return;
        }
        let pending = self.sidebar.pending_deletion.take().unwrap();
        if self.thread.deleted && pending.target == self.delete_target() {
            self.close_selected_thread(cx);
        }
    }

    fn prepare_deletion(
        &mut self,
        id: String,
        deletion: &Deletion,
        order: t3_core::sidebar::sort::ThreadSortOrder,
        now: Ms,
    ) -> Option<DeletionReply> {
        let project = self.thread_shell(&id)?.project_id.clone();
        let eligible = connected(&self.phase)
            && deletion
                .target
                .route(&self.delete_target(), &id, true, None)
                .is_some();
        let fallback = if eligible {
            self.deletion_fallback(&id, &deletion.doomed, order, now)
        } else {
            None
        };
        let owner = eligible.then(|| {
            self.sidebar.deletion_revision += 1;
            let token = self.sidebar.deletion_revision;
            self.sidebar.pending_deletion = Some(PendingDeletion {
                token,
                target: deletion.target.clone(),
            });
            token
        });
        Some(DeletionReply {
            owner,
            target: deletion.target.clone(),
            id,
            project,
            fallback,
        })
    }

    fn finish_deletion(
        &mut self,
        reply: DeletionReply,
        ok: bool,
        cx: &mut Context<Self>,
    ) -> Option<Option<String>> {
        let current = self.delete_target();
        let same_scope = reply.target.same_scope(&current);
        let owned = reply.owner.is_some_and(|token| {
            self.sidebar
                .pending_deletion
                .as_ref()
                .is_some_and(|pending| pending.token == token && pending.target == reply.target)
        });
        if owned {
            self.sidebar.pending_deletion = None;
        }
        if ok && same_scope {
            self.sidebar.undo.expire_placement(&reply.id, true);
            cx.emit(AppEvent::Notice);
        }
        if !ok || !connected(&self.phase) {
            if owned && reply.target == current && self.thread.deleted {
                self.close_selected_thread(cx);
            }
            return None;
        }
        if !same_scope {
            return None;
        }
        owned
            .then(|| {
                self.deletion_route(&reply.target, &reply.id, &reply.project, reply.fallback, ok)
            })
            .flatten()
    }

    fn delete_stopped_thread(
        &mut self,
        id: String,
        deletion: Deletion,
        failure_title: &'static str,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut AppState, bool, &mut Context<Self>) + 'static,
    ) {
        if self.client_handle().is_none() {
            return then(self, false, cx);
        }
        let Some(reply) = self.prepare_deletion(
            id.clone(),
            &deletion,
            crate::settings::store::client(cx).thread_sort_order(),
            now_ms(),
        ) else {
            return then(self, false, cx);
        };
        self.dispatch_then(
            actions::delete(&uuid(), &id),
            Some(failure_title),
            cx,
            move |state, ok, cx| {
                let same_scope = reply.target.same_scope(&state.delete_target());
                let route = state.finish_deletion(reply, ok, cx);
                if !same_scope {
                    return;
                }
                if let Some(route) = route {
                    match route {
                        Some(next) => state.select_thread(&next, cx),
                        None => state.clear_selection(cx),
                    }
                }
                then(state, ok, cx);
            },
        );
    }

    fn select_adjacent_after_removal(&mut self, removed: &str, cx: &mut Context<Self>) {
        let next = self.sidebar.model.adjacent_thread(Some(removed), true).or_else(|| self.sidebar.model.adjacent_thread(Some(removed), false)).map(str::to_owned);
        match next {
            Some(next) => self.select_thread(&next, cx),
            None => self.clear_selection(cx),
        }
    }

    pub fn mark_thread_unread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.dispatch_then(actions::mark_unread(&uuid(), thread_id), None, cx, |_, _, _| {});
    }

    pub fn acknowledge_woke(&mut self, thread_id: &str, woke_at: Ms, cx: &mut Context<Self>) {
        self.send_visit(thread_id, woke_at, false, cx);
    }

    pub fn rename_thread(&mut self, thread_id: &str, title: &str, cx: &mut Context<Self>) {
        let Some(original) = self.snapshot_thread(thread_id).map(|thread| thread.title) else { return };
        match actions::resolve_rename(title, &original) {
            RenameOutcome::Empty => self.show_toast(Toast::new(ToastKind::Warning, "Thread title cannot be empty"), cx),
            RenameOutcome::Unchanged => {}
            RenameOutcome::Rename(title) => {
                self.dispatch_then(actions::rename(&uuid(), thread_id, &title), Some("Failed to rename thread"), cx, |_, _, _| {});
            }
        }
    }

    pub fn snooze_presets(&self) -> Vec<SnoozePreset> {
        settled::resolve_snooze_presets(&self.snooze_settings(), now_ms(), &SystemZone, &time_label)
    }

    /// The environment's saved snooze options, or the defaults before its settings load (`Sidebar.snooze.ts`).
    pub(crate) fn snooze_settings(&self) -> Vec<SnoozePresetSetting> {
        self.server_config.config.as_ref().map_or_else(|| DEFAULT_SNOOZE_PRESETS.to_vec(), |config| menu_settings(&config.settings.threads.snooze_presets))
    }

    pub(crate) fn thread_copy_scope(&self) -> Option<(String, Option<String>, u64)> {
        if !connected(&self.phase) || !self.shell.synchronized {
            return None;
        }
        let environment = &self
            .server_config
            .config
            .as_ref()?
            .environment
            .environment_id;
        if environment.is_empty() || environment.trim() != environment {
            return None;
        }
        if self
            .environments
            .active()
            .and_then(|saved| saved.environment_id.as_deref())
            .is_some_and(|saved| saved != environment)
        {
            return None;
        }
        Some((
            environment.clone(),
            self.environments.active.clone(),
            self.connection_id()?,
        ))
    }

    /// Runs one context-menu or palette action for a thread.
    pub fn run_thread_action(&mut self, thread_id: &str, action: ThreadAction, shown_presets: &[SnoozePreset], cx: &mut Context<Self>) {
        let Some(thread) = self.snapshot_thread(thread_id) else { return };
        match action {
            ThreadAction::NewThreadOnBranch => self.start_branch_draft(thread_id, cx),
            ThreadAction::TurnIntoProject => { if let Some(task) = self.turn_into_project(thread_id, cx) { task.detach(); } },
            ThreadAction::Settle => self.settle_thread(thread_id, cx),
            ThreadAction::Unsettle => self.unsettle_thread(thread_id, cx),
            ThreadAction::Pin => self.pin_thread(thread_id, None, cx),
            ThreadAction::Unpin => self.request_unpin_thread(thread_id, cx),
            ThreadAction::Unsnooze => self.unsnooze_thread(thread_id, cx),
            ThreadAction::SnoozePreset(id) => {
                if let Some(preset) = settled::pick_snooze_preset(shown_presets, &id, now_ms(), &self.snooze_settings(), &SystemZone, &time_label) {
                    self.snooze_thread(thread_id, preset.snoozed_until, cx);
                }
            }
            ThreadAction::AutoSettleEnabled | ThreadAction::AutoSettleDisabled => {
                let enabled = action == ThreadAction::AutoSettleEnabled;
                self.dispatch_then(actions::set_auto_settle(&uuid(), thread_id, enabled), Some("Failed to update auto-settle"), cx, |_, _, _| {});
            }
            ThreadAction::RegenerateTitle if !thread.regenerating_title => {
                self.dispatch_then(actions::regenerate_title(&uuid(), thread_id), Some("Failed to regenerate thread title"), cx, |_, _, _| {});
            }
            ThreadAction::MarkUnread => self.mark_thread_unread(thread_id, cx),
            ThreadAction::Archive => self.request_archive_thread(thread_id, cx),
            ThreadAction::CopyPath => {
                let path = thread.worktree_path.clone().or_else(|| self.shell.project(&thread.project_id).map(|project| project.workspace_root.clone()));
                match path {
                    Some(path) => self.copy(path.clone(), "Path copied", Some(path), cx),
                    None => self.show_toast(Toast::new(ToastKind::Error, "Path unavailable").description("This thread does not have a workspace path to copy."), cx),
                }
            }
            ThreadAction::CopyBranch => {
                if let Some(branch) = thread.branch.clone() {
                    self.copy(branch.clone(), "Branch copied", Some(branch), cx);
                }
            }
            ThreadAction::CopyThread => {
                let Some((environment, _, _)) = self.thread_copy_scope() else {
                    return;
                };
                let Some(reference) =
                    actions::thread_reference(&thread.title, Some(&environment), &thread.id)
                else {
                    return;
                };
                crate::composer::clipboard::write(reference.text, reference.fragment, cx);
                self.show_toast(
                    Toast::new(ToastKind::Success, "Thread copied").description(reference.description),
                    cx,
                );
            }
            ThreadAction::CopyThreadId => self.copy(thread.id.clone(), "Thread ID copied", Some(thread.id.clone()), cx),
            ThreadAction::Find => {
                self.select_thread(thread_id, cx);
                cx.dispatch_action(&crate::thread_view::FindInThread);
            }
            _ => {}
        }
    }

    fn copy(&mut self, text: String, title: &'static str, description: Option<String>, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let toast = Toast::new(ToastKind::Success, title);
        self.show_toast(if let Some(description) = description { toast.description(description) } else { toast }, cx);
    }

    /// `undoLatestThreadAction`: restores the latest group of same-kind actions.
    pub fn undo_latest(&mut self, cx: &mut Context<Self>) -> bool {
        let group = self.sidebar.undo.take_latest_group();
        if group.is_empty() { return false; }
        let mut threads = Vec::new();
        for (context, plan) in group {
            match context {
                UndoContext::Thread(owner) => threads.push((*owner,plan)),
                UndoContext::Draft { environment,canonical,connection,generation,project } => {
                    if self.environments.active == environment && self.connection_id() == connection && self.shell.generation == generation
                        && self.server_config.config.as_ref().map(|config| config.environment.environment_id.clone()) == canonical
                        && self.shell.project(&project.id).is_some_and(|current| current.created_at == project.created_at && current.workspace_root == project.workspace_root)
                        && plan.contains(&UndoStep::RestoreDraft) {
                        self.sidebar.drafts_to_restore.push(project.id); cx.emit(AppEvent::RestoreDraft);
                    }
                }
                UndoContext::Unavailable { .. } => {}
            }
        }
        if !threads.is_empty() { self.run_owned_undo(threads,cx); }
        self.after_undo_change(cx); true
    }

    fn run_owned_undo(&mut self, group: Vec<(crate::context_menu::ThreadMenuOwner,Vec<UndoStep>)>, cx: &mut Context<Self>) {
        let clients = group.iter().map(|(owner,_)| {
            let scoped = owner.destination.as_ref().map(|destination| self.connections.destination(destination,t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
            let local = owner.destination.is_none().then(|| self.client_handle()).flatten();
            async move { match scoped { Some(resolve) => resolve.await.ok().map(|target| target.client), None => local } }
        }).collect::<Vec<_>>();
        cx.spawn(async move |this,cx| {
            let mut targets = Vec::new();
            for resolve in clients { let Some(target) = resolve.await else { return }; targets.push(target); }
            let valid = |state:&AppState, owner:&crate::context_menu::ThreadMenuOwner, client:&t3_client::supervisor::ClientHandle| owner.response_after_placement(state) && owner.can_operate_owner(state)
                && client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))
                && owner.destination.as_ref().is_none_or(|destination| *client.connection_generation().borrow() == destination.connection_generation);
            if !this.read_with(cx,|state,_| group.iter().zip(&targets).all(|((owner,_),client)| valid(state,owner,client))).unwrap_or(false) { return; }
            for ((owner,steps),client) in group.into_iter().zip(targets) {
                for step in steps.into_iter().filter(|step| actions::runnable_step(step,now_ms())) {
                    if !this.read_with(cx,|state,_| valid(state,&owner,&client)).unwrap_or(false) { return; }
                    let command = match step {
                        UndoStep::Unsettle => actions::unsettle(&uuid(),&owner.thread), UndoStep::Unsnooze => actions::unsnooze(&uuid(),&owner.thread),
                        UndoStep::Pin { order_key } => actions::pin(&uuid(),&owner.thread,order_key.as_deref()),
                        UndoStep::Snooze { snoozed_until } => actions::snooze(&uuid(),&owner.thread,snoozed_until),
                        UndoStep::Unarchive => actions::unarchive(&uuid(),&owner.thread), UndoStep::RestoreDraft => continue,
                    };
                    if let Err(error) = client.dispatch(command).await {
                        let _ = this.update(cx,|state,cx| state.show_toast(Toast::new(ToastKind::Error,"Could not undo thread action").description(error.to_string()),cx)); return;
                    }
                }
            }
        }).detach();
    }


}

pub fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod visit_tests {
    use super::*;

    #[test]
    fn selected_mount_claims_before_shell_and_dedupes_reopens() {
        let mut sidebar = SidebarController::default();
        let mut history = OpenHistory::default();
        let env = history.environment(Some("saved-a"), None);
        assert_eq!(
            sidebar.visit_decision(
                &mut history,
                env.clone().map(|env| (env, Some(1), "a")),
                None,
                1_000
            ),
            VisitDecision::Skip
        );
        assert!(
            sidebar.visit.is_some(),
            "the mount must claim before shell arrival"
        );
        let shell = VisitedShell {
            updated_at: 100,
            last_visited_at: Some(100),
            completed_at: None,
            woke_at: None,
        };
        assert_eq!(
            sidebar.visit_decision(
                &mut history,
                env.clone().map(|env| (env, Some(1), "a")),
                Some(shell),
                2_000
            ),
            VisitDecision::Now {
                visited_at: 100,
                opened: true
            }
        );
        sidebar.visit_decision(&mut history, None, None, 2_001);
        assert_eq!(
            sidebar.visit_decision(
                &mut history,
                env.clone().map(|env| (env, Some(1), "a")),
                Some(shell),
                3_000
            ),
            VisitDecision::Skip,
            "deduped reopen preserves a read watermark"
        );
    }

    #[test]
    fn read_thread_reopen_does_not_send_another_open() {
        let mut sidebar = SidebarController::default();
        let mut history = OpenHistory::default();
        let env = history.environment(Some("saved-a"), None);
        let shell = VisitedShell {
            updated_at: 100,
            last_visited_at: Some(100),
            completed_at: None,
            woke_at: None,
        };
        sidebar.visit_decision(
            &mut history,
            env.clone().map(|env| (env, Some(1), "a")),
            Some(shell),
            1_000,
        );
        sidebar.visit_decision(
            &mut history,
            env.clone().map(|env| (env, Some(1), "b")),
            Some(shell),
            2_000,
        );
        assert_eq!(
            sidebar.visit_decision(
                &mut history,
                env.clone().map(|env| (env, Some(1), "a")),
                Some(shell),
                3_000
            ),
            VisitDecision::Skip
        );
        let unread = VisitedShell {
            last_visited_at: Some(80),
            completed_at: Some(90),
            ..shell
        };
        for next in [
            unread,
            VisitedShell {
                updated_at: 50,
                last_visited_at: Some(50),
                ..shell
            },
            unread,
        ] {
            assert_eq!(
                sidebar.visit_decision(
                    &mut history,
                    env.clone().map(|env| (env, Some(1), "a")),
                    Some(next),
                    3_001
                ),
                VisitDecision::Skip
            );
        }
    }
    #[test]
    fn environment_identity_reconnect_and_client_clock_boundary_share_history() {
        let mut sidebar = SidebarController::default();
        let mut history = OpenHistory::default();
        let shell = VisitedShell {
            updated_at: 100,
            last_visited_at: Some(100),
            completed_at: None,
            woke_at: None,
        };
        let mut plan = |sidebar: &mut SidebarController, saved, canonical, connection, now| {
            let env = history.environment(saved, canonical);
            let decision = sidebar.visit_decision(
                &mut history,
                env.clone().map(|env| (env, Some(connection), "a")),
                Some(shell),
                now,
            );
            (
                decision,
                env.as_ref()
                    .and_then(|env| history.local_opened_at(env, "a")),
            )
        };
        assert_eq!(
            plan(&mut sidebar, None, None, 1, 0),
            (VisitDecision::Skip, None)
        );
        assert_eq!(
            plan(&mut sidebar, Some("saved-a"), None, 1, 1_000).0,
            VisitDecision::Now {
                visited_at: 100,
                opened: true
            }
        );
        assert_eq!(
            plan(&mut sidebar, Some("saved-a"), Some("canonical-a"), 1, 2_000).1,
            Some(1_000),
            "binding config is not a new mount"
        );
        assert_eq!(
            plan(&mut sidebar, Some("saved-b"), None, 2, 3_000).0,
            VisitDecision::Now {
                visited_at: 100,
                opened: true
            }
        );
        sidebar = SidebarController::default();
        assert_eq!(
            plan(&mut sidebar, Some("saved-a"), Some("canonical-a"), 3, 4_000),
            (VisitDecision::Skip, Some(4_000))
        );
        sidebar = SidebarController::default();
        assert_eq!(
            plan(
                &mut sidebar,
                Some("saved-a"),
                Some("canonical-a"),
                4,
                600_999
            )
            .0,
            VisitDecision::Skip
        );
        sidebar = SidebarController::default();
        assert_eq!(
            plan(
                &mut sidebar,
                Some("saved-a"),
                Some("canonical-a"),
                5,
                601_000
            )
            .0,
            VisitDecision::Now {
                visited_at: 100,
                opened: true
            }
        );
        assert_eq!(
            plan(
                &mut sidebar,
                Some("saved-a"),
                Some("replacement"),
                5,
                601_001
            )
            .0,
            VisitDecision::Now {
                visited_at: 100,
                opened: true
            }
        );
        assert_eq!(
            plan(&mut sidebar, None, Some("replacement"), 6, 601_002).0,
            VisitDecision::Now {
                visited_at: 100,
                opened: true
            },
            "canonical-only keys have their own prefix"
        );
    }

    #[test]
    fn stale_timers_cannot_authorize_a_returned_target_or_clear_a_new_timer() {
        let mut sidebar = SidebarController::default();
        let mut history = OpenHistory::default();
        let env = history.environment(Some("saved-a"), None).unwrap();
        let shell = VisitedShell {
            updated_at: 100,
            last_visited_at: Some(0),
            completed_at: None,
            woke_at: None,
        };
        sidebar.visit_decision(
            &mut history,
            Some((env.clone(), Some(1), "a")),
            Some(shell),
            1_000,
        );
        let updated = VisitedShell {
            updated_at: 200,
            ..shell
        };
        assert!(matches!(
            sidebar.visit_decision(
                &mut history,
                Some((env.clone(), Some(1), "a")),
                Some(updated),
                1_001
            ),
            VisitDecision::At { .. }
        ));
        let stale = sidebar.pending_visit(200).unwrap();
        for next in [None, Some("b"), Some("a")] {
            sidebar.visit_decision(
                &mut history,
                next.map(|id| (env.clone(), Some(1), id)),
                Some(shell),
                1_002,
            );
        }
        sidebar.visit_decision(
            &mut history,
            Some((env.clone(), Some(1), "a")),
            Some(updated),
            1_003,
        );
        let current = sidebar.pending_visit(200).unwrap();
        sidebar.visit_timer = Some(Task::ready(()));
        assert_eq!(
            sidebar.fire_visit(&stale, Some(&env), Some(1), Some("a"), 11_000),
            VisitDecision::Skip
        );
        assert!(sidebar.visit_timer.is_some());
        assert_eq!(
            sidebar.fire_visit(&current, Some(&env), Some(2), Some("a"), 11_000),
            VisitDecision::Skip
        );
        assert_eq!(
            sidebar.fire_visit(&current, Some(&env), Some(1), Some("b"), 11_000),
            VisitDecision::Skip
        );
        assert_eq!(
            sidebar.fire_visit(&current, None, Some(1), Some("a"), 11_000),
            VisitDecision::Skip
        );
        for shell in [
            None,
            Some(VisitedShell {
                last_visited_at: Some(200),
                ..updated
            }),
        ] {
            sidebar.visit_decision(
                &mut history,
                Some((env.clone(), Some(1), "a")),
                shell,
                1_004,
            );
            assert!(sidebar.visit_timer.is_none());
            assert_eq!(
                sidebar.fire_visit(&current, Some(&env), Some(1), Some("a"), 11_000),
                VisitDecision::Skip
            );
        }
        sidebar.visit_decision(
            &mut history,
            Some((env.clone(), Some(1), "a")),
            Some(VisitedShell {
                updated_at: 300,
                ..shell
            }),
            1_005,
        );
        let current = sidebar.pending_visit(300).unwrap();
        assert_eq!(
            sidebar.fire_visit(&current, Some(&env), Some(1), Some("a"), 11_000),
            VisitDecision::Now {
                visited_at: 300,
                opened: false
            }
        );
        assert_eq!(
            sidebar.fire_visit(&current, Some(&env), Some(1), Some("a"), 11_001),
            VisitDecision::Skip
        );
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    fn app_owned_history_survives_reset_and_prefers_installed_identity(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::AppContext;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let fixture: crate::chatgpt_setup::Fixture = serde_json::from_value(serde_json::json!({
                "provider": {"instanceId":"replay", "driver":"replay", "enabled":true, "installed":true, "status":"ready"},
                "auth":{"phase":"idle"}, "install":{"phase":"idle"}
            })).unwrap();
            cx.set_global(fixture);
        });
        let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
        state.update(cx, |state, cx| {
            state.environments = serde_json::from_value(serde_json::json!({"active":"saved", "environments":[{
                "id":"saved", "label":"Test", "httpBase":"http://127.0.0.1:14658", "environmentId":"stale-metadata"
            }]})).unwrap();
            let environment = state.visit_environment().unwrap();
            state.selected_thread = Some("a".into());
            state.track_visit(cx);
            assert!(state.open_history.local_opened_at(&environment, "a").is_some());
            assert!(state.sidebar.visit.is_some(), "missing shell still mounts the tracker");
            let config = state.server_config.clone();
            state.clear_connection_state(cx);
            state.server_config = config;
            let restored = state.visit_environment().unwrap();
            assert_eq!(environment, restored);
            assert!(state.open_history.local_opened_at(&restored, "a").is_some());
            let replaced = state.open_history.environment(Some("saved"), Some("replacement")).unwrap();
            assert_eq!(state.open_history.local_opened_at(&replaced, "a"), None);
            // Installed config beats stale saved metadata, including after a reset.
            assert_ne!(state.visit_environment().unwrap(), replaced);
        });
    }
}

#[cfg(test)]
mod deletion_tests {
    use super::*;

    #[test]
    fn deletion_reply_routes_only_the_original_open_mount_after_success() {
        let target = DeleteTarget {
            environment: Some("saved".into()),
            canonical: Some("environment".into()),
            connection: Some(1),
            selected: Some("deleted".into()),
            visit: None,
        };
        assert_eq!(
            target.route(&target, "deleted", true, Some("survivor".into())),
            Some(Some("survivor".into()))
        );
        assert_eq!(target.route(&target, "deleted", true, None), Some(None));
        assert_eq!(
            target.route(&target, "deleted", false, Some("survivor".into())),
            None
        );
        assert_eq!(
            target.route(&target, "background", true, Some("survivor".into())),
            None
        );
        for current in [
            DeleteTarget {
                selected: None,
                ..target.clone()
            },
            DeleteTarget {
                selected: Some("new-selection".into()),
                ..target.clone()
            },
            DeleteTarget {
                environment: Some("different".into()),
                ..target.clone()
            },
            DeleteTarget {
                canonical: Some("different".into()),
                ..target.clone()
            },
            DeleteTarget {
                connection: Some(2),
                ..target.clone()
            },
            DeleteTarget {
                connection: None,
                ..target.clone()
            },
        ] {
            assert_eq!(
                target.route(&current, "deleted", true, Some("survivor".into())),
                None
            );
        }
        let environment = OpenHistory::default()
            .environment(Some("saved"), None)
            .unwrap();
        let mounted = DeleteTarget {
            visit: Some(VisitTarget {
                environment,
                thread: "deleted".into(),
                connection: Some(1),
                mount: 1,
            }),
            ..target
        };
        let mut returned = mounted.clone();
        returned.visit.as_mut().unwrap().mount = 2;
        assert_eq!(
            mounted.route(&returned, "deleted", true, Some("survivor".into())),
            None
        );
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn scoped_selected_delete_holds_event_until_reply_and_uses_physical_fallback(cx: &mut gpui::TestAppContext) {
        use t3_client::supervisor::{ClientEvent, ClientFixture};
        use t3_core::sidebar::tag_filter::ThreadKey;

        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut old_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (client, mut fixture) = ClientFixture::handle(serde_json::from_value(serde_json::json!({
            "authenticated":true,"permissions":["orchestration:read","orchestration:operate"]
        })).unwrap());
        let (mut shell, config, connection) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone(), state.connection_id()));
        let make = |id, project, at| serde_json::from_value(serde_json::json!({
            "id":id,"projectId":project,"title":id,"createdAt":at,"updatedAt":at,
            "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},
            "runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":id},
            "status":"idle","hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0
        })).unwrap();
        shell.snapshot.as_mut().unwrap().threads = vec![
            make("deleted","checkout-a","2026-04-01T00:00:00Z"),
            make("survivor","checkout-a","2026-04-02T00:00:00Z"),
            make("logical-sibling","checkout-b","2026-04-03T00:00:00Z"),
        ];
        let deleted = serde_json::to_value(&shell.snapshot.as_ref().unwrap().threads[0]).unwrap();
        let (ready, loaded) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.scoped_environment("saved-fixture").is_some() {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        let install = state.update(cx, |state, _| state.connections.install_fixture("saved-fixture", client, shell, config, ConnectionStatus::Connected));
        let events = runtime.spawn(install).await.unwrap(); loaded.await.unwrap();
        let (ready, activated) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _activated = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.connection_id() != connection && state.thread_shell("deleted").is_some() {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        state.read_with(cx, |state, _| state.connections.activate(Some("saved-fixture")));
        activated.await.unwrap();
        state.update(cx, |state, cx| state.select_thread("deleted", cx));
        let (method, command, visit) = fixture.next_request().await;
        assert_eq!(method,t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(command["type"],"thread.visit");
        visit.send(Ok(serde_json::json!({"sequence":1}))).unwrap();
        let key = ThreadKey::new("saved-fixture","deleted");
        state.update(cx, |state, cx| state.run_scoped_thread_action(&key,ThreadAction::Delete,&[],cx));
        let (method, command, reply) = fixture.next_request().await;
        assert_eq!(method,t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(command["type"],"thread.delete");
        assert_eq!(command["threadId"],"deleted");
        let (sent, applied) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _applied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.thread_shell("deleted").is_none() {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        let mut payload = deleted;
        payload["deletedAt"] = serde_json::json!("2026-04-04T00:00:00Z");
        events.send(ClientEvent::Thread { thread_id:"deleted".into(), item:Box::new(serde_json::from_value(serde_json::json!({
            "kind":"event","sequence":2,"event":{"id":"deleted-event","threadId":"deleted","occurredAt":"2026-04-04T00:00:00Z","type":"thread.deleted","payload":payload}
        })).unwrap()) }).await.unwrap();
        events.send(ClientEvent::Shell(ShellStreamItem::ThreadRemoved { sequence:2, thread_id:"deleted".into() })).await.unwrap();
        applied.await.unwrap();
        state.read_with(cx, |state, _| {
            assert_eq!(state.selected_thread.as_deref(),Some("deleted"),"scoped selected deletion holds its original mount until the successful reply");
            assert!(state.hold_pending_thread_deletion());
        });
        let (sent, routed) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _routed = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Selection && state.selected_thread.as_deref() == Some("survivor") {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        reply.send(Ok(serde_json::json!({"sequence":2}))).unwrap();
        routed.await.unwrap();
        state.read_with(cx, |state, _| {
            assert_eq!(state.environments.active.as_deref(),Some("saved-fixture"));
            assert_eq!(state.selected_thread.as_deref(),Some("survivor"),"fallback stays in the physical checkout, not its newer logical sibling");
            assert!(!state.hold_pending_thread_deletion());
        });
        assert!(!old_fixture.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND));
        state.update(cx, |state, _| state.stop_pump());
        drop(events);
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn scoped_selected_delete_failure_and_remounted_reply_preserve_navigation_ownership(cx: &mut gpui::TestAppContext) {
        use t3_client::supervisor::{ClientEvent, ClientFixture};
        use t3_core::sidebar::tag_filter::ThreadKey;

        cx.executor().allow_parking();
        for mode in ["failure", "remount", "reply-first"] {
        let failed_reply = mode == "failure";
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut old_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (client, mut fixture) = ClientFixture::handle(serde_json::from_value(serde_json::json!({
            "authenticated":true,"permissions":["orchestration:read","orchestration:operate"]
        })).unwrap());
        let (mut shell, config, connection) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone(), state.connection_id()));
        let make = |id, project, at| serde_json::from_value(serde_json::json!({
            "id":id,"projectId":project,"title":id,"createdAt":at,"updatedAt":at,
            "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},
            "runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":id},
            "status":"idle","hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0
        })).unwrap();
        shell.snapshot.as_mut().unwrap().threads = vec![
            make("deleted","checkout-a","2026-04-01T00:00:00Z"),
            make("survivor","checkout-a","2026-04-02T00:00:00Z"),
            make("logical-sibling","checkout-b","2026-04-03T00:00:00Z"),
        ];
        let deleted = serde_json::to_value(&shell.snapshot.as_ref().unwrap().threads[0]).unwrap();
        let (ready, loaded) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.scoped_environment("saved-fixture").is_some() {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        let install = state.update(cx, |state, _| state.connections.install_fixture("saved-fixture", client, shell, config, ConnectionStatus::Connected));
        let events = runtime.spawn(install).await.unwrap(); loaded.await.unwrap();
        let (ready, activated) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _activated = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.connection_id() != connection && state.thread_shell("deleted").is_some() {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        state.read_with(cx, |state, _| state.connections.activate(Some("saved-fixture")));
        activated.await.unwrap();
        state.update(cx, |state, cx| state.select_thread("deleted", cx));
        let (method, command, visit) = fixture.next_request().await;
        assert_eq!(method,t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(command["type"],"thread.visit");
        visit.send(Ok(serde_json::json!({"sequence":1}))).unwrap();
        let key = ThreadKey::new("saved-fixture","deleted");
        state.update(cx, |state, cx| state.run_scoped_thread_action(&key,ThreadAction::Delete,&[],cx));
        let (method, command, reply) = fixture.next_request().await;
        assert_eq!(method,t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(command["type"],"thread.delete");
        assert_eq!(command["threadId"],"deleted");
        if failed_reply {
            let (sent, applied) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
            let _applied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                if *event == AppEvent::Shell && state.thread_shell("deleted").is_none() {
                    if let Some(sent) = sent.take() { let _ = sent.send(()); }
                }
            }));
            let mut payload = deleted.clone();
            payload["deletedAt"] = serde_json::json!("2026-04-04T00:00:00Z");
            events.send(ClientEvent::Thread { thread_id:"deleted".into(), item:Box::new(serde_json::from_value(serde_json::json!({
                "kind":"event","sequence":2,"event":{"id":"deleted-event","threadId":"deleted","occurredAt":"2026-04-04T00:00:00Z","type":"thread.deleted","payload":payload}
            })).unwrap()) }).await.unwrap();
            events.send(ClientEvent::Shell(ShellStreamItem::ThreadRemoved { sequence:2, thread_id:"deleted".into() })).await.unwrap();
            applied.await.unwrap();
            state.read_with(cx, |state, _| {
                assert_eq!(state.selected_thread.as_deref(),Some("deleted"));
                assert!(state.hold_pending_thread_deletion());
            });
        } else if mode == "remount" {
            let mount = state.read_with(cx, |state, _| state.sidebar.visit_target.clone());
            state.update(cx, |state, cx| {
                state.select_thread("survivor",cx);
                state.select_thread("deleted",cx);
            });
            state.read_with(cx, |state, _| {
                assert_ne!(state.sidebar.visit_target,mount,"away and back creates a new selected mount");
                assert_eq!(state.selected_thread.as_deref(),Some("deleted"));
            });
            fixture.take_commands();
        }
        let (sent, finished) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _finished = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            let finished = if failed_reply {
                *event == AppEvent::Notice && state.toasts.latest().is_some_and(|toast| toast.title == "Thread deletion incomplete")
            } else { *event == AppEvent::SidebarSelection && !state.hold_pending_thread_deletion() };
            if finished { if let Some(sent) = sent.take() { let _ = sent.send(()); } }
        }));
        reply.send(if failed_reply {
            Err(RpcError::Failure(t3_core::rpc::RpcFailure { tag:Some("OperationError".into()),message:"cannot delete".into(),error:serde_json::json!({"_tag":"OperationError"}) }))
        } else { Ok(serde_json::json!({"sequence":2})) }).unwrap();
        finished.await.unwrap();
        state.read_with(cx, |state, _| {
            assert_eq!(state.environments.active.as_deref(),Some("saved-fixture"));
            assert_eq!(state.selected_thread.as_deref(),if failed_reply { None } else if mode == "remount" { Some("deleted") } else { Some("survivor") },"a failed reply never selects fallback; an old successful reply never navigates the new mount");
            assert!(!state.hold_pending_thread_deletion());
        });
        if mode == "reply-first" {
            let (sent, applied) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
            let _applied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                if *event == AppEvent::Shell && state.thread_shell("deleted").is_none() {
                    if let Some(sent) = sent.take() { let _ = sent.send(()); }
                }
            }));
            let mut payload = deleted.clone();
            payload["deletedAt"] = serde_json::json!("2026-04-04T00:00:00Z");
            events.send(ClientEvent::Thread { thread_id:"deleted".into(), item:Box::new(serde_json::from_value(serde_json::json!({
                "kind":"event","sequence":2,"event":{"id":"late-deleted-event","threadId":"deleted","occurredAt":"2026-04-04T00:00:00Z","type":"thread.deleted","payload":payload}
            })).unwrap()) }).await.unwrap();
            events.send(ClientEvent::Shell(ShellStreamItem::ThreadRemoved { sequence:2, thread_id:"deleted".into() })).await.unwrap();
            applied.await.unwrap();
            state.read_with(cx, |state, _| {
                assert_eq!(state.selected_thread.as_deref(),Some("survivor"),"late deleted stream cannot clear the fallback selected after an owned success");
                assert!(!state.hold_pending_thread_deletion());
            });
        }
        assert!(!old_fixture.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND));
        state.update(cx, |state, _| state.stop_pump());
        drop(events);
        }
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn scoped_selected_bulk_delete_excludes_full_doomed_set_and_retains_later_failure(cx: &mut gpui::TestAppContext) {
        use t3_client::supervisor::{ClientEvent, ClientFixture};
        use t3_core::sidebar::tag_filter::ThreadKey;

        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut old_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (client, mut fixture) = ClientFixture::handle(serde_json::from_value(serde_json::json!({
            "authenticated":true,"permissions":["orchestration:read","orchestration:operate"]
        })).unwrap());
        let (mut shell, config, connection) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone(), state.connection_id()));
        let make = |id, project, at| serde_json::from_value(serde_json::json!({
            "id":id,"projectId":project,"title":id,"createdAt":at,"updatedAt":at,
            "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},
            "runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":id},
            "status":"idle","hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0
        })).unwrap();
        shell.snapshot.as_mut().unwrap().threads = vec![
            make("deleted","checkout-a","2026-04-01T00:00:00Z"),
            make("also-doomed","checkout-a","2026-04-02T00:00:00Z"),
            make("survivor","checkout-a","2026-03-31T00:00:00Z"),
            make("logical-sibling","checkout-b","2026-04-03T00:00:00Z"),
        ];
        let deleted = serde_json::to_value(&shell.snapshot.as_ref().unwrap().threads[0]).unwrap();
        let (ready, loaded) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.scoped_environment("saved-fixture").is_some() {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        let install = state.update(cx, |state, _| state.connections.install_fixture("saved-fixture", client, shell, config, ConnectionStatus::Connected));
        let events = runtime.spawn(install).await.unwrap(); loaded.await.unwrap();
        let (ready, activated) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _activated = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.connection_id() != connection && state.thread_shell("deleted").is_some() {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        state.read_with(cx, |state, _| state.connections.activate(Some("saved-fixture")));
        activated.await.unwrap();
        state.update(cx, |state, cx| state.select_thread("deleted", cx));
        let (method, command, visit) = fixture.next_request().await;
        assert_eq!(method,t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(command["type"],"thread.visit");
        visit.send(Ok(serde_json::json!({"sequence":1}))).unwrap();
        let key = ThreadKey::new("saved-fixture","deleted");
        let later = ThreadKey::new("saved-fixture","also-doomed");
        let owners = state.read_with(cx, |state, _| [&key, &later].into_iter().map(|key| crate::context_menu::ThreadMenuOwner::capture_scoped(state, key).unwrap()).collect());
        state.update(cx, |state, cx| {
            state.sidebar.fleet_selection.extend([key.clone(), later.clone()]);
            state.run_scoped_bulk_action(owners,t3_core::sidebar::selection::BulkAction::Delete,cx);
        });
        let (method, command, reply) = fixture.next_request().await;
        assert_eq!(method,t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(command["type"],"thread.delete");
        assert_eq!(command["threadId"],"deleted");
        let (sent, applied) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _applied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.thread_shell("deleted").is_none() {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        let mut payload = deleted;
        payload["deletedAt"] = serde_json::json!("2026-04-04T00:00:00Z");
        events.send(ClientEvent::Thread { thread_id:"deleted".into(), item:Box::new(serde_json::from_value(serde_json::json!({
            "kind":"event","sequence":2,"event":{"id":"deleted-event","threadId":"deleted","occurredAt":"2026-04-04T00:00:00Z","type":"thread.deleted","payload":payload}
        })).unwrap()) }).await.unwrap();
        events.send(ClientEvent::Shell(ShellStreamItem::ThreadRemoved { sequence:2, thread_id:"deleted".into() })).await.unwrap();
        applied.await.unwrap();
        state.read_with(cx, |state, _| {
            assert_eq!(state.selected_thread.as_deref(),Some("deleted"),"scoped selected deletion holds its original mount until the successful reply");
            assert!(state.hold_pending_thread_deletion());
        });
        let (sent, routed) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _routed = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Selection && state.selected_thread.as_deref() == Some("survivor") {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        reply.send(Ok(serde_json::json!({"sequence":2}))).unwrap();
        routed.await.unwrap();
        state.read_with(cx, |state, _| {
            assert_eq!(state.environments.active.as_deref(),Some("saved-fixture"));
            assert_eq!(state.selected_thread.as_deref(),Some("survivor"),"fallback stays in the physical checkout, not its newer logical sibling");
            assert!(!state.hold_pending_thread_deletion());
        });
        let (method, command, visit) = fixture.next_request().await;
        assert_eq!(method,t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(command["type"],"thread.visit");
        visit.send(Ok(serde_json::json!({"sequence":2}))).unwrap();
        let (method, command, reply) = fixture.next_request().await;
        assert_eq!(method,t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(command["type"],"thread.delete");
        assert_eq!(command["threadId"],"also-doomed");
        let (sent, finished) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _finished = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Notice && state.toasts.latest().is_some_and(|toast| toast.title == "Thread deletion incomplete") {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        reply.send(Err(RpcError::Failure(t3_core::rpc::RpcFailure {
            tag:Some("OperationError".into()), message:"cannot delete".into(), error:serde_json::json!({"_tag":"OperationError","message":"cannot delete"})
        }))).unwrap();
        finished.await.unwrap();
        state.read_with(cx, |state, _| {
            assert_eq!(state.selected_thread.as_deref(),Some("survivor"));
            assert!(!state.sidebar.fleet_selection.contains(&key));
            assert!(state.sidebar.fleet_selection.contains(&later));
            assert!(state.toasts.latest().unwrap().description.as_ref().unwrap().contains("1 thread(s) deleted, 1 failed"));
            assert!(!state.hold_pending_thread_deletion());
        });
        assert!(!old_fixture.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND));
        state.update(cx, |state, _| state.stop_pump());
        drop(events);
    }

    #[cfg(feature = "snapshot")]
    async fn scoped_delete_fixture(cx: &mut gpui::TestAppContext, runtime: &tokio::runtime::Runtime) -> (gpui::Entity<AppState>, t3_client::supervisor::ClientFixture, tokio::sync::mpsc::Sender<t3_client::supervisor::ClientEvent>) {
        let (state, _) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (client, fixture) = t3_client::supervisor::ClientFixture::handle(serde_json::from_value(serde_json::json!({
            "authenticated":true,"permissions":["orchestration:read","orchestration:operate"]
        })).unwrap());
        let (shell, config) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone()));
        let (ready, loaded) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.scoped_environment("bulk-fixture").is_some() {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        let install = state.update(cx, |state, _| state.connections.install_fixture("bulk-fixture", client, shell, config, ConnectionStatus::Connected));
        let events = runtime.spawn(install).await.unwrap(); loaded.await.unwrap();
        (state, fixture, events)
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn scoped_bulk_delete_continues_failures_but_stops_effect_interrupt(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        for outcome in ["failure", "interrupt", "closed"] {
            let interrupted = outcome != "failure";
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let (state, mut fixture, events) = scoped_delete_fixture(cx, &runtime).await;
            let keys = ["menu-thread", "sibling"].map(|id| t3_core::sidebar::tag_filter::ThreadKey::new("bulk-fixture", id));
            let owners = state.read_with(cx, |state, _| keys.iter().map(|key| crate::context_menu::ThreadMenuOwner::capture_scoped(state, key).unwrap()).collect());
            state.update(cx, |state, _| state.sidebar.fleet_selection.extend(keys.clone()));
            let (sent, mut finished) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
            let _finished = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                if *event == AppEvent::Notice && state.toasts.latest().is_some_and(|toast| toast.title == "Thread deletion incomplete") {
                    if let Some(sent) = sent.take() { let _ = sent.send(()); }
                }
            }));
            state.update(cx, |state, cx| state.run_scoped_bulk_action(owners, t3_core::sidebar::selection::BulkAction::Delete, cx));
            let (_, command, reply) = fixture.next_request().await;
            assert_eq!(command["threadId"], "menu-thread");
            let encoded = if interrupted {
                serde_json::json!({"_tag":"Failure","cause":[{"_tag":"Interrupt"}]})
            } else {
                serde_json::json!({"_tag":"Failure","cause":[{"_tag":"Fail","error":{"_tag":"OperationError","message":"cannot delete"}}]})
            };
            let failure = serde_json::from_value::<t3_core::rpc::ExitEncoded>(encoded).unwrap().into_result().unwrap_err();
            reply.send(Err(if outcome == "closed" { RpcError::Closed } else { RpcError::Failure(failure) })).unwrap();
            let next = tokio::select! {
                request = fixture.next_request() => Some(request),
                _ = &mut finished => None,
            };
            assert_eq!(next.is_none(), interrupted, "an Effect interrupt must stop before a later write, unlike an ordinary operation failure");
            if let Some((_, command, reply)) = next {
                assert_eq!(command["threadId"], "sibling");
                let fresh = t3_core::sidebar::tag_filter::ThreadKey::new("bulk-fixture", "other-thread");
                state.update(cx, |state, _| { state.sidebar.fleet_selection.insert(fresh.clone()); });
                reply.send(Ok(serde_json::json!({"sequence":2}))).unwrap();
                finished.await.unwrap();
                state.read_with(cx, |state, _| {
                    assert!(state.sidebar.fleet_selection.contains(&keys[0]));
                    assert!(!state.sidebar.fleet_selection.contains(&keys[1]));
                    assert!(state.sidebar.fleet_selection.contains(&fresh));
                    assert!(state.toasts.latest().unwrap().description.as_ref().unwrap().contains("1 thread(s) deleted, 1 failed"));
                });
            } else {
                state.read_with(cx, |state, _| assert!(keys.iter().all(|key| state.sidebar.fleet_selection.contains(key))));
            }
            state.update(cx, |state, _| state.stop_pump());
            drop(events);
        }
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn scoped_bulk_delete_rejects_held_reply_after_grant_target_or_owner_drift(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        for drift in ["grant", "target", "owner"] {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let (state, mut fixture, events) = scoped_delete_fixture(cx, &runtime).await;
            let keys = ["menu-thread", "sibling"].map(|id| t3_core::sidebar::tag_filter::ThreadKey::new("bulk-fixture", id));
            let owners: Vec<_> = state.read_with(cx, |state, _| keys.iter().map(|key| crate::context_menu::ThreadMenuOwner::capture_scoped(state, key).unwrap()).collect());
            let original = owners[0].destination.clone().unwrap();
            let route = state.read_with(cx, |state, _| (state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()));
            state.update(cx, |state, _| state.sidebar.fleet_selection.extend(keys.clone()));
            let (sent, finished) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
            let _finished = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                if *event == AppEvent::Notice && state.toasts.latest().is_some_and(|toast| toast.title == "Thread deletion incomplete") {
                    if let Some(sent) = sent.take() { let _ = sent.send(()); }
                }
            }));
            state.update(cx, |state, cx| state.run_scoped_bulk_action(owners,t3_core::sidebar::selection::BulkAction::Delete,cx));
            let (_, command, reply) = fixture.next_request().await;
            assert_eq!(command["type"], "thread.delete");
            assert_eq!(command["threadId"], "menu-thread");
            let (sent, changed) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
            let observed = original.clone();
            let _changed = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                let changed = match drift {
                    "grant" => !state.scoped_ready(&observed,t3_core::auth::ORCHESTRATION_OPERATE_SCOPE),
                    "target" => state.scoped_thread(&t3_core::sidebar::tag_filter::ThreadKey::new("bulk-fixture","menu-thread")).is_some_and(|row| row.created_at == "2026-06-01T00:00:00Z"),
                    _ => state.scoped_environment("bulk-fixture").is_some_and(|entry| entry.summary.owner != observed),
                };
                if *event == AppEvent::Shell && changed {
                    if let Some(sent) = sent.take() { let _ = sent.send(()); }
                }
            }));
            let mut replacement_fixture = None;
            let mut replacement_events = None;
            match drift {
                "grant" => { fixture.grants.send_replace(Some(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read"]})).unwrap())); }
                "target" => {
                    let mut thread = state.read_with(cx, |state, _| state.shell.thread("menu-thread").unwrap().clone());
                    thread.created_at = "2026-06-01T00:00:00Z".into();
                    events.send(t3_client::supervisor::ClientEvent::Shell(ShellStreamItem::ThreadUpdated { sequence:2, location:"active".into(), thread:Box::new(thread) })).await.unwrap();
                }
                _ => {
                    let (client, fixture) = t3_client::supervisor::ClientFixture::handle(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read","orchestration:operate"]})).unwrap());
                    let (shell, config) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone()));
                    let install = state.update(cx, |state, _| state.connections.install_fixture("bulk-fixture",client,shell,config,ConnectionStatus::Connected));
                    replacement_events = Some(runtime.spawn(install).await.unwrap());
                    replacement_fixture = Some(fixture);
                }
            }
            changed.await.unwrap();
            reply.send(Ok(serde_json::json!({"sequence":3}))).unwrap();
            finished.await.unwrap();
            state.read_with(cx, |state, _| {
                assert_eq!((state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()),route);
                assert!(keys.iter().all(|key| state.sidebar.fleet_selection.contains(key)),"stale success cannot deselect a replacement or unattempted original");
                assert!(state.toasts.latest().unwrap().description.as_ref().unwrap().contains("1 thread(s) deleted"));
            });
            assert!(!fixture.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND));
            if let Some(fixture) = replacement_fixture.as_mut() { assert!(!fixture.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND)); }
            state.update(cx, |state, _| state.stop_pump());
            drop(replacement_events);
            drop(events);
        }
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn scoped_bulk_delete_keeps_same_raw_id_results_local_to_each_owner(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut first, events) = scoped_delete_fixture(cx, &runtime).await;
        let (client, mut second) = t3_client::supervisor::ClientFixture::handle(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read","orchestration:operate"]})).unwrap());
        let (shell, mut config) = state.read_with(cx, |state, _| (state.shell.clone(),state.server_config.clone()));
        config.config.as_mut().unwrap().environment.environment_id = "second-env".into();
        let (sent, loaded) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.scoped_environment("second-fixture").is_some() {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        let install = state.update(cx, |state, _| state.connections.install_fixture("second-fixture",client,shell,config,ConnectionStatus::Connected));
        let second_events = runtime.spawn(install).await.unwrap(); loaded.await.unwrap();
        let keys = ["bulk-fixture","second-fixture"].map(|environment| t3_core::sidebar::tag_filter::ThreadKey::new(environment,"menu-thread"));
        let owners = state.read_with(cx, |state, _| keys.iter().map(|key| crate::context_menu::ThreadMenuOwner::capture_scoped(state,key).unwrap()).collect());
        let route = state.read_with(cx, |state, _| (state.environments.active.clone(),state.selected_thread.clone(),state.draft_project.clone()));
        state.update(cx, |state, _| state.sidebar.fleet_selection.extend(keys.clone()));
        let (sent, finished) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _finished = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Notice && state.toasts.latest().is_some_and(|toast| toast.title == "Thread deletion incomplete") {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        state.update(cx, |state, cx| state.run_scoped_bulk_action(owners,t3_core::sidebar::selection::BulkAction::Delete,cx));
        let (_, command, reply) = first.next_request().await;
        assert_eq!(command["threadId"],"menu-thread");
        reply.send(Err(RpcError::Failure(t3_core::rpc::RpcFailure { tag:Some("OperationError".into()),message:"cannot delete".into(),error:serde_json::json!({"_tag":"OperationError"}) }))).unwrap();
        let (_, command, reply) = second.next_request().await;
        assert_eq!(command["type"],"thread.delete");
        assert_eq!(command["threadId"],"menu-thread");
        reply.send(Ok(serde_json::json!({"sequence":2}))).unwrap();
        finished.await.unwrap();
        state.read_with(cx, |state, _| {
            assert!(state.sidebar.fleet_selection.contains(&keys[0]));
            assert!(!state.sidebar.fleet_selection.contains(&keys[1]));
            assert_eq!((state.environments.active.clone(),state.selected_thread.clone(),state.draft_project.clone()),route);
            assert!(state.toasts.latest().unwrap().description.as_ref().unwrap().contains("1 thread(s) deleted, 1 failed"));
        });
        assert!(!first.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND));
        assert!(!second.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND));
        state.update(cx, |state, _| state.stop_pump());
        drop(second_events);
        drop(events);
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn scoped_bulk_delete_checks_runtime_stop_failure_and_post_stop_grants(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        for revoke in [false,true] {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let (state, mut fixture, events) = scoped_delete_fixture(cx,&runtime).await;
            let keys = ["menu-thread","sibling"].map(|id| t3_core::sidebar::tag_filter::ThreadKey::new("bulk-fixture",id));
            let mut thread = state.read_with(cx, |state, _| state.shell.thread("menu-thread").unwrap().clone());
            thread.active_provider_thread_id = Some("running-provider".into());
            let (sent, running) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
            let _running = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                if *event == AppEvent::Shell && state.scoped_thread(&t3_core::sidebar::tag_filter::ThreadKey::new("bulk-fixture","menu-thread")).is_some_and(|row| row.thread.runtime.is_some()) {
                    if let Some(sent) = sent.take() { let _ = sent.send(()); }
                }
            }));
            events.send(t3_client::supervisor::ClientEvent::Shell(ShellStreamItem::ThreadUpdated { sequence:2,location:"active".into(),thread:Box::new(thread) })).await.unwrap(); running.await.unwrap();
            let owners: Vec<_> = state.read_with(cx, |state, _| keys.iter().map(|key| crate::context_menu::ThreadMenuOwner::capture_scoped(state,key).unwrap()).collect());
            let destination = owners[0].destination.clone().unwrap();
            state.update(cx, |state, _| state.sidebar.fleet_selection.extend(keys.clone()));
            let (sent, mut finished) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
            let _finished = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                if *event == AppEvent::Notice && state.toasts.latest().is_some_and(|toast| toast.title == "Thread deletion incomplete") {
                    if let Some(sent) = sent.take() { let _ = sent.send(()); }
                }
            }));
            state.update(cx, |state, cx| state.run_scoped_bulk_action(owners,t3_core::sidebar::selection::BulkAction::Delete,cx));
            let (_,command,stop) = fixture.next_request().await;
            assert_eq!(command["type"],t3_core::commands::stop_thread("id","menu-thread")["type"]);
            assert_eq!(command["threadId"],"menu-thread");
            if revoke {
                let (sent, denied) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
                let _denied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                    if *event == AppEvent::Shell && !state.scoped_ready(&destination,t3_core::auth::ORCHESTRATION_OPERATE_SCOPE) {
                        if let Some(sent) = sent.take() { let _ = sent.send(()); }
                    }
                }));
                fixture.grants.send_replace(Some(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read"]})).unwrap())); denied.await.unwrap();
                stop.send(Ok(serde_json::json!({"sequence":3}))).unwrap();
            } else {
                stop.send(Err(RpcError::Failure(t3_core::rpc::RpcFailure { tag:Some("OperationError".into()),message:"cannot stop".into(),error:serde_json::json!({"_tag":"OperationError"}) }))).unwrap();
            }
            let next = tokio::select! { request = fixture.next_request() => Some(request), _ = &mut finished => None };
            assert_eq!(next.is_none(),revoke,"a grant change during stop must prevent delete and later writes; ordinary stop failure continues");
            if let Some((_,command,reply)) = next {
                assert_eq!(command["type"],"thread.delete");
                assert_eq!(command["threadId"],"sibling");
                reply.send(Ok(serde_json::json!({"sequence":3}))).unwrap(); finished.await.unwrap();
            }
            state.read_with(cx, |state, _| {
                assert!(state.sidebar.fleet_selection.contains(&keys[0]));
                assert_eq!(state.sidebar.fleet_selection.contains(&keys[1]),revoke);
                assert_eq!(state.environments.active.as_deref(),Some("saved-fixture"));
            });
            assert!(!fixture.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND));
            state.update(cx, |state, _| state.stop_pump());
            drop(events);
        }
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn scoped_bulk_delete_skips_missing_original_before_valid_later_target(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture, events) = scoped_delete_fixture(cx, &runtime).await;
        let keys = ["menu-thread", "sibling"].map(|id| t3_core::sidebar::tag_filter::ThreadKey::new("bulk-fixture", id));
        let owners: Vec<_> = state.read_with(cx, |state, _| keys.iter().map(|key| crate::context_menu::ThreadMenuOwner::capture_scoped(state, key).unwrap()).collect());
        let destination = owners[0].destination.clone().unwrap();
        state.update(cx, |state, _| state.sidebar.fleet_selection.extend(keys.clone()));
        let (sent, removed) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _removed = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.scoped_thread(&t3_core::sidebar::tag_filter::ThreadKey::new("bulk-fixture", "menu-thread")).is_none() {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        events.send(t3_client::supervisor::ClientEvent::Shell(ShellStreamItem::ThreadRemoved { sequence:2, thread_id:"menu-thread".into() })).await.unwrap();
        removed.await.unwrap();
        state.update(cx, |state, cx| state.run_scoped_bulk_action(owners, t3_core::sidebar::selection::BulkAction::Delete, cx));
        cx.run_until_parked();
        let barrier = state.read_with(cx, |state, _| state.connections.destination(&destination, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
        runtime.spawn(barrier).await.unwrap().unwrap();
        cx.run_until_parked();
        use futures::FutureExt;
        let request = fixture.next_request().now_or_never();
        assert!(request.is_some(), "a missing original target must not abort deletion of a valid later target");
        let (_, command, reply) = request.unwrap();
        assert_eq!(command["threadId"], "sibling");
        let (sent, completed) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let _completed = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::SidebarSelection && state.sidebar.fleet_selection.is_empty() {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        reply.send(Ok(serde_json::json!({"sequence":3}))).unwrap(); completed.await.unwrap();
        state.read_with(cx, |state, _| assert_eq!(state.environments.active.as_deref(), Some("saved-fixture")));
        state.update(cx, |state, _| state.stop_pump());
        drop(events);
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn scoped_bulk_delete_entry_denial_reports_zero_saved(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture, events) = scoped_delete_fixture(cx, &runtime).await;
        let key = t3_core::sidebar::tag_filter::ThreadKey::new("bulk-fixture", "menu-thread");
        let owner = state.read_with(cx, |state, _| crate::context_menu::ThreadMenuOwner::capture_scoped(state, &key).unwrap());
        state.update(cx, |state, _| { state.sidebar.fleet_selection.insert(key.clone()); });
        fixture.grants.send_replace(Some(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read"]})).unwrap()));
        let destination = owner.destination.clone().unwrap();
        let barrier = state.read_with(cx, |state, _| state.connections.destination(&destination, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
        let _ = runtime.spawn(barrier).await.unwrap();
        cx.run_until_parked();
        state.update(cx, |state, cx| state.run_scoped_bulk_action(vec![owner], t3_core::sidebar::selection::BulkAction::Delete, cx));
        cx.run_until_parked();
        let barrier = state.read_with(cx, |state, _| state.connections.destination(&destination, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
        let _ = runtime.spawn(barrier).await.unwrap();
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            let toast = state.toasts.latest().expect("entry permission refusal must visibly report that no deletion was saved");
            assert_eq!(toast.kind, ToastKind::Error);
            assert!(toast.description.as_ref().unwrap().contains("0 thread(s) deleted"));
            assert!(state.sidebar.fleet_selection.contains(&key));
        });
        assert!(!fixture.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND));
        state.update(cx, |state, _| state.stop_pump());
        drop(events);
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    fn deletion_reads_environment_shells_and_mounts_before_sidebar_filters(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::AppContext;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let fixture: crate::chatgpt_setup::Fixture = serde_json::from_value(serde_json::json!({
                "provider":{"instanceId":"replay","driver":"replay","enabled":true,"installed":true,"status":"ready"},
                "auth":{"phase":"idle"},"install":{"phase":"idle"}
            })).unwrap();
            cx.set_global(fixture);
        });
        let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
        state.update(cx, |state, _| {
            let make = |id, project, at| serde_json::from_value(serde_json::json!({
                "id":id,"projectId":project,"title":id,"createdAt":at,"updatedAt":at,
                "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},
                "runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":id},
                "status":"idle","hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0
            })).unwrap();
            state.shell.snapshot = Some(t3_core::shell::ShellSnapshot {
                threads: vec![make("deleted", "p1", "2026-04-01T00:00:00Z"), make("filtered", "p1", "2026-04-02T00:00:00Z"), make("other-project", "p2", "2026-04-05T00:00:00Z")],
                ..Default::default()
            });
            state.omitted.mounts.mount("family", state.shell.generation, vec![t3_core::families::FamilyRead {
                snapshot_sequence: 1, threads: vec![make("omitted", "p1", "2026-04-03T00:00:00Z")],
            }]);
            state.selected_thread = Some("deleted".into());
            assert!(state.sidebar.model.threads.is_empty());
            let doomed = ["omitted".into()].into();
            assert_eq!(state.deletion_fallback("deleted", &Default::default(), t3_core::sidebar::sort::ThreadSortOrder::UpdatedAt, 0).as_deref(), Some("omitted"));
            assert_eq!(state.deletion_fallback("deleted", &doomed, t3_core::sidebar::sort::ThreadSortOrder::UpdatedAt, 0).as_deref(), Some("filtered"));
            let target = state.delete_target();
            assert_eq!(state.deletion_route(&target, "deleted", "p1", Some("filtered".into()), true), Some(Some("filtered".into())));
            state.shell.snapshot.as_mut().unwrap().threads.retain(|thread| thread.id != "filtered");
            assert_eq!(state.deletion_route(&target, "deleted", "p1", Some("filtered".into()), true), Some(None));
            state.shell.snapshot.as_mut().unwrap().threads.push(make("filtered", "p2", "2026-04-06T00:00:00Z"));
            assert_eq!(state.deletion_route(&target, "deleted", "p1", Some("filtered".into()), true), Some(None));

        });
        // Drive the real event boundary; finish_deletion is the production reply's pre-navigation plan.
        for mode in [
            "reply-first",
            "event-first",
            "failed-before-event",
            "failed-or-cancelled",
            "draft",
            "clear",
            "disconnect",
            "background-disconnect",
            "reset",
            "unowned",
        ] {
            let event_first = !matches!(
                mode,
                "reply-first" | "failed-before-event" | "background-disconnect"
            );
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            let Some((reply, claims, discard, other_claim, later_event)) = state.update(cx, |state, cx| {
                let make = |id, at| serde_json::from_value(serde_json::json!({
                    "id":id,"projectId":"p1","title":id,"createdAt":at,"updatedAt":at,
                    "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},
                    "runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":id},
                    "status":"idle","hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0
                })).unwrap();
                state.shell.snapshot = Some(t3_core::shell::ShellSnapshot {
                    projects: vec![serde_json::from_value(serde_json::json!({
                        "id":"p1","title":"Project","workspaceRoot":"/fixture","createdAt":"2026-04-01T00:00:00Z","updatedAt":"2026-04-01T00:00:00Z"
                    })).unwrap()],
                    threads: vec![make("deleted", "2026-04-01T00:00:00Z"), make("survivor", "2026-04-02T00:00:00Z"), make("other", "2026-03-01T00:00:00Z")],
                    ..Default::default()
                });
                state.start_draft("p1", cx);
                state.draft_project = None;
                state.selected_thread = Some("deleted".into());
                state.track_visit(cx);
                let mut payload = serde_json::to_value(state.thread_shell("deleted").unwrap()).unwrap();
                payload["deletedAt"] = serde_json::json!("2026-04-03T00:00:00Z");
                let event = || t3_client::supervisor::ClientEvent::Thread {
                    thread_id: "deleted".into(),
                    item: Box::new(serde_json::from_value(serde_json::json!({
                        "kind":"event","sequence":2,"event":{"id":"delete-event","threadId":"deleted","occurredAt":"2026-04-03T00:00:00Z","type":"thread.deleted","payload":payload}
                    })).unwrap()),
                };
                let deletion = Deletion { target: state.delete_target(), doomed: Arc::new(["deleted".into()].into()) };
                let background = state.prepare_deletion("survivor".into(), &deletion, t3_core::sidebar::sort::ThreadSortOrder::UpdatedAt, 0).unwrap();
                assert!(background.fallback.is_none(), "background deletion must not compute a discarded fallback");
                let reply = if mode == "background-disconnect" { Some(background) } else { (mode != "unowned").then(|| state.prepare_deletion("deleted".into(), &deletion, t3_core::sidebar::sort::ThreadSortOrder::UpdatedAt, 0).unwrap()) };
                if event_first { state.apply_batch(vec![event()], cx); }
                if mode == "unowned" {
                    assert_eq!(state.selected_thread, None);
                    assert_eq!(state.draft_project.as_deref(), Some("p1"));
                    assert!(!state.hold_pending_thread_deletion());
                    return None;
                }
                if event_first {
                    assert!(state.thread.deleted);
                    assert_eq!(state.selected_thread.as_deref(), Some("deleted"));
                    assert!(state.hold_pending_thread_deletion());
                }
                match mode {
                    "draft" => state.start_draft("p2", cx),
                    "clear" => state.clear_selection(cx),
                    "disconnect" | "background-disconnect" => state.apply_batch(vec![t3_client::supervisor::ClientEvent::Status(ConnectionStatus::Reconnecting { reason: "fixture disconnect".into(), attempt: 1 })], cx),
                    "reset" => state.clear_connection_state(cx),
                    _ => {}
                }
                let reply = reply.unwrap();
                let claims = [UndoKind::Settle, UndoKind::Snooze, UndoKind::Pin, UndoKind::Archive].map(|kind| state.sidebar.undo.begin(kind, &reply.id));
                state.sidebar.undo.show(UndoAction::Unpinned, claims[2].clone(), vec![UndoStep::Pin { order_key: None }], 0);
                let discard = state.sidebar.undo.begin(UndoKind::Discard, &reply.id);
                let other_claim = state.sidebar.undo.begin(UndoKind::Pin, "other");
                state.sidebar.undo.show(UndoAction::Unpinned, other_claim.clone(), vec![UndoStep::Pin { order_key: None }], 0);
                assert_eq!(state.sidebar.undo.notice().unwrap().count, 2);
                Some((reply, claims, discard, other_claim, event()))
            }) else { continue };
            let notices = std::rc::Rc::new(std::cell::Cell::new(0));
            let observed = notices.clone();
            let _notice_subscription = cx.update(|cx| {
                cx.subscribe(&state, move |_, event: &AppEvent, _| {
                    if matches!(event, AppEvent::Notice) {
                        observed.set(observed.get() + 1);
                    }
                })
            });
            let cleanup = !matches!(
                mode,
                "failed-or-cancelled" | "failed-before-event" | "reset"
            );
            state.update(cx, |state, cx| {
                let expected = matches!(mode, "reply-first" | "event-first")
                    .then_some(Some("survivor".into()));
                assert_eq!(
                    state.finish_deletion(
                        reply,
                        !matches!(mode, "failed-or-cancelled" | "failed-before-event"),
                        cx
                    ),
                    expected,
                    "mode={mode}, selected={:?}, draft={:?}",
                    state.selected_thread,
                    state.draft_project
                );
                assert!(!state.hold_pending_thread_deletion());
                assert!(state.sidebar.pending_deletion.is_none());
                for claim in &claims {
                    assert_eq!(
                        state.sidebar.undo.is_current(claim),
                        !cleanup,
                        "deleted placement/archive claim after {mode}"
                    );
                }
                assert!(
                    state.sidebar.undo.is_current(&discard),
                    "deletion leaves draft Undo alone"
                );
                assert!(
                    state.sidebar.undo.is_current(&other_claim),
                    "other thread Undo must survive {mode}"
                );
                assert_eq!(
                    state.sidebar.undo.notice().unwrap().count,
                    if cleanup { 1 } else { 2 }
                );
                if mode == "background-disconnect" {
                    assert_eq!(state.selected_thread.as_deref(), Some("deleted"));
                } else if !event_first {
                    // No navigation is executed in this command-plan test; the now-unowned event closes normally.
                    state.apply_batch(vec![later_event], cx);
                    assert_eq!(state.selected_thread, None);
                    assert_eq!(state.draft_project.as_deref(), Some("p1"));
                } else if !matches!(mode, "event-first") {
                    assert_eq!(
                        state.selected_thread, None,
                        "held deletion must not survive {mode}"
                    );
                    assert!(!state.thread.deleted);
                    if mode == "draft" {
                        assert_eq!(state.draft_project.as_deref(), Some("p2"));
                    }
                }
            });
            assert_eq!(
                notices.get(),
                usize::from(cleanup),
                "completion Notice for {mode}"
            );
        }
    }
}
