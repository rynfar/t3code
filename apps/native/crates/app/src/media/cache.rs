//! Bounded decoded media. Visible consumers protect pixels; inactive entries are evicted by
//! access order independently of transcript renders. Decode work runs on the network runtime.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AnyWindowHandle, App, Context, Entity, EntityId, RenderImage, SharedString, Task, Window};
use t3_core::media::{AssetContentKey, AssetResource, MediaSource};

use crate::app_state::AppState;

use super::decode::{self, Decoded};
use super::revisions::Revisions;

/// Decoded pixels kept across frames before offscreen images are dropped.
const DECODED_BUDGET: usize = 160 * 1024 * 1024;
/// Encoded bytes kept so scrolling back re-decodes without another round trip.
const ENCODED_BUDGET: usize = 64 * 1024 * 1024;
/// A failed load is shown until then, and retried the next time it is drawn after.
pub(super) const RETRY_AFTER: Duration = Duration::from_secs(30);

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
pub(super) struct Key {
    pub(super) environment: Arc<str>,
    pub(super) source: MediaSource,
    pub(super) content: Option<AssetContentKey>,
}

enum Slot {
    Loading { waiters: Vec<EntityId>, _task: Task<()> },
    Ready { image: Arc<RenderImage>, bytes: usize, drawn: u64 },
    Failed { message: SharedString, at: Instant },
}

/// A finished load: the encoded bytes, the size the server reported, and the decoded image.
type LoadError = (String, Option<String>);
struct CancelLoad(tokio::task::AbortHandle);
impl Drop for CancelLoad { fn drop(&mut self) { self.0.abort(); } }

type Loaded = Result<(Arc<[u8]>, Option<(u32, u32)>, Decoded), LoadError>;

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
    pub(super) state: Entity<AppState>,
    #[cfg(feature = "snapshot")]
    loads: Vec<String>,
    #[cfg(feature = "snapshot")]
    decoded_high_water: usize,
    pub(super) revisions: Revisions,
    pub(super) visibility: t3_core::media::MediaVisibility<(Key, Variant)>,
    pub(super) consumer_owners: HashMap<u64, EntityId>,
    pub(super) refresh_window: Option<AnyWindowHandle>,
    decoded_budget: usize,
    pub(super) refresh_timer: Option<Task<()>>,
    pub(super) refresh_deadline: Option<f64>,
    images: HashMap<(Key, Variant), Slot>,
    encoded: HashMap<Key, Encoded>,
    encoded_total: usize,
    decoded_total: usize,
    /// Each image's full size, from the server or the first decode, so a frame can be laid out
    /// before its pixels arrive.
    dimensions: HashMap<Key, (u32, u32)>,
    pinned: HashSet<(Key, Variant)>,
    pub(super) frame: u64,
    pub(super) permits: Arc<tokio::sync::Semaphore>,
}

impl MediaCache {
    #[cfg(feature = "snapshot")]
    pub fn audit(&self) -> serde_json::Value {
        serde_json::json!({"loads":self.loads,"decodedBytes":self.decoded_total,"decodedHighWater":self.decoded_high_water,"decodedBudget":self.decoded_budget,"consumers":self.visibility.len(),"images":self.images.len(),"timerArmed":self.refresh_timer.is_some()})
    }

    pub fn new(state: Entity<AppState>) -> Self {
        MediaCache {
            state,
            #[cfg(feature = "snapshot")]
            loads: Vec::new(),
            #[cfg(feature = "snapshot")]
            decoded_high_water: 0,
            revisions: Revisions::default(),
            visibility: Default::default(),
            consumer_owners: Default::default(),
            refresh_window: None,
            decoded_budget: {
                #[cfg(feature = "snapshot")]
                { std::env::args().collect::<Vec<_>>().windows(2).find(|pair| pair[0] == "--media-cache-budget").and_then(|pair| pair[1].parse::<usize>().ok()).unwrap_or(DECODED_BUDGET).clamp(65536, DECODED_BUDGET) }
                #[cfg(not(feature = "snapshot"))]
                { DECODED_BUDGET }
            },
            refresh_timer: None,
            refresh_deadline: None,
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
        let mut key = Key { environment: Arc::from(environment), source: source.clone(), content: None };
        if matches!(source, MediaSource::Asset(_)) {
            key.content = Some(self.revisions.shown_asset(&key)?.ok()?.content_key());
        }
        Some(key)
    }

    pub fn peek(&self, source: &MediaSource, variant: Variant, cx: &App) -> ImageState {
        let Some(resource) = self.resource_key(source, cx) else { return ImageState::Loading };
        if let Some(Err(message)) = self.revisions.shown_url(&resource) { return ImageState::Failed(message.to_owned().into()); }
        let Some(key) = self.key(source, cx) else { return ImageState::Loading };
        match self.images.get(&(key, variant)) {
            Some(Slot::Ready { image, .. }) => ImageState::Ready(image.clone()),
            Some(Slot::Failed { message, .. }) => ImageState::Failed(message.clone()),
            _ => ImageState::Loading,
        }
    }

    pub(super) fn resource_key(&self, source: &MediaSource, cx: &App) -> Option<Key> {
        Some(Key { environment: self.state.read(cx).environment()?.into(), source: source.clone(), content: None })
    }

    pub(super) fn source_visible(&self, key: &Key) -> bool {
        self.visibility.keys().any(|(visible, _)| visible.environment == key.environment && visible.source == key.source)
    }

    pub fn visible_image(&mut self, source: &MediaSource, variant: Variant, scope: &super::MediaScope, consumer: u64, window: &mut Window, cx: &mut Context<Self>) -> ImageState {
        let Some(resource) = self.resource_key(source, cx) else { return ImageState::Loading };
        let remount = !self.source_visible(&resource);
        self.refresh_window = Some(window.window_handle());
        let image = self.image(source, variant, scope.owner, super::revision_for(source, scope), cx);
        let key = self.key(source, cx).unwrap_or_else(|| resource.clone());
        let previous = self.visibility.get(consumer).cloned();
        let changed = self.visibility.set(consumer, Some((key, variant)));
        self.release_unused(previous);
        self.consumer_owners.insert(consumer, scope.owner);
        if remount { self.refresh_remount(&resource, scope.owner, cx); }
        if changed { self.schedule_refresh(window, cx); }
        image
    }

    fn release_unused(&mut self, previous: Option<(Key, Variant)>) {
        if let Some(key) = previous && !self.visibility.contains(&key) && !self.pinned.contains(&key)
            && matches!(self.images.get(&key), Some(Slot::Loading { .. } | Slot::Failed { .. }))
        { self.images.remove(&key); }
    }

    pub fn remove_consumer(&mut self, consumer: u64, cx: &mut Context<Self>) {
        let previous = self.visibility.get(consumer).cloned();
        if !self.visibility.set(consumer, None) { return; }
        self.consumer_owners.remove(&consumer);
        self.release_unused(previous);
        self.trim(cx);
        self.refresh_timer = None;
        self.refresh_deadline = None;
        if !self.visibility.is_empty() { self.defer_refresh_timer(cx); }
    }

    /// The image for `source`, loading it on first ask. `owner` is the view drawing it, notified
    /// when it arrives.
    pub fn image(&mut self, source: &MediaSource, variant: Variant, owner: EntityId, revision: Option<&str>, cx: &mut Context<Self>) -> ImageState {
        self.frame += 1;
        let Some(environment) = self.state.read(cx).environment() else { return ImageState::Loading };
        let resource_key = Key { environment: Arc::from(environment), source: source.clone(), content: None };
        self.refresh_asset(&resource_key, revision, owner, cx);
        if let Some(Err(message)) = self.revisions.shown_url(&resource_key) { return ImageState::Failed(message.to_owned().into()); }
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
        self.trim(cx);
        if self.images.len() >= 1024 { return ImageState::Loading; }
        let Some(task) = self.load(slot_key.clone(), owner, cx) else { return ImageState::Loading };
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

    /// Opens `resource` by its signed URL in the browser, as the web's links to an asset do.
    pub fn open(&self, resource: &AssetResource, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(environment) = state.environment().map(str::to_owned) else { return };
        let Some(client) = state.client_handle() else { return };
        let local_id = state.environments.environments.iter().find(|saved| saved.local && saved.http_base != environment).map(|saved| saved.id.clone());
        let local = local_id.map(|id| state.connections.client(&id));
        let resource = resource.clone();
        let job = state.runtime().spawn(async move {
            t3_client::assets::mint_media_url_with_fallback(&client, &resource, async move {
                match local { Some(local) => local.await, None => None }
            }).await
        });
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(asset)) = job.await
                && let Some(url) = t3_core::media::resolve_asset_url(&environment, &asset.relative_url)
            {
                cx.update(|cx| cx.open_url(&url));
            }
        })
        .detach();
    }

    pub fn unpin_all(&mut self) {
        self.pinned.clear();
    }

    fn load(&mut self, (key, variant): (Key, Variant), owner: EntityId, cx: &mut Context<Self>) -> Option<Task<()>> {
        #[cfg(feature = "snapshot")]
        self.loads.push(match &key.source { MediaSource::Asset(AssetResource::MediaFile { path, .. }) => path.clone(), _ => "<other>".into() });
        let cached = self.encoded.get_mut(&key).map(|encoded| {
            encoded.used = self.frame;
            encoded.bytes.clone()
        });
        let resource_key = Key { content: None, ..key.clone() };
        let asset_url = if cached.is_none() && matches!(key.source, MediaSource::Asset(_)) {
            Some(self.asset_for_fetch(&resource_key, owner, cx)?.relative_url)
        } else { None };
        let state = self.state.read(cx);
        let client = state.client_handle()?;
        let runtime = state.runtime().clone();
        let (permits, svg, source, max_edge) = (self.permits.clone(), cx.svg_renderer(), key.source.clone(), variant.max_edge());
        let job = runtime.spawn(async move {
            // Held through the decode as well, so a burst of large images is never all in memory.
            let permit = permits.acquire_owned().await.map_err(|_| ("Image unavailable.".to_owned(), None))?;
            let (bytes, dimensions) = match cached {
                Some(bytes) => (bytes, None),
                None => {
                    let fetched = match asset_url {
                        Some(url) => t3_client::assets::fetch_signed_media(&client, &url).await.map_err(|message| (message, Some(url)))?,
                        None => t3_client::assets::fetch_media(&client, &source).await.map_err(|message| (message, None))?,
                    };
                    (Arc::<[u8]>::from(fetched.bytes), fetched.dimensions)
                }
            };
            let input = bytes.clone();
            let decoded = t3_client::assets::decode_media(permit, move || decode::decode(&input, max_edge, &svg)).await.map_err(|message| (message, None))?;
            Ok::<_, LoadError>((bytes, dimensions, decoded))
        });
        let cancel = CancelLoad(job.abort_handle());
        Some(cx.spawn(async move |this, cx| {
            let _cancel = cancel;
            let result = job.await.unwrap_or_else(|_| Err(("Image unavailable.".to_owned(), None)));
            let _ = this.update(cx, |cache, cx| cache.finish(key, variant, result, cx));
        }))
    }

    fn finish(&mut self, key: Key, variant: Variant, result: Loaded, cx: &mut Context<Self>) {
        let waiters = match self.images.remove(&(key.clone(), variant)) {
            Some(Slot::Loading { waiters, .. }) => waiters,
            _ => Vec::new(),
        };
        if let Err((_, Some(relative_url))) = &result {
            let resource_key = Key { content: None, ..key.clone() };
            self.revisions.fetch_failed(&resource_key, relative_url);
        }
        let slot = match result {
            Ok((bytes, dimensions, decoded)) => {
                self.dimensions.insert(key.clone(), dimensions.unwrap_or(decoded.original));
                self.remember_encoded(key.clone(), bytes);
                self.decoded_total += decoded.bytes;
                Slot::Ready { image: decoded.image, bytes: decoded.bytes, drawn: self.frame }
            }
            Err((message, _)) => Slot::Failed { message: message.into(), at: Instant::now() },
        };
        let inserted = (key, variant);
        self.images.insert(inserted.clone(), slot);
        self.trim(cx);
        if self.decoded_total > self.decoded_budget && let Some(Slot::Ready { image, bytes, .. }) = self.images.remove(&inserted) {
            self.decoded_total -= bytes;
            Self::drop_pixels(vec![image], cx);
            self.images.insert(inserted, Slot::Failed { message: "The visible images exceed the image cache budget.".into(), at: Instant::now() });
        }
        #[cfg(feature = "snapshot")]
        { self.decoded_high_water = self.decoded_high_water.max(self.decoded_total); }
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
        while self.encoded_total > ENCODED_BUDGET || self.encoded.len() > 512 {
            let Some(oldest) = self.encoded.iter().min_by_key(|(_, encoded)| encoded.used).map(|(key, _)| key.clone()) else { break };
            if let Some(evicted) = self.encoded.remove(&oldest) {
                self.encoded_total -= evicted.bytes.len();
            }
        }
    }

    fn trim(&mut self, cx: &mut Context<Self>) {
        let residents = self.images.iter().filter_map(|(key, slot)| {
            if let Slot::Ready { bytes, drawn, .. } = slot {
                Some((key.clone(), *bytes, *drawn, self.pinned.contains(key) || self.visibility.contains(key)))
            } else { None }
        }).collect();
        let mut dropped = Vec::new();
        for key in t3_core::media::media_cache_evictions(residents, self.decoded_budget, 1023) {
            if let Some(Slot::Ready { image, bytes, .. }) = self.images.remove(&key) {
                self.decoded_total -= bytes;
                dropped.push(image);
            }
        }
        Self::drop_pixels(dropped, cx);
        if self.dimensions.len() > 1536 {
            let live: HashSet<_> = self.encoded.keys().chain(self.images.keys().map(|(key, _)| key)).collect();
            self.dimensions.retain(|key, _| live.contains(key));
        }
    }

    fn drop_pixels(images: Vec<Arc<RenderImage>>, cx: &mut Context<Self>) {
        if images.is_empty() { return; }
        // Prepaint temporarily removes its window from App; evict after that lease returns.
        cx.defer(move |cx| {
            for image in images { cx.drop_image(image, None); }
            cx.refresh_windows();
        });
    }

    pub fn begin_frame(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.images.retain(|_, slot| !matches!(slot, Slot::Failed { at, .. } if at.elapsed() >= RETRY_AFTER));
        self.trim(cx);
    }
}
