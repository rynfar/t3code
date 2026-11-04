//! `AcpRegistryIcon.tsx`: validated CDN icons, decoded by GPUI off the UI thread.
use std::{collections::HashMap, io::Cursor, sync::Arc};

use gpui::{App, Context, EntityId, Global, Image, ImageFormat, RenderImage, Task, Window};
use t3_core::settings::providers::registry::{Agent, icon_url};

use super::super::SettingsView;

#[cfg(feature = "snapshot")]
pub(super) struct FixtureIcons(pub HashMap<String, String>);
#[cfg(feature = "snapshot")]
impl Global for FixtureIcons {}

const CAPACITY: usize = 64;
struct Icons {
    slots: HashMap<(String, u32), Slot>,
    tick: u64,
    permits: Arc<tokio::sync::Semaphore>,
}
struct Slot {
    image: Option<Arc<RenderImage>>,
    used: u64,
    owner: EntityId,
    _task: Option<Task<()>>,
}
impl Default for Icons {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
            tick: 0,
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
    if let Some(slot) = store.slots.get_mut(&key) {
        slot.used = store.tick;
        slot.owner = owner;
        return slot.image.clone();
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
        tokio::task::spawn_blocking(move || {
            let (mime, bytes) = fetched;
            let format = ImageFormat::from_mime_type(&mime)?;
            if bytes.len() > 512 * 1024 {
                return None;
            }
            if format != ImageFormat::Svg {
                let reader = image::ImageReader::new(Cursor::new(&bytes))
                    .with_guessed_format()
                    .ok()?;
                let (w, h) = reader.into_dimensions().ok()?;
                if w == 0 || h == 0 || w > 2048 || h > 2048 {
                    return None;
                }
            }
            let decoded = Image::from_bytes(format, bytes).to_image_data(svg).ok()?;
            let size = decoded.size(0);
            let pixels = image::RgbaImage::from_raw(
                size.width.0 as u32,
                size.height.0 as u32,
                decoded.as_bytes(0)?.to_vec(),
            )?;
            let mut pixels = image::DynamicImage::ImageRgba8(pixels)
                .thumbnail(64, 64)
                .into_rgba8();
            // The web recolors the source alpha with currentColor; GPUI takes BGRA pixels.
            for pixel in pixels.pixels_mut() {
                pixel.0[..3].copy_from_slice(&[
                    (color >> 8) as u8,
                    (color >> 16) as u8,
                    (color >> 24) as u8,
                ]);
                pixel.0[3] = (u16::from(pixel.0[3]) * u16::from(color as u8) / 255) as u8;
            }
            Some(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
        })
        .await
        .ok()
        .flatten()
    });
    let loaded_key = key.clone();
    let task = cx.spawn(async move |_, cx| {
        let image = driver.await.ok().flatten();
        cx.update(|cx: &mut App| {
            if let Some(slot) = cx.global_mut::<Icons>().slots.get_mut(&loaded_key) {
                slot.image = image;
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
            _task: Some(task),
        },
    );
    None
}
