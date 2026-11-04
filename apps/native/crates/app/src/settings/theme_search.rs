//! Search community themes from `web/src/components/settings/ThemeSearchSection.tsx`.
use super::super::controls::{Select, SelectOption};
use super::*;
use gpui::StyledImage;
use gpui_component::input::InputState;
use t3_core::themes::open_vsx::{self, Extension, MarketplaceImport, SearchSession};
struct Abort(tokio::task::AbortHandle);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub(super) struct SearchUi {
    input: Entity<InputState>,
    session: SearchSession,
    results: Option<Vec<Extension>>,
    error: Option<String>,
    searching: bool,
    installing_id: Option<String>,
    debounce: Option<gpui::Task<()>>,
    request: Option<Abort>,
    _subscription: Option<Subscription>,
    #[cfg(feature = "snapshot")]
    fixture: bool,
}
impl SearchUi {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ThemeImportDialog>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search themes..."));
        input.update(cx, |input, cx| input.focus(window, cx));
        #[cfg(feature = "snapshot")]
        crate::snapshot::register_focus("theme-search", input.read(cx).focus_handle(cx), cx);
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value().to_string();
                    this.change_search(&query, window, cx);
                }
                if matches!(event, InputEvent::PressEnter { .. }) && !this.search.searching {
                    this.settle_search(true, window, cx);
                }
            },
        );
        Self {
            input,
            session: SearchSession::new(),
            results: None,
            error: None,
            searching: false,
            installing_id: None,
            debounce: None,
            request: None,
            _subscription: Some(subscription),
            #[cfg(feature = "snapshot")]
            fixture: false,
        }
    }
    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.read(cx).focus_handle(cx).focus(window, cx);
    }
    pub(super) fn session_installing(&self) -> bool {
        self.session.installing
    }
    pub(super) fn cancel(&mut self) {
        self.debounce = None;
        self.request = None;
        self.session.clear();
        self.session.end_install();
        self.installing_id = None;
        self.searching = false;
    }
}
impl ThemeImportDialog {
    fn change_search(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search.session.set_query(query) {
            return;
        }
        self.search.debounce = None;
        if query.trim().is_empty() {
            self.search.cancel();
            self.search.results = None;
            self.search.error = None;
            self.search.searching = false;
            return cx.notify();
        }
        self.search.debounce = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(open_vsx::DEBOUNCE_MS))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.search.debounce = None;
                this.settle_search(false, window, cx);
            });
        }));
        cx.notify();
    }
    pub(super) fn settle_search(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || !self.open {
            return;
        }
        if force {
            self.search.debounce = None;
        }
        let fetch = self.search.session.settle(force);
        if self.search.session.cancel_request {
            self.search.request = None;
            self.search.session.retire();
            self.search.searching = false;
        }
        if !fetch {
            return cx.notify();
        }
        self.search.request = None;
        self.search.session.retire();
        let epoch = self.search.session.epoch;
        self.search.error = None;
        self.search.searching = true;
        #[cfg(feature = "snapshot")]
        if self.search.fixture {
            self.search.searching = false;
            return cx.notify();
        }
        let key = self.search.session.key();
        let query = self.search.session.query.clone();
        let sort = self.search.session.sort.clone();
        let task = self
            .state
            .read(cx)
            .runtime()
            .spawn(t3_client::open_vsx::search(query, sort));
        self.search.request = Some(Abort(task.abort_handle()));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update_in(cx, |this, _, cx| {
                if !this.open || epoch != this.search.session.epoch {
                    return;
                }
                this.search.request = None;
                this.search.searching = false;
                match result {
                    Ok(results) => {
                        this.search.results = Some(results);
                        this.search.session.shown = Some(key);
                    }
                    Err(error) => {
                        this.search.results = None;
                        this.search.session.shown = None;
                        this.search.error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn download_theme(
        &mut self,
        extension: Extension,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving || self.search.session.installing {
            return;
        }
        self.search.request = None;
        self.search.session.begin_install();
        self.search.installing_id = Some(extension.id.clone());
        self.search.searching = false;
        self.search.error = None;
        self.save_error = None;
        let doc = store::client(cx).clone();
        let epoch = self.search.session.epoch;
        let task = self.state.read(cx).runtime().spawn(async move {
            let collection_id = extension.collection_id.clone();
            let expected = open_vsx::stored_collection(&doc, &collection_id);
            let themes = t3_client::open_vsx::download(extension).await?;
            Ok::<_, String>(MarketplaceImport {
                collection_id,
                expected,
                themes,
            })
        });
        self.search.request = Some(Abort(task.abort_handle()));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.open || epoch != this.search.session.epoch {
                    return;
                }
                this.search.request = None;
                this.search.session.end_install();
                this.search.installing_id = None;
                match result {
                    Ok(import) => {
                        if import.expected.is_empty() {
                            this.install(
                                InstallOperation::Marketplace(import, themes::ImportChoice::Update),
                                window,
                                cx,
                            );
                        } else {
                            this.marketplace = Some(import);
                            this.conflict_focus.focus(window, cx);
                        }
                    }
                    Err(error) => {
                        this.search.error = Some(error);
                        this.settle_search(false, window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn search_section(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let p = palette(cx);
        let busy = self.saving || self.session.reading || self.search.session.installing;
        let mut body = v_flex()
            .gap(r(12.))
            .child(
                v_flex()
                    .gap(r(2.))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Search community themes"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child("Find open-source themes from Open VSX."),
                    ),
            )
            .child(SearchField {
                input: self.search.input.clone(),
                disabled: busy,
                searching: self.search.searching,
            });
        if !self.search.searching || self.search.results.is_some() {
            let suggestions = h_flex()
                .flex_wrap()
                .gap(r(6.))
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child("Popular"),
                )
                .children(open_vsx::SUGGESTIONS.map(|suggestion| {
                    Button::new(suggestion, ButtonVariant::Ghost, ButtonSize::Xs)
                        .label(suggestion)
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if this.search.session.query == suggestion {
                                this.settle_search(true, window, cx);
                            } else {
                                this.search
                                    .input
                                    .update(cx, |i, cx| i.set_value(suggestion, window, cx));
                                this.change_search(suggestion, window, cx);
                            }
                        }))
                }));
            let mut bar = h_flex().justify_between().gap(r(8.)).child(suggestions);
            if self.search.results.as_ref().is_some_and(|r| !r.is_empty()) {
                let weak = cx.entity().downgrade();
                let sort = self.search.session.sort.clone();
                bar = bar.child(
                    h_flex()
                        .gap(r(8.))
                        .child(
                            div()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child("Sort"),
                        )
                        .child(
                            Select::new(
                                "open-vsx-sort",
                                open_vsx::SORTS
                                    .iter()
                                    .map(|(id, label)| SelectOption::new(*id, *label))
                                    .collect(),
                            )
                            .selected(sort.clone())
                            .label(
                                open_vsx::SORTS
                                    .iter()
                                    .find(|(id, _)| *id == sort)
                                    .unwrap()
                                    .1,
                            )
                            .width(160.)
                            .disabled(busy)
                            .on_change(move |sort, window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    if this.saving || this.search.session.installing {
                                        return;
                                    }
                                    this.search.session.sort = sort.to_string();
                                    if this.search.debounce.is_none() {
                                        this.settle_search(false, window, cx);
                                    }
                                });
                            }),
                        ),
                );
            }
            body = body.child(bar);
        }
        if self.search.searching && self.search.results.is_none() {
            body = body.child(
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .py(r(24.))
                    .text_sm()
                    .text_color(hsla(p.muted_foreground))
                    .child("Searching themes..."),
            );
        }
        if let Some(error) = &self.search.error {
            body = body.child(
                div()
                    .p(r(12.))
                    .rounded(r(12.))
                    .bg(hsla(p.error_surface))
                    .text_sm()
                    .text_color(hsla(p.error_foreground))
                    .child(error.clone()),
            );
        }
        if self.search.session.installing {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(hsla(p.muted_foreground))
                    .child("Installing..."),
            );
        }
        if let Some(results) = &self.search.results {
            if results.is_empty() {
                body = body.child(
                    v_flex()
                        .h(r(160.))
                        .justify_center()
                        .items_center()
                        .border_1()
                        .border_dashed()
                        .border_color(hsla(p.border))
                        .rounded(r(16.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child("No supported open-source themes found"),
                        )
                        .child(
                            div()
                                .mt(r(4.))
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child("Try a broader search."),
                        ),
                );
            }
            let installed = crate::theme::preferences(cx).custom.clone();
            let cards = results
                .iter()
                .map(|e| {
                    let e = e.clone();
                    let update = installed.iter().any(|t| {
                        t.collection
                            .as_ref()
                            .is_some_and(|c| c.id == e.collection_id)
                    });
                    let action = if self.search.installing_id.as_deref() == Some(&e.id) {
                        if update {
                            "Updating..."
                        } else {
                            "Installing..."
                        }
                    } else if update {
                        "Update"
                    } else {
                        "Install"
                    };
                    let source = e.source_url.clone();
                    let mut actions = h_flex().justify_between();
                    if let Some(url) = source {
                        actions = actions.child(
                            Button::new(
                                gpui::ElementId::Name(format!("source-{}", e.id).into()),
                                ButtonVariant::GhostMuted,
                                ButtonSize::IconMicro,
                            )
                            .icon(crate::icons::Icon::ExternalLink)
                            .tooltip(format!("View source for {}", e.name))
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                        );
                    } else {
                        actions = actions.child(div());
                    }
                    let icon = e.icon_url.clone();
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(r(12.))
                        .p(r(12.))
                        .rounded(r(12.))
                        .border_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .bg(hsla(p.card).opacity(0.6))
                        .child(
                            h_flex()
                                .gap(r(12.))
                                .child(
                                    div()
                                        .size(r(40.))
                                        .rounded(r(8.))
                                        .overflow_hidden()
                                        .bg(hsla(p.muted))
                                        .flex()
                                        .justify_center()
                                        .items_center()
                                        .map(|v| {
                                            if let Some(url) = icon {
                                                v.child(gpui::img(url).size(r(40.)).with_fallback(
                                                    move || {
                                                        crate::icons::Icon::Palette
                                                            .svg()
                                                            .size(r(16.))
                                                            .text_color(hsla(p.muted_foreground))
                                                            .into_any_element()
                                                    },
                                                ))
                                            } else {
                                                v.child(
                                                    crate::icons::Icon::Palette
                                                        .svg()
                                                        .size(r(16.))
                                                        .text_color(hsla(p.muted_foreground)),
                                                )
                                            }
                                        }),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .truncate()
                                                .child(e.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(hsla(p.muted_foreground))
                                                .truncate()
                                                .child(format!(
                                                    "{} · {} downloads",
                                                    e.publisher,
                                                    open_vsx::compact_downloads(e.download_count)
                                                )),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .h(r(32.))
                                .overflow_hidden()
                                .text_xs()
                                .line_height(r(16.))
                                .text_color(hsla(p.muted_foreground))
                                .child(if e.description.is_empty() {
                                    "A community color theme for your editor.".into()
                                } else {
                                    e.description.clone()
                                }),
                        )
                        .child(
                            actions.child(
                                Button::new(
                                    gpui::ElementId::Name(format!("install-{}", e.id).into()),
                                    ButtonVariant::Outline,
                                    ButtonSize::Xs,
                                )
                                .label(action)
                                .disabled(busy)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.download_theme(e.clone(), window, cx)
                                    },
                                )),
                            ),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            let mut cards = cards.into_iter();
            let rows = results.len().div_ceil(2);
            body = body.children((0..rows).map(|_| {
                let mut row = h_flex()
                    .gap(r(8.))
                    .items_start()
                    .children(cards.by_ref().take(2));
                if cards.len() == 0 && results.len() % 2 == 1 {
                    row = row.child(div().flex_1());
                }
                row
            }));
        }
        body.into_any_element()
    }
    #[cfg(feature = "snapshot")]
    pub(super) fn search_fixture(
        &mut self,
        value: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search.cancel();
        self.search.fixture = true;
        self.search.input.update(cx, |input, cx| {
            input.set_value(value["query"].as_str().unwrap_or("Dracula"), window, cx)
        });
        self.search.session.query = value["query"].as_str().unwrap_or("Dracula").into();
        self.search.searching = value["state"] == "loading";
        self.search.error = value["error"].as_str().map(str::to_owned);
        self.search.results = value
            .get("results")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        if value["state"] == "confirmation" {
            let mut theme=themes::import(r##"{"version":1,"name":"Dracula","appearance":"dark","colors":{"accent":"#bd93f9"}}"##).unwrap();
            theme.collection = Some(themes::Collection {
                id: "open-vsx:dracula-theme.theme-dracula".into(),
                label: "Dracula".into(),
            });
            self.marketplace = Some(MarketplaceImport {
                collection_id: theme.collection.as_ref().unwrap().id.clone(),
                expected: vec![theme.clone()],
                themes: vec![theme],
            });
        }
        cx.notify();
    }
}

#[derive(IntoElement)]
struct SearchField {
    input: Entity<InputState>,
    disabled: bool,
    searching: bool,
}
impl gpui::RenderOnce for SearchField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        self.input.update(cx, |input, cx| {
            input.set_disabled(self.disabled, cx);
            input.set_editor_paddings(gpui::Edges::all(gpui::px(0.)));
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
        div()
            .id("theme-search-input")
            .role(gpui::Role::TextInput)
            .aria_label("Search Open VSX themes")
            .h(r(40.))
            .w_full()
            .flex()
            .items_center()
            .gap(r(8.))
            .px(r(12.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(if focused { p.ring } else { p.input }))
            .bg(hsla(p.background))
            .text_sm()
            .line_height(r(20.))
            .child(
                crate::icons::Icon::Search
                    .svg()
                    .size(r(16.))
                    .text_color(hsla(p.muted_foreground)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(gpui_base::input::Input::new(&self.input)),
            )
            .when(self.searching, |v| {
                v.child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child("Searching..."),
                )
            })
    }
}
