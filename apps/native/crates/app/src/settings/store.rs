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

struct PendingSave {
    state: Entity<AppState>,
    change: ServerSettingChange,
    on_done: Option<OnDone>,
}

struct ClientPrefs {
    doc: ClientSettingsDoc,
    /// The last server save that failed, shown above the settings page until the next edit.
    error: Option<SharedString>,
    saving: bool,
    queue: VecDeque<PendingSave>,
}

impl Global for ClientPrefs {}

pub fn init(cx: &mut App) {
    let mut doc = ClientSettingsDoc::from_json(&store::load_client_settings().unwrap_or_default());
    if let Some(legacy_panels) = store::load_ui_state("panels") {
        let patch = doc.migrate_diff_layout(&legacy_panels);
        if !patch.is_empty() { store::patch_client_settings(patch); }
    }
    theme::set_preferences(preferences(&doc), cx);
    terminal::set_typography(terminal::typography(doc.entries()), cx);
    cx.set_global(ClientPrefs { doc, error: None, saving: false, queue: VecDeque::new() });
}

fn preferences(doc: &ClientSettingsDoc) -> Preferences {
    Preferences {
        theme: doc.theme(),
        mode: doc.appearance_mode(),
        interface_font_size: doc.interface_font_size() as f32,
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
    let prefs = cx.global_mut::<ClientPrefs>();
    let patch = prefs.doc.apply_change(&change);
    if patch.is_empty() {
        return;
    }
    store::patch_client_settings(patch);
    let (composer, next) = (prefs.doc.composer(), preferences(&prefs.doc));
    let typography = terminal::typography(prefs.doc.entries());
    state.update(cx, |state, cx| {
        state.client_settings = composer;
        if matches!(change, ClientSettingChange::DiffLayout(_) | ClientSettingChange::DiffFilesCollapsed(_) | ClientSettingChange::DiffIgnoreWhitespace(_)) {
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
    prefs.queue.push_back(PendingSave { state: state.clone(), change, on_done });
    if !prefs.saving {
        send_next(cx);
    }
}

fn send_next(cx: &mut App) {
    let prefs = cx.global_mut::<ClientPrefs>();
    let Some(PendingSave { state, change, on_done }) = prefs.queue.pop_front() else {
        prefs.saving = false;
        return;
    };
    prefs.saving = true;
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
