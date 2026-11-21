//! `SnapShotCoordinator` (`apps/web/src/components/desktop/SnapShotCoordinator.tsx`):
//! owns global capture shortcut lifecycle, permission refresh, capture execution, shutter
//! sound & flash feedback, target resolution (active thread / draft / last-used project /
//! pinned question draft), and attachment delivery via `Composer::attach_snapshot_png`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{
    AnyElement, App, Context, Entity, IntoElement, Styled, Subscription, Task, Window, div,
    prelude::*,
};
use t3_core::settings::snapshot::{
    CaptureTarget, ExistingTargetLookup, SnapShotBackendState, SnapShotShortcut,
    build_snapshot_source_json, capture_setup_access_ready, check_shortcut_availability,
    resolve_existing_snapshot_target, resolve_snapshot_attachment_target,
    snapshot_attachment_filename,
};

use super::{CapturedWindow, ShortcutListenerHandle, platform};
use crate::app_state::{AppEvent, AppState};
use crate::composer::Composer;
use crate::settings::store;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

const FLASH_DURATION: Duration = Duration::from_millis(180);

pub struct SnapShotCoordinator {
    state: Entity<AppState>,
    composer: Entity<Composer>,
    backend_state: SnapShotBackendState,
    shortcut_handle: Option<Box<dyn ShortcutListenerHandle>>,
    registered_shortcut: Option<SnapShotShortcut>,
    shortcut_suppressed: bool,
    shortcut_verified: bool,
    last_route_target: Option<CaptureTarget>,
    last_used_project_id: Option<String>,
    question_pins: HashMap<String, Option<String>>,
    capturing: bool,
    flash_visible: bool,
    flash_seq: u64,
    _flash_task: Option<Task<()>>,
    trigger_tx: mpsc::UnboundedSender<()>,
    _trigger_task: Task<()>,
    last_seen_settings: (bool, SnapShotShortcut, bool),
    _subscriptions: Vec<Subscription>,
}

impl Drop for SnapShotCoordinator {
    fn drop(&mut self) {
        if let Some(mut handle) = self.shortcut_handle.take() {
            handle.stop();
        }
    }
}

impl SnapShotCoordinator {
    pub fn new(
        state: Entity<AppState>,
        composer: Entity<Composer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (trigger_tx, mut trigger_rx) = mpsc::unbounded::<()>();
        let trigger_task = cx.spawn(async move |this, cx| {
            while trigger_rx.next().await.is_some() {
                let _ = this.update(cx, |this, cx| {
                    this.capture_now(true, cx);
                });
            }
        });

        let state_sub =
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, _, cx| {
                if matches!(event, AppEvent::Selection | AppEvent::Shell) {
                    this.record_active_route(cx);
                }
            });
        let prefs_sub = cx.observe(&state, |this, _, cx| {
            let doc = store::client(cx);
            let current = (
                doc.snap_shot_enabled(),
                doc.snap_shot_shortcut(),
                doc.snap_shot_include_accessibility(),
            );
            if current != this.last_seen_settings {
                this.last_seen_settings = current;
                this.refresh_state(cx);
            }
        });

        let doc = store::client(cx).clone();
        let initial_settings = (
            doc.snap_shot_enabled(),
            doc.snap_shot_shortcut(),
            doc.snap_shot_include_accessibility(),
        );
        let initial_backend = SnapShotBackendState::unsupported(doc.snap_shot_shortcut());
        let mut coordinator = Self {
            state,
            composer,
            backend_state: initial_backend,
            shortcut_handle: None,
            registered_shortcut: None,
            shortcut_suppressed: false,
            shortcut_verified: false,
            last_route_target: None,
            last_used_project_id: None,
            question_pins: HashMap::new(),
            capturing: false,
            flash_visible: false,
            flash_seq: 0,
            _flash_task: None,
            trigger_tx,
            _trigger_task: trigger_task,
            last_seen_settings: initial_settings,
            _subscriptions: vec![state_sub, prefs_sub],
        };
        coordinator.record_active_route(cx);
        coordinator.refresh_state(cx);
        coordinator
    }

    pub fn backend_state(&self) -> &SnapShotBackendState {
        &self.backend_state
    }

    pub fn render_flash_overlay(&self, cx: &App) -> Option<AnyElement> {
        if !self.flash_visible {
            return None;
        }
        let p = palette(cx);
        Some(
            div()
                .id("snapshot-capture-flash")
                .absolute()
                .inset_0()
                .border_2()
                .border_color(hsla(p.primary).opacity(0.65))
                .bg(hsla(p.foreground).opacity(0.08))
                .into_any_element(),
        )
    }

    fn record_active_route(&mut self, cx: &App) {
        let state = self.state.read(cx);
        let env_id = state.environment().unwrap_or("local").to_owned();
        if let Some(thread_id) = state.selected_thread.clone() {
            if let Some(project_id) = state
                .shell
                .snapshot
                .as_ref()
                .and_then(|snap| snap.threads.iter().find(|t| t.id == thread_id))
                .map(|t| t.project_id.clone())
            {
                self.last_used_project_id = Some(project_id);
            }
            self.last_route_target = Some(CaptureTarget::Thread { environment_id: env_id, thread_id });
        } else if let Some(draft) = state.active_draft() {
            self.last_used_project_id = Some(draft.project_id.clone());
            self.last_route_target = Some(CaptureTarget::Draft(draft));
        }
    }

    fn teardown_shortcut(&mut self) {
        if let Some(mut handle) = self.shortcut_handle.take() {
            handle.stop();
        }
        self.registered_shortcut = None;
    }

    pub fn set_shortcut_suppressed(&mut self, suppressed: bool, cx: &mut Context<Self>) {
        if self.shortcut_suppressed == suppressed {
            return;
        }
        self.shortcut_suppressed = suppressed;
        if suppressed {
            self.teardown_shortcut();
        }
        self.refresh_state(cx);
    }

    /// Re-reads platform permissions and synchronizes the global shortcut listener with the
    /// persisted `ClientSettingsDoc`.
    pub fn refresh_state(&mut self, cx: &mut Context<Self>) -> SnapShotBackendState {
        let doc = store::client(cx).clone();
        let shortcut = doc.snap_shot_shortcut();
        let enabled = doc.snap_shot_enabled();
        let include_ax = doc.snap_shot_include_accessibility();
        let plat = platform(cx);

        if !plat.is_supported() {
            self.teardown_shortcut();
            self.backend_state = SnapShotBackendState::unsupported(shortcut);
            cx.notify();
            return self.backend_state.clone();
        }

        let mac_permissions = plat.mac_permissions();
        let mut next = if let Some(perms) = mac_permissions {
            SnapShotBackendState::macos(
                shortcut.clone(),
                perms,
                include_ax,
                false,
                self.shortcut_verified,
            )
        } else {
            SnapShotBackendState {
                mode: t3_core::settings::snapshot::SnapShotMode::Direct,
                shortcut: shortcut.clone(),
                shortcut_registered: false,
                shortcut_pending: false,
                shortcut_verified: self.shortcut_verified,
                shortcut_label: None,
                shortcut_message: None,
                message: None,
                mac_permissions: None,
                windows: false,
            }
        };

        let should_register = enabled && !self.shortcut_suppressed && capture_setup_access_ready(&next);
        if should_register {
            let needs_rebind = self.shortcut_handle.is_none()
                || self.registered_shortcut.as_ref() != Some(&shortcut);
            if needs_rebind {
                self.teardown_shortcut();
                let availability = check_shortcut_availability(&shortcut, true, true);
                if !availability.available {
                    next.shortcut_registered = false;
                    next.shortcut_message = availability.message;
                } else {
                    let tx = self.trigger_tx.clone();
                    let on_trigger = Arc::new(move || {
                        let _ = tx.unbounded_send(());
                    });
                    match plat.register_global_shortcut(&shortcut, on_trigger) {
                        Ok(handle) => {
                            self.shortcut_handle = Some(handle);
                            self.registered_shortcut = Some(shortcut);
                            next.shortcut_registered = true;
                        }
                        Err(err) => {
                            next.shortcut_registered = false;
                            next.shortcut_message = Some(err);
                        }
                    }
                }
            } else {
                next.shortcut_registered = true;
            }
        } else {
            self.teardown_shortcut();
            next.shortcut_registered = false;
        }

        self.backend_state = next.clone();
        cx.notify();
        next
    }

    pub fn allow_screen_recording(&mut self, cx: &mut Context<Self>) {
        let plat = platform(cx);
        let _ = plat.request_screen_recording_permission();
        let _ = plat.open_screen_recording_settings();
        self.refresh_state(cx);
    }

    pub fn allow_accessibility(&mut self, cx: &mut Context<Self>) {
        let plat = platform(cx);
        let _ = plat.request_accessibility_permission();
        let _ = plat.open_accessibility_settings();
        self.refresh_state(cx);
    }

    /// Runs a setup test capture (`test-mac-capture` in `SnapShotSetupDialog.tsx`): verifies
    /// capture access and discards the test image.
    pub fn run_setup_test_capture(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let include_ax = store::client(cx).snap_shot_include_accessibility();
        let plat = platform(cx);
        let result = plat.capture_test_window(include_ax);
        self.refresh_state(cx);
        result.map(|_| ())
    }

    /// Triggers a live capture and delivers the resized PNG + optional `SnapShotSource`
    /// metadata to the active thread/draft composer (or a new draft in the last-used project).
    pub fn capture_now(&mut self, from_shortcut: bool, cx: &mut Context<Self>) {
        let doc = store::client(cx).clone();
        if from_shortcut && !doc.snap_shot_enabled() {
            return;
        }
        if self.capturing {
            return;
        }
        let plat = platform(cx);
        if !plat.is_supported() {
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    Toast::new(
                        ToastKind::Error,
                        crate::i18n::t(cx, "SnapShots capture is not supported on this platform.")
                            .to_owned(),
                    ),
                    cx,
                );
            });
            return;
        }

        self.capturing = true;
        if doc.snap_shot_play_sound() {
            plat.play_sound(doc.snap_shot_sound());
        }
        if doc.snap_shot_flash() {
            self.trigger_flash(cx);
        }

        // Snapshot current question attachment draft before capture begins so a later question
        // can never claim an earlier capture (`SnapShotCoordinator.tsx`).
        let open_question_draft = self.composer.read(cx).active_question_draft_key();
        let include_ax = doc.snap_shot_include_accessibility();
        cx.notify();

        let capture_task = cx
            .background_executor()
            .spawn(async move { plat.capture_frontmost_window(include_ax) });

        cx.spawn(async move |this, cx| {
            let captured = capture_task.await;
            let _ = this.update(cx, |this, cx| {
                this.capturing = false;
                if from_shortcut && captured.is_ok() {
                    this.shortcut_verified = true;
                }
                match captured {
                    Ok(window_capture) => {
                        this.deliver_capture(window_capture, open_question_draft.as_deref(), cx);
                    }
                    Err(err) => {
                        this.state.update(cx, |state, cx| {
                            state.show_toast(
                                Toast::new(
                                    ToastKind::Error,
                                    crate::i18n::t(cx, "Snapshot failed").to_owned(),
                                )
                                .description(err),
                                cx,
                            );
                        });
                    }
                }
                this.refresh_state(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn trigger_flash(&mut self, cx: &mut Context<Self>) {
        self.flash_seq = self.flash_seq.wrapping_add(1);
        let seq = self.flash_seq;
        self.flash_visible = true;
        cx.notify();
        self._flash_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH_DURATION).await;
            let _ = this.update(cx, |this, cx| {
                if this.flash_seq == seq {
                    this.flash_visible = false;
                    this._flash_task = None;
                    cx.notify();
                }
            });
        }));
    }

    fn resolve_or_create_target(&mut self, cx: &mut Context<Self>) -> Option<CaptureTarget> {
        self.record_active_route(cx);

        let existing = {
            let state = self.state.read(cx);
            let env_id = state.environment().unwrap_or("local");
            let route_thread = state.selected_thread.as_deref().map(|id| (env_id, id));
            let lookup = ExistingTargetLookup {
                route_thread,
                has_draft: &|draft| {
                    state.is_active_draft(draft) || (draft.environment_id.as_deref()
                        .is_none_or(|id| state.environments.active.as_deref() == Some(id))
                        && state.shell.project(&draft.project_id).is_some())
                },
                promoted_draft_thread: &|_| None,
                has_thread_shell: &|env, thread_id| {
                    env == env_id
                        && state
                            .shell
                            .snapshot
                            .as_ref()
                            .is_some_and(|s| s.threads.iter().any(|t| t.id == thread_id))
                },
            };
            self.last_route_target
                .as_ref()
                .and_then(|target| resolve_existing_snapshot_target(target, &lookup))
        };

        if let Some(target) = existing {
            match &target {
                CaptureTarget::Thread { thread_id, .. } => {
                    if self.state.read(cx).selected_thread.as_deref() != Some(thread_id.as_str()) {
                        let id = thread_id.clone();
                        self.state.update(cx, |state, cx| state.select_thread(&id, cx));
                    }
                }
                CaptureTarget::Draft(draft) => {
                    if !self.state.read(cx).is_active_draft(draft) {
                        self.state.update(cx, |state, cx| state.open_saved_draft(draft.clone(), cx));
                    }
                }
                CaptureTarget::Question { .. } => {}
            }
            return Some(target);
        }

        // Fall back to opening a new draft in the last-used project or default project.
        let project_id = self
            .last_used_project_id
            .clone()
            .filter(|id| {
                self.state
                    .read(cx)
                    .shell
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.projects.iter().any(|p| p.id == *id))
            })
            .or_else(|| crate::new_thread::default_project(self.state.read(cx)));

        let project_id = project_id?;
        self.last_used_project_id = Some(project_id.clone());
        self.state.update(cx, |state, cx| state.start_draft(&project_id, cx));
        let target = CaptureTarget::Draft(self.state.read(cx).active_draft()?);
        self.last_route_target = Some(target.clone());
        Some(target)
    }

    fn deliver_capture(
        &mut self,
        capture: CapturedWindow,
        open_question_draft: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(base_target) = self.resolve_or_create_target(cx) else {
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    Toast::new(
                        ToastKind::Error,
                        crate::i18n::t(cx, "Snapshot taken, but no project is available")
                            .to_owned(),
                    )
                    .description(
                        crate::i18n::t(cx, "Add a project, then capture the window again.")
                            .to_owned(),
                    ),
                    cx,
                );
            });
            return;
        };

        let resolved = resolve_snapshot_attachment_target(
            &mut self.question_pins,
            &capture.capture_id,
            base_target,
            open_question_draft,
        );
        let pinned_question = match &resolved {
            CaptureTarget::Question { draft_id } => Some(draft_id.as_str()),
            _ => None,
        };

        let source_json = build_snapshot_source_json(
            &capture.captured_at_iso,
            &capture.app_name,
            &capture.window_title,
            capture.app_identifier.as_deref(),
            capture.accessible_text.as_deref(),
            capture.accessible_text_truncated,
        );
        let filename = snapshot_attachment_filename(&capture.app_name, &capture.window_title);

        let attached = self.composer.update(cx, |composer, cx| {
            composer.attach_snapshot_png(
                &capture.capture_id,
                filename,
                capture.png_bytes,
                Some(source_json),
                pinned_question,
                cx,
            )
        });
        self.question_pins.remove(&capture.capture_id);

        if let Err(error) = attached {
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    Toast::new(
                        ToastKind::Error,
                        crate::i18n::t(cx, "Snapshot failed").to_owned(),
                    )
                    .description(error),
                    cx,
                );
            });
        }
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod gpui_tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use gpui::{Context, Entity, IntoElement, ParentElement, Render, Window};
    use serde_json::json;
    use t3_core::client_settings::ClientSettingChange;
    use t3_core::settings::snapshot::{
        MacSnapShotPermissions, SnapShotShortcut, SnapShotSound,
    };

    use super::*;
    use crate::snapshot_capture::{MockSnapShotPlatform, install_platform};

    struct CoordinatorHarness {
        composer: Entity<Composer>,
        coordinator: Entity<SnapShotCoordinator>,
    }

    impl Render for CoordinatorHarness {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            gpui::div().children(self.coordinator.read(cx).render_flash_overlay(cx))
        }
    }

    #[gpui::test]
    fn coordinator_registers_shortcut_captures_into_draft_and_tears_down_cleanly(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mock = Arc::new(MockSnapShotPlatform::macos_ready());
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            install_platform(mock.clone(), cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            state.update(cx, |state, cx| {
                if let Some(shell) = state.shell.snapshot.as_mut() {
                    shell.projects = vec![
                        serde_json::from_value(json!({
                            "id": "p-snap",
                            "title": "Snap Project",
                            "workspaceRoot": "/tmp/snap-project",
                            "defaultModelSelection": null,
                            "scripts": [],
                            "createdAt": "2026-11-20T00:00:00Z",
                            "updatedAt": "2026-11-20T00:00:00Z"
                        }))
                        .unwrap(),
                    ];
                }
                state.selected_thread = None;
                state.draft_project = None;
                cx.emit(AppEvent::Shell);
            });
            let mut scratch = t3_core::client_settings::ClientSettingsDoc::default();
            let mut patch = serde_json::Map::new();
            patch.extend(scratch.apply_change(&ClientSettingChange::SnapShotEnabled(true)));
            patch.extend(scratch.apply_change(&ClientSettingChange::SnapShotSound(
                SnapShotSound::CameraShutter,
            )));
            crate::settings::store::install_committed_client(&state, patch, cx);
        });

        let state_for_win = state.clone();
        let window = cx.add_window(|window, cx| {
            let composer = cx.new(|cx| Composer::new(state_for_win.clone(), window, cx));
            let coordinator = cx.new(|cx| {
                SnapShotCoordinator::new(state_for_win.clone(), composer.clone(), window, cx)
            });
            CoordinatorHarness { composer, coordinator }
        });
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();

        // Shortcut is registered when enabled and permissions are granted.
        assert_eq!(
            mock.registered_shortcuts.lock().unwrap().as_slice(),
            &[SnapShotShortcut::BothShiftKeys]
        );
        assert!(!mock.active_listener_stopped.load(Ordering::SeqCst));

        // Suppressing shortcut recording tears down the active listener cleanly.
        let coordinator = vcx.read(|cx| window.read(cx).unwrap().coordinator.clone());
        vcx.update(|_, cx| {
            coordinator.update(cx, |c, cx| c.set_shortcut_suppressed(true, cx));
        });
        assert!(mock.active_listener_stopped.load(Ordering::SeqCst));

        // Resuming re-registers the shortcut.
        vcx.update(|_, cx| {
            coordinator.update(cx, |c, cx| c.set_shortcut_suppressed(false, cx));
        });
        assert!(!mock.active_listener_stopped.load(Ordering::SeqCst));

        // Triggering the global shortcut captures the window, plays the sound, opens a new draft
        // in the available project, and attaches the PNG to the composer.
        mock.trigger_shortcut();
        vcx.run_until_parked();

        assert_eq!(
            mock.played_sounds.lock().unwrap().as_slice(),
            &[SnapShotSound::CameraShutter]
        );
        vcx.read(|cx| {
            assert_eq!(state.read(cx).draft_project.as_deref(), Some("p-snap"));
            assert!(coordinator.read(cx).backend_state().shortcut_verified);
        });

        // Capturing onto an active thread delivers the PNG attachment into the composer and
        // respects PROVIDER_SEND_TURN_MAX_ATTACHMENTS.
        let composer = vcx.read(|cx| window.read(cx).unwrap().composer.clone());
        let mut second_capture = mock.next_capture.lock().unwrap().clone().unwrap();
        second_capture.capture_id = "snap-test-2".to_owned();
        mock.set_next_capture(Ok(second_capture));
        vcx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.selected_thread = Some("t-snap".to_owned());
                state.draft_project = None;
                cx.emit(AppEvent::Selection);
            });
            coordinator.update(cx, |c, cx| c.capture_now(false, cx));
        });
        vcx.run_until_parked();
        vcx.read(|cx| {
            let staged = composer.read(cx).test_staged_thread_attachments("t-snap");
            assert_eq!(staged.len(), 1);
            assert_eq!(staged[0].mime_type, "image/png");
            assert_eq!(staged[0].name, "snapshot-t3-code-docs.png");
        });

        // When the active thread is deleted/cleared and `resolve_or_create_target` synchronously
        // opens a fallback draft before `Composer::refresh` runs, `attach_snapshot_png` must stage
        // onto the newly opened draft rather than the stale `composer.snapshot.target` thread.
        let mut third_capture = mock.next_capture.lock().unwrap().clone().unwrap();
        third_capture.capture_id = "snap-test-3".to_owned();
        third_capture.window_title = "Fallback Draft Capture".to_owned();
        mock.set_next_capture(Ok(third_capture));
        vcx.update(|_, cx| {
            state.update(cx, |state, _| {
                // Intentionally clear selected_thread without emitting Selection so Composer keeps
                // its stale `snapshot.target == Some(Thread("t-snap"))` when capture lands.
                state.selected_thread = None;
                state.draft_project = None;
            });
            coordinator.update(cx, |c, cx| c.capture_now(false, cx));
        });
        vcx.run_until_parked();
        vcx.read(|cx| {
            assert_eq!(state.read(cx).draft_project.as_deref(), Some("p-snap"));
            let thread_staged = composer.read(cx).test_staged_thread_attachments("t-snap");
            assert_eq!(thread_staged.len(), 1, "stale thread must not receive fallback capture");
            let draft_staged = composer.read(cx).test_staged_draft_attachments("p-snap");
            assert!(
                draft_staged
                    .iter()
                    .any(|att| att.name == "snapshot-fallback-draft-capture.png"),
                "fallback draft must receive the newly captured PNG"
            );
        });

        // Revoking screen recording permission tears down the shortcut and reports attention.
        mock.set_permissions(Some(MacSnapShotPermissions {
            screen_recording: false,
            accessibility: true,
        }));
        vcx.update(|_, cx| {
            coordinator.update(cx, |c, cx| {
                let st = c.refresh_state(cx);
                assert!(!st.shortcut_registered);
                assert!(st.message.is_some());
            });
        });
        assert!(mock.active_listener_stopped.load(Ordering::SeqCst));

        state.update(&mut vcx, |state, _| state.stop_pump());
        vcx.run_until_parked();
    }
}
