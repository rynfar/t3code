//! Native video playback (`MediaVideoPlayer.tsx`, `ChatMarkdownMedia.tsx`): hosts a sandboxed
//! `<video>` inside macOS `WKWebView` while keeping playback stable across asset URL re-signing
//! and falling back to a themed GPUI card in snapshot/headless builds or when playback fails.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, EntityId, FontWeight, Global, InteractiveElement, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Task,
    WeakEntity, Window, div, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::media::{MediaHints, MediaSource};

use super::{Media, MediaScope, VideoUrlState};
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::webview::{self, Layer, Page, WebEvent, WebView};

pub const MAX_INLINE_VIDEO_VIEWS: usize = 6;
const MAX_CACHED_INLINE_PLAYERS: usize = 64;
const HEADER_HEIGHT: f32 = 30.;
const VIDEO_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; media-src http: https: data: blob:; img-src data: blob: http: https:;";

static NEXT_CONSUMER: AtomicU64 = AtomicU64::new(1_000_000_000);

fn next_consumer() -> u64 {
    NEXT_CONSUMER.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatchUpdate {
    Keep,
    Reload,
}

/// Stabilizes the active `<video src>` against periodic signed-URL refreshes once playback starts,
/// while still reloading when the underlying workspace mutation revision changes or the user retries.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PlaybackLatch {
    pub revision: Option<String>,
    pub active_src: Option<String>,
    pub has_started: bool,
    pub error: Option<String>,
}

impl PlaybackLatch {
    pub fn reconcile(&mut self, incoming_src: Option<&str>, revision: Option<&str>) -> LatchUpdate {
        if self.revision.as_deref() != revision {
            self.revision = revision.map(str::to_owned);
            self.active_src = incoming_src.map(str::to_owned);
            self.has_started = false;
            self.error = None;
            return LatchUpdate::Reload;
        }
        if self.active_src.is_none() && incoming_src.is_some() {
            self.active_src = incoming_src.map(str::to_owned);
            self.error = None;
            return LatchUpdate::Reload;
        }
        if self.active_src.as_deref() != incoming_src && !self.has_started {
            self.active_src = incoming_src.map(str::to_owned);
            self.error = None;
            return LatchUpdate::Reload;
        }
        LatchUpdate::Keep
    }

    pub fn mark_started(&mut self) {
        self.has_started = true;
        self.error = None;
    }

    pub fn on_error(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
        self.has_started = false;
    }

    pub fn retry(&mut self, incoming_src: Option<&str>) {
        self.active_src = incoming_src.map(str::to_owned);
        self.has_started = false;
        self.error = None;
    }
}

/// Computes the rendered media box size (excluding header chrome) from metadata, authored HTML
/// dimensions, and surface constraints while preserving aspect ratio.
pub fn video_box_size(hints: MediaHints, natural: Option<(u32, u32)>, max_width: f32, max_height: f32) -> (f32, f32) {
    let max_width = max_width.max(120.);
    let max_height = max_height.max(90.);
    let (nat_w, nat_h) = match (natural, hints.width, hints.height) {
        (Some((w, h)), _, _) if w > 0 && h > 0 => (w as f32, h as f32),
        (_, Some(w), Some(h)) if w > 0 && h > 0 => (w as f32, h as f32),
        (_, Some(w), None) if w > 0 => (w as f32, (w as f32) * 9. / 16.),
        (_, None, Some(h)) if h > 0 => ((h as f32) * 16. / 9., h as f32),
        _ => (max_width, (max_width * 9. / 16.).min(max_height)),
    };
    let ratio = (nat_w / nat_h).clamp(0.25, 4.0);
    let mut cap_w = hints.width.map(|w| (w as f32).max(120.).min(max_width)).unwrap_or(max_width);
    let cap_h = hints.height.map(|h| (h as f32).max(90.).min(max_height)).unwrap_or(max_height);
    if natural.is_none() && hints.width.is_none() && hints.height.is_some() {
        cap_w = (cap_h * ratio).min(max_width);
    }
    let scale = (cap_w / nat_w).min(cap_h / nat_h).min(if natural.is_some() || hints.width.is_some() || hints.height.is_some() { 1.0 } else { f32::MAX });
    let w = (nat_w * scale).round().clamp(120., cap_w);
    let h = (w / ratio).round().clamp(90., cap_h);
    ((h * ratio).round().clamp(120., cap_w), h)
}

fn escape_html_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn video_html(src: &str, dark: bool) -> String {
    let bg = if dark { "#09090b" } else { "#f4f4f5" };
    let escaped = escape_html_attr(src);
    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
html, body {{
  margin: 0;
  padding: 0;
  width: 100%;
  height: 100%;
  overflow: hidden;
  background: {bg};
  display: flex;
  align-items: center;
  justify-content: center;
}}
video {{
  width: 100%;
  height: 100%;
  display: block;
  object-fit: contain;
  background: {bg};
  outline: none;
}}
</style>
</head>
<body>
<video id="player" controls playsinline preload="metadata" src="{escaped}"></video>
<script>
(function() {{
  var v = document.getElementById('player');
  if (!v) return;
  function send(payload) {{
    if (window.ipc && window.ipc.postMessage) {{
      window.ipc.postMessage(JSON.stringify(payload));
    }}
  }}
  v.addEventListener('loadedmetadata', function() {{
    if (v.videoWidth > 0 && v.videoHeight > 0) {{
      send({{ type: 'meta', width: v.videoWidth, height: v.videoHeight }});
    }}
  }});
  v.addEventListener('play', function() {{ send({{ type: 'play' }}); }});
  v.addEventListener('pause', function() {{ send({{ type: 'pause' }}); }});
  v.addEventListener('ended', function() {{ send({{ type: 'ended' }}); }});
  v.addEventListener('error', function() {{
    var code = v.error && v.error.code ? v.error.code : 0;
    send({{ type: 'error', message: code ? ('Playback error (code ' + code + ').') : 'Unable to play video.' }});
  }});
}})();
</script>
</body>
</html>"#
    )
}

pub type VideoAction = Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>;
type MountedWebview = (String, Rc<WebView>, Task<()>, Option<Task<()>>);

#[derive(Clone)]
pub struct VideoPlayerOptions {
    pub source: MediaSource,
    pub name: SharedString,
    pub revision: Option<String>,
    pub hints: MediaHints,
    pub modal: bool,
    pub max_width: f32,
    pub max_height: f32,
    pub on_expand: Option<VideoAction>,
    pub on_open_external: Option<VideoAction>,
    pub on_close_modal: Option<VideoAction>,
}

fn is_http_url(url: &str) -> bool {
    let trimmed = url.trim();
    (trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("http://"))
        || (trimmed.len() >= 8 && trimmed[..8].eq_ignore_ascii_case("https://"))
}

fn allows_external_open(source: &MediaSource, on_open_external: Option<&VideoAction>) -> bool {
    if on_open_external.is_some() {
        return true;
    }
    match source {
        MediaSource::Direct(url) => is_http_url(url),
        MediaSource::Asset(_) => true,
    }
}

fn scope_matches(a: Option<&MediaScope>, b: Option<&MediaScope>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.owner == b.owner
                && a.thread_id == b.thread_id
                && a.workspace_root == b.workspace_root
                && a.workspace_mutation_id == b.workspace_mutation_id
                && a.source_control_media == b.source_control_media
                && a.source_control_media_repository == b.source_control_media_repository
        }
        _ => false,
    }
}

pub struct VideoPlayer {
    options: VideoPlayerOptions,
    scope: Option<MediaScope>,
    consumer: u64,
    latch: PlaybackLatch,
    natural_size: Option<(u32, u32)>,
    playing: bool,
    leased: bool,
    webview: Option<MountedWebview>,
    mount_count: Arc<AtomicUsize>,
    unmount_tx: mpsc::UnboundedSender<()>,
    _unmount_task: Task<()>,
}

impl VideoPlayer {
    pub fn new(options: VideoPlayerOptions, scope: Option<MediaScope>, cx: &mut Context<Self>) -> Self {
        let leased = options.modal;
        let mount_count = Arc::new(AtomicUsize::new(0));
        let (unmount_tx, mut unmount_rx) = mpsc::unbounded();
        let _unmount_task = cx.spawn(async move |this, cx| {
            while unmount_rx.next().await.is_some() {
                let _ = this.update(cx, |player, cx| {
                    if player.mount_count.load(Ordering::Acquire) == 0 && (player.webview.is_some() || player.leased || player.playing) {
                        player.release_webview(cx);
                    }
                });
            }
        });
        Self {
            options,
            scope,
            consumer: next_consumer(),
            latch: PlaybackLatch::default(),
            natural_size: None,
            playing: false,
            leased,
            webview: None,
            mount_count,
            unmount_tx,
            _unmount_task,
        }
    }

    #[cfg(test)]
    pub(super) fn scope_for_test(&self) -> Option<&MediaScope> {
        self.scope.as_ref()
    }

    pub fn update_options(&mut self, options: VideoPlayerOptions, scope: Option<MediaScope>, cx: &mut Context<Self>) {
        let source_changed = self.options.source != options.source;
        let display_changed = self.options.name != options.name
            || self.options.revision != options.revision
            || self.options.hints != options.hints
            || self.options.modal != options.modal
            || (self.options.max_width - options.max_width).abs() > f32::EPSILON
            || (self.options.max_height - options.max_height).abs() > f32::EPSILON
            || self.options.on_expand.is_some() != options.on_expand.is_some()
            || self.options.on_open_external.is_some() != options.on_open_external.is_some()
            || self.options.on_close_modal.is_some() != options.on_close_modal.is_some()
            || !scope_matches(self.scope.as_ref(), scope.as_ref());
        self.options = options;
        self.scope = scope;
        if source_changed {
            self.latch = PlaybackLatch::default();
            self.natural_size = None;
            self.playing = false;
            self.drop_webview();
            cx.notify();
        } else if display_changed {
            cx.notify();
        }
    }

    pub fn pause(&mut self) {
        if let Some((_, view, _, _)) = &self.webview {
            view.evaluate("var v=document.getElementById('player');if(v&&!v.paused)v.pause();");
        }
        self.playing = false;
    }

    pub fn release_webview(&mut self, cx: &mut Context<Self>) {
        self.pause();
        self.drop_webview();
        self.leased = false;
        let id = cx.entity_id();
        cx.default_global::<VideoLeasePool>().release(id);
        cx.notify();
    }

    fn drop_webview(&mut self) {
        if let Some((_, view, _, _)) = self.webview.take() {
            view.evaluate("var v=document.getElementById('player');if(v){v.pause();v.removeAttribute('src');}");
        }
    }

    fn ensure_lease(&mut self, cx: &mut Context<Self>) -> bool {
        if self.options.modal {
            self.leased = true;
            return true;
        }
        let id = cx.entity_id();
        let entity = cx.entity().downgrade();
        let candidates = cx.default_global::<VideoLeasePool>().leases.clone();
        let stale: Vec<Entity<VideoPlayer>> = candidates
            .into_iter()
            .filter_map(|(existing, weak)| {
                if existing == id {
                    return None;
                }
                let player = weak.upgrade()?;
                let guard = player.read(cx);
                let unmounted = guard.mount_count.load(Ordering::Acquire) == 0;
                let hidden = guard.webview.as_ref().is_some_and(|(_, view, _, _)| !view.is_shown());
                (unmounted || hidden).then_some(player)
            })
            .collect();
        for victim in stale {
            victim.update(cx, |player, cx| player.release_webview(cx));
        }
        let pool = cx.default_global::<VideoLeasePool>();
        let (granted, evicted) = pool.claim(id, entity);
        self.leased = granted;
        for victim in evicted {
            if let Some(victim) = victim.upgrade()
                && victim.entity_id() != id
            {
                victim.update(cx, |player, cx| player.release_webview(cx));
            }
        }
        granted
    }

    fn activate_lease(&mut self, cx: &mut Context<Self>) {
        let id = cx.entity_id();
        let entity = cx.entity().downgrade();
        let evicted = cx.default_global::<VideoLeasePool>().promote(id, entity);
        self.leased = true;
        for victim in evicted {
            if let Some(victim) = victim.upgrade()
                && victim.entity_id() != id
            {
                victim.update(cx, |player, cx| player.release_webview(cx));
            }
        }
        cx.notify();
    }

    fn resolve_url(&mut self, window: &mut Window, cx: &mut Context<Self>) -> VideoUrlState {
        let revision = self
            .options
            .revision
            .clone()
            .or_else(|| self.scope.as_ref().and_then(|scope| super::revision_for(&self.options.source, scope).map(str::to_owned)));
        let Some(cache) = cx.try_global::<Media>().map(|media| media.cache.clone()) else {
            return match &self.options.source {
                MediaSource::Direct(url) => VideoUrlState::Ready(url.clone()),
                MediaSource::Asset(resource) => match resource.fallback_url() {
                    Some(url) => VideoUrlState::Ready(url.to_owned()),
                    None => VideoUrlState::Loading,
                },
            };
        };
        let owner = cx.entity_id();
        let scope = self.scope.clone();
        let source = self.options.source.clone();
        let consumer = self.consumer;
        let state = cache.update(cx, |cache, cx| cache.visible_video(&source, scope.as_ref(), owner, consumer, window, cx));
        let incoming = match &state {
            VideoUrlState::Ready(url) => Some(url.as_str()),
            _ => None,
        };
        if self.latch.reconcile(incoming, revision.as_deref()) == LatchUpdate::Reload {
            self.drop_webview();
        }
        state
    }

    fn ensure_webview(&mut self, src: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<Rc<WebView>> {
        if !webview::available(window) {
            return None;
        }
        let dark = cx.theme().is_dark();
        if let Some((active_src, view, _, _)) = &self.webview
            && active_src == src
        {
            view.set_dark(dark);
            if !self.options.modal && !view.is_shown() && self.playing {
                view.evaluate("var v=document.getElementById('player');if(v&&!v.paused)v.pause();");
                self.playing = false;
            }
            return Some(view.clone());
        }
        self.drop_webview();
        let (tx, mut rx) = mpsc::unbounded();
        let page = Page {
            html: Arc::from(video_html(src, dark)),
            dark,
            allow_popups: false,
            scroll_through: !self.options.modal,
            script: String::new(),
            content_security_policy: Some(VIDEO_CSP.to_owned()),
        };
        let layer = if self.options.modal { Layer::Modal } else { Layer::Transcript };
        let view = WebView::new(page, layer, tx, window, cx)?;
        if self.options.modal {
            view.set_dismiss_on_escape(true);
        }
        let handle = window.window_handle();
        let events = cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                if let WebEvent::Message(raw) = event
                    && let Ok(msg) = serde_json::from_str::<serde_json::Value>(&raw)
                {
                    let _ = this.update(cx, |player, cx| player.on_ipc(&msg, cx));
                }
            }
        });
        let escape = self.options.modal.then(|| {
            let mut escape_rx = view.watch_escape();
            cx.spawn(async move |this, cx| {
                while escape_rx.next().await.is_some() {
                    let close = this.read_with(cx, |player, _| player.options.on_close_modal.clone()).ok().flatten();
                    if let Some(close) = close {
                        let _ = handle.update(cx, |_, window, cx| close(window, cx));
                    }
                }
            })
        });
        self.webview = Some((src.to_owned(), view.clone(), events, escape));
        Some(view)
    }

    fn on_ipc(&mut self, msg: &serde_json::Value, cx: &mut Context<Self>) {
        match msg.get("type").and_then(|v| v.as_str()) {
            Some("meta") => {
                let width = msg.get("width").and_then(|v| v.as_u64()).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
                let height = msg.get("height").and_then(|v| v.as_u64()).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
                if width > 0 && height > 0 && self.natural_size != Some((width, height)) {
                    self.natural_size = Some((width, height));
                    cx.notify();
                }
            }
            Some("play") => {
                self.playing = true;
                self.latch.mark_started();
            }
            Some("pause") | Some("ended") => {
                self.playing = false;
            }
            Some("error") => {
                self.playing = false;
                let message = msg.get("message").and_then(|v| v.as_str()).unwrap_or("Unable to play video.");
                self.latch.on_error(message);
                self.drop_webview();
                cx.notify();
            }
            _ => {}
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        let revision = self
            .options
            .revision
            .clone()
            .or_else(|| self.scope.as_ref().and_then(|scope| super::revision_for(&self.options.source, scope).map(str::to_owned)));
        self.latch.retry(None);
        self.drop_webview();
        if let Some(cache) = cx.try_global::<Media>().map(|media| media.cache.clone()) {
            let source = self.options.source.clone();
            let owner = cx.entity_id();
            cache.update(cx, |cache, cx| cache.retry_video(&source, revision.as_deref(), owner, cx));
        }
        cx.notify();
    }

    fn can_open_external(&self) -> bool {
        allows_external_open(&self.options.source, self.options.on_open_external.as_ref())
    }

    fn open_external(&self, window: &mut Window, cx: &mut App) {
        if let Some(callback) = &self.options.on_open_external {
            callback(window, cx);
            return;
        }
        match &self.options.source {
            MediaSource::Direct(url) if is_http_url(url) => cx.open_url(url.trim()),
            MediaSource::Direct(_) => {}
            MediaSource::Asset(resource) => super::open_asset(resource, cx),
        }
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.drop_webview();
    }
}

impl Render for VideoPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let url_state = self.resolve_url(window, cx);
        let can_webview = webview::available(window);
        let leased = if can_webview && self.latch.error.is_none() { self.ensure_lease(cx) } else { false };
        let active_src = self.latch.active_src.clone();
        let web_view = if can_webview && leased && self.latch.error.is_none() {
            active_src.as_deref().and_then(|src| self.ensure_webview(src, window, cx))
        } else {
            self.drop_webview();
            None
        };

        let (box_w, box_h) = video_box_size(self.options.hints, self.natural_size, self.options.max_width, self.options.max_height);
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let modal = self.options.modal;
        let name = self.options.name.clone();
        let expand = self.options.on_expand.clone();
        let can_open = self.can_open_external();
        let mount_id: SharedString = format!("video-mount-{}", cx.entity_id().as_u64()).into();
        let tracker = mount_tracker(mount_id, self.mount_count.clone(), self.unmount_tx.clone());
        let open_btn_id: SharedString = format!("video-open-{}", cx.entity_id().as_u64()).into();
        let expand_btn_id: SharedString = format!("video-expand-{}", cx.entity_id().as_u64()).into();
        let retry_btn_id: SharedString = format!("video-retry-{}", cx.entity_id().as_u64()).into();
        let play_btn_id: SharedString = format!("video-play-{}", cx.entity_id().as_u64()).into();
        let web_el_id: SharedString = format!("video-web-{}", cx.entity_id().as_u64()).into();

        let body: AnyElement = if let Some(view) = web_view {
            div().size_full().child(webview::web_view(web_el_id, view, dark)).into_any_element()
        } else if let Some(err) = self.latch.error.clone().or_else(|| match &url_state {
            VideoUrlState::Unavailable(msg) => Some(msg.to_string()),
            _ => None,
        }) {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .p(px(16.))
                .child(Icon::TriangleAlert.render(px(18.), hsla(p.secondary_label)))
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Video unavailable"))
                .child(div().max_w(px((box_w - 32.).max(120.))).text_xs().text_color(hsla(p.secondary_label)).truncate().child(err))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(
                            h_flex()
                                .id(retry_btn_id)
                                .cursor_pointer()
                                .gap(px(4.))
                                .px(px(10.))
                                .py(px(5.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(hsla(p.border))
                                .bg(hsla(p.secondary))
                                .text_xs()
                                .text_color(hsla(p.foreground))
                                .child(Icon::RefreshCw.render(px(12.), hsla(p.foreground)))
                                .child("Retry")
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                        )
                        .when(can_open, |row| {
                            row.child(
                                h_flex()
                                    .id(SharedString::from(format!("video-err-open-{}", cx.entity_id().as_u64())))
                                    .cursor_pointer()
                                    .gap(px(4.))
                                    .px(px(10.))
                                    .py(px(5.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(hsla(p.border))
                                    .bg(hsla(p.secondary))
                                    .text_xs()
                                    .text_color(hsla(p.foreground))
                                    .child(Icon::ExternalLink.render(px(12.), hsla(p.foreground)))
                                    .child("Open")
                                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(|this, _, window, cx| this.open_external(window, cx))),
                            )
                        }),
                )
                .into_any_element()
        } else if matches!(url_state, VideoUrlState::Loading) && active_src.is_none() {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .child(Icon::Play.render(px(18.), hsla(p.secondary_label)))
                .child(div().text_xs().text_color(hsla(p.secondary_label)).child("Loading video…"))
                .into_any_element()
        } else {
            let needs_lease = can_webview && !leased;
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .p(px(16.))
                .child(
                    h_flex()
                        .id(play_btn_id)
                        .when(needs_lease, |el| el.cursor_pointer())
                        .gap(px(6.))
                        .px(px(12.))
                        .py(px(6.))
                        .rounded(px(999.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(p.secondary))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground))
                        .child(Icon::Play.render(px(13.), hsla(p.foreground)))
                        .child(if needs_lease { "Play video" } else { "Video preview" })
                        .when(needs_lease, |el| {
                            el.on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(|this, _, _, cx| this.activate_lease(cx)))
                        }),
                )
                .child(div().max_w(px((box_w - 32.).max(120.))).text_xs().text_color(hsla(p.secondary_label)).truncate().child(name.clone()))
                .into_any_element()
        };

        let header = if modal {
            None
        } else {
            Some(
                h_flex()
                    .h(px(HEADER_HEIGHT))
                    .flex_none()
                    .px(px(10.))
                    .gap(px(8.))
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.secondary))
                    .child(div().min_w_0().flex_1().truncate().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(name))
                    .child(
                        h_flex()
                            .flex_none()
                            .gap(px(4.))
                            .items_center()
                            .when_some(expand, |bar, expand| {
                                bar.child(
                                    div()
                                        .id(expand_btn_id)
                                        .cursor_pointer()
                                        .p(px(4.))
                                        .rounded(px(4.))
                                        .hover(|s| s.bg(hsla(p.accent)))
                                        .child(Icon::Maximize2.render(px(13.), hsla(p.secondary_label)))
                                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .on_click(move |_, window, cx| expand(window, cx)),
                                )
                            })
                            .when(can_open, |bar| {
                                bar.child(
                                    div()
                                        .id(open_btn_id)
                                        .cursor_pointer()
                                        .p(px(4.))
                                        .rounded(px(4.))
                                        .hover(|s| s.bg(hsla(p.accent)))
                                        .child(Icon::ExternalLink.render(px(13.), hsla(p.secondary_label)))
                                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .on_click(cx.listener(|this, _, window, cx| this.open_external(window, cx))),
                                )
                            }),
                    ),
            )
        };

        v_flex()
            .w(px(box_w))
            .overflow_hidden()
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.card))
            .children(header)
            .child(div().w(px(box_w)).h(px(box_h)).bg(hsla(p.card)).child(body))
            .child(tracker)
    }
}

pub(super) fn mount_tracker(id: SharedString, count: Arc<AtomicUsize>, tx: mpsc::UnboundedSender<()>) -> impl IntoElement {
    MountTracker { id, count, tx }
}

struct MountGuard {
    count: Arc<AtomicUsize>,
    tx: mpsc::UnboundedSender<()>,
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        if self.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.tx.unbounded_send(());
        }
    }
}

struct MountTracker {
    id: SharedString,
    count: Arc<AtomicUsize>,
    tx: mpsc::UnboundedSender<()>,
}

impl IntoElement for MountTracker {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl gpui::Element for MountTracker {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        Some(self.id.clone().into())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (gpui::LayoutId, ()) {
        let style = gpui::Style { size: gpui::size(px(0.).into(), px(0.).into()), ..Default::default() };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, id: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: gpui::Bounds<gpui::Pixels>, _: &mut (), window: &mut Window, _: &mut App) {
        let Some(id) = id else { return };
        window.with_element_state::<MountGuard, _>(id, |guard, _| match guard {
            Some(guard) => ((), guard),
            None => {
                self.count.fetch_add(1, Ordering::AcqRel);
                ((), MountGuard { count: self.count.clone(), tx: self.tx.clone() })
            }
        });
    }

    fn paint(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: gpui::Bounds<gpui::Pixels>, _: &mut (), _: &mut (), _: &mut Window, _: &mut App) {}
}

#[derive(Default)]
struct VideoLeasePool {
    leases: Vec<(EntityId, WeakEntity<VideoPlayer>)>,
}

impl Global for VideoLeasePool {}

impl VideoLeasePool {
    fn release(&mut self, id: EntityId) {
        self.leases.retain(|(existing, w)| *existing != id && w.upgrade().is_some());
    }

    fn claim(&mut self, id: EntityId, entity: WeakEntity<VideoPlayer>) -> (bool, Vec<WeakEntity<VideoPlayer>>) {
        self.leases.retain(|(_, w)| w.upgrade().is_some());
        if self.leases.iter().any(|(existing, _)| *existing == id) {
            return (true, Vec::new());
        }
        if self.leases.len() < MAX_INLINE_VIDEO_VIEWS {
            self.leases.push((id, entity));
            return (true, Vec::new());
        }
        (false, Vec::new())
    }

    fn promote(&mut self, id: EntityId, entity: WeakEntity<VideoPlayer>) -> Vec<WeakEntity<VideoPlayer>> {
        self.leases.retain(|(existing, w)| *existing != id && w.upgrade().is_some());
        let mut evicted = Vec::new();
        while self.leases.len() >= MAX_INLINE_VIDEO_VIEWS {
            let (_, victim) = self.leases.remove(0);
            evicted.push(victim);
        }
        self.leases.push((id, entity));
        evicted
    }
}

#[derive(Default)]
struct InlinePlayerRegistry {
    frame: u64,
    players: HashMap<String, (u64, Entity<VideoPlayer>)>,
}

impl Global for InlinePlayerRegistry {}

/// Releases WebViews and active leases for every inline video player owned by `owner`
/// (for example when switching threads or files).
pub fn retire_owner(owner: EntityId, cx: &mut App) {
    let mut victims: Vec<Entity<VideoPlayer>> = Vec::new();
    if let Some(registry) = cx.try_global::<InlinePlayerRegistry>() {
        for (_, entity) in registry.players.values() {
            if entity.read(cx).scope.as_ref().is_some_and(|scope| scope.owner == owner) {
                victims.push(entity.clone());
            }
        }
    }
    if let Some(pool) = cx.try_global::<VideoLeasePool>() {
        for (_, weak) in &pool.leases {
            if let Some(entity) = weak.upgrade()
                && entity.read(cx).scope.as_ref().is_some_and(|scope| scope.owner == owner)
                && !victims.iter().any(|existing| existing.entity_id() == entity.entity_id())
            {
                victims.push(entity);
            }
        }
    }
    for victim in victims {
        victim.update(cx, |player, cx| player.release_webview(cx));
    }
}

/// Gets or updates a persistent `VideoPlayer` entity for an inline transcript/user-attachment video.
pub fn inline_video_player(key: String, options: VideoPlayerOptions, scope: Option<&MediaScope>, cx: &mut App) -> AnyElement {
    let scope_owned = scope.cloned();
    let (entity, evicted) = {
        let registry = cx.default_global::<InlinePlayerRegistry>();
        registry.frame += 1;
        let frame = registry.frame;
        let mut evicted = Vec::new();
        if registry.players.len() > MAX_CACHED_INLINE_PLAYERS {
            let stale: Vec<_> = registry
                .players
                .iter()
                .filter(|(k, (last, _))| *k != &key && frame.saturating_sub(*last) > MAX_CACHED_INLINE_PLAYERS as u64)
                .map(|(k, _)| k.clone())
                .collect();
            for k in stale {
                if let Some((_, entity)) = registry.players.remove(&k) {
                    evicted.push(entity);
                }
            }
        }
        (registry.players.get(&key).map(|(_, entity)| entity.clone()), evicted)
    };
    for victim in evicted {
        victim.update(cx, |player, cx| player.release_webview(cx));
    }
    let entity = match entity {
        Some(entity) => {
            entity.update(cx, |player, cx| player.update_options(options, scope_owned, cx));
            let frame = cx.global::<InlinePlayerRegistry>().frame;
            if let Some(entry) = cx.global_mut::<InlinePlayerRegistry>().players.get_mut(&key) {
                entry.0 = frame;
            }
            entity
        }
        None => {
            let entity = cx.new(|cx| VideoPlayer::new(options, scope_owned, cx));
            let registry = cx.default_global::<InlinePlayerRegistry>();
            let frame = registry.frame;
            registry.players.insert(key, (frame, entity.clone()));
            entity
        }
    };
    entity.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_latch_holds_src_during_resign_until_revision_or_retry() {
        let mut latch = PlaybackLatch::default();
        assert_eq!(latch.reconcile(Some("https://host/a.mp4?sig=1"), Some("rev-1")), LatchUpdate::Reload);
        assert_eq!(latch.active_src.as_deref(), Some("https://host/a.mp4?sig=1"));

        // Before playback starts, a refreshed signature updates cleanly.
        assert_eq!(latch.reconcile(Some("https://host/a.mp4?sig=2"), Some("rev-1")), LatchUpdate::Reload);
        assert_eq!(latch.active_src.as_deref(), Some("https://host/a.mp4?sig=2"));

        // Once playback starts, a re-sign with the same revision stays latched.
        latch.mark_started();
        assert_eq!(latch.reconcile(Some("https://host/a.mp4?sig=3"), Some("rev-1")), LatchUpdate::Keep);
        assert_eq!(latch.active_src.as_deref(), Some("https://host/a.mp4?sig=2"));

        // A new revision (e.g. workspace file changed) invalidates the latch and reloads.
        assert_eq!(latch.reconcile(Some("https://host/a.mp4?sig=4"), Some("rev-2")), LatchUpdate::Reload);
        assert!(!latch.has_started);
        assert_eq!(latch.active_src.as_deref(), Some("https://host/a.mp4?sig=4"));

        // Playback error records failure and clears has_started; Retry resets cleanly.
        latch.mark_started();
        latch.on_error("Playback error (code 4).");
        assert!(!latch.has_started);
        assert_eq!(latch.error.as_deref(), Some("Playback error (code 4)."));
        latch.retry(Some("https://host/a.mp4?sig=5"));
        assert!(latch.error.is_none());
        assert_eq!(latch.active_src.as_deref(), Some("https://host/a.mp4?sig=5"));
    }

    #[test]
    fn video_box_size_respects_authored_hints_and_bounds() {
        // Explicit HTML 320x180 fits within 560x360 without upscaling.
        let (w, h) = video_box_size(MediaHints { width: Some(320), height: Some(180), explicit_video: true }, None, 560., 360.);
        assert_eq!((w, h), (320., 180.));

        // Huge authored 1280x720 clamps proportionally to 560x315.
        let (w, h) = video_box_size(MediaHints { width: Some(1280), height: Some(720), explicit_video: true }, None, 560., 360.);
        assert_eq!((w, h), (560., 315.));

        // Metadata overrides default 16:9 for portrait videos while obeying height cap.
        let (w, h) = video_box_size(MediaHints::default(), Some((1080, 1920)), 560., 360.);
        assert!(w < h && h <= 360., "expected portrait box <=360px tall, got {w}x{h}");
    }

    #[test]
    fn video_html_escapes_src_and_keeps_csp_controls() {
        let html = video_html("https://example.com/a.mp4?token=\"1&2\"", true);
        assert!(html.contains("<video id=\"player\" controls playsinline preload=\"metadata\" src=\"https://example.com/a.mp4?token=&quot;1&amp;2&quot;\"></video>"));
        assert!(!html.contains("token=\"1"));
    }

    #[test]
    fn external_open_allows_http_and_assets_only() {
        assert!(allows_external_open(&MediaSource::Direct("https://example.com/a.mp4".into()), None));
        assert!(allows_external_open(&MediaSource::Direct("HTTP://example.com/a.mp4".into()), None));
        assert!(!allows_external_open(&MediaSource::Direct("data:video/mp4;base64,AAAA".into()), None));
        assert!(!allows_external_open(&MediaSource::Direct("blob:https://example.com/1".into()), None));
        assert!(!allows_external_open(&MediaSource::Direct("javascript:alert(1)".into()), None));
        assert!(allows_external_open(
            &MediaSource::Asset(t3_core::media::AssetResource::MediaFile { thread_id: "t1".into(), path: "a.mp4".into() }),
            None,
        ));
    }

    #[gpui::test]
    fn unmount_and_owner_retire_release_lease_and_update_options_notifies(cx: &mut gpui::TestAppContext) {
        let options = |name: &str, hints: MediaHints| VideoPlayerOptions {
            source: MediaSource::Direct("https://example.com/demo.mp4".into()),
            name: name.to_owned().into(),
            revision: Some("r1".into()),
            hints,
            modal: false,
            max_width: 560.,
            max_height: 360.,
            on_expand: None,
            on_open_external: None,
            on_close_modal: None,
        };
        let (owner_view, cx) = cx.add_window_view(|_, _| gpui::EmptyView);
        let owner_id = owner_view.entity_id();
        let viewer = cx.update(|_, cx| cx.new(crate::media::MediaViewer::new));
        let scope = MediaScope {
            owner: owner_id,
            thread_id: Some("t1".into()),
            workspace_root: None,
            workspace_mutation_id: None,
            source_control_media: false,
            source_control_media_repository: None,
            viewer: viewer.downgrade(),
        };

        let mut player_entity: Option<Entity<VideoPlayer>> = None;
        cx.update(|_, cx| {
            let _ = inline_video_player("k1".into(), options("first.mp4", MediaHints::default()), Some(&scope), cx);
            player_entity = cx.global::<InlinePlayerRegistry>().players.get("k1").map(|(_, e)| e.clone());
        });
        let player = player_entity.expect("registered player");

        let notifications = Arc::new(AtomicUsize::new(0));
        let n = notifications.clone();
        cx.update(|_, cx| {
            cx.observe(&player, move |_, _| { n.fetch_add(1, Ordering::Relaxed); }).detach();
            // Same source, changed name/hints must notify.
            let _ = inline_video_player("k1".into(), options("renamed.mp4", MediaHints { width: Some(320), height: Some(180), explicit_video: true }), Some(&scope), cx);
        });
        assert!(notifications.load(Ordering::Relaxed) >= 1, "update_options should notify on display changes");

        // Simulating an active lease + playing flag clears cleanly on retire_owner.
        cx.update(|_, cx| {
            player.update(cx, |p, cx| {
                p.leased = true;
                p.playing = true;
                let id = cx.entity_id();
                let weak = cx.entity().downgrade();
                cx.default_global::<VideoLeasePool>().leases.push((id, weak));
            });
            retire_owner(owner_id, cx);
        });
        cx.read(|cx| {
            let p = player.read(cx);
            assert!(!p.leased);
            assert!(!p.playing);
            assert!(cx.global::<VideoLeasePool>().leases.is_empty());
        });
    }
}
