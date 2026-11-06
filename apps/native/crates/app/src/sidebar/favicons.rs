//! Project favicons for sidebar rows (`ProjectFavicon.tsx`, `client-runtime/src/state/assets.ts`
//! `createProjectFaviconUrlAtomFamily`). Each project's icon is minted and downloaded once per
//! session and chosen image (`faviconPath`, which the server resolves) on the network runtime and
//! held in the bounded `FaviconCache`; GPUI decodes the bytes once and caches the texture.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{Context, Image, ImageFormat};
use t3_core::sidebar::ENVIRONMENT_ID;
use t3_core::{settings::projects::icon::Presentation, shell::ProjectShell};
use crate::project_icon::ProjectVisual;
use t3_core::sidebar::favicon::{FaviconCache, FaviconFormat, project_favicon_resource_key};

use crate::app_state::{AppEvent, AppState};

#[derive(Default)]
pub struct FaviconStore {
    cache: FaviconCache,
    presentations: HashMap<String, (String, Option<serde_json::Value>, Arc<Presentation>)>,
    /// Project id to its cache key and decoded image, kept only while the cache holds the key.
    images: HashMap<String, (String, Arc<Image>)>,
    /// Project id to the `faviconPath` its icon was asked for with, so choosing another image
    /// fetches again.
    requested: HashMap<String, Option<String>>,
}

impl FaviconStore {
    pub fn visual(&self, project_id: &str) -> ProjectVisual {
        ProjectVisual { presentation: self.presentations.get(project_id).map(|(_, _, p)| p.clone()), image: self.image(project_id) }
    }

    fn sync_presentations(&mut self, projects: &[ProjectShell]) {
        let listed: HashSet<_> = projects.iter().map(|project| project.id.as_str()).collect();
        self.presentations.retain(|id, _| listed.contains(id.as_str()));
        for project in projects {
            let current = self.presentations.get(&project.id);
            if current.is_some_and(|(title, icon, _)| title == &project.title && icon == &project.project_icon) { continue; }
            let presentation = Presentation::new(project.project_icon.as_ref(), &project.title);
            self.presentations.insert(project.id.clone(), (project.title.clone(), project.project_icon.clone(), Arc::new(presentation)));
        }
    }

    fn image(&self, project_id: &str) -> Option<Arc<Image>> {
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
    /// Starts a fetch for every listed project not asked about yet this session, or whose chosen
    /// image changed since.
    pub(crate) fn ensure_favicons(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.shell.snapshot.as_ref() else { return };
        self.sidebar.favicons.sync_presentations(&snapshot.projects);
        let Some(client) = self.client_handle() else { return };
        let wanted: Vec<(String, String, Option<String>)> = snapshot
            .projects
            .iter()
            .filter(|project| self.sidebar.favicons.presentations.get(&project.id).is_some_and(|(_, _, p)| p.automatic()))
            .filter(|project| self.sidebar.favicons.requested.get(&project.id) != Some(&project.favicon_path))
            .map(|project| (project.id.clone(), project.workspace_root.clone(), project.favicon_path.clone()))
            .collect();
        for (project_id, cwd, favicon_path) in wanted {
            let key = project_favicon_resource_key(ENVIRONMENT_ID, &cwd, favicon_path.as_deref());
            self.sidebar.favicons.requested.insert(project_id.clone(), favicon_path);
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

impl AppState {
    pub(crate) fn ensure_scoped_favicons(&mut self, cx: &mut Context<Self>) {
        let owners = self.fleet.environments.iter().map(|entry| entry.summary.owner.clone()).collect::<HashSet<_>>();
        self.sidebar.owner_favicons.retain(|owner, _| owners.contains(owner));
        let mut wanted = Vec::new();
        for entry in &self.fleet.environments {
            let owner = &entry.summary.owner;
            let projects = entry.summary.projects.iter().map(|project| project.project.clone()).collect::<Vec<_>>();
            let store = self.sidebar.owner_favicons.entry(owner.clone()).or_default();
            store.sync_presentations(&projects);
            for project in projects {
                if !store.presentations.get(&project.id).is_some_and(|(_, _, p)| p.automatic()) || store.requested.get(&project.id) == Some(&project.favicon_path) { continue; }
                wanted.push((owner.clone(), project));
            }
        }
        for (owner, project) in wanted {
            if !self.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) { continue; }
            let resolve = self.connections.destination(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE);
            self.sidebar.owner_favicons.entry(owner.clone()).or_default().requested.insert(project.id.clone(), project.favicon_path.clone());
            let runtime = self.runtime().clone();
            cx.spawn(async move |this, cx| {
                let Ok(target) = resolve.await else { return };
                let root = project.workspace_root.clone();
                let Ok(Ok(fetched)) = runtime.spawn(async move { t3_client::favicon::fetch_project_favicon(&target.client, &root).await }).await else { return };
                let _ = this.update(cx, |state, cx| {
                    let reference = t3_core::sidebar::grouping::ProjectRef { environment_id: owner.saved_id.clone(), project_id: project.id.clone() };
                    if !state.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) || state.scoped_project(&reference).is_none_or(|current| current.project.created_at != project.created_at || current.project.workspace_root != project.workspace_root || current.project.favicon_path != project.favicon_path) { return; }
                    let store = state.sidebar.owner_favicons.entry(owner.clone()).or_default();
                    let key = project_favicon_resource_key(&owner.identity(), &project.workspace_root, project.favicon_path.as_deref());
                    match fetched {
                        None => { store.cache.remove(&key); store.images.remove(&project.id); }
                        Some(icon) => {
                            let image = Arc::new(Image::from_bytes(image_format(icon.format), icon.bytes.clone()));
                            if store.cache.insert(key.clone(), icon.revision, icon.bytes, icon.format) {
                                store.images.insert(project.id.clone(), (key, image));
                                let cache = &store.cache; store.images.retain(|_, (key, _)| cache.peek(key).is_some());
                            }
                        }
                    }
                    cx.emit(AppEvent::SidebarSelection);
                });
            }).detach();
        }
    }
}
