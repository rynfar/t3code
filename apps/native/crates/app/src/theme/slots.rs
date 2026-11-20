//! GPUI `ThemeSlot` application engine for compiled `THEME_CSS_HOOKS` overrides.
//!
//! Reads the pre-compiled `Arc<SlotStylesheet>` stored in GPUI global state (zero per-frame CSS
//! parsing) and refines `StyleRefinement`s directly with a zero-allocation fast path when no
//! custom CSS slot overrides are active.

// Part 1 of #457 exposes the slot engine helpers for Wave H4B's 29 UI call sites.
#![allow(dead_code)]

use std::sync::Arc;

use gpui::{
    AbsoluteLength, AlignItems, AnyElement, App, DefiniteLength, Display, Fill, FlexDirection,
    FontWeight, Global, InteractiveElement, IntoElement, JustifyContent, Length, ObjectFit,
    ParentElement, StatefulInteractiveElement, StyleRefinement, Styled, StyledImage, div, img,
    linear_color_stop, linear_gradient, point, px, relative, rems, solid_background,
    transparent_black,
};
use t3_core::client_settings::typography::{FontDefaults, resolve_font_stack};
use t3_core::themes::slots::{
    BackgroundImageSize, PILL_RADIUS_PX, SlotAlignItems, SlotBackground, SlotFlexDirection,
    SlotJustifyContent, SlotLength, SlotLineHeight, SlotSizeConstraint,
};
pub use t3_core::themes::slots::{SlotStyleOverride, SlotStyleSet, SlotStylesheet, ThemeSlot};

use super::{FontInventory, background_image, hsla};

pub(super) struct ActiveSlotStylesheet(pub(super) Arc<SlotStylesheet>);
impl Global for ActiveSlotStylesheet {}

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

/// Zero-allocation check for whether the active stylesheet defines a base `background` override for
/// `slot` (used by glass overlays to suppress `glass::backdrop` so custom backgrounds show cleanly).
pub fn slot_has_background(slot: ThemeSlot, cx: &App) -> bool {
    cx.try_global::<ActiveSlotStylesheet>()
        .is_some_and(|g| !g.0.is_empty() && g.0.get(slot).is_some_and(|set| set.base.background.is_some()))
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
    if let Some(bg) = &ov.background {
        match bg {
            SlotBackground::Solid(c) => {
                style.background = Some(Fill::Color(solid_background(hsla(*c))));
            }
            SlotBackground::Transparent => {
                style.background = Some(Fill::Color(solid_background(transparent_black())));
            }
            SlotBackground::LinearGradient { angle_deg, stops } => {
                if let (Some(first), Some(last)) = (stops.first(), stops.last()) {
                    let from = linear_color_stop(hsla(first.color), first.position.unwrap_or(0.0));
                    let to = linear_color_stop(hsla(last.color), last.position.unwrap_or(1.0));
                    style.background = Some(Fill::Color(linear_gradient(*angle_deg, from, to)));
                }
            }
            SlotBackground::ThemeBackgroundImage { .. } => {
                // Rendered by `theme_slot_background_layer` so the image sits cleanly behind children.
            }
        }
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
                .map(|s| gpui::BoxShadow {
                    color: hsla(s.color),
                    offset: point(px(s.offset_x), px(s.offset_y)),
                    blur_radius: px(s.blur_radius),
                    spread_radius: px(s.spread_radius),
                    inset: s.inset,
                })
                .collect(),
        );
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

/// Applies base `ThemeSlot` CSS overrides onto any GPUI `Styled` element.
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

/// Renders an absolutely-positioned background image layer when the active theme CSS scopes
/// `var(--theme-background-image)` to `slot` and a background image is loaded.
pub fn theme_slot_background_layer(slot: ThemeSlot, cx: &App) -> Option<AnyElement> {
    let global = cx.try_global::<ActiveSlotStylesheet>()?;
    if !global.0.has_scoped_background_image() {
        return None;
    }
    let set = global.0.get(slot)?;
    let SlotBackground::ThemeBackgroundImage { size, overlay_color } = &set.base.background.as_ref()? else {
        return None;
    };
    let image = background_image(cx)?;
    let fit = match size {
        BackgroundImageSize::Cover => ObjectFit::Cover,
        BackgroundImageSize::Contain => ObjectFit::Contain,
        BackgroundImageSize::Auto => ObjectFit::None,
    };
    let img_layer = img(image).object_fit(fit).size_full().absolute().inset_0();
    match overlay_color {
        Some(scrim) => Some(
            div()
                .absolute()
                .inset_0()
                .size_full()
                .child(img_layer)
                .child(div().absolute().inset_0().size_full().bg(hsla(*scrim)))
                .into_any_element(),
        ),
        None => Some(img_layer.into_any_element()),
    }
}

/// Like [`theme_slot_background_layer`], wrapped in an absolutely-positioned rounded clip container
/// matching `default_radius` (or `slot`'s overridden `border-radius`). Safe for containers that
/// cannot set `.overflow_hidden()` directly (such as menus with overflowing submenus or scrollable
/// dropdown lists).
pub fn theme_slot_clipped_background_layer(
    slot: ThemeSlot,
    default_radius: impl Into<AbsoluteLength>,
    cx: &App,
) -> Option<AnyElement> {
    let layer = theme_slot_background_layer(slot, cx)?;
    let mut clip = div().absolute().inset_0().rounded(default_radius.into()).overflow_hidden();
    if let Some(global) = cx.try_global::<ActiveSlotStylesheet>()
        && let Some(set) = global.0.get(slot)
        && !set.base.border_radii.is_empty()
    {
        let radii_only = SlotStyleOverride {
            border_radii: set.base.border_radii,
            ..Default::default()
        };
        apply_slot_override(clip.style(), &radii_only, cx);
    }
    Some(clip.child(layer).into_any_element())
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
        SlotLength::Auto => None,
    }
}

fn to_definite_length(len: SlotLength) -> Option<DefiniteLength> {
    match len {
        SlotLength::Px(v) => Some(px(v).into()),
        SlotLength::Rem(v) => Some(rems(v).into()),
        SlotLength::Fraction(f) => Some(relative(f)),
        SlotLength::Auto => None,
    }
}

fn to_length(len: SlotLength) -> Length {
    match len {
        SlotLength::Px(v) => px(v).into(),
        SlotLength::Rem(v) => rems(v).into(),
        SlotLength::Fraction(f) => relative(f).into(),
        SlotLength::Auto => Length::Auto,
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
}
