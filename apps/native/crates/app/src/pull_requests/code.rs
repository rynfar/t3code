//! The Code tab (`PullRequestCodeTab.tsx`): the patch from `POST /api/pull-requests/diff`, read in
//! slices (`nextCursor`) and parsed on the network runtime, shown in the diff panel.

use std::sync::Arc;

use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Task, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_core::pull_requests::wire::{DiffResult, decode};
use t3_core::pull_requests::{PrRef, failure_message};

use super::controls::{Button, Size, Variant};
use crate::app_state::AppState;
use crate::diff::{DiffPanel, External, Parsed};
use crate::theme::{hsla, palette};

/// A cap on slices held, so a huge change cannot grow without bound.
const MAX_SLICES: usize = 10;

pub struct CodeTab {
    state: Entity<AppState>,
    reference: PrRef,
    panel: Entity<DiffPanel>,
    /// Patches read so far, in order.
    slices: Vec<String>,
    next_cursor: Option<String>,
    truncated: bool,
    pending: bool,
    error: Option<String>,
    _fetch: Option<Task<()>>,
}

impl CodeTab {
    pub fn new(state: Entity<AppState>, reference: PrRef, cx: &mut Context<Self>) -> Self {
        let panel = cx.new(|cx| DiffPanel::detached(state.clone(), cx));
        let mut tab = CodeTab { state, reference, panel, slices: Vec::new(), next_cursor: None, truncated: false, pending: false, error: None, _fetch: None };
        tab.fetch(None, cx);
        tab
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.slices.clear();
        self.next_cursor = None;
        self.fetch(None, cx);
    }

    fn fetch(&mut self, cursor: Option<String>, cx: &mut Context<Self>) {
        let Some(client) = self.state.read(cx).client_handle() else {
            self.error = Some(super::NOT_CONNECTED.to_owned());
            return self.show(cx);
        };
        let mut payload = self.reference.payload();
        if let Some(cursor) = &cursor {
            payload["cursor"] = json!(cursor);
        }
        let held = self.slices.join("\n");
        let task = self.state.read(cx).runtime().spawn(async move {
            let value = client.pull_request_diff(payload).await.map_err(|failure| failure_message(failure.tag.as_deref(), &failure.error, &failure.message))?;
            let diff: DiffResult = decode(value)?;
            let patch = if held.is_empty() { diff.patch.clone() } else { format!("{held}\n{}", diff.patch) };
            let parsed = Parsed::new(&patch, diff.truncated || diff.next_cursor.is_some(), None, None, None);
            Ok::<_, String>((diff, parsed))
        });
        self.pending = true;
        self.error = None;
        self.show(cx);
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                match result {
                    Ok((diff, parsed)) => {
                        this.slices.push(diff.patch);
                        this.next_cursor = diff.next_cursor.filter(|_| this.slices.len() < MAX_SLICES);
                        this.truncated = diff.truncated;
                        this.show_parsed(parsed, cx);
                        if let Some(cursor) = this.next_cursor.clone() {
                            this.fetch(Some(cursor), cx);
                        }
                    }
                    Err(error) => {
                        this.error = Some(error);
                        this.show(cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn key(&self) -> String {
        format!("pull-request:{}", self.reference.key())
    }

    fn show_parsed(&mut self, parsed: Arc<Parsed>, cx: &mut Context<Self>) {
        let key = self.key();
        self.panel.update(cx, |panel, cx| panel.show_external(External { key, parsed: Some(parsed), pending: false, error: None }, cx));
    }

    fn show(&mut self, cx: &mut Context<Self>) {
        if !self.slices.is_empty() && self.error.is_none() {
            return cx.notify();
        }
        let external = External { key: self.key(), parsed: None, pending: self.pending, error: self.error.clone() };
        self.panel.update(cx, |panel, cx| panel.show_external(external, cx));
        cx.notify();
    }
}

impl Render for CodeTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let failed = self.error.is_some();
        let first = self.slices.is_empty();
        let partial = (!first && self.pending) || failed;
        v_flex()
            .size_full()
            .min_h_0()
            .pt(px(12.))
            .child(div().flex_1().min_h_0().child(self.panel.clone()))
            .when(partial, |this| {
                this.child(
                    h_flex()
                        .flex_none()
                        .justify_center()
                        .gap(px(8.))
                        .py(px(8.))
                        .border_t_1()
                        .border_color(hsla(p.border))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .when(!failed, |this| this.child("Loading more files..."))
                        .when(failed, |this| {
                            let cursor = self.next_cursor.clone();
                            this.when(!first, |this| this.child("The rest of this diff could not be loaded.")).child(
                                Button::new("pr-code-retry", Variant::Outline, Size::Xs).label("Retry").on_click(cx.listener(move |this, _, _, cx| this.fetch(cursor.clone(), cx))),
                            )
                        }),
                )
            })
    }
}
