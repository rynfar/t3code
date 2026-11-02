//! Transcript media: the bounded image cache, the zoomable viewer, and the scope that lets
//! markdown images (drawn by gpui-component, which only hands back `&mut App`) reach both.

mod cache;
mod decode;
mod viewer;

use gpui::{App, AppContext, Entity, EntityId, Global, WeakEntity};
use t3_core::media::MediaSource;

pub use cache::{ImageState, MediaCache, Variant};
pub use viewer::{MediaViewer, ViewerItem};

use crate::app_state::AppState;

/// Who draws transcript media right now: the transcript view, its thread, and its viewer.
/// Media outside that view (markdown in other panels) draws as a plain link instead, since only
/// the transcript stamps the frames the cache evicts by.
#[derive(Clone)]
pub struct MediaScope {
    pub owner: EntityId,
    pub thread_id: Option<String>,
    pub workspace_root: Option<String>,
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
            current.owner == scope.owner && current.thread_id == scope.thread_id && current.workspace_root == scope.workspace_root
        })
    {
        return;
    }
    if cx.has_global::<Media>() {
        cx.global_mut::<Media>().scope = Some(scope);
    }
}

/// The image for `source`, as drawn by `owner`.
pub fn image(source: &MediaSource, variant: Variant, owner: EntityId, cx: &mut App) -> ImageState {
    let Some(cache) = cx.try_global::<Media>().map(|media| media.cache.clone()) else { return ImageState::Loading };
    cache.update(cx, |cache, cx| cache.image(source, variant, owner, cx))
}

pub fn dimensions(source: &MediaSource, cx: &App) -> Option<(u32, u32)> {
    cx.try_global::<Media>()?.cache.read(cx).dimensions(source, cx)
}
