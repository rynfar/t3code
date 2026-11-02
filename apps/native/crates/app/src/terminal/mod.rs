//! The thread terminal drawer (ledger PN-TERMINAL). `drawer` owns layouts, the server's
//! terminal list and panes; `pane`, `session` and `element` are one terminal each; the VT core
//! and every pure rule live in `t3_core::terminal`. The `terminal.*` commands reach these
//! actions through `keymap` (the web's `mod+j`, and `mod+n`/`mod+d`/`mod+shift+d`/`mod+w` while a
//! terminal is focused); the clipboard keys the web's surface handles itself are bound here.

mod colors;
mod drawer;
mod drawer_view;
mod element;
mod pane;
mod session;

use gpui::prelude::FluentBuilder;
use gpui::{
    App, Entity, InteractiveElement, IntoElement, KeyBinding, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    actions, div, px,
};
use gpui_component::tooltip::Tooltip;

pub use drawer::TerminalDrawer;
pub use element::{set_typography, typography};

use crate::icons::Icon;
use crate::theme::{hsla, palette};

actions!(
    terminal,
    [ToggleTerminal, NewTerminal, SplitTerminal, SplitTerminalVertical, CloseTerminal, Copy, Paste, SelectAll, Clear, JumpToLatest]
);

const CONTEXT: &str = "Terminal";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, Some(CONTEXT)),
        KeyBinding::new("cmd-v", Paste, Some(CONTEXT)),
        KeyBinding::new("cmd-a", SelectAll, Some(CONTEXT)),
        KeyBinding::new("cmd-k", Clear, Some(CONTEXT)),
    ]);
}

/// The chat header's Terminal toggle (design-system §7.4): a 28px ghost toggle, pressed while
/// the drawer is open.
pub fn header_toggle(drawer: &Entity<TerminalDrawer>, cx: &App) -> impl IntoElement + use<> {
    let p = palette(cx);
    let open = drawer.read(cx).is_open(cx);
    let drawer = drawer.clone();
    div()
        .id("terminal-toggle")
        .size(px(28.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .when_some(open.then_some(hsla(p.accent)), |this, fill| this.bg(fill))
        .hover(move |style| style.bg(hsla(p.accent)))
        .tooltip(|window, cx| Tooltip::new(SharedString::from(with_shortcut("Toggle terminal", "terminal.toggle", cx))).build(window, cx))
        .on_click(move |_, window, cx| drawer.update(cx, |drawer, cx| drawer.toggle(window, cx)))
        .child(div().opacity(0.8).child(Icon::SquareTerminal.render(px(16.), hsla(p.foreground))))
}

/// `New Terminal (⌘N)`: the label with the command's current shortcut, as the web words them.
fn with_shortcut(name: &str, command: &str, cx: &App) -> String {
    match crate::keymap::label(command, cx) {
        Some(shortcut) => format!("{name} ({shortcut})"),
        None => name.to_owned(),
    }
}
