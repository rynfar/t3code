//! Thread hover card (`ThreadHoverCard.tsx`), rendered lazily inside the glass tooltip.

use gpui::{Context, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, px};

use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// `ThreadHoverCard` in the glass tooltip: the title, then project, branch, model and any error.
pub struct ThreadHoverCard {
    pub title: SharedString,
    pub project: Option<SharedString>,
    pub favicon: crate::project_icon::ProjectVisual,
    pub branch: Option<SharedString>,
    pub model: Option<SharedString>,
    /// The error line and whether it is a usage limit.
    pub error: Option<bool>,
}

impl Render for ThreadHoverCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let line = |icon: Icon, text: SharedString| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_w_0()
                .child(icon.sized(px(12.)).flex_none().text_color(hsla(p.muted_foreground)))
                .child(div().min_w_0().truncate().text_color(hsla(p.foreground).opacity(0.75)).child(text))
        };
        let error = self.error.map(|usage_limit| {
            let color = if usage_limit { hsla(p.warning) } else { hsla(p.error) };
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_color(color)
                .child(Icon::CircleAlert.sized(px(12.)).flex_none())
                .child(div().truncate().child(if usage_limit { "Usage limit reached" } else { "Error occurred" }))
        });
        div()
            .relative()
            .max_w(px(320.))
            .rounded(px(6.))
            .shadow_lg()
            .child(crate::glass::backdrop(6., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
            .child(
                div()
                    .relative()
                    .px(px(12.))
                    .py(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().min_w_0().truncate().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(self.title.clone()))
                    .child(
                        div()
                            .pl(px(2.))
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .children(self.project.clone().map(|project| {
                                let icon = self.favicon.render(crate::project_icon::size(12., cx), hsla(p.icon_muted), cx);
                                div().flex().items_center().gap(px(8.)).min_w_0().child(icon).child(div().min_w_0().truncate().text_color(hsla(p.foreground).opacity(0.75)).child(project))
                            }))
                            .children(self.branch.clone().map(|branch| line(Icon::GitBranch, branch)))
                            .children(self.model.clone().map(|model| line(Icon::Bot, model)))
                            .children(error),
                    ),
            )
    }
}
