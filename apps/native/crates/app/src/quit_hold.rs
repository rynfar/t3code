//! Cmd+Q under the "Quit confirmation" setting: the desktop's quit-shortcut handler
//! (`apps/desktop/src/window/DesktopWindow.ts`, logic in `t3_core::settings::quit_hold`) and its
//! hint (`web/src/components/QuitHoldOverlay.tsx`). The workspace root listens in the capture
//! phase so auto-repeat (`is_held`) is visible; the app menu's Quit item still quits at once.

use std::time::{Duration, Instant};

use gpui::{App, Div, FontWeight, Global, InteractiveElement, IntoElement, ParentElement, Styled, Task, div, point, px, rems};
use t3_core::settings::quit_hold::{QuitEffect, QuitHint, QuitKeyDown, QuitShortcut};

use crate::theme::{hsla, palette};

const HOLD_HINT_LINGER: Duration = Duration::from_millis(1200);

struct QuitHold {
    started: Instant,
    shortcut: QuitShortcut,
    modifier_down: bool,
    visible: Option<QuitHint>,
    pressed: QuitHint,
    tick: Option<Task<()>>,
    linger: Option<Task<()>>,
}

impl Global for QuitHold {}

fn state(cx: &mut App) -> &mut QuitHold {
    if !cx.has_global::<QuitHold>() {
        cx.set_global(QuitHold {
            started: Instant::now(),
            shortcut: QuitShortcut::default(),
            modifier_down: false,
            visible: None,
            pressed: QuitHint::Hold,
            tick: None,
            linger: None,
        });
    }
    cx.global_mut::<QuitHold>()
}

fn now(cx: &mut App) -> u64 {
    state(cx).started.elapsed().as_millis() as u64
}

fn modifier(modifiers: gpui::Modifiers) -> bool {
    if cfg!(target_os = "macos") { modifiers.platform } else { modifiers.control }
}

/// Watches the quit shortcut on the workspace root.
pub fn listen(element: Div) -> Div {
    element
        .capture_key_down(|event, _, cx| {
            let keystroke = &event.keystroke;
            let input = QuitKeyDown {
                key: &keystroke.key,
                modifier: modifier(keystroke.modifiers),
                alt: keystroke.modifiers.alt,
                shift: keystroke.modifiers.shift,
                repeat: event.is_held,
            };
            let mode = crate::settings::client_settings(cx).confirm_quit();
            let at = now(cx);
            let (consumed, effects) = state(cx).shortcut.key_down(input, mode, at);
            if consumed {
                cx.stop_propagation();
            }
            apply(effects, cx);
        })
        .capture_key_up(|event, _, cx| {
            if event.keystroke.key.eq_ignore_ascii_case("q") {
                let effects = state(cx).shortcut.key_up();
                apply(effects, cx);
            }
        })
        .on_modifiers_changed(|event, _, cx| {
            let down = modifier(event.modifiers);
            let at = now(cx);
            let quit = state(cx);
            let released = quit.modifier_down && !down;
            quit.modifier_down = down;
            if released {
                let effects = quit.shortcut.modifier_released(at);
                apply(effects, cx);
            }
        })
}

fn apply(effects: Vec<QuitEffect>, cx: &mut App) {
    for effect in effects {
        match effect {
            QuitEffect::Quit => cx.quit(),
            QuitEffect::ShowHint(hint) => {
                let quit = state(cx);
                quit.linger = None;
                quit.pressed = hint;
                quit.visible = Some(hint);
                cx.refresh_windows();
            }
            QuitEffect::HideHint if state(cx).pressed == QuitHint::DoublePress => {
                state(cx).visible = None;
                cx.refresh_windows();
            }
            QuitEffect::HideHint => {
                let linger = cx.spawn(async move |cx| {
                    cx.background_executor().timer(HOLD_HINT_LINGER).await;
                    cx.update(|cx| {
                        state(cx).visible = None;
                        cx.refresh_windows();
                    });
                });
                state(cx).linger = Some(linger);
            }
        }
    }
    schedule_tick(cx);
}

fn schedule_tick(cx: &mut App) {
    let at = now(cx);
    let tick = state(cx).shortcut.deadline().map(|deadline| {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(deadline.saturating_sub(at))).await;
            cx.update(|cx| {
                let at = now(cx);
                let quit = state(cx);
                quit.tick = None;
                let effects = quit.shortcut.tick(at);
                apply(effects, cx);
            });
        })
    });
    state(cx).tick = tick;
}

/// The hint pill, 22% down the window, while a press is waiting for its hold or second press.
pub fn hint(cx: &mut App) -> Option<impl IntoElement> {
    let visible = cx.try_global::<QuitHold>()?.visible?;
    let p = palette(cx);
    let shortcut = if cfg!(target_os = "macos") { "\u{2318}Q" } else { "Ctrl+Q" };
    Some(gpui::deferred(
        div().absolute().top(gpui::relative(0.22)).left_0().right_0().flex().justify_center().child(
            div()
                .flex_none()
                .rounded_full()
                .px(rems(2.))
                .py(rems(1.))
                .bg(hsla(p.foreground).opacity(0.95))
                .text_color(hsla(p.background))
                .text_size(rems(1.5))
                .font_weight(FontWeight::BOLD)
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::black().opacity(0.25),
                    offset: point(px(0.), px(20.)),
                    blur_radius: px(25.),
                    spread_radius: px(-5.),
                    inset: false,
                }])
                .child(visible.message(shortcut)),
        ),
    )
    .with_priority(3))
}
