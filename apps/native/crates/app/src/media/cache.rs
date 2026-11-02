//! Decoded transcript images, bounded. The web leans on the browser's image cache and lazy
//! `<img>`; here a fetched image is decoded on the network runtime's blocking pool, downscaled to
//! the size it can be drawn at, and kept while it is drawn. Over budget, images the transcript
//! has not drawn for a few frames (rows scrolled away) are dropped together with their GPU atlas
//! tiles, and come back from the encoded-bytes cache, or the network, when scrolled back to.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, Context, Entity, EntityId, RenderImage, SharedString, Task, Window};
use t3_core::media::MediaSource;

use crate::app_state::AppState;

use super::decode::{self, Decoded};

/// Decoded pixels kept across frames before offscreen images are dropped.
const DECODED_BUDGET: usize = 160 * 1024 * 1024;
/// Encoded bytes kept so scrolling back re-decodes without another round trip.
const ENCODED_BUDGET: usize = 64 * 1024 * 1024;
/// Frames an image survives undrawn before it may be evicted.
const KEEP_FRAMES: u64 = 2;
/// A failed load is shown until then, and retried the next time it is drawn after.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// How large an image decodes: thumbnails for the transcript, full size for the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Variant {
    Thumb,
    Full,
}

impl Variant {
    /// The longest edge in device pixels: a lone image or collage at 2x, a full-screen view at 2x.
    fn max_edge(self) -> u32 {
        match self {
            Variant::Thumb => 1280,
            Variant::Full => 3072,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    environment: Arc<str>,
    source: MediaSource,
}

enum Slot {
    Loading { waiters: Vec<EntityId>, _task: Task<()> },
    Ready { image: Arc<RenderImage>, bytes: usize, drawn: u64 },
    Failed { message: SharedString, at: Instant },
}

/// A finished load: the encoded bytes, the size the server reported, and the decoded image.
type Loaded = Result<(Arc<[u8]>, Option<(u32, u32)>, Decoded), String>;

pub enum ImageState {
    Loading,
    Ready(Arc<RenderImage>),
    Failed(SharedString),
}

struct Encoded {
    bytes: Arc<[u8]>,
    used: u64,
}

pub struct MediaCache {
    state: Entity<AppState>,
    images: HashMap<(Key, Variant), Slot>,
    encoded: HashMap<Key, Encoded>,
    encoded_total: usize,
    decoded_total: usize,
    /// Each image's full size, from the server or the first decode, so a frame can be laid out
    /// before its pixels arrive.
    dimensions: HashMap<Key, (u32, u32)>,
    pinned: HashSet<(Key, Variant)>,
    frame: u64,
    permits: Arc<tokio::sync::Semaphore>,
}

impl MediaCache {
    pub fn new(state: Entity<AppState>) -> Self {
        MediaCache {
            state,
            images: HashMap::new(),
            encoded: HashMap::new(),
            encoded_total: 0,
            decoded_total: 0,
            dimensions: HashMap::new(),
            pinned: HashSet::new(),
            frame: 0,
            permits: Arc::new(tokio::sync::Semaphore::new(t3_core::media::MAX_IN_FLIGHT)),
        }
    }

    fn key(&self, source: &MediaSource, cx: &App) -> Option<Key> {
        let environment = self.state.read(cx).environment()?;
        Some(Key { environment: Arc::from(environment), source: source.clone() })
    }

    /// The image for `source`, loading it on first ask. `owner` is the view drawing it, notified
    /// when it arrives.
    pub fn image(&mut self, source: &MediaSource, variant: Variant, owner: EntityId, cx: &mut Context<Self>) -> ImageState {
        let Some(key) = self.key(source, cx) else { return ImageState::Loading };
        let slot_key = (key, variant);
        match self.images.get_mut(&slot_key) {
            Some(Slot::Ready { image, drawn, .. }) => {
                *drawn = self.frame;
                return ImageState::Ready(image.clone());
            }
            Some(Slot::Loading { waiters, .. }) => {
                if !waiters.contains(&owner) {
                    waiters.push(owner);
                }
                return ImageState::Loading;
            }
            Some(Slot::Failed { message, at }) if at.elapsed() < RETRY_AFTER => return ImageState::Failed(message.clone()),
            _ => {}
        }
        let Some(task) = self.load(slot_key.clone(), cx) else { return ImageState::Loading };
        self.images.insert(slot_key, Slot::Loading { waiters: vec![owner], _task: task });
        ImageState::Loading
    }

    /// The image's full size when known, for layout before it decodes.
    pub fn dimensions(&self, source: &MediaSource, cx: &App) -> Option<(u32, u32)> {
        self.dimensions.get(&self.key(source, cx)?).copied()
    }

    /// Kept regardless of the budget while a surface outside the transcript shows it.
    pub fn pin(&mut self, source: &MediaSource, variant: Variant, cx: &App) {
        if let Some(key) = self.key(source, cx) {
            self.pinned.insert((key, variant));
        }
    }

    pub fn unpin_all(&mut self) {
        self.pinned.clear();
    }

    fn load(&mut self, (key, variant): (Key, Variant), cx: &mut Context<Self>) -> Option<Task<()>> {
        let state = self.state.read(cx);
        let client = state.client_handle()?;
        let runtime = state.runtime().clone();
        let cached = self.encoded.get_mut(&key).map(|encoded| {
            encoded.used = self.frame;
            encoded.bytes.clone()
        });
        let (permits, svg, source, max_edge) = (self.permits.clone(), cx.svg_renderer(), key.source.clone(), variant.max_edge());
        let job = runtime.spawn(async move {
            // Held through the decode as well, so a burst of large images is never all in memory.
            let _permit = permits.acquire_owned().await.map_err(|_| "Image unavailable.".to_owned())?;
            let (bytes, dimensions) = match cached {
                Some(bytes) => (bytes, None),
                None => {
                    let fetched = t3_client::assets::fetch_media(&client, &source).await?;
                    (Arc::<[u8]>::from(fetched.bytes), fetched.dimensions)
                }
            };
            let input = bytes.clone();
            let decoded = tokio::task::spawn_blocking(move || decode::decode(&input, max_edge, &svg)).await.map_err(|_| "Image unavailable.".to_owned())??;
            Ok::<_, String>((bytes, dimensions, decoded))
        });
        Some(cx.spawn(async move |this, cx| {
            let result = job.await.unwrap_or_else(|_| Err("Image unavailable.".to_owned()));
            let _ = this.update(cx, |cache, cx| cache.finish(key, variant, result, cx));
        }))
    }

    fn finish(&mut self, key: Key, variant: Variant, result: Loaded, cx: &mut Context<Self>) {
        let waiters = match self.images.remove(&(key.clone(), variant)) {
            Some(Slot::Loading { waiters, .. }) => waiters,
            _ => Vec::new(),
        };
        let slot = match result {
            Ok((bytes, dimensions, decoded)) => {
                self.dimensions.insert(key.clone(), dimensions.unwrap_or(decoded.original));
                self.remember_encoded(key.clone(), bytes);
                self.decoded_total += decoded.bytes;
                Slot::Ready { image: decoded.image, bytes: decoded.bytes, drawn: self.frame }
            }
            Err(message) => Slot::Failed { message: message.into(), at: Instant::now() },
        };
        self.images.insert((key, variant), slot);
        let app: &mut App = cx;
        for owner in waiters {
            app.notify(owner);
        }
    }

    fn remember_encoded(&mut self, key: Key, bytes: Arc<[u8]>) {
        if bytes.len() > ENCODED_BUDGET / 4 {
            return;
        }
        if let Some(old) = self.encoded.insert(key, Encoded { bytes: bytes.clone(), used: self.frame }) {
            self.encoded_total -= old.bytes.len();
        }
        self.encoded_total += bytes.len();
        while self.encoded_total > ENCODED_BUDGET {
            let Some(oldest) = self.encoded.iter().min_by_key(|(_, encoded)| encoded.used).map(|(key, _)| key.clone()) else { break };
            if let Some(evicted) = self.encoded.remove(&oldest) {
                self.encoded_total -= evicted.bytes.len();
            }
        }
    }

    /// Called once per transcript render, before rows draw: over budget, drops what the last
    /// frames did not draw, oldest first, freeing the atlas tiles with it.
    pub fn begin_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.frame += 1;
        if self.decoded_total <= DECODED_BUDGET {
            return;
        }
        let frame = self.frame;
        let mut stale: Vec<(u64, (Key, Variant))> = self
            .images
            .iter()
            .filter(|(key, _)| !self.pinned.contains(*key))
            .filter_map(|(key, slot)| match slot {
                Slot::Ready { drawn, .. } if drawn + KEEP_FRAMES < frame => Some((*drawn, key.clone())),
                _ => None,
            })
            .collect();
        stale.sort_by_key(|(drawn, _)| *drawn);
        for (_, key) in stale {
            if self.decoded_total <= DECODED_BUDGET {
                break;
            }
            if let Some(Slot::Ready { image, bytes, .. }) = self.images.remove(&key) {
                self.decoded_total -= bytes;
                cx.drop_image(image, Some(window));
            }
        }
    }
}
