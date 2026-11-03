//! Add a theme: file, pasted JSON, batch and conflict flows from `ThemeImportDialog.tsx`.
use super::{
    SettingsView,
    controls::{Button, ButtonSize, ButtonVariant, DialogFrame, r},
    store,
};
use crate::{
    app_state::AppState,
    theme::{hsla, palette},
};
#[cfg(feature = "snapshot")]
use gpui::Focusable;
use gpui::{
    App, AppContext, Context, Entity, ExternalPaths, FocusHandle, IntoElement, ParentElement,
    Render, Styled, Subscription, Window, div, prelude::*,
};
use gpui_component::{
    h_flex,
    input::{Escape, InputEvent, Textarea, TextareaState},
    v_flex,
};
use t3_core::{
    client_settings::ClientSettingChange,
    themes::{
        self, ThemeDefinition,
        importer::{self, ImportSession, Installed},
    },
};

pub(super) struct ThemeImportDialog {
    state: Entity<AppState>,
    input: Entity<TextareaState>,
    session: ImportSession,
    open: bool,
    focus: FocusHandle,
    previous: Option<FocusHandle>,
    _subscription: Subscription,
}
impl ThemeImportDialog {
    fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input=cx.new(|cx|TextareaState::new(window,cx).placeholder("{\n  \"version\": 1,\n  \"name\": \"Aurora\",\n  \"appearance\": \"light\",\n  \"colors\": { ... }\n}"));
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.session.json = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        let previous = window.focused(cx);
        input.update(cx, |s, cx| s.focus(window, cx));
        #[cfg(feature = "snapshot")]
        crate::snapshot::register_focus("theme-json", input.read(cx).focus_handle(cx), cx);
        Self {
            state,
            input,
            session: ImportSession::default(),
            open: true,
            focus: cx.focus_handle(),
            previous,
            _subscription: subscription,
        }
    }
    #[cfg(feature = "snapshot")]
    fn fixture(&mut self, value: serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(json) = value["json"].as_str() {
            self.session.json = json.into();
            self.sync_input(window, cx);
        }
        if let Some(paths) = value["paths"].as_array() {
            self.read_paths(
                paths
                    .iter()
                    .filter_map(|v| v.as_str().map(std::path::PathBuf::from))
                    .collect(),
                window,
                cx,
            );
        }
        if value["submit"] == true {
            self.submit(window, cx);
        }
    }
    pub(super) fn is_open(&self) -> bool {
        self.open
    }
    pub(super) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session.cancel();
        self.open = false;
        if let Some(focus) = self.previous.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }
    fn cycle_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        for _ in 0..512 {
            if forward {
                window.focus_next(cx);
            } else {
                window.focus_prev(cx);
            }
            if self.focus.contains_focused(window, cx) && !self.focus.is_focused(window) {
                return;
            }
        }
        self.focus.focus(window, cx);
    }
    fn pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Choose files".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.open {
                        this.read_paths(paths, window, cx);
                    }
                });
            }
        })
        .detach();
    }
    fn read_paths(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let request = self.session.begin_read();
        let batch = paths.len() > 1;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let files = t3_client::theme_import::read_files(paths);
                    if batch {
                        ReadResult::Batch(importer::parse_batch(files))
                    } else {
                        let (name, text) = files.into_iter().next().unwrap();
                        ReadResult::Single(name, text)
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.open || !this.session.current(request) {
                    return;
                }
                match result {
                    ReadResult::Single(name, text) => {
                        if this.session.finish_single(request, name, text) {
                            this.sync_input(window, cx);
                        }
                    }
                    ReadResult::Batch(batch) => {
                        if let Some(result) =
                            this.session.finish_batch(request, batch, store::client(cx))
                        {
                            this.apply(result, false, window, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn sync_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(self.session.json.clone(), window, cx)
        });
    }
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.reading || self.session.json.trim().is_empty() {
            return;
        }
        if let Some(error) = importer::oversized(self.session.json.encode_utf16().count() as u64) {
            self.session.error = Some(error);
            return cx.notify();
        }
        let request = self.session.begin_read();
        let json = self.session.json.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { themes::import(&json) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.open || !this.session.current(request) {
                    return;
                }
                this.session.reading = false;
                match result {
                    Ok(theme) => this.install_single(theme, window, cx),
                    Err(error) => this.session.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn install_single(
        &mut self,
        theme: ThemeDefinition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if themes::library(store::client(cx))
            .iter()
            .any(|t| t.id == theme.id)
        {
            self.session.error = None;
            self.session.conflicts = vec![theme];
            return;
        }
        match themes::install(store::client(cx), &theme) {
            Ok(library) => {
                store::update_client(&self.state, ClientSettingChange::ThemeLibrary(library), cx);
                let description = if theme.variants.is_empty() {
                    format!("It’s now your {} theme.", theme.appearance)
                } else {
                    "It’s now active.".into()
                };
                let title = format!("{} added", theme.label);
                let patch = themes::select_collection(store::client(cx), &[theme]);
                store::update_client(&self.state, ClientSettingChange::ThemePatch(patch), cx);
                self.state.update(cx, |state, cx| {
                    state.show_toast(
                        crate::toast::Toast::new(crate::toast::ToastKind::Success, title)
                            .description(description),
                        cx,
                    )
                });
                self.close(window, cx);
            }
            Err(error) => self.session.error = Some(error),
        }
    }
    fn apply(
        &mut self,
        result: Installed,
        updated: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(library) = result.library {
            store::update_client(&self.state, ClientSettingChange::ThemeLibrary(library), cx);
        }
        if !result.installed.is_empty() {
            let verb = if updated { "updated" } else { "added" };
            let title = if result.installed.len() == 1 {
                format!("{} {verb}", result.installed[0].label)
            } else {
                format!("{} themes {verb}", result.installed.len())
            };
            let description = result
                .installed
                .iter()
                .map(|t| t.label.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    crate::toast::Toast::new(crate::toast::ToastKind::Success, title)
                        .description(description),
                    cx,
                )
            });
        }
        if result.close {
            self.close(window, cx);
        }
    }
    fn resolve(
        &mut self,
        choice: themes::ImportChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = self.session.resolve_conflicts(store::client(cx), choice);
        self.apply(
            result,
            matches!(choice, themes::ImportChoice::Update),
            window,
            cx,
        );
        cx.notify();
    }
}
enum ReadResult {
    Single(String, Result<String, String>),
    Batch(importer::Batch),
}
impl Render for ThemeImportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let p = palette(cx);
        let mut body = v_flex().gap(r(16.));
        if !self.session.conflicts.is_empty() {
            body = body
                .child(
                    v_flex()
                        .p(r(12.))
                        .gap(r(4.))
                        .rounded(r(12.))
                        .border_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .bg(hsla(p.muted).opacity(0.2))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child("Already installed"),
                        )
                        .child(
                            div().text_xs().text_color(hsla(p.muted_foreground)).child(
                                self.session
                                    .conflicts
                                    .iter()
                                    .map(|t| t.label.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            ),
                        ),
                )
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap(r(8.))
                        .child(
                            Button::new(
                                "theme-update-existing",
                                ButtonVariant::Default,
                                ButtonSize::Sm,
                            )
                            .label("Update existing")
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.resolve(themes::ImportChoice::Update, window, cx)
                                },
                            )),
                        )
                        .child(
                            Button::new("theme-keep-both", ButtonVariant::Outline, ButtonSize::Sm)
                                .label("Keep both")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.resolve(themes::ImportChoice::KeepBoth, window, cx)
                                })),
                        )
                        .child(
                            Button::new("theme-import-back", ButtonVariant::Ghost, ButtonSize::Sm)
                                .label("Back")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.session.conflicts.clear();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new(
                                "theme-import-cancel",
                                ButtonVariant::Ghost,
                                ButtonSize::Sm,
                            )
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                        ),
                );
        } else {
            body = body
                .child(
                    h_flex()
                        .id("theme-file-drop")
                        .justify_between()
                        .gap(r(12.))
                        .p(r(12.))
                        .rounded(r(12.))
                        .border_1()
                        .border_dashed()
                        .border_color(hsla(p.border).opacity(0.8))
                        .bg(hsla(p.muted).opacity(0.2))
                        .drag_over::<ExternalPaths>(move |s, _, _, _| {
                            s.border_color(hsla(p.ring)).bg(hsla(p.accent).opacity(0.2))
                        })
                        .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                            this.read_paths(paths.paths().to_vec(), window, cx);
                            cx.stop_propagation();
                        }))
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child("Theme file"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(hsla(p.muted_foreground))
                                        .truncate()
                                        .child(self.session.file_name.clone().unwrap_or_else(
                                            || "Drop T3 Code or VS Code .json files".into(),
                                        )),
                                ),
                        )
                        .child(
                            Button::new(
                                "choose-theme-files",
                                ButtonVariant::Outline,
                                ButtonSize::Sm,
                            )
                            .icon(crate::icons::Icon::Download)
                            .label(if self.session.reading {
                                "Reading…"
                            } else {
                                "Choose files"
                            })
                            .disabled(self.session.reading)
                            .on_click(cx.listener(|this, _, window, cx| this.pick(window, cx))),
                        ),
                )
                .child(
                    v_flex()
                        .gap(r(8.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child("Theme JSON"),
                        )
                        .child(ThemeJsonField(self.input.clone())),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap(r(8.))
                        .child(
                            Button::new(
                                "theme-import-cancel",
                                ButtonVariant::Ghost,
                                ButtonSize::Default,
                            )
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                        )
                        .child(
                            Button::new(
                                "theme-import-submit",
                                ButtonVariant::Default,
                                ButtonSize::Default,
                            )
                            .icon(crate::icons::Icon::Plus)
                            .label("Add theme")
                            .disabled(self.session.json.trim().is_empty() || self.session.reading)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                        ),
                );
        }
        if let Some(error) = &self.session.error {
            body = body.child(
                div()
                    .p(r(12.))
                    .rounded(r(12.))
                    .border_1()
                    .border_color(hsla(p.error).opacity(0.32))
                    .bg(hsla(p.error_surface))
                    .text_sm()
                    .text_color(hsla(p.error_foreground))
                    .child(error.clone()),
            );
        }
        let weak = cx.entity().downgrade();
        let frame = DialogFrame {
            id: "theme-import-dialog",
            title: "Add a theme".into(),
            description: "".into(),
            max_width: 768.,
            body: body.into_any_element(),
            footer: div().into_any_element(),
            focus: self.focus.clone(),
        };
        div()
            .capture_action(
                cx.listener(|this, _: &super::FocusNextInSettings, window, cx| {
                    this.cycle_focus(true, window, cx);
                    cx.stop_propagation();
                }),
            )
            .capture_action(
                cx.listener(|this, _: &super::FocusPreviousInSettings, window, cx| {
                    this.cycle_focus(false, window, cx);
                    cx.stop_propagation();
                }),
            )
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.close(window, cx);
                cx.stop_propagation();
            }))
            .child(frame.render_modal(
                window,
                52,
                move |window, cx| {
                    let _ = weak.update(cx, |this, cx| this.close(window, cx));
                },
                cx,
            ))
            .into_any_element()
    }
}
#[derive(IntoElement)]
struct ThemeJsonField(Entity<TextareaState>);
impl gpui::RenderOnce for ThemeJsonField {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        Textarea::new(&self.0)
            .aria_label("Theme JSON")
            .h(r(176.))
            .font_family(crate::theme::font_family(
                t3_core::client_settings::typography::FontRole::Code,
                cx,
            ))
            .text_size(r(12.))
            .line_height(r(20.))
            .rounded(r(12.))
    }
}
impl SettingsView {
    pub(super) fn open_theme_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.appearance_ui.import =
            Some(cx.new(|cx| ThemeImportDialog::new(self.state.clone(), window, cx)));
        cx.notify();
    }
}

#[cfg(feature = "snapshot")]
pub(crate) fn fixture(
    value: serde_json::Value,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    let view = cx
        .global::<super::Host>()
        .view
        .clone()
        .ok_or("settings must be open for theme import")?;
    view.update(cx, |view, cx| {
        view.open_theme_import(window, cx);
        view.appearance_ui
            .import
            .as_ref()
            .unwrap()
            .update(cx, |dialog, cx| dialog.fixture(value, window, cx));
    });
    Ok(())
}
