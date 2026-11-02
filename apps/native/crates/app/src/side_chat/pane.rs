//! `SideChatPane.tsx`: a side chat tab's header (title, Bring back, Delete, Close) over the side
//! chat's own transcript and composer.

use gpui::{Context, Entity, IntoElement, ParentElement, PromptLevel, Render, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use t3_core::lineage::latest_merge_back_run;
use t3_core::right_panel::Surface;
use t3_core::side_chat::{BRING_BACK_HINT, bring_back_blocked_reason};

use crate::app_state::{AppEvent, AppState};
use crate::composer::Composer;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};
use crate::thread_view::ThreadView;

pub struct SideChatPane {
    state: Entity<AppState>,
    thread_id: String,
    transcript: Entity<ThreadView>,
    composer: Entity<Composer>,
    _subscription: Subscription,
}

impl SideChatPane {
    pub fn new(state: Entity<AppState>, thread_id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let transcript = cx.new(|cx| ThreadView::embedded(state.clone(), thread_id.clone(), window, cx));
        let composer = cx.new(|cx| Composer::embedded(state.clone(), thread_id.clone(), window, cx));
        let subscription = cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Shell | AppEvent::SideThread | AppEvent::Selection) {
                cx.notify();
            }
        });
        SideChatPane { state, thread_id, transcript, composer, _subscription: subscription }
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        let id = Surface::side_chat(&self.thread_id).id;
        self.state.update(cx, |state, cx| state.close_surface(&id, cx));
    }

    /// Confirms first when the user asked to (`confirmThreadDelete`), with the OS dialog the web uses.
    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.state.read(cx).thread_shell(&self.thread_id).map(|thread| thread.title.clone()) else { return };
        let id = self.thread_id.clone();
        if !crate::settings::store::client(cx).confirm_thread_delete() {
            self.state.update(cx, |state, cx| state.delete_side_chat(&id, cx));
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Delete side chat \"{title}\"?"),
            Some("This permanently clears its conversation history."),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.ok() == Some(0) {
                let _ = this.update(cx, |this, cx| this.state.update(cx, |state, cx| state.delete_side_chat(&id, cx)));
            }
        })
        .detach();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let state = self.state.read(cx);
        let shell = state.thread_shell(&self.thread_id);
        let title = shell.map_or_else(|| "Side chat".to_owned(), |thread| thread.title.clone());
        let busy = state.side_threads.busy.contains(&self.thread_id);
        let parent_active = state.selected_thread.as_deref().is_some_and(|parent| state.is_thread_running(parent));
        let merge_run = state.thread_state(&self.thread_id).projection.as_ref().and_then(|projection| latest_merge_back_run(&projection.runs));
        let blocked = bring_back_blocked_reason(parent_active, merge_run);
        let missing = shell.is_none();
        h_flex()
            .h(px(36.))
            .flex_none()
            .items_center()
            .gap(px(4.))
            .pl(px(12.))
            .pr(px(4.))
            .border_b_1()
            .border_color(hsla(p.border))
            .child(div().flex_1().min_w_0().truncate().text_xs().font_weight(gpui::FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
            .child(
                Button::new("side-chat-bring-back")
                    .size(Size::Xs)
                    .label("Bring back")
                    .disabled(blocked.is_some() || busy || missing)
                    .tooltip(blocked.unwrap_or(BRING_BACK_HINT))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let id = this.thread_id.clone();
                        this.state.update(cx, |state, cx| state.bring_back_side_chat(&id, cx));
                    })),
            )
            .child(
                Button::new("side-chat-delete")
                    .size(Size::IconXs)
                    .icon(Icon::Trash2)
                    .disabled(missing || busy)
                    .tooltip("Delete side chat")
                    .on_click(cx.listener(|this, _, window, cx| this.delete(window, cx))),
            )
            .child(
                Button::new("side-chat-close")
                    .variant(Variant::Ghost)
                    .size(Size::IconXs)
                    .icon(Icon::X)
                    .tooltip("Close side chat")
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
            .into_any_element()
    }
}

impl Render for SideChatPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let muted = hsla(palette(cx).muted_foreground);
        let state = self.state.read(cx);
        let missing = state.thread_shell(&self.thread_id).is_none();
        let body = if missing {
            let deleted = state.shell.synchronized.then(|| div().p(px(12.)).text_xs().text_color(muted).child("This side chat was deleted."));
            div().flex_1().children(deleted).into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .child(div().flex_1().min_h_0().child(self.transcript.clone()))
                .child(self.composer.clone())
                .into_any_element()
        };
        v_flex().size_full().min_h_0().child(header).child(body)
    }
}
