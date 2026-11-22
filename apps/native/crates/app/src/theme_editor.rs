//! Floating workspace host for `ThemeEditorPanel.tsx` / `ThemeEditorHost.tsx`.
use crate::{
    app_state::AppState,
    icons::Icon,
    settings::{
        controls::{Button, ButtonSize, ButtonVariant, Switch, r},
        store,
    },
    theme::{
        self, ThemeSlot, ThemeSlotExt, hsla, palette, slot_has_background,
        theme_slot_clipped_background_layer,
    },
};
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme, Sizable, h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    v_flex,
};
use std::{cell::Cell, collections::BTreeMap, rc::Rc};
use t3_core::{
    client_settings::typography::FontRole,
    themes::{
        self, ThemeDefinition, css,
        editor::{FAMILIES, Session, picker},
    },
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
    theme::slots::set_inspector_active(false, cx);
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
    css_input: Entity<TextareaState>,
    css_tab_indents: bool,
    fields: BTreeMap<&'static str, Entity<InputState>>,
    subscriptions: Vec<Subscription>,
    syncing: bool,
    preview: themes::editor::Preview,
    swatches: BTreeMap<&'static str, FocusHandle>,
    swatch_bounds: BTreeMap<&'static str, Rc<Cell<Bounds<Pixels>>>>,
    panel_bounds: Rc<Cell<Bounds<Pixels>>>,
    picker_epoch: usize,
    minimized: bool,
    inspecting: bool,
    hovered_inspection: Option<(Bounds<Pixels>, &'static str)>,
    scroll: ScrollHandle,
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
        let initial_css = session.css.clone();
        let css_input = cx.new(|cx| {
            let mut input = TextareaState::new(window, cx)
                .placeholder(":root {\n  --primary: #346bf1;\n}")
                .auto_grow(6, 12);
            if !initial_css.is_empty() {
                input.set_value(initial_css, window, cx);
            }
            input
        });
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
            cx.subscribe_in(&css_input, window, |this, input, event: &InputEvent, _, cx| {
                match event {
                    InputEvent::Focus => {
                        this.css_tab_indents = true;
                        cx.notify();
                    }
                    InputEvent::Change => {
                        if this.syncing || this.saving {
                            return;
                        }
                        this.session.set_css(input.read(cx).value().to_string());
                        this.apply_preview(cx);
                        cx.notify();
                    }
                    _ => {}
                }
            }),
            cx.observe(&css_input, |this, _, cx| {
                if this.session.css_open {
                    cx.notify();
                }
            }),
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
        let effective_css = session.effective_css(store::client(cx));
        let preview = themes::editor::Preview::new_with_css(session.dark, session.colors(), Some(&effective_css));
        theme::preview_with_css(Some((&preview.colors, preview.css.as_deref(), session.dark)), cx);
        Self {
            state,
            session,
            name,
            query,
            css_input,
            css_tab_indents: true,
            fields,
            subscriptions,
            syncing: false,
            preview,
            swatches: FAMILIES
                .iter()
                .map(|f| (f.role, cx.focus_handle().tab_stop(true)))
                .collect(),
            swatch_bounds: FAMILIES
                .iter()
                .map(|f| (f.role, Rc::new(Cell::new(Bounds::default()))))
                .collect(),
            panel_bounds: Rc::default(),
            picker_epoch: 0,
            minimized: false,
            inspecting: false,
            hovered_inspection: None,
            scroll: ScrollHandle::new(),
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
        let effective_css = self.session.effective_css(store::client(cx));
        if is_current_editor(cx.entity().entity_id(), cx)
            && self.preview.update_with_css(self.session.dark, self.session.colors(), Some(&effective_css))
        {
            theme::preview_with_css(
                Some((&self.preview.colors, self.preview.css.as_deref(), self.preview.dark)),
                cx,
            );
        }
    }
    fn apply_css_edit(&mut self, edit: css::EditResult, window: &mut Window, cx: &mut Context<Self>) {
        self.syncing = true;
        self.css_input.update(cx, |input, cx| {
            input.set_value(edit.text.clone(), window, cx);
            input.set_selected_range(edit.selection, cx);
        });
        self.syncing = false;
        self.session.set_css(edit.text);
        self.apply_preview(cx);
        cx.notify();
    }

    fn sync_inspector_state(&self, cx: &mut Context<Self>) {
        theme::slots::set_inspector_active(self.inspecting || self.selected.is_some(), cx);
    }

    fn toggle_selected_role(&mut self, role: &'static str, cx: &mut Context<Self>) {
        self.selected = if self.selected == Some(role) { None } else { Some(role) };
        self.sync_inspector_state(cx);
        cx.notify();
    }

    fn clear_inspector_selection(&mut self, cx: &mut Context<Self>) {
        self.inspecting = false;
        self.hovered_inspection = None;
        self.selected = None;
        self.sync_inspector_state(cx);
        cx.notify();
    }

    fn toggle_inspecting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inspecting {
            self.clear_inspector_selection(cx);
            return;
        }
        self.end_picker(false, window, cx);
        self.inspecting = true;
        self.hovered_inspection = None;
        self.sync_inspector_state(cx);
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn pick_inspection_target(
        pos: Point<Pixels>,
        viewport: Size<Pixels>,
        panel_bounds: Bounds<Pixels>,
        cx: &App,
    ) -> Option<(Bounds<Pixels>, &'static str)> {
        if panel_bounds.size.width > px(0.) && panel_bounds.contains(&pos) {
            return None;
        }
        let picked = theme::slots::inspector_entries(cx)
            .iter()
            .filter(|(_, bounds)| bounds.contains(&pos))
            .min_by(|(_, a), (_, b)| {
                let area_a = f32::from(a.size.width) * f32::from(a.size.height);
                let area_b = f32::from(b.size.width) * f32::from(b.size.height);
                area_a.total_cmp(&area_b)
            })
            .map(|(slot, bounds)| (*bounds, theme::slots::theme_slot_primary_role(*slot)));
        Some(picked.unwrap_or((Bounds::new(point(px(0.), px(0.)), viewport), "canvas")))
    }

    fn apply_inspected_role(&mut self, role: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.inspecting = false;
        self.hovered_inspection = None;
        self.selected = Some(role);
        if !matches!(role, "canvas" | "accent") && !self.session.advanced {
            self.session.set_advanced(true);
        }
        let query = self.query.read(cx).value().to_string();
        if !query.trim().is_empty()
            && !FAMILIES.iter().find(|f| f.role == role).is_some_and(|f| f.matches(&query))
        {
            self.syncing = true;
            self.query.update(cx, |input, cx| input.set_value("", window, cx));
            self.syncing = false;
        }
        if let Some(focus) = self.swatches.get(role) {
            focus.focus(window, cx);
        }
        self.sync_inspector_state(cx);
        self.sync(window, cx);
    }

    fn matching_inspection_bounds(&self, role: &'static str, cx: &App) -> Vec<Bounds<Pixels>> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let target_hex = self.session.colors()[role].trim().to_ascii_lowercase();
        let family_roles: &[&str] = FAMILIES
            .iter()
            .find(|f| f.role == role)
            .map_or(&[], |f| f.roles);
        for &(slot, bounds) in theme::slots::inspector_entries(cx) {
            let slot_role = theme::slots::theme_slot_primary_role(slot);
            let matches = if self.session.advanced {
                slot_role == role || family_roles.contains(&slot_role)
            } else {
                slot_role == role
                    || self.session.colors()[slot_role].trim().to_ascii_lowercase() == target_hex
            };
            if !matches {
                continue;
            }
            let key = (
                f32::from(bounds.origin.x).round() as i32,
                f32::from(bounds.origin.y).round() as i32,
                f32::from(bounds.size.width).round() as i32,
                f32::from(bounds.size.height).round() as i32,
            );
            if seen.insert(key) {
                out.push(bounds);
            }
        }
        out
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
                let before_library = store::client(cx)
                    .entries()
                    .get(t3_core::themes::LIBRARY_KEY)
                    .cloned();
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            t3_client::store::commit_client_settings_rebased(patch, before_library)
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.saving = false;
                        let receipt = match result {
                            Ok(receipt) => receipt,
                            Err(e) => {
                                this.session.error = Some(e);
                                cx.notify();
                                return;
                            }
                        };
                        store::install_client_commit(&this.state, receipt, cx);
                        let (title, description) = if let Some(dark) = save.merged_dark {
                            (
                                format!("{} updated", save.theme.label),
                                format!(
                                    "Its {} palette was added.",
                                    if dark { "dark" } else { "light" }
                                ),
                            )
                        } else if save.created {
                            (
                                format!("{} created", save.theme.label),
                                "It’s now active.".into(),
                            )
                        } else {
                            let p = theme::preferences(cx);
                            let active = p.theme.named_id() == Some(&save.theme.id)
                                || p.halves.values().any(|id| id == &save.theme.id);
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
                                crate::toast::Toast::new(crate::toast::ToastKind::Success, title)
                                    .description(description),
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
        let swatch_bounds = self.swatch_bounds[role].clone();
        h_flex()
            .id(ElementId::Name(format!("theme-role-row-{role}").into()))
            .gap(r(8.))
            .px(r(8.))
            .py(r(6.))
            .rounded(r(8.))
            .cursor_pointer()
            .when(self.selected == Some(role), |d| {
                d.bg(hsla(p.accent).opacity(0.6))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_selected_role(role, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(hsla(p.muted_foreground))
                    .child(crate::i18n::t(cx, label).to_owned()),
            )
            .child(
                div()
                    .id(ElementId::Name(format!("choose-{role}").into()))
                    .relative()
                    .role(Role::Button)
                    .aria_label(crate::i18n::t(cx, label).to_owned())
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
                    .when(
                        self.swatches[role].is_focused(window) && window.last_input_was_keyboard(),
                        |d| d.border_2().border_color(hsla(p.ring)),
                    )
                    .bg(hsla(
                        picker::swatch(&self.session.colors()[role]).unwrap_or(p.background),
                    ))
                    .child(
                        canvas(
                            move |bounds, _, _| swatch_bounds.set(bounds),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| this.open_picker(role, window, cx))),
            )
            .child(
                div()
                    .w(r(112.))
                    .when(invalid, |d| {
                        d.border_1()
                            .rounded(r(6.))
                            .border_color(hsla(p.destructive))
                    })
                    .child(theme_input(&self.fields[role], true, cx)),
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
        let previous_bounds = self.panel_bounds.get();
        let width = self
            .dimensions
            .map_or(px(416.), |size| size.width)
            .min((viewport.width - px(16.)).max(px(0.)));
        let default_left = (viewport.width - width - px(16.)).max(px(8.));
        let desired_left = self
            .position
            .map_or(default_left, |point| point.x)
            .clamp(px(8.), (viewport.width - width - px(8.)).max(px(8.)));
        let picker_margin = px(236.);
        let needs_space_for_picker = self.picker.is_some()
            && desired_left < picker_margin
            && viewport.width - desired_left - width < picker_margin;
        let (left, width) = if needs_space_for_picker {
            (
                picker_margin,
                width.min((viewport.width - picker_margin - px(8.)).max(px(0.))),
            )
        } else {
            (desired_left, width)
        };
        let top = self.position.map(|point| {
            point
                .y
                .clamp(px(8.), (viewport.height - px(72.)).max(px(8.)))
        });
        let max_height = (viewport.height - top.unwrap_or(px(16.)) - px(16.)).max(px(0.));
        let height = self
            .dimensions
            .map(|size| size.height)
            .or_else(|| self.session.advanced.then_some(px(672.)))
            .map(|height| height.min(max_height));
        let panel_bounds = self.panel_bounds.clone();
        let title = crate::i18n::t(
            cx,
            if self.session.is_editing() {
                "Edit theme"
            } else {
                "Create theme"
            },
        );
        let mut body = v_flex().gap(r(12.)).px(r(12.)).py(r(12.)).child(
            h_flex()
                .gap(r(12.))
                .child(
                    div()
                        .w(r(116.))
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(crate::i18n::t(cx, "Theme name")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(theme_input(&self.name, false, cx)),
                ),
        );
        if let Some(e) = &self.session.error {
            body = body.child(div().text_sm().text_color(hsla(p.destructive)).child(e.clone()));
        }
        body = body
            .child(
                h_flex()
                    .gap(r(12.))
                    .child(
                        div()
                            .w(r(116.))
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(crate::i18n::t(cx, "Appearance")),
                    )
                    .child(
                        h_flex()
                            .gap(r(2.))
                            .p(r(2.))
                            .rounded(r(8.))
                            .bg(hsla(p.muted))
                            .children([false, true].into_iter().map(|dark| {
                                let reason = self.session.lock_reason(dark, store::client(cx));
                                let button = Button::new(
                                    if dark { "editor-dark" } else { "editor-light" },
                                    if dark == self.session.dark {
                                        ButtonVariant::Secondary
                                    } else {
                                        ButtonVariant::GhostMuted
                                    },
                                    ButtonSize::Xs,
                                )
                                .label(if dark { "Dark" } else { "Light" })
                                .icon(if dark { Icon::Moon } else { Icon::Sun })
                                .disabled(reason.is_some() || self.saving);
                                button
                                    .when_some(reason, |button, reason| button.tooltip(reason))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.end_picker(false, window, cx);
                                        this.session.set_appearance(dark, store::client(cx));
                                        this.sync(window, cx);
                                    }))
                            })),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        v_flex()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(crate::i18n::t(cx, "Colors")))
                            .when(!self.session.advanced, |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(crate::i18n::t(cx, "Two colors, rest derived")),
                                )
                            }),
                    )
                    .child(
                        h_flex().gap(r(8.)).child(crate::i18n::t(cx, "Advanced")).child(
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
            body = body.child(theme_input(&self.query, false, cx));
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
            body = body.child(div().text_xs().child(crate::i18n::t(cx, "No matches.")));
        }
        let css_toggle = Button::new("toggle-theme-css", ButtonVariant::Ghost, ButtonSize::Xs)
            .icon(if self.session.css_open { Icon::ChevronDown } else { Icon::ChevronRight })
            .label("Custom CSS")
            .on_click(cx.listener(|this, _, _, cx| {
                this.session.css_open = !this.session.css_open;
                cx.notify();
            }));
        let mut css_section = v_flex().gap(r(8.)).child(h_flex().child(css_toggle));
        if self.session.css_open {
            let css_value = self.css_input.read(cx).value().to_string();
            let css_range = self.css_input.read(cx).selected_range();
            let (line, col) = css::line_and_column(&css_value, css_range.start);
            let diag = css::summarize_diagnostics(&css_value);
            let code_font = theme::font_family(FontRole::Code, cx);
            let status_note: Option<(String, gpui::Hsla)> = if diag.exceeds_max_length {
                Some(("Exceeds 100,000 char limit".into(), hsla(p.destructive)))
            } else if diag.loads_remote_files {
                Some(("Remote fetches ignored".into(), hsla(p.warning_foreground)))
            } else if diag.applied_slot_rules > 0 && diag.unmapped_dom_rules > 0 {
                Some((
                    format!(
                        "{} slot {} · {} unsupported",
                        diag.applied_slot_rules,
                        if diag.applied_slot_rules == 1 { "rule" } else { "rules" },
                        diag.unmapped_dom_rules
                    ),
                    hsla(p.muted_foreground),
                ))
            } else if diag.applied_slot_rules > 0 {
                Some((
                    format!(
                        "{} slot {}",
                        diag.applied_slot_rules,
                        if diag.applied_slot_rules == 1 { "rule" } else { "rules" }
                    ),
                    hsla(p.muted_foreground),
                ))
            } else if diag.unmapped_dom_rules > 0 {
                Some((format!("{} unsupported", diag.unmapped_dom_rules), hsla(p.muted_foreground)))
            } else {
                None
            };
            let editor_box = v_flex()
                .relative()
                .rounded(r(8.))
                .border_1()
                .border_color(if diag.exceeds_max_length { hsla(p.destructive) } else { hsla(p.input) })
                .bg(hsla(p.code_background))
                .theme_slot(ThemeSlot::Input, cx)
                .children(theme_slot_clipped_background_layer(
                    ThemeSlot::Input,
                    r(8.),
                    cx,
                ))
                .overflow_hidden()
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if this.saving
                        || event.keystroke.modifiers.control
                        || event.keystroke.modifiers.platform
                        || event.keystroke.modifiers.alt
                    {
                        return;
                    }
                    match event.keystroke.key.as_str() {
                        "escape" if this.css_tab_indents => {
                            this.css_tab_indents = false;
                            cx.stop_propagation();
                            cx.notify();
                        }
                        "tab" if this.css_tab_indents => {
                            let value = this.css_input.read(cx).value().to_string();
                            let range = this.css_input.read(cx).selected_range();
                            if let Some(edit) = css::indent_selection(&value, range, event.keystroke.modifiers.shift) {
                                this.apply_css_edit(edit, window, cx);
                            }
                            cx.stop_propagation();
                        }
                        "enter" => {
                            let range = this.css_input.read(cx).selected_range();
                            if range.start == range.end {
                                let value = this.css_input.read(cx).value().to_string();
                                let edit = css::newline_with_indent(&value, range.start);
                                this.apply_css_edit(edit, window, cx);
                                cx.stop_propagation();
                            }
                        }
                        _ => {}
                    }
                }))
                .child(
                    div()
                        .h(r(180.))
                        .p(r(8.))
                        .font_family(code_font.clone())
                        .text_xs()
                        .text_color(hsla(p.code_foreground))
                        .child(Textarea::new(&self.css_input).appearance(false).bordered(false).disabled(self.saving)),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .gap(r(8.))
                        .border_t_1()
                        .border_color(hsla(p.input))
                        .px(r(12.))
                        .py(r(4.))
                        .font_family(code_font)
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(
                            h_flex()
                                .gap(r(8.))
                                .child("CSS")
                                .when_some(status_note, |d, (msg, color)| d.child(div().text_color(color).child(msg))),
                        )
                        .child(format!("Ln {line}, Col {col}")),
                );
            css_section = css_section
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child("Saved with this theme and included when you export it. Custom properties and theme CSS hooks apply live here. Rules that load remote files are ignored."),
                )
                .child(editor_box)
                .child(
                    h_flex()
                        .justify_between()
                        .items_start()
                        .gap(r(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(if self.css_tab_indents {
                                    "Tab indents. Shift+Tab unindents. Press Escape, then Tab to leave."
                                } else {
                                    "Tab moves to the next control."
                                }),
                        )
                        .child(
                            Button::new("clear-theme-css", ButtonVariant::Ghost, ButtonSize::Xs)
                                .label("Clear CSS")
                                .disabled(self.session.css.is_empty() || self.saving)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.apply_css_edit(
                                        css::EditResult {
                                            text: String::new(),
                                            selection: 0..0,
                                        },
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                );
        }
        body = body.child(css_section);
        let matched_bounds = self
            .selected
            .filter(|_| !self.inspecting)
            .map(|role| self.matching_inspection_bounds(role, cx))
            .unwrap_or_default();
        let inspector_subtitle: Option<String> = if self.minimized {
            None
        } else if self.inspecting {
            Some(crate::i18n::t(cx, "Select an element \u{00b7} Esc to cancel").to_owned())
        } else if let Some(role) = self.selected {
            let label = FAMILIES
                .iter()
                .find(|f| f.role == role)
                .map_or(role, |f| crate::i18n::t(cx, f.label));
            let count = matched_bounds.len() as i64;
            Some(crate::i18n::t_fmt(
                cx,
                "{label} \u{00b7} {count, plural, one {{count} use} other {{count} uses}}",
                &[
                    ("label", label.into()),
                    ("count", t3_core::i18n::IcuArg::Int(count)),
                ],
            ))
        } else {
            None
        };
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
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .items_baseline()
                            .child(div().flex_none().font_weight(FontWeight::MEDIUM).child(title))
                            .when_some(inspector_subtitle, |row, subtitle| {
                                row.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(subtitle),
                                )
                            }),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            this.drag = Some((e.position, this.panel_bounds.get().origin, false));
                            cx.notify();
                        }),
                    ),
            )
            .child(
                Button::new(
                    "inspect-theme-colors",
                    if self.inspecting { ButtonVariant::Secondary } else { ButtonVariant::Ghost },
                    ButtonSize::Xs,
                )
                .icon(Icon::MousePointerClick)
                .label(if self.inspecting { "Cancel" } else { "Inspect" })
                .tooltip(if self.inspecting {
                    "Cancel inspecting app colors"
                } else {
                    "Pick a color from the app"
                })
                .disabled(self.saving)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_inspecting(window, cx))),
            )
            .child(
                Button::new("minimize-editor", ButtonVariant::Ghost, ButtonSize::IconXs)
                    .icon(if self.minimized { Icon::ChevronUp } else { Icon::ChevronDown })
                    .tooltip(if self.minimized {
                        "Expand the theme editor"
                    } else {
                        "Minimize the theme editor"
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.end_picker(false, window, cx);
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
            .pl(r(12.))
            .pr(r(24.))
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
            .left(left)
            .map(|panel| {
                if let Some(top) = top {
                    panel.top(top)
                } else {
                    panel.bottom(px(16.))
                }
            })
            .w(width)
            .max_h(max_height)
            .when_some(height.filter(|_| !self.minimized), |panel, height| {
                panel.h(height)
            })
            .rounded(r(12.))
            .overflow_hidden()
            .occlude()
            .text_sm()
            .text_color(hsla(p.popover_foreground))
            .when(!slot_has_background(ThemeSlot::DialogPopup, cx), |panel| {
                panel.child(crate::glass::backdrop(
                    12.,
                    hsla(p.dialog_surface),
                    hsla(p.dialog_border),
                    cx,
                ))
            })
            .when(slot_has_background(ThemeSlot::DialogPopup, cx), |panel| {
                panel
                    .bg(hsla(p.dialog_surface))
                    .border_1()
                    .border_color(hsla(p.dialog_border))
            })
            .theme_slot(ThemeSlot::DialogPopup, cx)
            .children(theme_slot_clipped_background_layer(
                ThemeSlot::DialogPopup,
                r(12.),
                cx,
            ))
            .child(
                canvas(
                    move |bounds, _, _| panel_bounds.set(bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(header)
            .when(!self.minimized, |d| {
                d.child(
                    div()
                        .id("theme-editor-scroll")
                        .track_scroll(&self.scroll)
                        .flex_auto()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(body),
                )
                .child(footer)
                .child(
                    div()
                        .id("theme-editor-resize")
                        .absolute()
                        .bottom_0()
                        .right_0()
                        .size(r(18.))
                        .cursor(CursorStyle::ResizeUpLeftDownRight)
                        .child(resize_grip(hsla(p.muted_foreground)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                let bounds = this.panel_bounds.get();
                                this.drag = Some((
                                    e.position,
                                    point(bounds.size.width, bounds.size.height),
                                    true,
                                ));
                                this.position = Some(bounds.origin);
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ),
                )
            })
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" {
                    if this.picker.is_some() {
                        this.end_picker(true, window, cx);
                        cx.stop_propagation();
                    } else if this.inspecting || this.selected.is_some() {
                        this.clear_inspector_selection(cx);
                        cx.stop_propagation();
                    } else if !this.saving {
                        close(cx);
                        cx.stop_propagation();
                    }
                }
            }));
        let hover_target = self.hovered_inspection;
        let hover_label = hover_target.map(|(_, role)| {
            FAMILIES
                .iter()
                .find(|f| f.role == role)
                .map_or(role, |f| crate::i18n::t(cx, f.label))
                .to_owned()
        });
        let hover_badge = hover_target.zip(hover_label).map(|((bounds, _), label)| {
            let below = f32::from(bounds.top()) < 32.;
            let top = if below { bounds.bottom() + px(6.) } else { (bounds.top() - px(28.)).max(px(6.)) };
            let left = bounds.left().clamp(px(8.), (viewport.width - px(180.)).max(px(8.)));
            div()
                .absolute()
                .top(top)
                .left(left)
                .px(px(8.))
                .py(px(3.))
                .rounded(px(6.))
                .bg(hsla(p.primary))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.primary_foreground))
                .shadow_md()
                .child(label)
        });
        let inspector_layer: Option<AnyElement> = if self.inspecting {
            let ring_color = hsla(p.primary);
            let fill_color = ring_color.opacity(0.12);
            let vp = viewport;
            Some(
                div()
                    .id("theme-inspector-capture")
                    .debug_selector(|| "theme-inspector-capture".into())
                    .absolute()
                    .inset_0()
                    .occlude()
                    .cursor(CursorStyle::Crosshair)
                    .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
                        let next = Self::pick_inspection_target(e.position, vp, this.panel_bounds.get(), cx);
                        if this.hovered_inspection != next {
                            this.hovered_inspection = next;
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            if let Some((_, role)) = Self::pick_inspection_target(e.position, vp, this.panel_bounds.get(), cx) {
                                this.apply_inspected_role(role, window, cx);
                            }
                        }),
                    )
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |_, (), window, _| {
                                if let Some((bounds, _)) = hover_target {
                                    window.paint_quad(
                                        gpui::fill(bounds, fill_color)
                                            .corner_radii(px(6.))
                                            .border_widths(px(2.))
                                            .border_color(ring_color),
                                    );
                                }
                            },
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .children(hover_badge)
                    .into_any_element(),
            )
        } else if !matched_bounds.is_empty() {
            let ring_color = hsla(p.primary).opacity(0.85);
            let fill_color = hsla(p.primary).opacity(0.08);
            Some(
                canvas(
                    |_, _, _| {},
                    move |_, (), window, _| {
                        for bounds in &matched_bounds {
                            let padded = Bounds::new(
                                bounds.origin - point(px(3.), px(3.)),
                                bounds.size + size(px(6.), px(6.)),
                            );
                            window.paint_quad(
                                gpui::fill(padded, fill_color)
                                    .corner_radii(px(8.))
                                    .border_widths(px(2.))
                                    .border_color(ring_color),
                            );
                        }
                    },
                )
                .absolute()
                .inset_0()
                .into_any_element(),
            )
        } else {
            None
        };
        div()
            .absolute()
            .inset_0()
            .children(inspector_layer)
            .child(panel)
            .children(self.render_picker(left, width, window, cx))
            .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
                if e.pressed_button != Some(MouseButton::Left) {
                    this.drag = None;
                    return;
                }
                if let Some((start, original, resize)) = this.drag {
                    let delta = e.position - start;
                    if resize {
                        this.dimensions = Some(size(
                            (original.x + delta.x)
                                .max(px(280.))
                                .min(viewport.width - previous_bounds.left() - px(8.)),
                            (original.y + delta.y)
                                .max(px(220.))
                                .min(viewport.height - previous_bounds.top() - px(8.)),
                        ));
                    } else {
                        this.position = Some(original + delta);
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.drag = None))
    }
}

fn theme_input(state: &Entity<InputState>, mono: bool, cx: &App) -> gpui::Div {
    let layer = theme_slot_clipped_background_layer(ThemeSlot::Input, r(8.), cx);
    let custom = layer.is_some();
    div()
        .relative()
        .w_full()
        .when(mono, |field| {
            field.font_family(theme::font_family(FontRole::Code, cx))
        })
        .children(layer)
        .child(
            Input::new(state)
                .appearance(!custom)
                .small()
                .theme_slot(ThemeSlot::Input, cx),
        )
}

fn resize_grip(ink: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let scale = window.rem_size() / 16.;
            let mut path = PathBuilder::stroke(scale);
            for offset in [7., 11.] {
                path.move_to(point(
                    bounds.right() - scale * offset,
                    bounds.bottom() - scale * 3.,
                ));
                path.line_to(point(
                    bounds.right() - scale * 3.,
                    bounds.bottom() - scale * offset,
                ));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, ink);
            }
        },
    )
    .size_full()
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
    if let Some(raw_css) = value.get("css").and_then(serde_json::Value::as_str) {
        session.set_css(raw_css.into());
        session.css_open = !raw_css.is_empty();
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

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::{Editor, Host, Session, close, init};
    use crate::{
        app_state::AppState,
        settings::store,
        theme::{self, palette},
    };
    use gpui::AppContext;
    use serde_json::json;
    use t3_core::themes::{self, css};

    #[gpui::test]
    fn custom_css_editor_previews_live_and_restores_on_close(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            theme::init(cx);
            store::init(cx);
            cx.set_global(
                serde_json::from_value::<crate::chatgpt_setup::Fixture>(json!({
                    "provider": {"instanceId":"test","driver":"test","enabled":false,"installed":false,"status":"unavailable"},
                    "auth": {"phase":"idle"},
                    "install": {"phase":"idle"}
                }))
                .unwrap(),
            );
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            init(state.clone(), cx);
            state
        });
        let saved_bg = cx.update(|cx| palette(cx).background);
        let seed = themes::decode(
            &json!({
                "version": 1,
                "name": "Styled",
                "appearance": "dark",
                "colors": {"canvas": "#111111"},
                "css": ".dark { --background: #141820; --primary: #33ddaa; }"
            }),
            false,
        )
        .unwrap();
        let session = Session::new(Some(seed), None, None, true);
        assert!(session.css_open, "themes with CSS open the Custom CSS editor automatically");

        let window = cx.add_window(|window, cx| {
            let editor = Editor::new(state.clone(), session, window, cx);
            cx.global_mut::<Host>().view = Some(cx.entity());
            editor
        });
        cx.run_until_parked();

        cx.update(|cx| {
            assert_eq!(palette(cx).background, 0x1418_20ff);
            assert_eq!(palette(cx).primary, 0x33dd_aaff);
        });

        window
            .update(cx, |editor, window, cx| {
                editor.apply_css_edit(
                    css::EditResult {
                        text: ".dark { --background: #201424; --primary: #ff6688; }".into(),
                        selection: 0..0,
                    },
                    window,
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();

        cx.update(|cx| {
            assert_eq!(palette(cx).background, 0x2014_24ff);
            assert_eq!(palette(cx).primary, 0xff66_88ff);
            close(cx);
            assert_eq!(palette(cx).background, saved_bg);
        });
        std::mem::forget(runtime);
    }

    #[gpui::test]
    fn color_inspector_picks_innermost_slot_suppresses_click_and_cancels_on_escape(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{MouseButton, MouseDownEvent, div, point, prelude::*, px, size};
        use std::{cell::Cell, rc::Rc};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            theme::init(cx);
            store::init(cx);
            cx.set_global(
                serde_json::from_value::<crate::chatgpt_setup::Fixture>(json!({
                    "provider": {"instanceId":"test","driver":"test","enabled":false,"installed":false,"status":"unavailable"},
                    "auth": {"phase":"idle"},
                    "install": {"phase":"idle"}
                }))
                .unwrap(),
            );
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            init(state.clone(), cx);
            state
        });
        assert!(cx.update(|cx| !theme::slots::inspector_active(cx)));

        struct HostHarness {
            editor: gpui::Entity<Editor>,
            underlying_clicked: Rc<Cell<bool>>,
        }
        impl gpui::Render for HostHarness {
            fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
                let clicked = self.underlying_clicked.clone();
                crate::theme::slots::begin_inspector_frame(cx);
                div()
                    .size_full()
                    .child(
                        div()
                            .id("harness-sidebar")
                            .relative()
                            .w(px(300.))
                            .h(px(400.))
                            .children(crate::theme::inspector_slot_probe(crate::theme::ThemeSlot::Sidebar, cx))
                            .child(
                                div()
                                    .id("harness-button")
                                    .relative()
                                    .mt(px(40.))
                                    .ml(px(40.))
                                    .w(px(100.))
                                    .h(px(40.))
                                    .children(crate::theme::inspector_slot_probe(crate::theme::ThemeSlot::Button, cx))
                                    .on_mouse_down(MouseButton::Left, move |_, _, _| clicked.set(true)),
                            ),
                    )
                    .child(self.editor.clone())
            }
        }

        let underlying_clicked = Rc::new(Cell::new(false));
        let (harness, vcx) = cx.add_window_view(|window, cx| {
            let session = Session::new(None, None, None, true);
            let editor = cx.new(|cx| Editor::new(state.clone(), session, window, cx));
            cx.global_mut::<Host>().view = Some(editor.clone());
            HostHarness { editor, underlying_clicked: underlying_clicked.clone() }
        });
        let editor = harness.read_with(vcx, |h, _| h.editor.clone());
        vcx.run_until_parked();

        // Start inspect mode and draw one frame to collect slot bounds.
        vcx.update(|window, cx| {
            editor.update(cx, |ed, cx| ed.toggle_inspecting(window, cx));
        });
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.update(|_, cx| theme::slots::inspector_active(cx)));
        assert!(!vcx.update(|_, cx| theme::slots::inspector_entries(cx).is_empty()));

        // Click inside the nested Button slot (at 80, 55): picks "messageAction" (Button) over "sidebar"
        // and blocks underlying click handler.
        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: point(px(80.), px(55.)),
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.run_until_parked();
        assert!(!underlying_clicked.get(), "inspector must block clicks to underlying app");
        editor.read_with(vcx, |ed, _| {
            assert!(!ed.inspecting);
            assert_eq!(ed.selected, Some("messageAction"));
            assert!(ed.session.advanced);
        });

        // Escape clears the spotlight selection without closing the Theme Editor.
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        editor.read_with(vcx, |ed, cx| {
            assert!(ed.selected.is_none());
            assert!(!theme::slots::inspector_active(cx));
        });
        assert!(vcx.update(|_, cx| cx.global::<Host>().view.is_some()));

        // Close clears inspector state too.
        vcx.update(|_, cx| {
            close(cx);
            assert!(!theme::slots::inspector_active(cx));
        });
        let _ = size(px(0.), px(0.));
        std::mem::forget(runtime);
    }
}
