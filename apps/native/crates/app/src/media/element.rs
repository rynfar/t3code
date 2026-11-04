//! Clip-aware media consumers. Keyed GPUI state unregisters when its element leaves the tree.
use gpui::{AnyElement, App, AvailableSpace, Bounds, Element, ElementId, Entity, GlobalElementId, InspectorElementId, IntoElement, LayoutId, ObjectFit, Pixels, Styled, StyledImage, Window, img, relative};
use t3_core::media::MediaSource;
use super::{ImageState, Media, MediaScope, Variant};

pub struct Consumer(std::rc::Rc<std::cell::Cell<bool>>);

pub struct MediaElement {
    source: MediaSource,
    scope: MediaScope,
    pixels: bool,
}

pub fn image(source: MediaSource, scope: MediaScope) -> MediaElement {
    MediaElement { source, scope, pixels: true }
}

pub fn observer(source: MediaSource, scope: MediaScope) -> MediaElement {
    MediaElement { source, scope, pixels: false }
}

impl IntoElement for MediaElement {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for MediaElement {
    type RequestLayoutState = Entity<Consumer>;
    type PrepaintState = Option<AnyElement>;
    fn id(&self) -> Option<ElementId> { Some("visible-media".into()) }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> { None }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Entity<Consumer>) {
        let consumer = window.use_keyed_state("media-consumer", cx, |_, cx| {
            let id = cx.entity_id().as_u64();
            cx.on_release(move |_, cx| {
                if let Some(cache) = cx.try_global::<Media>().map(|media| media.cache.clone()) {
                    cache.update(cx, |cache, cx| cache.remove_consumer(id, cx));
                }
            }).detach();
            Consumer(Default::default())
        });
        let seen = consumer.read(cx).0.clone();
        seen.set(false);
        let id = consumer.entity_id().as_u64();
        cx.defer(move |cx| {
            if !seen.get() && let Some(cache) = cx.try_global::<Media>().map(|media| media.cache.clone()) {
                cache.update(cx, |cache, cx| cache.remove_consumer(id, cx));
            }
        });
        let mut style = gpui::Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), consumer)
    }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, consumer: &mut Entity<Consumer>, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let cache = cx.try_global::<Media>()?.cache.clone();
        let clip = bounds.intersect(&window.content_mask().bounds);
        if clip.size.width <= gpui::px(0.) || clip.size.height <= gpui::px(0.) {
            cache.update(cx, |cache, cx| cache.remove_consumer(consumer.entity_id().as_u64(), cx));
            return None;
        }
        consumer.read(cx).0.set(true);
        let state = cache.update(cx, |cache, cx| cache.visible_image(&self.source, Variant::Thumb, &self.scope, consumer.entity_id().as_u64(), window, cx));
        let ImageState::Ready(image) = state else { return None };
        if !self.pixels { return None; }
        let mut child = img(image).size_full().object_fit(ObjectFit::Contain).into_any_element();
        child.prepaint_as_root(bounds.origin, bounds.size.map(AvailableSpace::Definite), window, cx);
        Some(child)
    }
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut Entity<Consumer>, child: &mut Option<AnyElement>, window: &mut Window, cx: &mut App) {
        if let Some(child) = child { child.paint(window, cx); }
    }
}
