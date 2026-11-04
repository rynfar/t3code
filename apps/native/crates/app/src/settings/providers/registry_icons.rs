//! `AcpRegistryIcon.tsx`: validated CDN icons, decoded by GPUI off the UI thread.
mod decode;
use std::{collections::HashMap, sync::Arc, time::Instant};

use gpui::{App, Context, EntityId, Global, RenderImage, Task, Window};
use t3_core::settings::providers::registry::{Agent, IconRetry, icon_url};

use super::super::SettingsView;

#[cfg(feature = "snapshot")]
pub(super) struct FixtureIcons(pub HashMap<String, String>);
#[cfg(feature = "snapshot")]
impl Global for FixtureIcons {}

const CAPACITY: usize = 64;
struct Icons {
    slots: HashMap<(String, u32), Slot>,
    tick: u64,
    clock: Instant,
    permits: Arc<tokio::sync::Semaphore>,
}
struct Slot {
    image: Option<Arc<RenderImage>>,
    used: u64,
    owner: EntityId,
    retry: IconRetry,
    _task: Option<Task<()>>,
}
impl Default for Icons {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
            tick: 0,
            clock: Instant::now(),
            permits: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }
}
impl Global for Icons {}

pub(super) fn image(
    agent: &Agent,
    color: u32,
    runtime: tokio::runtime::Handle,
    window: &mut Window,
    cx: &mut Context<SettingsView>,
) -> Option<Arc<RenderImage>> {
    let url = icon_url(agent)?;
    if !cx.has_global::<Icons>() {
        cx.set_global(Icons::default());
    }
    let key = (url.clone(), color);
    let owner = cx.entity_id();
    let store = cx.global_mut::<Icons>();
    store.tick += 1;
    let now = store.clock.elapsed().as_millis() as u64;
    if let Some(slot) = store.slots.get_mut(&key) {
        slot.used = store.tick;
        slot.owner = owner;
        if !slot.retry.due(now) {
            return slot.image.clone();
        }
        store.slots.remove(&key);
    }
    if store.slots.len() >= CAPACITY {
        let oldest = store
            .slots
            .iter()
            .filter(|(_, s)| s._task.is_none())
            .min_by_key(|(_, s)| s.used)
            .map(|(k, _)| k.clone());
        let old = oldest.and_then(|old| store.slots.remove(&old));
        let old = old?;
        if let Some(image) = old.image {
            cx.drop_image(image, Some(window));
        }
    }
    let permits = cx.global::<Icons>().permits.clone();
    let svg = cx.svg_renderer();
    #[cfg(feature = "snapshot")]
    let fixture = cx
        .try_global::<FixtureIcons>()
        .and_then(|f| f.0.get(&url))
        .cloned();
    let fixture_mode = super::fixture_active(cx);
    let driver = runtime.spawn(async move {
        let _permit = permits.acquire_owned().await.ok()?;
        let fetched = if fixture_mode {
            #[cfg(feature = "snapshot")]
            {
                Some((
                    "image/svg+xml".to_owned(),
                    tokio::fs::read(fixture?).await.ok()?,
                ))
            }
            #[cfg(not(feature = "snapshot"))]
            {
                None
            }
        } else {
            t3_client::acp_registry::icon(&url).await.ok()
        }?;
        tokio::task::spawn_blocking(move || decode::decode(&fetched.0, &fetched.1, color, &svg))
            .await
            .ok()
            .flatten()
    });
    let loaded_key = key.clone();
    let task = cx.spawn(async move |_, cx| {
        let image = driver.await.ok().flatten();
        cx.update(|cx: &mut App| {
            let store = cx.global_mut::<Icons>();
            let now = store.clock.elapsed().as_millis() as u64;
            if let Some(slot) = store.slots.get_mut(&loaded_key) {
                slot.image = image;
                if slot.image.is_none() {
                    slot.retry.failed(now);
                }
                slot._task = None;
                let owner = slot.owner;
                cx.notify(owner);
            }
        });
    });
    let store = cx.global_mut::<Icons>();
    store.slots.insert(
        key,
        Slot {
            image: None,
            used: store.tick,
            owner,
            retry: IconRetry::default(),
            _task: Some(task),
        },
    );
    None
}
