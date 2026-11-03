//! Floating workspace host for `ThemeEditorPanel.tsx` / `ThemeEditorHost.tsx`.
use crate::{
    app_state::AppState,
    icons::Icon,
    settings::{
        controls::{Button, ButtonSize, ButtonVariant, Switch, r},
        store,
    },
    theme::{self, hsla, palette},
};
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme, Sizable, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use std::collections::BTreeMap;
use t3_core::themes::{
    self, ThemeDefinition,
    editor::{FAMILIES, Session, picker},
};
mod color_picker;

actions!(theme_editor, [ToggleThemeEditor]);
struct Host {
    state: Entity<AppState>,
    view: Option<Entity<Editor>>,
    pending: Option<Session>,
}
impl Global for Host {}
pub fn init(state: Entity<AppState>, cx: &mut App) {
    cx.set_global(Host {
        state,
        view: None,
        pending: None,
    });
    cx.on_action(|_: &ToggleThemeEditor, cx| toggle(cx));
}
pub fn toggle(cx: &mut App) {
    if cx.global::<Host>().view.as_ref().is_some_and(|v| v.read(cx).saving) {
        return;
    }
    if cx.global::<Host>().view.is_some() || cx.global::<Host>().pending.is_some() {
        close(cx);
    } else {
        open(None, None, None, cx);
    }
}
pub fn open(editing: Option<ThemeDefinition>, seed: Option<ThemeDefinition>, name: Option<String>, cx: &mut App) {
    if cx.global::<Host>().view.as_ref().is_some_and(|v| v.read(cx).saving) {
        return;
    }
    let prefs = theme::preferences(cx);
    let dark = cx.theme().is_dark();
    let active = themes::half_theme(&prefs.theme, &prefs.halves, dark, &prefs.custom, theme::environment_themes(cx)).cloned();
    let seed = if editing.is_none() && seed.is_none() && name.is_none() {
        active
    } else {
        seed
    };
    cx.global_mut::<Host>().pending = Some(Session::new(editing, seed.as_ref(), name, dark));
    cx.refresh_windows();
}
fn is_current_editor(id: EntityId, cx: &App) -> bool {
    let host = cx.global::<Host>();
    host.pending.is_none() && host.view.as_ref().is_some_and(|view| view.entity_id() == id)
}
fn close(cx: &mut App) {
    let host = cx.global_mut::<Host>();
    host.view = None;
    host.pending = None;
    theme::preview(None, cx);
    cx.refresh_windows();
}
pub fn overlay(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    if let Some(session) = cx.global_mut::<Host>().pending.take() {
        let state = cx.global::<Host>().state.clone();
        let editor = cx.new(|cx| Editor::new(state, session, window, cx));
        editor.read(cx).name.focus_handle(cx).focus(window, cx);
        cx.global_mut::<Host>().view = Some(editor);
    }
    let view = cx.global::<Host>().view.clone()?;
    Some(deferred(div().absolute().inset_0().child(view)).with_priority(3).into_any_element())
}
struct Editor {
    state: Entity<AppState>,
    session: Session,
    name: Entity<InputState>,
    query: Entity<InputState>,
    fields: BTreeMap<&'static str, Entity<InputState>>,
    subscriptions: Vec<Subscription>,
    syncing: bool,
    preview: themes::editor::Preview,
    swatches: BTreeMap<&'static str, FocusHandle>,
    picker_epoch: usize,
    minimized: bool,
    picker: Option<color_picker::Picker>,
    selected: Option<&'static str>,
    position: Option<Point<Pixels>>,
    dimensions: Option<Size<Pixels>>,
    drag: Option<(Point<Pixels>, Point<Pixels>, bool)>,
    saving: bool,
    focus: FocusHandle,
}
impl Editor {
    fn new(state: Entity<AppState>, session: Session, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(if session.is_editing() { "Theme name" } else { "e.g. Aurora" })
                .default_value(session.name.clone())
        });
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Filter colors"));
        let mut subscriptions = vec![
            cx.subscribe_in(&name, window, |this, input, event: &InputEvent, window, cx| {
                if this.syncing || this.saving {
                    return;
                }
                if matches!(event, InputEvent::Change) {
                    this.session.set_name(input.read(cx).value().to_string(), store::client(cx));
                    this.sync(window, cx);
                }
            }),
            cx.subscribe(&query, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.stop_picker_drag(window, cx);
            }
        }));
        let fields = FAMILIES
            .iter()
            .map(|f| {
                let role = f.role;
                let input = cx.new(|cx| InputState::new(window, cx).default_value(display(&session.colors()[role])));
                subscriptions.push(cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| {
                    if this.syncing || this.saving {
                        return;
                    }
                    if matches!(event, InputEvent::Change) {
                        this.selected = Some(role);
                        this.session.update_color(role, input.read(cx).value().to_string());
                        this.apply_preview(cx);
                        cx.notify();
                    } else if matches!(event, InputEvent::Blur) {
                        this.sync_fields(window, cx);
                    }
                }));
                (role, input)
            })
            .collect();
        let preview = themes::editor::Preview::new(session.dark, session.colors());
        theme::preview(Some((&preview.colors, session.dark)), cx);
        Self {
            state,
            session,
            name,
            query,
            fields,
            subscriptions,
            syncing: false,
            preview,
            swatches: FAMILIES.iter().map(|f| (f.role, cx.focus_handle().tab_stop(true))).collect(),
            picker_epoch: 0,
            minimized: false,
            picker: None,
            selected: None,
            position: None,
            dimensions: None,
            drag: None,
            saving: false,
            focus: cx.focus_handle(),
        }
    }
    fn apply_preview(&mut self, cx: &mut Context<Self>) {
        if is_current_editor(cx.entity().entity_id(), cx) && self.preview.update(self.session.dark, self.session.colors()) {
            theme::preview(Some((&self.preview.colors, self.preview.dark)), cx);
        }
    }

    fn sync_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.syncing = true;
        for (role, field) in &self.fields {
            let value = display(&self.session.colors()[*role]);
            if field.read(cx).value().as_str() != value {
                field.update(cx, |s, cx| s.set_value(value, window, cx));
            }
        }
        self.syncing = false;
    }
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_fields(window, cx);
        self.apply_preview(cx);
        cx.notify();
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        match self.session.prepare_save(store::client(cx)) {
            Err(e) => {
                self.session.error = Some(e);
                cx.notify();
            }
            Ok(save) => {
                self.saving = true;
                cx.notify();
                let patch = save.patch.clone();
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { t3_client::store::commit_client_settings(patch) })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.saving = false;
                        if let Err(e) = result {
                            this.session.error = Some(e);
                            cx.notify();
                            return;
                        }
                        store::install_committed_client(&this.state, save.patch, cx);
                        let (title, description) = if let Some(dark) = save.merged_dark {
                            (
                                format!("{} updated", save.theme.label),
                                format!("Its {} palette was added.", if dark { "dark" } else { "light" }),
                            )
                        } else if save.created {
                            (format!("{} created", save.theme.label), "It’s now active.".into())
                        } else {
                            let p = theme::preferences(cx);
                            let active = p.theme.named_id() == Some(&save.theme.id) || p.halves.values().any(|id| id == &save.theme.id);
                            (
                                format!("{} saved", save.theme.label),
                                if active {
                                    "Your changes are now active."
                                } else {
                                    "Your changes are saved."
                                }
                                .into(),
                            )
                        };
                        this.state.update(cx, |state, cx| {
                            state.show_toast(
                                crate::toast::Toast::new(crate::toast::ToastKind::Success, title).description(description),
                                cx,
                            )
                        });
                        close(cx);
                    });
                })
                .detach();
            }
        }
    }
    fn field(&self, role: &'static str, label: &'static str, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let invalid = picker::hex(&self.session.colors()[role]).is_none();
        h_flex()
            .gap(r(8.))
            .px(r(8.))
            .py(r(6.))
            .rounded(r(8.))
            .when(self.selected == Some(role), |d| d.bg(hsla(p.accent).opacity(0.6)))
            .child(div().flex_1().min_w_0().text_sm().text_color(hsla(p.muted_foreground)).child(label))
            .child(
                div()
                    .id(ElementId::Name(format!("choose-{role}").into()))
                    .track_focus(&self.swatches[role])
                    .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                        if crate::settings::controls::activates(e) {
                            this.open_picker(role, window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .size(r(24.))
                    .rounded_full()
                    .border_1()
                    .border_color(hsla(p.foreground).opacity(0.3))
                    .when(self.swatches[role].is_focused(window) && window.last_input_was_keyboard(), |d| {
                        d.border_2().border_color(hsla(p.ring))
                    })
                    .bg(hsla(picker::swatch(&self.session.colors()[role]).unwrap_or(p.background)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| this.open_picker(role, window, cx))),
            )
            .child(
                div()
                    .w(r(112.))
                    .when(invalid, |d| d.border_1().rounded(r(6.)).border_color(hsla(p.destructive)))
                    .child(Input::new(&self.fields[role]).small()),
            )
            .into_any_element()
    }
}
fn display(value: &str) -> String {
    if value.trim().to_lowercase().starts_with("oklch(") || (value.len() == 9 && value.ends_with("ff")) {
        picker::hex(value).unwrap_or(value.into())
    } else {
        value.into()
    }
}
impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = &self.subscriptions;
        let p = palette(cx);
        let viewport = window.viewport_size();
        let weak = cx.entity().downgrade();
        let panel_size = self.dimensions.unwrap_or(size(
            px(416.),
            px(if self.minimized {
                40.
            } else if self.session.advanced {
                672.
            } else {
                320.
            })
            .min(viewport.height - px(96.)),
        ));
        let width = panel_size.width.min(viewport.width - px(16.)).max(px(280.));
        let height = panel_size
            .height
            .min(viewport.height - px(16.))
            .max(px(if self.minimized { 40. } else { 220. }));
        let pos = self
            .position
            .unwrap_or(point(viewport.width - width - px(16.), viewport.height - height - px(16.)));
        let pos = point(
            pos.x.max(px(8.)).min((viewport.width - width - px(8.)).max(px(8.))),
            pos.y.max(px(8.)).min((viewport.height - px(48.)).max(px(8.))),
        );
        let title = if self.session.is_editing() { "Edit theme" } else { "Create theme" };
        let mut body = v_flex().gap(r(12.)).px(r(12.)).py(r(12.)).child(
            h_flex()
                .gap(r(12.))
                .child(div().w(r(116.)).text_sm().font_weight(FontWeight::MEDIUM).child("Theme name"))
                .child(div().flex_1().child(Input::new(&self.name).small())),
        );
        if let Some(e) = &self.session.error {
            body = body.child(div().text_sm().text_color(hsla(p.destructive)).child(e.clone()));
        }
        body = body
            .child(
                h_flex()
                    .gap(r(12.))
                    .child(div().w(r(116.)).text_sm().font_weight(FontWeight::MEDIUM).child("Appearance"))
                    .child(h_flex().gap(r(2.)).children([false, true].into_iter().map(|dark| {
                        let reason = self.session.lock_reason(dark, store::client(cx));
                        Button::new(
                            if dark { "editor-dark" } else { "editor-light" },
                            if dark == self.session.dark {
                                ButtonVariant::Secondary
                            } else {
                                ButtonVariant::GhostMuted
                            },
                            ButtonSize::Xs,
                        )
                        .label(if dark { "Dark" } else { "Light" })
                        .disabled(reason.is_some() || self.saving)
                        .tooltip(reason.unwrap_or_default())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.end_picker(false, window, cx);
                            this.session.set_appearance(dark, store::client(cx));

                            this.sync(window, cx);
                        }))
                    }))),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        v_flex()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Colors"))
                            .when(!self.session.advanced, |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(hsla(p.muted_foreground))
                                        .child("Two colors, rest derived"),
                                )
                            }),
                    )
                    .child(
                        h_flex().gap(r(8.)).child("Advanced").child(
                            Switch::new("editor-advanced", self.session.advanced)
                                .disabled(self.saving)
                                .on_change(move |on, window, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.end_picker(false, window, cx);
                                        this.session.set_advanced(on);
                                        this.sync(window, cx);
                                    });
                                }),
                        ),
                    ),
            );
        if self.session.advanced {
            body = body.child(Input::new(&self.query).small());
        }
        let query = self.query.read(cx).value().to_string();
        let mut group = "";
        let mut found = false;
        for family in FAMILIES {
            if (!self.session.advanced && !matches!(family.role, "canvas" | "accent")) || (self.session.advanced && !family.matches(&query))
            {
                continue;
            }
            if self.session.advanced && group != family.group {
                group = family.group;
                body = body.child(div().pt(r(12.)).text_sm().font_weight(FontWeight::MEDIUM).child(group));
            }
            found = true;
            body = body.child(self.field(family.role, family.label, window, cx));
        }
        if !found {
            body = body.child(div().text_xs().child("No matches."));
        }
        let header = h_flex()
            .gap(r(8.))
            .px(r(12.))
            .py(r(8.))
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.7))
            .child(
                div()
                    .id("theme-editor-drag")
                    .flex_1()
                    .min_w_0()
                    .cursor_move()
                    .child(title)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            this.drag = Some((e.position, pos, false));
                            cx.notify();
                        }),
                    ),
            )
            .child(
                Button::new("minimize-editor", ButtonVariant::Ghost, ButtonSize::IconXs)
                    .icon(if self.minimized { Icon::ChevronUp } else { Icon::ChevronDown })
                    .tooltip(if self.minimized {
                        "Expand the theme editor"
                    } else {
                        "Minimize the theme editor"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.minimized = !this.minimized;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("close-editor", ButtonVariant::Ghost, ButtonSize::IconXs)
                    .icon(Icon::X)
                    .tooltip("Close the theme editor")
                    .disabled(self.saving)
                    .on_click(|_, _, cx| close(cx)),
            );
        let footer = h_flex()
            .justify_end()
            .gap(r(8.))
            .px(r(12.))
            .py(r(8.))
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.7))
            .child(
                Button::new("cancel-theme", ButtonVariant::Ghost, ButtonSize::Sm)
                    .label("Cancel")
                    .disabled(self.saving)
                    .on_click(|_, _, cx| close(cx)),
            )
            .child(
                Button::new("save-theme", ButtonVariant::Default, ButtonSize::Sm)
                    .label(self.session.save_label(store::client(cx)))
                    .disabled(self.session.name.trim().is_empty() || self.saving)
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            );
        let panel = v_flex()
            .id("theme-editor")
            .key_context("ThemeEditor")
            .track_focus(&self.focus)
            .absolute()
            .left(pos.x)
            .top(pos.y)
            .w(width)
            .when(!self.minimized, |d| d.h(height))
            .rounded(r(12.))
            .overflow_hidden()
            .occlude()
            .text_sm()
            .text_color(hsla(p.popover_foreground))
            .child(crate::glass::backdrop(12., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
            .child(header)
            .when(!self.minimized, |d| {
                d.child(div().id("theme-editor-scroll").flex_1().min_h_0().overflow_y_scroll().child(body))
                    .child(footer)
                    .child(
                        div()
                            .id("theme-editor-resize")
                            .absolute()
                            .bottom_0()
                            .right_0()
                            .size(px(18.))
                            .cursor_crosshair()
                            .child("◢")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                    this.drag = Some((e.position, point(width, height), true));
                                    cx.notify();
                                }),
                            ),
                    )
            })
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && this.picker.is_some() {
                    this.end_picker(true, window, cx);
                    cx.stop_propagation();
                }
            }));
        div()
            .absolute()
            .inset_0()
            .child(panel)
            .children(self.render_picker(window, cx))
            .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
                if e.pressed_button != Some(MouseButton::Left) {
                    this.drag = None;
                    return;
                }
                if let Some((start, original, resize)) = this.drag {
                    let delta = e.position - start;
                    if resize {
                        this.dimensions = Some(size(
                            (original.x + delta.x).max(px(280.)).min(viewport.width - pos.x - px(8.)),
                            (original.y + delta.y).max(px(220.)).min(viewport.height - pos.y - px(8.)),
                        ));
                        this.position = Some(pos);
                    } else {
                        this.position = Some(original + delta);
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.drag = None))
    }
}

#[cfg(feature = "snapshot")]
pub fn fixture(value: serde_json::Value, window: &mut Window, cx: &mut App) {
    let prefs = theme::preferences(cx);
    let find = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .and_then(|id| themes::resolve(id, &prefs.custom, theme::environment_themes(cx)))
            .cloned()
    };
    let editing = find("editing");
    let seed = find("seed");
    let mut session = Session::new(
        editing,
        seed.as_ref(),
        None,
        value
            .get("dark")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(cx.theme().is_dark()),
    );
    if let Some(name) = value.get("name").and_then(serde_json::Value::as_str) {
        session.set_name(name.into(), store::client(cx));
    }
    if let Some(advanced) = value.get("advanced").and_then(serde_json::Value::as_bool) {
        session.set_advanced(advanced);
    }
    if let Some(colors) = value.get("colors").and_then(serde_json::Value::as_object) {
        for (role, color) in colors {
            if let Some(color) = color.as_str() {
                session.update_color(role, color.into());
            }
        }
    }
    let state = cx.global::<Host>().state.clone();
    let view = cx.new(|cx| Editor::new(state, session, window, cx));
    view.update(cx, |this, cx| {
        if let Some(role) = value
            .get("picker")
            .and_then(serde_json::Value::as_str)
            .and_then(|r| FAMILIES.iter().find(|f| f.role == r).map(|f| f.role))
        {
            this.open_picker(role, window, cx);
        }
        if value.get("submit") == Some(&serde_json::Value::Bool(true)) {
            this.save(cx);
        }
        this.name.focus_handle(cx).focus(window, cx);
    });
    cx.global_mut::<Host>().view = Some(view.clone());
    if let Some(exit) = value.get("pendingExit").and_then(serde_json::Value::as_str) {
        view.update(cx, |this, cx| this.pending_sample_fixture(window, cx));
        match exit {
            "toggle" => toggle(cx),
            "cancel" => close(cx),
            "save" => view.update(cx, |this, cx| this.save(cx)),
            "replace" => open(None, None, Some("Replacement".into()), cx),
            _ => panic!("unknown pending preview exit"),
        }
    }
    cx.refresh_windows();
}

#[cfg(feature = "snapshot")]
pub fn verify_pending_exit_fixture(exit: &str, cx: &App) -> Result<(), String> {
    if exit == "replace" {
        let editor = cx
            .global::<Host>()
            .view
            .as_ref()
            .ok_or("Replacement editor did not open.")?
            .read(cx);
        theme::verify_preview_source(editor.session.colors(), editor.session.dark, cx)
    } else {
        theme::verify_saved_palette(cx)
    }
}

#[cfg(feature = "snapshot")]
pub fn verify_pending_exit_retired(exit: &str, cx: &App) -> Result<(), String> {
    if exit != "replace" && cx.global::<Host>().view.is_some() {
        return Err("The editor did not close before the queued frame.".into());
    }
    Ok(())
}
