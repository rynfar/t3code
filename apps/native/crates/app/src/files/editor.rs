//! In-place source editor, debounced/explicit save coordinator (`projects.writeFile`),
//! `filesystem:write` scope recovery, and unsaved-dismissal guard.
//! Ports `EditableFileSurface` in `FilePreviewPanel.tsx`, `fileSaveCoordinator.ts`,
//! `fileSaveSessions.ts`, `fileEditorDismissal.ts`, and `fileContentRevision.ts`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    App, AppContext, Context, Entity, Focusable, FontWeight, IntoElement, KeyBinding, ParentElement, SharedString,
    Styled, Subscription, Task, Window, actions, div, prelude::*, px,
};
use gpui_base::input::EditorState;
use gpui_component::input::InputEvent;
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::session::RpcError;
use t3_core::files::save::{
    DEFAULT_SAVE_DEBOUNCE_MS, DismissalOutcome, FileSaveSessionStore, FileSaveTarget, SaveEffect,
    TargetedSaveEffect, decide_escape_dismissal, decide_leave_editor,
};
use t3_core::files::{WRITE_FILE, WriteFailure, decode_write_file, write_file_request};

use super::preview::{Preview, Target};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

pub(super) const CONTEXT: &str = "FileEditor";

actions!(file_editor, [SaveFile, DismissEditor]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-s", SaveFile, Some(CONTEXT)),
        KeyBinding::new("escape", DismissEditor, Some(CONTEXT)),
    ]);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PendingDismiss {
    ExitEditMode,
}

/// Manages in-place editing, autosave/Cmd+S, write-scope watching, and unsaved-dismissal prompts.
pub struct FileEditorController {
    pub(super) editing: bool,
    pub(super) target: Option<FileSaveTarget>,
    /// Text loaded into `editor` when entering edit mode or switching targets.
    loaded_text: String,
    /// Last confirmed server text (for Discard restoration).
    server_text: String,
    language: Option<&'static str>,
    wrap: bool,
    pub(super) store: FileSaveSessionStore,
    pub(super) save_error: Option<String>,
    pub(super) pending_dismiss: Option<PendingDismiss>,
    pub(super) can_write: bool,
    editor: Option<Entity<EditorState>>,
    /// Suppresses `InputEvent::Change` callbacks caused by programmatic `set_value`.
    syncing_value: bool,
    needs_sync: bool,
    needs_focus: bool,
    _input_sub: Option<Subscription>,
    _debounce_task: Option<Task<()>>,
    pub(super) _persist_task: Option<Task<()>>,
    _grants_task: Option<Task<()>>,
}

impl FileEditorController {
    pub fn new() -> Self {
        Self {
            editing: false,
            target: None,
            loaded_text: String::new(),
            server_text: String::new(),
            language: None,
            wrap: true,
            store: FileSaveSessionStore::new(DEFAULT_SAVE_DEBOUNCE_MS),
            save_error: None,
            pending_dismiss: None,
            can_write: false,
            editor: None,
            syncing_value: false,
            needs_sync: false,
            needs_focus: false,
            _input_sub: None,
            _debounce_task: None,
            _persist_task: None,
            _grants_task: None,
        }
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn is_dirty(&self) -> bool {
        self.target.as_ref().is_some_and(|t| self.store.is_pending(t))
    }

    pub fn is_saving(&self) -> bool {
        self.target.as_ref().is_some_and(|t| self.store.is_saving(t))
    }

    #[cfg(test)]
    pub fn has_unsaved_prompt(&self) -> bool {
        self.pending_dismiss.is_some()
    }

    pub fn optimistic_for(&self, target: &FileSaveTarget) -> Option<&str> {
        self.store.optimistic_contents(target)
    }

    pub fn clear_confirmed_if_matching(&mut self, target: &FileSaveTarget, contents: &str) {
        self.store.clear_confirmed_if_matching(target, contents);
    }

    pub fn set_wrap(&mut self, wrap: bool) {
        self.wrap = wrap;
    }

    pub fn set_server_text(&mut self, text: String) {
        self.server_text = text;
    }
}

pub(super) fn target_to_save_target(target: &Target) -> FileSaveTarget {
    FileSaveTarget::new(&target.root.environment, &target.root.workspace.cwd, &target.path)
}

pub(super) fn check_can_write(state: &AppState) -> bool {
    state.client_handle().is_some_and(|client| {
        client
            .session_state()
            .borrow()
            .as_ref()
            .is_some_and(|session| session.grants(t3_core::auth::FILESYSTEM_WRITE_SCOPE))
    })
}

impl Preview {
    pub(super) fn refresh_write_scope(&mut self, cx: &mut Context<Self>) {
        let can_write = check_can_write(self.state.read(cx));
        let prev = self.editor.can_write;
        self.editor.can_write = can_write;
        if can_write && !prev {
            let env = self.state.read(cx).environment().map(str::to_owned);
            if let Some(env) = env {
                let effects = self.editor.store.on_writable_regained(&env, now_ms());
                self.run_save_effects(effects, cx);
            }
        }
        if self.editor._grants_task.is_none()
            && let Some(client) = self.state.read(cx).client_handle()
        {
            let mut grants = client.session_state();
            self.editor._grants_task = Some(cx.spawn(async move |this, cx| {
                while grants.changed().await.is_ok() {
                    let ok = this.update(cx, |this, cx| {
                        this.refresh_write_scope(cx);
                        cx.notify();
                    });
                    if ok.is_err() {
                        break;
                    }
                }
            }));
        }
    }

    pub fn enter_edit_mode(&mut self, cx: &mut Context<Self>) {
        if !self.can_edit_current(cx) {
            return;
        }
        self.refresh_write_scope(cx);
        let Some(target) = self.target.clone() else { return };
        let Some(server_contents) = self.current_text_contents() else { return };
        let save_target = target_to_save_target(&target);
        let initial = self
            .editor
            .store
            .optimistic_contents(&save_target)
            .map(str::to_owned)
            .unwrap_or_else(|| server_contents.clone());
        let can_write = self.editor.can_write;
        let effects = self.editor.store.acquire(&save_target, can_write, now_ms());
        self.editor.target = Some(save_target);
        self.editor.server_text = server_contents;
        self.editor.loaded_text = initial;
        self.editor.language = t3_core::files::syntax_language(&target.path);
        self.editor.editing = true;
        self.editor.pending_dismiss = None;
        self.editor.save_error = None;
        self.editor.needs_sync = true;
        self.editor.needs_focus = true;
        self.run_save_effects(effects, cx);
        cx.notify();
    }

    pub fn request_exit_edit_mode(&mut self, cx: &mut Context<Self>) {
        if !self.editor.editing {
            return;
        }
        match decide_leave_editor(self.editor.is_dirty(), self.editor.is_saving()) {
            DismissalOutcome::DismissNow | DismissalOutcome::Ignore => {
                self.finish_exit_edit_mode(cx);
            }
            DismissalOutcome::PromptUnsaved => {
                self.editor.pending_dismiss = Some(PendingDismiss::ExitEditMode);
                cx.notify();
            }
        }
    }

    pub fn save_active_file(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.editor.target.clone() else { return };
        self.refresh_write_scope(cx);
        let can_write = self.editor.can_write;
        if !can_write {
            self.editor.save_error = Some("Read-only connection. Unsaved edits are kept until write access returns.".to_owned());
            cx.notify();
            return;
        }
        self.editor.save_error = None;
        let effects = self.editor.store.flush(&target, can_write);
        self.run_save_effects(effects, cx);
        cx.notify();
    }

    pub fn discard_unsaved_and_exit(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.editor.target.clone() {
            self.editor.store.discard_unsaved(&target);
            self.editor.loaded_text = self.editor.server_text.clone();
            self.editor.needs_sync = true;
        }
        self.editor.save_error = None;
        self.editor.pending_dismiss = None;
        self.finish_exit_edit_mode(cx);
    }

    pub fn save_and_exit_edit_mode(&mut self, cx: &mut Context<Self>) {
        self.editor.pending_dismiss = Some(PendingDismiss::ExitEditMode);
        self.save_active_file(cx);
        if !self.editor.is_dirty() && !self.editor.is_saving() {
            self.finish_exit_edit_mode(cx);
        }
    }

    pub fn cancel_dismiss_prompt(&mut self, cx: &mut Context<Self>) {
        self.editor.pending_dismiss = None;
        cx.notify();
    }

    pub(super) fn release_active_editor_on_target_change(&mut self, cx: &mut Context<Self>) {
        if !self.editor.editing {
            return;
        }
        if let Some(target) = self.editor.target.take() {
            let can_write = self.editor.can_write;
            let effects = self.editor.store.release(&target, can_write);
            self.run_save_effects(effects, cx);
        }
        self.editor.editing = false;
        self.editor.pending_dismiss = None;
        self.editor.save_error = None;
        self.editor._debounce_task = None;
    }

    fn finish_exit_edit_mode(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.editor.target.take() {
            let can_write = self.editor.can_write;
            let effects = self.editor.store.release(&target, can_write);
            self.run_save_effects(effects, cx);
        }
        self.editor.editing = false;
        self.editor.pending_dismiss = None;
        self.editor._debounce_task = None;
        cx.notify();
    }

    pub(super) fn on_editor_text_changed(&mut self, next_text: String, cx: &mut Context<Self>) {
        if self.editor.syncing_value || !self.editor.editing {
            return;
        }
        let Some(target) = self.editor.target.clone() else { return };
        self.editor.loaded_text = next_text.clone();
        self.editor.save_error = None;
        let effects = self.editor.store.change(&target, next_text, now_ms());
        self.run_save_effects(effects, cx);
        cx.notify();
    }

    fn run_save_effects(&mut self, effects: Vec<TargetedSaveEffect>, cx: &mut Context<Self>) {
        for targeted in effects {
            let target = targeted.target;
            match targeted.effect {
                SaveEffect::CancelDebounce => {
                    self.editor._debounce_task = None;
                }
                SaveEffect::ScheduleDebounce { delay_ms, token } => {
                    self.editor._debounce_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(delay_ms)).await;
                        let _ = this.update(cx, |this, cx| {
                            let can_write = this.editor.can_write;
                            let next = this.editor.store.on_debounce_fired(&target, token, can_write);
                            this.run_save_effects(next, cx);
                            cx.notify();
                        });
                    }));
                }
                SaveEffect::Persist { revision, contents } => {
                    self.persist_file(target, revision, contents, cx);
                }
                SaveEffect::PendingChanged(false) => {
                    if self.editor.pending_dismiss == Some(PendingDismiss::ExitEditMode)
                        && !self.editor.is_dirty()
                        && !self.editor.is_saving()
                    {
                        self.finish_exit_edit_mode(cx);
                    }
                }
                SaveEffect::PendingChanged(true) | SaveEffect::Settled => {}
            }
        }
    }

    fn persist_file(
        &mut self,
        target: FileSaveTarget,
        revision: u64,
        contents: String,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else {
            let next = self.editor.store.on_persist_result(&target, revision, &contents, false, false, now_ms());
            self.editor.save_error = Some("Not connected to the server.".to_owned());
            self.run_save_effects(next, cx);
            return;
        };
        let payload = write_file_request(&target.cwd, &target.relative_path, &contents);
        let request = state.runtime().spawn(async move {
            match client.request(WRITE_FILE, payload).await {
                Ok(value) => decode_write_file(&value).map_err(WriteFailure::message),
                Err(RpcError::Failure(failure)) => Err(WriteFailure::from_error(&failure.error, &failure.message)),
                Err(RpcError::Closed) => Err(WriteFailure::message("Not connected to the server.")),
            }
        });
        let saved_contents = contents.clone();
        self.editor._persist_task = Some(cx.spawn(async move |this, cx| {
            let result = request
                .await
                .unwrap_or_else(|_| Err(WriteFailure::message("The file could not be saved.")));
            let _ = this.update(cx, |this, cx| {
                let can_write = this.editor.can_write;
                match result {
                    Ok(_) => {
                        this.editor.save_error = None;
                        this.editor.server_text = saved_contents.clone();
                        this.apply_saved_text_locally(&target, &saved_contents, cx);
                        let next = this.editor.store.on_persist_result(
                            &target,
                            revision,
                            &saved_contents,
                            true,
                            can_write,
                            now_ms(),
                        );
                        this.run_save_effects(next, cx);
                    }
                    Err(failure) => {
                        let description = failure.describe();
                        this.editor.save_error = Some(description.clone());
                        this.state.update(cx, |state, cx| {
                            state.show_toast(
                                Toast::new(ToastKind::Error, "Could not save file").description(description),
                                cx,
                            );
                        });
                        let next = this.editor.store.on_persist_result(
                            &target,
                            revision,
                            &saved_contents,
                            false,
                            can_write,
                            now_ms(),
                        );
                        this.run_save_effects(next, cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let wrap = self.editor.wrap;
        let language = self.editor.language;
        if self.editor.editor.is_none() {
            let initial = self.editor.loaded_text.clone();
            let editor = cx.new(|cx| {
                let mut state = EditorState::new(window, cx)
                    .line_number(true)
                    .indent_guides(true)
                    .folding(false)
                    .searchable(true)
                    .soft_wrap(wrap)
                    .default_value(initial);
                if let Some(lang) = language {
                    state = state.language(lang);
                }
                state
            });
            let sub = cx.subscribe(&editor, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    this.on_editor_text_changed(value, cx);
                }
            });
            self.editor.editor = Some(editor);
            self.editor._input_sub = Some(sub);
            self.editor.needs_sync = false;
        }

        let editor = self.editor.editor.clone().unwrap();
        if std::mem::take(&mut self.editor.needs_sync) {
            let value = self.editor.loaded_text.clone();
            self.editor.syncing_value = true;
            editor.update(cx, |state, cx| {
                state.set_value(value, window, cx);
            });
            self.editor.syncing_value = false;
        }
        editor.update(cx, |state, cx| {
            state.set_soft_wrap(wrap, window, cx);
            state.set_editor_style(gpui_base::input::InputEditorStyle {
                foreground: hsla(p.code_foreground),
                muted_foreground: hsla(p.muted_foreground),
                background: hsla(p.code_background),
                border: hsla(p.border),
                selection: hsla(p.primary).opacity(0.28),
                caret: hsla(p.foreground),
                ..Default::default()
            });
            if std::mem::take(&mut self.editor.needs_focus) {
                state.focus(window, cx);
            }
        });

        let dirty = self.editor.is_dirty();
        let saving = self.editor.is_saving();
        let can_write = self.editor.can_write;
        let save_error = self.editor.save_error.clone();
        let prompt = self.editor.pending_dismiss.is_some();
        let entity = cx.entity().downgrade();

        let status_label: SharedString = if saving {
            "Saving…".into()
        } else if dirty {
            "Unsaved changes".into()
        } else {
            "Saved".into()
        };

        v_flex()
            .id("file-editor-surface")
            .key_context(CONTEXT)
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(hsla(p.code_background))
            .on_action(cx.listener(|this, _: &SaveFile, _, cx| {
                this.save_active_file(cx);
            }))
            .on_action(cx.listener(|this, _: &DismissEditor, window, cx| {
                let focused = this
                    .editor
                    .editor
                    .as_ref()
                    .is_some_and(|ed| ed.read(cx).focus_handle(cx).is_focused(window));
                let blocked = this.editor.is_dirty() || this.editor.is_saving();
                match decide_escape_dismissal(focused, blocked) {
                    DismissalOutcome::DismissNow => this.finish_exit_edit_mode(cx),
                    DismissalOutcome::PromptUnsaved => {
                        this.editor.pending_dismiss = Some(PendingDismiss::ExitEditMode);
                        cx.notify();
                    }
                    DismissalOutcome::Ignore => {}
                }
            }))
            .child(
                h_flex()
                    .flex_none()
                    .h(px(32.))
                    .px(px(12.))
                    .gap(px(8.))
                    .items_center()
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .bg(hsla(p.background))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.))
                            .items_center()
                            .child(
                                div()
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(if dirty || saving {
                                        hsla(p.warning)
                                    } else {
                                        hsla(p.success)
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(status_label),
                            ),
                    )
                    .child(
                        Button::new("file-editor-save")
                            .variant(Variant::Secondary)
                            .size(Size::Xs)
                            .icon(Icon::Check)
                            .label("Save")
                            .disabled(!dirty || saving || !can_write)
                            .tooltip("Save file (⌘S)")
                            .on_click({
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    if let Some(entity) = entity.upgrade() {
                                        entity.update(cx, |this, cx| this.save_active_file(cx));
                                    }
                                }
                            }),
                    )
                    .child(
                        Button::new("file-editor-done")
                            .variant(Variant::Ghost)
                            .size(Size::Xs)
                            .label("Done")
                            .tooltip("Exit edit mode (Esc)")
                            .on_click({
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    if let Some(entity) = entity.upgrade() {
                                        entity.update(cx, |this, cx| this.request_exit_edit_mode(cx));
                                    }
                                }
                            }),
                    ),
            )
            .when(!can_write, |this| {
                this.child(
                    div()
                        .flex_none()
                        .border_b_1()
                        .border_color(hsla(p.border).opacity(0.6))
                        .px(px(12.))
                        .py(px(6.))
                        .text_size(px(11.))
                        .text_color(hsla(p.muted_foreground))
                        .child("Read-only connection. Unsaved edits are kept until write access returns."),
                )
            })
            .when_some(save_error, |this, err| {
                this.child(
                    div()
                        .flex_none()
                        .border_b_1()
                        .border_color(hsla(p.error).opacity(0.25))
                        .bg(hsla(p.error_surface))
                        .px(px(12.))
                        .py(px(6.))
                        .text_size(px(11.))
                        .text_color(hsla(p.error_foreground))
                        .child(err),
                )
            })
            .when(prompt, |this| {
                this.child(
                    h_flex()
                        .id("file-editor-dismiss-guard")
                        .flex_none()
                        .px(px(12.))
                        .py(px(6.))
                        .gap(px(8.))
                        .items_center()
                        .border_b_1()
                        .border_color(hsla(p.warning).opacity(0.25))
                        .bg(hsla(p.warning_surface))
                        .child(Icon::TriangleAlert.render(px(14.), hsla(p.warning_foreground)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(p.warning_foreground))
                                .child("You have unsaved changes."),
                        )
                        .when(can_write, |bar| {
                            bar.child(
                                Button::new("file-editor-guard-save")
                                    .variant(Variant::Secondary)
                                    .size(Size::Xs)
                                    .label("Save & Close")
                                    .on_click({
                                        let entity = entity.clone();
                                        move |_, _, cx| {
                                            if let Some(entity) = entity.upgrade() {
                                                entity.update(cx, |this, cx| this.save_and_exit_edit_mode(cx));
                                            }
                                        }
                                    }),
                            )
                        })
                        .child(
                            Button::new("file-editor-guard-discard")
                                .variant(Variant::Ghost)
                                .size(Size::Xs)
                                .label("Discard")
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        if let Some(entity) = entity.upgrade() {
                                            entity.update(cx, |this, cx| this.discard_unsaved_and_exit(cx));
                                        }
                                    }
                                }),
                        )
                        .child(
                            Button::new("file-editor-guard-cancel")
                                .variant(Variant::Ghost)
                                .size(Size::Xs)
                                .label("Cancel")
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        if let Some(entity) = entity.upgrade() {
                                            entity.update(cx, |this, cx| this.cancel_dismiss_prompt(cx));
                                        }
                                    }
                                }),
                        ),
                )
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .line_height(px(crate::theme::code_line_height(cx)))
                    .child(div().size_full().child(editor)),
            )
            .into_any_element()
    }
}
