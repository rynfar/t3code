//! Pull request link hover preview card (`PullRequestLinkPreview.tsx`).
//! Delays 350ms on hover before reading `pullRequests.preview` (and `pullRequests.activity` when
//! the URL names a comment or commit anchor), falls back to any held summary/list entry, and
//! closes after 120ms when the pointer leaves.

use std::time::Duration;

use gpui::{
    AnyElement, App, BoxShadow, Context, Entity, FontWeight, IntoElement, ParentElement, Styled, Task, div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::pull_requests::anchor::{Anchor, link_commit, parse_anchor};
use t3_core::pull_requests::detail::{LinkSubject, StateKind, link_subject, review_outcome};
use t3_core::pull_requests::wire::{Activity, ListEntry, Preview, decode};
use t3_core::pull_requests::{PrRef, method};
use t3_core::sidebar::time::{compact_relative_label, parse_iso};

use super::controls::{avatar, outcome_glyph, state_glyph};
use super::{owner_for_pr, read_for_owner, settle};
use crate::app_state::AppState;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

pub const PREVIEW_OPEN_DELAY: Duration = Duration::from_millis(350);
pub const PREVIEW_CLOSE_DELAY: Duration = Duration::from_millis(120);

#[derive(Debug, Clone)]
pub struct PreviewTarget {
    pub environment_id: Option<String>,
    pub reference: PrRef,
    pub url: String,
    pub fallback: Option<Preview>,
}

impl PreviewTarget {
    #[cfg(test)]
    pub fn from_entry(entry: &ListEntry) -> Self {
        Self::from_entry_in_env(entry, None)
    }

    pub fn from_entry_in_env(entry: &ListEntry, environment_id: Option<String>) -> Self {
        PreviewTarget {
            environment_id,
            reference: PrRef::of(entry),
            url: entry.url.clone(),
            fallback: Some(Preview {
                project_id: entry.project_id.clone(),
                repository: entry.repository.clone(),
                number: entry.number,
                title: entry.title.clone(),
                url: entry.url.clone(),
                author: entry.author.clone(),
                state: entry.state,
                is_draft: entry.is_draft,
                created_at: entry.created_at.clone(),
                updated_at: Some(entry.updated_at.clone()),
                observed_at: entry.observed_at.clone(),
            }),
        }
    }
}

/// Manages delayed hover open/close and live `pullRequests.preview` + optional `activity` reads
/// for a host view (`PullRequestsPage`, `ThreadPullRequestsPanel`, or `DetailView`).
#[derive(Default)]
pub struct LinkPreviewController {
    active: Option<PreviewTarget>,
    loaded: Option<Preview>,
    activity: Option<Activity>,
    error: Option<String>,
    _timer: Option<Task<()>>,
    _fetch: Option<Task<()>>,
}

impl LinkPreviewController {
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.active.is_some()
    }

    #[cfg(test)]
    pub fn active_url(&self) -> Option<&str> {
        self.active.as_ref().map(|target| target.url.as_str())
    }

    pub fn hover<V: 'static>(
        &mut self,
        state: &Entity<AppState>,
        target: Option<PreviewTarget>,
        get: fn(&mut V) -> &mut LinkPreviewController,
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
        if self.active.as_ref().is_some_and(|current| current.url == target.url && current.reference == target.reference) {
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
        self.activity = None;
        self.error = None;
        self._timer = None;
        self._fetch = None;
    }

    pub(super) fn open_now<V: 'static>(
        &mut self,
        state: &Entity<AppState>,
        target: PreviewTarget,
        get: fn(&mut V) -> &mut LinkPreviewController,
        cx: &mut Context<V>,
    ) {
        let names_subject = matches!(parse_anchor(&target.url), Some(Anchor::Timeline(_))) || link_commit(&target.url).is_some();
        let app = state.read(cx);
        let owner = owner_for_pr(app, target.environment_id.as_deref(), &target.reference.project_id);
        let preview_task = read_for_owner(app, owner.as_ref(), method::PREVIEW, target.reference.payload(), decode::<Preview>);
        let activity_task = if names_subject {
            read_for_owner(app, owner.as_ref(), method::ACTIVITY, target.reference.payload(), decode::<Activity>)
        } else {
            None
        };
        let target_url = target.url.clone();
        self.active = Some(target);
        self.loaded = None;
        self.activity = None;
        self.error = None;
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let preview = settle(preview_task).await;
            let activity = match activity_task {
                Some(task) => settle(Some(task)).await.ok(),
                None => None,
            };
            let _ = this.update(cx, |view, cx| {
                let ctrl = get(view);
                if ctrl.active.as_ref().map(|t| t.url.as_str()) != Some(target_url.as_str()) {
                    return;
                }
                match preview {
                    Ok(preview) => {
                        ctrl.loaded = Some(preview);
                        ctrl.error = None;
                    }
                    Err(err) => ctrl.error = Some(err),
                }
                if let Some(activity) = activity {
                    ctrl.activity = Some(activity);
                }
                cx.notify();
            });
        }));
    }

    /// Renders the floating preview card when open and a preview or fallback summary is ready.
    pub fn render_card(&self, cx: &App) -> Option<AnyElement> {
        let target = self.active.as_ref()?;
        let card = self.loaded.as_ref().or(target.fallback.as_ref())?;
        let subject = self.activity.as_ref().and_then(|activity| link_subject(&target.url, activity));
        Some(render_preview_card(card, subject.as_ref(), self.loaded.is_some(), cx))
    }
}

pub fn render_preview_card(card: &Preview, subject: Option<&LinkSubject>, from_host: bool, cx: &App) -> AnyElement {
    let p = palette(cx);
    let kind = StateKind::of(card.state, card.is_draft);
    let (icon, color) = state_glyph(kind, cx);
    let author_login = card.author.as_ref().map_or("ghost", |a| a.login.as_str());
    let author_label = match card.author.as_ref() {
        None => "ghost".to_owned(),
        Some(actor) => match actor.name.as_deref() {
            Some(name) if !name.is_empty() && name != actor.login => format!("{name} (@{})", actor.login),
            _ => actor.login.clone(),
        },
    };
    let opened = parse_iso(&card.created_at).map(|at| compact_relative_label(at, now_ms()));
    let body: AnyElement = match subject {
        Some(LinkSubject::Comment { author, created_at, review_state, excerpt }) => {
            let login = author.as_ref().map_or("ghost", |a| a.login.as_str());
            let when = parse_iso(created_at).map(|at| compact_relative_label(at, now_ms())).unwrap_or_default();
            let outcome = review_outcome(review_state.as_deref());
            v_flex()
                .mt(px(6.))
                .gap(px(4.))
                .child(
                    h_flex()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(avatar(login, 16., cx))
                        .child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(login.to_owned()))
                        .children(outcome.map(|o| {
                            let (o_icon, o_color) = outcome_glyph(o, cx);
                            h_flex().gap(px(3.)).text_color(o_color).child(o_icon.sized(px(12.))).child(o.label())
                        }))
                        .when(outcome.is_none(), |this| this.child("commented"))
                        .child("·")
                        .child(when),
                )
                .children(excerpt.as_ref().map(|text| div().text_xs().line_height(px(18.)).text_color(hsla(p.foreground)).child(text.clone())))
                .into_any_element()
        }
        Some(LinkSubject::Commit(commit)) => {
            let author = commit.authors.first();
            let when = parse_iso(&commit.committed_date).map(|at| compact_relative_label(at, now_ms())).unwrap_or_default();
            let mono = cx.theme().mono_font_family.clone();
            v_flex()
                .mt(px(6.))
                .gap(px(4.))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(commit.message_headline.clone()))
                .child(
                    h_flex()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(div().font_family(mono).child(t3_core::pull_requests::detail::short_sha(&commit.oid).to_owned()))
                        .children(author.map(|a| h_flex().gap(px(4.)).child(avatar(&a.login, 14., cx)).child(a.login.clone())))
                        .child("·")
                        .child(when),
                )
                .into_any_element()
        }
        None => v_flex()
            .gap(px(6.))
            .child(div().mt(px(4.)).text_sm().font_weight(FontWeight::MEDIUM).line_height(px(18.)).text_color(hsla(p.foreground)).child(card.title.clone()))
            .child(
                h_flex()
                    .gap(px(6.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(avatar(author_login, 16., cx))
                    .child(div().min_w_0().truncate().child(author_label))
                    .when(from_host, |this| {
                        this.children(opened.map(|when| h_flex().flex_none().gap(px(4.)).child("·").child(format!("opened {when}"))))
                    }),
            )
            .into_any_element(),
    };

    v_flex()
        .id("pr-link-preview-card")
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
        .child(
            h_flex()
                .gap(px(6.))
                .min_w_0()
                .text_size(px(11.))
                .text_color(hsla(p.muted_foreground))
                .child(div().min_w_0().truncate().child(card.repository.clone()))
                .child(div().flex_none().child(format!("#{}", card.number)))
                .child("·")
                .child(h_flex().flex_none().gap(px(4.)).text_color(color).child(icon.sized(px(12.))).child(kind.label())),
        )
        .child(body)
        .into_any_element()
}
