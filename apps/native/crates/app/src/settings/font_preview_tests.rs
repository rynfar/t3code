//! Render the production previews under Settings' actual focus trap with harmless workspace actions.

use super::super::super::super::{
    CloseSettings, FocusNextInSettings, FocusPreviousInSettings, FocusSettingsSearch, SettingsView,
};
use super::*;
use gpui::{AppContext, Entity, TestAppContext, WindowHandle};
use gpui_base::input::{Input, Textarea};

struct PreviewHost {
    settings: Entity<SettingsView>,
    before: FocusHandle,
    after: FocusHandle,
    outside: FocusHandle,
    workspace_actions: usize,
    escapes: usize,
}

impl PreviewHost {
    fn terminal(&self, cx: &App) -> Entity<TerminalPreview> {
        self.settings.read(cx).typography.previews.terminal.clone()
    }

    fn prompt(&self, cx: &App) -> Entity<gpui_component::input::TextareaState> {
        self.settings.read(cx).typography.previews.prompt.clone()
    }
}

impl Render for PreviewHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (modal, search, prompt, terminal) = self.settings.update(cx, |settings, cx| {
            (
                settings.focus.clone(),
                settings.search.clone(),
                // The same editor engine avoids the chrome's AppKit content-type call in fake windows.
                super::super::prompt_surface(
                    Textarea::new(&settings.typography.previews.prompt),
                    cx,
                )
                .into_any_element(),
                settings.typography.previews.render(FontRole::Terminal, cx),
            )
        });
        div()
            .size_full()
            .key_context(crate::keymap::root_context(false, false))
            .on_action(
                cx.listener(|this, _: &crate::keymap::UndoThreadAction, _, _| {
                    this.workspace_actions += 1
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::keymap::CopyThreadReference, _, _| {
                    this.workspace_actions += 1
                }),
            )
            .on_action(cx.listener(|this, _: &crate::terminal::NewTerminal, _, _| {
                this.workspace_actions += 1
            }))
            .child(div().h(px(20.)).track_focus(&self.outside))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .key_context("SettingsModal")
                    .track_focus(&modal)
                    .on_action(cx.listener(|this, _: &FocusNextInSettings, window, cx| {
                        this.settings
                            .update(cx, |view, cx| view.cycle_focus(true, window, cx))
                    }))
                    .on_action(
                        cx.listener(|this, _: &FocusPreviousInSettings, window, cx| {
                            this.settings
                                .update(cx, |view, cx| view.cycle_focus(false, window, cx))
                        }),
                    )
                    .on_action(cx.listener(|this, _: &FocusSettingsSearch, window, cx| {
                        this.settings.update(cx, |view, cx| {
                            view.on_focus_search(&FocusSettingsSearch, window, cx)
                        })
                    }))
                    .on_action(cx.listener(|this, _: &CloseSettings, _, _| this.escapes += 1))
                    .on_key_down(cx.listener(|this, event, window, cx| {
                        this.settings
                            .update(cx, |view, cx| view.on_key_down(event, window, cx))
                    }))
                    .child(Input::new(&search))
                    .child(prompt)
                    .child(div().h(px(20.)).track_focus(&self.before))
                    .child(terminal)
                    .child(div().h(px(20.)).track_focus(&self.after)),
            )
    }
}

fn mount(cx: &mut TestAppContext) -> (tokio::runtime::Runtime, WindowHandle<PreviewHost>) {
    let home = std::env::temp_dir().join(format!("t3-font-preview-keys-{}", uuid::Uuid::new_v4()));
    t3_client::store::use_home(home);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let state = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        crate::keymap::init(cx);
        let state = cx.new(|cx| crate::app_state::AppState::new(runtime.handle().clone(), cx));
        crate::settings::init(state.clone(), cx);
        state
    });
    let window = cx.add_window(|window, cx| PreviewHost {
        settings: cx.new(|cx| {
            SettingsView::new(
                state,
                t3_core::settings::search::Section::Appearance,
                None,
                window,
                cx,
            )
        }),
        before: cx.focus_handle().tab_stop(true),
        after: cx.focus_handle().tab_stop(true),
        outside: cx.focus_handle().tab_stop(true),
        workspace_actions: 0,
        escapes: 0,
    });
    cx.run_until_parked();
    (runtime, window)
}

fn terminal_rows(terminal: &TerminalPreview) -> Vec<String> {
    let mut snapshot = t3_core::terminal::emulator::Snapshot::default();
    terminal.emulator.snapshot(&mut snapshot);
    (0..snapshot.rows)
        .map(|row| {
            snapshot
                .row(row)
                .iter()
                .map(|cell| cell.c)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[gpui::test]
fn rendered_previews_keep_keybindings_and_input_local(cx: &mut TestAppContext) {
    let (_runtime, window) = mount(cx);
    window
        .update(cx, |host, window, cx| {
            host.terminal(cx)
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx)
        })
        .unwrap();
    cx.simulate_input(window.into(), "src/test x");
    cx.simulate_keystrokes(window.into(), "backspace enter");
    cx.simulate_input(window.into(), "ready");
    window
        .update(cx, |host, _, cx| {
            let terminal = host.terminal(cx);
            let rows = terminal_rows(terminal.read(cx));
            assert!(rows[rows.len() - 2].ends_with("src/test"));
            assert!(rows.last().unwrap().ends_with("ready"));
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "cmd-z cmd-shift-c");
    window
        .update(cx, |host, _, _| {
            assert_eq!(
                host.workspace_actions, 0,
                "preview keys must not dispatch workspace actions"
            )
        })
        .unwrap();

    cx.update(|cx| {
        cx.bind_keys([
            gpui::KeyBinding::new("x", crate::keymap::UndoThreadAction, None),
            gpui::KeyBinding::new("g g", crate::keymap::UndoThreadAction, None),
            gpui::KeyBinding::new(
                "ctrl-shift-y",
                crate::terminal::NewTerminal,
                Some("Terminal"),
            ),
        ])
    });
    cx.simulate_input(window.into(), "xgg");
    cx.simulate_keystrokes(window.into(), "ctrl-shift-y");
    window
        .update(cx, |host, window, cx| {
            let rows = terminal_rows(host.terminal(cx).read(cx));
            assert!(rows.last().unwrap().ends_with("readyxgg"));
            assert_eq!(host.workspace_actions, 0);
            host.prompt(cx).read(cx).focus_handle(cx).focus(window, cx);
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "cmd-a");
    cx.simulate_input(window.into(), "apps/web xgg");
    cx.simulate_keystrokes(window.into(), "cmd-shift-c cmd-c cmd-x cmd-v");
    window
        .update(cx, |host, window, cx| {
            assert_eq!(host.workspace_actions, 0);
            assert_eq!(host.prompt(cx).read(cx).value().as_ref(), "apps/web xgg");
            assert!(host.prompt(cx).read(cx).focus_handle(cx).is_focused(window));
            assert!(host.settings.read(cx).query(cx).is_empty());
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "cmd-z");
    window
        .update(cx, |host, _, cx| {
            assert_ne!(host.prompt(cx).read(cx).value().as_ref(), "apps/web xgg");
            assert_eq!(host.workspace_actions, 0);
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "cmd-shift-z");
    window
        .update(cx, |host, window, cx| {
            assert_eq!(host.prompt(cx).read(cx).value().as_ref(), "apps/web xgg");
            host.outside.focus(window, cx);
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "cmd-z cmd-shift-c");
    window
        .update(cx, |host, _, _| {
            assert_eq!(
                host.workspace_actions, 2,
                "the boundary must not change shortcuts outside previews"
            )
        })
        .unwrap();
}

#[gpui::test]
fn rendered_terminal_preview_enters_and_leaves_settings_tab_order(cx: &mut TestAppContext) {
    let (_runtime, window) = mount(cx);
    window
        .update(cx, |host, window, cx| host.before.focus(window, cx))
        .unwrap();
    cx.simulate_keystrokes(window.into(), "tab");
    window
        .update(cx, |host, window, cx| {
            assert!(
                host.terminal(cx)
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "Tab must reach the local terminal preview"
            )
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "tab");
    window
        .update(cx, |host, window, _| assert!(host.after.is_focused(window)))
        .unwrap();
    cx.simulate_keystrokes(window.into(), "shift-tab");
    window
        .update(cx, |host, window, cx| {
            assert!(
                host.terminal(cx)
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            )
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "shift-tab");
    window
        .update(cx, |host, window, _| {
            assert!(host.before.is_focused(window))
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "tab escape");
    window
        .update(cx, |host, window, cx| {
            assert_eq!(host.escapes, 1);
            host.terminal(cx)
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "cmd-f");
    window
        .update(cx, |host, window, cx| {
            assert!(
                host.settings
                    .read(cx)
                    .search
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
            host.after.focus(window, cx);
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "tab");
    window
        .update(cx, |host, window, cx| {
            assert!(
                host.settings
                    .read(cx)
                    .search
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
            assert!(!host.outside.is_focused(window));
            assert_eq!(host.workspace_actions, 0);
        })
        .unwrap();
}
