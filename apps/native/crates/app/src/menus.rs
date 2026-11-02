//! The app menu bar (`apps/desktop/src/window/DesktopApplicationMenu.ts`) for what the native
//! client has: app, File, Edit (routed to the focused text field), View, and Window.

use gpui::{App, Entity, KeyBinding, Menu, MenuItem, SystemMenuType, actions};
use gpui_component::input::{Copy, Cut, Delete, Paste, Redo, SelectAll, Undo};

use crate::app_state::AppState;
use crate::keymap::{ToggleCommandPalette, ToggleSidebar};
use crate::settings::OpenSettings;

actions!(t3, [Quit, SwitchEnvironment, Disconnect, Hide, HideOthers, ShowAll, CloseWindow, Minimize, Zoom, ToggleFullScreen]);

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut gpui::Window)) {
    if let Some(window) = cx.active_window() {
        let _ = window.update(cx, |_, window, _| f(window));
    }
}

pub fn init(state: Entity<AppState>, cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, |window| window.remove_window()));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |window| window.zoom_window()));
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
    cx.set_menus([
        Menu::new("T3 Code").items([
            MenuItem::action("Settings...", OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Switch Environment...", SwitchEnvironment),
            MenuItem::action("Disconnect", Disconnect),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide T3 Code", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit T3 Code", Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Close Window", CloseWindow)]),
        Menu::new("Edit").items([
            MenuItem::action("Undo", Undo),
            MenuItem::action("Redo", Redo),
            MenuItem::separator(),
            MenuItem::action("Cut", Cut),
            MenuItem::action("Copy", Copy),
            MenuItem::action("Paste", Paste),
            MenuItem::action("Delete", Delete),
            MenuItem::separator(),
            MenuItem::action("Select All", SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Command Palette", ToggleCommandPalette),
            MenuItem::separator(),
            MenuItem::action("Toggle Full Screen", ToggleFullScreen),
        ]),
        Menu::new("Window").items([MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom)]),
    ]);
}
