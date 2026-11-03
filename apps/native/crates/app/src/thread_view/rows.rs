//! The virtualized transcript, cached separately from minimap hover and motion.

use super::*;
use gpui::WeakEntity;

pub(super) struct TimelineRows {
    owner: WeakEntity<ThreadView>,
    _subscription: Subscription,
}

impl TimelineRows {
    pub fn new(owner: &Entity<ThreadView>, cx: &mut Context<Self>) -> Self {
        Self { owner: owner.downgrade(), _subscription: cx.observe(owner, |_, _, cx| {
            #[cfg(feature = "snapshot")]
            crate::snapshot::record_render("rows-owner-notify", cx);
            cx.notify();
        }) }
    }
}

impl Render for TimelineRows {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "snapshot")]
        crate::snapshot::record_render("transcript", cx);
        self.owner.update(cx, |owner, cx| owner.render_rows(window, cx)).unwrap_or_else(|_| div().into_any_element())
    }
}

impl ThreadView {
    fn render_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        self.clock = Clock::now();
        self.window_height = f32::from(window.viewport_size().height);
        self.markdown.begin_frame();
        self.media.update(cx, |media, cx| media.begin_frame(window, cx));
        let scope = crate::media::MediaScope {
            owner: cx.entity_id(),
            thread_id: self.thread_id.clone(),
            workspace_root: self.workspace_root.clone(),
            viewer: self.viewer.downgrade(),
        };
        crate::media::set_scope(scope, cx);
        list(
            self.list.clone(),
            cx.processor(|this, ix: usize, _window, cx| {
                let offset = this.header_count();
                if ix < offset {
                    let content = match &this.history {
                        Some(control) => history_control(control.has_more, control.loading, control.error.as_deref(), cx),
                        None => div().into_any_element(),
                    };
                    return lane(content, true, crate::theme::chat_max_width(cx), this.gutter());
                }
                let Some(row) = this.rows.get(ix - offset).cloned() else {
                    return lane(this.render_footer(cx), ix == 0, crate::theme::chat_max_width(cx), this.gutter());
                };
                let content = this.with_env(|env, markdown| render_row(&row, env, markdown, cx));
                let lane = lane(content, ix == 0, crate::theme::chat_max_width(cx), this.gutter());
                if this.anchor_gap.borrow().as_ref().is_none_or(|(id, _)| *id != row.id) {
                    return lane;
                }
                let gap = this.anchor_gap.clone();
                // Scrolling up into rows above this one in the same frame, so it never shows moved.
                let request = canvas(
                    move |bounds, window, _| {
                        if let Some((_, gap)) = gap.borrow_mut().take() {
                            window.request_autoscroll(Bounds::new(point(bounds.left(), bounds.top() - px(gap)), size(bounds.size.width, px(1.))));
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full();
                div().relative().child(lane).child(request).into_any_element()
            }),
        )
        .size_full().into_any_element()
    }
}
