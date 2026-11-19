//! Ports `web/src/components/ManagedPolicy.tsx`; the disabled primitive owns input suppression.
use crate::{
    icons::Icon,
    settings::controls::{Button, Select, Switch, focus_handle, r},
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
impl ManagedElement for Select {
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
                el.tab_index(0)
                    .role(gpui::Role::Group)
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
        let fg = hsla(palette(cx).foreground);
        h_flex()
            .flex_wrap()
            .items_start()
            .gap(r(6.))
            .child(notice(self.text, cx))
            .children(self.support_url.map(|url| {
                let focus = focus_handle(&"managed-support".into(), window, cx);
                let keyboard_url = url.clone();
                div()
                    .id("managed-support")
                    .track_focus(&focus)
                    .role(gpui::Role::Link)
                    .aria_label("Get help")
                    .text_xs()
                    .line_height(r(19.5))
                    .text_color(fg)
                    .underline()
                    .cursor_pointer()
                    .child("Get help")
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .on_key_down(move |event, _, cx| {
                        if crate::settings::controls::activates(event) {
                            cx.stop_propagation();
                            cx.open_url(&keyboard_url);
                        }
                    })
            }))
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use crate::settings::controls::{ButtonSize, ButtonVariant, SelectOption};
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct ManagedFrame {
        locked_clicks: Arc<AtomicUsize>,
        unlocked_clicks: Arc<AtomicUsize>,
    }

    impl Render for ManagedFrame {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let locked_clicks = self.locked_clicks.clone();
            let unlocked_clicks = self.unlocked_clicks.clone();
            let policy = json!({
                "organization": "Example Corp",
                "supportUrl": "https://support.example.com"
            });
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap(r(8.))
                .child(
                    div().debug_selector(|| "managed-locked-button".to_owned()).child(
                        ManagedControl::new(
                            "test-managed-btn-locked",
                            Some("Managed by Example Corp.".to_owned()),
                            Button::new("btn-locked", ButtonVariant::Outline, ButtonSize::Sm)
                                .label("Locked")
                                .on_click(move |_, _, _| {
                                    locked_clicks.fetch_add(1, Ordering::SeqCst);
                                }),
                        ),
                    ),
                )
                .child(
                    div().debug_selector(|| "managed-unlocked-button".to_owned()).child(
                        ManagedControl::new(
                            "test-managed-btn-unlocked",
                            None,
                            Button::new("btn-unlocked", ButtonVariant::Outline, ButtonSize::Sm)
                                .label("Unlocked")
                                .on_click(move |_, _, _| {
                                    unlocked_clicks.fetch_add(1, Ordering::SeqCst);
                                }),
                        ),
                    ),
                )
                .child(
                    div().debug_selector(|| "managed-locked-switch".to_owned()).child(
                        ManagedControl::new(
                            "test-managed-switch-locked",
                            Some("Managed by Example Corp.".to_owned()),
                            Switch::new("switch-locked", false),
                        ),
                    ),
                )
                .child(
                    div().debug_selector(|| "managed-locked-select".to_owned()).child(
                        ManagedControl::new(
                            "test-managed-select-locked",
                            Some("Managed by Example Corp.".to_owned()),
                            Select::new("select-locked", vec![SelectOption::new("a", "Option A")])
                                .selected("a"),
                        ),
                    ),
                )
                .child(
                    div()
                        .debug_selector(|| "managed-attribution".to_owned())
                        .child(attribution(&policy, cx)),
                )
        }
    }

    #[gpui::test]
    fn managed_control_locks_button_switch_and_select_and_renders_attribution(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let locked_clicks = Arc::new(AtomicUsize::new(0));
        let unlocked_clicks = Arc::new(AtomicUsize::new(0));
        let window = cx.add_window(|_, _| ManagedFrame {
            locked_clicks: locked_clicks.clone(),
            unlocked_clicks: unlocked_clicks.clone(),
        });
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let locked_bounds = vcx.debug_bounds("managed-locked-button").expect("locked button bounds");
        let unlocked_bounds = vcx.debug_bounds("managed-unlocked-button").expect("unlocked button bounds");
        assert!(vcx.debug_bounds("managed-locked-switch").is_some());
        assert!(vcx.debug_bounds("managed-locked-select").is_some());
        assert!(vcx.debug_bounds("managed-attribution").is_some());

        vcx.simulate_click(locked_bounds.center(), gpui::Modifiers::none());
        vcx.simulate_click(unlocked_bounds.center(), gpui::Modifiers::none());
        vcx.run_until_parked();
        assert_eq!(locked_clicks.load(Ordering::SeqCst), 0, "locked ManagedControl must suppress clicks");
        assert_eq!(unlocked_clicks.load(Ordering::SeqCst), 1, "unlocked ManagedControl must pass clicks through");
    }
}
