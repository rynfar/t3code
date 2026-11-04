//! Transcript media: the bounded image cache, the zoomable viewer, and the scope that lets
//! markdown images (drawn by gpui-component, which only hands back `&mut App`) reach both.

mod cache;
mod decode;
mod revisions;
mod viewer;

use gpui::{App, AppContext, Entity, EntityId, Global, WeakEntity};
use t3_core::media::{AssetResource, MediaSource};

pub use cache::{ImageState, MediaCache, Variant};
pub use viewer::{MediaViewer, ViewerItem};

use crate::app_state::AppState;

/// The view drawing media, its thread and image base directory, and its viewer.
#[derive(Clone)]
pub struct MediaScope {
    pub owner: EntityId,
    pub thread_id: Option<String>,
    pub workspace_root: Option<String>,
    pub workspace_mutation_id: Option<String>,
    pub viewer: WeakEntity<MediaViewer>,
}

pub struct Media {
    pub cache: Entity<MediaCache>,
    pub scope: Option<MediaScope>,
}

impl Global for Media {}

/// The app's media cache, created with the first transcript.
pub fn cache(state: &Entity<AppState>, cx: &mut App) -> Entity<MediaCache> {
    if let Some(media) = cx.try_global::<Media>() {
        return media.cache.clone();
    }
    let cache = cx.new(|_| MediaCache::new(state.clone()));
    cx.set_global(Media { cache: cache.clone(), scope: None });
    cache
}

pub fn set_scope(scope: MediaScope, cx: &mut App) {
    if let Some(media) = cx.try_global::<Media>()
        && media.scope.as_ref().is_some_and(|current| {
            current.owner == scope.owner && current.thread_id == scope.thread_id && current.workspace_root == scope.workspace_root && current.workspace_mutation_id == scope.workspace_mutation_id
        })
    {
        return;
    }
    if cx.has_global::<Media>() {
        let cache = cx.global::<Media>().cache.clone();
        cx.global_mut::<Media>().scope = Some(scope.clone());
        cache.update(cx, |cache, cx| cache.refresh_scope(&scope, cx));
    }
}

/// The image for `source`, as drawn by `owner`.
pub fn image(source: &MediaSource, variant: Variant, owner: EntityId, cx: &mut App) -> ImageState {
    let Some(cache) = cx.try_global::<Media>().map(|media| media.cache.clone()) else { return ImageState::Loading };
    let scope = cx.try_global::<Media>().and_then(|media| media.scope.as_ref());
    let revision = scope.and_then(|scope| revision_for(source, scope)).map(str::to_owned);
    cache.update(cx, |cache, cx| cache.image(source, variant, owner, revision.as_deref(), cx))
}

fn revision_for<'a>(source: &MediaSource, scope: &'a MediaScope) -> Option<&'a str> {
    match source {
        MediaSource::Asset(AssetResource::MediaFile { thread_id, .. }) if Some(thread_id) == scope.thread_id.as_ref() => scope.workspace_mutation_id.as_deref(),
        _ => None,
    }
}

pub fn scoped_image(source: &MediaSource, variant: Variant, scope: &MediaScope, cx: &mut App) -> ImageState {
    let Some(cache) = cx.try_global::<Media>().map(|media| media.cache.clone()) else { return ImageState::Loading };
    cache.update(cx, |cache, cx| cache.image(source, variant, scope.owner, revision_for(source, scope), cx))
}

pub fn dimensions(source: &MediaSource, cx: &App) -> Option<(u32, u32)> {
    cx.try_global::<Media>()?.cache.read(cx).dimensions(source, cx)
}

/// Opens an asset in the browser by its signed URL.
pub fn open_asset(resource: &AssetResource, cx: &mut App) {
    if let Some(cache) = cx.try_global::<Media>().map(|media| media.cache.clone()) {
        cache.update(cx, |cache, cx| cache.open(resource, cx));
    }
}

/// Renews only images whose painted bounds intersect the current clip.
pub fn observe_image(source: MediaSource, scope: MediaScope) -> impl gpui::IntoElement {
    use gpui::{ParentElement, Styled};
    gpui::div().absolute().inset_0().child(gpui::canvas(move |bounds, window, cx| {
        let clip = bounds.intersect(&window.content_mask().bounds);
        if clip.size.width > gpui::px(0.) && clip.size.height > gpui::px(0.)
            && let Some(cache) = cx.try_global::<Media>().map(|media| media.cache.clone())
        {
            cache.update(cx, |cache, cx| cache.refresh_visible(&source, scope.owner, window, cx));
        }
    }, |_, _, _, _| {}).size_full())
}
