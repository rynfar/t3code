//! Audio file preview (`AudioPreview.tsx`): bounded asset download, pure-Rust audio header
//! inspection, native macOS `NSSound` playback with play/pause/stop/time progress (ticking only
//! while actively playing), and external-open fallback on unsupported codecs/platforms.

use std::time::Duration;

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Task, div,
    prelude::*, px,
};
use gpui_component::{h_flex, v_flex};
use t3_core::files::media_preview::{AudioInfo, MAX_AUDIO_BYTES, format_audio_time, inspect_audio_bytes};
use t3_core::files::{CREATE_ASSET_URL, absolute_path, decode_asset_url, group_thousands, workspace_asset_request};

use super::Root;
use super::preview::Preview;
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

#[cfg(target_os = "macos")]
mod native_audio {
    use objc2::{AnyThread, MainThreadMarker, rc::Retained};
    use objc2_app_kit::NSSound;
    use objc2_foundation::NSData;

    pub struct SoundPlayer {
        sound: Retained<NSSound>,
        duration: f64,
    }

    impl SoundPlayer {
        pub fn new(bytes: &[u8]) -> Option<Self> {
            let _mtm = MainThreadMarker::new()?;
            let data = NSData::with_bytes(bytes);
            let sound = NSSound::initWithData(NSSound::alloc(), &data)?;
            let duration = sound.duration();
            Some(Self { sound, duration: if duration.is_finite() && duration > 0.0 { duration } else { 0.0 } })
        }

        pub fn play(&self) -> bool {
            self.sound.play()
        }

        pub fn pause(&self) -> bool {
            self.sound.pause()
        }

        pub fn resume(&self) -> bool {
            self.sound.resume()
        }

        pub fn stop(&self) {
            self.sound.stop();
        }

        pub fn is_playing(&self) -> bool {
            self.sound.isPlaying()
        }

        pub fn duration(&self) -> f64 {
            self.duration
        }

        pub fn current_time(&self) -> f64 {
            let t = self.sound.currentTime();
            if t.is_finite() && t >= 0.0 { t } else { 0.0 }
        }
    }

    impl Drop for SoundPlayer {
        fn drop(&mut self) {
            self.sound.stop();
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod native_audio {
    pub struct SoundPlayer;

    impl SoundPlayer {
        pub fn new(_bytes: &[u8]) -> Option<Self> {
            None
        }
        pub fn play(&self) -> bool {
            false
        }
        pub fn pause(&self) -> bool {
            false
        }
        pub fn resume(&self) -> bool {
            false
        }
        pub fn stop(&self) {}
        pub fn is_playing(&self) -> bool {
            false
        }
        pub fn duration(&self) -> f64 {
            0.0
        }
        pub fn current_time(&self) -> f64 {
            0.0
        }
    }
}

pub struct DecodedAudio {
    pub name: String,
    pub path: String,
    pub thread_id: String,
    pub info: AudioInfo,
    pub bytes: Vec<u8>,
}

pub enum AudioState {
    Loading,
    Ready {
        name: String,
        path: String,
        thread_id: String,
        info: AudioInfo,
        player: Option<native_audio::SoundPlayer>,
        playing: bool,
        paused: bool,
        elapsed: f64,
        duration: f64,
        _tick: Option<Task<()>>,
    },
    Failed(String),
}

pub fn load(
    state: &AppState,
    root: &Root,
    path: &str,
    mutation: Option<&str>,
) -> Option<tokio::task::JoinHandle<Result<DecodedAudio, String>>> {
    let client = state.client_handle()?;
    let abs = absolute_path(path, &root.workspace.cwd);
    let request = workspace_asset_request(&root.workspace.thread_id, &root.workspace.cwd, &abs);
    let mutation = mutation.map(str::to_owned);
    let path_owned = path.to_owned();
    let thread_id = root.workspace.thread_id.clone();
    Some(state.runtime().spawn(async move {
        let value = client
            .request(CREATE_ASSET_URL, request)
            .await
            .map_err(|_| "Unable to load workspace audio.".to_owned())?;
        let url = decode_asset_url(&value).map_err(|_| "Unable to load workspace audio.".to_owned())?;
        let bytes = client
            .asset_bytes(
                &format!("{url}{}", t3_core::media::workspace_image_suffix(&url, mutation.as_deref())),
                MAX_AUDIO_BYTES,
            )
            .await?;
        let info = inspect_audio_bytes(&path_owned, &bytes);
        let name = t3_core::files::basename(&path_owned).to_owned();
        Ok(DecodedAudio { name, path: abs, thread_id, info, bytes })
    }))
}

impl AudioState {
    pub fn from_decoded(result: Result<DecodedAudio, String>) -> Self {
        match result {
            Ok(decoded) => {
                let player = native_audio::SoundPlayer::new(&decoded.bytes);
                let duration = player
                    .as_ref()
                    .map(native_audio::SoundPlayer::duration)
                    .filter(|&d| d > 0.0)
                    .or(decoded.info.duration_secs)
                    .unwrap_or(0.0);
                AudioState::Ready {
                    name: decoded.name,
                    path: decoded.path,
                    thread_id: decoded.thread_id,
                    info: decoded.info,
                    player,
                    playing: false,
                    paused: false,
                    elapsed: 0.0,
                    duration,
                    _tick: None,
                }
            }
            Err(error) => AudioState::Failed(error),
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self, AudioState::Failed(_))
    }

    pub fn stop(&mut self) {
        if let AudioState::Ready { player, playing, paused, elapsed, _tick, .. } = self {
            if let Some(player) = player {
                player.stop();
            }
            *playing = false;
            *paused = false;
            *elapsed = 0.0;
            *_tick = None;
        }
    }
}

impl Preview {
    pub(super) fn toggle_audio_playback(&mut self, cx: &mut Context<Self>) {
        let Some(AudioState::Ready { player, playing, paused, elapsed, _tick, .. }) = self.audio_state_mut() else {
            return;
        };
        let Some(player) = player else { return };
        if *playing {
            player.pause();
            *playing = false;
            *paused = true;
            *elapsed = player.current_time();
            *_tick = None;
            cx.notify();
            return;
        }
        let started = if *paused { player.resume() } else { player.play() };
        if started {
            *playing = true;
            *paused = false;
            *_tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                    let keep_going = this.update(cx, |this, cx| {
                        let Some(AudioState::Ready { player: Some(player), playing, paused, elapsed, _tick, .. }) =
                            this.audio_state_mut()
                        else {
                            return false;
                        };
                        if !*playing {
                            return false;
                        }
                        if player.is_playing() {
                            *elapsed = player.current_time();
                            cx.notify();
                            true
                        } else {
                            *playing = false;
                            *paused = false;
                            *elapsed = 0.0;
                            *_tick = None;
                            cx.notify();
                            false
                        }
                    });
                    if !matches!(keep_going, Ok(true)) {
                        break;
                    }
                }
            }));
            cx.notify();
        }
    }

    pub(super) fn stop_audio_playback(&mut self, cx: &mut Context<Self>) {
        if let Some(audio) = self.audio_state_mut() {
            audio.stop();
            cx.notify();
        }
    }

    pub(super) fn render_audio(&self, cx: &Context<Self>) -> AnyElement {
        let p = palette(cx);
        let entity = cx.entity().downgrade();
        let Some(audio) = self.audio_state() else {
            return div().into_any_element();
        };
        match audio {
            AudioState::Loading => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child("Loading audio…")
                .into_any_element(),
            AudioState::Failed(error) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .px(px(24.))
                .text_center()
                .text_xs()
                .text_color(hsla(p.destructive))
                .child(error.clone())
                .into_any_element(),
            AudioState::Ready {
                name,
                path,
                thread_id,
                info,
                player,
                playing,
                elapsed,
                duration,
                ..
            } => {
                let playable = player.is_some();
                let progress = if *duration > 0.0 {
                    (*elapsed / *duration).clamp(0.0, 1.0) as f32
                } else {
                    0.0
                };
                let mut details = vec![info.format.to_owned()];
                if let Some(rate) = info.sample_rate {
                    details.push(format!("{:.1} kHz", f64::from(rate) / 1000.0));
                }
                if let Some(ch) = info.channels {
                    details.push(if ch == 1 { "Mono".into() } else if ch == 2 { "Stereo".into() } else { format!("{ch} ch") });
                }
                if let Some(bits) = info.bits_per_sample {
                    details.push(format!("{bits}-bit"));
                }
                details.push(format!("{} bytes", group_thousands(info.byte_length)));

                let time_label = if *duration > 0.0 {
                    format!("{} / {}", format_audio_time(*elapsed), format_audio_time(*duration))
                } else {
                    format_audio_time(*elapsed)
                };
                let open_path = path.clone();
                let open_thread = thread_id.clone();

                v_flex()
                    .id("file-audio-preview")
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .p(px(24.))
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(520.))
                            .p(px(16.))
                            .gap(px(12.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(hsla(p.border))
                            .bg(hsla(p.card))
                            .child(
                                h_flex()
                                    .gap(px(10.))
                                    .items_center()
                                    .child(Icon::Play.render(px(16.), hsla(p.primary)))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(hsla(p.foreground))
                                            .child(SharedString::from(name.clone())),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(hsla(p.muted_foreground))
                                            .child(SharedString::from(time_label)),
                                    ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(6.))
                                    .rounded_full()
                                    .bg(hsla(p.secondary))
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .h_full()
                                            .w(gpui::relative(progress))
                                            .rounded_full()
                                            .bg(hsla(p.primary)),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        h_flex()
                                            .gap(px(6.))
                                            .when(playable, |this| {
                                                this.child(
                                                    Button::new("audio-play-pause")
                                                        .variant(Variant::Secondary)
                                                        .size(Size::Xs)
                                                        .icon(if *playing { Icon::Square } else { Icon::Play })
                                                        .label(if *playing { "Pause" } else { "Play" })
                                                        .on_click({
                                                            let entity = entity.clone();
                                                            move |_, _, cx| {
                                                                if let Some(entity) = entity.upgrade() {
                                                                    entity.update(cx, |this, cx| {
                                                                        this.toggle_audio_playback(cx);
                                                                    });
                                                                }
                                                            }
                                                        }),
                                                )
                                                .child(
                                                    Button::new("audio-stop")
                                                        .variant(Variant::Ghost)
                                                        .size(Size::Xs)
                                                        .label("Stop")
                                                        .disabled(!*playing && *elapsed <= 0.0)
                                                        .on_click({
                                                            let entity = entity.clone();
                                                            move |_, _, cx| {
                                                                if let Some(entity) = entity.upgrade() {
                                                                    entity.update(cx, |this, cx| {
                                                                        this.stop_audio_playback(cx);
                                                                    });
                                                                }
                                                            }
                                                        }),
                                                )
                                            })
                                            .when(!playable, |this| {
                                                this.child(
                                                    Button::new("audio-open-external")
                                                        .variant(Variant::Secondary)
                                                        .size(Size::Xs)
                                                        .icon(Icon::ExternalLink)
                                                        .label("Open externally")
                                                        .on_click(move |_, _, cx| {
                                                            crate::media::open_asset(
                                                                &t3_core::media::AssetResource::MediaFile {
                                                                    thread_id: open_thread.clone(),
                                                                    path: open_path.clone(),
                                                                },
                                                                cx,
                                                            );
                                                        }),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(hsla(p.muted_foreground))
                                            .child(SharedString::from(details.join(" · "))),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
        }
    }
}
