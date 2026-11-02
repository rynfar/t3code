//! `ProviderInstanceIcon.tsx`: a provider's brand mark, else its initials, with the instance badge
//! that tells two instances of one driver apart (design system §8).

use gpui::{App, FontWeight, Hsla, Pixels, SharedString, Window, div, img, prelude::*, px, rgba};
use gpui_component::ActiveTheme;
use t3_core::providers::{self, ProviderEntry};

use crate::icons::{ProviderLogo, provider_logo};
use crate::theme::{hsla, metrics, palette};

/// What identifies one provider instance on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderMark {
    driver: SharedString,
    initials: SharedString,
    /// The configured accent as `0xrrggbbff`.
    accent: Option<u32>,
    badge: bool,
}

impl ProviderMark {
    pub fn of(entry: &ProviderEntry, entries: &[ProviderEntry]) -> Self {
        ProviderMark {
            driver: entry.driver().to_owned().into(),
            initials: providers::initials(&entry.display_name).into(),
            accent: providers::accent_color(entry.provider),
            badge: providers::shows_instance_badge(entry, entries),
        }
    }
}

/// The badge's size: the rail's default, or the smaller one rows and the trigger use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeSize {
    Default,
    Compact,
}

#[derive(IntoElement)]
pub struct ProviderIcon {
    mark: ProviderMark,
    size: Pixels,
    badge: BadgeSize,
    /// The surface behind the badge, drawn as its border so it reads as cut out.
    ring: Option<Hsla>,
}

impl ProviderIcon {
    pub fn new(mark: ProviderMark) -> Self {
        ProviderIcon { mark, size: px(16.), badge: BadgeSize::Compact, ring: None }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    pub fn badge(mut self, badge: BadgeSize, ring: Hsla) -> Self {
        self.badge = badge;
        self.ring = Some(ring);
        self
    }
}

impl RenderOnce for ProviderIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let mark = match provider_logo(&self.mark.driver) {
            Some(ProviderLogo::Mono(path)) => gpui::svg().path(path).size(self.size).flex_none().into_any_element(),
            Some(ProviderLogo::Image { light, dark }) => {
                img(if cx.theme().is_dark() { dark } else { light }).size(self.size).flex_none().into_any_element()
            }
            None => div()
                .text_size(px(metrics::TEXT_3XS))
                .line_height(px(metrics::TEXT_3XS))
                .font_weight(FontWeight::SEMIBOLD)
                .child(self.mark.initials.clone())
                .into_any_element(),
        };
        let badge = self.ring.filter(|_| self.mark.badge).map(|ring| {
            let (height, text, inset) = match self.badge {
                BadgeSize::Default => (px(14.), metrics::TEXT_4XS, px(0.)),
                BadgeSize::Compact => (px(12.), metrics::TEXT_5XS, px(-2.)),
            };
            let (background, foreground) = match self.mark.accent {
                Some(accent) => (rgba(accent).into(), if providers::accent_wants_black(accent) { gpui::black() } else { gpui::white() }),
                None => (hsla(p.card), hsla(p.muted_foreground)),
            };
            div()
                .absolute()
                .right(inset)
                .bottom(inset)
                .h(height)
                .min_w(height)
                .px(px(2.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(ring)
                .bg(background)
                .text_size(px(text))
                .line_height(px(text))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(foreground)
                .child(self.mark.initials)
        });
        div().relative().flex_none().flex().items_center().justify_center().size(self.size).child(mark).children(badge)
    }
}
