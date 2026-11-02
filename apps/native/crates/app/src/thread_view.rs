//! Transcript: one list row per visible turn item, in a bottom-aligned `list()` so streaming
//! keeps the tail pinned. Updates splice only the changed range (prefix/suffix match on item
//! identity), so a streaming token remeasures one row, not the transcript.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Context, Entity, IntoElement, ListAlignment, ListState, ParentElement, Render, Styled, Subscription, Window, div, list, prelude::*, px};
use gpui_component::ActiveTheme;
use gpui_component::text::TextViewState;
use t3_core::turn_item::{TurnItem, TurnItemKind};

use crate::app_state::{AppEvent, AppState};
use crate::transcript_row::render_item;

pub const CONTENT_MAX_WIDTH: f32 = 768.;

struct Row {
    id: String,
    item: Arc<TurnItem>,
}

pub struct ThreadView {
    state: Entity<AppState>,
    list: ListState,
    rows: Vec<Row>,
    markdown: HashMap<String, Entity<TextViewState>>,
    thread_id: Option<String>,
    _subscription: Subscription,
}

impl ThreadView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::ThreadChanged | AppEvent::SelectionChanged) {
                this.sync_rows(cx);
            }
        });
        ThreadView {
            state,
            list: ListState::new(0, ListAlignment::Bottom, px(600.)),
            rows: Vec::new(),
            markdown: HashMap::new(),
            thread_id: None,
            _subscription: subscription,
        }
    }

    fn sync_rows(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let selected = state.selected_thread.clone();
        let next: Vec<Row> = state
            .thread
            .projection
            .as_ref()
            .map(|projection| {
                projection
                    .visible_turn_items
                    .iter()
                    .map(|row| Row { id: row.source_item_id.clone(), item: row.item.clone() })
                    .collect()
            })
            .unwrap_or_default();

        if selected != self.thread_id {
            self.thread_id = selected;
            self.markdown.clear();
            self.rows = next;
            self.sync_markdown(0..self.rows.len(), cx);
            self.list.reset(self.rows.len());
            cx.notify();
            return;
        }

        let same = |a: &Row, b: &Row| a.id == b.id && Arc::ptr_eq(&a.item, &b.item);
        let prefix = self.rows.iter().zip(&next).take_while(|(a, b)| same(a, b)).count();
        if prefix == self.rows.len() && prefix == next.len() {
            return;
        }
        let max_suffix = self.rows.len().min(next.len()) - prefix;
        let suffix = self.rows.iter().rev().zip(next.iter().rev()).take(max_suffix).take_while(|(a, b)| same(a, b)).count();
        let old_range = prefix..self.rows.len() - suffix;
        let new_count = next.len() - suffix - prefix;
        let live: std::collections::HashSet<&str> = next.iter().map(|row| row.id.as_str()).collect();
        self.markdown.retain(|id, _| live.contains(id.as_str()));
        self.rows = next;
        self.sync_markdown(prefix..prefix + new_count, cx);
        self.list.splice(old_range, new_count);
        cx.notify();
    }

    /// Markdown rows keep a `TextViewState` per item; streamed text updates it incrementally.
    fn sync_markdown(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) {
        for row in &self.rows[range] {
            let Some(text) = markdown_source(&row.item) else { continue };
            match self.markdown.get(&row.id) {
                Some(state) => state.update(cx, |state, cx| state.set_text(text, cx)),
                None => {
                    let state = cx.new(|cx| TextViewState::markdown(text, cx).selectable(true));
                    self.markdown.insert(row.id.clone(), state);
                }
            }
        }
    }
}

fn markdown_source(item: &TurnItem) -> Option<&str> {
    match &item.kind {
        TurnItemKind::AssistantMessage { text, .. } => Some(text),
        TurnItemKind::ProposedPlan { markdown, .. } => Some(markdown),
        _ => None,
    }
}

impl Render for ThreadView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.state.read(cx).thread.projection.is_none();
        let theme = cx.theme();
        if loading {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Loading thread…")
                .into_any_element();
        }
        list(
            self.list.clone(),
            cx.processor(|this, ix: usize, _window, cx| {
                let Some(row) = this.rows.get(ix) else { return div().into_any_element() };
                let markdown = this.markdown.get(&row.id).cloned();
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .px_6()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(CONTENT_MAX_WIDTH))
                            .py_1p5()
                            .child(render_item(&row.item, markdown, cx)),
                    )
                    .into_any_element()
            }),
        )
        .size_full()
        .pt_4()
        .into_any_element()
    }
}
