//! Sidebar thread search (`searchSidebarThreads` in `Sidebar.logic.ts`, the server half in
//! `client-runtime/src/state/threadSearch.ts`): title and branch matches ranked locally, then
//! threads listed only because their messages match, in the order the server ranked them.

use std::time::Duration;

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, Task, div, prelude::*, px, uniform_list};
use serde_json::json;

use super::SidebarPane;
use crate::app_state::AppState;
use crate::theme::{hsla, palette};

const SEARCH_THREADS: &str = "orchestration.searchThreads";
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
const RESULT_HEIGHT: f32 = 36.;

#[derive(Default)]
pub struct SearchState {
    query: String,
    results: Vec<String>,
    content_matches: Vec<(String, String)>,
    highlighted: usize,
    request: Option<Task<()>>,
}

/// The server needs a word of three or more letters to match messages.
fn searches_messages(query: &str) -> bool {
    query.split(|c: char| !c.is_alphanumeric()).any(|word| word.chars().count() >= 3)
}

impl SearchState {
    pub fn is_active(&self) -> bool {
        !self.query.trim().is_empty()
    }

    pub fn highlighted_id(&self) -> Option<&str> {
        self.results.get(self.highlighted).map(String::as_str)
    }

    pub fn move_highlight(&mut self, step: isize) {
        let count = self.results.len() as isize;
        if count > 0 {
            self.highlighted = (self.highlighted as isize + step).rem_euclid(count) as usize;
        }
    }

    pub fn set_query(&mut self, query: String, state: &Entity<AppState>, cx: &mut Context<SidebarPane>) {
        if query == self.query {
            return;
        }
        self.query = query;
        self.highlighted = 0;
        self.content_matches.clear();
        self.request = None;
        self.refresh(state, cx);
        let trimmed = self.query.trim().to_owned();
        if trimmed.chars().count() < 2 || !searches_messages(&trimmed) {
            return;
        }
        let Some(client) = state.read(cx).client_handle() else { return };
        let runtime = state.read(cx).runtime().clone();
        let state = state.clone();
        self.request = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let query = trimmed.clone();
            let result = runtime.spawn(async move { client.request(SEARCH_THREADS, json!({ "query": query })).await }).await;
            let Ok(Ok(value)) = result else { return };
            let matches: Vec<(String, String)> = value
                .get("matches")
                .and_then(|matches| matches.as_array())
                .map(|matches| {
                    matches
                        .iter()
                        .filter_map(|m| Some((m.get("threadId")?.as_str()?.to_owned(), m.get("snippet").and_then(|s| s.as_str()).unwrap_or("").to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.search.query.trim() == trimmed {
                    this.search.content_matches = matches;
                    this.search.refresh(&state, cx);
                    cx.notify();
                }
            });
        }));
    }

    /// Re-ranks against the current model, keeping the highlight on the same thread.
    pub fn refresh(&mut self, state: &Entity<AppState>, cx: &mut Context<SidebarPane>) {
        if !self.is_active() {
            self.results.clear();
            return;
        }
        let previous = self.highlighted_id().map(str::to_owned);
        let model = state.read(cx).sidebar.model.clone();
        let now = crate::sidebar::controller::now_ms();
        let mut results = t3_core::sidebar::search_titles(&model, &self.query, now);
        for (id, _) in &self.content_matches {
            if model.thread(id).is_some() && !results.contains(id) {
                results.push(id.clone());
            }
        }
        self.highlighted = previous.and_then(|id| results.iter().position(|candidate| *candidate == id)).unwrap_or(0);
        self.results = results;
    }

    fn snippet(&self, thread_id: &str) -> Option<&str> {
        self.content_matches.iter().find(|(id, _)| id == thread_id).map(|(_, snippet)| snippet.as_str()).filter(|s| !s.is_empty())
    }
}

impl SidebarPane {
    pub(super) fn render_search_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        if self.search.results.is_empty() {
            return div().px(px(16.)).py(px(24.)).text_xs().text_color(hsla(p.muted_foreground).opacity(0.6)).child("No matching threads").into_any_element();
        }
        let count = self.search.results.len();
        uniform_list(
            "sidebar-search-results",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| range.map(|index| this.render_search_result(index, cx)).collect::<Vec<_>>()),
        )
        .flex_1()
        .into_any_element()
    }

    fn render_search_result(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(id) = self.search.results.get(index).cloned() else { return div().into_any_element() };
        let state = self.state.read(cx);
        let Some(thread) = state.sidebar.model.thread(&id) else { return div().h(px(RESULT_HEIGHT)).into_any_element() };
        let project = state.sidebar.model.project_names.get(&thread.project_id).cloned();
        let highlighted = index == self.search.highlighted;
        let snippet = self.search.snippet(&id).map(|s| SharedString::from(s.to_owned()));
        let title: SharedString = thread.title.clone().into();
        div().h(px(RESULT_HEIGHT)).px(px(8.)).py(px(1.)).child(
            div()
                .id(SharedString::from(format!("search-result-{id}")))
                .size_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(10.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(highlighted, |this| this.bg(hsla(p.sidebar_row_active)))
                .hover(|this| this.bg(hsla(p.sidebar_row_hover)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
                        .children(snippet.map(|snippet| div().truncate().text_xs().text_color(hsla(p.sidebar_secondary_label)).child(snippet))),
                )
                .children(project.map(|name| div().flex_none().max_w(px(96.)).truncate().text_xs().text_color(hsla(p.sidebar_secondary_label)).child(name)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.clear_search(window, cx);
                    this.state.update(cx, |state, cx| state.select_thread(&id, cx));
                })),
        )
        .into_any_element()
    }
}
