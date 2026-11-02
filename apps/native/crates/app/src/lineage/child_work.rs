//! `NativeChildWorkDialog.tsx`: a read-only view of what a provider-native subagent's own
//! children did, from `orchestration.getNativeChildWork`. Loads only while open.

use gpui::{
    AnyElement, App, BoxShadow, Context, Entity, EventEmitter, FocusHandle, FontWeight, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
    SharedString, Styled, Window, anchored, deferred, div, point, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::lineage::{GET_NATIVE_CHILD_WORK, NativeChildStep, NativeChildWork, decode_native_child_work, native_child_work_request};

use crate::app_state::AppState;
use crate::composer::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};

pub struct Dismissed;

enum Load {
    Loading,
    Loaded(Vec<NativeChildWork>),
    Failed(String),
}

pub struct ChildWorkDialog {
    state: Entity<AppState>,
    thread_id: String,
    load: Load,
    pending: bool,
    focus: FocusHandle,
}

impl EventEmitter<Dismissed> for ChildWorkDialog {}

impl ChildWorkDialog {
    pub fn new(state: Entity<AppState>, thread_id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut dialog = ChildWorkDialog { state, thread_id, load: Load::Loading, pending: false, focus };
        dialog.refresh(cx);
        dialog
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let Some(task) = self.state.read(cx).request(GET_NATIVE_CHILD_WORK, native_child_work_request(&self.thread_id), decode_native_child_work) else {
            self.load = Load::Failed("Not connected.".to_owned());
            return;
        };
        self.pending = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                this.load = match result {
                    Ok(children) => Load::Loaded(children),
                    Err(error) => Load::Failed(error),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn render_step(step: &NativeChildStep, cx: &App) -> AnyElement {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let calls = (!step.tool_calls.is_empty())
            .then(|| step.tool_calls.iter().map(|call| format!("{} {}", call.name, call.input)).collect::<Vec<_>>().join("\n"));
        v_flex()
            .min_w_0()
            .gap(px(4.))
            .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child(step.heading()))
            .children(calls.map(|calls| div().text_xs().font_family(mono.clone()).text_color(hsla(p.foreground)).child(calls)))
            .children(step.text.clone().map(|text| {
                div()
                    .id(SharedString::from(format!("native-step-{}", step.index)))
                    .max_h(px(192.))
                    .overflow_y_scroll()
                    .text_xs()
                    .font_family(mono.clone())
                    .text_color(hsla(p.foreground))
                    .child(text)
            }))
            .into_any_element()
    }

    fn render_child(child: &NativeChildWork, cx: &App) -> AnyElement {
        let p = palette(cx);
        let role = if child.role.is_empty() { "Subagent".to_owned() } else { child.role.clone() };
        let note = |text: &'static str| div().text_xs().text_color(hsla(p.muted_foreground)).child(text);
        v_flex()
            .min_w_0()
            .gap(px(8.))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(role))
            .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(child.prompt.clone()))
            .when(child.truncated, |this| this.child(note("Earlier steps are not shown.")))
            .child(match &child.steps {
                None => note("No transcript yet.").into_any_element(),
                Some(steps) => v_flex()
                    .gap(px(8.))
                    .pl(px(12.))
                    .border_l_1()
                    .border_color(hsla(p.border))
                    .children(steps.iter().map(|step| Self::render_step(step, cx)))
                    .into_any_element(),
            })
            .into_any_element()
    }
}


impl Render for ChildWorkDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let muted = hsla(p.muted_foreground);
        let body: Vec<AnyElement> = match &self.load {
            Load::Loading => vec![div().text_sm().text_color(muted).child("Loading…").into_any_element()],
            Load::Failed(error) => vec![div().text_sm().text_color(hsla(p.destructive_foreground)).child(error.clone()).into_any_element()],
            Load::Loaded(children) if children.is_empty() => {
                vec![div().text_sm().text_color(muted).child("The provider has no record of which subagents this call started.").into_any_element()]
            }
            Load::Loaded(children) => children.iter().map(|child| Self::render_child(child, cx)).collect(),
        };
        let viewport = window.viewport_size();
        let dark = cx.theme().is_dark();
        deferred(
            anchored().position(point(px(0.), px(0.))).child(
                div()
                    .id("native-child-work-backdrop")
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(16.))
                    .bg(hsla(p.dialog_backdrop))
                    .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.emit(Dismissed)))
                    .child(
                        v_flex()
                            .id("native-child-work")
                            .track_focus(&self.focus)
                            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "escape" {
                                    cx.stop_propagation();
                                    cx.emit(Dismissed);
                                }
                            }))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .w_full()
                            .max_w(px(672.))
                            .max_h(viewport.height - px(32.))
                            .relative()
                            .rounded(px(16.))
                            .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                            .shadow(vec![BoxShadow {
                                color: gpui::black().opacity(if dark { 0.9 } else { 0.65 }),
                                offset: point(px(0.), px(24.)),
                                blur_radius: px(64.),
                                spread_radius: px(-24.),
                                inset: false,
                            }])
                            .child(
                                v_flex()
                                    .p(px(24.))
                                    .gap(px(8.))
                                    .child(div().text_xl().line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child("Subagent work"))
                                    .child(div().text_sm().text_color(muted).child("Read only. These subagents cannot take messages.")),
                            )
                            .child(v_flex().id("native-child-work-panel").flex_1().min_h_0().overflow_y_scroll().px(px(24.)).pt(px(4.)).pb(px(24.)).gap(px(16.)).children(body))
                            .child(
                                h_flex().justify_end().px(px(24.)).pb(px(16.)).child(
                                    Button::new("native-child-work-refresh", ButtonVariant::Outline, ButtonSize::Sm)
                                        .label("Refresh")
                                        .disabled(self.pending)
                                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                                ),
                            ),
                    ),
            ),
        )
        .with_priority(1)
    }
}
