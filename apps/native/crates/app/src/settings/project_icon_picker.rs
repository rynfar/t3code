//! `web/src/components/settings/ProjectIconPickerDialog.tsx` and
//! `ProjectFaviconPickerDialog.tsx`; saves through the Projects page.

use super::{
    SettingsView,
    controls::{Button, ButtonSize, ButtonVariant, DialogFrame, dialog_footer, focus_handle, focus_ring, icon, r, tooltip},
    store,
};
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, PathPromptOptions, SharedString, Styled,
    Subscription, Window, div, prelude::*,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use t3_core::settings::projects::{
    self as model, ProjectIcon, ProjectUpdate,
    icon::{self as project_icon, COLORS},
    picker::{self, Draft, Mode},
};

pub(super) struct Picker {
    pub target: super::projects::IconTarget,
    kind: PickerKind,
    focus: FocusHandle,
    previous: Option<FocusHandle>,
    _subscriptions: Vec<Subscription>,
}

enum PickerKind {
    Icon(IconPicker),
    File(FilePicker),
}

struct IconPicker {
    draft: Draft,
    automatic: ProjectIcon,
    query: Entity<InputState>,
    letters: Entity<InputState>,
    custom: Entity<InputState>,
}

struct FilePicker {
    query: Entity<InputState>,
    entries: Vec<String>,
    selected: Option<String>,
    loading: bool,
    error: Option<String>,
    search_seq: u64,
    workspace_root: String,
    can_browse_local: bool,
}

impl Picker {
    pub(super) fn text_field_focused(&self, window: &Window, cx: &App) -> bool {
        match &self.kind {
            PickerKind::Icon(icon) => [&icon.query, &icon.letters, &icon.custom]
                .iter()
                .any(|input| input.read(cx).focus_handle(cx).is_focused(window)),
            PickerKind::File(file) => file.query.read(cx).focus_handle(cx).is_focused(window),
        }
    }
}

impl SettingsView {
    pub(super) fn open_project_icon_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit_projects(cx) || self.projects.saving_icon {
            return;
        }
        let Some(shown) = self.shown_project(cx) else { return };
        let current = ProjectIcon::decode(shown.members[0].project_icon.as_ref());
        let draft = Draft::new(current.as_ref(), &shown.members[0].title);
        let target = self.icon_target(cx);
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search available Lucide icons"));
        let letters = cx.new(|cx| InputState::new(window, cx).default_value(draft.letters.clone()));
        let custom = cx.new(|cx| InputState::new(window, cx).placeholder("Paste an emoji"));
        #[cfg(feature = "snapshot")]
        for (name, input) in [
            ("project-icon-query", &query),
            ("project-icon-letters", &letters),
            ("project-icon-custom", &custom),
        ] {
            crate::snapshot::register_focus(name, input.read(cx).focus_handle(cx), cx);
        }
        let subscriptions = vec![
            cx.subscribe(&query, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&letters, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(Picker { kind: PickerKind::Icon(icon), .. }) = &mut this.projects.picker
                {
                    icon.draft.letters = input.read(cx).value().to_string();
                    cx.notify();
                }
            }),
            cx.subscribe(&custom, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(Picker { kind: PickerKind::Icon(icon), .. }) = &mut this.projects.picker
                {
                    icon.draft.custom_emoji(&input.read(cx).value());
                    cx.notify();
                }
            }),
        ];
        let previous = window.focused(cx);
        match draft.mode {
            Mode::Lucide => &query,
            Mode::Emoji => &custom,
            Mode::Monogram => &letters,
        }
        .update(cx, |input, cx| input.focus(window, cx));
        self.projects.picker = Some(Picker {
            target,
            kind: PickerKind::Icon(IconPicker {
                draft,
                automatic: project_icon::identity(&shown.members[0].title),
                query,
                letters,
                custom,
            }),
            focus: cx.focus_handle(),
            previous,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    pub(super) fn open_project_favicon_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit_projects(cx) || self.projects.saving_icon {
            return;
        }
        let Some(shown) = self.shown_project(cx) else { return };
        let target = self.icon_target(cx);
        let workspace_root = shown.targets[0].project.workspace_root.clone();
        let selected = shown.members[0].favicon_path.clone();
        let can_browse_local = target.members.len() == 1 && std::path::Path::new(&workspace_root).is_dir();
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Filter project image files"));
        let subscriptions = vec![cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.search_project_favicon_files(cx);
            }
        })];
        let previous = window.focused(cx);
        query.update(cx, |input, cx| input.focus(window, cx));
        self.projects.picker = Some(Picker {
            target,
            kind: PickerKind::File(FilePicker {
                query,
                entries: Vec::new(),
                selected,
                loading: true,
                error: None,
                search_seq: 0,
                workspace_root,
                can_browse_local,
            }),
            focus: cx.focus_handle(),
            previous,
            _subscriptions: subscriptions,
        });
        self.search_project_favicon_files(cx);
        cx.notify();
    }

    fn search_project_favicon_files(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.projects.picker.as_mut() else { return };
        let Some(project) = picker.target.members.first().cloned() else { return };
        let PickerKind::File(file) = &mut picker.kind else { return };
        file.search_seq += 1;
        let seq = file.search_seq;
        let query = file.query.read(cx).value().to_string();
        if !store::project_current(self.state.read(cx), &project, "filesystem:read") {
            file.loading = false;
            file.entries.clear();
            file.error = Some("This connection cannot browse project files.".into());
            cx.notify();
            return;
        }
        file.loading = true;
        file.error = None;
        let workspace_root = file.workspace_root.clone();
        let lookup = self.state.read(cx).connections.destination(&project.owner, "filesystem:read");
        let state = self.state.clone();
        cx.spawn(async move |this, cx| {
            let result = match lookup.await {
                Ok(destination) => {
                    let runtime = state.read_with(cx, |state, _| state.runtime().clone());
                    let request = model::favicon_search_request(&workspace_root, &query);
                    match runtime.spawn(async move { destination.client.request(model::SEARCH_ENTRIES, request).await }).await {
                        Ok(Ok(reply)) => Ok(model::decode_favicon_entries(&reply)),
                        Ok(Err(err)) => Err(err.to_string()),
                        Err(err) => Err(err.to_string()),
                    }
                }
                Err(err) => Err(format!("{err:?}")),
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(Picker { kind: PickerKind::File(file), .. }) = &mut this.projects.picker
                    && file.search_seq == seq
                {
                    file.loading = false;
                    match result {
                        Ok(entries) => {
                            file.entries = entries;
                            file.error = None;
                        }
                        Err(err) => {
                            file.entries.clear();
                            file.error = Some(err);
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn browse_local_favicon_file(&mut self, cx: &mut Context<Self>) {
        let Some(Picker { kind: PickerKind::File(file), .. }) = self.projects.picker.as_ref() else { return };
        let workspace_root = file.workspace_root.clone();
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose project icon image".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else { return };
            let Some(chosen) = paths.into_iter().next() else { return };
            let relative = chosen
                .strip_prefix(&workspace_root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| chosen.to_string_lossy().into_owned());
            let _ = this.update(cx, |this, cx| {
                if let Some(Picker { kind: PickerKind::File(file), .. }) = &mut this.projects.picker {
                    file.selected = Some(relative);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn dismiss_project_icon_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(picker) = self.projects.picker.take() else { return false };
        if let Some(focus) = picker.previous {
            focus.focus(window, cx);
        }
        cx.notify();
        true
    }

    pub(super) fn cycle_icon_focus(&self, forward: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(picker) = &self.projects.picker else { return false };
        for _ in 0..512 {
            if forward {
                window.focus_next(cx);
            } else {
                window.focus_prev(cx);
            }
            if picker.focus.contains_focused(window, cx) && !picker.focus.is_focused(window) {
                return true;
            }
        }
        picker.focus.focus(window, cx);
        true
    }

    fn save_project_icon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.projects.picker.as_ref() else { return };
        let target = picker.target.clone();
        let update = match &picker.kind {
            PickerKind::Icon(icon) => {
                let Some(selected) = icon.draft.selected() else { return };
                picker::update(selected)
            }
            PickerKind::File(file) => {
                let Some(path) = file.selected.clone() else { return };
                ProjectUpdate::favicon_file(path)
            }
        };
        if !target.allows(&self.icon_target(cx), self.can_edit_projects(cx), self.projects.saving_icon) {
            return;
        }
        self.dismiss_project_icon_picker(window, cx);
        let updates = target.members.iter().map(|project| (project.clone(), update.clone())).collect();
        self.save_icon_updates(target, updates, cx);
    }

    pub(super) fn project_icon_picker_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.section != t3_core::settings::search::Section::Projects {
            self.dismiss_project_icon_picker(window, cx);
            return None;
        }
        let target = self.projects.picker.as_ref()?.target.clone();
        if !self
            .shown_project(cx)
            .is_some_and(|_| target.allows(&self.icon_target(cx), self.can_edit_projects(cx), self.projects.saving_icon))
        {
            self.dismiss_project_icon_picker(window, cx);
            return None;
        }
        let picker = self.projects.picker.as_ref()?;
        let p = palette(cx);
        let (frame_id, title, description, max_width, can_save, body) = match &picker.kind {
            PickerKind::Icon(icon_picker) => {
                let mode = icon_picker.draft.mode;
                let selected = icon_picker.draft.selected();
                let fallback = match &icon_picker.automatic {
                    ProjectIcon::Monogram { text, .. } => ProjectIcon::Monogram {
                        text: text.clone(),
                        color: icon_picker.draft.color.clone(),
                    },
                    icon => icon.clone(),
                };
                let preview = selected.as_ref().unwrap_or(&fallback);
                let mut body = v_flex().gap(r(16.)).child(
                    h_flex().gap(r(2.)).children(
                        [(Mode::Lucide, "Icons"), (Mode::Emoji, "Emoji"), (Mode::Monogram, "Monogram")]
                            .into_iter()
                            .map(|(next, label)| {
                                Button::new(
                                    SharedString::from(format!("project-icon-mode-{label}")),
                                    if mode == next { ButtonVariant::Secondary } else { ButtonVariant::GhostMuted },
                                    ButtonSize::Compact,
                                )
                                .label(label)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some(Picker { kind: PickerKind::Icon(icon), .. }) = &mut this.projects.picker {
                                        icon.draft.mode = next;
                                        match next {
                                            Mode::Lucide => &icon.query,
                                            Mode::Emoji => &icon.custom,
                                            Mode::Monogram => &icon.letters,
                                        }
                                        .update(cx, |input, cx| input.focus(window, cx));
                                    }
                                    cx.notify();
                                }))
                            }),
                    ),
                );
                if mode != Mode::Emoji {
                    let color = icon_picker.draft.color.clone();
                    body = body.child(
                        v_flex()
                            .gap(r(8.))
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Color"))
                            .child(h_flex().flex_wrap().gap(r(6.)).children(COLORS.into_iter().map(|value| {
                                let id = gpui::ElementId::Name(format!("project-icon-color-{value}").into());
                                let focus = focus_handle(&id, window, cx);
                                let ring = focus.is_focused(window) && window.last_input_was_keyboard();
                                div()
                                    .id(id)
                                    .size(r(24.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(if color == value {
                                        hsla(p.foreground).opacity(0.64)
                                    } else {
                                        gpui::transparent_black()
                                    })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .track_focus(&focus)
                                    .when(ring, |v| v.shadow(focus_ring(hsla(p.ring))))
                                    .tooltip(tooltip(value.into()))
                                    .child(div().size(r(16.)).rounded_full().bg(crate::project_icon::color_token(value, p)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(Picker { kind: PickerKind::Icon(icon), .. }) = &mut this.projects.picker {
                                            icon.draft.color = value.into();
                                        }
                                        cx.notify();
                                    }))
                            }))),
                    );
                }
                let PickerKind::Icon(icon_picker) = &self.projects.picker.as_ref()?.kind else { return None };
                match mode {
                    Mode::Lucide => {
                        let names = picker::filter_names(&icon_picker.query.read(cx).value(), CATALOG);
                        let color = icon_picker.draft.color.clone();
                        let chosen = icon_picker.draft.icon_name.clone();
                        body = body.child(Input::new(&icon_picker.query)).child(
                            div()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(format!("{} Lucide icons available in native.", CATALOG.len())),
                        );
                        if !CATALOG.contains(&chosen.as_str()) {
                            body = body.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(format!(
                                "Saved icon “{chosen}” is outside the native catalog. It is preserved until you choose another icon."
                            )));
                        }
                        let mut grid = v_flex().gap(r(4.));
                        if names.is_empty() {
                            grid = grid.child(div().py(r(32.)).text_sm().text_color(hsla(p.muted_foreground)).child("No icons found."));
                        }
                        for row in names.chunks(10) {
                            grid = grid.child(h_flex().gap(r(4.)).children(row.iter().map(|name| {
                                let name = *name;
                                let icon = ProjectIcon::Lucide {
                                    name: name.into(),
                                    color: color.clone(),
                                };
                                cell(format!("project-icon-{name}"), name == chosen, name, window, cx)
                                    .child(crate::project_icon::preview(&icon, crate::project_icon::size(20., cx), cx))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(Picker { kind: PickerKind::Icon(icon), .. }) = &mut this.projects.picker {
                                            icon.draft.icon_name = name.into();
                                        }
                                        cx.notify();
                                    }))
                            })));
                        }
                        body = body.child(div().id("project-icon-results").max_h(r(256.)).overflow_y_scroll().child(grid));
                    }
                    Mode::Emoji => {
                        let mut grid = v_flex().gap(r(4.));
                        for row in picker::EMOJIS.chunks(10) {
                            grid = grid.child(h_flex().gap(r(4.)).children(row.iter().map(|(emoji, label)| {
                                cell(format!("project-emoji-{label}"), icon_picker.draft.emoji == *emoji, label, window, cx)
                                    .text_size(r(20.))
                                    .child(*emoji)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(Picker { kind: PickerKind::Icon(icon), .. }) = &mut this.projects.picker {
                                            icon.draft.emoji = (*emoji).into();
                                        }
                                        cx.notify();
                                    }))
                            })));
                        }
                        body = body.child(grid).child(
                            v_flex()
                                .gap(r(8.))
                                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Or paste any emoji"))
                                .child(Input::new(&icon_picker.custom)),
                        );
                    }
                    Mode::Monogram => {
                        body = body.child(
                            v_flex()
                                .gap(r(8.))
                                .child(div().text_sm().child("Letters"))
                                .child(Input::new(&icon_picker.letters))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(hsla(if selected.is_some() { p.muted_foreground } else { p.error_foreground }))
                                        .child("One or two letters or numbers."),
                                ),
                        );
                    }
                }
                body = body.child(
                    h_flex()
                        .gap(r(16.))
                        .child(crate::project_icon::preview(preview, crate::project_icon::size(48., cx), cx))
                        .child(div().text_sm().text_color(hsla(p.muted_foreground)).child("Preview")),
                );
                (
                    "project-icon-picker",
                    "Choose project icon",
                    "Choose an icon, emoji, or monogram.",
                    512.,
                    selected.is_some(),
                    body.into_any_element(),
                )
            }
            PickerKind::File(file_picker) => {
                let query_text = file_picker.query.read(cx).value().to_string();
                let selected = file_picker.selected.clone();
                let mut header = h_flex().gap(r(8.)).child(div().flex_1().child(Input::new(&file_picker.query)));
                if file_picker.can_browse_local {
                    header = header.child(
                        Button::new("project-favicon-browse", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Browse…")
                            .on_click(cx.listener(|this, _, _, cx| this.browse_local_favicon_file(cx))),
                    );
                }
                let mut list = v_flex().gap(r(2.));
                if file_picker.loading {
                    list = list.child(div().py(r(24.)).text_sm().text_color(hsla(p.muted_foreground)).child("Searching project image files…"));
                } else if let Some(err) = &file_picker.error {
                    list = list.child(div().py(r(12.)).text_sm().text_color(hsla(p.error_foreground)).child(err.clone()));
                } else if file_picker.entries.is_empty() {
                    let empty = if query_text.trim().is_empty() {
                        "No image files found in this project."
                    } else {
                        "No matching image files."
                    };
                    list = list.child(div().py(r(24.)).text_sm().text_color(hsla(p.muted_foreground)).child(empty));
                } else {
                    for (idx, path) in file_picker.entries.iter().enumerate() {
                        let is_selected = selected.as_deref() == Some(path.as_str());
                        let chosen = path.clone();
                        list = list.child(
                            h_flex()
                                .id(SharedString::from(format!("project-favicon-entry-{idx}")))
                                .w_full()
                                .px(r(10.))
                                .py(r(6.))
                                .gap(r(8.))
                                .rounded(r(6.))
                                .cursor_pointer()
                                .when(is_selected, |v| v.bg(hsla(p.accent)))
                                .hover(|v| v.bg(hsla(p.accent).opacity(0.6)))
                                .child(div().flex_1().min_w_0().truncate().text_sm().text_color(hsla(p.foreground)).child(path.clone()))
                                .when(is_selected, |v| v.child(icon(Icon::Check, 14., hsla(p.foreground))))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(Picker { kind: PickerKind::File(file), .. }) = &mut this.projects.picker {
                                        file.selected = Some(chosen.clone());
                                    }
                                    cx.notify();
                                })),
                        );
                    }
                }
                let mut body = v_flex()
                    .gap(r(12.))
                    .child(header)
                    .child(div().id("project-favicon-results").max_h(r(260.)).overflow_y_scroll().child(list));
                if let Some(path) = &selected {
                    body = body.child(
                        div()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .truncate()
                            .child(format!("Selected: {path}")),
                    );
                }
                (
                    "project-favicon-picker",
                    "Choose project icon file",
                    "Pick an image file from this project to use as its icon.",
                    512.,
                    selected.is_some(),
                    body.into_any_element(),
                )
            }
        };
        let footer = dialog_footer(
            false,
            vec![
                Button::new("project-icon-cancel", ButtonVariant::Outline, ButtonSize::Default)
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.dismiss_project_icon_picker(window, cx);
                    }))
                    .into_any_element(),
                Button::new("project-icon-save", ButtonVariant::Default, ButtonSize::Default)
                    .label("Save icon")
                    .disabled(!can_save)
                    .on_click(cx.listener(|this, _, window, cx| this.save_project_icon(window, cx)))
                    .into_any_element(),
            ],
            cx,
        );
        let focus = self.projects.picker.as_ref()?.focus.clone();
        let frame = DialogFrame {
            id: frame_id,
            title: title.into(),
            description: description.into(),
            max_width,
            body,
            footer: footer.into_any_element(),
            focus,
        };
        let weak = cx.entity().downgrade();
        Some(
            div()
                .key_context("SettingsModal")
                .capture_action(cx.listener(|this, _: &gpui_component::input::Escape, window, cx| {
                    this.dismiss_project_icon_picker(window, cx);
                    cx.stop_propagation();
                }))
                .capture_action(cx.listener(|this, _: &gpui_base::input::IndentInline, window, cx| {
                    this.cycle_icon_focus(true, window, cx);
                    cx.stop_propagation();
                }))
                .capture_action(cx.listener(|this, _: &gpui_base::input::OutdentInline, window, cx| {
                    this.cycle_icon_focus(false, window, cx);
                    cx.stop_propagation();
                }))
                .child(frame.render_modal(
                    window,
                    52,
                    move |window, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.dismiss_project_icon_picker(window, cx);
                        });
                    },
                    cx,
                ))
                .into_any_element(),
        )
    }
}

fn cell(id: String, selected: bool, label: &str, window: &mut Window, cx: &mut App) -> gpui::Stateful<gpui::Div> {
    let p = palette(cx);
    let id = gpui::ElementId::Name(id.into());
    let focus = focus_handle(&id, window, cx);
    let focused = focus.is_focused(window);
    div()
        .id(id)
        .size(r(40.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(r(6.))
        .border_1()
        .border_color(if selected { hsla(p.border) } else { gpui::transparent_black() })
        .when(selected, |v| v.bg(hsla(p.accent)))
        .hover(|v| v.bg(hsla(p.accent)))
        .cursor_pointer()
        .track_focus(&focus)
        .when(focused, |v| v.shadow(focus_ring(hsla(p.ring))))
        .tooltip(tooltip(label.to_owned().into()))
}

const CATALOG: &[&str] = &[
    "alarm-clock",
    "alarm-clock-off",
    "archive",
    "archive-x",
    "arrow-down",
    "arrow-down-up",
    "arrow-left",
    "arrow-right",
    "arrow-right-left",
    "arrow-up",
    "arrow-up-left",
    "arrow-up-right",
    "blocks",
    "bookmark",
    "bot",
    "bug",
    "building",
    "calendar-clock",
    "chart-no-axes-column",
    "check",
    "chevron-down",
    "chevron-left",
    "chevron-right",
    "chevron-up",
    "chevrons-down-up",
    "chevrons-left-right-ellipsis",
    "chevrons-up-down",
    "circle-alert",
    "circle-arrow-up",
    "circle-check",
    "circle-dashed",
    "circle-dot",
    "circle-dot-dashed",
    "circle-slash",
    "circle-stop",
    "circle-x",
    "clock",
    "cloud",
    "cloud-download",
    "cloud-upload",
    "code-xml",
    "columns-2",
    "copy",
    "corner-left-up",
    "corner-up-right",
    "download",
    "earth",
    "ellipsis",
    "external-link",
    "eye",
    "file",
    "file-braces",
    "file-diff",
    "file-minus",
    "file-plus",
    "file-symlink",
    "file-text",
    "files",
    "flask-conical",
    "folder",
    "folder-code",
    "folder-git",
    "folder-git-2",
    "folder-open",
    "folder-plus",
    "folder-tree",
    "gauge",
    "git-branch",
    "git-branch-plus",
    "git-commit-horizontal",
    "git-fork",
    "git-merge",
    "git-pull-request",
    "git-pull-request-arrow",
    "git-pull-request-closed",
    "git-pull-request-draft",
    "globe",
    "hammer",
    "hash",
    "history",
    "house",
    "image",
    "info",
    "key-round",
    "keyboard",
    "laptop",
    "link",
    "link-2",
    "list-checks",
    "list-filter",
    "list-plus",
    "list-tree",
    "loader-circle",
    "lock",
    "lock-open",
    "mail-open",
    "maximize-2",
    "message-circle",
    "message-circle-question-mark",
    "message-square",
    "message-square-dashed",
    "message-square-plus",
    "message-square-text",
    "messages-square",
    "minimize-2",
    "minus",
    "monitor",
    "mouse-pointer-click",
    "package",
    "palette",
    "panel-left",
    "panel-right",
    "panels-top-left",
    "paperclip",
    "pen-line",
    "pencil",
    "pencil-ruler",
    "pilcrow",
    "pin",
    "pin-off",
    "play",
    "plus",
    "power",
    "qr-code",
    "quote",
    "radio-tower",
    "redo-2",
    "refresh-cw",
    "rocket",
    "rotate-ccw",
    "rows-3",
    "search",
    "server",
    "settings",
    "settings-2",
    "shield",
    "shield-check",
    "shield-off",
    "shield-question-mark",
    "sliders-horizontal",
    "smartphone",
    "sparkles",
    "square",
    "square-arrow-out-up-right",
    "square-menu",
    "square-pen",
    "square-split-horizontal",
    "square-split-vertical",
    "square-terminal",
    "star",
    "tag",
    "target",
    "terminal",
    "text-search",
    "text-wrap",
    "ticket",
    "timer",
    "trash",
    "trash-2",
    "trending-down",
    "trending-up",
    "triangle-alert",
    "undo-2",
    "unlink-2",
    "user-check",
    "user-round",
    "user-round-x",
    "users",
    "wrench",
    "x",
];
