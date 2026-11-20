//! GPUI `ThemeSlot` application engine for compiled `THEME_CSS_HOOKS` overrides.
//!
//! Reads the pre-compiled `Arc<SlotStylesheet>` stored in GPUI global state (zero per-frame CSS
//! parsing) and refines `StyleRefinement`s directly with a zero-allocation fast path when no
//! custom CSS slot overrides are active.

// Part 1 of #457 exposes the slot engine helpers for Wave H4B's 29 UI call sites.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use gpui::{
    AbsoluteLength, AlignItems, AnyElement, App, BorderStyle, Bounds, BoxShadow, Corners,
    DefiniteLength, Display, Edges, Fill, FlexDirection, FontWeight, Global, InteractiveElement,
    IntoElement, JustifyContent, Length, ObjectFit, ParentElement, Pixels, RenderImage, Size,
    StatefulInteractiveElement, StyleRefinement, Styled, StyledImage, Window, canvas, div, img,
    linear_color_stop, linear_gradient, point, px, quad, relative, rems, size, solid_background,
    transparent_black,
};
use t3_core::client_settings::typography::{FontDefaults, resolve_font_stack};
use t3_core::themes::slots::{
    BackgroundImageSize, GradientStop, PILL_RADIUS_PX, SlotAlignItems, SlotBackground,
    SlotFlexDirection, SlotJustifyContent, SlotLength, SlotLineHeight, SlotSizeConstraint,
};
pub use t3_core::themes::slots::{
    SlotPseudoBar, SlotState, SlotStyleOverride, SlotStyleSet, SlotStylesheet, ThemeSlot,
};

use super::{FontInventory, background_image, hsla};

pub(super) struct ActiveSlotStylesheet(pub(super) Arc<SlotStylesheet>);
impl Global for ActiveSlotStylesheet {}

const GRADIENT_TEX_SIZE: u32 = 128;
const GRADIENT_MAX_DIM: u32 = 128;
const GRADIENT_MIN_DIM: u32 = 16;
const GRADIENT_GUTTER: u32 = 2;
const MAX_CACHED_GRADIENTS: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum GradientCacheKey {
    Linear {
        aspect_bucket: i16,
        angle_bits: u32,
        stops: Vec<(u32, u32)>,
    },
    Radial {
        aspect_bucket: i16,
        cx_bits: u32,
        cy_bits: u32,
        rx_bits: u32,
        ry_bits: u32,
        stops: Vec<(u32, u32)>,
    },
}

#[derive(Clone)]
struct RasterizedGradient {
    image: Arc<RenderImage>,
    inner_w: u32,
    inner_h: u32,
}

#[derive(Default)]
struct GradientCache {
    entries: HashMap<GradientCacheKey, RasterizedGradient>,
    pending_drop: Vec<Arc<RenderImage>>,
}

static GRADIENT_CACHE: LazyLock<Mutex<GradientCache>> =
    LazyLock::new(|| Mutex::new(GradientCache::default()));

/// Clears cached rasterized gradient textures whenever the active theme slot stylesheet changes.
pub(super) fn clear_gradient_cache() {
    if let Ok(mut cache) = GRADIENT_CACHE.lock() {
        let evicted: Vec<_> = cache.entries.drain().map(|(_, v)| v.image).collect();
        cache.pending_drop.extend(evicted);
    }
}

fn drain_dropped_gradients(window: &mut Window) {
    let pending = match GRADIENT_CACHE.lock() {
        Ok(mut cache) if !cache.pending_drop.is_empty() => std::mem::take(&mut cache.pending_drop),
        _ => return,
    };
    for image in pending {
        let _ = window.drop_image(image);
    }
}

fn all_stops_equal(stops: &[GradientStop]) -> Option<u32> {
    let first = stops.first()?.color;
    stops.iter().all(|s| s.color == first).then_some(first)
}

fn sample_gradient_stops(stops: &[GradientStop], t: f32) -> [u8; 4] {
    if stops.is_empty() {
        return [0, 0, 0, 0];
    }
    let unpack = |packed: u32| -> [f32; 4] {
        let r = ((packed >> 24) & 0xff) as f32 / 255.0;
        let g = ((packed >> 16) & 0xff) as f32 / 255.0;
        let b = ((packed >> 8) & 0xff) as f32 / 255.0;
        let a = (packed & 0xff) as f32 / 255.0;
        [r, g, b, a]
    };
    let to_bgra = |[r, g, b, a]: [f32; 4]| -> [u8; 4] {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        // GPUI's atlas expects straight-alpha BGRA8 bytes.
        [c(b), c(g), c(r), c(a)]
    };

    let first_pos = stops[0].position.unwrap_or(0.0);
    if t <= first_pos || stops.len() == 1 {
        return to_bgra(unpack(stops[0].color));
    }
    let last = &stops[stops.len() - 1];
    let last_pos = last.position.unwrap_or(1.0);
    if t >= last_pos {
        return to_bgra(unpack(last.color));
    }

    for pair in stops.windows(2) {
        let p0 = pair[0].position.unwrap_or(0.0);
        let p1 = pair[1].position.unwrap_or(1.0);
        if t <= p1 {
            let span = (p1 - p0).max(1e-6);
            let frac = ((t - p0) / span).clamp(0.0, 1.0);
            let c0 = unpack(pair[0].color);
            let c1 = unpack(pair[1].color);
            // CSS gradients interpolate in premultiplied RGBA so fading to `transparent` stays pure.
            let a = c0[3] + (c1[3] - c0[3]) * frac;
            if a <= 1e-6 {
                return [0, 0, 0, 0];
            }
            let pr = c0[0] * c0[3] + (c1[0] * c1[3] - c0[0] * c0[3]) * frac;
            let pg = c0[1] * c0[3] + (c1[1] * c1[3] - c0[1] * c0[3]) * frac;
            let pb = c0[2] * c0[3] + (c1[2] * c1[3] - c0[2] * c0[3]) * frac;
            return to_bgra([pr / a, pg / a, pb / a, a]);
        }
    }
    to_bgra(unpack(last.color))
}

fn aspect_bucket_and_dims(width_px: f32, height_px: f32) -> (i16, f32, u32, u32) {
    let ratio = (width_px.max(1.0) / height_px.max(1.0)).clamp(0.05, 20.0);
    let bucket = (ratio.ln() * 16.0).round() as i16;
    if bucket == 0 {
        return (0, 1.0, GRADIENT_TEX_SIZE, GRADIENT_TEX_SIZE);
    }
    let q_ratio = (bucket as f32 / 16.0).exp();
    let (tex_w, tex_h) = if q_ratio >= 1.0 {
        let h = (GRADIENT_MAX_DIM as f32 / q_ratio)
            .round()
            .clamp(GRADIENT_MIN_DIM as f32, GRADIENT_MAX_DIM as f32) as u32;
        (GRADIENT_MAX_DIM, h)
    } else {
        let w = (GRADIENT_MAX_DIM as f32 * q_ratio)
            .round()
            .clamp(GRADIENT_MIN_DIM as f32, GRADIENT_MAX_DIM as f32) as u32;
        (w, GRADIENT_MAX_DIM)
    };
    (bucket, q_ratio, tex_w, tex_h)
}

fn gradient_cache_key(bg: &SlotBackground, aspect_bucket: i16) -> Option<GradientCacheKey> {
    match bg {
        SlotBackground::LinearGradient { angle_deg, stops } if stops.len() >= 2 => {
            Some(GradientCacheKey::Linear {
                aspect_bucket,
                angle_bits: angle_deg.to_bits(),
                stops: stops
                    .iter()
                    .map(|s| (s.color, s.position.unwrap_or(0.0).to_bits()))
                    .collect(),
            })
        }
        SlotBackground::RadialGradient { cx_frac, cy_frac, rx_frac, ry_frac, stops }
            if stops.len() >= 2 =>
        {
            Some(GradientCacheKey::Radial {
                aspect_bucket,
                cx_bits: cx_frac.to_bits(),
                cy_bits: cy_frac.to_bits(),
                rx_bits: rx_frac.to_bits(),
                ry_bits: ry_frac.to_bits(),
                stops: stops
                    .iter()
                    .map(|s| (s.color, s.position.unwrap_or(0.0).to_bits()))
                    .collect(),
            })
        }
        _ => None,
    }
}

fn rasterize_gradient_for_size(
    bg: &SlotBackground,
    width_px: f32,
    height_px: f32,
) -> Option<RasterizedGradient> {
    let (bucket, q_ratio, inner_w, inner_h) = aspect_bucket_and_dims(width_px, height_px);
    let key = gradient_cache_key(bg, bucket)?;
    if let Ok(cache) = GRADIENT_CACHE.lock()
        && let Some(hit) = cache.entries.get(&key)
    {
        return Some(hit.clone());
    }

    // GPUI packs sprite tiles with 0 atlas padding and linear sampling across [0, 1], so surround
    // the gradient with a clamped 2-texel gutter and crop it cleanly via `paint_image_fitted`.
    let tex_w = inner_w + GRADIENT_GUTTER * 2;
    let tex_h = inner_h + GRADIENT_GUTTER * 2;
    let inv_w = 1.0 / inner_w as f32;
    let inv_h = 1.0 / inner_h as f32;
    let clamp_u = |x: u32| -> f32 {
        let ix = x.clamp(GRADIENT_GUTTER, inner_w + GRADIENT_GUTTER - 1) - GRADIENT_GUTTER;
        (ix as f32 + 0.5) * inv_w
    };
    let clamp_v = |y: u32| -> f32 {
        let iy = y.clamp(GRADIENT_GUTTER, inner_h + GRADIENT_GUTTER - 1) - GRADIENT_GUTTER;
        (iy as f32 + 0.5) * inv_h
    };
    let mut pixels = image::RgbaImage::new(tex_w, tex_h);

    match bg {
        SlotBackground::LinearGradient { angle_deg, stops } => {
            let rad = angle_deg.to_radians();
            let dx = rad.sin();
            let dy = -rad.cos();
            let w = q_ratio;
            let h = 1.0f32;
            let line_len = (w * dx.abs() + h * dy.abs()).max(1e-6);
            for y in 0..tex_h {
                let py = (clamp_v(y) - 0.5) * h;
                for x in 0..tex_w {
                    let px_x = (clamp_u(x) - 0.5) * w;
                    let t = ((px_x * dx + py * dy) / line_len + 0.5).clamp(0.0, 1.0);
                    pixels.put_pixel(x, y, image::Rgba(sample_gradient_stops(stops, t)));
                }
            }
        }
        SlotBackground::RadialGradient { cx_frac, cy_frac, rx_frac, ry_frac, stops } => {
            let rx = rx_frac.max(1e-4);
            let ry = ry_frac.max(1e-4);
            for y in 0..tex_h {
                let ny = (clamp_v(y) - *cy_frac) / ry;
                for x in 0..tex_w {
                    let nx = (clamp_u(x) - *cx_frac) / rx;
                    let t = (nx * nx + ny * ny).sqrt();
                    pixels.put_pixel(x, y, image::Rgba(sample_gradient_stops(stops, t)));
                }
            }
        }
        _ => return None,
    }

    let rendered = RasterizedGradient {
        image: Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])),
        inner_w,
        inner_h,
    };
    if let Ok(mut cache) = GRADIENT_CACHE.lock() {
        if cache.entries.len() >= MAX_CACHED_GRADIENTS {
            let evicted: Vec<_> = cache.entries.drain().map(|(_, v)| v.image).collect();
            cache.pending_drop.extend(evicted);
        }
        cache.entries.insert(key, rendered.clone());
    }
    Some(rendered)
}

fn rasterize_gradient(bg: &SlotBackground) -> Option<RasterizedGradient> {
    rasterize_gradient_for_size(bg, GRADIENT_TEX_SIZE as f32, GRADIENT_TEX_SIZE as f32)
}

fn fitted_bounds_for_gutter(
    visible: Bounds<Pixels>,
    inner_w: u32,
    inner_h: u32,
) -> Bounds<Pixels> {
    let pad_x = f32::from(visible.size.width) * (GRADIENT_GUTTER as f32 / inner_w.max(1) as f32);
    let pad_y = f32::from(visible.size.height) * (GRADIENT_GUTTER as f32 / inner_h.max(1) as f32);
    Bounds::new(
        point(visible.origin.x - px(pad_x), visible.origin.y - px(pad_y)),
        size(
            visible.size.width + px(2.0 * pad_x),
            visible.size.height + px(2.0 * pad_y),
        ),
    )
}

fn override_uses_background_layer(ov: &SlotStyleOverride, base: Option<&SlotStyleOverride>) -> bool {
    let blur = ov
        .backdrop_blur
        .or_else(|| base.and_then(|b| b.backdrop_blur))
        .is_some_and(|b| b > 0.0);
    let has_fill = ov.background.is_some() || ov.background_color.is_some();
    ov.border_gradient.is_some()
        || matches!(ov.background, Some(SlotBackground::RadialGradient { .. }))
        || matches!(
            &ov.background,
            Some(SlotBackground::LinearGradient { stops, .. })
                if ov.background_color.is_some()
                    || (stops.len() >= 3 && all_stops_equal(stops).is_none())
        )
        || (blur && has_fill)
}

fn override_uses_raster_or_rim(ov: &SlotStyleOverride) -> bool {
    override_uses_background_layer(ov, None)
}

fn override_has_live_background(ov: &SlotStyleOverride, has_wallpaper: bool) -> bool {
    if ov.border_gradient.is_some() {
        return true;
    }
    match ov.background.as_ref() {
        Some(SlotBackground::ThemeBackgroundImage { .. }) => has_wallpaper,
        Some(
            SlotBackground::Solid(_)
            | SlotBackground::LinearGradient { .. }
            | SlotBackground::RadialGradient { .. }
            | SlotBackground::Transparent,
        ) => true,
        None => ov.background_color.is_some(),
    }
}

/// Returns the active compiled `SlotStylesheet` for the current theme and appearance mode.
pub fn slot_stylesheet(cx: &App) -> Arc<SlotStylesheet> {
    cx.try_global::<ActiveSlotStylesheet>()
        .map(|s| Arc::clone(&s.0))
        .unwrap_or_else(|| Arc::new(SlotStylesheet::empty()))
}

/// Fast O(1) lookup for a slot's compiled overrides. Returns `None` when the active stylesheet
/// has no rules targeting `slot` (including all stock themes).
pub fn slot_style(slot: ThemeSlot, cx: &App) -> Option<SlotStyleSet> {
    let sheet = &cx.try_global::<ActiveSlotStylesheet>()?.0;
    if sheet.is_empty() {
        return None;
    }
    sheet.get(slot).cloned()
}

/// Zero-allocation check for whether the active stylesheet defines `:hover` overrides for `slot`.
pub fn slot_has_hover(slot: ThemeSlot, cx: &App) -> bool {
    cx.try_global::<ActiveSlotStylesheet>()
        .is_some_and(|g| !g.0.is_empty() && g.0.get(slot).is_some_and(|set| !set.hover.is_empty()))
}

/// Zero-allocation check for whether the active stylesheet defines `:active` overrides for `slot`.
pub fn slot_has_active(slot: ThemeSlot, cx: &App) -> bool {
    cx.try_global::<ActiveSlotStylesheet>()
        .is_some_and(|g| !g.0.is_empty() && g.0.get(slot).is_some_and(|set| !set.active.is_empty()))
}

/// Zero-allocation check for whether the active stylesheet defines a live base `background`
/// override for `slot` (used by glass overlays to suppress `glass::backdrop` only when a visible
/// custom background will actually paint). Returns `false` for `ThemeBackgroundImage` when no
/// background image is currently loaded so overlays fall back to stock glass rather than turning
/// transparent.
pub fn slot_has_background(slot: ThemeSlot, cx: &App) -> bool {
    slot_has_state_background(slot, SlotState::Base, cx)
}

/// Zero-allocation check for whether `slot` defines a live background (including `border_gradient`,
/// `RadialGradient`, multi-stop `LinearGradient`, or loaded wallpaper) in `state` or `Base`.
pub fn slot_has_state_background(slot: ThemeSlot, state: SlotState, cx: &App) -> bool {
    let Some(g) = cx.try_global::<ActiveSlotStylesheet>() else {
        return false;
    };
    if g.0.is_empty() {
        return false;
    }
    let Some(set) = g.0.get(slot) else {
        return false;
    };
    let has_wallpaper = g.0.has_scoped_background_image() && background_image(cx).is_some();
    if state != SlotState::Base && override_has_live_background(set.state(state), has_wallpaper) {
        return true;
    }
    override_has_live_background(&set.base, has_wallpaper)
}

/// True when `slot` defines base margins or `calc(100% - <px>)` dimensions (e.g. a floating
/// inset sidebar card that should size with `.flex_1()` instead of `.size_full()`).
pub fn slot_has_floating_inset(slot: ThemeSlot, cx: &App) -> bool {
    let Some(g) = cx.try_global::<ActiveSlotStylesheet>() else {
        return false;
    };
    if g.0.is_empty() {
        return false;
    }
    let Some(set) = g.0.get(slot) else {
        return false;
    };
    !set.base.margin.is_empty()
        || matches!(set.base.width, Some(SlotLength::FullMinusPx(_)))
        || matches!(set.base.height, Some(SlotLength::FullMinusPx(_)))
}

/// Zero-allocation lookup for a slot's compiled base `border-color` override, if any.
pub fn slot_border_color(slot: ThemeSlot, cx: &App) -> Option<gpui::Hsla> {
    let g = cx.try_global::<ActiveSlotStylesheet>()?;
    if g.0.is_empty() {
        return None;
    }
    g.0.get(slot)?.base.border_color.map(hsla)
}

/// Applies a compiled [`SlotStyleOverride`] directly onto a GPUI [`StyleRefinement`].
pub fn apply_slot_override(style: &mut StyleRefinement, ov: &SlotStyleOverride, cx: &App) {
    apply_slot_override_with_base(style, ov, None, cx);
}

fn apply_slot_override_with_base(
    style: &mut StyleRefinement,
    ov: &SlotStyleOverride,
    base: Option<&SlotStyleOverride>,
    cx: &App,
) {
    if ov.is_empty() {
        return;
    }
    if let Some(display_none) = ov.display_none {
        if display_none {
            style.display = Some(Display::None);
        } else if style.display == Some(Display::None) {
            style.display = Some(Display::Flex);
        }
    }
    let uses_bg_layer = override_uses_background_layer(ov, base);
    if uses_bg_layer {
        // Let the rendered gradient / backdrop-blur / border-gradient canvas layer paint cleanly
        // without the host element's own background washing it out or double-tinting blurs.
        style.background = Some(Fill::Color(solid_background(transparent_black())));
    } else if let Some(bg) = &ov.background {
        match bg {
            SlotBackground::Solid(c) => {
                style.background = Some(Fill::Color(solid_background(hsla(*c))));
            }
            SlotBackground::Transparent => {
                style.background = Some(Fill::Color(solid_background(transparent_black())));
            }
            SlotBackground::LinearGradient { angle_deg, stops } => {
                if let Some(uniform) = all_stops_equal(stops) {
                    style.background = Some(Fill::Color(solid_background(hsla(uniform))));
                } else if let (Some(first), Some(last)) = (stops.first(), stops.last()) {
                    let from = linear_color_stop(hsla(first.color), first.position.unwrap_or(0.0));
                    let to = linear_color_stop(hsla(last.color), last.position.unwrap_or(1.0));
                    style.background = Some(Fill::Color(linear_gradient(*angle_deg, from, to)));
                }
            }
            SlotBackground::RadialGradient { .. } | SlotBackground::ThemeBackgroundImage { .. } => {
                // Rendered by `theme_slot_state_background_layer`.
            }
        }
    } else if let Some(c) = ov.background_color {
        style.background = Some(Fill::Color(solid_background(hsla(c))));
    }
    if let Some(color) = ov.color {
        style.text.color = Some(hsla(color));
    }
    if let Some(border_color) = ov.border_color {
        style.border_color = Some(hsla(border_color));
    }
    if let Some(w) = ov.border_widths.top {
        style.border_widths.top = Some(px(w).into());
    }
    if let Some(w) = ov.border_widths.right {
        style.border_widths.right = Some(px(w).into());
    }
    if let Some(w) = ov.border_widths.bottom {
        style.border_widths.bottom = Some(px(w).into());
    }
    if let Some(w) = ov.border_widths.left {
        style.border_widths.left = Some(px(w).into());
    }
    if let Some(r) = ov.border_radii.top_left.and_then(to_radius_length) {
        style.corner_radii.top_left = Some(r);
    }
    if let Some(r) = ov.border_radii.top_right.and_then(to_radius_length) {
        style.corner_radii.top_right = Some(r);
    }
    if let Some(r) = ov.border_radii.bottom_right.and_then(to_radius_length) {
        style.corner_radii.bottom_right = Some(r);
    }
    if let Some(r) = ov.border_radii.bottom_left.and_then(to_radius_length) {
        style.corner_radii.bottom_left = Some(r);
    }
    if let Some(v) = ov.padding.top.and_then(to_definite_length) {
        style.padding.top = Some(v);
    }
    if let Some(v) = ov.padding.right.and_then(to_definite_length) {
        style.padding.right = Some(v);
    }
    if let Some(v) = ov.padding.bottom.and_then(to_definite_length) {
        style.padding.bottom = Some(v);
    }
    if let Some(v) = ov.padding.left.and_then(to_definite_length) {
        style.padding.left = Some(v);
    }
    if let Some(v) = ov.margin.top.map(to_length) {
        style.margin.top = Some(v);
    }
    if let Some(v) = ov.margin.right.map(to_length) {
        style.margin.right = Some(v);
    }
    if let Some(v) = ov.margin.bottom.map(to_length) {
        style.margin.bottom = Some(v);
    }
    if let Some(v) = ov.margin.left.map(to_length) {
        style.margin.left = Some(v);
    }
    if let Some(v) = ov.gap.and_then(to_definite_length) {
        style.gap.width = Some(v);
        style.gap.height = Some(v);
    }
    if let Some(v) = ov.row_gap.and_then(to_definite_length) {
        style.gap.height = Some(v);
    }
    if let Some(v) = ov.column_gap.and_then(to_definite_length) {
        style.gap.width = Some(v);
    }
    if let Some(v) = ov.width.map(to_length) {
        style.size.width = Some(v);
    }
    if let Some(v) = ov.height.map(to_length) {
        style.size.height = Some(v);
    }
    if let Some(v) = ov.min_width.map(to_length) {
        style.min_size.width = Some(v);
    }
    if let Some(v) = ov.min_height.map(to_length) {
        style.min_size.height = Some(v);
    }
    if let Some(v) = ov.max_width.map(to_constraint_length) {
        style.max_size.width = Some(v);
    }
    if let Some(v) = ov.max_height.map(to_constraint_length) {
        style.max_size.height = Some(v);
    }
    if let Some(shadows) = &ov.box_shadows {
        style.box_shadow = Some(
            shadows
                .iter()
                // Inset shadows must paint above the canvas fill layer rather than behind it.
                .filter(|s| !uses_bg_layer || !s.inset)
                .map(|s| BoxShadow {
                    color: hsla(s.color),
                    offset: point(px(s.offset_x), px(s.offset_y)),
                    blur_radius: px(s.blur_radius),
                    spread_radius: px(s.spread_radius),
                    inset: s.inset,
                })
                .collect(),
        );
    } else if uses_bg_layer && let Some(existing) = &mut style.box_shadow {
        existing.retain(|s| !s.inset);
    }
    if let Some(opacity) = ov.opacity {
        style.opacity = Some(opacity);
    }
    if let Some(family) = &ov.font_family {
        let resolved = resolve_slot_font_family(family, cx);
        style.text.font_family = Some(resolved);
    }
    if let Some(font_size) = ov.font_size {
        style.text.font_size = Some(px(font_size).into());
    }
    if let Some(font_weight) = ov.font_weight {
        style.text.font_weight = Some(FontWeight(f32::from(font_weight)));
    }
    if let Some(line_height) = ov.line_height {
        let lh = match line_height {
            SlotLineHeight::Px(v) => px(v).into(),
            SlotLineHeight::Rem(v) => rems(v).into(),
            SlotLineHeight::Relative(r) => relative(r),
        };
        style.text.line_height = Some(lh);
    }
    if let Some(dir) = ov.flex_direction {
        style.flex_direction = Some(match dir {
            SlotFlexDirection::Row => FlexDirection::Row,
            SlotFlexDirection::Column => FlexDirection::Column,
            SlotFlexDirection::RowReverse => FlexDirection::RowReverse,
            SlotFlexDirection::ColumnReverse => FlexDirection::ColumnReverse,
        });
    }
    if let Some(align) = ov.align_items {
        style.align_items = Some(match align {
            SlotAlignItems::Start => AlignItems::Start,
            SlotAlignItems::End => AlignItems::End,
            SlotAlignItems::FlexStart => AlignItems::FlexStart,
            SlotAlignItems::FlexEnd => AlignItems::FlexEnd,
            SlotAlignItems::Center => AlignItems::Center,
            SlotAlignItems::Baseline => AlignItems::Baseline,
            SlotAlignItems::Stretch => AlignItems::Stretch,
        });
    }
    if let Some(justify) = ov.justify_content {
        style.justify_content = Some(match justify {
            SlotJustifyContent::Start => JustifyContent::Start,
            SlotJustifyContent::End => JustifyContent::End,
            SlotJustifyContent::FlexStart => JustifyContent::FlexStart,
            SlotJustifyContent::FlexEnd => JustifyContent::FlexEnd,
            SlotJustifyContent::Center => JustifyContent::Center,
            SlotJustifyContent::Stretch => JustifyContent::Stretch,
            SlotJustifyContent::SpaceBetween => JustifyContent::SpaceBetween,
            SlotJustifyContent::SpaceAround => JustifyContent::SpaceAround,
            SlotJustifyContent::SpaceEvenly => JustifyContent::SpaceEvenly,
        });
    }
}

/// Composes `slot`'s `:hover` CSS overrides onto an existing hover `StyleRefinement` (safe for
/// elements that already register their own `.hover(...)` block).
pub fn refine_slot_hover(mut style: StyleRefinement, slot: ThemeSlot, cx: &App) -> StyleRefinement {
    let Some(global) = cx.try_global::<ActiveSlotStylesheet>() else {
        return style;
    };
    if global.0.is_empty() {
        return style;
    }
    let sheet = Arc::clone(&global.0);
    if let Some(set) = sheet.get(slot)
        && !set.hover.is_empty()
    {
        apply_slot_override(&mut style, &set.hover, cx);
    }
    style
}

/// Composes `slot`'s `:active` CSS overrides onto an existing active `StyleRefinement`.
pub fn refine_slot_active(mut style: StyleRefinement, slot: ThemeSlot, cx: &App) -> StyleRefinement {
    let Some(global) = cx.try_global::<ActiveSlotStylesheet>() else {
        return style;
    };
    if global.0.is_empty() {
        return style;
    }
    let sheet = Arc::clone(&global.0);
    if let Some(set) = sheet.get(slot)
        && !set.active.is_empty()
    {
        apply_slot_override(&mut style, &set.active, cx);
    }
    style
}

/// Applies base or state-specific `ThemeSlot` CSS overrides onto any GPUI `Styled` element.
pub trait ThemeSlotExt: Styled + Sized {
    /// Applies compiled base overrides for `slot`. Fast-path no-op when no overrides exist.
    fn theme_slot(mut self, slot: ThemeSlot, cx: &App) -> Self {
        let Some(global) = cx.try_global::<ActiveSlotStylesheet>() else {
            return self;
        };
        if global.0.is_empty() {
            return self;
        }
        let sheet = Arc::clone(&global.0);
        if let Some(set) = sheet.get(slot)
            && !set.base.is_empty()
        {
            apply_slot_override(self.style(), &set.base, cx);
        }
        self
    }

    /// Applies compiled overrides for `slot` in `state` (e.g. `SlotState::Selected`,
    /// `SlotState::Unchecked`, `SlotState::FocusWithin`). Fast-path no-op when no overrides exist.
    fn theme_slot_state(mut self, slot: ThemeSlot, state: SlotState, cx: &App) -> Self {
        let Some(global) = cx.try_global::<ActiveSlotStylesheet>() else {
            return self;
        };
        if global.0.is_empty() {
            return self;
        }
        let sheet = Arc::clone(&global.0);
        if let Some(set) = sheet.get(slot) {
            let ov = set.state(state);
            if !ov.is_empty() {
                apply_slot_override_with_base(self.style(), ov, Some(&set.base), cx);
            }
        }
        self
    }

    /// Applies compiled overrides for `[data-timeline-row-kind]` and `[data-timeline-row-kind="<kind>"]`.
    fn theme_slot_timeline_row(mut self, kind: &str, cx: &App) -> Self {
        let Some(global) = cx.try_global::<ActiveSlotStylesheet>() else {
            return self;
        };
        if global.0.is_empty() {
            return self;
        }
        let sheet = Arc::clone(&global.0);
        if let Some(set) = sheet.timeline_row(kind)
            && !set.base.is_empty()
        {
            apply_slot_override(self.style(), &set.base, cx);
        }
        self
    }
}

impl<E: Styled + Sized> ThemeSlotExt for E {}

/// Applies base and `:hover` `ThemeSlot` CSS overrides onto any `InteractiveElement + Styled`.
///
/// Note: If the element already calls `.hover(...)`, compose via [`ThemeSlotExt::theme_slot`] plus
/// [`refine_slot_hover`] inside the existing `.hover(...)` closure to avoid GPUI's duplicate hover
/// debug assertion.
pub trait ThemeSlotHoverExt: InteractiveElement + Styled + Sized {
    fn theme_slot_hover(mut self, slot: ThemeSlot, cx: &App) -> Self {
        let Some(global) = cx.try_global::<ActiveSlotStylesheet>() else {
            return self;
        };
        if global.0.is_empty() {
            return self;
        }
        let sheet = Arc::clone(&global.0);
        let Some(set) = sheet.get(slot) else {
            return self;
        };
        if !set.base.is_empty() {
            apply_slot_override(self.style(), &set.base, cx);
        }
        if !set.hover.is_empty() {
            self = self.hover(|mut style| {
                apply_slot_override(&mut style, &set.hover, cx);
                style
            });
        }
        self
    }
}

impl<E: InteractiveElement + Styled + Sized> ThemeSlotHoverExt for E {}

/// Applies base, `:hover`, and `:active` `ThemeSlot` CSS overrides onto any stateful interactive
/// GPUI element (e.g. `div().id(...)`).
///
/// Registers `.hover(...)` and `.active(...)` ONLY when the compiled stylesheet actually defines
/// `:hover` / `:active` overrides for `slot`. If the element already calls `.hover(...)`, compose
/// via [`ThemeSlotExt::theme_slot`] + [`refine_slot_hover`] + [`refine_slot_active`].
pub trait ThemeSlotInteractiveExt: StatefulInteractiveElement + Styled + Sized {
    fn theme_slot_interactive(mut self, slot: ThemeSlot, cx: &App) -> Self {
        let Some(global) = cx.try_global::<ActiveSlotStylesheet>() else {
            return self;
        };
        if global.0.is_empty() {
            return self;
        }
        let sheet = Arc::clone(&global.0);
        let Some(set) = sheet.get(slot) else {
            return self;
        };
        if !set.base.is_empty() {
            apply_slot_override(self.style(), &set.base, cx);
        }
        if !set.hover.is_empty() {
            self = self.hover(|mut style| {
                apply_slot_override(&mut style, &set.hover, cx);
                style
            });
        }
        if !set.active.is_empty() {
            self = self.active(|mut style| {
                apply_slot_override(&mut style, &set.active, cx);
                style
            });
        }
        self
    }
}

impl<E: StatefulInteractiveElement + Styled + Sized> ThemeSlotInteractiveExt for E {}

fn absolute_length_px(len: AbsoluteLength, rem_px: f32) -> f32 {
    match len {
        AbsoluteLength::Pixels(p) => f32::from(p),
        AbsoluteLength::Rems(r) => r.0 * rem_px,
    }
}

fn slot_radius_px(len: Option<SlotLength>, fallback: f32, rem_px: f32) -> f32 {
    match len {
        Some(SlotLength::Px(v)) => v.max(0.0),
        Some(SlotLength::Rem(v)) => (v * rem_px).max(0.0),
        Some(SlotLength::Fraction(f)) => {
            if f >= 0.5 {
                PILL_RADIUS_PX
            } else if f <= 0.0 {
                0.0
            } else {
                (f * 32.0).clamp(0.0, PILL_RADIUS_PX)
            }
        }
        Some(SlotLength::FullMinusPx(_) | SlotLength::Auto) | None => fallback.max(0.0),
    }
}

fn resolve_radii_px(
    ov: &SlotStyleOverride,
    base: &SlotStyleOverride,
    default_radius: AbsoluteLength,
    cx: &App,
) -> [f32; 4] {
    let rem_px = super::preferences(cx).interface_font_size.max(8.0);
    let default_px = absolute_length_px(default_radius, rem_px);
    let tl = slot_radius_px(ov.border_radii.top_left.or(base.border_radii.top_left), default_px, rem_px);
    let tr = slot_radius_px(ov.border_radii.top_right.or(base.border_radii.top_right), default_px, rem_px);
    let br = slot_radius_px(
        ov.border_radii.bottom_right.or(base.border_radii.bottom_right),
        default_px,
        rem_px,
    );
    let bl = slot_radius_px(
        ov.border_radii.bottom_left.or(base.border_radii.bottom_left),
        default_px,
        rem_px,
    );
    [tl, tr, br, bl]
}

fn resolve_border_inset_px(ov: &SlotStyleOverride, base: &SlotStyleOverride) -> f32 {
    let pick = |a: Option<f32>, b: Option<f32>| a.or(b);
    let edges = [
        pick(ov.border_widths.top, base.border_widths.top),
        pick(ov.border_widths.right, base.border_widths.right),
        pick(ov.border_widths.bottom, base.border_widths.bottom),
        pick(ov.border_widths.left, base.border_widths.left),
    ];
    edges
        .into_iter()
        .flatten()
        .fold(0.0f32, f32::max)
        .max(1.0)
}

fn resolve_border_edges(ov: &SlotStyleOverride, base: &SlotStyleOverride) -> [Option<f32>; 4] {
    [
        ov.border_widths.top.or(base.border_widths.top),
        ov.border_widths.right.or(base.border_widths.right),
        ov.border_widths.bottom.or(base.border_widths.bottom),
        ov.border_widths.left.or(base.border_widths.left),
    ]
}

fn expand_bounds(bounds: Bounds<Pixels>, edges: [f32; 4]) -> Bounds<Pixels> {
    Bounds::new(
        point(bounds.origin.x - px(edges[3]), bounds.origin.y - px(edges[0])),
        size(
            (bounds.size.width + px(edges[1] + edges[3])).max(px(0.0)),
            (bounds.size.height + px(edges[0] + edges[2])).max(px(0.0)),
        ),
    )
}

fn inset_bounds(bounds: Bounds<Pixels>, edges: [f32; 4]) -> Bounds<Pixels> {
    Bounds::new(
        point(bounds.origin.x + px(edges[3]), bounds.origin.y + px(edges[0])),
        size(
            (bounds.size.width - px(edges[1] + edges[3])).max(px(0.0)),
            (bounds.size.height - px(edges[0] + edges[2])).max(px(0.0)),
        ),
    )
}

fn to_corners_px(radii: [f32; 4], inset: [f32; 4], box_size: Size<Pixels>) -> Corners<Pixels> {
    Corners {
        top_left: px((radii[0] - inset[0].max(inset[3])).max(0.0)),
        top_right: px((radii[1] - inset[0].max(inset[1])).max(0.0)),
        bottom_right: px((radii[2] - inset[2].max(inset[1])).max(0.0)),
        bottom_left: px((radii[3] - inset[2].max(inset[3])).max(0.0)),
    }
    .clamp_radii_for_quad_size(box_size)
}

fn round_div_corners(div_el: gpui::Div, radii: [f32; 4]) -> gpui::Div {
    div_el
        .rounded_tl(px(radii[0]))
        .rounded_tr(px(radii[1]))
        .rounded_br(px(radii[2]))
        .rounded_bl(px(radii[3]))
}

fn paint_slot_fill(
    bounds: Bounds<Pixels>,
    corner_radii: Corners<Pixels>,
    bg: &SlotBackground,
    underlay_color: Option<u32>,
    window: &mut Window,
) {
    if bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
        return;
    }
    if let Some(col) = underlay_color {
        window.paint_quad(quad(
            bounds,
            corner_radii,
            solid_background(hsla(col)),
            Edges::default(),
            transparent_black(),
            BorderStyle::Solid,
        ));
    }
    match bg {
        SlotBackground::Solid(c) => {
            window.paint_quad(quad(
                bounds,
                corner_radii,
                solid_background(hsla(*c)),
                Edges::default(),
                transparent_black(),
                BorderStyle::Solid,
            ));
        }
        SlotBackground::Transparent | SlotBackground::ThemeBackgroundImage { .. } => {}
        SlotBackground::LinearGradient { angle_deg, stops } => {
            if let Some(uniform) = all_stops_equal(stops) {
                window.paint_quad(quad(
                    bounds,
                    corner_radii,
                    solid_background(hsla(uniform)),
                    Edges::default(),
                    transparent_black(),
                    BorderStyle::Solid,
                ));
            } else if stops.len() == 2 {
                let from = linear_color_stop(hsla(stops[0].color), stops[0].position.unwrap_or(0.0));
                let to = linear_color_stop(hsla(stops[1].color), stops[1].position.unwrap_or(1.0));
                window.paint_quad(quad(
                    bounds,
                    corner_radii,
                    linear_gradient(*angle_deg, from, to),
                    Edges::default(),
                    transparent_black(),
                    BorderStyle::Solid,
                ));
            } else if let Some(grad) = rasterize_gradient_for_size(
                bg,
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            ) {
                drain_dropped_gradients(window);
                let fitted = fitted_bounds_for_gutter(bounds, grad.inner_w, grad.inner_h);
                let _ = window.paint_image_fitted(bounds, fitted, corner_radii, grad.image, 0, false);
            }
        }
        SlotBackground::RadialGradient { .. } => {
            if let Some(grad) = rasterize_gradient_for_size(
                bg,
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            ) {
                drain_dropped_gradients(window);
                let fitted = fitted_bounds_for_gutter(bounds, grad.inner_w, grad.inner_h);
                let _ = window.paint_image_fitted(bounds, fitted, corner_radii, grad.image, 0, false);
            }
        }
    }
}

fn render_theme_wallpaper_layer(
    size: &BackgroundImageSize,
    overlay_color: Option<u32>,
    underlay_color: Option<u32>,
    radii: [f32; 4],
    backdrop_blur: Option<f32>,
    inset_shadows: Vec<BoxShadow>,
    cx: &App,
) -> Option<AnyElement> {
    let image = background_image(cx)?;
    let fit = match size {
        BackgroundImageSize::Cover => ObjectFit::Cover,
        BackgroundImageSize::Contain => ObjectFit::Contain,
        BackgroundImageSize::Auto => ObjectFit::None,
    };
    let img_layer = img(image)
        .object_fit(fit)
        .size_full()
        .absolute()
        .inset_0()
        .rounded_tl(px(radii[0]))
        .rounded_tr(px(radii[1]))
        .rounded_br(px(radii[2]))
        .rounded_bl(px(radii[3]));

    let mut wrap = round_div_corners(div().absolute().inset_0().size_full().overflow_hidden(), radii);
    if let Some(blur) = backdrop_blur {
        wrap = wrap.child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let corners = to_corners_px(radii, [0.0; 4], bounds.size);
                    window.paint_backdrop_blur(bounds, corners, px(blur));
                },
            )
            .absolute()
            .inset_0()
            .size_full(),
        );
    }
    if let Some(col) = underlay_color {
        wrap = wrap.child(round_div_corners(
            div().absolute().inset_0().size_full().bg(hsla(col)),
            radii,
        ));
    }
    wrap = wrap.child(img_layer);
    if let Some(scrim) = overlay_color {
        wrap = wrap.child(round_div_corners(
            div().absolute().inset_0().size_full().bg(hsla(scrim)),
            radii,
        ));
    }
    if !inset_shadows.is_empty() {
        wrap = wrap.child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let corners = to_corners_px(radii, [0.0; 4], bounds.size);
                    window.paint_inset_shadows(bounds, corners, &inset_shadows);
                },
            )
            .absolute()
            .inset_0()
            .size_full(),
        );
    }
    Some(wrap.into_any_element())
}

fn render_override_background_layer(
    ov: &SlotStyleOverride,
    base: &SlotStyleOverride,
    default_radius: AbsoluteLength,
    cx: &App,
) -> Option<AnyElement> {
    let radii = resolve_radii_px(ov, base, default_radius, cx);
    let backdrop_blur = ov
        .backdrop_blur
        .or(base.backdrop_blur)
        .filter(|b| *b > 0.0);
    let inset_shadows: Vec<BoxShadow> = ov
        .box_shadows
        .as_deref()
        .or(base.box_shadows.as_deref())
        .into_iter()
        .flatten()
        .filter(|s| s.inset)
        .map(|s| BoxShadow {
            color: hsla(s.color),
            offset: point(px(s.offset_x), px(s.offset_y)),
            blur_radius: px(s.blur_radius),
            spread_radius: px(s.spread_radius),
            inset: true,
        })
        .collect();

    if ov.border_gradient.is_none()
        && let Some(SlotBackground::ThemeBackgroundImage { size, overlay_color }) = &ov.background
    {
        return render_theme_wallpaper_layer(
            size,
            *overlay_color,
            ov.background_color,
            radii,
            backdrop_blur,
            inset_shadows,
            cx,
        );
    }

    let needs_layer = ov.border_gradient.is_some()
        || (backdrop_blur.is_some() && (ov.background.is_some() || ov.background_color.is_some()))
        || match ov.background.as_ref() {
            Some(SlotBackground::RadialGradient { .. }) => true,
            Some(SlotBackground::LinearGradient { stops, .. }) => {
                ov.background_color.is_some()
                    || (stops.len() >= 3 && all_stops_equal(stops).is_none())
            }
            Some(
                SlotBackground::ThemeBackgroundImage { .. }
                | SlotBackground::Solid(_)
                | SlotBackground::Transparent,
            )
            | None => false,
        };
    if !needs_layer {
        return None;
    }

    let edge_opts = resolve_border_edges(ov, base);
    let edges = [
        edge_opts[0].unwrap_or(0.0),
        edge_opts[1].unwrap_or(0.0),
        edge_opts[2].unwrap_or(0.0),
        edge_opts[3].unwrap_or(0.0),
    ];
    let has_explicit_border_widths = edges.iter().any(|&w| w > 0.0);
    let effective_border_color = ov.border_color.or(base.border_color);
    let transparent_border = effective_border_color.is_some_and(|c| (c & 0xff) == 0);

    let (expand_outer, inset_inner, radius_inset) = if ov.border_gradient.is_some() {
        if has_explicit_border_widths && transparent_border {
            // Host already reserved transparent border widths in Taffy, placing our canvas at the
            // padding box while its clip mask stays on the outer border box.
            (edges, [0.0; 4], edges)
        } else {
            let bw = resolve_border_inset_px(ov, base);
            ([0.0; 4], [bw; 4], [bw; 4])
        }
    } else if has_explicit_border_widths && !transparent_border {
        ([0.0; 4], [0.0; 4], edges)
    } else {
        ([0.0; 4], [0.0; 4], [0.0; 4])
    };

    let rim = ov.border_gradient.clone();
    let fill_bg = ov.background.clone();
    let underlay_color = ov.background_color;

    Some(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                if bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
                    return;
                }
                let outer_bounds = expand_bounds(bounds, expand_outer);
                let inner_bounds = inset_bounds(bounds, inset_inner);
                let outer_radii = to_corners_px(radii, [0.0; 4], outer_bounds.size);
                let inner_radii = to_corners_px(radii, radius_inset, inner_bounds.size);

                if let Some(blur) = backdrop_blur {
                    // `backdrop_blur_fragment` uses a binary `discard` instead of AA coverage, so
                    // inset it safely inside `inner_bounds` when a translucent rim surrounds it.
                    if rim.is_some() {
                        let blur_bounds = inset_bounds(inner_bounds, [0.5; 4]);
                        if blur_bounds.size.width > px(0.0) && blur_bounds.size.height > px(0.0) {
                            let blur_radii = to_corners_px(
                                radii,
                                radius_inset.map(|r| r + 0.5),
                                blur_bounds.size,
                            );
                            window.paint_backdrop_blur(blur_bounds, blur_radii, px(blur));
                        }
                    } else {
                        window.paint_backdrop_blur(inner_bounds, inner_radii, px(blur));
                    }
                }

                if let Some(rim_bg) = &rim {
                    paint_slot_fill(outer_bounds, outer_radii, rim_bg, None, window);
                    if let Some(bg) = &fill_bg {
                        paint_slot_fill(inner_bounds, inner_radii, bg, underlay_color, window);
                    } else if let Some(col) = underlay_color {
                        paint_slot_fill(
                            inner_bounds,
                            inner_radii,
                            &SlotBackground::Solid(col),
                            None,
                            window,
                        );
                    }
                } else if let Some(bg) = &fill_bg {
                    paint_slot_fill(inner_bounds, inner_radii, bg, underlay_color, window);
                } else if let Some(col) = underlay_color {
                    paint_slot_fill(
                        inner_bounds,
                        inner_radii,
                        &SlotBackground::Solid(col),
                        None,
                        window,
                    );
                }

                if !inset_shadows.is_empty()
                    && inner_bounds.size.width > px(0.0)
                    && inner_bounds.size.height > px(0.0)
                {
                    window.paint_inset_shadows(inner_bounds, inner_radii, &inset_shadows);
                }
            },
        )
        .absolute()
        .inset_0()
        .size_full()
        .into_any_element(),
    )
}

/// Renders an absolutely-positioned background layer for `slot` in `state` when the compiled
/// stylesheet uses a prismatic `border_gradient` (`background-clip: padding-box, border-box`),
/// `backdrop-filter: blur(...)`, a `radial-gradient(...)`, a multi-stop (`>= 3`)
/// `linear-gradient(...)`, or a scoped `var(--theme-background-image)`. Falls back to
/// `SlotState::Base` when `state` has no background layer of its own.
pub fn theme_slot_state_background_layer(
    slot: ThemeSlot,
    state: SlotState,
    default_radius: impl Into<AbsoluteLength>,
    cx: &App,
) -> Option<AnyElement> {
    let global = cx.try_global::<ActiveSlotStylesheet>()?;
    if global.0.is_empty() {
        return None;
    }
    let set = global.0.get(slot)?;
    let default_radius = default_radius.into();
    if state != SlotState::Base {
        let state_ov = set.state(state);
        if state_ov.border_gradient.is_some()
            || state_ov.background.is_some()
            || state_ov.background_color.is_some()
            || state_ov.backdrop_blur.is_some()
        {
            return render_override_background_layer(state_ov, &set.base, default_radius, cx);
        }
    }
    render_override_background_layer(&set.base, &set.base, default_radius, cx)
}

/// Renders an absolutely-positioned background layer for `slot` in `SlotState::Base` (multi-stop
/// linear gradients, radial gradients, prismatic `border_gradient` rims, `backdrop-filter`, or
/// scoped wallpapers).
pub fn theme_slot_background_layer(slot: ThemeSlot, cx: &App) -> Option<AnyElement> {
    theme_slot_state_background_layer(slot, SlotState::Base, px(0.), cx)
}

/// Like [`theme_slot_background_layer`], rounding and clipping all painted layers to `default_radius`
/// (or `slot`'s overridden `border-radius`). Safe for containers that cannot set
/// `.overflow_hidden()` directly (such as menus with overflowing submenus or scrollable dropdown lists).
pub fn theme_slot_clipped_background_layer(
    slot: ThemeSlot,
    default_radius: impl Into<AbsoluteLength>,
    cx: &App,
) -> Option<AnyElement> {
    let global = cx.try_global::<ActiveSlotStylesheet>()?;
    if global.0.is_empty() {
        return None;
    }
    let set = global.0.get(slot)?;
    render_override_background_layer(&set.base, &set.base, default_radius.into(), cx)
}

/// Renders a compiled `::before` pseudo-element bar (e.g. an active thread row's 3px left accent
/// bar) as an absolutely-positioned child element inside the host slot.
pub fn theme_slot_pseudo_before(
    slot: ThemeSlot,
    state: SlotState,
    cx: &App,
) -> Option<AnyElement> {
    let global = cx.try_global::<ActiveSlotStylesheet>()?;
    if global.0.is_empty() {
        return None;
    }
    let set = global.0.get(slot)?;
    let bar = set
        .state(state)
        .before
        .as_ref()
        .or_else(|| (state != SlotState::Base).then_some(set.base.before.as_ref()).flatten())?;

    let mut el = div().absolute();
    if let Some(v) = bar.left.map(to_length) {
        el.style().inset.left = Some(v);
    }
    if let Some(v) = bar.right.map(to_length) {
        el.style().inset.right = Some(v);
    }
    if let Some(v) = bar.top.map(to_length) {
        el.style().inset.top = Some(v);
    }
    if let Some(v) = bar.bottom.map(to_length) {
        el.style().inset.bottom = Some(v);
    }
    if let Some(v) = bar.width.map(to_length) {
        el.style().size.width = Some(v);
    }
    if let Some(v) = bar.height.map(to_length) {
        el.style().size.height = Some(v);
    }
    let rem_px = super::preferences(cx).interface_font_size.max(8.0);
    let radius_px = bar
        .border_radius
        .map(|len| slot_radius_px(Some(len), 0.0, rem_px))
        .unwrap_or(0.0);
    if let Some(r) = bar.border_radius.and_then(to_radius_length) {
        el.style().corner_radii.top_left = Some(r);
        el.style().corner_radii.top_right = Some(r);
        el.style().corner_radii.bottom_right = Some(r);
        el.style().corner_radii.bottom_left = Some(r);
    }
    if let Some(bg) = &bar.background {
        match bg {
            SlotBackground::Solid(c) => {
                el.style().background = Some(Fill::Color(solid_background(hsla(*c))));
            }
            SlotBackground::Transparent => {
                el.style().background = Some(Fill::Color(solid_background(transparent_black())));
            }
            SlotBackground::LinearGradient { angle_deg, stops } => {
                if let Some(uniform) = all_stops_equal(stops) {
                    el.style().background = Some(Fill::Color(solid_background(hsla(uniform))));
                } else if stops.len() == 2 {
                    let from = linear_color_stop(hsla(stops[0].color), stops[0].position.unwrap_or(0.0));
                    let to = linear_color_stop(hsla(stops[1].color), stops[1].position.unwrap_or(1.0));
                    el.style().background = Some(Fill::Color(linear_gradient(*angle_deg, from, to)));
                } else {
                    let bg_clone = bg.clone();
                    el = el.child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                let corners =
                                    Corners::all(px(radius_px)).clamp_radii_for_quad_size(bounds.size);
                                paint_slot_fill(bounds, corners, &bg_clone, None, window);
                            },
                        )
                        .absolute()
                        .inset_0()
                        .size_full(),
                    );
                }
            }
            SlotBackground::RadialGradient { .. } => {
                let bg_clone = bg.clone();
                el = el.child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            let corners =
                                Corners::all(px(radius_px)).clamp_radii_for_quad_size(bounds.size);
                            paint_slot_fill(bounds, corners, &bg_clone, None, window);
                        },
                    )
                    .absolute()
                    .inset_0()
                    .size_full(),
                );
            }
            SlotBackground::ThemeBackgroundImage { size, overlay_color } => {
                if let Some(layer) = render_theme_wallpaper_layer(
                    size,
                    *overlay_color,
                    None,
                    [radius_px; 4],
                    None,
                    Vec::new(),
                    cx,
                ) {
                    el = el.child(layer);
                }
            }
        }
    }
    Some(el.into_any_element())
}

fn to_radius_length(len: SlotLength) -> Option<AbsoluteLength> {
    match len {
        SlotLength::Px(v) => Some(px(v).into()),
        SlotLength::Rem(v) => Some(rems(v).into()),
        SlotLength::Fraction(f) => {
            if f >= 0.5 {
                Some(px(PILL_RADIUS_PX).into())
            } else if f <= 0.0 {
                Some(px(0.0).into())
            } else {
                Some(px((f * 32.0).clamp(0.0, PILL_RADIUS_PX)).into())
            }
        }
        SlotLength::FullMinusPx(_) | SlotLength::Auto => None,
    }
}

fn to_definite_length(len: SlotLength) -> Option<DefiniteLength> {
    match len {
        SlotLength::Px(v) => Some(px(v).into()),
        SlotLength::Rem(v) => Some(rems(v).into()),
        SlotLength::Fraction(f) => Some(relative(f)),
        SlotLength::FullMinusPx(_) | SlotLength::Auto => None,
    }
}

fn to_length(len: SlotLength) -> Length {
    match len {
        SlotLength::Px(v) => px(v).into(),
        SlotLength::Rem(v) => rems(v).into(),
        SlotLength::Fraction(f) => relative(f).into(),
        // `calc(100% - <px>)` resolves cleanly via flex stretch + margins instead of overflowing `100%`.
        SlotLength::FullMinusPx(_) | SlotLength::Auto => Length::Auto,
    }
}

fn to_constraint_length(c: SlotSizeConstraint) -> Length {
    match c {
        SlotSizeConstraint::Px(v) => px(v).into(),
        SlotSizeConstraint::Rem(v) => rems(v).into(),
        SlotSizeConstraint::Fraction(f) => relative(f).into(),
        SlotSizeConstraint::None => Length::Auto,
    }
}

fn resolve_slot_font_family(raw: &str, cx: &App) -> gpui::SharedString {
    if let Some(inventory) = cx.try_global::<FontInventory>() {
        let resolved = resolve_font_stack(
            raw,
            &inventory.names,
            FontDefaults {
                sans: ".SystemUIFont",
                mono: &inventory.mono,
                serif: &inventory.serif,
            },
            "",
        );
        if !resolved.is_empty() {
            return resolved.to_owned().into();
        }
    }
    let first = raw
        .split(',')
        .next()
        .map(|s| s.trim().trim_matches(['"', '\'']).trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(".SystemUIFont");
    first.to_owned().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use gpui::{InteractiveElement, div};
    use serde_json::json;
    use t3_core::client_settings::ClientSettingsDoc;
    use t3_core::themes;

    #[gpui::test]
    fn stock_theme_has_empty_slot_stylesheet_and_zero_mutation_fast_path(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            theme::init(cx);

            assert!(slot_stylesheet(cx).is_empty());
            assert!(slot_style(ThemeSlot::Button, cx).is_none());
            assert!(!slot_stylesheet(cx).has_scoped_background_image());

            let mut baseline = div().bg(hsla(0x1122_33ff)).rounded(px(6.));
            let mut styled = div()
                .bg(hsla(0x1122_33ff))
                .rounded(px(6.))
                .theme_slot(ThemeSlot::Button, cx);
            assert_eq!(styled.style(), baseline.style());

            let mut interactive_before = div().id("stock-btn").bg(hsla(0x1122_33ff));
            let mut interactive_after = div()
                .id("stock-btn")
                .bg(hsla(0x1122_33ff))
                .theme_slot_interactive(ThemeSlot::Button, cx);
            assert_eq!(interactive_after.style(), interactive_before.style());
        });
    }

    #[gpui::test]
    fn init_from_doc_and_preview_with_css_update_slot_styles_live_and_clear_cleanly(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut published = themes::environment(&[json!({
            "id": "neon-slots",
            "name": "Neon Slots",
            "appearance": "dark",
            "canvas": "#10121a",
            "accent": "#ff44aa",
            "css": r#"
                :root { --pill-r: 999px; --btn-shadow: 0 4px 12px rgba(0, 0, 0, 0.5); }
                .dark [data-slot="button"] {
                    background: linear-gradient(90deg, #ff0066, #6600ff);
                    color: #ffffff;
                    border: 2px solid #ff44aa;
                    border-radius: var(--pill-r);
                    padding: 6px 14px;
                    margin: 2px 4px;
                    gap: 8px;
                    min-width: 80px;
                    max-width: 320px;
                    box-shadow: var(--btn-shadow);
                    opacity: 0.9;
                    font-family: "Inter", sans-serif;
                    font-size: 15px;
                    font-weight: 600;
                    line-height: 1.5;
                    flex-direction: row;
                    align-items: center;
                    justify-content: space-between;
                }
                .dark [data-slot="button"]:hover {
                    background: #334455;
                    opacity: 1;
                }
                .dark [data-slot="button"]:active {
                    background: #112233;
                }
                [data-timeline-row-kind="work"] {
                    display: none;
                }
                [data-workspace-area="conversation"] {
                    background: var(--theme-background-image) center / cover no-repeat;
                }
            "#
        })]);
        let neon = published.pop().unwrap();
        let doc = ClientSettingsDoc::from_json(
            &json!({
                "t3code:theme": "neon-slots",
                "t3code:theme-appearance-mode": "dark",
                themes::ENVIRONMENT_THEMES_STORAGE_KEY: [neon],
            })
            .to_string(),
        );

        cx.update(|cx| {
            gpui_component::init(cx);
            theme::init(cx);
            theme::init_from_doc(&doc, cx);

            let sheet = slot_stylesheet(cx);
            assert!(!sheet.is_empty());
            assert!(sheet.has_scoped_background_image());

            let btn_set = slot_style(ThemeSlot::Button, cx).expect("button slot compiled");
            assert!(!btn_set.base.is_empty());
            assert!(!btn_set.hover.is_empty());
            assert!(!btn_set.active.is_empty());

            // Verify `theme_slot` refines StyleRefinement accurately.
            let mut el = div().theme_slot(ThemeSlot::Button, cx);
            let style = el.style();
            assert!(matches!(style.background, Some(Fill::Color(_))));
            assert_eq!(style.border_color, Some(hsla(0xff44_aaff)));
            assert_eq!(style.border_widths.top, Some(px(2.0).into()));
            assert_eq!(style.border_widths.right, Some(px(2.0).into()));
            assert_eq!(style.corner_radii.top_left, Some(px(999.0).into()));
            assert_eq!(style.padding.top, Some(px(6.0).into()));
            assert_eq!(style.padding.right, Some(px(14.0).into()));
            assert_eq!(style.margin.top, Some(px(2.0).into()));
            assert_eq!(style.margin.left, Some(px(4.0).into()));
            assert_eq!(style.gap.width, Some(px(8.0).into()));
            assert_eq!(style.gap.height, Some(px(8.0).into()));
            assert_eq!(style.min_size.width, Some(px(80.0).into()));
            assert_eq!(style.max_size.width, Some(px(320.0).into()));
            assert_eq!(style.opacity, Some(0.9));
            assert_eq!(style.flex_direction, Some(FlexDirection::Row));
            assert_eq!(style.align_items, Some(AlignItems::Center));
            assert_eq!(style.justify_content, Some(JustifyContent::SpaceBetween));
            assert_eq!(style.text.color, Some(hsla(0xffff_ffff)));
            assert_eq!(style.text.font_size, Some(px(15.0).into()));
            assert_eq!(style.text.font_weight, Some(FontWeight(600.0)));
            assert_eq!(style.text.line_height, Some(relative(1.5)));
            assert!(style.text.font_family.is_some());
            assert_eq!(style.box_shadow.as_ref().map(Vec::len), Some(1));

            // Verify `theme_slot_interactive` wires base, hover, and active without panicking.
            let mut interactive = div()
                .id("neon-btn")
                .theme_slot_interactive(ThemeSlot::Button, cx);
            assert_eq!(interactive.style().border_color, Some(hsla(0xff44_aaff)));

            // Verify `refine_slot_hover` / `refine_slot_active` compose cleanly.
            let hovered = refine_slot_hover(StyleRefinement::default(), ThemeSlot::Button, cx);
            assert_eq!(hovered.background, Some(Fill::Color(solid_background(hsla(0x3344_55ff)))));
            assert_eq!(hovered.opacity, Some(1.0));
            let active = refine_slot_active(StyleRefinement::default(), ThemeSlot::Button, cx);
            assert_eq!(active.background, Some(Fill::Color(solid_background(hsla(0x1122_33ff)))));

            // Verify `theme_slot_timeline_row` applies kind-specific `display: none`.
            let mut work_row = div().theme_slot_timeline_row("work", cx);
            assert_eq!(work_row.style().display, Some(Display::None));
            let mut msg_row = div().theme_slot_timeline_row("message", cx);
            assert_eq!(msg_row.style().display, None);

            // Live preview overrides slot stylesheet and restores cleanly when cleared.
            let colors = themes::standard(true);
            theme::preview_with_css(
                Some((
                    &colors,
                    Some(r#"[data-slot="sidebar"] { padding: 20px; background: #010203; }"#),
                    true,
                )),
                cx,
            );
            assert!(slot_style(ThemeSlot::Button, cx).is_none());
            assert!(!slot_stylesheet(cx).has_scoped_background_image());
            let mut sb = div().theme_slot(ThemeSlot::Sidebar, cx);
            assert_eq!(sb.style().padding.top, Some(px(20.0).into()));
            assert_eq!(
                sb.style().background,
                Some(Fill::Color(solid_background(hsla(0x0102_03ff))))
            );

            theme::preview(None, cx);
            assert!(slot_style(ThemeSlot::Sidebar, cx).is_none());
            assert!(slot_style(ThemeSlot::Button, cx).is_some());
            assert!(slot_stylesheet(cx).has_scoped_background_image());
        });
    }

    #[gpui::test]
    fn primitive_and_overlay_slots_render_cleanly_without_duplicate_hover_panics(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{AppContext, Context, Entity, Render, Window};
        use gpui_component::input::{Input, InputState};

        struct Harness {
            input: Entity<InputState>,
            dialog_focus: gpui::FocusHandle,
        }

        impl Render for Harness {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let settings_tooltip = crate::settings::controls::tooltip("Settings tip".into())(window, cx);
                let panel_tooltip = crate::right_panel::tooltip_view("Panel tip".into(), window, cx);
                let select_options = vec![
                    crate::settings::controls::SelectOption::new("a", "Alpha"),
                    crate::settings::controls::SelectOption::new("b", "Beta"),
                ];
                let dialog = crate::settings::controls::DialogFrame {
                    id: "slot-test-dialog",
                    title: "Dialog".into(),
                    description: "Description".into(),
                    max_width: 420.,
                    body: div().child("Body").into_any_element(),
                    footer: crate::settings::controls::dialog_footer(false, vec![], cx).into_any_element(),
                    focus: self.dialog_focus.clone(),
                }
                .render(window, |_, _| {}, cx);

                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        crate::settings::controls::Button::new(
                            "settings-btn-enabled",
                            crate::settings::controls::ButtonVariant::Default,
                            crate::settings::controls::ButtonSize::Default,
                        )
                        .label("Save"),
                    )
                    .child(
                        crate::settings::controls::Button::new(
                            "settings-btn-disabled",
                            crate::settings::controls::ButtonVariant::Outline,
                            crate::settings::controls::ButtonSize::Sm,
                        )
                        .label("Disabled")
                        .disabled(true),
                    )
                    .child(crate::settings::controls::Switch::new("settings-sw-on", true))
                    .child(crate::settings::controls::Switch::new("settings-sw-off", false).disabled(true))
                    .child(
                        crate::settings::controls::Select::new("settings-select", select_options)
                            .selected("a"),
                    )
                    .child(
                        crate::right_panel::Button::new("rp-btn-enabled")
                            .variant(crate::right_panel::Variant::Ghost)
                            .label("Diff"),
                    )
                    .child(
                        crate::right_panel::Button::new("rp-btn-disabled")
                            .variant(crate::right_panel::Variant::Secondary)
                            .disabled(true),
                    )
                    .child(
                        crate::composer::controls::ComposerControl::new("comp-ctrl")
                            .label("Model"),
                    )
                    .child(
                        crate::composer::controls::Button::new(
                            "comp-btn",
                            crate::composer::controls::ButtonVariant::Outline,
                            crate::composer::controls::ButtonSize::Sm,
                        )
                        .label("Attach"),
                    )
                    .child(crate::composer::controls::menu_surface(cx).child("Menu item"))
                    .child(crate::composer::controls::popup_surface(cx).child("Popover body"))
                    .child(
                        crate::thread_details::controls::Row::new("td-row-btn", "Open")
                            .on_click(|_, _, _| {}),
                    )
                    .child(
                        crate::thread_details::controls::Row::new("td-row-sel", "Branch")
                            .select()
                            .opens_menu(),
                    )
                    .child(crate::thread_details::controls::MenuTrigger::new("td-trigger", "More"))
                    .child(Input::new(&self.input).theme_slot(ThemeSlot::Input, cx))
                    .child(settings_tooltip)
                    .child(panel_tooltip)
                    .child(dialog)
            }
        }

        let css = r#"
            [data-slot="button"] { background: #223344; color: #ffffff; border: 1px solid #445566; border-radius: 10px; }
            [data-slot="button"]:hover { background: #334455; }
            [data-slot="button"]:active { background: #112233; }
            [data-slot="input"] { background: #1a1b26; color: #c0caf5; border: 1px solid #7aa2f7; }
            [data-slot="switch"] { background: #2a2b3d; border-radius: 999px; }
            [data-slot="switch"]:hover { opacity: 0.92; }
            [data-slot="switch"]:active { opacity: 0.8; }
            [data-slot="select-trigger"] { background: #1f2335; border: 1px solid #565f89; }
            [data-slot="select-trigger"]:hover { background: #292e42; }
            [data-slot="select-trigger"]:active { background: #16161e; }
            [data-slot="dialog-popup"] { background: #141622; border: 2px solid #7aa2f7; border-radius: 20px; }
            [data-slot="popover-popup"] { background: #181a28; border: 1px solid #bb9af7; }
            [data-slot="menu-popup"] { background: #1c1e2e; border: 1px solid #9ece6a; }
            [data-slot="tooltip-popup"] { background: #0f1018; color: #e0af68; border: 1px solid #e0af68; }
            [data-slot="scroll-area-scrollbar"] { background: #10121a; }
            [data-slot="scroll-area-thumb"] { background: #565f89; }
            [data-slot="scroll-area-thumb"]:hover { background: #7aa2f7; }
        "#;

        cx.update(|cx| {
            gpui_component::init(cx);
            theme::init(cx);
            let colors = themes::standard(true);
            theme::preview_with_css(Some((&colors, Some(css), true)), cx);

            assert!(slot_has_hover(ThemeSlot::Button, cx));
            assert!(slot_has_active(ThemeSlot::Button, cx));
            assert!(slot_has_hover(ThemeSlot::Switch, cx));
            assert!(slot_has_active(ThemeSlot::Switch, cx));
            assert!(slot_has_hover(ThemeSlot::SelectTrigger, cx));
            assert!(slot_has_active(ThemeSlot::SelectTrigger, cx));
            assert!(slot_has_background(ThemeSlot::DialogPopup, cx));
            assert!(slot_has_background(ThemeSlot::PopoverPopup, cx));
            assert!(slot_has_background(ThemeSlot::MenuPopup, cx));
            assert_eq!(slot_border_color(ThemeSlot::DialogPopup, cx), Some(hsla(0x7aa2_f7ff)));

            // Verify menu_surface and popup_surface apply their compiled overrides directly.
            let mut menu = crate::composer::controls::menu_surface(cx);
            assert_eq!(menu.style().border_color, Some(hsla(0x9ece_6aff)));
            assert_eq!(
                menu.style().background,
                Some(Fill::Color(solid_background(hsla(0x1c1e_2eff))))
            );
            let mut popover = crate::composer::controls::popup_surface(cx);
            assert_eq!(popover.style().border_color, Some(hsla(0xbb9a_f7ff)));
            assert_eq!(
                popover.style().background,
                Some(Fill::Color(solid_background(hsla(0x181a_28ff))))
            );

            // Verify Tooltip receives ThemeSlot::TooltipPopup overrides.
            let mut tip = gpui_component::tooltip::Tooltip::new("Tip").theme_slot(ThemeSlot::TooltipPopup, cx);
            assert_eq!(tip.style().border_color, Some(hsla(0xe0af_68ff)));
            assert_eq!(tip.style().text.color, Some(hsla(0xe0af_68ff)));
        });

        let (_view, visual_cx) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            Harness { input, dialog_focus: cx.focus_handle() }
        });
        visual_cx.refresh().unwrap();

        cx.update(|cx| {
            theme::preview(None, cx);
        });
    }

    #[gpui::test]
    fn glass_overlay_fallbacks_preserve_backdrop_without_wallpaper_and_border_without_border_decl(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{Context, Render, Window};

        struct OverlayHarness {
            dialog_focus: gpui::FocusHandle,
        }

        impl Render for OverlayHarness {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let dialog = crate::settings::controls::DialogFrame {
                    id: "fallback-dialog",
                    title: "Fallback".into(),
                    description: "Checks glass fallback".into(),
                    max_width: 400.,
                    body: div().child("Body").into_any_element(),
                    footer: crate::settings::controls::dialog_footer(false, vec![], cx).into_any_element(),
                    focus: self.dialog_focus.clone(),
                }
                .render(window, |_, _| {}, cx);
                div()
                    .size_full()
                    .child(crate::composer::controls::popup_surface(cx).child("Popover"))
                    .child(dialog)
            }
        }

        let colors = themes::standard(true);

        // 1. Scoped `var(--theme-background-image)` with `background_image == None`:
        // `slot_has_background` MUST return `false` so overlays keep `glass::backdrop`.
        cx.update(|cx| {
            gpui_component::init(cx);
            theme::init(cx);
            theme::set_background_image(None, cx);
            theme::preview_with_css(
                Some((
                    &colors,
                    Some(
                        r#"
                        [data-slot="dialog-popup"] { background: var(--theme-background-image) center / cover no-repeat; }
                        [data-slot="popover-popup"] { background: var(--theme-background-image) center / contain no-repeat; }
                        [data-slot="menu-popup"] { background: var(--theme-background-image) center / cover no-repeat; }
                        "#,
                    ),
                    true,
                )),
                cx,
            );
            assert!(slot_stylesheet(cx).has_scoped_background_image());
            assert!(!slot_has_background(ThemeSlot::DialogPopup, cx));
            assert!(!slot_has_background(ThemeSlot::PopoverPopup, cx));
            assert!(!slot_has_background(ThemeSlot::MenuPopup, cx));
            assert!(theme_slot_background_layer(ThemeSlot::DialogPopup, cx).is_none());
            assert!(theme_slot_clipped_background_layer(ThemeSlot::MenuPopup, px(8.), cx).is_none());
        });

        let (_view, visual_cx) = cx.add_window_view(|_, cx| OverlayHarness {
            dialog_focus: cx.focus_handle(),
        });
        visual_cx.refresh().unwrap();

        // 2. Solid `background` without `border` keeps the 1px fallback border and default border color.
        visual_cx.update(|_, cx| {
            let expected_border = hsla(theme::palette(cx).dropdown_border);
            theme::preview_with_css(
                Some((
                    &colors,
                    Some(
                        r#"
                        [data-slot="dialog-popup"] { background: #141622; }
                        [data-slot="popover-popup"] { background: #181a28; }
                        "#,
                    ),
                    true,
                )),
                cx,
            );
            assert!(slot_has_background(ThemeSlot::DialogPopup, cx));
            assert!(slot_has_background(ThemeSlot::PopoverPopup, cx));
            let mut popover = crate::composer::controls::popup_surface(cx);
            assert_eq!(
                popover.style().background,
                Some(Fill::Color(solid_background(hsla(0x181a_28ff))))
            );
            assert_eq!(popover.style().border_widths.top, Some(px(1.).into()));
            assert_eq!(popover.style().border_color, Some(expected_border));

            // Explicit `background: transparent; border: none;` overrides the fallback cleanly.
            theme::preview_with_css(
                Some((
                    &colors,
                    Some(r#"[data-slot="popover-popup"] { background: transparent; border: none; }"#),
                    true,
                )),
                cx,
            );
            assert!(slot_has_background(ThemeSlot::PopoverPopup, cx));
            let mut clear_popover = crate::composer::controls::popup_surface(cx);
            assert_eq!(
                clear_popover.style().background,
                Some(Fill::Color(solid_background(transparent_black())))
            );
            assert_eq!(clear_popover.style().border_widths.top, Some(px(0.).into()));
        });
        visual_cx.refresh().unwrap();

        cx.update(|cx| {
            theme::preview(None, cx);
        });
    }

    #[gpui::test]
    fn kraken_glass_and_kinetic_gradients_rims_states_and_pseudo_bars_render_cleanly(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::prelude::FluentBuilder as _;
        use gpui::{AppContext, Context, Entity, Render, Window};
        use gpui_component::input::{Input, InputState};

        struct KrakenHarness {
            input: Entity<InputState>,
            dialog_focus: gpui::FocusHandle,
        }

        impl Render for KrakenHarness {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let settings_tooltip =
                    crate::settings::controls::tooltip("Kraken tip".into())(window, cx);
                let select_options = vec![
                    crate::settings::controls::SelectOption::new("a", "Alpha"),
                    crate::settings::controls::SelectOption::new("b", "Beta"),
                ];
                let dialog = crate::settings::controls::DialogFrame {
                    id: "kraken-dialog",
                    title: "Kraken Dialog".into(),
                    description: "Verifies prismatic rim".into(),
                    max_width: 400.,
                    body: div().child("Body").into_any_element(),
                    footer: crate::settings::controls::dialog_footer(false, vec![], cx)
                        .into_any_element(),
                    focus: self.dialog_focus.clone(),
                }
                .render(window, |_, _| {}, cx);

                let floating = slot_has_floating_inset(ThemeSlot::SidebarInner, cx);
                let sb_inner = div()
                    .id("kraken-sb-inner")
                    .relative()
                    .overflow_hidden()
                    .when(floating, |this| this.flex_1().min_w_0().min_h_0())
                    .when(!floating, |this| this.size_full())
                    .theme_slot_interactive(ThemeSlot::SidebarInner, cx)
                    .children(theme_slot_state_background_layer(
                        ThemeSlot::SidebarInner,
                        SlotState::Base,
                        px(0.),
                        cx,
                    ));

                let active_row = div()
                    .id("kraken-active-row")
                    .relative()
                    .rounded(px(6.))
                    .theme_slot(ThemeSlot::ThreadItem, cx)
                    .theme_slot_state(ThemeSlot::ThreadItem, SlotState::Selected, cx)
                    .children(theme_slot_state_background_layer(
                        ThemeSlot::ThreadItem,
                        SlotState::Selected,
                        px(6.),
                        cx,
                    ))
                    .children(theme_slot_pseudo_before(
                        ThemeSlot::ThreadItem,
                        SlotState::Selected,
                        cx,
                    ))
                    .child("Selected thread");

                let focused_composer = div()
                    .id("kraken-focused-composer")
                    .relative()
                    .overflow_hidden()
                    .rounded(px(16.))
                    .theme_slot_interactive(ThemeSlot::ComposerHost, cx)
                    .theme_slot_state(ThemeSlot::ComposerHost, SlotState::FocusWithin, cx)
                    .children(theme_slot_state_background_layer(
                        ThemeSlot::ComposerHost,
                        SlotState::FocusWithin,
                        px(16.),
                        cx,
                    ))
                    .child("Composer");

                let header = crate::chat_header::container(false, cx).child("Chat header");

                let details_slot = slot_style(ThemeSlot::ThreadDetailsCard, cx);
                let details_card = div()
                    .id("kraken-details-card")
                    .relative()
                    .overflow_hidden()
                    .w(px(260.))
                    .h(px(140.))
                    .rounded(px(16.))
                    .theme_slot_interactive(ThemeSlot::ThreadDetailsCard, cx)
                    .children(theme_slot_state_background_layer(
                        ThemeSlot::ThreadDetailsCard,
                        SlotState::Base,
                        px(16.),
                        cx,
                    ))
                    .when(details_slot.is_some(), |this| this.child("Details"));

                let group = crate::settings::controls::group(
                    vec![
                        div().child("Row 1").into_any_element(),
                        div().child("Row 2").into_any_element(),
                    ],
                    cx,
                );

                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(header)
                    .child(div().w(px(240.)).h(px(120.)).flex().flex_col().child(sb_inner))
                    .child(active_row)
                    .child(focused_composer)
                    .child(details_card)
                    .child(group)
                    .child(crate::settings::controls::Switch::new("kraken-sw-on", true))
                    .child(crate::settings::controls::Switch::new("kraken-sw-off", false))
                    .child(crate::composer::controls::checkbox(true, cx))
                    .child(crate::composer::controls::checkbox(false, cx))
                    .child(
                        crate::settings::controls::Select::new("kraken-select", select_options)
                            .selected("a"),
                    )
                    .child(crate::composer::controls::menu_surface(cx).child("Menu"))
                    .child(crate::composer::controls::popup_surface(cx).child("Popover"))
                    .child(Input::new(&self.input).theme_slot(ThemeSlot::Input, cx))
                    .child(settings_tooltip)
                    .child(dialog)
            }
        }

        let kraken_glass_css = r#"
            :root {
                --kraken-halo: linear-gradient(135deg, rgb(243 60 159 / 0.90) 0%, rgb(173 55 248 / 0.88) 24%, rgb(94 74 251 / 0.90) 46%, rgb(11 183 245 / 0.92) 72%, rgb(2 196 156 / 0.92) 88%, rgb(243 60 159 / 0.90) 100%);
                --kraken-halo-soft: linear-gradient(135deg, rgb(243 60 159 / 0.30) 0%, rgb(173 55 248 / 0.26) 24%, rgb(94 74 251 / 0.26) 46%, rgb(11 183 245 / 0.28) 72%, rgb(2 196 156 / 0.28) 88%, rgb(243 60 159 / 0.28) 100%);
                --kraken-switch-on: linear-gradient(135deg, #8B74FF 0%, #6A4FF5 100%);
                --kraken-switch-off: rgb(255 255 255 / 0.16);
                --font-mono: "Menlo", monospace;
            }
            body {
                font-family: "Helvetica Neue", system-ui, sans-serif;
            }
            .dark [data-slot="sidebar"],
            .dark [data-slot="sidebar"] > * {
                background: #0A0B0E !important;
                border-color: transparent !important;
                border-right: 0 !important;
            }
            [data-slot="sidebar-inner"] {
                margin: 6px !important;
                width: calc(100% - 12px) !important;
                height: calc(100% - 12px) !important;
                border-radius: 14px !important;
                border: 1px solid transparent !important;
                background-clip: padding-box, border-box !important;
                backdrop-filter: blur(32px) saturate(160%) !important;
            }
            .dark [data-slot="sidebar-inner"] {
                background-image: linear-gradient(180deg, rgb(21 23 30 / 0.94) 0%, rgb(15 16 22 / 0.97) 100%), var(--kraken-halo-soft) !important;
                box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.07), 0 18px 40px -14px rgb(0 0 0 / 0.82) !important;
            }
            .dark [data-chat-header] {
                background: color-mix(in srgb, #0A0B0E 82%, transparent) !important;
                backdrop-filter: blur(24px) saturate(160%);
                border-bottom: 1px solid rgb(255 255 255 / 0.06) !important;
            }
            [data-thread-item] {
                border-radius: 11px !important;
                border: 1px solid transparent !important;
            }
            .dark [data-thread-item]:hover {
                background: rgb(255 255 255 / 0.05) !important;
            }
            .dark [data-thread-item]:is([data-active]:not([data-active="false"]), [aria-selected="true"]) {
                color: #F4F5F8 !important;
                border: 1px solid transparent !important;
                background-image: linear-gradient(#262934, #262934), var(--kraken-halo-soft) !important;
                background-clip: padding-box, border-box !important;
                box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.06) !important;
            }
            .dark [data-thread-details-card] {
                border: 1px solid transparent !important;
                border-radius: 14px !important;
                background-image: linear-gradient(#14161D, #14161D), var(--kraken-halo-soft) !important;
                background-clip: padding-box, border-box !important;
                box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.05), 0 10px 28px -14px rgb(0 0 0 / 0.65);
            }
            [data-slot="composer-host"] {
                border-radius: 14px !important;
                border: 1px solid #282C36 !important;
                background: #161820 !important;
            }
            [data-slot="composer-host"]:focus-within {
                border-color: transparent !important;
                background-image: linear-gradient(var(--composer-bg, #161820), var(--composer-bg, #161820)), var(--kraken-halo) !important;
                background-clip: padding-box, border-box !important;
            }
            .dark [data-slot="composer-host"]:focus-within {
                --composer-bg: #161820;
                box-shadow: 0 0 0 3px rgb(139 116 255 / 0.16) !important;
            }
            [data-slot="switch"] { background: var(--kraken-switch-off) !important; }
            [data-slot="switch"][data-checked] {
                background: var(--kraken-switch-on) !important;
                box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.28), 0 0 12px -2px rgb(139 116 255 / 0.48) !important;
            }
            [data-slot="switch-thumb"] { background: #FFFFFF !important; }
            .dark [data-slot="switch-thumb"][data-unchecked] { background: #DCE0EC !important; }
            [data-slot="checkbox"] { border-color: #3a3f50 !important; }
            [data-slot="checkbox-indicator"][data-checked] { background: var(--kraken-switch-on) !important; color: #FFFFFF !important; }
            [data-slot="settings-group"] {
                background: rgb(18 20 27 / 0.88) !important;
                border-color: rgb(255 255 255 / 0.09) !important;
                border-radius: 14px !important;
                backdrop-filter: blur(18px) saturate(140%);
            }
            [data-slot="input-control"][class*="border-input"] { border-color: rgb(255 255 255 / 0.14) !important; border-radius: 10px !important; }
            [data-slot="dialog-popup"], [data-slot="popover-popup"], [data-slot="menu-popup"], [data-slot="select-popup"] > .dropdown-glass {
                border: 1px solid transparent !important;
                border-radius: 14px !important;
                background-clip: padding-box, border-box !important;
                background-image: linear-gradient(rgb(21 23 30 / 0.96), rgb(15 16 22 / 0.97)), var(--kraken-halo-soft) !important;
            }
            [data-slot="tooltip-popup"] {
                border: 1px solid transparent !important;
                border-radius: 9px !important;
                background-clip: padding-box, border-box !important;
                background-image: linear-gradient(rgb(24 26 34 / 0.96), rgb(17 19 26 / 0.97)), var(--kraken-halo-soft) !important;
                color: #EEF0F6 !important;
            }
        "#;

        let kraken_kinetic_css = r#"
            [data-slot="sidebar-inner"] {
                background-color: #07080C !important;
                background-image: radial-gradient(120% 55% at 50% 108%, rgb(91 61 245 / 0.22), transparent 72%) !important;
            }
            [data-thread-item][data-active="true"] {
                background: rgb(255 255 255 / 0.12) !important;
            }
            [data-thread-item][data-active="true"]::before {
                content: "";
                position: absolute;
                left: 0;
                top: 22%;
                bottom: 22%;
                width: 3px;
                border-radius: 3px;
                background: #8B74FF;
            }
        "#;

        let colors = themes::standard(true);

        cx.update(|cx| {
            gpui_component::init(cx);
            theme::init(cx);
            theme::preview_with_css(Some((&colors, Some(kraken_glass_css), true)), cx);

            // Floating sidebar card & zeroed sidebar divider border.
            assert!(slot_has_floating_inset(ThemeSlot::SidebarInner, cx));
            let mut sb = div().border_r_1().theme_slot(ThemeSlot::Sidebar, cx);
            assert_eq!(sb.style().border_widths.right, Some(px(0.0).into()));
            assert_eq!(sb.style().border_color, Some(hsla(0x0000_0000)));

            // `calc(100% - 12px)` resolves to `Length::Auto` and `border_gradient` sets transparent host bg.
            let mut sb_in = div().size_full().theme_slot(ThemeSlot::SidebarInner, cx);
            assert_eq!(sb_in.style().size.width, Some(Length::Auto));
            assert_eq!(sb_in.style().size.height, Some(Length::Auto));
            assert_eq!(sb_in.style().margin.top, Some(px(6.0).into()));
            assert_eq!(
                sb_in.style().background,
                Some(Fill::Color(solid_background(transparent_black())))
            );
            // Inset shadow moves onto the background layer canvas so only the outer drop shadow stays on host.
            let sb_shadows = sb_in.style().box_shadow.as_ref().expect("sidebar drop shadow");
            assert_eq!(sb_shadows.len(), 1);
            assert!(!sb_shadows[0].inset);

            // ThreadItem:selected has inset-only box-shadow, so host keeps `Some([])` and canvas paints inset highlight.
            let mut sel_row = div()
                .theme_slot(ThemeSlot::ThreadItem, cx)
                .theme_slot_state(ThemeSlot::ThreadItem, SlotState::Selected, cx);
            assert_eq!(sel_row.style().box_shadow.as_ref().map(Vec::len), Some(0));

            // Switch:checked uses a native 2-stop gradient without a background layer, keeping both inset + glow shadows.
            let mut sw_checked = div()
                .theme_slot(ThemeSlot::Switch, cx)
                .theme_slot_state(ThemeSlot::Switch, SlotState::Selected, cx);
            assert_eq!(sw_checked.style().box_shadow.as_ref().map(Vec::len), Some(2));

            assert!(slot_has_background(ThemeSlot::SidebarInner, cx));
            assert!(slot_has_background(ThemeSlot::DialogPopup, cx));
            assert!(slot_has_background(ThemeSlot::PopoverPopup, cx));
            assert!(slot_has_background(ThemeSlot::MenuPopup, cx));

            // Multi-stop linear gradient rasterization populates cache, surrounds it with a clamped
            // 2-texel gutter, and reuses the cached RenderImage Arc across hits.
            let rim = slot_style(ThemeSlot::SidebarInner, cx)
                .unwrap()
                .base
                .border_gradient
                .unwrap();
            let img1 = rasterize_gradient(&rim).expect("rasterized 6-stop halo");
            let img2 = rasterize_gradient(&rim).expect("cached 6-stop halo");
            assert!(Arc::ptr_eq(&img1.image, &img2.image));
            assert_eq!(img1.inner_w, GRADIENT_TEX_SIZE);
            assert_eq!(img1.inner_h, GRADIENT_TEX_SIZE);
            assert_eq!(
                img1.image.size(0).width.0,
                (GRADIENT_TEX_SIZE + 2 * GRADIENT_GUTTER) as i32
            );
            assert_eq!(
                img1.image.size(0).height.0,
                (GRADIENT_TEX_SIZE + 2 * GRADIENT_GUTTER) as i32
            );

            // Aspect-aware gradient rasterization on a wide 600x92 composer sweeps pink -> emerald across top edge.
            let composer_halo = slot_style(ThemeSlot::ComposerHost, cx)
                .unwrap()
                .focus_within
                .border_gradient
                .unwrap();
            let wide1 = rasterize_gradient_for_size(&composer_halo, 600.0, 92.0)
                .expect("wide composer gradient");
            let wide2 = rasterize_gradient_for_size(&composer_halo, 604.0, 92.0)
                .expect("same bucket cache hit");
            assert!(Arc::ptr_eq(&wide1.image, &wide2.image));
            let tall = rasterize_gradient_for_size(&composer_halo, 260.0, 900.0)
                .expect("tall sidebar gradient");
            assert!(wide1.inner_w > wide1.inner_h);
            assert!(tall.inner_h > tall.inner_w);

            let tw = wide1.image.size(0).width.0 as usize;
            let th = wide1.image.size(0).height.0 as usize;
            let g = GRADIENT_GUTTER as usize;
            let bytes = wide1.image.as_bytes(0).unwrap();
            let pixel_rgba = |x: usize, y: usize| -> [u8; 4] {
                let off = (y * tw + x) * 4;
                // Stored in straight-alpha BGRA8 byte order.
                [bytes[off + 2], bytes[off + 1], bytes[off], bytes[off + 3]]
            };
            let tl = pixel_rgba(g, g);
            let tr = pixel_rgba(tw - g - 1, g);
            let bl = pixel_rgba(g, th - g - 1);
            let br = pixel_rgba(tw - g - 1, th - g - 1);
            assert!(tl[0] > 200 && tl[2] > 120, "top-left should be pink: {tl:?}");
            assert!(tr[1] > 170 && tr[0] < 40, "top-right should be emerald/cyan: {tr:?}");
            assert!(bl[0] > 180 && bl[2] > 160, "bottom-left should be pink/violet: {bl:?}");
            assert!(br[0] > 200 && br[2] > 120, "bottom-right should be pink: {br:?}");

            // Outer 2-texel gutter clamps the adjacent interior edge color (never transparent black).
            let mid_x = tw / 2;
            let mid_y = th / 2;
            assert_eq!(pixel_rgba(0, g), tl);
            assert_eq!(pixel_rgba(0, mid_y), pixel_rgba(g, mid_y));
            assert_eq!(pixel_rgba(tw - 1, mid_y), pixel_rgba(tw - g - 1, mid_y));
            assert_eq!(pixel_rgba(mid_x, 0), pixel_rgba(mid_x, g));
            assert_eq!(pixel_rgba(mid_x, th - 1), pixel_rgba(mid_x, th - g - 1));
            assert!(pixel_rgba(0, mid_y)[3] > 0 && pixel_rgba(mid_x, th - 1)[3] > 0);

            // `fitted_bounds_for_gutter` reproduces GPUI's exact interior crop rectangle `(g, g, inner_w, inner_h)`.
            for (w_f, h_f, grad_ref) in [
                (600.0f32, 92.0f32, &wide1),
                (240.0f32, 900.0f32, &tall),
                (253.5f32, 877.25f32, &tall),
            ] {
                let visible = Bounds::new(point(px(6.0), px(6.0)), size(px(w_f), px(h_f)));
                let fitted = fitted_bounds_for_gutter(visible, grad_ref.inner_w, grad_ref.inner_h);
                let total_w = (grad_ref.inner_w + 2 * GRADIENT_GUTTER) as f32;
                let total_h = (grad_ref.inner_h + 2 * GRADIENT_GUTTER) as f32;
                let fx = (f32::from(visible.origin.x) - f32::from(fitted.origin.x))
                    / f32::from(fitted.size.width);
                let fy = (f32::from(visible.origin.y) - f32::from(fitted.origin.y))
                    / f32::from(fitted.size.height);
                let fsw = f32::from(visible.size.width) / f32::from(fitted.size.width);
                let fsh = f32::from(visible.size.height) / f32::from(fitted.size.height);
                assert_eq!((fx * total_w).round() as u32, GRADIENT_GUTTER);
                assert_eq!((fy * total_h).round() as u32, GRADIENT_GUTTER);
                assert_eq!((fsw * total_w).round() as u32, grad_ref.inner_w);
                assert_eq!((fsh * total_h).round() as u32, grad_ref.inner_h);
            }

            // ChatHeader with `backdrop-filter: blur(24px)` + translucent solid bg clears host bg for canvas blur.
            let mut ch = div().theme_slot(ThemeSlot::ChatHeader, cx);
            assert_eq!(
                ch.style().background,
                Some(Fill::Color(solid_background(transparent_black())))
            );
            assert!(theme_slot_background_layer(ThemeSlot::ChatHeader, cx).is_some());

            // Focused composer gets transparent host bg + border_gradient layer + focus shadow.
            let mut comp = div()
                .theme_slot(ThemeSlot::ComposerHost, cx)
                .theme_slot_state(ThemeSlot::ComposerHost, SlotState::FocusWithin, cx);
            assert_eq!(
                comp.style().background,
                Some(Fill::Color(solid_background(transparent_black())))
            );
            assert_eq!(comp.style().box_shadow.as_ref().map(Vec::len), Some(1));
            assert!(
                theme_slot_state_background_layer(
                    ThemeSlot::ComposerHost,
                    SlotState::FocusWithin,
                    px(16.),
                    cx
                )
                .is_some()
            );

            // SwitchThumb dark checked vs unchecked.
            let mut thumb_checked = div()
                .theme_slot(ThemeSlot::SwitchThumb, cx)
                .theme_slot_state(ThemeSlot::SwitchThumb, SlotState::Selected, cx);
            assert_eq!(
                thumb_checked.style().background,
                Some(Fill::Color(solid_background(hsla(0xffff_ffff))))
            );
            let mut thumb_unchecked = div()
                .theme_slot(ThemeSlot::SwitchThumb, cx)
                .theme_slot_state(ThemeSlot::SwitchThumb, SlotState::Unchecked, cx);
            assert_eq!(
                thumb_unchecked.style().background,
                Some(Fill::Color(solid_background(hsla(0xdce0_ecff))))
            );
        });

        // Render Kraken Glass inside a real GPUI test window (verifying zero duplicate hover panics).
        let (_view, visual_cx) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            KrakenHarness { input, dialog_focus: cx.focus_handle() }
        });
        visual_cx.refresh().unwrap();

        // Switch live to Kraken Kinetic and verify radial gradient + `::before` bar + cache invalidation.
        visual_cx.update(|_, cx| {
            theme::preview_with_css(Some((&colors, Some(kraken_kinetic_css), true)), cx);
            assert!(
                !GRADIENT_CACHE.lock().unwrap().pending_drop.is_empty(),
                "cache invalidation queues old textures for window.drop_image cleanup"
            );
            assert!(!slot_has_floating_inset(ThemeSlot::SidebarInner, cx));
            assert!(slot_has_background(ThemeSlot::SidebarInner, cx));
            let sb_bg = slot_style(ThemeSlot::SidebarInner, cx).unwrap().base.background.unwrap();
            let rad_img = rasterize_gradient(&sb_bg).expect("rasterized radial gradient");
            // Interior bottom-center pixel (near 50% 108%) should have non-zero violet alpha; top-left should be transparent.
            let tw = rad_img.image.size(0).width.0 as usize;
            let th = rad_img.image.size(0).height.0 as usize;
            let g = GRADIENT_GUTTER as usize;
            let bytes = rad_img.image.as_bytes(0).unwrap();
            let top_left_alpha = bytes[(g * tw + g) * 4 + 3];
            let bottom_center_offset = ((th - g - 1) * tw + tw / 2) * 4;
            let bottom_center_alpha = bytes[bottom_center_offset + 3];
            assert_eq!(top_left_alpha, 0);
            assert!(bottom_center_alpha > 0);
            assert!(
                theme_slot_pseudo_before(ThemeSlot::ThreadItem, SlotState::Selected, cx).is_some()
            );
        });
        visual_cx.refresh().unwrap();
        assert!(
            GRADIENT_CACHE.lock().unwrap().pending_drop.is_empty(),
            "paint pass drains queued gradient textures via window.drop_image"
        );

        cx.update(|cx| {
            theme::preview(None, cx);
        });
    }

    #[gpui::test]
    fn two_stop_linear_gradient_with_background_color_uses_layered_underlay(
        cx: &mut gpui::TestAppContext,
    ) {
        let css = r#"
            [data-slot="sidebar-inner"] {
                background-color: #07080C;
                background-image: linear-gradient(180deg, rgba(139, 116, 255, 0.3), transparent);
                border-radius: 12px;
            }
            [data-slot="settings-group"] {
                background-image: linear-gradient(180deg, rgba(139, 116, 255, 0.3), transparent);
            }
        "#;
        let colors = themes::standard(true);

        cx.update(|cx| {
            gpui_component::init(cx);
            theme::init(cx);
            theme::preview_with_css(Some((&colors, Some(css), true)), cx);

            // With `background-color` + 2-stop `linear-gradient(...)`, the slot delegates painting
            // to the background layer (so the solid underlay is preserved beneath the gradient).
            let sb_ov = slot_style(ThemeSlot::SidebarInner, cx).unwrap().base;
            assert_eq!(sb_ov.background_color, Some(0x0708_0cff));
            assert!(override_uses_raster_or_rim(&sb_ov));
            let mut sb = div().theme_slot(ThemeSlot::SidebarInner, cx);
            assert_eq!(
                sb.style().background,
                Some(Fill::Color(solid_background(transparent_black())))
            );
            assert!(theme_slot_background_layer(ThemeSlot::SidebarInner, cx).is_some());
            assert!(
                theme_slot_clipped_background_layer(ThemeSlot::SidebarInner, px(12.), cx)
                    .is_some()
            );

            // Without `background-color`, a plain 2-stop `linear-gradient(...)` still uses native
            // GPUI `linear_gradient` directly on the host style with no extra background layer.
            let group_ov = slot_style(ThemeSlot::SettingsGroup, cx).unwrap().base;
            assert!(group_ov.background_color.is_none());
            assert!(!override_uses_raster_or_rim(&group_ov));
            assert!(theme_slot_background_layer(ThemeSlot::SettingsGroup, cx).is_none());

            theme::preview(None, cx);
        });
    }
}
