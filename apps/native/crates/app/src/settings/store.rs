//! Where settings edits go. Client settings live in `settings.json` under `T3_NATIVE_HOME` and
//! apply at once; the store's writer thread saves each edit as a patch over the file as it is
//! then, so an unreadable file is never replaced by defaults and a second instance's saves
//! survive. Server settings go through `server.updateSettings` one at a time per app, as the
//! web's serial `updateSettings` command does (`client-runtime/src/state/server.ts`); a reply
//! applies only if no newer settings streamed in while it was in flight (`useSettings.ts`).

use std::collections::VecDeque;

use gpui::{App, Entity, Global, SharedString};
use t3_client::store;
use t3_core::client_settings::{ClientSettingChange, ClientSettingsDoc};
use t3_core::commands::method;
use t3_core::settings::{ServerSettingChange, decode_update_reply};

use crate::app_state::{AppEvent, AppState};
use crate::terminal;
use crate::theme::{self, Preferences};

type OnFailed = Box<dyn FnOnce(&mut App)>;
/// Told when a save settles: `None` once saved, else why it was not.
type OnDone = Box<dyn FnOnce(Option<String>, &mut App)>;

enum SaveTarget {
    Active(ServerSettingChange),
    Projects { members: Vec<t3_core::sidebar::grouping::ScopedProject>, edit: t3_core::settings::projects::OverrideEdit },
}

struct PendingSave {
    state: Entity<AppState>,
    target: SaveTarget,
    on_done: Option<OnDone>,
}

struct ClientPrefs {
    doc: ClientSettingsDoc,
    versions: std::collections::BTreeMap<String,u64>,
    library_versions: t3_core::themes::transaction::LibraryVersions,
    /// The last server save that failed, shown above the settings page until the next edit.
    error: Option<SharedString>,
    saving: bool,
    queue: VecDeque<PendingSave>,
}

impl Global for ClientPrefs {}

pub fn init(cx: &mut App) {
    let mut doc = ClientSettingsDoc::from_json(&store::load_client_settings().unwrap_or_default());
    if !doc.entries().contains_key(t3_core::client_settings::typography::ADVANCED_KEY)
        && let Some(advanced) = store::load_ui_state("typography-advanced").and_then(|value| value.as_bool()) {
        store::patch_client_settings(doc.apply_change(&ClientSettingChange::SeparateFonts(advanced)));
    }
    if let Some(legacy_panels) = store::load_ui_state("panels") {
        let patch = doc.migrate_diff_layout(&legacy_panels);
        if !patch.is_empty() { store::patch_client_settings(patch); }
    }
    crate::notifications::prepare_sounds(doc.notification_mode());
    theme::set_preferences(preferences(&doc), cx);
    terminal::set_typography(terminal::typography(doc.entries()), cx);
    cx.set_global(ClientPrefs { doc, versions: Default::default(), library_versions: Default::default(), error: None, saving: false, queue: VecDeque::new() });
}

fn preferences(doc: &ClientSettingsDoc) -> Preferences {
    Preferences {
        theme: doc.theme(),
        contrast: doc.appearance_contrast(), glass: doc.glass_opacity(),
        halves: t3_core::themes::halves(doc), custom: t3_core::themes::library(doc),
        mode: doc.appearance_mode(),
        interface_font_size: doc.interface_font_size() as f32,
        typography: doc.typography(),
        chat_width: doc.chat_width(),
        diff_color_scheme: doc.diff_color_scheme(),
    }
}

pub fn client(cx: &App) -> &ClientSettingsDoc {
    &cx.global::<ClientPrefs>().doc
}

pub fn error(cx: &App) -> Option<SharedString> {
    cx.global::<ClientPrefs>().error.clone()
}

pub fn clear_error(cx: &mut App) {
    cx.global_mut::<ClientPrefs>().error = None;
}

/// Shows `message` above the settings page until the next edit, as a failed save does.
pub fn show_error(message: impl Into<SharedString>, cx: &mut App) {
    cx.global_mut::<ClientPrefs>().error = Some(message.into());
    cx.refresh_windows();
}

/// Applies a client setting everywhere at once and saves the changed keys in the background.
pub fn update_client(state: &Entity<AppState>, change: ClientSettingChange, cx: &mut App) {
    apply_client(state, change, true, cx);
}
#[cfg(all(test, feature = "snapshot"))]
pub fn install_committed_client(
    state: &Entity<AppState>,
    patch: serde_json::Map<String, serde_json::Value>,
    cx: &mut App,
) {
    apply_client(state, ClientSettingChange::ThemePatch(patch), false, cx);
}
pub fn install_client_commit(state: &Entity<AppState>, receipt: store::ClientCommit, cx: &mut App) {
    let prefs = cx.global_mut::<ClientPrefs>();
    let mut patch = serde_json::Map::new();
    for (key, value) in receipt.patch {
        if key == t3_core::themes::LIBRARY_KEY {
            let library = prefs.library_versions.committed(
                prefs.doc.entries().get(&key),
                &value,
                receipt.sequence,
            );
            patch.insert(key, library);
        } else if prefs.versions.get(&key).copied().unwrap_or_default() <= receipt.sequence {
            prefs.versions.insert(key.clone(), receipt.sequence);
            patch.insert(key, value);
        }
    }
    apply_client(state, ClientSettingChange::ThemePatch(patch), false, cx);
}
fn apply_client(
    state: &Entity<AppState>,
    change: ClientSettingChange,
    persist: bool,
    cx: &mut App,
) {
    let prefs = cx.global_mut::<ClientPrefs>();
    let library_changed = matches!(&change, ClientSettingChange::ThemeLibrary(_))
        || matches!(&change,ClientSettingChange::ThemePatch(patch) if patch.contains_key(t3_core::themes::LIBRARY_KEY));
    let before_library = library_changed
        .then(|| {
            prefs
                .doc
                .entries()
                .get(t3_core::themes::LIBRARY_KEY)
                .cloned()
        })
        .flatten();
    let grouping_override = matches!(&change, ClientSettingChange::ProjectGroupingOverride { .. }).then(|| prefs.doc.clone());
    let patch = prefs.doc.apply_change(&change);
    if patch.is_empty() && !(persist && grouping_override.is_some()) {
        return;
    }
    if persist {
        let (keys, sequence) = if let Some(fallback) = grouping_override {
            (vec!["sidebarProjectGroupingOverrides".to_owned()], store::patch_client_setting_change(fallback, change.clone()))
        } else {
            (patch.keys().cloned().collect::<Vec<_>>(), store::patch_client_settings_rebased(patch, before_library.clone()))
        };
        if let Some(sequence) = sequence {
            for key in keys {
                prefs.versions.insert(key, sequence);
            }
            if library_changed
                && let Some(after) = prefs.doc.entries().get(t3_core::themes::LIBRARY_KEY)
            {
                prefs
                    .library_versions
                    .edited(before_library.as_ref(), after, sequence);
            }
        }
    }
    let (composer, next) = (prefs.doc.composer(), preferences(&prefs.doc));
    let typography = terminal::typography(prefs.doc.entries());
    state.update(cx, |state, cx| {
        state.client_settings = composer;
        let grouping = client(cx).project_grouping_settings();
        if client(cx).working_shelf() != state.sidebar.settings.working_shelf_enabled || grouping.mode != state.sidebar.settings.grouping.mode || grouping.overrides != state.sidebar.settings.grouping.overrides || matches!(change, ClientSettingChange::ProjectSortOrder(_) | ClientSettingChange::BoringSidebarEnabled(_)) {
            state.rebuild_sidebar(cx);
            cx.emit(AppEvent::Shell);
        }
        if matches!(
            change,
            ClientSettingChange::NotificationMode(_) | ClientSettingChange::InAppNotifications(_)
        ) {
            crate::notifications::prepare_sounds(state.client_settings.notification_mode);
            state.observe_notifications(cx);
        }
        if matches!(
            change,
            ClientSettingChange::DiffLayout(_)
                | ClientSettingChange::DiffFilesCollapsed(_)
                | ClientSettingChange::DiffIgnoreWhitespace(_)
        ) {
            cx.emit(AppEvent::RightPanel);
        }
    });
    theme::set_preferences(next, cx);
    terminal::set_typography(typography, cx);
    cx.refresh_windows();
}
/// Saves one server setting. A failure keeps the saved value on screen and says why.
pub fn update_server(state: &Entity<AppState>, change: ServerSettingChange, cx: &mut App) {
    update_server_with(state, change, None, cx);
}

/// `update_server`, telling the caller when the save failed so it can drop a local draft.
pub fn update_server_with(state: &Entity<AppState>, change: ServerSettingChange, on_failed: Option<OnFailed>, cx: &mut App) {
    let on_done = on_failed.map(|on_failed| -> OnDone {
        Box::new(move |error, cx| {
            if error.is_some() {
                on_failed(cx);
            }
        })
    });
    enqueue(state, change, on_done, cx);
}

/// `update_server`, then `on_done` once the save settles, so a caller can follow it with writes
/// that depend on it.
pub fn update_server_then(state: &Entity<AppState>, change: ServerSettingChange, on_done: impl FnOnce(Option<String>, &mut App) + 'static, cx: &mut App) {
    enqueue(state, change, Some(Box::new(on_done)), cx);
}

fn enqueue(state: &Entity<AppState>, change: ServerSettingChange, on_done: Option<OnDone>, cx: &mut App) {
    let prefs = cx.global_mut::<ClientPrefs>();
    prefs.error = None;
    prefs.queue.push_back(PendingSave { state: state.clone(), target: SaveTarget::Active(change), on_done });
    if !prefs.saving {
        send_next(cx);
    }
}

fn send_next(cx: &mut App) {
    let prefs = cx.global_mut::<ClientPrefs>();
    let Some(PendingSave { state, target, on_done }) = prefs.queue.pop_front() else {
        prefs.saving = false;
        return;
    };
    prefs.saving = true;
    let change = match target {
        SaveTarget::Active(change) => change,
        SaveTarget::Projects { members, edit } => return send_projects(state, members, edit, on_done, cx),
    };
    let Some(payload) = change.payload_for(state.read(cx).server_config.config.as_ref().map(|config| &config.settings)) else {
        if let Some(on_done) = on_done {
            on_done(Some("Reconnect to save this setting.".to_owned()), cx);
        }
        return send_next(cx);
    };
    let (label, sent_at, task) = {
        let app_state = state.read(cx);
        let label = app_state
            .server_config
            .config
            .as_ref()
            .map(|config| config.environment.label.clone())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| "the environment".to_owned());
        (label, app_state.server_config.settings_revision(), app_state.rpc(method::SERVER_UPDATE_SETTINGS, payload))
    };
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(reply) => {
                    if let Some(settings) = decode_update_reply(reply) {
                        state.update(cx, |state, cx| {
                            if state.server_config.apply_settings_reply(settings, sent_at) {
                                cx.emit(AppEvent::ServerConfig);
                            }
                        });
                    }
                    if let Some(on_done) = on_done {
                        on_done(None, cx);
                    }
                }
                Err(error) => {
                    cx.global_mut::<ClientPrefs>().error = Some(format!("Could not save on {label}: {error}").into());
                    if let Some(on_done) = on_done {
                        on_done(Some(error.to_string()), cx);
                    }
                    cx.refresh_windows();
                }
            }
            send_next(cx);
        });
    })
    .detach();
}

pub(super) fn project_current(state: &AppState, project: &t3_core::sidebar::grouping::ScopedProject, scope: &str) -> bool {
    state.scoped_ready(&project.owner, scope)
        && state.scoped_project(&project.reference()).is_some_and(|current| current.owner == project.owner
            && current.project.created_at == project.project.created_at && current.project.workspace_root == project.project.workspace_root)
}

pub(super) fn update_projects(
    state: &Entity<AppState>, members: Vec<t3_core::sidebar::grouping::ScopedProject>,
    edit: t3_core::settings::projects::OverrideEdit, on_done: impl FnOnce(Option<String>, &mut App) + 'static, cx: &mut App,
) {
    let prefs = cx.global_mut::<ClientPrefs>();
    prefs.error = None;
    prefs.queue.push_back(PendingSave { state: state.clone(), target: SaveTarget::Projects { members, edit }, on_done: Some(Box::new(on_done)) });
    if !prefs.saving { send_next(cx); }
}

fn prepare_project_writes(
    app: &AppState, destination: &t3_client::environments::Destination,
    members: &[t3_core::sidebar::grouping::ScopedProject], edit: &t3_core::settings::projects::OverrideEdit,
) -> Option<(Vec<t3_core::settings::project_actions::ActionWrite>, u64, u64, String)> {
    use t3_core::settings::project_actions::{ActionWrite, Member, plan_writes};
    let view = app.connections.view();
    let view = view.borrow();
    if !destination.is_current(&view) { return None; }
    let entry = view.environment(&destination.owner.saved_id)?;
    let config = entry.config.config.as_ref()?;
    let selected = members.iter().filter(|project| project.owner == destination.owner).collect::<Vec<_>>();
    if selected.is_empty() { return None; }
    let as_member = |project: &t3_core::sidebar::grouping::ScopedProject| Member {
        id: project.project.id.clone(), scripts: project.project.scripts.iter().filter_map(|script| serde_json::from_value(script.clone()).ok()).collect(),
    };
    let targets = selected.iter().map(|project| {
        if !project_current(app, project, "settings:write") { return None; }
        let current = entry.summary.projects.iter().find(|current| current.project.id == project.project.id)?;
        (current.project.created_at == project.project.created_at && current.project.workspace_root == project.project.workspace_root)
            .then(|| as_member(current))
    }).collect::<Option<Vec<_>>>()?;
    let writes = match edit {
        t3_core::settings::projects::OverrideEdit::Scripts(edit) => plan_writes(&config.settings, &t3_core::keybindings::decode_resolved_keybindings(&config.keybindings),
            &entry.summary.projects.iter().map(as_member).collect::<Vec<_>>(), &targets, edit),
        _ => vec![ActionWrite::Settings(t3_core::settings::projects::override_patch(&config.settings, &targets, edit))],
    };
    Some((writes, entry.settings_revision, entry.keybindings_revision, config.environment.label.clone()))
}

fn send_projects(
    state: Entity<AppState>, members: Vec<t3_core::sidebar::grouping::ScopedProject>,
    edit: t3_core::settings::projects::OverrideEdit, on_done: Option<OnDone>, cx: &mut App,
) {
    const SCOPE: &str = "settings:write";
    let mut owners = Vec::new();
    for project in &members {
        if !owners.contains(&project.owner) { owners.push(project.owner.clone()); }
    }
    let valid = !members.is_empty() && members.iter().all(|project| project_current(state.read(cx), project, SCOPE));
    if !valid {
        let message = "A project destination changed or cannot change settings.".to_owned();
        show_error(message.clone(), cx);
        if let Some(done) = on_done { done(Some(message), cx); }
        return send_next(cx);
    }
    let lookups = owners.iter().map(|owner| state.read(cx).connections.destination(owner, SCOPE)).collect::<Vec<_>>();
    cx.spawn(async move |cx| {
        let mut failure = None;
        let mut destinations = Vec::new();
        for lookup in lookups {
            match lookup.await {
                Ok(destination) => destinations.push(destination),
                Err(error) => { failure = Some(format!("Project settings preflight failed: {error:?}")); break; }
            }
        }
        if failure.is_none() && !cx.update(|cx| members.iter().all(|project| project_current(state.read(cx), project, SCOPE))
            && destinations.iter().all(|destination| destination.is_current(&state.read(cx).fleet)
                && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(SCOPE)))) {
            failure = Some("Project settings preflight failed: a checkout or permission changed; nothing was sent.".to_owned());
        }
        let prepared = cx.update(|cx| destinations.iter().all(|destination| prepare_project_writes(state.read(cx), destination, &members, &edit).is_some()));
        if !prepared && failure.is_none() { failure = Some("Project settings preflight failed: a checkout or its settings are unavailable; nothing was sent.".to_owned()); }
        let mut changed = 0;
        if failure.is_none() {
            for destination in destinations {
                let plan = cx.update(|cx| prepare_project_writes(state.read(cx), &destination, &members, &edit));
                let Some((writes, settings_revision, keybindings_revision, label)) = plan else {
                    failure = Some(format!("Project settings stopped after {changed} saved write(s): a checkout or its settings changed."));
                    break;
                };
                for write in writes {
                    let valid = cx.update(|cx| {
                        let app = state.read(cx);
                        let view = app.connections.view();
                        let view = view.borrow();
                        destination.is_current(&view)
                            && members.iter().filter(|project| project.owner == destination.owner).all(|project| {
                                project_current(app, project, SCOPE) && view.environment(&destination.owner.saved_id).is_some_and(|entry|
                                    entry.summary.projects.iter().any(|current| current.project.id == project.project.id
                                        && current.project.created_at == project.project.created_at && current.project.workspace_root == project.project.workspace_root))
                            })
                    }) && destination.client.session_state().borrow().as_ref().is_some_and(|grants| grants.grants(SCOPE));
                    if !valid { failure = Some(format!("Project settings stopped on {label} after {changed} saved write(s): permission or checkout changed.")); break; }
                    let client = destination.client.clone();
                    let method = write.method();
                    if method != method::SERVER_UPDATE_SETTINGS && !cx.update(|cx| {
                        let view = state.read(cx).connections.view();
                        let view = view.borrow();
                        destination.is_current(&view) && view.environment(&destination.owner.saved_id)
                            .is_some_and(|entry| entry.keybindings_revision == keybindings_revision)
                    }) {
                        failure = Some(format!("Project settings stopped on {label} after {changed} saved write(s): shortcuts changed."));
                        break;
                    }
                    let payload = write.payload().clone();
                    let runtime = cx.update(|cx| state.read(cx).runtime().clone());
                    let response = runtime.spawn(async move { client.request(method, payload).await }).await;
                    let reply = match response {
                        Ok(Ok(reply)) => reply,
                        Ok(Err(error)) => { failure = Some(format!("Project settings stopped on {label} after {changed} saved write(s): {error}")); break; }
                        Err(error) => { failure = Some(format!("Project settings stopped on {label} after {changed} saved write(s): {error}")); break; }
                    };
                    changed += 1;
                    let applied = if method == method::SERVER_UPDATE_SETTINGS {
                        let Some(settings) = decode_update_reply(reply.clone()) else { failure = Some(format!("Project settings stopped on {label}: invalid reply after {changed} saved write(s).")); break; };
                        let apply = cx.update(|cx| state.read(cx).connections.apply_settings_reply(&destination.owner, settings, settings_revision));
                        apply.await
                    } else {
                        let apply = cx.update(|cx| state.read(cx).connections.apply_keybindings_reply(&destination.owner, reply.clone(), keybindings_revision));
                        apply.await
                    };
                    if let Err(error) = applied { failure = Some(format!("Project settings stopped on {label} after {changed} saved write(s): reply owner changed ({error:?}).")); break; }
                    cx.update(|cx| state.update(cx, |app, cx| {
                        if !destination.is_current(&app.connections.view().borrow()) || app.environments.active.as_deref() != Some(&destination.owner.saved_id) { return; }
                        if method == method::SERVER_UPDATE_SETTINGS {
                            if let Some(settings) = decode_update_reply(reply) { app.server_config.apply_settings_reply(settings, settings_revision); }
                        } else { app.server_config.apply_keybindings_reply(&reply, keybindings_revision); }
                        cx.emit(AppEvent::ServerConfig);
                    }));
                }
                if failure.is_some() { break; }
            }
        }
        cx.update(|cx| {
            if let Some(message) = &failure { show_error(message.clone(), cx); }
            if let Some(done) = on_done { done(failure, cx); }
            send_next(cx);
        });
    }).detach();
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use serde_json::json;
    use t3_client::{supervisor::{ClientFixture, ConnectionStatus}, session::RpcError};
    use t3_core::{server_config::ServerConfigState, shell::ShellState, settings::projects::OverrideEdit};

    #[gpui::test]
    fn working_shelf_preference_drives_cached_sidebar_on_load_and_edits(cx: &mut gpui::TestAppContext) {
        use std::{cell::Cell, rc::Rc, sync::Arc};
        use t3_core::sidebar::{Section, fleet::EnvironmentSummary, grouping::EnvironmentOwner};

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (state, mut fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        cx.update(|cx| cx.global_mut::<ClientPrefs>().doc = ClientSettingsDoc::from_json(r#"{
            "sidebarWorkingShelfEnabled":false,"boringSidebarEnabled":false
        }"#));
        state.update(cx, |state, cx| {
            let snapshot = state.shell.snapshot.as_mut().unwrap();
            snapshot.threads.truncate(2);
            for (index, thread) in snapshot.threads.iter_mut().enumerate() {
                thread.project_id = "checkout-a".into();
                thread.status = "running".into();
                thread.latest_run_id = Some(format!("run-{index}"));
                thread.active_run_id = thread.latest_run_id.clone();
                thread.active_order_key = Some(if index == 0 { "a" } else { "z" }.into());
                thread.latest_user_authored_message_at = Some(Some(format!("2026-01-01T0{}:00:00Z", index + 1)));
            }
            let owner = EnvironmentOwner {
                saved_id:"saved-fixture".into(), canonical_id:Some("fixture-env".into()),
                generation:1, connection_generation:1, shell_generation:state.shell.generation,
            };
            let summary = Arc::new(EnvironmentSummary::from_shell(owner, &state.shell, &state.server_config));
            state.sidebar.grouping_projects = summary.projects.clone();
            state.sidebar.grouping_threads = summary.threads.clone();
            state.fleet = Arc::new(t3_client::environments::FleetView { revision:1, environments:vec![Arc::new(t3_client::environments::EnvironmentView {
                summary, status:ConnectionStatus::Connected, config:state.server_config.clone(),
                grants:Some(crate::context_menu::project_filter_tests::session(true)), settings_revision:0, keybindings_revision:0,
            })] });
            state.project_preferences = Default::default();
            state.rebuild_sidebar(cx);
        });
        let unchanged_shell = state.read_with(cx, |state, _| state.shell.snapshot.clone());
        let unchanged_summary = state.read_with(cx, |state, _| state.fleet.environments[0].summary.clone());
        let shell_events = Rc::new(Cell::new(0));
        let observed = shell_events.clone();
        let _subscription = state.update(cx, |_, cx| cx.subscribe(&state, move |_, _, event, _| {
            if *event == AppEvent::Shell { observed.set(observed.get() + 1); }
        }));
        let assert_sidebar = |cx: &gpui::TestAppContext, enabled: bool, folders: bool, step: &str| {
            state.read_with(cx, |state, cx| {
                let section = if enabled && !folders { Section::Working } else { Section::Active };
                assert_eq!(state.sidebar.fleet.section_of(&t3_core::sidebar::tag_filter::ThreadKey::new("saved-fixture", "menu-thread")), Some(section), "{step}: committed preference drives actual scoped partition");
                assert_eq!(state.sidebar.model.section_of("menu-thread"), Some(if enabled { Section::Working } else { Section::Active }), "{step}: local partition consumes the same preference");
                assert_eq!(state.sidebar.settings.working_shelf_enabled, enabled, "{step}");
                assert_eq!(client(cx).working_shelf(), enabled, "{step}");
                let indexes = if enabled && !folders { &state.sidebar.fleet.partition.working } else { &state.sidebar.fleet.partition.active };
                let ids = indexes.iter().map(|index| state.sidebar.fleet.threads[*index].thread.id.as_str()).collect::<Vec<_>>();
                assert_eq!(ids, if enabled || folders { vec!["sibling", "menu-thread"] } else { vec!["menu-thread", "sibling"] }, "{step}: send versus saved Active order");
                assert_eq!(state.shell.snapshot, unchanged_shell, "{step}: no shell change needed");
                assert!(Arc::ptr_eq(&state.fleet.environments[0].summary, &unchanged_summary));
            });
        };
        assert_sidebar(cx, false, false, "loaded false");
        for (step, enabled, folders, receipt) in [
            ("toggle on", true, false, false),
            ("toggle off", false, false, false),
            ("reset default", true, false, false),
            ("committed false", false, false, true),
            ("committed true", true, false, true),
            ("folder off", false, true, false),
            ("folder on", true, true, false),
            ("folder committed false", false, true, true),
            ("normal stored false", false, false, true),
        ] {
            cx.update(|cx| {
                if client(cx).boring_sidebar_enabled() != folders {
                    update_client(&state, ClientSettingChange::BoringSidebarEnabled(folders), cx);
                }
            });
            let before = shell_events.get();
            let changed = cx.update(|cx| client(cx).working_shelf() != enabled);
            cx.update(|cx| {
                if receipt {
                    install_client_commit(&state, store::ClientCommit {
                        sequence:u64::MAX,
                        patch:serde_json::from_value(json!({"sidebarWorkingShelfEnabled":enabled})).unwrap(),
                    }, cx);
                } else { update_client(&state, ClientSettingChange::WorkingShelf(enabled), cx); }
            });
            assert_sidebar(cx, enabled, folders, step);
            if changed { assert!(shell_events.get() > before, "{step}: settings edit invalidates cached sidebar without a shell update"); }
        }
        assert!(!fixture.take_commands().iter().any(|method| *method == t3_core::commands::method::DISPATCH_COMMAND), "preference changes never dispatch thread mutations");
        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| { cx.shutdown(); cx.clear_globals(); });
        drop(fixture); drop(runtime); cx.run_until_parked();
    }

    #[test]
    fn production_working_shelf_control() {
        use gpui::{AppContext, HeadlessAppContext, IntoElement, ParentElement, Render, Styled, div, px, size};
        use std::sync::Arc;
        use t3_core::sidebar::{Section, Shelf, fleet::ScopedSidebarRow, tag_filter::ThreadKey};

        let Some(out) = std::env::var_os("T3_WORKING_CAPTURE") else { return };
        let out = std::path::PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        let click_at = std::env::var("T3_WORKING_CLICK").ok().map(|value| {
            let (x, y) = value.split_once(',').expect("T3_WORKING_CLICK=x,y logical pixels");
            (x.parse::<f32>().unwrap(), y.parse::<f32>().unwrap())
        });
        struct WorkingFrame {
            pane: Entity<crate::sidebar::SidebarPane>,
            menu: Entity<crate::context_menu::ContextMenu>,
            dialog: Entity<crate::dialog::ConfirmDialog>,
            snooze: Entity<crate::snooze_dialog::SnoozeDialog>,
        }
        impl Render for WorkingFrame {
            fn render(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
                div().relative().size_full().flex().bg(crate::theme::hsla(crate::theme::palette(cx).background))
                    .child(div().w(px(320.)).h_full().child(self.pane.clone()))
                    .child(div().flex_1().h_full())
                    .child(self.menu.clone()).child(self.dialog.clone()).child(self.snooze.clone())
                    .children(crate::settings::overlay(window, cx))
            }
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut cx = HeadlessAppContext::with_platform(
            gpui_platform::current_platform(true).text_system(), Arc::new(crate::icons::Assets),
            gpui_platform::current_headless_renderer,
        );
        cx.allow_parking();
        let (state, mut active_fixture) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            install_committed_client(&installed.0, serde_json::from_value(json!({
                "sidebarWorkingShelfEnabled":true,"boringSidebarEnabled":false
            })).unwrap(), cx);
            installed
        });
        store::save_ui_state(t3_core::composer::drafts::STORE_NAME, &json!({"version":1,"drafts":{}}));
        runtime.block_on(runtime.spawn_blocking(|| store::commit_client_settings(serde_json::Map::new())))
            .expect("fixture writer barrier task").expect("fixture drafts committed");
        let (ready, loaded) = tokio::sync::oneshot::channel();
        let mut ready = Some(ready);
        let _loaded = state.update(&mut cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::Shell && state.fleet.environments.len() == 1 {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        let (actor, mut fixture) = ClientFixture::handle(serde_json::from_value(json!({
            "authenticated":true,"permissions":["orchestration:read","orchestration:operate"]
        })).unwrap());
        let mut shell = ShellState::default();
        shell.apply(serde_json::from_value(json!({"kind":"snapshot","snapshot":{
            "snapshotSequence":1,"projects":[{
                "id":"working-project","title":"Working preference","workspaceRoot":"/fixture/working",
                "createdAt":"2026-11-01T00:00:00Z","updatedAt":"2026-11-01T00:00:00Z",
                "projectIcon":{"kind":"monogram","text":"W","color":"blue"}
            }],"threads":(["running","idle"].map(|id| json!({
                "id":id,"projectId":"working-project","title":format!("{id} thread"),
                "createdAt":"2026-11-01T00:00:00Z","updatedAt":"2026-11-01T00:00:00Z",
                "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},
                "runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":id},
                "status":if id=="running" { "running" } else { "idle" },
                "latestRunId":if id=="running" { Some("fixture-run") } else { None },
                "activeRunId":if id=="running" { Some("fixture-run") } else { None },
                "activityRunStartedAt":if id=="running" { Some("2026-11-01T00:00:00Z") } else { None },
                "hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0
            })))
        }})).unwrap());
        shell.synchronized = true;
        let mut config = ServerConfigState::default();
        config.apply(serde_json::from_value(json!({"type":"snapshot","config":{
            "environment":{"environmentId":"working-owner","label":"Working fixture"},
            "cwd":"/fixture/working","settings":{}
        }})).unwrap());
        let install = state.update(&mut cx, |state, _| state.connections.install_fixture("working-owner", actor, shell, config, ConnectionStatus::Connected));
        let events = runtime.block_on(install);
        let executor = cx.update(|cx| cx.foreground_executor().clone());
        executor.block_test(loaded).unwrap();
        state.update(&mut cx, |state, cx| {
            state.project_preferences = Default::default();
            state.sidebar.shelves.working_expanded = true;
            state.rebuild_sidebar(cx);
        });
        let route = state.read_with(&cx, |state, _| (state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()));
        let assert_sidebar = |cx: &HeadlessAppContext, enabled: bool| {
            state.read_with(cx, |state, cx| {
                assert_eq!(client(cx).working_shelf(), enabled);
                assert!(error(cx).is_none());
                let model = &state.sidebar.fleet;
                let mut keys = model.threads.iter().map(|row| row.key.clone()).collect::<Vec<_>>();
                keys.sort_by(|a,b| (&a.environment_id,&a.thread_id).cmp(&(&b.environment_id,&b.thread_id)));
                assert_eq!(keys, [ThreadKey::new("working-owner","idle"),ThreadKey::new("working-owner","running")]);
                assert_eq!(model.section_of(&keys[0]), Some(Section::Active));
                assert_eq!(model.section_of(&keys[1]), Some(if enabled { Section::Working } else { Section::Active }));
                assert_eq!(model.rows.iter().filter(|row| matches!(row,ScopedSidebarRow::Shelf { shelf:Shelf::Working, .. })).count(), usize::from(enabled));
                assert_eq!((state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()), route);
            });
        };
        assert_sidebar(&cx, true);
        let window = cx.open_window(size(px(1280.), px(900.)), |window, cx| {
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
            let dialog = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| crate::sidebar::SidebarPane::new(state.clone(), composer, menu.clone(), dialog.clone(), snooze.clone(), window, cx));
            cx.new(|_| WorkingFrame { pane, menu, dialog, snooze })
        }).unwrap();
        let capture = |cx:&mut HeadlessAppContext, name:&str| {
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| { window.refresh(); window.draw(cx).clear(); }).unwrap();
            cx.capture_screenshot(window.into()).unwrap().save(out.join(format!("{name}.png"))).unwrap();
        };
        capture(&mut cx,"normal-enabled");
        cx.update(|cx| crate::settings::open_setting("working-shelf",cx));
        capture(&mut cx,"settings-enabled");
        if let Some((x,y)) = click_at {
            for (enabled, settings_image, normal_image) in [
                (false,"settings-disabled","normal-disabled"),
                (true,"settings-restored","normal-restored"),
            ] {
                cx.update_window(window.into(), |_, window, cx| {
                    let position = gpui::point(px(x),px(y));
                    window.simulate_mouse_move(position,cx);
                    window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent { button:gpui::MouseButton::Left,position,modifiers:Default::default(),click_count:1,first_mouse:false }),cx);
                    window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent { button:gpui::MouseButton::Left,position,modifiers:Default::default(),click_count:1 }),cx);
                }).unwrap();
                cx.run_until_parked();
                assert_sidebar(&cx,enabled);
                capture(&mut cx,settings_image);
                cx.update_window(window.into(), |_,window,cx| window.dispatch_action(Box::new(crate::settings::CloseSettings),cx)).unwrap();
                capture(&mut cx,normal_image);
                if !enabled {
                    cx.update(|cx| crate::settings::open_setting("working-shelf",cx));
                    capture(&mut cx,"settings-reopened");
                }
            }
            runtime.block_on(runtime.spawn_blocking(|| store::commit_client_settings(serde_json::Map::new())))
                .expect("preference writer barrier task").expect("preference restored on disk");
            assert!(ClientSettingsDoc::from_json(&store::load_client_settings().unwrap()).working_shelf());
        } else {
            cx.update_window(window.into(), |_,window,cx| window.dispatch_action(Box::new(crate::settings::CloseSettings),cx)).unwrap();
        }
        let mut commands = active_fixture.take_commands();
        commands.extend(fixture.take_commands());
        assert!(!commands.iter().any(|method| matches!(*method,
            t3_core::commands::method::DISPATCH_COMMAND | t3_core::commands::method::LAUNCH_THREAD | t3_core::projects::PROJECTS_MUTATE
            | t3_core::commands::method::SERVER_UPDATE_SETTINGS | t3_core::settings::keybindings::SERVER_UPSERT_KEYBINDING
            | t3_core::settings::keybindings::SERVER_REMOVE_KEYBINDING | "guestShare.create" | "projects.createFromThread" | "projects.createNew"
        )),"actual Settings clicks must not send RPC writes");
        std::fs::write(out.join("working-control.json"),serde_json::to_vec_pretty(&json!({
            "setting":"working-shelf","switch":"working-shelf-switch","window":[1280,900],
            "stage":if click_at.is_some() { "actual-click-off-on" } else { "calibration-only" },"observedClick":click_at,
            "normalEnabled":{"running":"Working","idle":"Active"},
            "disabled":click_at.map(|_| json!({"running":"Active","idle":"Active"})),
            "restored":click_at.map(|_| json!({"running":"Working","idle":"Active"})),
            "keys":[["working-owner","idle"],["working-owner","running"]],"routeUnchanged":true,"forbiddenRpcWrites":0,
            "input":"native Input/NSView not exercised","commandsObserved":commands
        })).unwrap()).unwrap();
        state.update(&mut cx,|state,_| state.stop_pump());
        drop(events);
        cx.update(|cx| { cx.shutdown(); cx.clear_globals(); });
        drop(fixture); drop(active_fixture); drop(runtime); cx.run_until_parked();
    }

    #[gpui::test]
    async fn grouped_settings_queue_keeps_destinations_and_reports_partial_writes(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        for scenario in ["saved", "denied", "missing-config", "partial", "action-partial", "later-owner-stream", "shortcut-stream"] {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let (state, _active_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
            let before = state.read_with(cx, |state, _| (state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()));
            let (ready, loaded) = tokio::sync::oneshot::channel();
            let mut ready = Some(ready);
            let _subscription = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                if *event == AppEvent::Shell && state.fleet.environments.len() == 2 {
                    if let Some(ready) = ready.take() { let _ = ready.send(()); }
                }
            }));
            let mut fixtures = Vec::new();
            let mut events = Vec::new();
            for saved in ["a", "b"] {
                let session = serde_json::from_value(json!({"authenticated":true,"permissions":if scenario == "denied" && saved == "b" { Vec::<&str>::new() } else { vec!["settings:write"] }})).unwrap();
                let (client, fixture) = ClientFixture::handle(session);
                let mut shell = ShellState::default();
                shell.apply(serde_json::from_value(json!({"kind":"snapshot","snapshot":{"snapshotSequence":1,"projects":[{
                    "id":"same", "title":saved, "workspaceRoot":format!("/fixture/{saved}"), "createdAt":"created", "updatedAt":"updated",
                    "projectIcon":{"kind":"monogram","text":"P","color":"blue"}
                }],"threads":[]}})).unwrap());
                shell.synchronized = true;
                let mut config = ServerConfigState::default();
                if scenario != "missing-config" || saved != "b" {
                    config.apply(serde_json::from_value(json!({"type":"snapshot","config":{
                        "environment":{"environmentId":saved,"label":saved},
                        "settings":{"projectSettingsOverrides":{"same":{"defaultThreadEnvMode":"local","retained":saved}}}
                    }})).unwrap());
                }
                let install = state.update(cx, |state, _| state.connections.install_fixture(saved, client, shell, config, ConnectionStatus::Connected));
                events.push(runtime.spawn(install).await.unwrap());
                fixtures.push(fixture);
            }
            loaded.await.unwrap();
            let members = state.read_with(cx, |state, _| state.fleet.environments.iter().flat_map(|entry| entry.summary.projects.iter().cloned()).collect::<Vec<_>>());
            let (finished, done) = tokio::sync::oneshot::channel();
            let edit = if matches!(scenario, "action-partial" | "shortcut-stream") {
                OverrideEdit::Scripts(t3_core::settings::project_actions::ScriptsEdit::Save {
                    id:"test".into(), is_new:true, input:t3_core::settings::project_actions::ScriptInput {
                        name:"Test".into(), command:"node --test".into(), keybinding:Some(Some("cmd-t".into())), ..Default::default()
                    },
                })
            } else { OverrideEdit::Set { key:"defaultThreadEnvMode".into(), value:json!("worktree") } };
            cx.update(|cx| update_projects(&state, members.clone(), edit, move |failure, _| { let _ = finished.send(failure); }, cx));
            if matches!(scenario, "denied" | "missing-config") {
                assert!(done.await.unwrap().is_some());
                assert!(fixtures.iter_mut().all(|fixture| fixture.take_commands().is_empty()), "all-member preflight sends nothing");
                continue;
            }
            if scenario == "shortcut-stream" {
                let (rpc, payload, reply) = fixtures[0].next_request().await;
                assert_eq!(rpc, method::SERVER_UPDATE_SETTINGS);
                let expected_revision = state.read_with(cx, |state, _| state.connections.view().borrow().environment("a").unwrap().keybindings_revision + 1);
                let (streamed, applied) = tokio::sync::oneshot::channel();
                let mut streamed = Some(streamed);
                let _streamed = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                    if *event == AppEvent::ServerConfig && state.scoped_environment("a").is_some_and(|entry| entry.keybindings_revision == expected_revision) {
                        if let Some(streamed) = streamed.take() { let _ = streamed.send(()); }
                    }
                }));
                events[0].send(t3_client::supervisor::ClientEvent::ServerConfig(Box::new(serde_json::from_value(json!({
                    "type":"keybindingsUpdated", "payload":{"keybindings":[]}
                })).unwrap()))).await.unwrap();
                applied.await.unwrap();
                reply.send(Ok(payload["patch"].clone())).unwrap();
                let failure = {
                    let next = fixtures[0].next_request();
                    futures::pin_mut!(next);
                    match futures::future::select(done, next).await {
                        futures::future::Either::Left((result, _)) => result.unwrap().expect("shortcut stream drift stops instead of sending a stale plan"),
                        futures::future::Either::Right(((method, _, _), _)) => panic!("stale shortcut write escaped revision preflight: {method}"),
                    }
                };
                assert!(failure.contains("after 1 saved write(s)"));
                assert!(failure.contains("shortcuts changed"));
                assert!(fixtures[1].take_commands().is_empty(), "later owner untouched on shortcut drift");
                continue;
            }
            if scenario == "action-partial" {
                let (rpc, payload, reply) = fixtures[0].next_request().await;
                assert_eq!(rpc, method::SERVER_UPDATE_SETTINGS);
                assert_eq!(payload["patch"]["projectSettingsOverrides"]["same"]["defaultProjectScripts"][0]["id"], "test");
                reply.send(Ok(payload["patch"].clone())).unwrap();
                let (rpc, _, reply) = fixtures[0].next_request().await;
                assert_eq!(rpc, t3_core::settings::keybindings::SERVER_UPSERT_KEYBINDING);
                reply.send(Err(RpcError::Closed)).unwrap();
                let failure = done.await.unwrap().unwrap();
                assert!(failure.contains("after 1 saved write(s)"));
                assert!(fixtures[1].take_commands().is_empty(), "a later owner is untouched after partial shortcut failure");
                continue;
            }
            for (index, fixture) in fixtures.iter_mut().enumerate() {
                let (rpc, payload, reply) = fixture.next_request().await;
                assert_eq!(rpc, method::SERVER_UPDATE_SETTINGS);
                let overrides = &payload["patch"]["projectSettingsOverrides"];
                assert_eq!(overrides["same"]["defaultThreadEnvMode"], "worktree");
                assert_eq!(overrides["same"]["retained"], ["a", "b"][index]);
                if scenario == "later-owner-stream" && index == 0 {
                    let (streamed, applied) = tokio::sync::oneshot::channel();
                    let mut streamed = Some(streamed);
                    let _streamed = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                        if *event == AppEvent::ServerConfig && state.scoped_environment("b").and_then(|entry| entry.config.config.as_ref())
                            .is_some_and(|config| config.settings.project_settings_overrides["same"].raw.contains_key("concurrent")) {
                            if let Some(streamed) = streamed.take() { let _ = streamed.send(()); }
                        }
                    }));
                    events[1].send(t3_client::supervisor::ClientEvent::ServerConfig(Box::new(serde_json::from_value(json!({
                        "type":"settingsUpdated", "payload":{"settings":{"projectSettingsOverrides":{"same":{
                            "defaultThreadEnvMode":"local", "retained":"b", "concurrent":"preserve"
                        }}}}
                    })).unwrap()))).await.unwrap();
                    applied.await.unwrap();
                }
                if scenario == "later-owner-stream" && index == 1 { assert_eq!(overrides["same"]["concurrent"], "preserve", "later destination patch keeps a newer streamed field"); }
                if scenario == "partial" && index == 1 {
                    reply.send(Err(RpcError::Closed)).unwrap();
                } else { reply.send(Ok(payload["patch"].clone())).unwrap(); }
            }
            let failure = done.await.unwrap();
            assert_eq!(failure.is_some(), scenario == "partial");
            if let Some(failure) = failure { assert!(failure.contains("after 1 saved write(s)")); }
            let first_config = state.read_with(cx, |state, _| state.connections.view().borrow().environment("a").unwrap().config.config.clone().unwrap());
            assert_eq!(first_config.settings.project_settings_overrides["same"].raw["defaultThreadEnvMode"], "worktree");
            state.read_with(cx, |state, _| assert_eq!((state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()), before));
            assert!(fixtures.iter_mut().all(|fixture| fixture.take_commands().is_empty()));
            let (finished, done) = tokio::sync::oneshot::channel();
            cx.update(|cx| update_projects(&state, vec![members[0].clone()], OverrideEdit::Set { key:"sidebarAutoSettleAfterDays".into(), value:json!(7) }, move |failure, _| { let _ = finished.send(failure); }, cx));
            let (rpc, payload, reply) = fixtures[0].next_request().await;
            assert_eq!(rpc, method::SERVER_UPDATE_SETTINGS);
            assert_eq!(payload["patch"]["projectSettingsOverrides"]["same"]["defaultThreadEnvMode"], "worktree", "next save keeps the acknowledged first override");
            reply.send(Ok(payload["patch"].clone())).unwrap();
            assert!(done.await.unwrap().is_none());
            drop(events);
        }
    }
}
