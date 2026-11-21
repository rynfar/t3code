//! Floating browser previews and the read-only popout. Ports the browser player in
//! `ThreadPreviewMiniPlayer.tsx` and the stream consumer in `serverPictureInPicture.ts`.

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Context, DragMoveEvent, EmptyView, Entity, EntityId, IntoElement,
    ParentElement, Pixels, Point, Render, Styled, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowOptions,
    canvas, div, point, prelude::*, px, size,
};
use gpui_component::{h_flex, v_flex};
use t3_client::preview_stream::{PreviewStream, StreamEvent, StreamSender, StreamTarget};
use t3_client::supervisor::ClientHandle;
use t3_core::preview::ThreadPreviews;

use super::frame::{FrameImages, FrameView, decode_frames};
use super::{Abort, Availability, Previews, PreviewsEvent, previews};
use crate::app_state::AppState;
use crate::i18n::t;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

pub(super) struct FloatingFrame {
    pub(super) thread: String,
    pub(super) tab: String,
    frame: Entity<FrameView>,
    pending_popup: Option<String>,
}

impl Previews {
    pub(super) fn is_floating(&self, thread: &str, tab: &str) -> bool {
        self.floating_tabs.get(thread).is_some_and(|selected| selected == tab)
    }

    pub(super) fn float_from_panel(&mut self, thread: &str, tab: &str, frame: Entity<FrameView>, cx: &mut Context<Self>) {
        if AppState::is_utility_panel_key(thread) || self.session(thread, tab).is_none() {
            return;
        }
        self.select_float(thread, tab, cx);
        if self.floating_frame.as_ref().is_none_or(|slot| slot.frame != frame) {
            self.stop_floating_frame(cx);
            frame.update(cx, |frame, _| frame.relocate(true, false));
            self.floating_frame = Some(FloatingFrame { thread: thread.to_owned(), tab: tab.to_owned(), frame, pending_popup: None });
        }
        self.state.update(cx, |state, cx| {
            state.mutate_panels(cx, |panels, selected| {
                if selected != thread {
                    return false;
                }
                let unmaximized = panels.panels.set_maximized(thread, false);
                panels.panels.hide(thread) || unmaximized
            });
        });
    }

    fn select_float(&mut self, thread: &str, tab: &str, cx: &mut Context<Self>) {
        if !self.is_floating(thread, tab) {
            if self.floating_frame.as_ref().is_some_and(|slot| slot.thread == thread && slot.tab != tab) {
                self.stop_floating_frame(cx);
            }
            self.floating_tabs.insert(thread.to_owned(), tab.to_owned());
            cx.emit(PreviewsEvent::Thread(thread.to_owned()));
            cx.notify();
        }
    }

    pub(super) fn dismiss_float(&mut self, thread: &str, tab: &str, cx: &mut Context<Self>) {
        if self.is_floating(thread, tab) {
            self.floating_tabs.remove(thread);
            if self.floating_frame.as_ref().is_some_and(|slot| slot.thread == thread && slot.tab == tab) {
                self.stop_floating_frame(cx);
            }
            cx.emit(PreviewsEvent::Thread(thread.to_owned()));
            cx.notify();
        }
    }

    pub(super) fn stop_floating_frame(&mut self, cx: &mut Context<Self>) {
        if let Some(slot) = self.floating_frame.take() {
            slot.frame.update(cx, |frame, cx| frame.set_visible(false, cx));
        }
    }

    pub(super) fn restore_float(&mut self, thread: &str, tab: &str, cx: &mut Context<Self>) {
        if !self.is_floating(thread, tab) || self.session(thread, tab).is_none() {
            return;
        }
        self.floating_tabs.remove(thread);
        self.state.update(cx, |state, cx| {
            state.mutate_panels(cx, |panels, selected| selected == thread && panels.panels.open_browser(thread, Some(tab)));
        });
        cx.emit(PreviewsEvent::Thread(thread.to_owned()));
        cx.notify();
    }

    pub(super) fn take_docked_frame(&mut self, thread: &str, tab: &str) -> Option<Entity<FrameView>> {
        if self.is_floating(thread, tab) {
            return None;
        }
        self.floating_frame.take_if(|slot| slot.thread == thread && slot.tab == tab).map(|slot| slot.frame)
    }

    pub(super) fn popup_from_float(&mut self, thread: &str, tab: &str, popup: String, cx: &mut Context<Self>) {
        if !self.is_floating(thread, tab) {
            return;
        }
        if self.session(thread, &popup).is_some() {
            self.select_float(thread, &popup, cx);
        } else if let Some(slot) = self.floating_frame.as_mut().filter(|slot| slot.thread == thread && slot.tab == tab) {
            slot.pending_popup = Some(popup);
        }
    }

    pub(super) fn follow_floating_popup(&mut self, thread: &str, cx: &mut Context<Self>) {
        if let Some(popup) = self.floating_frame.as_ref()
            .filter(|slot| slot.thread == thread && self.is_floating(thread, &slot.tab))
            .and_then(|slot| slot.pending_popup.as_ref())
            .filter(|popup| self.session(thread, popup).is_some())
            .cloned()
        {
            self.select_float(thread, &popup, cx);
        }
    }

    pub(super) fn reveal_floating(&mut self, thread: &str, before: &ThreadPreviews, cx: &mut Context<Self>) -> Vec<String> {
        if self.server_browser != Some(true) || AppState::is_utility_panel_key(thread) {
            return Vec::new();
        }
        let auto = crate::settings::store::try_client(cx)
            .and_then(|doc| doc.entries().get("browserAutoShowFloatingPreview"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let Some(next) = self.threads.thread(thread) else { return Vec::new() };
        let reveals: Vec<_> = next.freshly_revealed(before).into_iter().map(|session| {
            (session.tab_id.clone(), session.reveal_request.as_ref().is_some_and(|request| request.force))
        }).collect();
        let mut floated = Vec::new();
        for (tab, force) in reveals {
            if !auto && !force {
                continue;
            }
            let state = self.state.read(cx);
            let selected = state.panel_thread().as_deref() == Some(thread);
            let exists = state.panels.panels.panel(thread).surfaces.iter().any(|surface| surface.preview_tab() == Some(tab.as_str()));
            if !exists {
                self.select_float(thread, &tab, cx);
                floated.push(tab);
            } else if force {
                // A forced reveal of an already docked tab belongs in the panel.
                self.restore_float(thread, &tab, cx);
                if !selected {
                    self.state.update(cx, |state, cx| {
                        if state.panels.panels.open_browser(thread, Some(&tab)) {
                            state.save_panels(cx);
                        }
                    });
                }
            }
        }
        floated
    }
}

/// A workspace child layered above the chat and below dialogs.
pub(crate) fn floating_preview(window: &mut Window, cx: &mut App) -> Option<Entity<FloatingPreview>> {
    let previews = previews(cx)?;
    Some(window.use_keyed_state("floating-browser-preview", cx, |_, cx| FloatingPreview::new(previews, cx)))
}

pub(crate) struct FloatingPreview {
    previews: Entity<Previews>,
    position: Option<Point<Pixels>>,
    width: f32,
    drag_start: Option<DragStart>,
    frame_sub: Option<(EntityId, Subscription)>,
    _subscriptions: [Subscription; 2],
}

struct PlayerDrag;

enum DragStart {
    Move(Point<Pixels>),
    Resize { mouse_x: Pixels, width: f32 },
}

impl FloatingPreview {
    fn new(previews: Entity<Previews>, cx: &mut Context<Self>) -> Self {
        let changes = cx.observe(&previews, |_, _, cx| cx.notify());
        let sessions = cx.subscribe(&previews, |this, _, PreviewsEvent::Thread(thread), cx| {
            if this.previews.read(cx).state.read(cx).panel_thread().as_deref() == Some(thread) {
                cx.notify();
            }
        });
        Self { previews, position: None, width: 320., drag_start: None, frame_sub: None, _subscriptions: [changes, sessions] }
    }

    fn clamp_position(position: Point<Pixels>, bounds: Bounds<Pixels>, window: &Window) -> Point<Pixels> {
        let viewport = window.viewport_size();
        point(
            position.x.clamp(px(0.), (viewport.width - bounds.size.width).max(px(0.))),
            position.y.clamp(px(0.), (viewport.height - bounds.size.height).max(px(0.))),
        )
    }
}

impl Render for FloatingPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let previews = self.previews.read(cx);
        let state = previews.state.clone();
        let hidden = div().into_any_element();
        let Some(thread) = state.read(cx).panel_thread().filter(|key| !AppState::is_utility_panel_key(key)) else {
            return hidden;
        };
        let Some(tab) = previews.floating_tabs.get(&thread).cloned() else { return hidden };
        let Some(session) = previews.session(&thread, &tab) else { return hidden };
        if previews.availability() != Availability::Available {
            return hidden;
        }
        let title = session.nav_status.title().filter(|title| !title.is_empty()).unwrap_or(t(cx, "Floating browser preview")).to_owned();
        let frame = self.previews.update(cx, |previews, cx| {
            if let Some(slot) = &previews.floating_frame
                && slot.thread == thread && slot.tab == tab
            {
                return slot.frame.clone();
            }
            previews.stop_floating_frame(cx);
            let controller = cx.entity();
            let frame = cx.new(|cx| {
                let mut frame = FrameView::new(state, controller, thread.clone(), tab.clone(), window, cx);
                frame.relocate(true, false);
                frame.set_visible(true, cx);
                frame
            });
            previews.floating_frame = Some(FloatingFrame { thread: thread.clone(), tab: tab.clone(), frame: frame.clone(), pending_popup: None });
            frame
        });
        if self.frame_sub.as_ref().is_none_or(|(id, _)| *id != frame.entity_id()) {
            self.frame_sub = Some((frame.entity_id(), cx.subscribe(&frame, |_, _, _, cx| cx.notify())));
        }

        let viewport = window.viewport_size();
        let ratio = frame.read(cx).aspect_ratio().clamp(0.25, 4.);
        let chrome_height = 73.;
        let max_height = (f32::from(viewport.height) - 100.).max(chrome_height + 64.);
        let max_width = f32::from(viewport.width - px(32.)).min((max_height - chrome_height) * ratio + 14.);
        let width = self.width.min(max_width.max(80.));
        let height = ((width - 14.) / ratio + chrome_height).min(max_height);
        let player_size = size(px(width), px(height));
        let default = point(viewport.width - player_size.width - px(16.), px(66.));
        let origin = Self::clamp_position(self.position.unwrap_or(default), Bounds::new(default, player_size), window);
        let p = palette(cx);
        let (restore_thread, restore_tab) = (thread.clone(), tab.clone());
        let (popout_thread, popout_tab) = (thread.clone(), tab.clone());
        let grab_player = cx.weak_entity();
        let resize_player = grab_player.clone();
        let separate = self.previews.read(cx).separate_window_open(&thread, &tab);
        let can_popout = self.previews.read(cx).can_open_separate(cx);

        v_flex()
            .id("floating-browser-preview")
            .absolute()
            .left(origin.x)
            .top(origin.y)
            .w(player_size.width)
            .h(player_size.height)
            .rounded(px(12.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.background))
            .shadow_lg()
            .occlude()
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<PlayerDrag>, window, cx| {
                match this.drag_start {
                    Some(DragStart::Move(offset)) => {
                        this.position = Some(Self::clamp_position(event.event.position - offset, event.bounds, window));
                    }
                    Some(DragStart::Resize { mouse_x, width }) => {
                        this.width = (width + f32::from(event.event.position.x - mouse_x))
                            .clamp(160., f32::from(window.viewport_size().width - event.bounds.origin.x).max(160.));
                    }
                    None => return,
                }
                cx.notify();
            }))
            .child(
                h_flex()
                    .h(px(32.))
                    .flex_none()
                    .gap(px(4.))
                    .px(px(6.))
                    .child(
                        div().id("floating-browser-grab")
                            .flex_1().h_full().min_w_0().flex().items_center().cursor(gpui::CursorStyle::OpenHand)
                            .child(div().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(title))
                            .on_drag(PlayerDrag, move |_, offset, _, cx| {
                                let _ = grab_player.update(cx, |player, _| {
                                    player.drag_start = Some(DragStart::Move(offset + point(px(7.), px(1.))));
                                });
                                cx.new(|_| EmptyView)
                            }),
                    )
                    .child(Button::new("floating-browser-restore").variant(Variant::Ghost).size(Size::IconXs)
                        .icon(Icon::PanelRight).tooltip(t(cx, "Open in right panel"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.previews.update(cx, |previews, cx| previews.restore_float(&restore_thread, &restore_tab, cx));
                        })))
                    .when(can_popout, |header| header.child(
                        Button::new("floating-browser-popout").variant(Variant::Ghost).size(Size::IconXs).icon(Icon::ExternalLink)
                            .tooltip(t(cx, if separate { "Close separate preview window" } else { "Open separate preview window" }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.previews.update(cx, |previews, cx| previews.toggle_separate_window(&popout_thread, &popout_tab, cx));
                            })),
                    ))
                    .child(Button::new("floating-browser-close").variant(Variant::Ghost).size(Size::IconXs)
                        .icon(Icon::X).tooltip(t(cx, "Close floating preview"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.previews.update(cx, |previews, cx| previews.dismiss_float(&thread, &tab, cx));
                        }))),
            )
            .child(div().flex_1().relative().min_h_0().overflow_hidden().mx(px(6.)).mb(px(6.)).child(frame))
            .child(
                div().id("floating-browser-resize").absolute().bottom_0().right_0().size(px(10.))
                    .cursor(gpui::CursorStyle::ResizeUpLeftDownRight)
                    .on_drag(PlayerDrag, move |_, _, window, cx| {
                        let _ = resize_player.update(cx, |player, _| {
                            player.position = Some(origin);
                            player.drag_start = Some(DragStart::Resize { mouse_x: window.mouse_position().x, width });
                        });
                        cx.new(|_| EmptyView)
                    }),
            )
            .into_any_element()
    }
}

pub(super) type ReadOnlyStreamOpener = fn(&ClientHandle, &tokio::runtime::Handle, StreamTarget) -> PreviewStream;
struct ReadOnlyStreams(ReadOnlyStreamOpener);
impl gpui::Global for ReadOnlyStreams {}

/// Registers the client transport that appends `interactive=false` to preview stream upgrades.
pub(crate) fn install_read_only_stream_opener(open: ReadOnlyStreamOpener, cx: &mut App) {
    cx.set_global(ReadOnlyStreams(open));
}

pub(super) struct SeparateWindow {
    pub(super) thread: String,
    pub(super) tab: String,
    handle: AnyWindowHandle,
}

impl Previews {
    pub(super) fn can_open_separate(&self, cx: &App) -> bool {
        self.availability() == Availability::Available
            && cx.has_global::<ReadOnlyStreams>()
            && self.state.read(cx).client_handle().is_some()
    }

    pub(super) fn separate_window_open(&self, thread: &str, tab: &str) -> bool {
        self.separate_window.as_ref().is_some_and(|window| window.thread == thread && window.tab == tab)
    }

    pub(super) fn toggle_separate_window(&mut self, thread: &str, tab: &str, cx: &mut Context<Self>) {
        if AppState::is_utility_panel_key(thread) {
            return;
        }
        if self.separate_window_open(thread, tab) {
            self.close_separate_window(cx);
            return;
        }
        if !self.can_open_separate(cx) {
            return;
        }
        let Some(session) = self.session(thread, tab) else { return };
        let Some(opener) = cx.try_global::<ReadOnlyStreams>().map(|opener| opener.0) else { return };
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let runtime = state.runtime().clone();
        let tab_title = session.nav_status.title().filter(|title| !title.is_empty()).unwrap_or(t(cx, "Browser preview")).to_owned();
        let (thread, tab) = (thread.to_owned(), tab.to_owned());
        let target = StreamTarget { thread_id: thread.clone(), tab_id: tab.clone(), max_width: 1280, max_height: 1280 };
        let bounds = Bounds::centered(None, size(px(720.), px(480.)), cx);
        let controller = cx.entity().downgrade();
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some(tab_title.into()), ..Default::default() }),
                window_min_size: Some(size(px(240.), px(160.))),
                ..Default::default()
            },
            move |window, cx| {
                crate::theme::follow_system_appearance(window, cx);
                let id = window.window_handle().window_id();
                let stream = opener(&client, &runtime, target);
                cx.new(|cx| {
                    cx.on_release(move |_: &mut SeparatePreview, cx| {
                        let _ = controller.update(cx, |previews, cx| {
                            if let Some(window) = previews.separate_window.take_if(|window| window.handle.window_id() == id) {
                                cx.emit(PreviewsEvent::Thread(window.thread));
                                cx.notify();
                            }
                        });
                    }).detach();
                    SeparatePreview::new(stream, &runtime, window, cx)
                })
            },
        );
        match result {
            Ok(handle) => {
                self.close_separate_window(cx);
                cx.emit(PreviewsEvent::Thread(thread.clone()));
                self.separate_window = Some(SeparateWindow { thread, tab, handle: handle.into() });
                cx.notify();
            }
            Err(error) => {
                let title = t(cx, "Unable to pop out preview").to_owned();
                self.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, title).description(error.to_string()), cx));
            }
        }
    }

    pub(super) fn close_separate_window(&mut self, cx: &mut Context<Self>) {
        if let Some(window) = self.separate_window.take() {
            cx.defer(move |cx| { let _ = window.handle.update(cx, |_, window, _| window.remove_window()); });
            cx.emit(PreviewsEvent::Thread(window.thread));
            cx.notify();
        }
    }
}

struct SeparatePreview {
    images: FrameImages,
    _input: StreamSender,
    _decoder: Abort,
    _frame_task: Task<()>,
    _close_task: Task<()>,
}

impl SeparatePreview {
    fn new(stream: PreviewStream, runtime: &tokio::runtime::Handle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let PreviewStream { input, frames, mut events } = stream;
        let (decoder, mut decoded) = decode_frames(runtime, frames);
        cx.on_release(|this: &mut Self, cx| this.images.clear(cx)).detach();
        let frames = cx.spawn(async move |this, cx| {
            while decoded.changed().await.is_ok() {
                let Some(frame) = decoded.borrow_and_update().clone() else { continue };
                if this.update(cx, |this, cx| {
                    this.images.replace(frame, cx);
                    cx.notify();
                }).is_err() {
                    return;
                }
            }
        });
        let closed = window.spawn(cx, async move |cx| {
            while let Some(event) = events.recv().await {
                if matches!(event, StreamEvent::Gone | StreamEvent::HostSetup(_) | StreamEvent::Refused) {
                    break;
                }
            }
            let _ = cx.update(|window, _| window.remove_window());
        });
        Self { images: FrameImages::default(), _input: input, _decoder: decoder, _frame_task: frames, _close_task: closed }
    }
}

impl Render for SeparatePreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let p = palette(cx);
        v_flex()
            .size_full()
            .bg(hsla(p.background))
            .child(div().flex_none().px(px(12.)).py(px(6.)).text_xs().text_color(hsla(p.muted_foreground)).child(t(cx, "Read-only")))
            .child(div().flex_1().min_h_0().child(
                canvas(|_, _, _| (), move |bounds, (), window, cx| entity.read(cx).images.paint(bounds, window)).size_full(),
            ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[gpui::test]
    fn separate_window_closes_only_when_its_stream_ends(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        // Keep the transport runtime unpolled and drive the view with synthetic stream events.
        let socket = t3_client::preview_stream::open(
            runtime.handle(),
            Arc::new(t3_client::auth::Credentials { http_base: "http://127.0.0.1:1".into(), access_token: String::new() }),
            StreamTarget { thread_id: "thread".into(), tab_id: "tab".into(), max_width: 1280, max_height: 1280 },
        );
        let (send, events) = tokio::sync::mpsc::unbounded_channel();
        let stream = PreviewStream { events, ..socket };
        let window = cx.open_window(size(px(720.), px(480.)), |window, cx| SeparatePreview::new(stream, runtime.handle(), window, cx));
        let handle = AnyWindowHandle::from(window);
        send.send(StreamEvent::Connected(false)).unwrap();
        cx.run_until_parked();
        assert!(cx.update(|cx| cx.windows().contains(&handle)));

        send.send(StreamEvent::Gone).unwrap();
        cx.run_until_parked();
        assert!(!cx.update(|cx| cx.windows().contains(&handle)));
    }
}
