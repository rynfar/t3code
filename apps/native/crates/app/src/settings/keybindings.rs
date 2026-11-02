//! Keybindings: the environment's keymap as a searchable, grouped table
//! (`web/src/components/settings/KeybindingsSettings.tsx`). Read-only for now: recording and
//! editing shortcuts (`server.upsertKeybinding` / `server.removeKeybinding`) wait on the native
//! keymap (ledger KB-USER).

use gpui::{AppContext, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, Subscription, Window, div, prelude::*};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use t3_core::settings::keybindings;

use super::controls::{group, icon, muted_text, r, section, section_title};
use super::{Block, SettingsView};
use crate::composer::controls::kbd;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

pub(crate) struct KeybindingsState {
    pub(crate) filter: Entity<InputState>,
    _subscription: Subscription,
}

impl KeybindingsState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Search keybindings"));
        let subscription = cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        KeybindingsState { filter, _subscription: subscription }
    }
}

impl SettingsView {
    pub(super) fn keybindings_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let p = palette(cx);
        let query = self.keybindings.filter.read(cx).value().to_string();
        let rows = self
            .state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .map(|config| keybindings::rows(&config.keybindings, cfg!(target_os = "macos")))
            .unwrap_or_default();
        let search = section(
            "Keybindings",
            vec![h_flex()
                .px(r(16.))
                .py(r(12.))
                .child(
                    h_flex()
                        .w(r(320.))
                        .h(r(28.))
                        .px(r(9.))
                        .gap(r(6.))
                        .rounded(r(8.))
                        .border_1()
                        .border_color(hsla(p.input))
                        .child(icon(Icon::Search, 14., hsla(p.muted_foreground)))
                        .child(div().flex_1().child(Input::new(&self.keybindings.filter).appearance(false).small())),
                )
                .into_any_element()],
            cx,
        );
        let mut blocks = vec![Block::new(&["keybindings"], search)];
        let filtered = keybindings::filter(&rows, &query);
        if filtered.is_empty() {
            let message = if rows.is_empty() { "No keybindings yet." } else { "No keybindings match your search." };
            blocks.push(Block::new(&[], group(vec![div().px(r(16.)).py(r(48.)).flex().justify_center().child(muted_text(message, cx)).into_any_element()], cx)));
            return blocks;
        }
        for (title, members) in keybindings::groups(&filtered) {
            let anchors = members.iter().map(|row| SharedString::from(format!("keybinding-{}", row.command))).collect();
            let rows = members
                .into_iter()
                .map(|row| {
                    let anchor: SharedString = format!("keybinding-{}", row.command).into();
                    let highlighted = self.is_target(&anchor);
                    h_flex()
                        .id(gpui::ElementId::Name(anchor))
                        .px(r(16.))
                        .py(r(10.))
                        .gap(r(16.))
                        .rounded(r(12.))
                        .when(highlighted, |this| this.bg(hsla(p.accent).opacity(0.6)))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(r(2.))
                                .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(row.label.clone()))
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(if row.conditional { format!("{} \u{00b7} in some contexts", row.command) } else { row.command.clone() }),
                                ),
                        )
                        .child(h_flex().flex_none().gap(r(4.)).children(row.shortcuts.iter().map(|shortcut| kbd(shortcut.clone(), cx))))
                        .into_any_element()
                })
                .collect();
            blocks.push(Block::with_anchors(anchors, v_flex().gap(r(10.)).child(section_title(title)).child(group(rows, cx))));
        }
        blocks
    }
}
