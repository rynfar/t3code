//! Project favicons for sidebar rows (`ProjectFavicon.tsx`, `client-runtime/src/state/assets.ts`
//! `createProjectFaviconUrlAtomFamily`). Each project's icon is minted and downloaded once per
//! session on the network runtime and held in the bounded `FaviconCache`; GPUI decodes the bytes
//! once and caches the texture.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{Context, Image, ImageFormat};
use t3_core::sidebar::ENVIRONMENT_ID;
use t3_core::sidebar::favicon::{FaviconCache, FaviconFormat, project_favicon_resource_key};

use crate::app_state::{AppEvent, AppState};

#[derive(Default)]
pub struct FaviconStore {
    cache: FaviconCache,
    /// Project id to its cache key and decoded image, kept only while the cache holds the key.
    images: HashMap<String, (String, Arc<Image>)>,
    requested: HashSet<String>,
}

impl FaviconStore {
    pub fn image(&self, project_id: &str) -> Option<Arc<Image>> {
        self.images.get(project_id).map(|(_, image)| image.clone())
    }
}

fn image_format(format: FaviconFormat) -> ImageFormat {
    match format {
        FaviconFormat::Png => ImageFormat::Png,
        FaviconFormat::Jpeg => ImageFormat::Jpeg,
        FaviconFormat::Gif => ImageFormat::Gif,
        FaviconFormat::Webp => ImageFormat::Webp,
        FaviconFormat::Svg => ImageFormat::Svg,
        FaviconFormat::Ico => ImageFormat::Ico,
        FaviconFormat::Bmp => ImageFormat::Bmp,
        FaviconFormat::Tiff => ImageFormat::Tiff,
    }
}


impl AppState {
    /// Starts a fetch for every listed project not asked about yet this session.
    pub(crate) fn ensure_favicons(&mut self, cx: &mut Context<Self>) {
        let (Some(snapshot), Some(client)) = (self.shell.snapshot.as_ref(), self.client_handle()) else { return };
        let wanted: Vec<(String, String)> = snapshot
            .projects
            .iter()
            .filter(|project| !self.sidebar.favicons.requested.contains(&project.id))
            .map(|project| (project.id.clone(), project.workspace_root.clone()))
            .collect();
        for (project_id, cwd) in wanted {
            self.sidebar.favicons.requested.insert(project_id.clone());
            let key = project_favicon_resource_key(ENVIRONMENT_ID, &cwd, None);
            let client = client.clone();
            let task = self.runtime().spawn(async move { t3_client::favicon::fetch_project_favicon(&client, &cwd).await });
            cx.spawn(async move |this, cx| {
                let Ok(Ok(fetched)) = task.await else { return };
                let _ = this.update(cx, |state, cx| {
                    let store = &mut state.sidebar.favicons;
                    match fetched {
                        None => {
                            store.cache.remove(&key);
                            store.images.remove(&project_id);
                        }
                        Some(icon) => {
                            let image = Arc::new(Image::from_bytes(image_format(icon.format), icon.bytes.clone()));
                            if store.cache.insert(key.clone(), icon.revision, icon.bytes, icon.format) {
                                store.images.insert(project_id, (key, image));
                                let cache = &store.cache;
                                store.images.retain(|_, (key, _)| cache.peek(key).is_some());
                            }
                        }
                    }
                    cx.emit(AppEvent::SidebarSelection);
                });
            })
            .detach();
        }
    }
}
