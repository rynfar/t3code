//! App menu bar and the app-level actions behind it.

use gpui::{App, Entity, KeyBinding, Menu, MenuItem, SystemMenuType, actions};

use crate::app_state::AppState;

actions!(t3, [Quit, Disconnect, Hide, HideOthers, ShowAll]);

pub fn init(state: Entity<AppState>, cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(move |_: &Disconnect, cx| state.update(cx, |state, cx| state.forget_connection(cx)));
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
    ]);
    cx.set_menus([Menu::new("T3 Code").items([
        MenuItem::action("Disconnect from Server", Disconnect),
        MenuItem::separator(),
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action("Hide T3 Code", Hide),
        MenuItem::action("Hide Others", HideOthers),
        MenuItem::action("Show All", ShowAll),
        MenuItem::separator(),
        MenuItem::action("Quit T3 Code", Quit),
    ])]);
}
