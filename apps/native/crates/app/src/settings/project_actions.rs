//! A project's Actions (`ProjectActionsSettings.tsx`, `ProjectActionsList.tsx`) and the action
//! editor dialog (`ProjectScriptEditorDialog` in `projectScriptEditor.tsx`): add, edit, delete,
//! import from `t3.json`, and reset to the inherited list. Saves follow
//! `useProjectScriptSettings.ts`: each member's override, then its shortcuts, in order, stopping at
//! the first failure. The rules live in `t3_core::settings::project_actions`.

use gpui::{
    AnyElement, App, AppContext, BoxShadow, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, Keystroke,
    MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, anchored, deferred, div, point,
    prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::keybindings::{ResolvedKeybindingRule, ShortcutMatchContext, decode_resolved_keybindings, shortcut_label_for_command};
use t3_core::project_scripts::{ProjectScript, ScriptIcon, command_for_project_script};
use t3_core::settings::keybindings::keybinding_from_keystroke;
use t3_core::settings::ServerSettingChange;
use t3_core::settings::project_actions::{
    ACTIONS_DESCRIPTION, ActionWrite, EditorRequest, FileScript, FormValues, Member, NO_ACTIONS, ProjectFile, SCRIPT_ICONS, ScriptInput, ScriptsEdit,
    importable, keybinding_writes, scripts_mixed, scripts_overridden, shown_scripts, submit_id, validate_form,
};
use t3_core::settings::projects::OverrideEdit;
use t3_core::shell::ProjectShell;

use super::controls::{Button, ButtonSize, ButtonVariant, Row, Switch, focus_ring, group, icon, r, section_title};
use super::projects::{SETTINGS_WRITE_SCOPE, Shown};
use super::{SettingsView, store};
use crate::icons::Icon;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

const MAC: bool = cfg!(target_os = "macos");

pub(crate) fn script_icon(icon: ScriptIcon) -> Icon {
    match icon {
        ScriptIcon::Test => Icon::FlaskConical,
        ScriptIcon::Lint => Icon::ListChecks,
        ScriptIcon::Configure => Icon::Wrench,
        ScriptIcon::Build => Icon::Hammer,
        ScriptIcon::Debug => Icon::Bug,
        ScriptIcon::Play => Icon::Play,
    }
}

/// The open editor: the request and the fields that are not text inputs.
pub(super) struct Editor {
    request: EditorRequest,
    icon: ScriptIcon,
    run_on_worktree_create: bool,
    wait_for_setup: bool,
    run_on_settle: bool,
    keybinding: String,
    auto_open_preview: bool,
    icon_picker_open: bool,
    error: Option<String>,
    saving: bool,
    /// Which open this is, so a save that settles late never closes or fails a newer one.
    generation: u64,
}

/// What a finished save does besides reporting a failure.
enum AfterSave {
    Nothing,
    /// Close the editor this open started, or show its error there.
    Editor(u64),
    /// An import that failed opens the editor on it with the error (`importFileScript`).
    Import(ScriptInput),
}

pub(crate) struct ActionsState {
    pub(super) editor: Option<Editor>,
    /// An editor to open at the next render, which has the window (a failed import, a snapshot).
    pub(super) pending: Option<EditorRequest>,
    /// A save is running; the section's buttons wait for it.
    saving: bool,
    pub(super) import_open: bool,
    generation: u64,
    pub(super) name: Entity<InputState>,
    pub(super) command: Entity<TextareaState>,
    pub(super) preview_url: Entity<InputState>,
    keybinding_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl ActionsState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Test"));
        let command = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 10).placeholder("bun test"));
        let preview_url = cx.new(|cx| InputState::new(window, cx).placeholder("http://localhost:5173"));
        let view = cx.entity().downgrade();
        let subscriptions = vec![
            cx.subscribe(&preview_url, |_, _, _: &gpui_component::input::InputEvent, cx| cx.notify()),
            // Bound chords dispatch as actions before key listeners see them.
            cx.intercept_keystrokes(move |event, window, cx| {
                let _ = view.update(cx, |this, cx| this.capture_action_keybinding(&event.keystroke, window, cx));
            }),
        ];
        ActionsState {
            editor: None,
            pending: None,
            saving: false,
            import_open: false,
            generation: 0,
            name,
            command,
            preview_url,
            keybinding_focus: cx.focus_handle().tab_stop(true),
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn close(&mut self) {
        self.editor = None;
        self.pending = None;
        self.import_open = false;
    }

    /// Closes the editor's icon picker, the editor, or the import menu: true when one was open,
    /// so Escape stops there.
    pub(super) fn dismiss(&mut self) -> bool {
        if let Some(editor) = self.editor.as_mut() {
            if editor.icon_picker_open {
                editor.icon_picker_open = false;
            } else {
                self.editor = None;
            }
            return true;
        }
        std::mem::take(&mut self.import_open)
    }

    pub(super) fn inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        [&self.name, &self.preview_url].into_iter()
    }
}

pub(super) fn members_of(projects: &[ProjectShell]) -> Vec<Member> {
    projects
        .iter()
        .map(|project| Member { id: project.id.clone(), scripts: project.scripts.iter().filter_map(|script| serde_json::from_value(script.clone()).ok()).collect() })
        .collect()
}

impl SettingsView {
    fn actions_keybindings(&self, cx: &App) -> Vec<ResolvedKeybindingRule> {
        self.thread_settings(cx).map(|config| decode_resolved_keybindings(&config.keybindings)).unwrap_or_default()
    }

    fn open_action_editor(&mut self, request: EditorRequest, window: &mut Window, cx: &mut Context<Self>) {
        let actions = &mut self.projects.actions;
        actions.import_open = false;
        actions.generation += 1;
        let initial = request.initial.clone();
        let (name, command, preview) = (initial.name.clone(), initial.command.clone(), initial.preview_url.clone().unwrap_or_default());
        actions.name.update(cx, |input, cx| input.set_value(name, window, cx));
        actions.command.update(cx, |input, cx| input.set_value(command, window, cx));
        actions.preview_url.update(cx, |input, cx| input.set_value(preview, window, cx));
        actions.editor = Some(Editor {
            icon: initial.icon,
            run_on_worktree_create: initial.run_on_worktree_create,
            wait_for_setup: initial.wait_for_setup,
            run_on_settle: initial.run_on_settle,
            keybinding: initial.keybinding.clone().flatten().unwrap_or_default(),
            auto_open_preview: initial.auto_open_preview,
            icon_picker_open: false,
            error: request.error.clone(),
            saving: false,
            generation: actions.generation,
            request,
        });
        actions.name.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn close_action_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.projects.actions.editor = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// `captureKeybinding`: the focused shortcut field records chords; Backspace or Delete clears
    /// it, Tab moves on, Escape closes the editor.
    fn capture_action_keybinding(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        if self.projects.actions.editor.is_none() || !self.projects.actions.keybinding_focus.is_focused(window) {
            return;
        }
        if !self.may(SETTINGS_WRITE_SCOPE, cx) || matches!(keystroke.key.as_str(), "tab" | "escape") {
            return;
        }
        cx.stop_propagation();
        let next = if matches!(keystroke.key.as_str(), "backspace" | "delete") {
            Some(String::new())
        } else {
            let modifiers = keystroke.modifiers;
            let mapper = cx.keyboard_mapper().clone();
            keybinding_from_keystroke(&keystroke.key, modifiers.platform, modifiers.control, modifiers.alt, modifiers.shift, MAC, mapper.get_key_equivalents())
        };
        if let (Some(next), Some(editor)) = (next, self.projects.actions.editor.as_mut()) {
            editor.keybinding = next;
            cx.notify();
        }
    }

    fn save_action_editor(&mut self, cx: &mut Context<Self>) {
        let can_write = self.may(SETTINGS_WRITE_SCOPE, cx);
        let shown = self.shown_members(cx);
        let Some(first) = shown.first().map(|member| member.id.clone()) else { return };
        let scripts = self.thread_settings(cx).map(|config| shown_scripts(&config.settings, &first).to_vec()).unwrap_or_default();
        let actions = &self.projects.actions;
        let Some(editor) = actions.editor.as_ref().filter(|editor| !editor.saving) else { return };
        let form = FormValues {
            name: actions.name.read(cx).value().to_string(),
            command: actions.command.read(cx).value().to_string(),
            icon: editor.icon,
            run_on_worktree_create: editor.run_on_worktree_create,
            wait_for_setup: editor.wait_for_setup,
            run_on_settle: editor.run_on_settle,
            keybinding: editor.keybinding.clone(),
            preview_url: actions.preview_url.read(cx).value().to_string(),
            auto_open_preview: editor.auto_open_preview,
        };
        let result = validate_form(&editor.request, &form, &scripts, can_write, can_write, now_ms().max(0) as u64);
        let (script_id, generation) = (editor.request.script_id.clone(), editor.generation);
        let Some(editor) = self.projects.actions.editor.as_mut() else { return };
        match result {
            Err(message) => editor.error = Some(message),
            Ok(input) => {
                editor.error = None;
                editor.saving = true;
                editor.icon_picker_open = false;
                self.submit_action(script_id, input, AfterSave::Editor(generation), cx);
            }
        }
        cx.notify();
    }

    /// `submit`: a new action takes an id no project uses; the others give up the roles it claims.
    fn submit_action(&mut self, script_id: Option<String>, input: ScriptInput, after: AfterSave, cx: &mut Context<Self>) {
        let members = members_of(&self.shown_members(cx));
        let projects = members_of(&self.state.read(cx).shell.snapshot.as_ref().map(|snapshot| snapshot.projects.clone()).unwrap_or_default());
        let Some(config) = self.thread_settings(cx) else { return };
        let id = submit_id(&config.settings, &projects, &members, script_id.as_deref(), &input.name, now_ms().max(0) as u64);
        self.persist_actions(ScriptsEdit::Save { id, input, is_new: script_id.is_none() }, after, cx);
    }

    /// `persist`: refuses while another save runs or without settings access. The list joins the
    /// settings queue and is built when it sends, so it keeps a model or workspace change queued
    /// ahead of it; the shortcuts follow once it saved.
    fn persist_actions(&mut self, edit: ScriptsEdit, after: AfterSave, cx: &mut Context<Self>) {
        let shown = self.shown_members(cx);
        if self.projects.actions.saving || shown.is_empty() {
            let message = "No available machine, or another action change is saving.";
            store::show_error(format!("Actions not saved: {message}"), cx);
            return self.finish_actions_save(after, Some(message.to_owned()), cx);
        }
        if !self.may(SETTINGS_WRITE_SCOPE, cx) {
            let message = "This connection cannot change environment settings.";
            store::show_error(format!("Failed to save project actions: {message}"), cx);
            return self.finish_actions_save(after, Some(message.to_owned()), cx);
        }
        let members = members_of(&shown);
        let projects = members_of(&self.state.read(cx).shell.snapshot.as_ref().map(|snapshot| snapshot.projects.clone()).unwrap_or_default());
        let keybindings = self.actions_keybindings(cx);
        let Some(config) = self.thread_settings(cx) else { return };
        let shortcuts = keybinding_writes(&config.settings, &keybindings, &projects, &members, &edit);
        self.projects.actions.saving = true;
        cx.notify();
        let (view, state) = (cx.entity().downgrade(), self.state.clone());
        let change = ServerSettingChange::ProjectOverride { members, edit: OverrideEdit::Scripts(edit) };
        store::update_server_then(
            &self.state,
            change,
            move |error, cx| {
                if let Some(message) = error {
                    let _ = view.update(cx, |this, cx| {
                        this.projects.actions.saving = false;
                        this.finish_actions_save(after, Some(message), cx);
                    });
                    return;
                }
                let requests: Vec<_> = shortcuts.iter().map(|write: &ActionWrite| state.read(cx).rpc(write.method(), write.payload().clone())).collect();
                cx.spawn(async move |cx| {
                    let mut failure = None;
                    for request in requests {
                        if let Err(error) = request.await {
                            failure = Some(error.to_string());
                            break;
                        }
                    }
                    let _ = view.update(cx, |this, cx| {
                        this.projects.actions.saving = false;
                        if let Some(message) = &failure {
                            store::show_error(format!("Failed to save project actions: {message}"), cx);
                        }
                        this.finish_actions_save(after, failure, cx);
                    });
                })
                .detach();
            },
            cx,
        );
    }

    fn finish_actions_save(&mut self, after: AfterSave, failure: Option<String>, cx: &mut Context<Self>) {
        let actions = &mut self.projects.actions;
        match after {
            AfterSave::Nothing => {}
            AfterSave::Editor(generation) => {
                if let Some(editor) = actions.editor.as_mut().filter(|editor| editor.generation == generation) {
                    match failure {
                        Some(message) => {
                            editor.saving = false;
                            editor.error = Some(message);
                        }
                        None => actions.editor = None,
                    }
                }
            }
            AfterSave::Import(input) => {
                if let Some(message) = failure {
                    actions.pending = Some(EditorRequest { script_id: None, initial: input, error: Some(message) });
                }
            }
        }
        cx.notify();
    }

    /// Opens the Add action editor at the next render.
    #[cfg(feature = "snapshot")]
    pub(super) fn queue_add_action(&mut self, cx: &mut Context<Self>) {
        self.projects.actions.pending = Some(EditorRequest::add());
        cx.notify();
    }

    /// Opens the editor a failed import left, now that a render has the window.
    pub(super) fn open_pending_action_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(request) = self.projects.actions.pending.take() {
            self.open_action_editor(request, window, cx);
        }
    }

    fn import_file_script(&mut self, file: FileScript, cx: &mut Context<Self>) {
        self.projects.actions.import_open = false;
        let input = file.import_input();
        self.submit_action(None, input.clone(), AfterSave::Import(input), cx);
    }

    fn delete_action(&mut self, id: String, cx: &mut Context<Self>) {
        self.persist_actions(ScriptsEdit::Delete { id }, AfterSave::Nothing, cx);
    }

    fn reset_actions(&mut self, cx: &mut Context<Self>) {
        self.persist_actions(ScriptsEdit::Reset, AfterSave::Nothing, cx);
    }

    /// The editor's Delete: confirm first (`AlertDialog`), then close and delete.
    fn request_delete_action(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.projects.actions.editor.as_ref() else { return };
        let Some(script_id) = editor.request.script_id.clone() else { return };
        let name = self.projects.actions.name.read(cx).value().to_string();
        let view = cx.entity().downgrade();
        crate::dialog::ask(format!("Delete action \"{name}\"?"), "This action cannot be undone.", "Delete action", move |_, cx| {
            let _ = view.update(cx, |this, cx| {
                if !this.may(SETTINGS_WRITE_SCOPE, cx) {
                    if let Some(editor) = this.projects.actions.editor.as_mut() {
                        editor.error = Some("This connection cannot change project actions.".to_owned());
                    }
                    return cx.notify();
                }
                this.projects.actions.editor = None;
                this.delete_action(script_id, cx);
            });
        }, cx);
    }

    /// The Actions section: its header row, the list (or why there is none), and a broken
    /// `t3.json`.
    pub(super) fn project_actions_section(&self, shown: &Shown, config: &t3_core::server_config::ServerConfig, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let ids = shown.member_ids();
        let settings = &config.settings;
        let can_write = self.may(SETTINGS_WRITE_SCOPE, cx);
        let actions = &self.projects.actions;
        let disabled = actions.saving || !can_write;
        let scripts = shown_scripts(settings, ids[0]).to_vec();
        let mixed = scripts_mixed(settings, &ids);
        let keybindings = decode_resolved_keybindings(&config.keybindings);
        let file_scripts: Vec<FileScript> = self.project_file().map(ProjectFile::scripts).unwrap_or_default().to_vec();
        let importable: Vec<FileScript> = importable(&file_scripts, &scripts).into_iter().cloned().collect();

        let import = (!importable.is_empty()).then(|| {
            let trigger = Button::new("import-scripts", ButtonVariant::Ghost, ButtonSize::Xs).label("Import scripts").trailing(Icon::ChevronDown).disabled(disabled).on_click(
                cx.listener(|this, _, _, cx| {
                    this.projects.actions.import_open = !this.projects.actions.import_open;
                    cx.notify();
                }),
            );
            div().relative().child(trigger).when(actions.import_open, |this| this.child(self.import_menu(&importable, cx))).into_any_element()
        });
        let add = Button::new("add-action", ButtonVariant::Outline, ButtonSize::Xs)
            .icon(Icon::Plus)
            .label("Add action")
            .disabled(disabled || ids.is_empty())
            .on_click(cx.listener(|this, _, window, cx| this.open_action_editor(EditorRequest::add(), window, cx)));
        let header = Row::new("project-actions-row", "Actions")
            .description(ACTIONS_DESCRIPTION)
            .reset_tooltip("Reset to inherited value")
            .reset(can_write && scripts_overridden(settings, &ids), cx.listener(|this, _, _, cx| this.reset_actions(cx)))
            .control(h_flex().flex_wrap().gap(r(6.)).children(import).child(add));

        let mut rows = vec![header.into_any_element()];
        if mixed {
            rows.push(
                Row::new("project-actions-mixed", "Different actions across environments")
                    .description("Choose one environment to edit its list. Adding an action here adds it on every selected environment.")
                    .into_any_element(),
            );
        } else if scripts.is_empty() {
            rows.push(div().px(r(16.)).py(r(8.)).text_sm().text_color(hsla(p.muted_foreground)).child(NO_ACTIONS).into_any_element());
        } else {
            rows.extend(scripts.iter().map(|script| self.action_row(script, &keybindings, disabled, cx)));
        }
        if matches!(self.project_file(), Some(ProjectFile::Invalid)) {
            rows.push(
                Row::new("project-actions-invalid-file", "t3.json is invalid")
                    .description("A t3.json exists in this checkout but fails to parse, so every action and icon it declares is ignored. Check the JSON syntax and icon values.")
                    .into_any_element(),
            );
        }
        let highlighted = self.is_target("project-actions");
        v_flex()
            .gap(r(10.))
            .rounded(r(12.))
            .when(highlighted, |this| this.bg(hsla(p.accent).opacity(0.3)))
            .child(section_title("Actions"))
            .child(group(rows, cx))
            .into_any_element()
    }

    /// `ProjectActionsList`'s row: icon, name, role badges, command, shortcut, and an edit button
    /// shown on hover.
    fn action_row(&self, script: &ProjectScript, keybindings: &[ResolvedKeybindingRule], disabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let shortcut = command_for_project_script(&script.id).and_then(|command| shortcut_label_for_command(keybindings, &command, &ShortcutMatchContext::default(), MAC));
        let badge = |text: &'static str| {
            div().flex_none().px(r(6.)).rounded(r(4.)).border_1().border_color(hsla(p.border).opacity(0.6)).text_size(r(11.)).line_height(r(16.)).font_weight(FontWeight::NORMAL).text_color(hsla(p.muted_foreground)).child(text)
        };
        let edit_script = script.clone();
        let group_name = SharedString::from(format!("action-row-{}", script.id));
        let row_keybindings = keybindings.to_vec();
        h_flex()
            .id(SharedString::from(format!("project-action-{}", script.id)))
            .group(group_name.clone())
            .px(r(16.))
            .py(r(8.))
            .gap(r(32.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(r(4.))
                    .child(
                        h_flex()
                            .min_h(r(20.))
                            .gap(r(8.))
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child(icon(script_icon(script.icon), 16., hsla(p.muted_foreground)))
                            .child(div().min_w_0().truncate().child(script.name.clone()))
                            .when(script.run_on_worktree_create, |this| this.child(badge("setup")))
                            .when(script.runs_on_settle(), |this| this.child(badge("on settle")))
                            .when(script.preview_url.is_some(), |this| this.child(badge("preview \u{b7} desktop only"))),
                    )
                    .child(div().truncate().text_xs().font_family(cx.theme().mono_font_family.clone()).text_color(hsla(p.muted_foreground)).child(script.command.clone())),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(r(8.))
                    .children(shortcut.map(|label| div().text_xs().text_color(hsla(p.muted_foreground)).child(label)))
                    .child(
                        div().invisible().group_hover(group_name, |this| this.visible()).child(
                            Button::new(SharedString::from(format!("edit-action-{}", script.id)), ButtonVariant::GhostMuted, ButtonSize::IconXs)
                                .icon(Icon::Settings)
                                .tooltip(format!("Edit {}", script.name))
                                .disabled(disabled)
                                .on_click(cx.listener(move |this, _, window, cx| this.open_action_editor(EditorRequest::edit(&edit_script, &row_keybindings), window, cx))),
                        ),
                    ),
            )
            .into_any_element()
    }

    /// "Import from t3.json": one item per importable script.
    fn import_menu(&self, scripts: &[FileScript], cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let mono = cx.theme().mono_font_family.clone();
        let items = scripts.iter().enumerate().map(|(index, file)| {
            let pick = file.clone();
            h_flex()
                .id(("import-script", index))
                .min_h(r(28.))
                .px(r(8.))
                .py(r(4.))
                .gap(r(8.))
                .rounded(r(4.))
                .cursor_pointer()
                .hover(|this| this.bg(hsla(p.accent)))
                .child(div().opacity(0.8).child(icon(script_icon(file.icon.unwrap_or_default()), 16., hsla(p.muted_foreground))))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(file.name.clone()))
                        .child(div().truncate().font_family(mono.clone()).text_color(hsla(p.muted_foreground)).child(file.command.clone())),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.import_file_script(pick.clone(), cx)))
        });
        div()
            .absolute()
            .bottom_0()
            .right_0()
            .child(
                deferred(
                    anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
                        v_flex()
                            .id("import-scripts-menu")
                            .occlude()
                            .mt(r(4.))
                            .w(r(288.))
                            .max_h(r(360.))
                            .overflow_y_scroll()
                            .p(r(4.))
                            .rounded(r(8.))
                            .border_1()
                            .border_color(hsla(p.dropdown_border))
                            .bg(hsla(p.popover))
                            .shadow(crate::composer::controls::floating_shadow(dark))
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.projects.actions.import_open = false;
                                cx.notify();
                            }))
                            .child(div().px(r(8.)).py(r(6.)).text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child("Import from t3.json"))
                            .child(div().px(r(8.)).pb(r(8.)).text_sm().text_color(hsla(p.muted_foreground)).child("Add actions declared by this checkout without editing them first."))
                            .child(div().h(px(1.)).my(r(4.)).mx(r(-4.)).bg(hsla(p.border)))
                            .children(items),
                    ),
                )
                .with_priority(2),
            )
            .into_any_element()
    }

    /// `ProjectScriptEditorDialog` over the window, while an editor is open.
    pub(super) fn action_editor_overlay(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let editor = self.projects.actions.editor.as_ref()?;
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let can_write = self.may(SETTINGS_WRITE_SCOPE, cx);
        let locked = editor.saving || !can_write;
        let editing = editor.request.script_id.is_some();
        let viewport = window.viewport_size();
        let actions = &self.projects.actions;
        let has_preview = !actions.preview_url.read(cx).value().trim().is_empty();
        let label = |text: &'static str| div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(text);
        let hint = |text: &'static str| div().text_xs().text_color(hsla(p.muted_foreground)).child(text);
        let field = |children: Vec<AnyElement>| v_flex().gap(r(6.)).children(children);

        let icon_button = Button::new("action-icon", ButtonVariant::Outline, ButtonSize::IconLg).icon(script_icon(editor.icon)).tooltip("Choose icon").disabled(locked).on_click(cx.listener(
            |this, _, _, cx| {
                if let Some(editor) = this.projects.actions.editor.as_mut() {
                    editor.icon_picker_open = !editor.icon_picker_open;
                }
                cx.notify();
            },
        ));
        let icon_picker = editor.icon_picker_open.then(|| self.icon_picker(editor.icon, cx));
        let name_row = h_flex()
            .gap(r(8.))
            .child(div().relative().child(icon_button).children(icon_picker))
            .child(div().flex_1().min_w_0().child(Input::new(&actions.name).disabled(locked)));

        let keybinding = self.keybinding_field(&editor.keybinding, locked || !can_write, window, cx);
        let toggle = |id: &'static str, text: &'static str, checked: bool, enabled: bool, set: fn(&mut Editor, bool)| {
            let view = cx.entity().downgrade();
            h_flex()
                .justify_between()
                .gap(r(12.))
                .px(r(12.))
                .py(r(8.))
                .rounded(r(6.))
                .border_1()
                .border_color(if dark { gpui::transparent_black() } else { hsla(p.border).opacity(0.7) })
                .when(dark, |this| this.bg(hsla(p.secondary)))
                .text_sm()
                .text_color(hsla(p.foreground))
                .when(!enabled, |this| this.opacity(0.6))
                .child(text)
                .child(Switch::new(id, checked).disabled(!enabled || locked).on_change(move |checked, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        if let Some(editor) = this.projects.actions.editor.as_mut() {
                            set(editor, checked);
                        }
                        cx.notify();
                    });
                }))
        };
        let panel = v_flex()
            .id("action-editor-panel")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(r(24.))
            .pt(r(4.))
            .pb(r(24.))
            .gap(r(16.))
            .child(field(vec![label("Name").into_any_element(), name_row.into_any_element()]))
            .child(field(vec![
                label("Keybinding").into_any_element(),
                keybinding,
                hint("Press a shortcut. Use Backspace to clear. Shortcuts are environment-wide. Projects using the same action share its shortcut.").into_any_element(),
            ]))
            .child(field(vec![label("Command").into_any_element(), Textarea::new(&actions.command).disabled(locked).into_any_element()]))
            .child(field(vec![
                label("Preview URL (optional)").into_any_element(),
                Input::new(&actions.preview_url).disabled(locked).into_any_element(),
                hint("Open this URL in the in-app preview when this action runs.").into_any_element(),
            ]))
            .child(toggle("action-run-on-create", "Run automatically on worktree creation", editor.run_on_worktree_create, true, |editor, on| editor.run_on_worktree_create = on))
            .child(toggle("action-wait-for-setup", "Wait for it to finish before the agent starts", editor.wait_for_setup, editor.run_on_worktree_create, |editor, on| editor.wait_for_setup = on))
            .child(toggle("action-run-on-settle", "Run in the thread's worktree when the thread settles", editor.run_on_settle, true, |editor, on| editor.run_on_settle = on))
            .child(toggle("action-auto-open-preview", "Open preview automatically when this action runs", editor.auto_open_preview, has_preview, |editor, on| editor.auto_open_preview = on))
            .children(editor.error.clone().map(|error| div().text_sm().text_color(hsla(p.destructive)).child(error)));

        let footer = h_flex()
            .gap(r(8.))
            .px(r(24.))
            .py(r(16.))
            .when(editing, |this| {
                this.child(
                    Button::new("action-delete", ButtonVariant::DestructiveOutline, ButtonSize::Default)
                        .label("Delete")
                        .disabled(locked)
                        .on_click(cx.listener(|this, _, _, cx| this.request_delete_action(cx))),
                )
            })
            .child(div().flex_1())
            .child(Button::new("action-cancel", ButtonVariant::Outline, ButtonSize::Default).label("Cancel").on_click(cx.listener(|this, _, window, cx| this.close_action_editor(window, cx))))
            .child(
                Button::new("action-save", ButtonVariant::Default, ButtonSize::Default)
                    .label(if editor.saving {
                        "Saving\u{2026}"
                    } else if editing {
                        "Save changes"
                    } else {
                        "Save action"
                    })
                    .disabled(locked)
                    .on_click(cx.listener(|this, _, _, cx| this.save_action_editor(cx))),
            );

        let dialog = v_flex()
            .id("action-editor")
            .relative()
            .w_full()
            .max_w(px(512.))
            .max_h(viewport.height - px(32.))
            .rounded(px(16.))
            .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
            .shadow(vec![BoxShadow { color: gpui::black().opacity(if dark { 0.9 } else { 0.65 }), offset: point(px(0.), px(24.)), blur_radius: px(64.), spread_radius: px(-24.), inset: false }])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .p(r(24.))
                    .gap(r(8.))
                    .child(div().text_xl().line_height(r(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child(if editing { "Edit Action" } else { "Add Action" }))
                    .child(div().text_sm().text_color(hsla(p.muted_foreground)).child("Actions are project-scoped commands you can run from the top bar or keybindings.")),
            )
            .child(panel)
            .child(footer);
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("action-editor-backdrop")
                        .w(viewport.width)
                        .h(viewport.height)
                        .occlude()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(16.))
                        .bg(hsla(p.dialog_backdrop))
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_action_editor(window, cx)))
                        .child(dialog),
                ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    /// The read-only shortcut field: shows the recorded key, or "Press shortcut".
    fn keybinding_field(&self, key: &str, disabled: bool, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let focus = self.projects.actions.keybinding_focus.clone();
        let focused = focus.is_focused(window);
        let placeholder = key.is_empty();
        h_flex()
            .id("action-keybinding")
            .when(!disabled, |this| this.track_focus(&focus).cursor_text())
            .h(r(32.))
            .px(r(11.))
            .rounded(r(8.))
            .border_1()
            .border_color(if focused { hsla(p.ring) } else { hsla(p.input) })
            .bg(if dark { hsla(p.input).opacity(0.32) } else { hsla(p.background) })
            .when(focused, |this| this.shadow(focus_ring(hsla(p.ring).opacity(0.24))))
            .when(disabled, |this| this.opacity(0.64))
            .text_sm()
            .text_color(if placeholder { hsla(p.placeholder) } else { hsla(p.foreground) })
            .child(div().truncate().child(if placeholder { "Press shortcut".to_owned() } else { key.to_owned() }))
            .on_click(cx.listener(move |_, _, window, cx| {
                if !disabled {
                    window.focus(&focus, cx);
                }
            }))
            .into_any_element()
    }

    /// The icon popover: six choices in a three-column grid.
    fn icon_picker(&self, selected: ScriptIcon, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let choices = SCRIPT_ICONS.iter().map(|(choice, _, label)| {
            let (choice, chosen) = (*choice, *choice == selected);
            v_flex()
                .id(SharedString::from(format!("action-icon-{label}")))
                .items_center()
                .gap(r(8.))
                .px(r(8.))
                .py(r(8.))
                .rounded(r(6.))
                .border_1()
                .text_xs()
                .text_color(hsla(p.foreground))
                .cursor_pointer()
                .map(|this| {
                    if chosen {
                        this.border_color(if dark { gpui::transparent_black() } else { hsla(p.primary).opacity(0.7) })
                            .bg(hsla(p.primary).opacity(0.1))
                            .when(dark, |this| this.shadow(vec![BoxShadow { color: hsla(p.primary).opacity(0.3), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(1.), inset: true }]))
                    } else {
                        this.border_color(if dark { gpui::transparent_black() } else { hsla(p.border).opacity(0.7) })
                            .when(dark, |this| this.bg(hsla(p.secondary)))
                            .hover(|this| this.bg(hsla(p.accent).opacity(0.6)))
                    }
                })
                .child(icon(script_icon(choice), 16., hsla(p.foreground)))
                .child(*label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(editor) = this.projects.actions.editor.as_mut() {
                        editor.icon = choice;
                        editor.icon_picker_open = false;
                    }
                    cx.notify();
                }))
        });
        div()
            .absolute()
            .bottom_0()
            .left_0()
            .child(
                deferred(
                    anchored().snap_to_window_with_margin(px(8.)).child(
                        div()
                            .id("action-icon-picker")
                            .occlude()
                            .mt(r(4.))
                            .w(r(256.))
                            .p(r(16.))
                            .rounded(r(8.))
                            .border_1()
                            .border_color(hsla(p.dropdown_border))
                            .bg(hsla(p.popover))
                            .shadow(crate::composer::controls::floating_shadow(dark))
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                if let Some(editor) = this.projects.actions.editor.as_mut() {
                                    editor.icon_picker_open = false;
                                }
                                cx.notify();
                            }))
                            .child(div().grid().grid_cols(3).gap(r(8.)).children(choices)),
                    ),
                )
                .with_priority(4),
            )
            .into_any_element()
    }
}
