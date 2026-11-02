//! Where settings edits go. Client settings live in `settings.json` under `T3_NATIVE_HOME` and
//! apply at once; the store's writer thread saves the file, newest edit wins. Server settings go
//! through `server.updateSettings` and the reply is applied as a `settingsUpdated` event, so every
//! view reads the saved value (`useSettings.ts` `useUpdateSettingsTarget`).

use gpui::{App, Entity, Global, SharedString};
use t3_client::store;
use t3_core::client_settings::{ClientSettingChange, ClientSettingsDoc};
use t3_core::commands::method;
use t3_core::server_config::{ServerConfigStreamEvent, SettingsPayload};
use t3_core::settings::{ServerSettingChange, decode_update_reply};

use crate::app_state::{AppEvent, AppState};
use crate::terminal;
use crate::theme::{self, Preferences};

struct ClientPrefs {
    doc: ClientSettingsDoc,
    /// The last server save that failed, shown above the settings page until the next edit.
    error: Option<SharedString>,
}

impl Global for ClientPrefs {}

pub fn init(cx: &mut App) {
    let doc = ClientSettingsDoc::from_json(&store::load_client_settings().unwrap_or_default());
    theme::set_preferences(preferences(&doc), cx);
    terminal::set_typography(terminal::typography(doc.entries()), cx);
    cx.set_global(ClientPrefs { doc, error: None });
}

fn preferences(doc: &ClientSettingsDoc) -> Preferences {
    Preferences {
        theme: doc.theme(),
        mode: doc.appearance_mode(),
        interface_font_size: doc.interface_font_size() as f32,
        chat_width: doc.chat_width(),
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

/// Applies a client setting everywhere at once and saves the file in the background.
pub fn update_client(state: &Entity<AppState>, change: ClientSettingChange, cx: &mut App) {
    let prefs = cx.global_mut::<ClientPrefs>();
    if !prefs.doc.apply(&change) {
        return;
    }
    store::save_ui_state("settings", prefs.doc.entries());
    let (composer, next) = (prefs.doc.composer(), preferences(&prefs.doc));
    let typography = terminal::typography(prefs.doc.entries());
    state.update(cx, |state, _| state.client_settings = composer);
    theme::set_preferences(next, cx);
    terminal::set_typography(typography, cx);
    cx.refresh_windows();
}

/// Saves one server setting. The reply replaces the environment's settings; a failure keeps the
/// saved value on screen and says why.
pub fn update_server(state: &Entity<AppState>, change: ServerSettingChange, cx: &mut App) {
    cx.global_mut::<ClientPrefs>().error = None;
    let label = state
        .read(cx)
        .server_config
        .config
        .as_ref()
        .map(|config| config.environment.label.clone())
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| "the environment".to_owned());
    let task = state.read(cx).rpc(method::SERVER_UPDATE_SETTINGS, change.payload());
    let state = state.clone();
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(reply) => {
                let Some(settings) = decode_update_reply(reply) else { return };
                state.update(cx, |state, cx| {
                    let event = ServerConfigStreamEvent::SettingsUpdated { payload: Box::new(SettingsPayload { settings }) };
                    if state.server_config.apply(event) {
                        cx.emit(AppEvent::ServerConfig);
                    }
                });
            }
            Err(error) => {
                cx.global_mut::<ClientPrefs>().error = Some(format!("Could not save on {label}: {error}").into());
                cx.refresh_windows();
            }
        });
    })
    .detach();
}
