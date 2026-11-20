//! The app menu bar (`apps/desktop/src/window/DesktopApplicationMenu.ts`, `pingdotgg/t3code#39224`)
//! for what the native client has: app, File, Edit (routed to the focused text field), View, and
//! Window. Rebuilt reactively via `install(cx)` whenever the active UI language changes.

use gpui::{App, Entity, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, actions};
use gpui_component::input::{Copy, Cut, Delete, Paste, Redo, SelectAll, Undo};
use t3_core::i18n::{translate, translate_menu, translate_menu_fmt};

use crate::app_state::AppState;
use crate::composer::PasteAsText;
use crate::i18n::active_locale;
use crate::keymap::{ToggleCommandPalette, ToggleFilePicker, ToggleProjectSearch, ToggleSidebar};
use crate::new_thread::{NewThread, NewThreadWithoutProject};
use crate::settings::OpenSettings;
use crate::thread_view::FindInThread;

actions!(t3, [Quit, SwitchEnvironment, Disconnect, Hide, HideOthers, ShowAll, CloseWindow, Minimize, Zoom, BringAllToFront, ToggleFullScreen]);

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut gpui::Window)) {
    if let Some(window) = cx.active_window() {
        let _ = window.update(cx, |_, window, _| f(window));
    }
}

fn app_menu(locale: &str) -> Menu {
    Menu::new("T3 Code").items([
        MenuItem::action(translate_menu(locale, "Settings..."), OpenSettings),
        MenuItem::separator(),
        MenuItem::action(translate(locale, "Open Environments"), SwitchEnvironment),
        MenuItem::action(translate(locale, "Disconnect"), Disconnect),
        MenuItem::separator(),
        MenuItem::os_submenu(translate_menu(locale, "Services"), SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action(translate_menu_fmt(locale, "Hide {appName}", &[("appName", "T3 Code".into())]), Hide),
        MenuItem::action(translate_menu(locale, "Hide Others"), HideOthers),
        MenuItem::action(translate_menu(locale, "Show All"), ShowAll),
        MenuItem::separator(),
        MenuItem::action(translate_menu_fmt(locale, "Quit {appName}", &[("appName", "T3 Code".into())]), Quit),
    ])
}

fn file_menu(locale: &str) -> Menu {
    Menu::new(translate_menu(locale, "File")).items([
        MenuItem::action(translate(locale, "New thread"), NewThread),
        MenuItem::action(translate(locale, "New thread without a project"), NewThreadWithoutProject),
        MenuItem::separator(),
        MenuItem::action(translate_menu(locale, "Close Window"), CloseWindow),
    ])
}

fn edit_menu(locale: &str) -> Menu {
    Menu::new(translate_menu(locale, "Edit")).items([
        MenuItem::action(translate_menu(locale, "Undo"), Undo),
        MenuItem::action(translate_menu(locale, "Redo"), Redo),
        MenuItem::separator(),
        MenuItem::os_action(translate_menu(locale, "Cut"), Cut, OsAction::Cut),
        MenuItem::os_action(translate_menu(locale, "Copy"), Copy, OsAction::Copy),
        MenuItem::os_action(translate_menu(locale, "Paste"), Paste, OsAction::Paste),
        MenuItem::action(translate_menu(locale, "Paste as Text"), PasteAsText),
        MenuItem::action(translate_menu(locale, "Delete"), Delete),
        MenuItem::separator(),
        MenuItem::os_action(translate_menu(locale, "Select All"), SelectAll, OsAction::SelectAll),
    ])
}

fn view_menu(locale: &str) -> Menu {
    Menu::new(translate_menu(locale, "View")).items([
        MenuItem::action(translate(locale, "Toggle Sidebar"), ToggleSidebar),
        MenuItem::action(translate(locale, "Command palette"), ToggleCommandPalette),
        MenuItem::action(translate(locale, "Go to file"), ToggleFilePicker),
        MenuItem::action(translate(locale, "Search project contents"), ToggleProjectSearch),
        MenuItem::action(translate(locale, "Find in thread"), FindInThread),
        MenuItem::separator(),
        MenuItem::action(translate_menu(locale, "Toggle Full Screen"), ToggleFullScreen),
    ])
}

fn window_menu(locale: &str) -> Menu {
    Menu::new(translate_menu(locale, "Window")).items([
        MenuItem::action(translate_menu(locale, "Minimize"), Minimize),
        MenuItem::action(translate_menu(locale, "Zoom"), Zoom),
        MenuItem::separator(),
        MenuItem::action(translate_menu(locale, "Bring All to Front"), BringAllToFront),
    ])
}

/// Builds the full localized macOS application menu bar for `locale`.
pub fn build_menus(locale: &str) -> Vec<Menu> {
    vec![
        app_menu(locale),
        file_menu(locale),
        edit_menu(locale),
        view_menu(locale),
        window_menu(locale),
    ]
}

/// Installs (or reactively rebuilds) the native menu bar in the active UI language
/// (`pingdotgg/t3code#39224`).
pub fn install(cx: &mut App) {
    let locale = active_locale(cx);
    cx.set_menus(build_menus(locale));
}

pub fn init(state: Entity<AppState>, cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, |window| window.remove_window()));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |window| window.zoom_window()));
    cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
    cx.on_action(|_: &ToggleFullScreen, cx| with_active_window(cx, |window| window.toggle_fullscreen()));
    cx.on_action({
        let state = state.clone();
        move |_: &SwitchEnvironment, cx| state.update(cx, |state, cx| state.open_switcher(cx))
    });
    cx.on_action(move |_: &Disconnect, cx| state.update(cx, |state, cx| state.disconnect(cx)));
    cx.bind_keys([
        // Labels the menu item only: `quit_hold` handles the key so it can see auto-repeat.
        KeyBinding::new("cmd-q", Quit, Some("QuitMenuLabel")),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
    ]);
    install(cx);
}

#[cfg(test)]
mod tests {
    use std::any::TypeId;

    use gpui::AppContext;

    use super::*;

    fn os_action_tag(action: Option<OsAction>) -> Option<&'static str> {
        match action? {
            OsAction::Cut => Some("cut"),
            OsAction::Copy => Some("copy"),
            OsAction::Paste => Some("paste"),
            OsAction::SelectAll => Some("select_all"),
            OsAction::Undo => Some("undo"),
            OsAction::Redo => Some("redo"),
        }
    }

    #[test]
    fn edit_menu_routes_clipboard_and_select_all_through_os_actions() {
        let menu = edit_menu("en");
        let actions: Vec<_> = menu
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, action, os_action, .. } => Some((name.as_ref(), action.as_ref().type_id(), os_action_tag(*os_action))),
                _ => None,
            })
            .collect();
        assert_eq!(
            actions,
            vec![
                ("Undo", TypeId::of::<Undo>(), None),
                ("Redo", TypeId::of::<Redo>(), None),
                ("Cut", TypeId::of::<Cut>(), Some("cut")),
                ("Copy", TypeId::of::<Copy>(), Some("copy")),
                ("Paste", TypeId::of::<Paste>(), Some("paste")),
                ("Paste as Text", TypeId::of::<PasteAsText>(), None),
                ("Delete", TypeId::of::<Delete>(), None),
                ("Select All", TypeId::of::<SelectAll>(), Some("select_all")),
            ]
        );
    }

    #[test]
    fn file_view_and_window_menus_match_desktop_structure() {
        let names = |menu: Menu| -> Vec<String> {
            menu.items
                .into_iter()
                .filter_map(|item| match item {
                    MenuItem::Action { name, .. } => Some(name.to_string()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(names(file_menu("en")), ["New thread", "New thread without a project", "Close Window"]);
        assert_eq!(names(view_menu("en")), ["Toggle Sidebar", "Command palette", "Go to file", "Search project contents", "Find in thread", "Toggle Full Screen"]);
        assert_eq!(names(window_menu("en")), ["Minimize", "Zoom", "Bring All to Front"]);
    }

    #[test]
    fn menus_localize_into_non_english_locales() {
        let de = build_menus("de");
        let titles: Vec<_> = de.iter().map(|m| m.name.as_ref()).collect();
        assert_eq!(titles, ["T3 Code", "Datei", "Bearbeiten", "Ansicht", "Fenster"]);
        let edit_names: Vec<_> = de[2]
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.as_ref()),
                _ => None,
            })
            .collect();
        assert!(edit_names.contains(&"Als Text einfügen"));
        assert!(edit_names.contains(&"Alles auswählen"));

        let sv = build_menus("sv");
        let sv_titles: Vec<_> = sv.iter().map(|m| m.name.as_ref()).collect();
        assert_eq!(sv_titles[0], "T3 Code");
        assert_ne!(sv_titles[1], "File");
        assert_ne!(sv_titles[2], "Edit");
        assert_ne!(sv_titles[3], "View");
        assert_ne!(sv_titles[4], "Window");

        let ja = build_menus("ja");
        let ja_titles: Vec<_> = ja.iter().map(|m| m.name.as_ref()).collect();
        assert_eq!(ja_titles[0], "T3 Code");
        assert_ne!(ja_titles[1], "File");
        assert_ne!(ja_titles[4], "Window");
    }

    #[gpui::test]
    fn reactive_language_switch_updates_i18n_and_rebuilds_menus(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            crate::menus::init(state.clone(), cx);
            crate::settings::store::init(cx);
            state
        });
        // Switch en -> de -> sv -> ja and verify active_locale, active_formatting_locale, i18n::t,
        // and reactive menu rebuild all succeed cleanly.
        for (lang, expected_settings, expected_file_menu) in [
            ("en", "Settings", "File"),
            ("de", "Einstellungen", "Datei"),
            ("sv", "Inställningar", "Arkiv"),
            ("ja", "設定", "ファイル"),
        ] {
            cx.update(|cx| {
                crate::settings::store::update_client(
                    &state,
                    t3_core::client_settings::ClientSettingChange::Language(lang.to_owned()),
                    cx,
                );
                assert_eq!(crate::i18n::active_locale(cx), lang);
                assert_eq!(crate::i18n::active_formatting_locale(cx), lang);
                assert_eq!(crate::i18n::t(cx, "Settings"), expected_settings);
                let menus = build_menus(crate::i18n::active_locale(cx));
                assert_eq!(menus[1].name.as_ref(), expected_file_menu);
                if let Some(installed) = cx.get_menus() {
                    assert_eq!(installed[1].name.as_ref(), expected_file_menu);
                }
            });
        }
    }
}
