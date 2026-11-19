//! Linked issue rows inside the thread details card (`ThreadDetailsIssueRows.tsx`,
//! `pingdotgg/t3code#19270`, `pingdotgg/t3code#38282`, `pingdotgg/t3code#38995`).

use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, FontWeight, IntoElement, ParentElement,
    SharedString, Styled, Task, div, prelude::*, px,
};
use gpui_component::{h_flex, v_flex};
use t3_core::issues::wire::{
    IssueDetail, IssueRef, ThreadIssueLink, decode, method, thread_issue_key_of,
};
use t3_core::right_panel::IssueTarget;

use super::controls::{hover_fill, section_with_options};
use crate::app_state::AppState;
use crate::i18n::t;
use crate::icons::Icon;
use crate::issues::{
    HeldIssueKey, environment_owner_for, held_issue_detail, hold_issue_detail,
    issues_supported_for, read_for_owner, settle, state_glyph, tracker_icon,
};
use crate::right_panel::tooltip_view;
use crate::theme::{hsla, palette};

/// Lazily loads and caches live `IssueDetail` for each issue linked to the active thread so
/// `ThreadDetailsIssueRows` can show live state, title, and comment count.
#[derive(Default)]
pub(super) struct LinkedIssuesFeed {
    details: HashMap<String, IssueDetail>,
    in_flight: HashSet<String>,
    failed: HashSet<String>,
    _tasks: Vec<Task<()>>,
}

impl LinkedIssuesFeed {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn reset(&mut self) {
        self.in_flight.clear();
        self.failed.clear();
        self._tasks.clear();
    }

    pub(super) fn sync<V: 'static>(
        &mut self,
        state: &Entity<AppState>,
        active: bool,
        links: &[ThreadIssueLink],
        get: fn(&mut V) -> &mut LinkedIssuesFeed,
        cx: &mut Context<V>,
    ) {
        if !active || links.is_empty() {
            return;
        }
        let app = state.read(cx);
        let env_id = app
            .environments
            .active
            .as_deref()
            .or_else(|| app.environment())
            .map(str::to_owned);
        if !issues_supported_for(app, env_id.as_deref(), cx) {
            return;
        }
        let owner = environment_owner_for(app, env_id.as_deref());
        for link in links {
            let key = thread_issue_key_of(&link.issue_key());
            if self.details.contains_key(&key)
                || self.in_flight.contains(&key)
                || self.failed.contains(&key)
            {
                continue;
            }
            let reference = IssueRef {
                project_id: link.project_id.clone(),
                tracker: link.tracker.clone(),
                host: (!link.host.is_empty()).then(|| link.host.clone()),
                scope: link.scope.clone(),
                key: link.key.clone(),
            };
            let held_key = HeldIssueKey::of(app, env_id.as_deref(), &reference);
            if let Some(cached) = held_key.as_ref().and_then(|k| held_issue_detail(k, cx)) {
                self.details.insert(key, cached);
                continue;
            }
            let task = read_for_owner(
                app,
                owner.as_ref(),
                method::DETAIL,
                reference.payload(),
                decode::<IssueDetail>,
            );
            self.in_flight.insert(key.clone());
            let key_done = key.clone();
            self._tasks.push(cx.spawn(async move |this, cx| {
                let result = settle(task).await;
                let _ = this.update(cx, |view, cx| {
                    let feed = get(view);
                    feed.in_flight.remove(&key_done);
                    match result {
                        Ok(detail) => {
                            if let Some(hk) = held_key.clone() {
                                hold_issue_detail(hk, &detail, cx);
                            }
                            feed.details.insert(key_done, detail);
                        }
                        Err(_) => {
                            feed.failed.insert(key_done);
                        }
                    }
                    cx.notify();
                });
            }));
        }
    }

    pub(super) fn detail_for(&self, link: &ThreadIssueLink, state: &AppState, cx: &App) -> Option<IssueDetail> {
        let key = thread_issue_key_of(&link.issue_key());
        if let Some(detail) = self.details.get(&key) {
            return Some(detail.clone());
        }
        let reference = IssueRef {
            project_id: link.project_id.clone(),
            tracker: link.tracker.clone(),
            host: (!link.host.is_empty()).then(|| link.host.clone()),
            scope: link.scope.clone(),
            key: link.key.clone(),
        };
        let env_id = state
            .environments
            .active
            .as_deref()
            .or_else(|| state.environment());
        HeldIssueKey::of(state, env_id, &reference)
            .and_then(|k| held_issue_detail(&k, cx))
    }

    /// Renders the linked-issues section (`ThreadDetailsIssueRows`) when `links` is non-empty.
    pub(super) fn render(
        &self,
        state: &Entity<AppState>,
        links: &[ThreadIssueLink],
        separated: bool,
        compact: bool,
        on_open: impl Fn(&mut App) + Clone + 'static,
        cx: &App,
    ) -> Option<AnyElement> {
        if links.is_empty() {
            return None;
        }
        let app = state.read(cx);
        let env_id = app
            .environments
            .active
            .as_deref()
            .or_else(|| app.environment())
            .map(str::to_owned);
        let supported = issues_supported_for(app, env_id.as_deref(), cx);
        let p = palette(cx);
        let fill = hover_fill(cx);

        let rows: Vec<AnyElement> = links
            .iter()
            .enumerate()
            .map(|(idx, link)| {
                let detail = self.detail_for(link, app, cx);
                let (icon, icon_color) = match &detail {
                    Some(detail) => state_glyph(&detail.state, cx),
                    None => (Icon::CircleDot, hsla(p.muted_foreground)),
                };
                let title = detail
                    .as_ref()
                    .map(|d| d.title.clone())
                    .unwrap_or_else(|| link.title.clone());
                let comment_count = detail.as_ref().map_or(0, |d| d.comment_count);
                let t_icon = tracker_icon(&link.tracker);
                let tooltip_text: SharedString = match &detail {
                    Some(d) => format!("{} {} · {} — {}", d.scope, d.display_key, d.state.name, d.title).into(),
                    None => format!("{} {} — {}", link.scope, link.display_key, title).into(),
                };
                let state_click = state.clone();
                let link_click = link.clone();
                let env_click = env_id.clone();
                let on_open_click = on_open.clone();
                let row_id = SharedString::from(format!("thread-details-issue-{idx}"));
                let debug_id = row_id.to_string();

                v_flex()
                    .id(row_id)
                    .debug_selector(move || debug_id.clone())
                    .w_full()
                    .min_w_0()
                    .px(px(10.))
                    .py(px(6.))
                    .gap(px(2.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(move |style| style.bg(fill))
                    .tooltip(move |window, cx| tooltip_view(tooltip_text.clone(), window, cx))
                    .on_click(move |event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        let mods = event.modifiers();
                        if mods.platform || mods.control || mods.shift || mods.alt || !supported {
                            cx.open_url(&link_click.url);
                            return;
                        }
                        let target = IssueTarget::from_thread_link(env_click.clone(), &link_click);
                        state_click.update(cx, |app, cx| app.open_issue_surface(target, cx));
                        on_open_click(cx);
                    })
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap(px(8.))
                            .items_start()
                            .child(div().pt(px(2.)).flex_none().child(icon.sized(px(15.)).text_color(icon_color)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.foreground).opacity(0.9))
                                    .child(title),
                            ),
                    )
                    .child(
                        h_flex()
                            .pl(px(23.))
                            .gap(px(6.))
                            .items_center()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            // Key displayed once (`pingdotgg/t3code#38995`).
                            .child(div().flex_none().child(link.display_key.clone()))
                            .child(t_icon.sized(px(12.)))
                            .when(comment_count > 0, |this| {
                                this.child(
                                    h_flex()
                                        .flex_none()
                                        .gap(px(3.))
                                        .items_center()
                                        .child(Icon::MessageSquare.sized(px(11.)))
                                        .child(comment_count.to_string()),
                                )
                            }),
                    )
                    .into_any_element()
            })
            .collect();

        let heading = Some(SharedString::from(t(cx, "Linked issues").to_owned()));
        Some(
            section_with_options(separated, heading, compact, cx)
                .children(rows)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use serde_json::json;
    use t3_core::issues::wire::{IssueDetail, ThreadIssueLink, decode};

    use super::*;

    fn test_runtime() -> tokio::runtime::Handle {
        static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RT.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test tokio runtime")
        })
        .handle()
        .clone()
    }

    #[gpui::test]
    fn thread_details_issue_rows_render_and_use_held_detail(cx: &mut gpui::TestAppContext) {
        let handle = test_runtime();
        let link: ThreadIssueLink = decode(json!({
            "tracker": "github",
            "host": "github.com",
            "scope": "acme/web",
            "key": "19",
            "projectId": "p1",
            "url": "https://github.com/acme/web/issues/19",
            "source": "manual",
            "linkedAt": "2026-01-03T00:00:00Z",
            "displayKey": "#19",
            "title": "Linked issue fallback title"
        }))
        .unwrap();
        let detail: IssueDetail = decode(json!({
            "tracker": "github",
            "host": "github.com",
            "scope": "acme/web",
            "key": "19",
            "displayKey": "#19",
            "title": "Live updated issue title",
            "url": "https://github.com/acme/web/issues/19",
            "state": { "category": "closed", "name": "Done", "closeReason": "completed" },
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-04T00:00:00Z",
            "projectId": "p1",
            "projectTitle": "Web",
            "commentCount": 5
        }))
        .unwrap();

        struct Host {
            state: Entity<AppState>,
            feed: LinkedIssuesFeed,
            links: Vec<ThreadIssueLink>,
        }
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().size_full().children(self.feed.render(
                    &self.state,
                    &self.links,
                    true,
                    false,
                    |_| {},
                    cx,
                ))
            }
        }

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
        });
        let state = cx.new(|cx| AppState::new(handle.clone(), cx));
        state.update(cx, |app, _| {
            app.environments.active = Some("env-a".into());
        });

        let link_for_host = link.clone();
        let state_for_host = state.clone();
        let window = cx.add_window(|_, _| Host {
            state: state_for_host,
            feed: LinkedIssuesFeed::new(),
            links: vec![link_for_host],
        });
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("thread-details-issue-0").is_some());

        vcx.update(|_, cx| {
            let feed = LinkedIssuesFeed::new();
            assert!(feed.render(&state, &[], false, false, |_| {}, cx).is_none());

            // Holding live detail surfaces updated title and comment count immediately.
            let reference = IssueRef {
                project_id: link.project_id.clone(),
                tracker: link.tracker.clone(),
                host: Some(link.host.clone()),
                scope: link.scope.clone(),
                key: link.key.clone(),
            };
            let key = HeldIssueKey::of(state.read(cx), Some("env-a"), &reference).unwrap();
            hold_issue_detail(key, &detail, cx);
            let resolved = feed.detail_for(&link, state.read(cx), cx).unwrap();
            assert_eq!(resolved.title, "Live updated issue title");
            assert_eq!(resolved.comment_count, 5);
        });
    }
}
