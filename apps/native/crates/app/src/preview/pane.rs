//! A browser tab in the right panel, as `components/preview/PreviewView.tsx` shows a server tab:
//! the chrome row (`PreviewChromeRow.tsx`: back, forward, refresh/stop, profile badge, address bar,
//! open in system browser, annotate button, more menu), device toolbar (`BrowserDeviceToolbar.tsx`),
//! zoom indicator (`ZoomIndicator.tsx`), the streamed page (`frame::FrameView`), annotation
//! composer with voice dictation (`PreviewAnnotationSpeech.tsx`, `pingdotgg/t3code#30624`), and
//! over it the empty state (`PreviewEmptyState.tsx` with `PreviewRecentUrlCard` and
//! `PreviewLocalServerCard`), the file-not-shown / unreachable page (`PreviewUnreachable.tsx`),
//! or the unavailable message (`PreviewPanel.tsx`).
//! Its root is the `Preview` key context, where `cmd-r`, `cmd-l`, zoom keys, and `cmd-shift-t` act.

use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, Context, ElementId, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, Render, SharedString, StyleRefinement, Styled, Subscription, Task, Window, deferred, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_client::session::RpcError;
use t3_core::composer::attachments::AttachmentKind;
use t3_core::preview::address::{HistoryEntry, RECENT_SHOWN, UrlError, normalize_preview_url, resolve_address_bar_input};
use t3_core::preview::annotation::{AnnotationTarget, PreviewAnnotationDraft, format_annotation_prompt};
use t3_core::preview::stream::StreamInput;
use t3_core::preview::viewport::{
    AppearancePreference, BROWSER_DEVICE_TOOLBAR_HEIGHT, ViewportSetting, profile_name, responsive_viewport_for_toggle,
};
use t3_core::preview::{
    DiscoveredServer, FileDownload, NavStatus, SessionSnapshot, decode_discovered_servers, describe_error, error_label, host_label, method,
    shows_empty_state, step_zoom,
};

use super::annotation::{AnnotationEditor, AnnotationEditorEvent};
use super::device_toolbar::{DeviceToolbar, DeviceToolbarEvent};
use super::frame::{CONTROL_BAR_HEIGHT, FrameEvent, FrameView};
use super::{
    AUTHORIZATION_ERROR, Abort, Availability, FocusUrl, Previews, PreviewsEvent, Refresh, ReopenClosedTab, ResetZoom, ZoomIn, ZoomOut,
};
use crate::app_state::{AppState, RestoredAttachment};
use crate::composer::controls::{MenuItem, popup_surface};
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::settings::controls::{Button as ActionButton, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

const ZOOM_INDICATOR_HIDE_MS: u64 = 1_500;
const ZOOM_EPSILON: f64 = 0.001;

pub struct PreviewPane {
    state: Entity<AppState>,
    previews: Entity<Previews>,
    thread_id: String,
    surface_id: String,
    tab_id: Option<String>,
    url: Entity<InputState>,
    url_focused: bool,
    frame: Option<Entity<FrameView>>,
    frame_sub: Option<Subscription>,
    device_toolbar: Option<Entity<DeviceToolbar>>,
    device_toolbar_sub: Option<Subscription>,
    annotation: Option<Entity<AnnotationEditor>>,
    annotation_sub: Option<Subscription>,
    pick_active: bool,
    more_menu_open: bool,
    last_zoom: Option<f64>,
    zoom_indicator: Option<SharedString>,
    zoom_indicator_task: Option<Task<()>>,
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
        cx.on_release(|this: &mut PreviewPane, cx| {
            if let Some(editor) = this.annotation.take() {
                editor.update(cx, |editor, cx| editor.dispose(cx));
            }
        })
        .detach();
        let mut pane = PreviewPane {
            state,
            previews,
            thread_id,
            surface_id,
            tab_id,
            url,
            url_focused: false,
            frame: None,
            frame_sub: None,
            device_toolbar: None,
            device_toolbar_sub: None,
            annotation: None,
            annotation_sub: None,
            pick_active: false,
            more_menu_open: false,
            last_zoom: None,
            zoom_indicator: None,
            zoom_indicator_task: None,
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

    fn page_title(&self, cx: &App) -> String {
        self.snapshot(cx).and_then(|snapshot| snapshot.nav_status.title()).unwrap_or_default().to_owned()
    }

    /// The tab shows only while it is the panel's active tab: its stream and local server scan
    /// run only then.
    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            if !visible {
                self.cancel_annotation(window, cx);
                self.more_menu_open = false;
            } else if let Some(tab) = &self.tab_id {
                let (thread, tab) = (self.thread_id.clone(), tab.clone());
                self.previews.update(cx, |previews, _| previews.set_active(&thread, &tab));
            }
            self.sync(window, cx);
        }
    }

    /// Follows the session: its frame view exists while the server knows the tab.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx).cloned();
        let has_session = snapshot.is_some();
        match (&self.frame, has_session, self.tab_id.clone()) {
            (None, true, Some(tab_id)) => {
                let frame = cx.new(|cx| FrameView::new(self.state.clone(), self.previews.clone(), self.thread_id.clone(), tab_id, window, cx));
                self.frame_sub = Some(cx.subscribe_in(&frame, window, |this, _, event: &FrameEvent, window, cx| match event {
                    FrameEvent::Changed => cx.notify(),
                    FrameEvent::Picked(target) => {
                        this.pick_active = false;
                        this.open_annotation_editor(target.clone(), window, cx);
                    }
                    FrameEvent::CancelPick => {
                        this.pick_active = false;
                        cx.notify();
                    }
                }));
                self.frame = Some(frame);
            }
            (Some(_), false, _) => {
                self.cancel_annotation(window, cx);
                self.frame = None;
                self.frame_sub = None;
            }
            _ => {}
        }
        if let Some(frame) = &self.frame {
            let visible = self.visible;
            let fills_panel = snapshot.as_ref().is_none_or(SessionSnapshot::fills_panel);
            frame.update(cx, |frame, cx| {
                frame.set_follow_size(fills_panel);
                frame.set_visible(visible, cx);
            });
        }
        if let Some(snapshot) = &snapshot {
            let zoom = snapshot.zoom();
            if let Some(previous) = self.last_zoom
                && (previous - zoom).abs() > ZOOM_EPSILON
            {
                self.show_zoom_indicator(zoom, cx);
            }
            self.last_zoom = Some(zoom);
            let viewport = snapshot.viewport_setting();
            let panel_width = self.frame.as_ref().and_then(|frame| frame.read(cx).panel_size()).map_or(800., |(w, _)| w as f32);
            if viewport.is_fill() {
                self.device_toolbar = None;
                self.device_toolbar_sub = None;
            } else if let Some(toolbar) = &self.device_toolbar {
                toolbar.update(cx, |toolbar, cx| toolbar.sync_setting(viewport, panel_width, window, cx));
            } else {
                let toolbar = cx.new(|cx| DeviceToolbar::new(viewport, window, cx));
                self.device_toolbar_sub = Some(cx.subscribe(&toolbar, |this, _, event: &DeviceToolbarEvent, cx| {
                    let DeviceToolbarEvent::Change(setting) = event;
                    this.commit_viewport(setting.clone(), cx);
                }));
                self.device_toolbar = Some(toolbar);
            }
        } else {
            self.device_toolbar = None;
            self.device_toolbar_sub = None;
        }
        let wants_servers = self.visible && self.previews.read(cx).availability() == Availability::Available && shows_empty_state(snapshot.as_ref());
        if wants_servers != self.servers_feed.is_some() {
            if wants_servers { self.watch_servers(cx) } else { self.servers_feed = None }
        }
        if !self.url_focused {
            self.show_url(window, cx);
        }
        cx.notify();
    }

    fn show_zoom_indicator(&mut self, zoom: f64, cx: &mut Context<Self>) {
        self.zoom_indicator = Some(SharedString::from(format!("{}%", (zoom * 100.).round() as u32)));
        self.zoom_indicator_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(ZOOM_INDICATOR_HIDE_MS)).await;
            let _ = this.update(cx, |this, cx| {
                this.zoom_indicator = None;
                this.zoom_indicator_task = None;
                cx.notify();
            });
        }));
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
    /// session a new one opens on the address (which records its own history once opened).
    /// Returns `Some(profile_id)` when an existing tab navigated, or `None` when no visit should
    /// be recorded directly here.
    fn navigate_to(&mut self, url: String, cx: &mut Context<Self>) -> Option<Option<String>> {
        if let Some(frame) = &self.frame {
            let profile_id = self.snapshot(cx).and_then(|snapshot| snapshot.profile_id.clone());
            return frame.update(cx, |frame, _| frame.send(StreamInput::Navigate { url })).then_some(profile_id);
        }
        if self.tab_id.is_some() {
            return None;
        }
        let thread = self.thread_id.clone();
        self.previews.update(cx, |previews, cx| previews.open_session(thread, Some(url), cx));
        None
    }

    /// `handleSubmitUrl`: a URL, or a search for the text. Incognito visits stay out of
    /// persisted project history (`pingdotgg/t3code#34859`).
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.url.read(cx).value().to_string();
        match resolve_address_bar_input(&typed, crate::settings::client_settings(cx).browser_search_template()) {
            Ok(url) => {
                if let Some(profile_id) = self.navigate_to(url.clone(), cx) {
                    let thread = self.thread_id.clone();
                    self.previews.update(cx, |previews, cx| previews.record_visit(&thread, profile_id.as_deref(), &url, cx));
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
                if let Some(profile_id) = self.navigate_to(url, cx) {
                    let thread = self.thread_id.clone();
                    let raw = raw.to_owned();
                    self.previews.update(cx, |previews, cx| previews.record_visit(&thread, profile_id.as_deref(), &raw, cx));
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

    fn adjust(&mut self, change: serde_json::Value, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_id.clone() else { return };
        let thread = self.thread_id.clone();
        self.previews.update(cx, |previews, cx| previews.adjust(&thread, &tab, change, cx));
    }

    fn commit_viewport(&mut self, setting: ViewportSetting, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_id.clone() else { return };
        let thread = self.thread_id.clone();
        self.previews.update(cx, |previews, cx| previews.resize(&thread, &tab, setting, cx));
    }

    fn toggle_device_toolbar(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot(cx).cloned() else { return };
        if !snapshot.fills_panel() {
            self.commit_viewport(ViewportSetting::Fill, cx);
            return;
        }
        let defaults = self.previews.read(cx).browser_defaults(cx);
        let panel_rect = self.frame.as_ref().and_then(|frame| frame.read(cx).panel_size());
        let next = responsive_viewport_for_toggle(&defaults.viewport, panel_rect, snapshot.zoom());
        self.commit_viewport(next, cx);
    }

    fn zoom(&mut self, direction: i8, cx: &mut Context<Self>) {
        let (Some(tab), Some(current)) = (self.tab_id.clone(), self.snapshot(cx).map(SessionSnapshot::zoom)) else { return };
        let next = step_zoom(current, direction);
        if (next - current).abs() > f64::EPSILON {
            let thread = self.thread_id.clone();
            self.previews.update(cx, |previews, cx| previews.adjust(&thread, &tab, json!({ "zoomFactor": next }), cx));
        }
    }

    fn toggle_annotate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pick_active || self.annotation.is_some() {
            self.cancel_annotation(window, cx);
            return;
        }
        let Some(frame) = &self.frame else { return };
        self.more_menu_open = false;
        self.pick_active = true;
        frame.update(cx, |frame, cx| frame.set_pick_mode(true, window, cx));
        cx.notify();
    }

    fn open_annotation_editor(&mut self, target: AnnotationTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_annotation(window, cx);
        let Some(tab_id) = self.tab_id.clone() else { return };
        let page_url = self.page_url(cx);
        let page_title = self.page_title(cx);
        let editor = cx.new(|cx| {
            AnnotationEditor::new(
                self.state.clone(),
                self.thread_id.clone(),
                tab_id,
                page_url,
                page_title,
                target,
                window,
                cx,
            )
        });
        self.annotation_sub = Some(cx.subscribe_in(&editor, window, |this, _, event: &AnnotationEditorEvent, window, cx| match event {
            AnnotationEditorEvent::Submit(draft) => this.submit_annotation(draft.clone(), window, cx),
            AnnotationEditorEvent::Cancel => this.cancel_annotation(window, cx),
        }));
        self.annotation = Some(editor);
        cx.notify();
    }

    fn submit_annotation(&mut self, draft: PreviewAnnotationDraft, window: &mut Window, cx: &mut Context<Self>) {
        let screenshot = self
            .frame
            .as_ref()
            .and_then(|frame| frame.read(cx).capture_annotation_png(&draft.target))
            .map(|bytes| RestoredAttachment {
                kind: AttachmentKind::Image,
                name: "preview-annotation.png".to_owned(),
                mime_type: "image/png".to_owned(),
                bytes,
            });
        let prompt = format_annotation_prompt(&draft);
        let attachments = screenshot.into_iter().collect();
        let thread_id = self.thread_id.clone();
        self.state.update(cx, |state, cx| {
            state.restore_composer_prompt(&thread_id, prompt, attachments, cx);
            state.show_toast(Toast::new(ToastKind::Success, "Annotation added to composer"), cx);
        });
        self.cancel_annotation(window, cx);
    }

    fn cancel_annotation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_active = false;
        if let Some(frame) = &self.frame {
            frame.update(cx, |frame, cx| frame.set_pick_mode(false, window, cx));
        }
        if let Some(editor) = self.annotation.take() {
            editor.update(cx, |editor, cx| editor.dispose(cx));
        }
        self.annotation_sub = None;
        cx.notify();
    }

    fn render_chrome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.snapshot(cx);
        let frame = self.frame.as_ref().map(|frame| frame.read(cx));
        // Until this viewer controls the tab, page navigation is the controller's.
        let input_disabled = frame.is_some_and(|frame| !frame.controlled());
        let is_unreachable = matches!(snapshot.map(|s| &s.nav_status), Some(NavStatus::LoadFailed { .. }));
        let empty = shows_empty_state(snapshot);
        let loading = snapshot.is_some_and(|snapshot| matches!(snapshot.nav_status, NavStatus::Loading { .. }))
            || (frame.is_some_and(|frame| !frame.has_frame()) && !empty && !is_unreachable);
        let can_back = snapshot.is_some_and(|snapshot| snapshot.can_go_back) && !input_disabled;
        let can_forward = snapshot.is_some_and(|snapshot| snapshot.can_go_forward) && !input_disabled;
        let refresh_disabled = empty || input_disabled;
        let url = self.page_url(cx);
        let defaults = self.previews.read(cx).browser_defaults(cx);
        let active_profile_id = snapshot.map_or(defaults.profile_id.as_str(), SessionSnapshot::profile_id_or_default);
        let show_profile_badge = active_profile_id != defaults.profile_id;
        let active_profile_name = profile_name(&defaults.profiles, active_profile_id);
        let annotate_active = self.pick_active || self.annotation.is_some();
        let annotate_disabled = self.tab_id.is_none() || empty || is_unreachable || frame.is_none_or(|frame| !frame.has_frame());
        let p = palette(cx);

        div()
            .relative()
            .flex_none()
            .child(
                h_flex()
                    .h(px(28.))
                    .mb(px(12.))
                    .px(px(8.))
                    .gap(px(4.))
                    .child(
                        h_flex()
                            .gap(px(2.))
                            .child(
                                Button::new("preview-back")
                                    .variant(Variant::Ghost)
                                    .size(Size::IconXs)
                                    .icon(Icon::ArrowLeft)
                                    .tooltip("Back")
                                    .disabled(!can_back)
                                    .on_click(cx.listener(|this, _, _, cx| this.send(StreamInput::History { delta: -1 }, cx))),
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
                                    .tooltip(if loading { "Stop" } else { "Refresh" })
                                    .disabled(refresh_disabled)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send(if loading { StreamInput::StopLoading } else { StreamInput::Reload { ignore_cache: None } }, cx);
                                    })),
                            ),
                    )
                    .when(show_profile_badge, |this| {
                        this.child(
                            div()
                                .id("preview-profile-badge")
                                .flex_none()
                                .max_w(px(112.))
                                .h(px(20.))
                                .px(px(6.))
                                .rounded(px(999.))
                                .border_1()
                                .border_color(hsla(p.border))
                                .bg(hsla(p.muted).opacity(0.4))
                                .flex()
                                .items_center()
                                .child(div().truncate().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(active_profile_name)),
                        )
                    })
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
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                if event.keystroke.key == "escape" && this.url_focused {
                                    cx.stop_propagation();
                                    this.url_focused = false;
                                    this.show_url(window, cx);
                                    if let Some(frame) = &this.frame {
                                        window.focus(&gpui::Focusable::focus_handle(frame.read(cx), cx), cx);
                                    } else {
                                        window.blur();
                                    }
                                    cx.notify();
                                }
                            }))
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
                    .child(
                        Button::new("preview-annotate")
                            .variant(if annotate_active { Variant::Secondary } else { Variant::Ghost })
                            .size(Size::IconXs)
                            .icon(Icon::MousePointerClick)
                            .tooltip(if annotate_disabled && is_unreachable {
                                "Page didn't load. Pick is unavailable until the page renders"
                            } else if annotate_active {
                                "Cancel annotation (Esc)"
                            } else {
                                "Annotate elements, regions, and drawings"
                            })
                            .disabled(annotate_disabled && !annotate_active)
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_annotate(window, cx))),
                    )
                    .child(
                        Button::new("preview-more-menu")
                            .variant(if self.more_menu_open { Variant::Secondary } else { Variant::Ghost })
                            .size(Size::IconXs)
                            .icon(Icon::Ellipsis)
                            .tooltip("More")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.more_menu_open = !this.more_menu_open;
                                cx.notify();
                            })),
                    ),
            )
            .when(loading, |this| {
                this.child(div().absolute().bottom_0().left_0().right_0().h(px(2.)).bg(hsla(p.primary)))
            })
    }

    fn render_more_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.more_menu_open {
            return None;
        }
        let snapshot = self.snapshot(cx);
        let enabled = self.tab_id.is_some() && snapshot.is_some();
        let zoom_factor = snapshot.map_or(t3_core::preview::DEFAULT_ZOOM, SessionSnapshot::zoom);
        let zoom_label = format!("{}%", (zoom_factor * 100.).round() as u32);
        let color_scheme = snapshot.map_or(AppearancePreference::System, SessionSnapshot::color_scheme_preference);
        let device_toolbar_visible = snapshot.is_some_and(|s| !s.fills_panel());
        let defaults = self.previews.read(cx).browser_defaults(cx);
        let active_profile_id = snapshot.map_or(defaults.profile_id.as_str(), SessionSnapshot::profile_id_or_default);
        let active_profile_name = profile_name(&defaults.profiles, active_profile_id);
        let has_closed = self.previews.read(cx).has_closed_tab(&self.thread_id);
        let p = palette(cx);

        Some(deferred(
            div()
                .id("preview-more-menu-backdrop")
                .absolute()
                .inset_0()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.more_menu_open = false;
                    cx.notify();
                }))
                .child(
                    popup_surface(cx)
                        .id("preview-more-menu-popup")
                        .absolute()
                        .top(px(34.))
                        .right(px(8.))
                        .w(px(236.))
                        .p(px(4.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            MenuItem::new("preview-menu-hard-reload", "Hard reload")
                                .disabled(!enabled)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.more_menu_open = false;
                                    this.adjust(json!({ "hardReload": true }), cx);
                                    cx.notify();
                                })),
                        )
                        .child(
                            MenuItem::new(
                                "preview-menu-device-toolbar",
                                if device_toolbar_visible { "Hide device toolbar" } else { "Show device toolbar" },
                            )
                            .disabled(!enabled)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.more_menu_open = false;
                                this.toggle_device_toolbar(cx);
                                cx.notify();
                            })),
                        )
                        .when(has_closed, |this| {
                            this.child(MenuItem::new("preview-menu-reopen-closed", "Reopen closed tab").on_click(cx.listener(|this, _, _, cx| {
                                this.more_menu_open = false;
                                let thread = this.thread_id.clone();
                                this.previews.update(cx, |previews, cx| {
                                    previews.reopen_last_closed(&thread, cx);
                                });
                                cx.notify();
                            })))
                        })
                        .child(div().my(px(4.)).h(px(1.)).bg(hsla(p.border)))
                        .child(div().px(px(8.)).py(px(2.)).text_xs().text_color(hsla(p.muted_foreground)).child("Appearance"))
                        .children(AppearancePreference::ALL.into_iter().map(|pref| {
                            MenuItem::new(SharedString::from(format!("preview-menu-scheme-{}", pref.as_str())), pref.label())
                                .checked(color_scheme == pref)
                                .disabled(!enabled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.more_menu_open = false;
                                    this.adjust(json!({ "colorScheme": pref.as_str() }), cx);
                                    cx.notify();
                                }))
                        }))
                        .child(div().my(px(4.)).h(px(1.)).bg(hsla(p.border)))
                        .child(
                            h_flex()
                                .min_h(px(28.))
                                .px(px(8.))
                                .py(px(4.))
                                .justify_between()
                                .items_center()
                                .child(div().text_sm().text_color(hsla(p.foreground)).child("Zoom"))
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .child(
                                            ActionButton::new("preview-menu-zoom-out", ButtonVariant::Outline, ButtonSize::IconXs)
                                                .icon(Icon::Minus)
                                                .tooltip("Zoom out")
                                                .disabled(!enabled)
                                                .on_click(cx.listener(|this, _, _, cx| this.zoom(-1, cx))),
                                        )
                                        .child(div().min_w(px(40.)).text_center().text_xs().text_color(hsla(p.muted_foreground)).child(zoom_label))
                                        .child(
                                            ActionButton::new("preview-menu-zoom-in", ButtonVariant::Outline, ButtonSize::IconXs)
                                                .icon(Icon::Plus)
                                                .tooltip("Zoom in")
                                                .disabled(!enabled)
                                                .on_click(cx.listener(|this, _, _, cx| this.zoom(1, cx))),
                                        )
                                        .child(
                                            Button::new("preview-menu-zoom-reset")
                                                .variant(Variant::Ghost)
                                                .size(Size::IconXs)
                                                .icon(Icon::RotateCcw)
                                                .tooltip("Reset zoom")
                                                .disabled(!enabled)
                                                .on_click(cx.listener(|this, _, _, cx| this.zoom(0, cx))),
                                        ),
                                ),
                        )
                        .child(div().my(px(4.)).h(px(1.)).bg(hsla(p.border)))
                        .child(
                            div()
                                .px(px(8.))
                                .py(px(2.))
                                .truncate()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(format!("Profile: {active_profile_name}")),
                        )
                        .child(
                            MenuItem::new("preview-menu-clear-cookies", "Clear cookies")
                                .disabled(!enabled)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.more_menu_open = false;
                                    this.adjust(json!({ "clear": "cookies" }), cx);
                                    cx.notify();
                                })),
                        )
                        .child(
                            MenuItem::new("preview-menu-clear-cache", "Clear cache")
                                .disabled(!enabled)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.more_menu_open = false;
                                    this.adjust(json!({ "clear": "cache" }), cx);
                                    cx.notify();
                                })),
                        ),
                ),
        ))
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

    /// `PreviewFileNotShown` (`PreviewUnreachable.tsx`): shown when a navigation resolved to a
    /// downloaded file rather than an inline renderable page (`NavStatus::LoadFailed { download: Some(..) }`).
    fn render_file_not_shown(&self, url: &str, download: &FileDownload, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let host = host_label(url).unwrap_or_else(|| url.to_owned());
        let file_name = download.file_name.clone();
        let download_id = download.id.clone();
        let open_url = url.to_owned();
        let has_frame = self.frame.is_some();
        div()
            .id("preview-file-not-shown")
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
                    .child(div().mb(px(12.)).text_2xl().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child("This file can\u{2019}t be shown here"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(hsla(p.muted_foreground))
                            .child(gpui::StyledText::new(format!("{file_name} from {host} was downloaded instead.")).with_highlights([(
                                0..file_name.len(),
                                gpui::HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), color: Some(hsla(p.foreground)), ..Default::default() },
                            )])),
                    )
                    .child(
                        h_flex()
                            .mt_auto()
                            .pt(px(32.))
                            .gap(px(8.))
                            .child(div().flex_1())
                            .child(
                                ActionButton::new("preview-file-open-browser", ButtonVariant::Outline, ButtonSize::Sm)
                                    .label("Open in browser")
                                    .on_click(move |_, _, cx| cx.open_url(&open_url)),
                            )
                            .when(has_frame, |this| {
                                let download_id = download_id.clone();
                                let file_name = file_name.clone();
                                this.child(
                                    ActionButton::new("preview-file-download", ButtonVariant::Default, ButtonSize::Sm)
                                        .label("Download")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(frame) = &this.frame {
                                                let id = download_id.clone();
                                                let name = file_name.clone();
                                                frame.update(cx, |frame, cx| frame.save_download(id, name, cx));
                                            }
                                        })),
                                )
                            }),
                    ),
            )
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

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn frame_for_test(&self) -> Option<Entity<FrameView>> {
        self.frame.clone()
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn annotation_for_test(&self) -> Option<Entity<AnnotationEditor>> {
        self.annotation.clone()
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn open_annotation_for_test(&mut self, target: AnnotationTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.open_annotation_editor(target, window, cx);
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn toggle_device_toolbar_for_test(&mut self, cx: &mut Context<Self>) {
        self.toggle_device_toolbar(cx);
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
        let load_failed = match snapshot.as_ref().map(|snapshot| &snapshot.nav_status) {
            Some(NavStatus::LoadFailed { url, download: Some(download), .. }) if !blocked => {
                Some(self.render_file_not_shown(url, download, cx))
            }
            Some(NavStatus::LoadFailed { url, code, description, download: None, .. }) if !blocked => {
                Some(self.render_unreachable(url, *code, description, cx))
            }
            _ => None,
        };
        let show_device_toolbar = self.device_toolbar.is_some() && !empty && load_failed.is_none();
        // The stream's control bar stays above these, as the web keeps it visible, so control of an
        // agent's tab can still be taken from them.
        let top = if self.frame.is_some() { CONTROL_BAR_HEIGHT } else { 0. };
        let frame_top = if show_device_toolbar { BROWSER_DEVICE_TOOLBAR_HEIGHT } else { 0. };
        let body = div()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .when(show_device_toolbar, |this| {
                this.children(self.device_toolbar.clone().map(|toolbar| div().absolute().top_0().left_0().right_0().child(toolbar)))
            })
            .children(self.frame.clone().map(|frame| {
                div()
                    .absolute()
                    .top(px(frame_top))
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .child(frame.cached(StyleRefinement::default().size_full()))
            }))
            .when(empty, |this| {
                this.child(div().absolute().top(px(top)).bottom_0().left_0().right_0().bg(hsla(p.background)).child(self.render_empty(cx)))
            })
            .children(load_failed.map(|page| div().absolute().top(px(top)).bottom_0().left_0().right_0().child(page)))
            .when_some(self.zoom_indicator.clone(), |this, percent| {
                this.child(
                    div()
                        .id("preview-zoom-indicator")
                        .absolute()
                        .top(px(frame_top + 12.))
                        .right(px(12.))
                        .px(px(10.))
                        .py(px(4.))
                        .rounded(px(999.))
                        .border_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .bg(hsla(p.popover))
                        .shadow_sm()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground))
                        .child(percent),
                )
            })
            .when_some(self.annotation.clone(), |this, editor| {
                this.child(
                    div()
                        .absolute()
                        .top(px(frame_top + top + 12.))
                        .left(px(12.))
                        .right(px(12.))
                        .flex()
                        .justify_center()
                        .child(editor),
                )
            });

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
            .on_action(cx.listener(|this, _: &ReopenClosedTab, _, cx| {
                let thread = this.thread_id.clone();
                this.previews.update(cx, |previews, cx| {
                    previews.reopen_last_closed(&thread, cx);
                });
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                let mods = &event.keystroke.modifiers;
                if key == "escape" {
                    if this.more_menu_open {
                        cx.stop_propagation();
                        this.more_menu_open = false;
                        cx.notify();
                        return;
                    }
                    if let Some(toolbar) = &this.device_toolbar
                        && toolbar.update(cx, |toolbar, cx| toolbar.close_menus(cx))
                    {
                        cx.stop_propagation();
                        return;
                    }
                    if this.pick_active {
                        cx.stop_propagation();
                        this.cancel_annotation(window, cx);
                        return;
                    }
                }
                if key == "t" && mods.shift && (mods.platform || mods.control) && !mods.alt {
                    let thread = this.thread_id.clone();
                    if this.previews.update(cx, |previews, cx| previews.reopen_last_closed(&thread, cx)) {
                        cx.stop_propagation();
                    }
                }
            }))
            .child(self.render_chrome(cx))
            .child(body)
            .children(self.render_more_menu(cx))
            .into_any_element()
    }
}
