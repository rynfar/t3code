//! `web/src/components/settings/ProjectIconPickerDialog.tsx`; saves through the Projects page.

use super::{
    SettingsView,
    controls::{Button, ButtonSize, ButtonVariant, DialogFrame, dialog_footer, focus_handle, focus_ring, r, tooltip},
};
use crate::theme::{hsla, palette};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, SharedString, Styled, Subscription, Window, div,
    prelude::*,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use t3_core::settings::projects::{
    ProjectIcon,
    icon::{self, COLORS},
    picker::{self, Draft, Mode, Target},
};

pub(super) struct Picker {
    pub target: Target,
    draft: Draft,
    automatic: ProjectIcon,
    query: Entity<InputState>,
    letters: Entity<InputState>,
    custom: Entity<InputState>,
    focus: FocusHandle,
    previous: Option<FocusHandle>,
    _subscriptions: Vec<Subscription>,
}

impl Picker {
    pub(super) fn text_field_focused(&self, window: &Window, cx: &App) -> bool {
        [&self.query, &self.letters, &self.custom]
            .iter()
            .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
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
        let target = self.icon_target(&shown.members, cx);
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
                    && let Some(picker) = &mut this.projects.picker
                {
                    picker.draft.letters = input.read(cx).value().to_string();
                    cx.notify();
                }
            }),
            cx.subscribe(&custom, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(picker) = &mut this.projects.picker
                {
                    picker.draft.custom_emoji(&input.read(cx).value());
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
            draft,
            automatic: icon::identity(&shown.members[0].title),
            query,
            letters,
            custom,
            focus: cx.focus_handle(),
            previous,
            _subscriptions: subscriptions,
        });
        cx.notify();
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
        let Some(icon) = picker.draft.selected() else { return };
        let target = picker.target.clone();
        let Some(shown) = self.shown_project(cx) else { return };
        if !target.allows(&self.icon_target(&shown.members, cx), self.can_edit_projects(cx), self.projects.saving_icon) {
            return;
        }
        self.dismiss_project_icon_picker(window, cx);
        let updates = target.members.iter().map(|(id, _)| (id.clone(), picker::update(icon.clone()))).collect();
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
            .is_some_and(|shown| target.allows(&self.icon_target(&shown.members, cx), self.can_edit_projects(cx), self.projects.saving_icon))
        {
            self.dismiss_project_icon_picker(window, cx);
            return None;
        }
        let picker = self.projects.picker.as_ref()?;
        let p = palette(cx);
        let mode = picker.draft.mode;
        let selected = picker.draft.selected();
        let fallback = match &picker.automatic {
            ProjectIcon::Monogram { text, .. } => ProjectIcon::Monogram {
                text: text.clone(),
                color: picker.draft.color.clone(),
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
                            if let Some(picker) = &mut this.projects.picker {
                                picker.draft.mode = next;
                                match next {
                                    Mode::Lucide => &picker.query,
                                    Mode::Emoji => &picker.custom,
                                    Mode::Monogram => &picker.letters,
                                }
                                .update(cx, |input, cx| input.focus(window, cx));
                            }
                            cx.notify();
                        }))
                    }),
            ),
        );
        if mode != Mode::Emoji {
            let color = picker.draft.color.clone();
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
                                if let Some(picker) = &mut this.projects.picker {
                                    picker.draft.color = value.into();
                                }
                                cx.notify();
                            }))
                    }))),
            );
        }
        let picker = self.projects.picker.as_ref()?;
        match mode {
            Mode::Lucide => {
                let names = picker::filter_names(&picker.query.read(cx).value(), CATALOG);
                let color = picker.draft.color.clone();
                let chosen = picker.draft.icon_name.clone();
                body = body.child(Input::new(&picker.query)).child(
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
                                if let Some(picker) = &mut this.projects.picker {
                                    picker.draft.icon_name = name.into();
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
                        cell(format!("project-emoji-{label}"), picker.draft.emoji == *emoji, label, window, cx)
                            .text_size(r(20.))
                            .child(*emoji)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(picker) = &mut this.projects.picker {
                                    picker.draft.emoji = (*emoji).into();
                                }
                                cx.notify();
                            }))
                    })));
                }
                body = body.child(grid).child(
                    v_flex()
                        .gap(r(8.))
                        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Or paste any emoji"))
                        .child(Input::new(&picker.custom)),
                );
            }
            Mode::Monogram => {
                body = body.child(
                    v_flex()
                        .gap(r(8.))
                        .child(div().text_sm().child("Letters"))
                        .child(Input::new(&picker.letters))
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
                    .disabled(selected.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.save_project_icon(window, cx)))
                    .into_any_element(),
            ],
            cx,
        );
        let frame = DialogFrame {
            id: "project-icon-picker",
            title: "Choose project icon".into(),
            description: "Choose an icon, emoji, or monogram.".into(),
            max_width: 512.,
            body: body.into_any_element(),
            footer: footer.into_any_element(),
            focus: picker.focus.clone(),
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
