//! Issue link hover preview card (`apps/web/src/components/issue/IssueLinkPreview.tsx`,
//! `pingdotgg/t3code#38294`).
//!
//! Delays 350ms on hover before reading `issues.detail` (painting immediately from `HeldIssues`
//! or an optional fallback entry), closes after 120ms when the pointer leaves, and degrades to a
//! compact URL tooltip when the read fails and no cached detail exists.

use std::time::Duration;

use gpui::{
    AnyElement, App, BoxShadow, Context, Entity, FontWeight, IntoElement, ParentElement, Styled,
    Task, div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::{h_flex, v_flex};
use t3_core::issues::presentation::issue_author_label;
use t3_core::issues::wire::{IssueDetail, IssueListEntry, IssueRef, decode, method};
use t3_core::sidebar::time::{compact_relative_label, parse_iso};

use super::{
    HeldIssueKey, environment_owner_for, held_issue_detail, hold_issue_detail, read_for_owner,
    settle, state_glyph,
};
use crate::app_state::AppState;
use crate::pull_requests::controls::avatar;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

pub const PREVIEW_OPEN_DELAY: Duration = Duration::from_millis(350);
pub const PREVIEW_CLOSE_DELAY: Duration = Duration::from_millis(120);

#[derive(Debug, Clone)]
pub struct IssuePreviewTarget {
    pub environment_id: Option<String>,
    pub reference: IssueRef,
    pub original_url: String,
    pub fallback: Option<IssueListEntry>,
}

impl IssuePreviewTarget {
    pub fn from_entry(environment_id: Option<String>, entry: &IssueListEntry) -> Self {
        Self {
            environment_id,
            reference: IssueRef::of(entry),
            original_url: entry.url.clone(),
            fallback: Some(entry.clone()),
        }
    }
}

/// Manages delayed hover open/close and live `issues.detail` reads for issue links.
#[derive(Default)]
pub struct IssueLinkPreviewController {
    active: Option<IssuePreviewTarget>,
    loaded: Option<IssueDetail>,
    error: Option<String>,
    _timer: Option<Task<()>>,
    _fetch: Option<Task<()>>,
}

impl IssueLinkPreviewController {
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.active.is_some()
    }

    #[cfg(test)]
    pub fn active_url(&self) -> Option<&str> {
        self.active.as_ref().map(|target| target.original_url.as_str())
    }

    #[cfg(test)]
    pub fn has_error(&self) -> bool {
        self.error.is_some()
    }

    pub fn hover<V: 'static>(
        &mut self,
        state: &Entity<AppState>,
        target: Option<IssuePreviewTarget>,
        get: fn(&mut V) -> &mut IssueLinkPreviewController,
        cx: &mut Context<V>,
    ) {
        self._timer = None;
        let Some(target) = target else {
            if self.active.is_none() {
                return;
            }
            self._timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(PREVIEW_CLOSE_DELAY).await;
                let _ = this.update(cx, |view, cx| {
                    let ctrl = get(view);
                    ctrl.clear();
                    cx.notify();
                });
            }));
            return;
        };
        if self.active.as_ref().is_some_and(|current| {
            current.original_url == target.original_url && current.reference == target.reference
        }) {
            return;
        }
        let state = state.clone();
        self._timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREVIEW_OPEN_DELAY).await;
            let _ = this.update(cx, |view, cx| {
                let ctrl = get(view);
                ctrl.open_now(&state, target, get, cx);
                cx.notify();
            });
        }));
    }

    pub fn clear(&mut self) {
        self.active = None;
        self.loaded = None;
        self.error = None;
        self._timer = None;
        self._fetch = None;
    }

    pub fn open_now<V: 'static>(
        &mut self,
        state: &Entity<AppState>,
        target: IssuePreviewTarget,
        get: fn(&mut V) -> &mut IssueLinkPreviewController,
        cx: &mut Context<V>,
    ) {
        let held_key =
            HeldIssueKey::of(state.read(cx), target.environment_id.as_deref(), &target.reference);
        let cached = held_key
            .as_ref()
            .and_then(|key| held_issue_detail(key, cx));
        let owner = environment_owner_for(state.read(cx), target.environment_id.as_deref());
        let task = read_for_owner(
            state.read(cx),
            owner.as_ref(),
            method::DETAIL,
            target.reference.payload(),
            decode::<IssueDetail>,
        );
        let target_url = target.original_url.clone();
        self.active = Some(target);
        self.loaded = cached;
        self.error = None;
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |view, cx| {
                let ctrl = get(view);
                if ctrl.active.as_ref().map(|t| t.original_url.as_str())
                    != Some(target_url.as_str())
                {
                    return;
                }
                match result {
                    Ok(detail) => {
                        if let Some(key) = held_key.clone() {
                            hold_issue_detail(key, &detail, cx);
                        }
                        ctrl.loaded = Some(detail);
                        ctrl.error = None;
                    }
                    Err(failure) => {
                        ctrl.error = Some(failure.message);
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Renders the floating preview card when detail or fallback is ready, or the URL tooltip
    /// fallback when the read failed with no cached data (`IssueLinkPreview.tsx`).
    pub fn render_card(&self, cx: &App) -> Option<AnyElement> {
        let target = self.active.as_ref()?;
        if let Some(detail) = &self.loaded {
            return Some(render_issue_preview_card(detail, cx));
        }
        if let Some(entry) = &target.fallback {
            return Some(render_issue_preview_entry(entry, cx));
        }
        if self.error.is_some() {
            return Some(render_url_fallback(&target.original_url, cx));
        }
        None
    }
}

fn preview_card_shell(id: &'static str, cx: &App) -> gpui::Stateful<gpui::Div> {
    let p = palette(cx);
    div()
        .id(id)
        .w(px(320.))
        .max_w(px(360.))
        .p(px(12.))
        .rounded(px(10.))
        .border_1()
        .border_color(hsla(p.border))
        .bg(hsla(p.popover))
        .shadow(vec![BoxShadow {
            color: gpui_hsla(0., 0., 0., 0.35),
            offset: point(px(0.), px(8.)),
            blur_radius: px(24.),
            spread_radius: px(-8.),
            inset: false,
        }])
}

/// Renders the `IssuePreviewCardBody` for a loaded `IssueDetail`.
pub fn render_issue_preview_card(detail: &IssueDetail, cx: &App) -> AnyElement {
    let p = palette(cx);
    let (icon, color) = state_glyph(&detail.state, cx);
    let author_login = detail
        .author
        .as_ref()
        .map_or("ghost", |actor| actor.login.as_str());
    let author_label = issue_author_label(detail.author.as_ref());
    let opened = parse_iso(&detail.created_at)
        .map(|at| compact_relative_label(at, now_ms()))
        .unwrap_or_default();

    preview_card_shell("issue-link-preview-card", cx)
        .child(
            v_flex()
                .min_w_0()
                .child(
                    h_flex()
                        .min_w_0()
                        .gap(px(6.))
                        .text_size(px(11.))
                        .text_color(hsla(p.muted_foreground))
                        .child(div().min_w_0().truncate().child(detail.scope.clone()))
                        .child(div().flex_none().child(detail.display_key.clone()))
                        .child("·")
                        .child(
                            h_flex()
                                .flex_none()
                                .gap(px(4.))
                                .text_color(color)
                                .child(icon.sized(px(12.)))
                                .child(detail.state.name.clone()),
                        ),
                )
                .child(
                    div()
                        .mt(px(4.))
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .line_height(px(18.))
                        .text_color(hsla(p.foreground))
                        .child(detail.title.clone()),
                )
                .child(
                    h_flex()
                        .mt(px(8.))
                        .min_w_0()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(avatar(author_login, 16., cx))
                        .child(div().min_w_0().truncate().child(author_label))
                        .child("·")
                        .child(
                            div()
                                .flex_none()
                                .child(crate::i18n::t_fmt(
                                    cx,
                                    "opened {_gt_value_1}",
                                    &[("_gt_value_1", opened.into())],
                                )),
                        ),
                ),
        )
        .into_any_element()
}

/// Renders the preview card from a list row summary while `issues.detail` is still in flight.
pub fn render_issue_preview_entry(entry: &IssueListEntry, cx: &App) -> AnyElement {
    let p = palette(cx);
    let (icon, color) = state_glyph(&entry.state, cx);
    let author_login = entry
        .author
        .as_ref()
        .map_or("ghost", |actor| actor.login.as_str());
    let author_label = issue_author_label(entry.author.as_ref());
    let opened = parse_iso(&entry.created_at)
        .map(|at| compact_relative_label(at, now_ms()))
        .unwrap_or_default();

    preview_card_shell("issue-link-preview-card", cx)
        .child(
            v_flex()
                .min_w_0()
                .child(
                    h_flex()
                        .min_w_0()
                        .gap(px(6.))
                        .text_size(px(11.))
                        .text_color(hsla(p.muted_foreground))
                        .child(div().min_w_0().truncate().child(entry.scope.clone()))
                        .child(div().flex_none().child(entry.display_key.clone()))
                        .child("·")
                        .child(
                            h_flex()
                                .flex_none()
                                .gap(px(4.))
                                .text_color(color)
                                .child(icon.sized(px(12.)))
                                .child(entry.state.name.clone()),
                        ),
                )
                .child(
                    div()
                        .mt(px(4.))
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .line_height(px(18.))
                        .text_color(hsla(p.foreground))
                        .child(entry.title.clone()),
                )
                .child(
                    h_flex()
                        .mt(px(8.))
                        .min_w_0()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(avatar(author_login, 16., cx))
                        .child(div().min_w_0().truncate().child(author_label))
                        .child("·")
                        .child(
                            div()
                                .flex_none()
                                .child(crate::i18n::t_fmt(
                                    cx,
                                    "opened {_gt_value_1}",
                                    &[("_gt_value_1", opened.into())],
                                )),
                        ),
                ),
        )
        .into_any_element()
}

fn render_url_fallback(original_url: &str, cx: &App) -> AnyElement {
    let p = palette(cx);
    preview_card_shell("issue-link-preview-url-fallback", cx)
        .child(
            div()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(original_url.to_owned()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use serde_json::json;
    use t3_core::issues::wire::{IssueListEntry, IssueRef, IssueTrackerKind, decode};

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
    fn issue_link_preview_controller_renders_fallback_and_url_error_cleanly(
        cx: &mut gpui::TestAppContext,
    ) {
        let handle = test_runtime();
        let entry: IssueListEntry = decode(json!({
            "tracker": "github",
            "host": "github.com",
            "scope": "acme/web",
            "key": "12",
            "displayKey": "#12",
            "title": "Preview hover card",
            "url": "https://github.com/acme/web/issues/12",
            "author": { "login": "alice", "name": "Alice" },
            "state": { "category": "open", "name": "Open" },
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z",
            "projectId": "p1",
            "projectTitle": "Web"
        }))
        .unwrap();

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let state = cx.new(|cx| AppState::new(handle.clone(), cx));
            struct Host {
                preview: IssueLinkPreviewController,
            }
            let host = cx.new(|_| Host { preview: IssueLinkPreviewController::default() });
            host.update(cx, |host, cx| {
                assert!(!host.preview.is_open());
                host.preview.open_now(
                    &state,
                    IssuePreviewTarget::from_entry(Some("env-a".into()), &entry),
                    |h| &mut h.preview,
                    cx,
                );
                assert!(host.preview.is_open());
                assert_eq!(
                    host.preview.active_url(),
                    Some("https://github.com/acme/web/issues/12")
                );
                assert!(host.preview.render_card(cx).is_some());

                // Without fallback and with error, renders URL tooltip fallback.
                host.preview.clear();
                host.preview.active = Some(IssuePreviewTarget {
                    environment_id: Some("env-a".into()),
                    reference: IssueRef {
                        project_id: "p1".into(),
                        tracker: IssueTrackerKind::GitHub,
                        host: Some("github.com".into()),
                        scope: "acme/web".into(),
                        key: "99".into(),
                    },
                    original_url: "https://github.com/acme/web/issues/99".into(),
                    fallback: None,
                });
                host.preview.error = Some("Not found".into());
                assert!(host.preview.has_error());
                assert!(host.preview.render_card(cx).is_some());

                host.preview.clear();
                assert!(!host.preview.is_open());
                assert!(host.preview.render_card(cx).is_none());
            });
        });
    }
}
