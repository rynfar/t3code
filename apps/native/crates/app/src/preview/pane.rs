//! A browser tab in the right panel, as `components/preview/PreviewView.tsx` shows a server tab:
//! the chrome row (`PreviewChromeRow.tsx`: back, forward, refresh, the address bar, open in the
//! system browser), the streamed page (`frame::FrameView`), and over it the empty state
//! (`PreviewEmptyState.tsx` with `PreviewRecentUrlCard` and `PreviewLocalServerCard`), the
//! unreachable page (`PreviewUnreachable.tsx`), or the unavailable message (`PreviewPanel.tsx`).
//! Its root is the `previewFocus` key context, where `cmd-r`, `cmd-l` and the zoom keys act.

use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, Context, ElementId, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StyleRefinement, Styled, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_client::session::RpcError;
use t3_core::preview::address::{DEFAULT_SEARCH_TEMPLATE, HistoryEntry, RECENT_SHOWN, UrlError, normalize_preview_url, resolve_address_bar_input};
use t3_core::preview::stream::StreamInput;
use t3_core::preview::{DiscoveredServer, NavStatus, SessionSnapshot, decode_discovered_servers, describe_error, error_label, host_label, method, shows_empty_state, step_zoom};

use super::frame::{CONTROL_BAR_HEIGHT, FrameEvent, FrameView};
use super::{AUTHORIZATION_ERROR, Abort, Availability, FocusUrl, Previews, PreviewsEvent, Refresh, ResetZoom, ZoomIn, ZoomOut};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::settings::controls::{Button as ActionButton, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

pub struct PreviewPane {
    state: Entity<AppState>,
    previews: Entity<Previews>,
    thread_id: String,
    surface_id: String,
    tab_id: Option<String>,
    url: Entity<InputState>,
    url_focused: bool,
    frame: Option<Entity<FrameView>>,
    focus: FocusHandle,
    visible: bool,
    servers: Vec<DiscoveredServer>,
    servers_feed: Option<(Abort, Task<()>)>,
    unreachable_details: bool,
    _subscriptions: Vec<Subscription>,
}

impl PreviewPane {
    pub fn new(
        state: Entity<AppState>,
        previews: Entity<Previews>,
        thread_id: String,
        surface_id: String,
        tab_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("Search or enter URL"));
        let input_events = cx.subscribe_in(&url, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Focus => {
                this.url_focused = true;
                this.url.update(cx, |input, cx| input.select_all(window, cx));
                cx.notify();
            }
            InputEvent::Blur => {
                this.url_focused = false;
                this.show_url(window, cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.submit(window, cx),
            _ => {}
        });
        let preview_events = cx.subscribe_in(&previews, window, |this, _, event: &PreviewsEvent, window, cx| {
            let PreviewsEvent::Thread(thread) = event;
            if *thread == this.thread_id {
                this.sync(window, cx);
            }
        });
        let mut pane = PreviewPane {
            state,
            previews,
            thread_id,
            surface_id,
            tab_id,
            url,
            url_focused: false,
            frame: None,
            focus: cx.focus_handle(),
            visible: false,
            servers: Vec::new(),
            servers_feed: None,
            unreachable_details: false,
            _subscriptions: vec![input_events, preview_events],
        };
        pane.sync(window, cx);
        pane
    }

    pub fn surface_id(&self) -> &str {
        &self.surface_id
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    fn snapshot<'a>(&self, cx: &'a App) -> Option<&'a SessionSnapshot> {
        self.previews.read(cx).session(&self.thread_id, self.tab_id.as_deref()?)
    }

    fn page_url(&self, cx: &App) -> String {
        self.snapshot(cx).and_then(|snapshot| snapshot.nav_status.url()).unwrap_or_default().to_owned()
    }

    /// The tab shows only while it is the panel's active tab: its stream and local server scan
    /// run only then.
    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            if let (true, Some(tab)) = (visible, &self.tab_id) {
                let (thread, tab) = (self.thread_id.clone(), tab.clone());
                self.previews.update(cx, |previews, _| previews.set_active(&thread, &tab));
            }
            self.sync(window, cx);
        }
    }

    /// Follows the session: its frame view exists while the server knows the tab.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_session = self.snapshot(cx).is_some();
        match (&self.frame, has_session, self.tab_id.clone()) {
            (None, true, Some(tab_id)) => {
                let frame = cx.new(|cx| FrameView::new(self.state.clone(), self.previews.clone(), self.thread_id.clone(), tab_id, window, cx));
                self._subscriptions.push(cx.subscribe(&frame, |_, _, _: &FrameEvent, cx| cx.notify()));
                self.frame = Some(frame);
            }
            (Some(_), false, _) => self.frame = None,
            _ => {}
        }
        if let Some(frame) = &self.frame {
            let visible = self.visible;
            frame.update(cx, |frame, cx| frame.set_visible(visible, cx));
        }
        let wants_servers = self.visible && self.previews.read(cx).availability() == Availability::Available && shows_empty_state(self.snapshot(cx));
        if wants_servers != self.servers_feed.is_some() {
            if wants_servers { self.watch_servers(cx) } else { self.servers_feed = None }
        }
        if !self.url_focused {
            self.show_url(window, cx);
        }
        cx.notify();
    }

    fn show_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.page_url(cx);
        if self.url.read(cx).value().as_ref() != url {
            self.url.update(cx, |input, cx| input.set_value(url, window, cx));
        }
    }

    /// `subscribeDiscoveredLocalServers`, held only while the empty state shows.
    fn watch_servers(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<DiscoveredServer>>();
        let driver = state.runtime().spawn(async move {
            let mut attempt = 0u32;
            loop {
                match client.stream(method::SUBSCRIBE_DISCOVERED_SERVERS, json!({})).await {
                    Ok(mut stream) => {
                        while let Some(item) = stream.receiver.recv().await {
                            match item {
                                Ok(value) => {
                                    attempt = 0;
                                    if tx.send(decode_discovered_servers(&value)).is_err() {
                                        return;
                                    }
                                }
                                Err(RpcError::Failure(failure)) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) => return,
                                Err(_) => break,
                            }
                        }
                    }
                    Err(RpcError::Failure(failure)) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) => return,
                    Err(_) => {}
                }
                if tx.is_closed() {
                    return;
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_millis((500u64 << attempt.min(4)).min(8_000))).await;
            }
        });
        let pump = cx.spawn(async move |this, cx| {
            while let Some(mut servers) = rx.recv().await {
                while let Ok(newer) = rx.try_recv() {
                    servers = newer;
                }
                let applied = this.update(cx, |this, cx| {
                    if this.servers != servers {
                        this.servers = servers;
                        cx.notify();
                    }
                });
                if applied.is_err() {
                    return;
                }
            }
        });
        self.servers_feed = Some((Abort(driver), pump));
    }

    fn toast(&self, title: &'static str, description: &'static str, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, title).description(description), cx));
    }

    /// `navigateToResolvedUrl`: the page navigates while this viewer controls it; without a
    /// session a new one opens on the address.
    fn navigate_to(&mut self, url: String, cx: &mut Context<Self>) -> bool {
        if let Some(frame) = &self.frame {
            return frame.update(cx, |frame, _| frame.send(StreamInput::Navigate { url }));
        }
        if self.tab_id.is_some() {
            return false;
        }
        let thread = self.thread_id.clone();
        self.previews.update(cx, |previews, cx| previews.open_session(thread, Some(url), cx));
        true
    }

    /// `handleSubmitUrl`: a URL, or a search for the text.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.url.read(cx).value().to_string();
        match resolve_address_bar_input(&typed, DEFAULT_SEARCH_TEMPLATE) {
            Ok(url) => {
                if self.navigate_to(url.clone(), cx) {
                    let thread = self.thread_id.clone();
                    self.previews.update(cx, |previews, cx| previews.record_visit(&thread, &url, cx));
                    if let Some(frame) = &self.frame {
                        window.focus(&gpui::Focusable::focus_handle(frame.read(cx), cx), cx);
                    }
                }
            }
            Err(UrlError::Empty) => {}
            Err(_) => self.toast("Could not open this address", UrlError::MESSAGE, cx),
        }
    }

    /// `handleOpenServerUrl`: a recently used address or a discovered server.
    fn open_listed(&mut self, raw: &str, cx: &mut Context<Self>) {
        match normalize_preview_url(raw) {
            Ok(url) => {
                if self.navigate_to(url, cx) {
                    let thread = self.thread_id.clone();
                    let raw = raw.to_owned();
                    self.previews.update(cx, |previews, cx| previews.record_visit(&thread, &raw, cx));
                }
            }
            Err(_) => self.toast("Could not open this address", UrlError::MESSAGE, cx),
        }
    }

    fn send(&mut self, input: StreamInput, cx: &mut Context<Self>) {
        if let Some(frame) = &self.frame {
            frame.update(cx, |frame, _| frame.send(input));
        }
    }

    fn zoom(&mut self, direction: i8, cx: &mut Context<Self>) {
        let (Some(tab), Some(current)) = (self.tab_id.clone(), self.snapshot(cx).map(SessionSnapshot::zoom)) else { return };
        let next = step_zoom(current, direction);
        if (next - current).abs() > f64::EPSILON {
            let thread = self.thread_id.clone();
            self.previews.update(cx, |previews, cx| previews.adjust(&thread, &tab, json!({ "zoomFactor": next }), cx));
        }
    }

    fn render_chrome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.snapshot(cx);
        let frame = self.frame.as_ref().map(|frame| frame.read(cx));
        // Until this viewer controls the tab, page navigation is the controller's.
        let input_disabled = frame.is_some_and(|frame| !frame.controlled());
        let loading = snapshot.is_some_and(|snapshot| matches!(snapshot.nav_status, NavStatus::Loading { .. }))
            || (frame.is_some_and(|frame| !frame.has_frame()) && !shows_empty_state(snapshot) && !matches!(snapshot.map(|s| &s.nav_status), Some(NavStatus::LoadFailed { .. })));
        let can_back = snapshot.is_some_and(|snapshot| snapshot.can_go_back) && !input_disabled;
        let can_forward = snapshot.is_some_and(|snapshot| snapshot.can_go_forward) && !input_disabled;
        let refresh_disabled = shows_empty_state(snapshot) || input_disabled;
        let url = self.page_url(cx);
        let p = palette(cx);
        h_flex()
            .flex_none()
            .h(px(28.))
            .mb(px(12.))
            .px(px(8.))
            .gap(px(4.))
            .child(
                h_flex()
                    .gap(px(2.))
                    .child(
                        Button::new("preview-back").variant(Variant::Ghost).size(Size::IconXs).icon(Icon::ArrowLeft).tooltip("Back").disabled(!can_back).on_click(cx.listener(
                            |this, _, _, cx| this.send(StreamInput::History { delta: -1 }, cx),
                        )),
                    )
                    .child(
                        Button::new("preview-forward")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::ArrowRight)
                            .tooltip("Forward")
                            .disabled(!can_forward)
                            .on_click(cx.listener(|this, _, _, cx| this.send(StreamInput::History { delta: 1 }, cx))),
                    )
                    .child(
                        Button::new("preview-refresh")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(if loading { Icon::LoaderCircle } else { Icon::RefreshCw })
                            .tooltip(if loading { "Loading…" } else { "Refresh" })
                            .disabled(refresh_disabled)
                            .on_click(cx.listener(|this, _, _, cx| this.send(StreamInput::Reload { ignore_cache: None }, cx))),
                    ),
            )
            .child(
                h_flex()
                    .id("preview-address")
                    .flex_1()
                    .min_w_0()
                    .h(px(28.))
                    .pl(px(8.))
                    .pr(px(2.))
                    .rounded(px(8.))
                    .when(self.url_focused, |this| this.bg(hsla(p.background)))
                    .when(!self.url_focused, |this| this.hover(|style| style.bg(hsla(p.muted).opacity(0.4))))
                    .when(input_disabled, |this| this.opacity(0.64))
                    .child(div().flex_1().min_w_0().text_sm().child(Input::new(&self.url).appearance(false).bordered(false).disabled(input_disabled)))
                    .when(!url.is_empty() && !self.url_focused, |this| {
                        this.child(
                            Button::new("preview-open-external")
                                .variant(Variant::Ghost)
                                .size(Size::IconXs)
                                .icon(Icon::ExternalLink)
                                .tooltip("Open in system browser")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }),
            )
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let recents: Vec<HistoryEntry> =
            self.previews.read(cx).recent(&self.thread_id, cx).into_iter().take(RECENT_SHOWN).collect();
        if recents.is_empty() && self.servers.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .px(px(24.))
                .text_center()
                .child(
                    div()
                        .mb(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(36.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(p.card))
                        .child(Icon::Globe.svg().size(px(18.)).text_color(hsla(p.muted_foreground))),
                )
                .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child("No preview yet"))
                .child(
                    div()
                        .mt(px(4.))
                        .max_w(px(384.))
                        .text_sm()
                        .text_color(hsla(p.muted_foreground))
                        .child("Type a URL above, or run a dev script. Local servers this thread starts will show up here automatically."),
                )
                .into_any_element();
        }
        let header = |icon: Icon, label: &'static str| {
            h_flex()
                .gap(px(8.))
                .text_sm()
                .text_color(hsla(p.muted_foreground))
                .child(icon.svg().size(px(16.)).text_color(hsla(p.muted_foreground)))
                .child(div().font_weight(FontWeight::MEDIUM).child(label))
        };
        let list = || v_flex().overflow_hidden().rounded(px(12.)).border_1().border_color(hsla(p.border).opacity(0.7)).bg(hsla(p.background));
        let recent_rows: Vec<AnyElement> = recents
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let label = entry.label();
                let (title, subtitle) = match &entry.title {
                    Some(title) => (title.clone(), label.clone()),
                    None => (label.clone(), String::new()),
                };
                let (open_url, remove_url) = (entry.url.clone(), entry.url.clone());
                self.row(
                    ElementId::from(("preview-recent", index)),
                    title,
                    subtitle,
                    index > 0,
                    cx.listener(move |this, _, _, cx| this.open_listed(&open_url, cx)),
                    Some(
                        Button::new(("preview-recent-remove", index))
                            .variant(Variant::GhostMuted)
                            .size(Size::IconMicro)
                            .icon(Icon::X)
                            .tooltip(SharedString::from(format!("Remove {label} from history")))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let thread = this.thread_id.clone();
                                this.previews.update(cx, |previews, cx| previews.remove_recent(&thread, &remove_url, cx));
                            }))
                            .into_any_element(),
                    ),
                    cx,
                )
            })
            .collect();
        let server_rows: Vec<AnyElement> = self
            .servers
            .iter()
            .enumerate()
            .map(|(index, server)| {
                let url = server.url.clone();
                self.row(
                    ElementId::from(("preview-server", index)),
                    server.title().to_owned(),
                    server.address(),
                    index > 0,
                    cx.listener(move |this, _, _, cx| this.open_listed(&url, cx)),
                    None,
                    cx,
                )
            })
            .collect();
        div()
            .id("preview-empty")
            .size_full()
            .overflow_y_scroll()
            .px(px(20.))
            .py(px(32.))
            .child(
                v_flex()
                    .mx_auto()
                    .w_full()
                    .max_w(px(576.))
                    .gap(px(24.))
                    .when(!recent_rows.is_empty(), |this| this.child(v_flex().gap(px(12.)).child(header(Icon::History, "Recently used")).child(list().children(recent_rows))))
                    .when(!server_rows.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap(px(12.))
                                .child(header(Icon::RadioTower, "Local servers"))
                                .child(list().children(server_rows))
                                .child(div().px(px(4.)).text_xs().text_color(hsla(p.muted_foreground)).child("Select a live local server to open it in this browser tab.")),
                        )
                    }),
            )
            .into_any_element()
    }

    /// `DiscoveryListRow`: a 28px page mark, a title and a muted line.
    #[allow(clippy::too_many_arguments)]
    fn row(
        &self,
        id: ElementId,
        title: String,
        subtitle: String,
        divided: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        trailing: Option<AnyElement>,
        cx: &App,
    ) -> AnyElement {
        let p = palette(cx);
        h_flex()
            .id(id)
            .w_full()
            .gap(px(12.))
            .px(px(12.))
            .py(px(12.))
            .cursor_pointer()
            .when(divided, |this| this.border_t_1().border_color(hsla(p.border).opacity(0.6)))
            .hover(|style| style.bg(hsla(p.accent).opacity(0.4)))
            .on_click(on_click)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .rounded(px(4.))
                    .bg(hsla(p.muted))
                    .child(Icon::Globe.svg().size(px(16.)).text_color(hsla(p.muted_foreground))),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
                    .when(!subtitle.is_empty(), |this| this.child(div().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(subtitle))),
            )
            .children(trailing)
            .into_any_element()
    }

    fn render_unreachable(&self, url: &str, code: i64, description: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let host = host_label(url).unwrap_or_else(|| url.to_owned());
        let details = self.unreachable_details;
        div()
            .id("preview-unreachable")
            .size_full()
            .overflow_y_scroll()
            .bg(hsla(p.background))
            .child(
                v_flex()
                    .mx_auto()
                    .w_full()
                    .max_w(px(576.))
                    .min_h_full()
                    .px(px(32.))
                    .py(px(64.))
                    .child(Icon::PreviewError.svg().mb(px(24.)).size(px(48.)).text_color(hsla(p.muted_foreground).opacity(0.7)))
                    .child(div().mb(px(12.)).text_2xl().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child("This site can\u{2019}t be reached"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(hsla(p.muted_foreground))
                            .child(gpui::StyledText::new(format!("{host}: {}.", describe_error(description))).with_highlights([(
                                0..host.len(),
                                gpui::HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), color: Some(hsla(p.foreground)), ..Default::default() },
                            )])),
                    )
                    .when(details, |this| {
                        let item = |text: &'static str| h_flex().gap(px(8.)).child("•").child(text);
                        this.child(
                            v_flex()
                                .mt(px(24.))
                                .gap(px(4.))
                                .p(px(16.))
                                .rounded(px(8.))
                                .border_1()
                                .border_color(hsla(p.border))
                                .bg(hsla(p.muted).opacity(0.4))
                                .text_sm()
                                .text_color(hsla(p.muted_foreground))
                                .child(div().mb(px(4.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Try:"))
                                .child(item("Checking your connection"))
                                .child(item("Confirming the dev server is running"))
                                .child(item("Checking the proxy and the firewall")),
                        )
                    })
                    .child(div().mt(px(32.)).text_xs().text_color(hsla(p.muted_foreground).opacity(0.7)).child(error_label(code, description).to_uppercase()))
                    .child(
                        h_flex()
                            .mt_auto()
                            .pt(px(32.))
                            .gap(px(8.))
                            .child(ActionButton::new("preview-details", ButtonVariant::Outline, ButtonSize::Sm).label(if details { "Hide details" } else { "Details" }).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.unreachable_details = !this.unreachable_details;
                                    cx.notify();
                                }),
                            ))
                            .child(div().flex_1())
                            .child(
                                ActionButton::new("preview-reload", ButtonVariant::Default, ButtonSize::Sm)
                                    .label("Reload")
                                    .on_click(cx.listener(|this, _, _, cx| this.send(StreamInput::Reload { ignore_cache: None }, cx))),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for PreviewPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let availability = self.previews.read(cx).availability();
        let root = v_flex()
            .id(SharedString::from(format!("preview-pane-{}", self.surface_id)))
            .key_context("Preview")
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .bg(hsla(p.background));
        if let Some(message) = availability.message() {
            return root
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap(px(12.))
                        .p(px(32.))
                        .text_center()
                        .child(div().max_w(px(384.)).text_sm().text_color(hsla(p.muted_foreground)).child(message)),
                )
                .into_any_element();
        }
        let snapshot = self.snapshot(cx).cloned();
        let blocked = self.frame.as_ref().is_some_and(|frame| frame.read(cx).blocks());
        let empty = shows_empty_state(snapshot.as_ref()) && !blocked;
        let unreachable = match snapshot.as_ref().map(|snapshot| &snapshot.nav_status) {
            Some(NavStatus::LoadFailed { url, code, description, .. }) if !blocked => Some(self.render_unreachable(url, *code, description, cx)),
            _ => None,
        };
        // The stream's control bar stays above these, as the web keeps it visible, so control of an
        // agent's tab can still be taken from them.
        let top = if self.frame.is_some() { CONTROL_BAR_HEIGHT } else { 0. };
        let body = div()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .children(self.frame.clone().map(|frame| frame.cached(StyleRefinement::default().size_full())))
            .when(empty, |this| {
                this.child(div().absolute().top(px(top)).bottom_0().left_0().right_0().bg(hsla(p.background)).child(self.render_empty(cx)))
            })
            .children(unreachable.map(|page| div().absolute().top(px(top)).bottom_0().left_0().right_0().child(page)));
        root.on_action(cx.listener(|this, _: &Refresh, _, cx| this.send(StreamInput::Reload { ignore_cache: None }, cx)))
            .on_action(cx.listener(|this, _: &FocusUrl, window, cx| {
                this.url.update(cx, |input, cx| {
                    input.focus(window, cx);
                    input.select_all(window, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom(1, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom(-1, cx)))
            .on_action(cx.listener(|this, _: &ResetZoom, _, cx| this.zoom(0, cx)))
            .child(self.render_chrome(cx))
            .child(body)
            .into_any_element()
    }
}
