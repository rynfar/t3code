//! Add a theme: file, pasted JSON, batch and conflict flows from `ThemeImportDialog.tsx`.
#[path = "theme_search.rs"]
mod search_impl;
use search_impl as theme_search;
use super::{
    SettingsView,
    controls::{Button, ButtonSize, ButtonVariant, DialogFrame, r},
    store,
};
use crate::{
    app_state::AppState,
    theme::{hsla, palette},
};
use gpui::Focusable;
use gpui::{
    App, AppContext, Context, Entity, ExternalPaths, FocusHandle, IntoElement, ParentElement,
    Render, Styled, Subscription, Window, div, prelude::*,
};
use gpui_component::{
    ActiveTheme, h_flex,
    input::{Escape, InputEvent, TextareaState},
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
    conflict_focus: FocusHandle,
    saving: bool,
    save_error: Option<String>,
    _subscription: Subscription,
    search: theme_search::SearchUi,
    marketplace: Option<t3_core::themes::open_vsx::MarketplaceImport>,
    #[cfg(all(test, feature = "snapshot"))]
    install_gates: Option<(futures::channel::oneshot::Receiver<()>, futures::channel::oneshot::Receiver<()>)>,
}
impl ThemeImportDialog {
    fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input=cx.new(|cx|TextareaState::new(window,cx).placeholder("{\n  \"version\": 1,\n  \"name\": \"Aurora\",\n  \"appearance\": \"light\",\n  \"colors\": { ... }\n}"));
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.session.json = input.read(cx).value().to_string();
            }
            cx.notify();
        });
        let previous = window.focused(cx);
        input.update(cx, |s, cx| s.focus(window, cx));
        #[cfg(feature = "snapshot")]
        crate::snapshot::register_focus("theme-json", input.read(cx).focus_handle(cx), cx);
        let search = theme_search::SearchUi::new(window, cx);
        Self {
            search, marketplace: None,
            state,
            input,
            session: ImportSession::default(),
            open: true,
            focus: cx.focus_handle(),
            previous,
            conflict_focus: cx.focus_handle().tab_stop(true),
            saving: false,
            save_error: None,
            _subscription: subscription,
            #[cfg(all(test, feature = "snapshot"))]
            install_gates: None,
        }
    }
    #[cfg(feature = "snapshot")]
    fn fixture(&mut self, value: serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        if value.get("search").is_some() { self.search_fixture(&value["search"], window, cx); }
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
        if self.saving {
            return;
        }
        self.search.cancel();
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
        if self.saving || paths.is_empty() {
            return;
        }
        self.search.cancel();
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
                        this.install(InstallOperation::Batch(batch), window, cx);
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
        if self.saving || self.search.session_installing() || self.session.reading || self.session.json.trim().is_empty() {
            return;
        }
        if let Some(error) = importer::oversized(self.session.json.encode_utf16().count() as u64) {
            self.session.error = Some(error);
            return cx.notify();
        }
        let request = self.session.begin_read();
        let json = self.session.json.clone();
        self.focus.focus(window, cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let result = themes::import(&json);
                    (json, result)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.open || !this.session.current(request) {
                    return;
                }
                this.session.reading = false;
                let (submitted, result) = result;
                if this.input.read(cx).value().as_str() != submitted {
                    return cx.notify();
                }
                match result {
                    Ok(theme) => this.install_single(theme, window, cx),
                    Err(error) => {
                        this.session.error = Some(error);
                        this.input.update(cx, |input, cx| input.focus(window, cx));
                    }
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
        self.install(InstallOperation::Single(theme), window, cx);
    }
    fn install(
        &mut self,
        operation: InstallOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving {
            return;
        }
        self.saving = true;
        self.save_error = None;
        self.focus.focus(window, cx);
        let session = self.session.clone();
        let doc = store::client(cx).clone();
        let resolving = matches!(operation, InstallOperation::Conflicts(_));
        #[cfg(all(test, feature = "snapshot"))]
        let gates = self.install_gates.take();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            #[cfg(all(test, feature = "snapshot"))]
            let completion_gate = if let Some((preparation, completion)) = gates {
                let _ = preparation.await;
                Some(completion)
            } else {
                None
            };
            let transaction = cx
                .background_executor()
                .spawn(async move {
                    t3_client::store::commit_client_settings_with(doc, move |current| {
                        let prepared = prepare_import(session, current, operation);
                        (prepared.patch.clone(), prepared)
                    })
                })
                .await;
            #[cfg(all(test, feature = "snapshot"))]
            if let Some(gate) = completion_gate {
                let _ = gate.await;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                this.session.reading = false;
                let (prepared, result) = match transaction {
                    Ok(value) => value,
                    Err(error) => {
                        this.save_error = Some(error);
                        this.focus_form(window, cx);
                        return cx.notify();
                    }
                };
                let receipt = match result {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        this.save_error = Some(
                            prepared
                                .result
                                .installed
                                .iter()
                                .map(|t| format!("{}: {error}", t.label))
                                .collect::<Vec<_>>()
                                .join("; "),
                        );
                        this.session.error = prepared.session.error;
                        if !resolving {
                            this.session.conflicts = prepared.session.conflicts;
                        }
                        this.focus_form(window, cx);
                        return cx.notify();
                    }
                };
                store::install_client_commit(&this.state, receipt, cx);
                this.session = prepared.session;
                if !prepared.result.installed.is_empty() {
                    let themes = &prepared.result.installed;
                    let verb = if prepared.updated { "updated" } else { "added" };
                    let title = if themes.len() == 1 {
                        format!("{} {verb}", themes[0].label)
                    } else {
                        format!("{} themes {verb}", themes.len())
                    };
                    let description = if prepared.single {
                        if themes[0].variants.is_empty() {
                            format!("It’s now your {} theme.", themes[0].appearance)
                        } else {
                            "It’s now active.".into()
                        }
                    } else {
                        themes
                            .iter()
                            .map(|t| t.label.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    this.state.update(cx, |state, cx| {
                        state.show_toast(
                            crate::toast::Toast::new(crate::toast::ToastKind::Success, title)
                                .description(description),
                            cx,
                        )
                    });
                }
                if prepared.result.close {
                    this.close(window, cx);
                } else {
                    this.focus_form(window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn focus_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.conflicts.is_empty() && self.marketplace.is_none() {
            self.input.update(cx, |input, cx| input.focus(window, cx));
        } else {
            self.conflict_focus.focus(window, cx);
        }
    }
    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let marketplace = self.marketplace.take().is_some();
        self.save_error = None;
        self.session.conflicts.clear();
        if marketplace { self.search.focus(window, cx); self.settle_search(false, window, cx); } else { self.focus_form(window, cx); }
        cx.notify();
    }
    fn resolve(
        &mut self,
        choice: themes::ImportChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(import) = self.marketplace.clone() {
            self.install(InstallOperation::Marketplace(import, choice), window, cx);
        } else { self.install(InstallOperation::Conflicts(choice), window, cx); }
    }
}
enum InstallOperation {
    Marketplace(t3_core::themes::open_vsx::MarketplaceImport, themes::ImportChoice),
    Single(ThemeDefinition),
    Batch(importer::Batch),
    Conflicts(themes::ImportChoice),
}
struct PreparedImport {
    session: ImportSession,
    result: Installed,
    patch: serde_json::Map<String, serde_json::Value>,
    updated: bool,
    single: bool,
}
fn prepare_import(
    mut session: ImportSession,
    doc: &t3_core::client_settings::ClientSettingsDoc,
    operation: InstallOperation,
) -> PreparedImport {
    let single = matches!(operation, InstallOperation::Single(_));
    let updated = matches!(
        operation,
        InstallOperation::Conflicts(themes::ImportChoice::Update)
    );
    let mut result = match operation {
        InstallOperation::Marketplace(import, choice) => {
            return match import.prepare(doc, choice) {
                Ok((patch, installed)) => PreparedImport {session, result: Installed {library: None, installed, close: true}, patch, updated: !import.expected.is_empty(), single: false},
                Err(error) => { session.error = Some(error); PreparedImport {session, result: Installed {library: None, installed: Vec::new(), close: false}, patch: Default::default(), updated: false, single: false} }
            };
        }
        InstallOperation::Single(theme) => {
            let request = session.begin_read();
            session
                .finish_batch(
                    request,
                    importer::Batch {
                        themes: vec![theme],
                        failures: Vec::new(),
                    },
                    doc,
                )
                .unwrap()
        }
        InstallOperation::Batch(batch) => {
            let request = session.begin_read();
            session.finish_batch(request, batch, doc).unwrap()
        }
        InstallOperation::Conflicts(choice) => session.resolve_conflicts(doc, choice),
    };
    let mut patch = serde_json::Map::new();
    if let Some(library) = result.library.take() {
        if single {
            let mut doc = doc.clone();
            doc.apply_change(&ClientSettingChange::ThemeLibrary(library.clone()));
            patch.extend(themes::select_collection(&doc, &result.installed));
        }
        patch.insert(themes::LIBRARY_KEY.into(), library);
    }
    PreparedImport {
        session,
        result,
        patch,
        updated,
        single,
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
        if !self.session.conflicts.is_empty() || self.marketplace.is_some() {
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
                                .child(if self.marketplace.is_some() {"Update installed themes?"} else {"Already installed"}),
                        )
                        .child(
                            div().text_xs().text_color(hsla(p.muted_foreground)).child(
                                self.marketplace.as_ref().map(|import| &import.themes).unwrap_or(&self.session.conflicts)
                                    .iter().map(|t| t.label.as_str()).collect::<Vec<_>>().join(", "),
                            ),
                        ),
                )
                .when(self.marketplace.is_some(), |body| body.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Update existing replaces installed variants, including local edits, and removes retired variants. Keep both adds copies.")))
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
                            .focus_handle(self.conflict_focus.clone())
                            .disabled(self.saving)
                            .label("Update existing")
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.resolve(themes::ImportChoice::Update, window, cx)
                                },
                            )),
                        )
                        .child(
                            Button::new("theme-keep-both", ButtonVariant::Outline, ButtonSize::Sm)
                                .disabled(self.saving)
                                .label("Keep both")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.resolve(themes::ImportChoice::KeepBoth, window, cx)
                                })),
                        )
                        .child(
                            Button::new("theme-import-back", ButtonVariant::Ghost, ButtonSize::Sm)
                                .disabled(self.saving)
                                .label("Back")
                                .on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
                        )
                        .child(
                            Button::new(
                                "theme-import-cancel",
                                ButtonVariant::Ghost,
                                ButtonSize::Sm,
                            )
                            .disabled(self.saving)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                        ),
                );
        } else {
            body = body.child(self.search_section(window, cx)).child(div().text_xs().text_color(hsla(p.muted_foreground)).child("or import a file"));
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
                            .disabled(self.session.reading || self.saving || self.search.session_installing())
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
                        .child(ThemeJsonField {
                            input: self.input.clone(),
                            disabled: self.saving || self.search.session_installing(),
                        }),
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
                            .disabled(self.saving)
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
                            .disabled(
                                self.session.json.trim().is_empty()
                                    || self.session.reading
                                    || self.saving || self.search.session_installing(),
                            )
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                        ),
                );
        }
        let errors = self
            .session
            .error
            .iter()
            .chain(self.save_error.iter())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("; ");
        if !errors.is_empty() {
            body = body.child(
                div()
                    .p(r(12.))
                    .rounded(r(12.))
                    .border_1()
                    .border_color(hsla(p.error).opacity(0.32))
                    .bg(hsla(p.error_surface))
                    .text_sm()
                    .text_color(hsla(p.error_foreground))
                    .child(errors),
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
            .key_context("SettingsModal")
            .capture_action(
                cx.listener(|this, _: &gpui_base::input::IndentInline, window, cx| {
                    this.cycle_focus(true, window, cx);
                    cx.stop_propagation();
                }),
            )
            .capture_action(
                cx.listener(|this, _: &gpui_base::input::OutdentInline, window, cx| {
                    this.cycle_focus(false, window, cx);
                    cx.stop_propagation();
                }),
            )
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
struct ThemeJsonField {
    input: Entity<TextareaState>,
    disabled: bool,
}
impl gpui::RenderOnce for ThemeJsonField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        self.input.update(cx, |input, cx| {
            input.set_disabled(self.disabled, cx);
            input.on_context_menu(std::rc::Rc::new(|_, capabilities, position, window, cx| {
                let editable = !capabilities.is_disabled() && !capabilities.is_readonly();
                gpui_component::native_menu::NativeMenu::new()
                    .menu_with_disabled(
                        "Cut",
                        !(editable && capabilities.has_selection()),
                        Box::new(gpui_base::input::Cut),
                    )
                    .menu_with_disabled(
                        "Copy",
                        !capabilities.has_selection(),
                        Box::new(gpui_base::input::Copy),
                    )
                    .menu_with_disabled("Paste", !editable, Box::new(gpui_base::input::Paste))
                    .separator()
                    .menu("Select All", Box::new(gpui_base::input::SelectAll))
                    .show(position, window, cx);
            }));
            input.set_editor_paddings(gpui::Edges::all(window.rem_size() * 0.75));
            input.set_editor_style(gpui_base::input::InputEditorStyle {
                foreground: hsla(p.foreground),
                muted_foreground: hsla(p.muted_foreground),
                background: hsla(p.background),
                border: hsla(p.input),
                selection: cx.theme().selection,
                caret: hsla(p.foreground),
                ..Default::default()
            });
        });
        let accessible_value = window.is_a11y_active().then(|| self.input.read(cx).value());
        let accessible_input = self.input.clone();
        div()
            .id("theme-json-field")
            .role(gpui::Role::MultilineTextInput)
            .aria_label("Theme JSON")
            .when_some(accessible_value, |v, value| v.aria_value(value))
            .when(!self.disabled, |v| {
                v.on_a11y_action(gpui::AccessibleAction::SetValue, move |data, window, cx| {
                    if let Some(gpui::accesskit::ActionData::Value(value)) = data {
                        accessible_input.update(cx, |input, cx| {
                            input.set_value(value.to_string(), window, cx)
                        });
                    }
                })
            })
            .h(r(176.))
            .w_full()
            .rounded(r(12.))
            .border_1()
            .border_color(hsla(if focused { p.ring } else { p.input }))
            .bg(hsla(p.background))
            .when(focused, |v| {
                v.shadow(vec![gpui::BoxShadow {
                    color: hsla(p.ring).opacity(0.24),
                    offset: gpui::point(gpui::px(0.), gpui::px(0.)),
                    blur_radius: gpui::px(0.),
                    spread_radius: gpui::px(3.),
                    inset: true,
                }])
            })
            .font_family(crate::theme::font_family(
                t3_core::client_settings::typography::FontRole::Code,
                cx,
            ))
            .text_size(r(12.))
            .line_height(r(20.))
            .child(gpui_base::input::Textarea::new(&self.input))
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

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::{KeyBinding, TestAppContext};
    use serde_json::json;
    fn setup(
        cx: &mut TestAppContext,
    ) -> (
        tokio::runtime::Runtime,
        Entity<AppState>,
        std::path::PathBuf,
    ) {
        static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        let home = HOME
            .get_or_init(|| {
                std::env::temp_dir().join(format!("t3-theme-import-ui-{}", uuid::Uuid::new_v4()))
            })
            .clone();
        t3_client::store::use_home(home.clone());
        let _ = t3_client::store::commit_client_settings(serde_json::Map::new());
        let settings = home.join("settings.json");
        if settings.is_dir() { std::fs::remove_dir_all(settings).unwrap(); }
        else if settings.exists() { std::fs::remove_file(settings).unwrap(); }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state=cx.update(|cx| {
            gpui_component::init(cx);crate::theme::init(cx);store::init(cx);
            cx.bind_keys([KeyBinding::new("tab",super::super::FocusNextInSettings,Some("SettingsModal")),KeyBinding::new("shift-tab",super::super::FocusPreviousInSettings,Some("SettingsModal"))]);
            cx.set_global(serde_json::from_value::<crate::chatgpt_setup::Fixture>(json!({"provider":{"instanceId":"test","driver":"test","enabled":false,"installed":false,"status":"unavailable"},"auth":{"phase":"idle"},"install":{"phase":"idle"}})).unwrap());
            let state=cx.new(|cx|AppState::new(runtime.handle().clone(),cx));
            store::install_committed_client(&state,serde_json::Map::from_iter([(themes::LIBRARY_KEY.into(),json!([]))]),cx);
            state
        });
        (runtime, state, home)
    }
    fn source() -> String {
        json!({"version":1,"name":"Dusk","appearance":"dark","colors":{"accent":"#123456"}})
            .to_string()
    }
    fn keys(cx: &mut TestAppContext, window: gpui::AnyWindowHandle, text: &str) {
        for key in text.split_whitespace() {
            cx.simulate_keystrokes(window, key);
            cx.update_window(window, |_, window, cx| {
                window.dispatch_event(
                    gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                        keystroke: gpui::Keystroke::parse(key).unwrap(),
                    }),
                    cx,
                );
            })
            .unwrap();
            cx.run_until_parked();
        }
    }
    #[gpui::test]
    fn tab_leaves_json_and_keyboard_conflict_back_restores_editor(cx: &mut TestAppContext) {
        let (_runtime, state, _home) = setup(cx);
        let window = cx.add_window(|window, cx| ThemeImportDialog::new(state.clone(), window, cx));
        window
            .update(cx, |view, window, cx| {
                view.session.json = source();
                view.sync_input(window, cx);
                let theme = themes::import(&source()).unwrap();
                let library = themes::install(store::client(cx), &theme).unwrap();
                let patch=serde_json::Map::from_iter([(themes::LIBRARY_KEY.into(), library)]);
                t3_client::store::commit_client_settings(patch.clone()).unwrap();
                store::install_committed_client(&state,patch,cx);
                view.input.update(cx, |input, cx| input.focus(window, cx));
            })
            .unwrap();
        cx.run_until_parked();
        keys(cx, window.into(), "tab");
        cx.run_until_parked();
        window
            .update(cx, |view, window, cx| {
                assert_eq!(view.input.read(cx).value().as_str(), source());
                assert!(!view.input.read(cx).focus_handle(cx).is_focused(window));
            })
            .unwrap();
        keys(cx, window.into(), "shift-tab");
        cx.run_until_parked();
        window
            .update(cx, |view, window, cx| {
                assert!(view.input.read(cx).focus_handle(cx).is_focused(window));
            })
            .unwrap();
        keys(cx, window.into(), "shift-tab");
        window
            .update(cx, |view, window, cx| {
                assert!(!view.input.read(cx).focus_handle(cx).is_focused(window));
                assert_eq!(view.input.read(cx).value().as_str(), source());
            })
            .unwrap();
        keys(cx, window.into(), "tab");
        keys(cx, window.into(), "tab tab enter");
        cx.run_until_parked();
        window
            .update(cx, |view, window, cx| {
                assert_eq!(view.session.conflicts.len(), 1);
                assert!(view.focus.contains_focused(window, cx));
                assert!(view.conflict_focus.is_focused(window));
            })
            .unwrap();
        keys(cx, window.into(), "tab tab enter");
        cx.run_until_parked();
        window
            .update(cx, |view, window, cx| {
                assert!(view.session.conflicts.is_empty());
                assert!(view.input.read(cx).focus_handle(cx).is_focused(window));
            })
            .unwrap();
        keys(cx, window.into(), "escape");
        cx.run_until_parked();
        window.update(cx, |view, _, _| assert!(!view.open)).unwrap();
    }
    #[gpui::test]
    fn editing_after_submit_retires_the_parse_before_it_installs(cx: &mut TestAppContext) {
        let (_runtime, state, _home) = setup(cx);
        let window = cx.add_window(|window, cx| ThemeImportDialog::new(state.clone(), window, cx));
        window
            .update(cx, |view, window, cx| {
                view.session.json = source();
                view.sync_input(window, cx);
                view.submit(window, cx);
                view.input
                    .update(cx, |input, cx| input.set_value("{", window, cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| {
                assert!(view.open);
                assert!(themes::library(store::client(cx)).is_empty());
                assert_eq!(view.input.read(cx).value().as_str(), "{");
                assert!(!view.session.reading);
            })
            .unwrap();
    }
    #[gpui::test]
    fn concurrent_library_edits_survive_preparation_and_delayed_completion(
        cx: &mut TestAppContext,
    ) {
        let (_runtime, state, home) = setup(cx);
        let theme = |name: &str| themes::import(&source().replace("Dusk", name)).unwrap();
        let baseline = serde_json::json!([theme("Keep"), theme("Remove")]);
        let edited = serde_json::json!([theme("Keep"), theme("Other")]);
        let mut outcomes = Vec::new();
        for confirmed in [false, true] {
            for after_commit in [false, true] {
                let patch =
                    serde_json::Map::from_iter([(themes::LIBRARY_KEY.into(), baseline.clone())]);
                t3_client::store::commit_client_settings(patch.clone()).unwrap();
                cx.update(|cx| store::install_committed_client(&state, patch, cx));
                let (prepare, preparation) = futures::channel::oneshot::channel();
                let mut prepare = Some(prepare);
                let (complete, completion) = futures::channel::oneshot::channel();
                let window =
                    cx.add_window(|window, cx| ThemeImportDialog::new(state.clone(), window, cx));
                window
                    .update(cx, |view, window, cx| {
                        view.install_gates = Some((preparation, completion));
                        view.install_single(theme("Dusk"), window, cx);
                    })
                    .unwrap();
                cx.run_until_parked();
                if after_commit {
                    prepare.take().unwrap().send(()).unwrap();
                    cx.run_until_parked();
                }
                if confirmed {
                    let before = cx.read(|cx| {
                        store::client(cx)
                            .entries()
                            .get(themes::LIBRARY_KEY)
                            .cloned()
                    });
                    let receipt = t3_client::store::commit_client_settings_rebased(
                        serde_json::Map::from_iter([(themes::LIBRARY_KEY.into(), edited.clone())]),
                        before,
                    )
                    .unwrap();
                    cx.update(|cx| store::install_client_commit(&state, receipt, cx));
                } else {
                    cx.update(|cx| {
                        store::update_client(
                            &state,
                            ClientSettingChange::ThemeLibrary(edited.clone()),
                            cx,
                        )
                    });
                }
                t3_client::store::commit_client_settings(serde_json::Map::new()).unwrap();
                if !after_commit {
                    prepare.take().unwrap().send(()).unwrap();
                }
                complete.send(()).unwrap();
                cx.run_until_parked();
                let memory = cx.read(|cx| {
                    themes::library(store::client(cx))
                        .into_iter()
                        .map(|t| t.id)
                        .collect::<std::collections::BTreeSet<_>>()
                });
                let disk = t3_core::client_settings::ClientSettingsDoc::from_json(
                    &std::fs::read_to_string(home.join("settings.json")).unwrap(),
                );
                let disk = themes::library(&disk)
                    .into_iter()
                    .map(|t| t.id)
                    .collect::<std::collections::BTreeSet<_>>();
                outcomes.push((confirmed, after_commit, memory, disk));
            }
        }
        let expected =
            std::collections::BTreeSet::from(["dusk".into(), "keep".into(), "other".into()]);
        for (confirmed, after_commit, memory, disk) in outcomes {
            assert_eq!(
                disk, expected,
                "persisted library, confirmed: {confirmed}, delayed completion: {after_commit}"
            );
            assert_eq!(
                memory, expected,
                "in-memory library, confirmed: {confirmed}, delayed completion: {after_commit}"
            );
        }
    }
    #[gpui::test]
    fn single_batch_marketplace_and_conflict_stay_open_when_the_settings_write_fails(cx: &mut TestAppContext) {
        fn marketplace() -> themes::open_vsx::MarketplaceImport {
            let mut theme = themes::import(&source()).unwrap();
            theme.collection = Some(themes::Collection { id: "open-vsx:demo.theme".into(), label: "Demo".into() });
            themes::open_vsx::MarketplaceImport { collection_id: "open-vsx:demo.theme".into(), expected: Vec::new(), themes: vec![theme] }
        }
        let (_runtime, state, home) = setup(cx);
        std::fs::create_dir_all(home.join("settings.json")).unwrap();
        let mut last = None;
        for mode in [3, 0, 1, 2] {
            cx.update(store::init);
            let before = cx.read(|cx| store::client(cx).clone());
            let window =
                cx.add_window(|window, cx| ThemeImportDialog::new(state.clone(), window, cx));
            window
                .update(cx, |view, window, cx| {
                    let theme = themes::import(&source()).unwrap();
                    match mode {
                        0 => view.install_single(theme, window, cx),
                        1 => {
                            let batch =
                                importer::parse_batch(vec![("dusk.json".into(), Ok(source()))]);
                            view.install(InstallOperation::Batch(batch), window, cx);
                        }
                        3 => view.install(InstallOperation::Marketplace(marketplace(), themes::ImportChoice::Update), window, cx),
                        _ => {
                            let library = themes::install(store::client(cx), &theme).unwrap();
                            store::install_committed_client(
                                &state,
                                serde_json::Map::from_iter([(themes::LIBRARY_KEY.into(), library)]),
                                cx,
                            );
                            view.session.conflicts = vec![theme];
                            view.resolve(themes::ImportChoice::KeepBoth, window, cx);
                        }
                    }
                })
                .unwrap();
            cx.run_until_parked();
            window
                .update(cx, |view, _, cx| {
                    assert!(view.open);
                    assert!(view.save_error.is_some() || view.session.error.is_some());
                    assert_eq!(
                        themes::library(store::client(cx)).len(),
                        usize::from(mode == 2)
                    );
                    if mode != 2 {
                        assert_eq!(store::client(cx).theme(), before.theme());
                    }
                })
                .unwrap();
            if mode == 3 {
                std::fs::remove_dir_all(home.join("settings.json")).unwrap();
                window.update(cx, |view, window, cx| {
                    view.install(InstallOperation::Marketplace(marketplace(), themes::ImportChoice::Update), window, cx);
                }).unwrap();
                cx.run_until_parked();
                window.update(cx, |view, _, cx| {
                    assert!(!view.open);
                    assert!(view.save_error.is_none());
                    assert_eq!(themes::library(store::client(cx)).len(), 1);
                }).unwrap();
                let stored = t3_core::client_settings::ClientSettingsDoc::from_json(&t3_client::store::load_client_settings().unwrap());
                assert_eq!(themes::open_vsx::stored_collection(&stored, "open-vsx:demo.theme").len(), 1);
                std::fs::remove_file(home.join("settings.json")).unwrap();
                std::fs::create_dir_all(home.join("settings.json")).unwrap();
            }
            last = Some(window);
        }
        std::fs::remove_dir_all(home.join("settings.json")).unwrap();
        let baseline=cx.read(|cx|store::client(cx).entries().get(themes::LIBRARY_KEY).cloned().unwrap());
        t3_client::store::commit_client_settings(serde_json::Map::from_iter([(themes::LIBRARY_KEY.into(),baseline)])).unwrap();
        let window = last.unwrap();
        window
            .update(cx, |view, window, cx| {
                view.resolve(themes::ImportChoice::KeepBoth, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| {
                assert!(!view.open);
                assert_eq!(themes::library(store::client(cx)).len(), 2);
            })
            .unwrap();
        let stored = t3_core::client_settings::ClientSettingsDoc::from_json(
            &t3_client::store::load_client_settings().unwrap(),
        );
        assert_eq!(themes::library(&stored).len(), 2);
    }
}
