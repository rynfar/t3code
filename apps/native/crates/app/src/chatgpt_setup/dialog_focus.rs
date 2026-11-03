//! Nested modal keyboard scope for the ChatGPT picker, account wizard and welcome.
use gpui::{AnyElement, App, Div, FocusHandle, KeyBinding, ParentElement, Stateful, Window, actions, div, prelude::*};

actions!(chatgpt_dialog, [Next, Previous, Close]);
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", Next, Some("ChatGptDialog")),
        KeyBinding::new("shift-tab", Previous, Some("ChatGptDialog")),
        KeyBinding::new("escape", Close, Some("ChatGptDialog")),
    ]);
}
pub struct Scope {
    pub focus: FocusHandle,
    previous: Option<FocusHandle>,
    visible: bool,
}
impl Scope {
    pub fn new(cx: &mut App) -> Self {
        Self { focus: cx.focus_handle(), previous: None, visible: false }
    }
    pub fn sync(&mut self, visible: bool, window: &mut Window, cx: &mut App) {
        if visible && !self.visible {
            self.previous = window.focused(cx);
            window.focus(&self.focus, cx);
        }
        if !visible
            && self.visible
            && let Some(previous) = self.previous.take()
        {
            window.focus(&previous, cx);
        }
        self.visible = visible;
    }
    pub fn wrap(&self, body: AnyElement) -> Stateful<Div> {
        let next = self.focus.clone();
        let previous = self.focus.clone();
        div()
            .id("chatgpt-focus-scope")
            .key_context("ChatGptDialog")
            .on_action(move |_: &Next, window, cx| {
                cycle(&next, true, window, cx);
                cx.stop_propagation();
            })
            .on_action(move |_: &Previous, window, cx| {
                cycle(&previous, false, window, cx);
                cx.stop_propagation();
            })
            .child(body)
    }
}

fn cycle(focus: &FocusHandle, forward: bool, window: &mut Window, cx: &mut App) {
    for _ in 0..512 {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
        if focus.contains_focused(window, cx) && !focus.is_focused(window) {
            return;
        }
    }
    window.focus(focus, cx);
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::{Context,IntoElement,Render,Styled,TestAppContext,px};
    struct Harness {
        scope: Scope,
        outer: FocusHandle,
        first: FocusHandle,
        last: FocusHandle,
        open: bool,
    }
    impl Render for Harness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.scope.sync(self.open, window, cx);
            div().child(div().id("outside").track_focus(&self.outer).size(px(20.))).when(self.open, |v| {
                v.child(
                    self.scope.wrap(
                        gpui::deferred(
                            div()
                                .track_focus(&self.scope.focus)
                                .child(div().id("first").track_focus(&self.first).size(px(20.)))
                                .child(div().id("last").track_focus(&self.last).size(px(20.))),
                        )
                        .with_priority(65)
                        .into_any_element(),
                    ),
                )
            })
        }
    }
    #[gpui::test]
    fn tab_wraps_inside_the_nested_dialog_and_closing_restores_focus(cx: &mut TestAppContext) {
        cx.update(init);
        let window = cx.add_window(|_, cx| Harness { scope: Scope::new(cx), outer: cx.focus_handle().tab_stop(true), first: cx.focus_handle().tab_stop(true), last: cx.focus_handle().tab_stop(true), open: false });
        window.update(cx, |view, window, cx| window.focus(&view.outer, cx)).unwrap();
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| {
                view.open = true;
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        for key in ["tab", "tab", "tab", "shift-tab", "shift-tab", "shift-tab"] {
            cx.simulate_keystrokes(window.into(), key);
            cx.run_until_parked();
            window
                .update(cx, |view, window, cx| {
                    assert!(view.scope.focus.contains_focused(window, cx));
                    assert!(!view.outer.is_focused(window));
                    assert!(!view.scope.focus.is_focused(window));
                })
                .unwrap();
        }
        window
            .update(cx, |view, _, cx| {
                view.open = false;
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        window.update(cx, |view, window, _| assert!(view.outer.is_focused(window))).unwrap();
    }
}
