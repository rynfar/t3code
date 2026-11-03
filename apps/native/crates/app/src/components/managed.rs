//! Ports `web/src/components/ManagedPolicy.tsx`; the disabled primitive owns input suppression.
use crate::{
    icons::Icon,
    settings::controls::{Button, Switch, focus_handle, r},
    theme::{hsla, palette},
};
use gpui::{
    App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
    prelude::*,
};
use gpui_component::{h_flex, tooltip::Tooltip};

pub trait ManagedElement: IntoElement {
    fn lock(self) -> Self;
}
impl ManagedElement for Button {
    fn lock(self) -> Self {
        self.disabled(true)
    }
}
impl ManagedElement for Switch {
    fn lock(self) -> Self {
        self.disabled(true)
    }
}
#[derive(IntoElement)]
pub struct ManagedControl {
    id: ElementId,
    reason: Option<SharedString>,
    child: gpui::AnyElement,
}
impl ManagedControl {
    pub fn new(
        id: impl Into<ElementId>,
        reason: Option<String>,
        child: impl ManagedElement,
    ) -> Self {
        let child = if reason.is_some() {
            child.lock()
        } else {
            child
        };
        Self {
            id: id.into(),
            reason: reason.map(Into::into),
            child: child.into_any_element(),
        }
    }
}
impl RenderOnce for ManagedControl {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ring = hsla(palette(cx).ring);
        let locked = self.reason.is_some();
        let focus = self
            .reason
            .as_ref()
            .map(|_| focus_handle(&self.id, window, cx));
        div()
            .id(self.id)
            .flex()
            .rounded(r(6.))
            .when_some(focus, |el, focus| {
                el.track_focus(&focus)
                    .focus_visible(move |s| s.shadow(vec![gpui::BoxShadow { color: ring, offset: gpui::point(gpui::px(0.), gpui::px(0.)), blur_radius: gpui::px(0.), spread_radius: gpui::px(2.), inset: true }]))
            })
            .when_some(self.reason, |el, reason| {
                el.role(gpui::Role::Group)
                    .aria_label(reason.clone())
                    .aria_description("Managed by your organization")
                    .tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx))
            })
            .child(
                div()
                    .when(locked, |el| el.opacity(0.5 / 0.64))
                    .child(self.child),
            )
    }
}
pub fn notice(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let color = hsla(palette(cx).muted_foreground);
    h_flex()
        .items_start()
        .gap(r(8.))
        .text_xs()
        .line_height(r(19.5))
        .text_color(color)
        .child(
            div()
                .mt(r(2.))
                .child(crate::settings::controls::icon(Icon::Building, 14., color)),
        )
        .child(div().flex_1().min_w_0().child(text.into()))
}
#[derive(IntoElement)]
pub struct ManagedAttribution { text: String, support_url: Option<String> }
pub fn attribution(policy: &serde_json::Value, _: &App) -> ManagedAttribution {
    ManagedAttribution { text: t3_core::managed_policy::attribution(policy), support_url: policy["supportUrl"].as_str().map(str::to_owned) }
}
impl RenderOnce for ManagedAttribution {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        div().child(notice(self.text, cx)).children(self.support_url.map(|url| {
            let focus = focus_handle(&"managed-support".into(), window, cx);
            let keyboard_url = url.clone();
            div().id("managed-support").track_focus(&focus).role(gpui::Role::Link).aria_label("Get help")
                .text_xs().underline().cursor_pointer().child("Get help")
                .on_click(move |_, _, cx| cx.open_url(&url))
                .on_key_down(move |event, _, cx| { if crate::settings::controls::activates(event) { cx.stop_propagation(); cx.open_url(&keyboard_url); } })
        }))
    }
}
