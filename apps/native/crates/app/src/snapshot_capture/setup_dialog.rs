//! `SnapShotSetupDialog` (`apps/web/src/components/settings/SnapShotSetupDialog.tsx`):
//! two-step setup wizard (Access permissions + Shortcut setup) rendered as a workspace modal
//! overlay above Settings, plus shared SnapShot shortcut keycap and recording helpers.

use gpui::{
    AnyElement, App, Context, Entity, FontWeight, InteractiveElement, IntoElement, Keystroke,
    MouseButton, ParentElement, Render, SharedString, Styled, Subscription, Window, div,
    prelude::*,
};
use gpui_component::{h_flex, v_flex};
use t3_core::client_settings::ClientSettingChange;
use t3_core::keybindings::{decode_resolved_keybindings, parse_keybinding_shortcut};
use t3_core::settings::keybindings::{command_label, keybinding_from_keystroke};
use t3_core::settings::snapshot::{
    CaptureSetupStep, SnapShotBackendState, SnapShotModifier, SnapShotShortcut,
    capture_setup_access_ready, capture_setup_check_message, capture_setup_initial_step,
    capture_setup_mac_permissions_ready, capture_setup_shortcut_ready,
    capture_setup_should_disable_on_close, check_shortcut_availability,
    format_snap_shot_shortcut_label, same_snap_shot_shortcut, snap_shot_keybinding_conflict,
    snap_shot_shortcut_key_labels, snap_shot_shortcut_status,
};

use super::SnapShotCoordinator;
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant, icon, r};
use crate::settings::store;
use crate::theme::{hsla, palette};

const MAC: bool = cfg!(target_os = "macos");
const WINDOWS: bool = cfg!(target_os = "windows");

/// Renders keycap pills for a `SnapShotShortcut` (`SnapShotShortcutKeys.tsx`).
pub fn render_shortcut_keycaps(shortcut: &SnapShotShortcut, cx: &App) -> AnyElement {
    let p = palette(cx);
    let labels = snap_shot_shortcut_key_labels(shortcut, MAC, WINDOWS);
    h_flex()
        .items_center()
        .gap(r(3.))
        .children(labels.into_iter().map(|cap| {
            div()
                .px(r(5.))
                .py(r(1.))
                .rounded(r(4.))
                .border_1()
                .border_color(hsla(p.border))
                .bg(hsla(p.muted))
                .text_size(r(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.foreground))
                .child(SharedString::from(cap))
        }))
        .into_any_element()
}

/// Converts a GPUI `Keystroke` into a modified `SnapShotShortcut::KeyChord`, matching
/// `useSnapShotShortcutRecorder.tsx` (requiring at least one modifier key).
pub fn shortcut_from_keystroke(keystroke: &Keystroke, cx: &App) -> Option<SnapShotShortcut> {
    let m = keystroke.modifiers;
    if !m.platform && !m.control && !m.alt && !m.shift {
        return None;
    }
    let mapper = cx.keyboard_mapper().clone();
    let equivalents = mapper.get_key_equivalents();
    let input = keybinding_from_keystroke(
        &keystroke.key,
        m.platform,
        m.control,
        m.alt,
        m.shift,
        MAC,
        equivalents,
    )?;
    let chord = parse_keybinding_shortcut(&input)?;
    Some(SnapShotShortcut::KeyChord(chord))
}

/// Returns a conflict message if `candidate` conflicts with an existing app keybinding or
/// reserved system shortcut.
pub fn shortcut_conflict_message(
    candidate: &SnapShotShortcut,
    state: &Entity<AppState>,
    cx: &App,
) -> Option<String> {
    let bindings = state
        .read(cx)
        .server_config
        .config
        .as_ref()
        .map(|cfg| decode_resolved_keybindings(&cfg.keybindings))
        .unwrap_or_else(|| t3_core::keybindings::default_resolved_keybindings().to_vec());
    if let Some(command) = snap_shot_keybinding_conflict(candidate, &bindings, MAC) {
        let label = command_label(command);
        return Some(crate::i18n::t_fmt(
            cx,
            "T3 Code already uses this for \"{command}\".",
            &[("command", label.as_str().into())],
        ));
    }
    let avail = check_shortcut_availability(candidate, true, true);
    if !avail.available {
        return avail.message;
    }
    None
}

pub struct SnapShotSetupDialog {
    state: Entity<AppState>,
    coordinator: Entity<SnapShotCoordinator>,
    open: bool,
    step: CaptureSetupStep,
    was_enabled: bool,
    checked: bool,
    error: Option<String>,
    candidate: SnapShotShortcut,
    recording: bool,
    _subscriptions: Vec<Subscription>,
}

impl SnapShotSetupDialog {
    pub fn new(
        state: Entity<AppState>,
        coordinator: Entity<SnapShotCoordinator>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = cx.entity().downgrade();
        let key_sub = cx.intercept_keystrokes(move |event, _, cx| {
            let _ = view.update(cx, |this, cx| this.handle_intercepted_keystroke(&event.keystroke, cx));
        });
        let coord_sub = cx.observe(&coordinator, |_, _, cx| cx.notify());
        let saved = store::client(cx).snap_shot_shortcut();
        Self {
            state,
            coordinator,
            open: false,
            step: CaptureSetupStep::Access,
            was_enabled: false,
            checked: false,
            error: None,
            candidate: saved,
            recording: false,
            _subscriptions: vec![key_sub, coord_sub],
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self, requested: Option<CaptureSetupStep>, cx: &mut Context<Self>) {
        let backend = self.coordinator.update(cx, |c, cx| c.refresh_state(cx));
        let doc = store::client(cx).clone();
        self.was_enabled = doc.snap_shot_enabled();
        let effective_requested = if backend.mac_permissions.is_some() {
            Some(CaptureSetupStep::Access)
        } else {
            requested
        };
        let initial_step = capture_setup_initial_step(&backend, effective_requested);
        if !self.was_enabled && initial_step != CaptureSetupStep::Access {
            store::update_client(&self.state, ClientSettingChange::SnapShotEnabled(true), cx);
        }
        self.step = initial_step;
        self.checked = false;
        self.error = None;
        self.candidate = doc.snap_shot_shortcut();
        self.stop_recording(cx);
        self.open = true;
        cx.notify();
    }

    pub fn close(&mut self, completed: bool, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.stop_recording(cx);
        let currently_enabled = store::client(cx).snap_shot_enabled();
        if currently_enabled && capture_setup_should_disable_on_close(self.was_enabled, completed) {
            store::update_client(&self.state, ClientSettingChange::SnapShotEnabled(false), cx);
        }
        self.open = false;
        self.error = None;
        self.checked = false;
        self.coordinator.update(cx, |c, cx| {
            c.refresh_state(cx);
        });
        cx.notify();
    }

    pub fn handle_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.open {
            return false;
        }
        if self.recording {
            self.stop_recording(cx);
            return true;
        }
        self.close(false, cx);
        true
    }

    fn start_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = true;
        self.error = None;
        self.coordinator
            .update(cx, |c, cx| c.set_shortcut_suppressed(true, cx));
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if !self.recording {
            return;
        }
        self.recording = false;
        self.coordinator
            .update(cx, |c, cx| c.set_shortcut_suppressed(false, cx));
        cx.notify();
    }

    fn handle_intercepted_keystroke(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        if !self.open || !self.recording {
            return;
        }
        if keystroke.key == "tab" {
            return;
        }
        cx.stop_propagation();
        if keystroke.key == "escape" {
            self.stop_recording(cx);
            return;
        }
        if let Some(shortcut) = shortcut_from_keystroke(keystroke, cx) {
            self.candidate = shortcut;
            self.stop_recording(cx);
            cx.notify();
        }
    }

    fn test_capture_and_continue(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        let test_result = self.coordinator.update(cx, |c, cx| c.run_setup_test_capture(cx));
        if let Err(err) = test_result {
            self.error = Some(err);
            cx.notify();
            return;
        }
        if !store::client(cx).snap_shot_enabled() {
            store::update_client(&self.state, ClientSettingChange::SnapShotEnabled(true), cx);
        }
        let next = self.coordinator.update(cx, |c, cx| c.refresh_state(cx));
        if capture_setup_access_ready(&next) {
            self.step = CaptureSetupStep::Shortcut;
            self.checked = false;
        } else if let Some(msg) = next.message {
            self.error = Some(msg);
        }
        cx.notify();
    }

    fn save_shortcut(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(conflict) = shortcut_conflict_message(&self.candidate, &self.state, cx) {
            self.error = Some(conflict);
            cx.notify();
            return false;
        }
        store::update_client(
            &self.state,
            ClientSettingChange::SnapShotShortcut(self.candidate.clone()),
            cx,
        );
        let next = self.coordinator.update(cx, |c, cx| c.refresh_state(cx));
        if let Some(msg) = next.shortcut_message {
            self.error = Some(msg);
            cx.notify();
            return false;
        }
        self.error = None;
        cx.notify();
        next.shortcut_registered || next.shortcut_pending
    }

    #[allow(clippy::too_many_arguments)]
    fn render_permission_row(
        &self,
        id: &'static str,
        glyph: Icon,
        title: &'static str,
        description: String,
        granted: bool,
        on_allow: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        h_flex()
            .id(id)
            .w_full()
            .items_center()
            .justify_between()
            .gap(r(12.))
            .p(r(12.))
            .rounded(r(10.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.card))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(r(10.))
                    .child(
                        div()
                            .size(r(32.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(r(8.))
                            .bg(hsla(p.muted))
                            .child(icon(glyph, 16., hsla(p.foreground))),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(r(2.))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.foreground))
                                    .child(crate::i18n::t(cx, title).to_owned()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(description),
                            ),
                    ),
            )
            .child(if granted {
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(r(4.))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.success_foreground))
                    .child(icon(Icon::CircleCheck, 14., hsla(p.success_foreground)))
                    .child(crate::i18n::t(cx, "Allowed").to_owned())
                    .into_any_element()
            } else {
                let entity = cx.entity().downgrade();
                Button::new(
                    SharedString::from(format!("{id}-allow")),
                    ButtonVariant::Outline,
                    ButtonSize::Xs,
                )
                .icon(Icon::ExternalLink)
                .label("Open System Settings")
                .on_click(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| on_allow(this, cx));
                })
                .into_any_element()
            })
            .into_any_element()
    }
}

impl Render for SnapShotSetupDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let p = palette(cx);
        let doc = store::client(cx).clone();
        let saved_shortcut = doc.snap_shot_shortcut();
        let include_ax = doc.snap_shot_include_accessibility();
        let backend: SnapShotBackendState = self.coordinator.read(cx).backend_state().clone();
        let access_ready = capture_setup_access_ready(&backend);
        let mac_perms = backend.mac_permissions;
        let mac_perms_ready = capture_setup_mac_permissions_ready(&backend, include_ax);
        let shortcut_changed = !same_snap_shot_shortcut(&self.candidate, &saved_shortcut, MAC);
        let conflict = shortcut_changed
            .then(|| shortcut_conflict_message(&self.candidate, &self.state, cx))
            .flatten();
        let can_save_shortcut = shortcut_changed && conflict.is_none();
        let shortcut_ready = capture_setup_shortcut_ready(&backend, shortcut_changed);

        let title = match self.step {
            CaptureSetupStep::Access => crate::i18n::t(cx, "Allow snapshots").to_owned(),
            CaptureSetupStep::Shortcut => crate::i18n::t(cx, "Choose your shortcut").to_owned(),
        };
        let description = match self.step {
            CaptureSetupStep::Access => {
                if mac_perms.is_some() {
                    if mac_perms_ready {
                        crate::i18n::t(
                            cx,
                            "Test a snapshot of the current window. If macOS asks to bypass its window picker, choose Allow. The test image is discarded.",
                        )
                        .to_owned()
                    } else {
                        crate::i18n::t(cx, "Allow each permission, then continue.").to_owned()
                    }
                } else {
                    crate::i18n::t(
                        cx,
                        "Allow access when prompted to start capturing windows.",
                    )
                    .to_owned()
                }
            }
            CaptureSetupStep::Shortcut => {
                crate::i18n::t(cx, "Use both Shift keys, or record a different shortcut.")
                    .to_owned()
            }
        };

        let close_entity = cx.entity().downgrade();
        let close_btn_entity = cx.entity().downgrade();
        let back_entity = cx.entity().downgrade();
        let finish_later_entity = cx.entity().downgrade();
        let primary_entity = cx.entity().downgrade();

        let body: AnyElement = match self.step {
            CaptureSetupStep::Access => {
                let refresh_entity = cx.entity().downgrade();
                let mut rows = v_flex().gap(r(8.));
                if let Some(perms) = mac_perms {
                    rows = rows
                        .child(self.render_permission_row(
                            "snapshot-perm-screen-recording",
                            Icon::Monitor,
                            "Screen Recording",
                            crate::i18n::t(cx, "Capture the window you're using.").to_owned(),
                            perms.screen_recording,
                            |this, cx| {
                                this.coordinator.update(cx, |c, cx| c.allow_screen_recording(cx));
                                cx.notify();
                            },
                            cx,
                        ))
                        .child(self.render_permission_row(
                            "snapshot-perm-accessibility",
                            Icon::Eye,
                            "Accessibility",
                            if include_ax {
                                crate::i18n::t(
                                    cx,
                                    "Include text and controls from the captured app.",
                                )
                                .to_owned()
                            } else {
                                crate::i18n::t(
                                    cx,
                                    "Optional. Include text and controls from the captured app.",
                                )
                                .to_owned()
                            },
                            perms.accessibility,
                            |this, cx| {
                                this.coordinator.update(cx, |c, cx| c.allow_accessibility(cx));
                                cx.notify();
                            },
                            cx,
                        ));
                }
                v_flex()
                    .gap(r(12.))
                    .child(rows)
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap(r(8.))
                            .child(
                                Button::new(
                                    "snapshot-setup-check-status",
                                    ButtonVariant::GhostMuted,
                                    ButtonSize::Xs,
                                )
                                .icon(Icon::RefreshCw)
                                .label("Check again")
                                .on_click(move |_, _, cx| {
                                    let _ = refresh_entity.update(cx, |this, cx| {
                                        this.coordinator.update(cx, |c, cx| {
                                            c.refresh_state(cx);
                                        });
                                        this.checked = true;
                                        cx.notify();
                                    });
                                }),
                            )
                            .when(self.checked, |el| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(
                                            crate::i18n::t(
                                                cx,
                                                capture_setup_check_message(&backend),
                                            )
                                            .to_owned(),
                                        ),
                                )
                            }),
                    )
                    .into_any_element()
            }
            CaptureSetupStep::Shortcut => {
                let record_entity = cx.entity().downgrade();
                let test_entity = cx.entity().downgrade();
                let status_text = if self.recording {
                    Some(crate::i18n::t(cx, "Press your shortcut. Esc cancels.").to_owned())
                } else if let Some(conflict_msg) = conflict.clone() {
                    Some(conflict_msg)
                } else if shortcut_changed {
                    Some(crate::i18n::t(cx, "Ready to save.").to_owned())
                } else {
                    snap_shot_shortcut_status(Some(&backend))
                        .map(|s| crate::i18n::t(cx, &s).to_owned())
                };
                let preset_buttons = h_flex().flex_wrap().gap(r(6.)).children(
                    SnapShotModifier::ALL.iter().copied().map(|modifier| {
                        let preset = SnapShotShortcut::from_modifier_pair(modifier);
                        let selected = same_snap_shot_shortcut(&self.candidate, &preset, MAC);
                        let preset_entity = cx.entity().downgrade();
                        let label = modifier.pair_label(MAC);
                        Button::new(
                            SharedString::from(format!("snapshot-setup-preset-{}", modifier.as_str())),
                            if selected {
                                ButtonVariant::Secondary
                            } else {
                                ButtonVariant::Outline
                            },
                            ButtonSize::Xs,
                        )
                        .label(label)
                        .on_click(move |_, _, cx| {
                            let _ = preset_entity.update(cx, |this, cx| {
                                this.stop_recording(cx);
                                this.candidate = preset.clone();
                                this.error = None;
                                cx.notify();
                            });
                        })
                    }),
                );

                v_flex()
                    .gap(r(12.))
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap(r(8.))
                            .p(r(12.))
                            .rounded(r(10.))
                            .border_1()
                            .border_color(hsla(p.border))
                            .bg(hsla(p.card))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(r(8.))
                                    .child(icon(Icon::Keyboard, 15., hsla(p.muted_foreground)))
                                    .child(if self.recording {
                                        div()
                                            .text_xs()
                                            .text_color(hsla(p.muted_foreground))
                                            .child(
                                                crate::i18n::t(cx, "Press shortcut\u{2026}")
                                                    .to_owned(),
                                            )
                                            .into_any_element()
                                    } else {
                                        render_shortcut_keycaps(&self.candidate, cx)
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap(r(6.))
                                    .child(
                                        Button::new(
                                            "snapshot-setup-record-shortcut",
                                            if self.recording {
                                                ButtonVariant::Secondary
                                            } else {
                                                ButtonVariant::Outline
                                            },
                                            ButtonSize::Xs,
                                        )
                                        .label(if self.recording {
                                            "Cancel"
                                        } else {
                                            "Record shortcut"
                                        })
                                        .on_click(move |_, _, cx| {
                                            let _ = record_entity.update(cx, |this, cx| {
                                                if this.recording {
                                                    this.stop_recording(cx);
                                                } else {
                                                    this.start_recording(cx);
                                                }
                                            });
                                        }),
                                    )
                                    .child(
                                        Button::new(
                                            "snapshot-setup-test-capture",
                                            ButtonVariant::GhostMuted,
                                            ButtonSize::Xs,
                                        )
                                        .label("Test capture")
                                        .on_click(move |_, _, cx| {
                                            let _ = test_entity.update(cx, |this, cx| {
                                                match this
                                                    .coordinator
                                                    .update(cx, |c, cx| c.run_setup_test_capture(cx))
                                                {
                                                    Ok(()) => {
                                                        this.error = None;
                                                        this.checked = true;
                                                    }
                                                    Err(err) => this.error = Some(err),
                                                }
                                                cx.notify();
                                            });
                                        }),
                                    ),
                            ),
                    )
                    .child(preset_buttons)
                    .when_some(status_text, |el, msg| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(if conflict.is_some() {
                                    hsla(p.destructive_foreground)
                                } else {
                                    hsla(p.muted_foreground)
                                })
                                .child(msg),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(format!(
                                "{}: {}",
                                crate::i18n::t(cx, "Capture shortcut"),
                                format_snap_shot_shortcut_label(&self.candidate, MAC)
                            )),
                    )
                    .into_any_element()
            }
        };

        let step_index = match self.step {
            CaptureSetupStep::Access => 0,
            CaptureSetupStep::Shortcut => 1,
        };
        let step_pills = h_flex().gap(r(8.)).children(
            [("Access", 0usize), ("Shortcut", 1usize)].into_iter().map(|(label, idx)| {
                let active = idx == step_index;
                let completed = idx < step_index;
                h_flex()
                    .items_center()
                    .gap(r(6.))
                    .px(r(8.))
                    .py(r(4.))
                    .rounded(r(999.))
                    .bg(if active {
                        hsla(p.primary).opacity(0.14)
                    } else {
                        hsla(p.muted)
                    })
                    .text_xs()
                    .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                    .text_color(if active {
                        hsla(p.primary)
                    } else if completed {
                        hsla(p.foreground)
                    } else {
                        hsla(p.muted_foreground)
                    })
                    .child(SharedString::from(format!("{}. {}", idx + 1, crate::i18n::t(cx, label))))
            }),
        );

        let primary_label: &'static str = match self.step {
            CaptureSetupStep::Access => {
                if mac_perms.is_some() {
                    "Test capture and continue"
                } else {
                    "Continue"
                }
            }
            CaptureSetupStep::Shortcut => {
                if shortcut_changed {
                    "Save and finish"
                } else {
                    "Done"
                }
            }
        };
        let primary_disabled = match self.step {
            CaptureSetupStep::Access => !mac_perms_ready,
            CaptureSetupStep::Shortcut => {
                !access_ready || if shortcut_changed { !can_save_shortcut } else { !shortcut_ready }
            }
        };

        div()
            .id("snapshot-setup-dialog-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(p.dialog_backdrop))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                let _ = close_entity.update(cx, |this, cx| this.close(false, cx));
            })
            .child(
                v_flex()
                    .id("snapshot-setup-dialog")
                    .w(r(480.))
                    .max_w_full()
                    .rounded(r(16.))
                    .border_1()
                    .border_color(hsla(p.dialog_border))
                    .bg(hsla(p.dialog_surface))
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        v_flex()
                            .gap(r(10.))
                            .p(r(20.))
                            .border_b_1()
                            .border_color(hsla(p.border))
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(hsla(p.foreground))
                                            .child(
                                                crate::i18n::t(cx, "Set up snapshots").to_owned(),
                                            ),
                                    )
                                    .child(
                                        Button::new(
                                            "snapshot-setup-close-x",
                                            ButtonVariant::GhostMuted,
                                            ButtonSize::IconXs,
                                        )
                                        .icon(Icon::X)
                                        .on_click(move |_, _, cx| {
                                            let _ = close_btn_entity
                                                .update(cx, |this, cx| this.close(false, cx));
                                        }),
                                    ),
                            )
                            .child(step_pills),
                    )
                    .child(
                        v_flex()
                            .gap(r(14.))
                            .p(r(20.))
                            .child(
                                v_flex()
                                    .gap(r(4.))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(hsla(p.foreground))
                                            .child(title),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(hsla(p.muted_foreground))
                                            .child(description),
                                    ),
                            )
                            .child(body)
                            .when_some(self.error.clone(), |el, err| {
                                el.child(
                                    h_flex()
                                        .items_center()
                                        .gap(r(6.))
                                        .p(r(10.))
                                        .rounded(r(8.))
                                        .bg(hsla(p.error_surface))
                                        .text_xs()
                                        .text_color(hsla(p.destructive_foreground))
                                        .child(icon(
                                            Icon::CircleAlert,
                                            14.,
                                            hsla(p.destructive_foreground),
                                        ))
                                        .child(err),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .justify_end()
                            .gap(r(8.))
                            .px(r(20.))
                            .py(r(14.))
                            .border_t_1()
                            .border_color(hsla(p.border))
                            .bg(hsla(p.muted).opacity(0.5))
                            .when(self.step != CaptureSetupStep::Access, |el| {
                                el.child(
                                    Button::new(
                                        "snapshot-setup-back",
                                        ButtonVariant::Ghost,
                                        ButtonSize::Sm,
                                    )
                                    .label("Back")
                                    .on_click(move |_, _, cx| {
                                        let _ = back_entity.update(cx, |this, cx| {
                                            this.stop_recording(cx);
                                            this.step = CaptureSetupStep::Access;
                                            this.checked = false;
                                            cx.notify();
                                        });
                                    }),
                                )
                            })
                            .child(
                                Button::new(
                                    "snapshot-setup-cancel",
                                    ButtonVariant::Ghost,
                                    ButtonSize::Sm,
                                )
                                .label(if self.was_enabled { "Close" } else { "Finish later" })
                                .on_click(move |_, _, cx| {
                                    let _ = finish_later_entity
                                        .update(cx, |this, cx| this.close(false, cx));
                                }),
                            )
                            .child(
                                Button::new(
                                    "snapshot-setup-primary",
                                    ButtonVariant::Default,
                                    ButtonSize::Sm,
                                )
                                .label(primary_label)
                                .disabled(primary_disabled)
                                .on_click(move |_, _, cx| {
                                    let _ = primary_entity.update(cx, |this, cx| match this.step {
                                        CaptureSetupStep::Access => {
                                            this.test_capture_and_continue(cx);
                                        }
                                        CaptureSetupStep::Shortcut => {
                                            let saved = store::client(cx).snap_shot_shortcut();
                                            let changed = !same_snap_shot_shortcut(
                                                &this.candidate,
                                                &saved,
                                                MAC,
                                            );
                                            if !changed || this.save_shortcut(cx) {
                                                this.close(true, cx);
                                            }
                                        }
                                    });
                                }),
                            ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod gpui_tests {
    use std::sync::Arc;

    use gpui::{Context, Entity, IntoElement, ParentElement, Render, Window};
    use t3_core::settings::snapshot::MacSnapShotPermissions;

    use super::*;
    use crate::composer::Composer;
    use crate::snapshot_capture::{MockSnapShotPlatform, install_platform};

    struct SetupHarness {
        dialog: Entity<SnapShotSetupDialog>,
    }

    impl Render for SetupHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            gpui::div().child(self.dialog.clone())
        }
    }

    #[gpui::test]
    fn setup_dialog_walks_permissions_test_capture_and_shortcut_completion(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mock = Arc::new(MockSnapShotPlatform::macos_ready());
        mock.set_permissions(Some(MacSnapShotPermissions {
            screen_recording: false,
            accessibility: false,
        }));
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            install_platform(mock.clone(), cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
        });

        let state_for_win = state.clone();
        let window = cx.add_window(|window, cx| {
            let composer = cx.new(|cx| Composer::new(state_for_win.clone(), window, cx));
            let coordinator = cx.new(|cx| {
                SnapShotCoordinator::new(state_for_win.clone(), composer.clone(), window, cx)
            });
            let dialog = cx.new(|cx| {
                SnapShotSetupDialog::new(state_for_win.clone(), coordinator.clone(), window, cx)
            });
            SetupHarness { dialog }
        });
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let dialog = vcx.read(|cx| window.read(cx).unwrap().dialog.clone());

        vcx.update(|window, cx| {
            dialog.update(cx, |d, cx| d.open(None, cx));
            window.refresh();
            window.draw(cx).clear();
        });
        vcx.read(|cx| {
            let d = dialog.read(cx);
            assert!(d.is_open());
            assert_eq!(d.step, CaptureSetupStep::Access);
        });

        // Grant both permissions, run test capture & advance to Shortcut step.
        mock.set_permissions(Some(MacSnapShotPermissions {
            screen_recording: true,
            accessibility: true,
        }));
        vcx.update(|window, cx| {
            dialog.update(cx, |d, cx| d.test_capture_and_continue(cx));
            window.refresh();
            window.draw(cx).clear();
        });
        vcx.read(|cx| {
            let d = dialog.read(cx);
            assert_eq!(d.step, CaptureSetupStep::Shortcut);
            assert!(store::client(cx).snap_shot_enabled());
        });

        // Finishing keeps SnapShots enabled.
        vcx.update(|_, cx| {
            dialog.update(cx, |d, cx| d.close(true, cx));
        });
        vcx.read(|cx| {
            assert!(!dialog.read(cx).is_open());
            assert!(store::client(cx).snap_shot_enabled());
        });

        state.update(&mut vcx, |state, _| state.stop_pump());
        vcx.run_until_parked();
    }
}
