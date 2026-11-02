//! Keybindings: the environment's keymap as a searchable, grouped, editable table
//! (`web/src/components/settings/KeybindingsSettings.tsx`). A row records a shortcut in place and
//! edits its `when` clause in a builder popover, then saves through `server.upsertKeybinding`; the
//! row menu resets a custom binding to its default or removes it (`server.removeKeybinding`). The
//! reply's keybindings install at once. The edit logic lives in `t3_core::settings::keybindings`.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Context, ElementId, Entity, FocusHandle, FontWeight, Hsla, IntoElement, Keystroke, MouseButton, ParentElement,
    SharedString, Styled, Subscription, Window, div, point, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::Value;
use t3_client::supervisor::ConnectionStatus;
use t3_core::keybindings::KeybindingWhenNode;
use t3_core::project_search::{OPEN_IN_EDITOR, open_in_editor_request, preferred_editor};
use t3_core::settings::keybindings::{self as model, BindingRow, SERVER_REMOVE_KEYBINDING, SERVER_UPSERT_KEYBINDING, WhenEdit, WhenOperator};

use super::controls::{Button, ButtonSize, ButtonVariant, ModalLayout, Select, SelectOption, focus_ring, group, icon, muted_text, r, section, section_title};
use super::{Block, SettingsView, store};
use crate::app_state::{AppEvent, Phase};
use crate::composer::controls::{floating_shadow, kbd};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// The draft id of the "New keybinding" row.
const NEW_ROW: &str = "new";
const MAC: bool = cfg!(target_os = "macos");
/// `w-[min(34rem,calc(100vw-2rem))]`; the tree gets it less the popover's padding and border.
const BUILDER_WIDTH: f32 = 544.;
const BUILDER_CONTENT: f32 = BUILDER_WIDTH - 34.;

/// A row's unsaved key and `when` (`KeybindingRowDraftState`).
#[derive(Debug, Clone)]
struct Draft {
    key: String,
    when: Option<KeybindingWhenNode>,
    when_valid: bool,
}

impl Draft {
    fn of(row: Option<&BindingRow>) -> Self {
        Draft { key: row.map(|row| row.key.clone()).unwrap_or_default(), when: row.and_then(|row| row.when_ast.clone()), when_valid: true }
    }

    fn when_expression(&self) -> String {
        model::when_ast_to_expression(self.when.as_ref())
    }

    fn dirty(&self, row: &BindingRow) -> bool {
        self.key != row.key || self.when_expression() != row.when
    }
}

/// The table built from one server keymap, rebuilt only when the keymap changes.
struct Catalog {
    source: Value,
    rows: Rc<Vec<BindingRow>>,
    commands: Rc<Vec<String>>,
}

pub(crate) struct KeybindingsState {
    pub(crate) filter: Entity<InputState>,
    /// The expression field of the open when builder.
    pub(crate) when_input: Entity<InputState>,
    /// Held by the shortcut field of the row in `recording`.
    capture: FocusHandle,
    catalog: Option<Catalog>,
    /// Edited rows by row id; untouched rows show their saved binding.
    drafts: HashMap<String, Draft>,
    adding: bool,
    new_command: Option<String>,
    recording: Option<String>,
    /// The focused field records the next chord (`isRecording`); after one it lets Tab through.
    armed: bool,
    when_open: Option<String>,
    menu: Option<String>,
    /// The command being saved (`savingCommand`).
    saving: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl KeybindingsState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Search keybindings"));
        let when_input = cx.new(|cx| InputState::new(window, cx).placeholder("Always"));
        let capture = cx.focus_handle();
        let view = cx.entity().downgrade();
        let subscriptions = vec![
            cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&when_input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.when_text_changed(&text, cx);
                }
            }),
            cx.on_blur(&capture, window, |this, _, cx| {
                this.keybindings.armed = false;
                cx.notify();
            }),
            // Bound chords (Cmd+K, Escape) dispatch as actions before key listeners see them.
            cx.intercept_keystrokes(move |event, window, cx| {
                let _ = view.update(cx, |this, cx| this.capture_keystroke(&event.keystroke, window, cx));
            }),
        ];
        KeybindingsState {
            filter,
            when_input,
            capture,
            catalog: None,
            drafts: HashMap::new(),
            adding: false,
            new_command: None,
            recording: None,
            armed: false,
            when_open: None,
            menu: None,
            saving: None,
            _subscriptions: subscriptions,
        }
    }

    fn rows(&self) -> Rc<Vec<BindingRow>> {
        self.catalog.as_ref().map(|catalog| catalog.rows.clone()).unwrap_or_default()
    }

    /// Rebuilds the table when the server's keymap changed, dropping drafts of rows it replaced.
    fn refresh(&mut self, keybindings: &Value) {
        if self.catalog.as_ref().is_some_and(|catalog| catalog.source == *keybindings) {
            return;
        }
        let rows = model::binding_rows(keybindings, MAC);
        let live = |id: &String| id == NEW_ROW || rows.iter().any(|row| row.id == *id);
        self.drafts.retain(|id, _| live(id));
        for target in [&mut self.recording, &mut self.when_open, &mut self.menu] {
            if target.as_ref().is_some_and(|id| !live(id)) {
                *target = None;
            }
        }
        self.catalog = Some(Catalog { source: keybindings.clone(), commands: Rc::new(model::command_options(keybindings)), rows: Rc::new(rows) });
    }

    fn draft_for(&self, row: &BindingRow) -> Draft {
        self.drafts.get(&row.id).cloned().unwrap_or_else(|| Draft::of(Some(row)))
    }

    fn draft(&self, id: &str) -> Draft {
        self.drafts.get(id).cloned().unwrap_or_else(|| Draft::of(self.rows().iter().find(|row| row.id == id)))
    }

    fn draft_mut(&mut self, id: &str) -> &mut Draft {
        let rows = self.rows();
        self.drafts.entry(id.to_owned()).or_insert_with(|| Draft::of(rows.iter().find(|row| row.id == id)))
    }

    /// The key a field reverts to on Escape: the saved one, or nothing for a new binding.
    fn saved_key(&self, id: &str) -> String {
        self.rows().iter().find(|row| row.id == id).map(|row| row.key.clone()).unwrap_or_default()
    }

    fn is_dirty(&self, id: &str) -> bool {
        let rows = self.rows();
        match rows.iter().find(|row| row.id == id) {
            Some(row) => self.draft(id).dirty(row),
            None => true,
        }
    }
}

impl SettingsView {
    /// Closes the when builder or a row menu; true when one was open, so Escape stops there.
    pub(super) fn dismiss_keybinding_overlay(&mut self, cx: &mut Context<Self>) -> bool {
        if self.keybindings.menu.take().is_some() {
            cx.notify();
            return true;
        }
        if self.keybindings.when_open.is_some() {
            self.close_when(cx);
            return true;
        }
        false
    }

    fn connected(&self, cx: &App) -> bool {
        matches!(self.state.read(cx).phase, Phase::Online(ConnectionStatus::Connected))
    }

    /// `captureKeybinding`: the focused shortcut field takes every chord; Escape restores the saved
    /// one, and Tab passes once a chord is recorded.
    fn capture_keystroke(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.keybindings.recording.clone() else { return };
        if !self.keybindings.capture.is_focused(window) || (keystroke.key == "tab" && !self.keybindings.armed) {
            return;
        }
        cx.stop_propagation();
        if keystroke.key == "escape" {
            let saved = self.keybindings.saved_key(&target);
            self.keybindings.draft_mut(&target).key = saved;
            self.stop_recording(window, cx);
            return;
        }
        let modifiers = keystroke.modifiers;
        let Some(next) = model::keybinding_from_keystroke(&keystroke.key, modifiers.platform, modifiers.control, modifiers.alt, modifiers.shift, MAC)
        else {
            return;
        };
        self.keybindings.draft_mut(&target).key = next;
        self.keybindings.armed = false;
        // An unchanged row swaps its field back for the pill, which would drop focus.
        if !self.keybindings.is_dirty(&target) {
            self.stop_recording(window, cx);
        }
        cx.notify();
    }

    fn start_recording(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.keybindings.recording = Some(id.to_owned());
        self.keybindings.armed = true;
        self.keybindings.menu = None;
        self.keybindings.capture.focus(window, cx);
        cx.notify();
    }

    fn stop_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keybindings.recording = None;
        self.keybindings.armed = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn open_when(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let expression = self.keybindings.draft(id).when_expression();
        self.keybindings.when_open = Some(id.to_owned());
        self.keybindings.menu = None;
        self.keybindings.when_input.update(cx, |input, cx| input.set_value(expression, window, cx));
        cx.notify();
    }

    /// Closing keeps the last valid clause and drops an unparsed expression (`WhenClauseControl`).
    fn close_when(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.keybindings.when_open.take()
            && let Some(draft) = self.keybindings.drafts.get_mut(&id)
        {
            draft.when_valid = true;
        }
        cx.notify();
    }

    fn when_text_changed(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(id) = self.keybindings.when_open.clone() else { return };
        let draft = self.keybindings.draft_mut(&id);
        match model::parse_when_draft(text) {
            Ok(when) => {
                draft.when = when;
                draft.when_valid = true;
            }
            Err(_) => draft.when_valid = false,
        }
        cx.notify();
    }

    fn set_when(&mut self, id: &str, when: Option<KeybindingWhenNode>, window: &mut Window, cx: &mut Context<Self>) {
        let expression = model::when_ast_to_expression(when.as_ref());
        let draft = self.keybindings.draft_mut(id);
        draft.when = when;
        draft.when_valid = true;
        self.keybindings.when_input.update(cx, |input, cx| input.set_value(expression, window, cx));
        cx.notify();
    }

    fn edit_when(&mut self, id: &str, path: &[usize], edit: &WhenEdit, window: &mut Window, cx: &mut Context<Self>) {
        let when = model::apply_when_edit(self.keybindings.draft(id).when.as_ref(), path, edit);
        self.set_when(id, when, window, cx);
    }

    fn add_to_when(&mut self, id: &str, group: bool, window: &mut Window, cx: &mut Context<Self>) {
        let when = model::add_to_root(self.keybindings.draft(id).when.as_ref(), group);
        self.set_when(id, Some(when), window, cx);
    }

    fn save_binding(&mut self, id: &str, cx: &mut Context<Self>) {
        let rows = self.keybindings.rows();
        let row = rows.iter().find(|row| row.id == id);
        let Some(command) = row.map(|row| row.command.clone()).or_else(|| self.keybindings.new_command.clone()) else { return };
        let draft = self.keybindings.draft(id);
        if draft.key.trim().is_empty() || !draft.when_valid {
            return;
        }
        let payload = model::upsert_payload(&command, &draft.key, &draft.when_expression(), row);
        self.send_keybinding(SERVER_UPSERT_KEYBINDING, payload, command, Some(id.to_owned()), "Unable to save keybinding", cx);
    }

    fn reset_binding(&mut self, row: &BindingRow, cx: &mut Context<Self>) {
        self.keybindings.menu = None;
        if let Some(payload) = model::reset_payload(row) {
            self.send_keybinding(SERVER_UPSERT_KEYBINDING, payload, row.command.clone(), Some(row.id.clone()), "Unable to save keybinding", cx);
        }
    }

    fn remove_binding(&mut self, row: &BindingRow, cx: &mut Context<Self>) {
        self.keybindings.menu = None;
        self.send_keybinding(SERVER_REMOVE_KEYBINDING, model::remove_payload(row), row.command.clone(), Some(row.id.clone()), "Unable to remove keybinding", cx);
    }

    /// Sends one keymap write; its reply installs the new keymap before the config stream repeats it.
    fn send_keybinding(&mut self, method: &'static str, payload: Value, command: String, done: Option<String>, failure: &'static str, cx: &mut Context<Self>) {
        store::clear_error(cx);
        self.keybindings.saving = Some(command);
        let task = self.state.read(cx).rpc(method, payload);
        let state = self.state.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.keybindings.saving = None;
                match result {
                    Ok(reply) => {
                        state.update(cx, |state, cx| {
                            if state.server_config.apply_keybindings_reply(&reply) {
                                state.sync_server_config(cx);
                                cx.emit(AppEvent::ServerConfig);
                            }
                        });
                        if let Some(id) = done {
                            if id == NEW_ROW {
                                this.keybindings.adding = false;
                                this.keybindings.new_command = None;
                            }
                            this.keybindings.drafts.remove(&id);
                        }
                    }
                    Err(error) => store::show_error(format!("{failure}: {error}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_keybindings_file(&mut self, cx: &mut Context<Self>) {
        let Some((path, editor)) = self.keybindings_file_target(cx) else { return };
        let task = self.state.read(cx).rpc(OPEN_IN_EDITOR, open_in_editor_request(&path, editor));
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |_, cx| store::show_error(format!("Unable to open keybindings file: {error}"), cx));
            }
        })
        .detach();
    }

    fn keybindings_file_target(&self, cx: &App) -> Option<(String, &'static str)> {
        let config = self.state.read(cx).server_config.config.as_ref()?;
        let path = config.keybindings_config_path.clone().filter(|path| !path.is_empty())?;
        preferred_editor(&config.available_editors, None).map(|(editor, _)| (path, editor))
    }

    pub(super) fn keybindings_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        static NONE: Value = Value::Null;
        let keybindings = self.state.read(cx).server_config.config.as_ref().map_or(&NONE, |config| &config.keybindings);
        self.keybindings.refresh(keybindings);
        let connected = self.connected(cx);
        let rows = self.keybindings.rows();
        // A settings-search jump to a binding clears the table filter so its row shows.
        if self.scroll_to_target && self.target.as_deref().is_some_and(|target| target.starts_with("keybinding-")) {
            self.keybindings.filter.update(cx, |input, cx| input.set_value("", window, cx));
        }
        let query = self.keybindings.filter.read(cx).value().to_string();
        let mut blocks = vec![Block::new(&["keybindings"], self.keybindings_toolbar(connected, cx))];
        if !connected {
            blocks.push(Block::new(&[], div().px(r(16.)).text_xs().text_color(hsla(palette(cx).muted_foreground)).child("Connect an environment to change keybindings.")));
        }
        if self.keybindings.adding {
            blocks.push(Block::new(&[], group(vec![self.new_binding_row(&rows, window, cx)], cx)));
        }
        let filtered = model::filter_binding_rows(&rows, &query);
        if filtered.is_empty() {
            let empty = div().px(r(16.)).py(r(48.)).flex().justify_center().child(muted_text("No keybindings match your search.", cx));
            blocks.push(Block::new(&[], group(vec![empty.into_any_element()], cx)));
            return blocks;
        }
        // Search jumps land on a command's first row (`keybindingAnchorIds`).
        let mut anchored: Vec<&str> = Vec::new();
        for (title, members) in model::group_binding_rows(&filtered) {
            let mut anchors = Vec::new();
            let elements = members
                .into_iter()
                .map(|row| {
                    let anchor = (!anchored.contains(&row.command.as_str())).then(|| {
                        anchored.push(&row.command);
                        SharedString::from(format!("keybinding-{}", row.command))
                    });
                    anchors.extend(anchor.clone());
                    self.binding_row(row, &rows, anchor, window, cx)
                })
                .collect();
            blocks.push(Block::with_anchors(anchors, v_flex().gap(r(10.)).child(section_title(title)).child(group(elements, cx))));
        }
        blocks
    }

    fn keybindings_toolbar(&self, connected: bool, cx: &mut Context<Self>) -> gpui::Div {
        let p = palette(cx);
        let view = cx.entity();
        let open_view = view.clone();
        let search = h_flex()
            .flex_1()
            .min_w(r(160.))
            .h(r(32.))
            .px(r(9.))
            .gap(r(6.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.input))
            .child(icon(Icon::Search, 14., hsla(p.muted_foreground)))
            .child(div().flex_1().child(Input::new(&self.keybindings.filter).appearance(false).small()));
        section(
            "Keybindings",
            vec![h_flex()
                .flex_wrap()
                .px(r(16.))
                .py(r(12.))
                .gap(r(8.))
                .child(search)
                .child(
                    Button::new("keybindings-add", ButtonVariant::Outline, ButtonSize::Default)
                        .icon(Icon::Plus)
                        .label("Add keybinding")
                        .disabled(self.keybindings.adding || !connected)
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.keybindings.adding = true;
                                cx.notify();
                            })
                        }),
                )
                .child(
                    Button::new("keybindings-open-file", ButtonVariant::Outline, ButtonSize::Icon)
                        .icon(Icon::FileBraces)
                        .tooltip("Open keybindings.json")
                        .disabled(!connected || self.keybindings_file_target(cx).is_none())
                        .on_click(move |_, _, cx| open_view.update(cx, |this, cx| this.open_keybindings_file(cx))),
                )
                .into_any_element()],
            cx,
        )
    }

    /// `KeybindingSettingsRow`: title and source badge, the when clause below, and on the right
    /// conflicts, the hover menu, and the shortcut pill or field with Save while edited.
    fn binding_row(&self, row: &BindingRow, rows: &[BindingRow], anchor: Option<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let view = cx.entity();
        let draft = self.keybindings.draft_for(row);
        let when = draft.when_expression();
        let dirty = draft.dirty(row);
        let conflicts = if dirty { model::conflict_labels(rows, &row.id, &draft.key, &when, MAC) } else { row.conflicts.clone() };
        let saving = self.keybindings.saving.as_deref() == Some(row.command.as_str());
        let highlighted = anchor.as_deref().is_some_and(|anchor| self.is_target(anchor));
        let id = element_id("kb-row", &row.id);
        let recording = self.recording_in(&row.id, window);
        let show_pill = !recording && !dirty && !row.key.is_empty();

        let title = h_flex()
            .id(element_id("kb-title", &row.id))
            .min_h(r(20.))
            .gap(r(8.))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(row.label()))
            .when(row.source != model::BindingSource::Default, |this| this.child(source_badge(row.source.label(), cx)))
            .tooltip(text_tooltip(row.command.clone().into()));
        let description = h_flex()
            .h(r(24.))
            .min_w_0()
            .gap(r(6.))
            .child(div().text_xs().text_color(hsla(p.muted_foreground).opacity(0.7)).child("When"))
            .child(self.when_control(&row.id, &when, &draft, window, cx));

        let save_id = row.id.clone();
        let save_view = view.clone();
        let control = h_flex()
            .flex_wrap()
            .justify_end()
            .gap(r(6.))
            .children(conflict_warning(&conflicts, cx))
            .children(self.row_menu(row, saving, cx))
            .when(dirty, |this| {
                this.child(
                    Button::new(element_id("kb-save", &row.id), ButtonVariant::Default, ButtonSize::Sm)
                        .label(if saving { "Saving" } else { "Save" })
                        .disabled(saving || draft.key.trim().is_empty() || !draft.when_valid)
                        .on_click(move |_, _, cx| save_view.update(cx, |this, cx| this.save_binding(&save_id, cx))),
                )
            })
            .child(if show_pill { self.key_pill(row, cx) } else { self.capture_field(&row.id, &draft.key, 176., window, cx) });

        let fill = highlighted.then(|| hsla(p.accent).opacity(0.6));
        row_layout(id, fill, title.into_any_element(), description.into_any_element(), control.into_any_element(), window)
    }

    /// `NewKeybindingSettingsRow`.
    fn new_binding_row(&self, rows: &[BindingRow], window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let view = cx.entity();
        let draft = self.keybindings.draft(NEW_ROW);
        let when = draft.when_expression();
        let conflicts = model::conflict_labels(rows, NEW_ROW, &draft.key, &when, MAC);
        let saving = self.keybindings.saving.is_some();
        let can_save = self.keybindings.new_command.is_some() && !draft.key.trim().is_empty() && draft.when_valid;
        let commands = self.keybindings.catalog.as_ref().map(|catalog| catalog.commands.clone()).unwrap_or_default();
        let options = commands.iter().map(|command| SelectOption::new(command.clone(), model::command_label(command))).collect();
        let command_view = view.clone();
        let mut select = Select::new("kb-new-command", options).width(224.).popup_width(280.).on_change(move |value, _, cx| {
            command_view.update(cx, |this, cx| {
                this.keybindings.new_command = Some(value.to_string());
                cx.notify();
            })
        });
        select = match &self.keybindings.new_command {
            Some(command) => select.selected(command.clone()),
            None => select.label("Command"),
        };
        let (save_view, cancel_view) = (view.clone(), view);
        let title = div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("New keybinding");
        let description = h_flex()
            .h(r(24.))
            .min_w_0()
            .gap(r(6.))
            .child(div().text_xs().text_color(hsla(p.muted_foreground).opacity(0.7)).child("When"))
            .child(self.when_control(NEW_ROW, &when, &draft, window, cx));
        let control = h_flex()
            .flex_wrap()
            .gap(r(8.))
            .child(select)
            .children(conflict_warning(&conflicts, cx))
            .child(self.capture_field(NEW_ROW, &draft.key, 176., window, cx))
            .child(
                Button::new("kb-new-save", ButtonVariant::Default, ButtonSize::Sm)
                    .label(if saving { "Saving" } else { "Save" })
                    .disabled(saving || !can_save)
                    .on_click(move |_, _, cx| save_view.update(cx, |this, cx| this.save_binding(NEW_ROW, cx))),
            )
            .child(
                Button::new("kb-new-cancel", ButtonVariant::GhostMuted, ButtonSize::IconSm).icon(Icon::X).tooltip("Cancel").disabled(saving).on_click(
                    move |_, _, cx| {
                        cancel_view.update(cx, |this, cx| {
                            this.keybindings.adding = false;
                            this.keybindings.new_command = None;
                            this.keybindings.drafts.remove(NEW_ROW);
                            if this.keybindings.recording.as_deref() == Some(NEW_ROW) {
                                this.keybindings.recording = None;
                            }
                            if this.keybindings.when_open.as_deref() == Some(NEW_ROW) {
                                this.keybindings.when_open = None;
                            }
                            cx.notify();
                        })
                    },
                ),
            );
        row_layout("kb-new".into(), Some(hsla(p.muted).opacity(0.15)), title.into_any_element(), description.into_any_element(), control.into_any_element(), window)
    }

    fn recording_in(&self, id: &str, window: &Window) -> bool {
        self.keybindings.recording.as_deref() == Some(id) && self.keybindings.armed && self.keybindings.capture.is_focused(window)
    }

    /// The saved shortcut as key caps; clicking starts recording (`KeybindingKeyControl`).
    fn key_pill(&self, row: &BindingRow, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let id = row.id.clone();
        h_flex()
            .id(element_id("kb-pill", &row.id))
            .h(r(28.))
            .px(r(6.))
            .mr(r(-6.))
            .gap(r(4.))
            .rounded(r(6.))
            .border_1()
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .hover(|this| this.border_color(hsla(p.border).opacity(0.7)).bg(hsla(p.accent)))
            .children(model::key_pill_parts(&row.key, MAC).into_iter().map(|part| kbd(part, cx)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.start_recording(&id, window, cx)))
            .into_any_element()
    }

    /// The shortcut field: "Press shortcut" while recording, else the draft key or "Unassigned".
    fn capture_field(&self, id: &str, key: &str, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let holder = self.keybindings.recording.as_deref() == Some(id);
        let focused = holder && self.keybindings.capture.is_focused(window);
        let recording = self.recording_in(id, window);
        let (text, placeholder) = match (recording, key.is_empty()) {
            (true, _) => ("Press shortcut".to_owned(), true),
            (false, true) => ("Unassigned".to_owned(), true),
            (false, false) => (key.to_owned(), false),
        };
        let target = id.to_owned();
        h_flex()
            .id(element_id("kb-capture", id))
            .when(holder, |this| this.track_focus(&self.keybindings.capture))
            .w(r(width))
            .h(r(28.))
            .px(r(9.))
            .rounded(r(8.))
            .border_1()
            .border_color(if focused { hsla(p.ring) } else { hsla(p.input) })
            .bg(if dark { hsla(p.input).opacity(0.32) } else { hsla(p.background) })
            .when(focused, |this| this.shadow(focus_ring(hsla(p.ring).opacity(0.24))))
            .cursor_text()
            .font_family(cx.theme().mono_font_family.clone())
            .text_sm()
            .text_color(if placeholder { hsla(p.muted_foreground).opacity(0.72) } else { hsla(p.foreground) })
            .child(div().truncate().child(text))
            // GPUI keeps focus on a click elsewhere; the web field blurs and stops recording.
            .when(focused, |this| {
                this.on_mouse_down_out(cx.listener(|this, _, window, cx| {
                    this.keybindings.armed = false;
                    this.focus.focus(window, cx);
                    cx.notify();
                }))
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.start_recording(&target, window, cx)))
            .into_any_element()
    }

    /// `KeybindingHoverRowMenu`: Reset to default and Remove, shown on row hover.
    fn row_menu(&self, row: &BindingRow, saving: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !row.can_reset() && !row.can_remove() {
            return None;
        }
        let p = palette(cx);
        let open = self.keybindings.menu.as_deref() == Some(row.id.as_str());
        let (view, id) = (cx.entity(), row.id.clone());
        let trigger = Button::new(element_id("kb-menu", &row.id), ButtonVariant::GhostMuted, ButtonSize::IconSm).icon(Icon::Ellipsis).disabled(saving).on_click(
            move |_, _, cx| {
                view.update(cx, |this, cx| {
                    this.keybindings.menu = if open { None } else { Some(id.clone()) };
                    cx.notify();
                })
            },
        );
        let popup = open.then(|| {
            let item = |label: &'static str, destructive: bool| {
                h_flex()
                    .id(label)
                    .min_h(r(28.))
                    .px(r(8.))
                    .py(r(4.))
                    .rounded(r(4.))
                    .text_sm()
                    .cursor_pointer()
                    .text_color(if destructive { hsla(p.destructive_foreground) } else { hsla(p.foreground) })
                    .hover(|this| this.bg(hsla(p.accent)))
                    .child(label)
            };
            let (reset_row, remove_row) = (row.clone(), row.clone());
            div().absolute().bottom_0().right_0().child(
                gpui::deferred(
                    gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
                        v_flex()
                            .occlude()
                            .mt(r(4.))
                            .min_w(r(160.))
                            .p(r(4.))
                            .rounded(r(8.))
                            .border_1()
                            .border_color(hsla(p.dropdown_border))
                            .bg(hsla(p.popover))
                            .shadow(floating_shadow(cx.theme().is_dark()))
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.keybindings.menu = None;
                                cx.notify();
                            }))
                            .when(row.can_reset(), |this| {
                                this.child(item("Reset to default", false).on_click(cx.listener(move |this, _, _, cx| this.reset_binding(&reset_row, cx))))
                            })
                            .when(row.can_remove(), |this| {
                                this.child(item("Remove", true).on_click(cx.listener(move |this, _, _, cx| this.remove_binding(&remove_row, cx))))
                            }),
                    ),
                )
                .with_priority(2),
            )
        });
        Some(
            div()
                .relative()
                .flex_none()
                .when(!open, |this| this.opacity(0.).group_hover("kb-row", |style| style.opacity(1.)))
                .child(trigger)
                .children(popup)
                .into_any_element(),
        )
    }

    /// `WhenClauseControl`: the clause (or "Always") as a micro button that opens the builder.
    fn when_control(&self, id: &str, expression: &str, draft: &Draft, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let open = self.keybindings.when_open.as_deref() == Some(id);
        let target = id.to_owned();
        let trigger = h_flex()
            .id(element_id("kb-when", id))
            .min_w_0()
            .h(r(20.))
            .px(r(5.))
            .gap(r(4.))
            .rounded(r(4.))
            .text_size(r(11.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if expression.is_empty() { hsla(p.muted_foreground) } else { hsla(p.foreground) })
            .cursor_pointer()
            .when(open, |this| this.bg(hsla(p.accent)).text_color(hsla(p.foreground)))
            .hover(|this| this.bg(hsla(p.accent)).text_color(hsla(p.foreground)))
            .child(div().min_w_0().truncate().font_family(cx.theme().mono_font_family.clone()).child(if expression.is_empty() { "Always".to_owned() } else { expression.to_owned() }))
            .child(icon(Icon::ChevronDown, 14., hsla(p.muted_foreground).opacity(0.6)).flex_none())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                if this.keybindings.when_open.as_deref() == Some(target.as_str()) {
                    this.close_when(cx);
                } else {
                    this.open_when(&target, window, cx);
                }
            }));
        let popover = open.then(|| {
            let viewport = window.viewport_size();
            let backdrop = div()
                .id("kb-when-backdrop")
                .w(viewport.width)
                .h(viewport.height)
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_when(cx)));
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .child(gpui::deferred(gpui::anchored().position(point(px(0.), px(0.))).child(backdrop)).with_priority(1))
                .child(
                    gpui::deferred(gpui::anchored().anchor(gpui::Anchor::TopLeft).snap_to_window_with_margin(px(8.)).child(self.when_builder(id, draft, window, cx)))
                        .with_priority(2),
                )
        });
        div().relative().min_w_0().child(trigger).children(popover).into_any_element()
    }

    /// `WhenExpressionBuilder`: root add buttons, the expression field, and the visual tree.
    fn when_builder(&self, id: &str, draft: &Draft, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let view = cx.entity();
        let text = self.keybindings.when_input.read(cx).value().to_string();
        let parsed = model::parse_when_draft(&text);
        let error = parsed.as_ref().err().copied();
        let unknown = parsed.as_ref().ok().map(|when| model::unknown_when_variables(when.as_ref())).unwrap_or_default();
        let add = |suffix: &'static str, label: &'static str, variant: ButtonVariant, group: bool| {
            let (view, id) = (view.clone(), id.to_owned());
            Button::new(element_id(suffix, &id), variant, ButtonSize::Compact)
                .icon(Icon::Plus)
                .label(label)
                .on_click(move |_, window, cx| view.update(cx, |this, cx| this.add_to_when(&id, group, window, cx)))
        };
        let field = h_flex()
            .h(r(28.))
            .px(r(8.))
            .gap(r(6.))
            .rounded(r(6.))
            .border_1()
            .border_color(if error.is_some() { hsla(p.destructive).opacity(0.36) } else { hsla(p.input) })
            .bg(if dark { hsla(p.input).opacity(0.32) } else { hsla(p.background) })
            .font_family(cx.theme().mono_font_family.clone())
            .child(div().flex_1().min_w_0().child(Input::new(&self.keybindings.when_input).appearance(false).small()))
            .children(unknown_warning(&unknown, cx));
        let tree = match &draft.when {
            Some(root) => self.when_node(id, root, root, Vec::new(), BUILDER_CONTENT, cx),
            None => div()
                .p(r(12.))
                .rounded(r(6.))
                .border_1()
                .border_dashed()
                .border_color(hsla(p.border).opacity(0.8))
                .bg(hsla(p.muted).opacity(0.15))
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap(r(8.))
                        .child(add("kb-when-empty-condition", "Condition", ButtonVariant::Default, false))
                        .child(add("kb-when-empty-group", "Group", ButtonVariant::Outline, true)),
                )
                .into_any_element(),
        };
        let viewport = window.viewport_size();
        v_flex()
            .id("kb-when-builder")
            .occlude()
            .mt(r(6.))
            .w(r(BUILDER_WIDTH))
            .max_w(viewport.width - px(32.))
            .max_h(viewport.height - px(16.))
            .overflow_y_scroll()
            .p(r(16.))
            .gap(r(12.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.dropdown_border))
            .bg(hsla(p.popover))
            .shadow(floating_shadow(dark))
            .text_color(hsla(p.foreground))
            .child(
                h_flex()
                    .justify_between()
                    .gap(r(12.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("When"))
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .child(add("kb-when-add-condition", "Condition", ButtonVariant::Outline, false))
                            .child(add("kb-when-add-group", "Group", ButtonVariant::Outline, true)),
                    ),
            )
            .child(v_flex().gap(r(6.)).child(field).children(error.map(|message| {
                h_flex()
                    .gap(r(6.))
                    .text_size(r(11.))
                    .text_color(hsla(p.destructive))
                    .child(icon(Icon::CircleX, 14., hsla(p.destructive)))
                    .child(message)
            })))
            .child(div().relative().child(tree).when(error.is_some(), |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(r(16.))
                        .rounded(r(8.))
                        .border_1()
                        .border_color(hsla(p.destructive).opacity(0.3))
                        .bg(hsla(p.background).opacity(0.75))
                        .text_xs()
                        .text_center()
                        .text_color(hsla(p.destructive))
                        .child("Fix the expression above to continue editing visually."),
                )
            }))
            .into_any_element()
    }

    /// `WhenExpressionNodeEditor` for the node at `path`, laid out in `width`.
    fn when_node(&self, id: &str, root: &KeybindingWhenNode, node: &KeybindingWhenNode, path: Vec<usize>, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let depth = path.len();
        let dark = cx.theme().is_dark();
        let view = cx.entity();
        let key = format!("{id}\u{0}{path:?}");
        let edit = |edit: WhenEdit| {
            let (view, id, path) = (view.clone(), id.to_owned(), path.clone());
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| view.update(cx, |this, cx| this.edit_when(&id, &path, &edit, window, cx))
        };
        let remove = model::when_can_remove(root, &path).then(|| {
            Button::new(element_id("kb-when-remove", &key), ButtonVariant::Ghost, ButtonSize::IconSm)
                .icon(Icon::Minus)
                .tooltip(model::when_remove_label(node, depth))
                .on_click(edit(WhenEdit::Remove))
        });

        if let Some((identifier, negated)) = model::condition_parts(node) {
            let unknown = if model::is_known_when_variable(identifier) { Vec::new() } else { vec![identifier.to_owned()] };
            let mut variables = model::when_variable_options();
            if !variables.iter().any(|variable| variable == identifier) {
                variables.insert(0, identifier.to_owned());
            }
            let options = variables.into_iter().map(|variable| SelectOption::new(variable.clone(), variable)).collect();
            let select_width = (width - 18. - 40. - 8. - if remove.is_some() { 36. } else { 0. } - if unknown.is_empty() { 0. } else { 26. }).max(96.);
            let (select_view, select_id, select_path) = (view.clone(), id.to_owned(), path.clone());
            return h_flex()
                .gap(r(8.))
                .px(r(8.))
                .py(r(8.))
                .rounded(r(6.))
                .border_1()
                .border_color(hsla(p.border).opacity(0.7))
                .bg(hsla(p.background).opacity(0.6))
                .child(not_toggle(element_id("kb-when-not", &key), negated, dark, cx).on_click(edit(WhenEdit::SetNegated(!negated))))
                .child(Select::new(element_id("kb-when-variable", &key), options).selected(identifier.to_owned()).width(select_width).popup_width(220.).on_change(
                    move |value, window, cx| {
                        select_view.update(cx, |this, cx| this.edit_when(&select_id, &select_path, &WhenEdit::SetIdentifier(value.to_string()), window, cx))
                    },
                ))
                .children(unknown_warning(&unknown, cx))
                .children(remove)
                .into_any_element();
        }

        let nested = depth > 0;
        let child_width = width - 18. - 16.;
        if let KeybindingWhenNode::Not { node: inner } = node {
            let mut child_path = path.clone();
            child_path.push(0);
            return v_flex()
                .gap(r(8.))
                .p(r(8.))
                .rounded(r(8.))
                .border_1()
                .border_color(hsla(p.border).opacity(if nested { 0.5 } else { 0.7 }))
                .bg(if nested { hsla(p.background).opacity(0.5) } else { hsla(p.muted).opacity(0.2) })
                .child(
                    h_flex()
                        .gap(r(8.))
                        .child(not_toggle(element_id("kb-when-not", &key), true, dark, cx).on_click(edit(WhenEdit::SetNegated(false))))
                        .child(div().flex_1())
                        .children(remove),
                )
                .child(indented(self.when_node(id, root, inner, child_path, child_width, cx), hsla(p.border).opacity(0.7)))
                .into_any_element();
        }

        let operator = model::when_operator(node).unwrap_or(WhenOperator::And);
        let (operator_view, operator_id, operator_path) = (view.clone(), id.to_owned(), path.clone());
        let line = if nested { hsla(p.border).opacity(0.7) } else { hsla(p.border) };
        let children: Vec<AnyElement> = model::when_children(node)
            .into_iter()
            .enumerate()
            .map(|(index, child)| {
                let mut child_path = path.clone();
                child_path.push(index);
                indented(self.when_node(id, root, &child, child_path, child_width, cx), line)
            })
            .collect();
        v_flex()
            .gap(r(8.))
            .p(r(8.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.border).opacity(if nested { 0.7 } else { 0.6 }))
            .bg(if nested { hsla(p.background).opacity(0.55) } else { hsla(p.muted).opacity(0.1) })
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(r(8.))
                    .child(
                        Select::new(element_id("kb-when-operator", &key), vec![SelectOption::new("and", "and"), SelectOption::new("or", "or")])
                            .selected(if operator == WhenOperator::Or { "or" } else { "and" })
                            .width(96.)
                            .popup_width(96.)
                            .on_change(move |value, window, cx| {
                                let next = if value.as_ref() == "or" { WhenOperator::Or } else { WhenOperator::And };
                                operator_view.update(cx, |this, cx| this.edit_when(&operator_id, &operator_path, &WhenEdit::SetOperator(next), window, cx))
                            }),
                    )
                    .child(Button::new(element_id("kb-when-condition", &key), ButtonVariant::Outline, ButtonSize::Compact).icon(Icon::Plus).label("Condition").on_click(edit(WhenEdit::AddCondition)))
                    .child(Button::new(element_id("kb-when-group", &key), ButtonVariant::Outline, ButtonSize::Compact).icon(Icon::Plus).label("Group").on_click(edit(WhenEdit::AddGroup)))
                    .child(div().flex_1())
                    .children(remove),
            )
            .child(v_flex().gap(r(8.)).children(children))
            .into_any_element()
    }
}

fn element_id(prefix: &str, id: &str) -> ElementId {
    ElementId::Name(format!("{prefix}:{id}").into())
}

fn text_tooltip(text: SharedString) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView {
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

/// `SettingsRow` with element title and description, as the keybinding rows need.
fn row_layout(id: ElementId, fill: Option<Hsla>, title: AnyElement, description: AnyElement, control: AnyElement, window: &Window) -> AnyElement {
    let stacked = ModalLayout::of(window).rows_stacked;
    v_flex()
        .id(id)
        .group("kb-row")
        .px(r(16.))
        .py(r(12.))
        .when_some(fill, |this, fill| this.rounded(r(12.)).bg(fill))
        .child(
            div()
                .w_full()
                .flex()
                .map(|this| if stacked { this.flex_col().gap(r(12.)) } else { this.flex_row().items_center().gap(r(32.)) })
                .child(v_flex().flex_1().min_w_0().gap(r(4.)).child(title).child(description))
                .child(h_flex().map(|this| if stacked { this.w_full().min_w_0() } else { this.flex_none().max_w(r(560.)).justify_end() }).child(control)),
        )
        .into_any_element()
}

/// `Badge variant="outline" size="sm"`.
fn source_badge(label: &'static str, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    div()
        .h(r(18.))
        .min_w(r(18.))
        .px(r(3.))
        .flex()
        .items_center()
        .rounded(r(4.))
        .border_1()
        .border_color(hsla(p.input))
        .bg(if dark { hsla(p.input).opacity(0.32) } else { hsla(p.background) })
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.foreground))
        .child(label)
}

/// `WarningTooltipIcon`: a warning glyph whose tooltip explains.
fn warning_icon(id: ElementId, size: f32, tooltip: String, cx: &App) -> AnyElement {
    let p = palette(cx);
    div()
        .id(id)
        .size(r(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(r(4.))
        .hover(|this| this.bg(hsla(p.warning).opacity(0.1)))
        .child(icon(Icon::TriangleAlert, 14., hsla(p.warning)))
        .tooltip(text_tooltip(tooltip.into()))
        .into_any_element()
}

/// `KeybindingConflictWarning`.
fn conflict_warning(labels: &[String], cx: &App) -> Option<AnyElement> {
    let description = model::conflict_description(labels)?;
    let tooltip = format!("{description} The most recent matching binding wins when both conditions can apply.");
    Some(warning_icon(ElementId::Name(format!("kb-conflict:{description}").into()), 20., tooltip, cx))
}

/// `UnknownWhenVariableWarning`.
fn unknown_warning(identifiers: &[String], cx: &App) -> Option<AnyElement> {
    let label = model::unknown_when_label(identifiers)?;
    let tooltip = format!("{label}. T3 Code does not recognize this condition yet. It can still be saved, but it may not match unless the runtime provides it.");
    Some(warning_icon(ElementId::Name(format!("kb-unknown:{label}").into()), 18., tooltip, cx))
}

/// `Toggle variant="outline" size="compact"` reading "Not".
fn not_toggle(id: ElementId, pressed: bool, dark: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let p = palette(cx);
    let fill = match (pressed, dark) {
        (true, true) => hsla(p.input),
        (true, false) => hsla(p.input).opacity(0.64),
        (false, true) => hsla(p.input).opacity(0.32),
        (false, false) => hsla(p.background),
    };
    h_flex()
        .id(id)
        .flex_none()
        .h(r(28.))
        .min_w(r(40.))
        .px(r(3.))
        .justify_center()
        .rounded(r(6.))
        .border_1()
        .border_color(hsla(p.input))
        .bg(fill)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(if pressed { hsla(p.accent_foreground) } else { hsla(p.foreground) })
        .cursor_pointer()
        .when(!pressed, |this| this.hover(|this| this.bg(hsla(p.accent))))
        .child("Not")
}

/// A child of a when group, indented with the tree's guide lines.
fn indented(child: AnyElement, line: Hsla) -> AnyElement {
    div()
        .relative()
        .pl(r(16.))
        .child(div().absolute().top_0().bottom_0().left(r(6.)).w(px(1.)).bg(line))
        .child(div().absolute().top(r(16.)).left(r(6.)).w(r(10.)).h(px(1.)).bg(line))
        .child(child)
        .into_any_element()
}
