//! Limits (`UsageLimitsPooled.tsx`): one section per provider, a card per pooled window with
//! the share left and its pace, and a bar of one segment per account. A segment's details open
//! on click, with the account's reset credits. External usage pages and notices follow. Times
//! read against the moment Limits last refreshed, so the page never ticks on its own.

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, Hsla, IntoElement, ParentElement, SharedString, Styled, Window, anchored, deferred, div, prelude::*, px, relative, rgba};
use gpui_component::{h_flex, v_flex};
use t3_core::providers::driver_label;
use t3_core::server_config::UsageWindow;
use t3_core::timeline;
use t3_core::usage::limits::{
    self, CHATGPT_USAGE_URL, LimitAccount, LimitPace, LimitPool, LimitPoolMember, LimitPoolWindow, account_hue, account_initials, account_title, accent_rgba,
    format_resets_in, pace_remaining_percent, remaining_percent, reset_credits_summary,
};

use super::controls::{ActionButton, ButtonSize, ButtonVariant, brand_mark, driver_color, heading, icon, r};
use super::{SegmentKey, UsageView};
use crate::icons::Icon;
use crate::right_panel::{focusable, tooltip_view};
use crate::theme::{hsla, palette};

const CURSOR_KEYCHAIN_COPY: &str = "Requires access to your Cursor login in macOS Keychain.";

fn pace_icon(pace: LimitPace, id: SharedString, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let glyph = match pace {
        LimitPace::Ahead => Icon::TrendingUp,
        LimitPace::On => Icon::Gauge,
        LimitPace::Under => Icon::TrendingDown,
    };
    let label: SharedString = pace.label().into();
    div().id(id).flex_none().child(icon(glyph, 14., hsla(p.muted_foreground))).tooltip(move |window, cx| tooltip_view(label.clone(), window, cx))
}

/// `oklch(0.85 0.08 h)` and `oklch(0.35 0.1 h)`: an account chip's fill and text for its hue.
fn chip_colors(hue: u32) -> (Hsla, Hsla) {
    let oklch = |lightness: f32, chroma: f32| -> Hsla {
        let angle = (hue as f32).to_radians();
        let (a, b) = (chroma * angle.cos(), chroma * angle.sin());
        let l_ = lightness + 0.396_337_78 * a + 0.215_803_76 * b;
        let m_ = lightness - 0.105_561_346 * a - 0.063_854_17 * b;
        let s_ = lightness - 0.089_484_18 * a - 1.291_485_5 * b;
        let (l, m, s) = (l_.powi(3), m_.powi(3), s_.powi(3));
        let linear = [
            4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
            -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
            -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
        ];
        let encode = |value: f32| {
            let value = value.clamp(0., 1.);
            if value <= 0.003_130_8 { value * 12.92 } else { 1.055 * value.powf(1. / 2.4) - 0.055 }
        };
        let [red, green, blue] = linear.map(|channel| (encode(channel) * 255.).round() as u32);
        rgba((red << 24) | (green << 16) | (blue << 8) | 0xff).into()
    };
    (oklch(0.85, 0.08), oklch(0.35, 0.1))
}

fn account_chip(email: &str) -> impl IntoElement {
    let (fill, text) = chip_colors(account_hue(email));
    div()
        .size(r(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(fill)
        .text_color(text)
        .text_size(r(10.))
        .line_height(r(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(account_initials(email))
}

/// `AccountName`: the display name, else an email chip, else the driver's label.
fn account_name(account: &LimitAccount) -> AnyElement {
    match (&account.display_name, &account.email) {
        (Some(name), _) => div().min_w_0().truncate().child(name.clone()).into_any_element(),
        (None, Some(email)) => account_chip(email).into_any_element(),
        (None, None) => div().min_w_0().truncate().child(driver_label(&account.driver)).into_any_element(),
    }
}

fn segment_color(account: &LimitAccount, fallback: Hsla) -> Hsla {
    accent_rgba(account.accent_color.as_deref()).map_or(fallback, |accent| rgba(accent).into())
}

impl UsageView {
    pub(super) fn render_limits(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let data = self.data.limits(self.state.read(cx));
        let environment = self.data.environment(self.state.read(cx));
        let show_cursor = !self.data.hidden().contains(&t3_core::usage::UsageProvider::Cursor)
            && !t3_core::usage::cursor_keychain_prompt(&[(environment.summary.as_deref(), environment.needs_cursor_keychain_access)]).is_empty();
        let prompt_at = data.pools.iter().position(|pool| pool.driver == "claudeAgent").max(data.pools.iter().position(|pool| pool.driver == "codex")).map_or(0, |index| index + 1);
        let empty = data.pools.is_empty() && data.notices.is_empty() && !show_cursor && data.links.is_empty();
        let mut sections: Vec<AnyElement> = Vec::new();
        for (index, pool) in data.pools.iter().enumerate() {
            if index == prompt_at && show_cursor {
                sections.push(self.render_cursor_limits(cx));
            }
            sections.push(self.render_pool(pool, window, cx));
        }
        if prompt_at >= data.pools.len() && show_cursor {
            sections.push(self.render_cursor_limits(cx));
        }
        v_flex()
            .gap(r(32.))
            .when(empty, |this| {
                this.child(div().text_size(r(14.)).text_color(hsla(p.muted_foreground)).child("No provider on the selected environments reports subscription limits."))
            })
            .children(sections)
            .children(data.links.iter().enumerate().map(|(index, link)| {
                let url = link.url.clone();
                h_flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(r(12.))
                    .p(r(16.))
                    .rounded(r(12.))
                    .border_1()
                    .border_color(hsla(p.border))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(r(12.))
                            .when(link.url == CHATGPT_USAGE_URL, |this| this.child(brand_mark("codex", 20., hsla(p.foreground), cx)))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap(r(4.))
                                    .child(heading(link.label.clone(), cx))
                                    .child(div().text_size(r(12.)).text_color(hsla(p.muted_foreground)).child(link.accounts.join(", ")))
                                    .children(if link.url == CHATGPT_USAGE_URL {
                                        Some("View usage in ChatGPT with your connected account.".to_owned())
                                    } else {
                                        link.message.clone()
                                    }
                                    .map(|text| div().max_w(r(576.)).text_size(r(12.)).text_color(hsla(p.muted_foreground)).child(text))),
                            ),
                    )
                    .child(
                        ActionButton::new(("usage-external", index), "Manage usage", ButtonVariant::GhostMuted, ButtonSize::Xs)
                            .trailing(Icon::ExternalLink)
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
            }))
            .when(!data.notices.is_empty(), |this| {
                this.child(
                    h_flex()
                        .items_start()
                        .gap(r(8.))
                        .px(r(14.))
                        .py(r(12.))
                        .rounded(r(12.))
                        .border_1()
                        .border_color(hsla(p.warning).opacity(0.32))
                        .bg(hsla(p.warning_surface))
                        .text_size(r(14.))
                        .text_color(hsla(p.warning_foreground))
                        .child(div().pt(r(2.)).child(icon(Icon::TriangleAlert, 16., hsla(p.warning_foreground))))
                        .child(v_flex().min_w_0().gap(r(2.)).children(data.notices.iter().map(|notice| div().font_weight(FontWeight::MEDIUM).child(notice.clone())))),
                )
            })
    }

    /// `CursorEnableLimits`.
    fn render_cursor_limits(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        v_flex()
            .gap(r(12.))
            .child(h_flex().gap(r(8.)).child(div().size(r(20.)).flex().items_center().justify_center().child(brand_mark("cursor", 16., hsla(p.foreground), cx))).child(heading("Cursor", cx)))
            .child(
                v_flex()
                    .items_start()
                    .gap(r(12.))
                    .p(r(16.))
                    .rounded(r(8.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .child(div().text_size(r(12.)).text_color(hsla(p.muted_foreground)).child(CURSOR_KEYCHAIN_COPY))
                    .child(
                        ActionButton::new("usage-cursor-limits-enable", "Enable", ButtonVariant::Outline, ButtonSize::Sm)
                            .disabled(self.data.keychain_pending)
                            .on_click(cx.listener(|this, _, _, cx| this.enable_cursor_keychain(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_pool(&mut self, pool: &LimitPool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let color = driver_color(&pool.driver, p);
        v_flex()
            .gap(r(12.))
            .child(
                h_flex()
                    .gap(r(8.))
                    .child(div().size(r(20.)).flex().items_center().justify_center().text_color(hsla(p.foreground).opacity(0.8)).child(brand_mark(&pool.driver, 16., hsla(p.foreground).opacity(0.8), cx)))
                    .child(heading(driver_label(&pool.driver), cx)),
            )
            .children(limits::display_limit_windows(pool).into_iter().map(|pool_window| self.render_pool_window(&pool.driver, pool_window, color, window, cx)))
            .into_any_element()
    }

    fn render_pool_window(&mut self, driver: &str, pool: &LimitPoolWindow, color: Hsla, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let details = (driver == "cursor").then(|| limits::cursor_usage_window_details(&pool.id)).flatten();
        let next_refill = pool.resets.iter().find(|reset| reset.restores_percent > 0).filter(|_| pool.columns.len() > 1);
        let wide = Self::page_width(window) >= 768.;
        let roomy = Self::page_width(window).min(1024.) - 48. - 34. - 200. >= 672.;
        let summary = v_flex()
            .gap(r(4.))
            .when(wide, |this| this.w(r(176.)).flex_none())
            .child(heading(details.map_or_else(|| pool.label.clone(), |details| details.label.to_owned()), cx))
            .child(
                h_flex()
                    .items_baseline()
                    .gap(r(8.))
                    .child(div().text_size(r(30.)).line_height(r(36.)).font_weight(FontWeight::SEMIBOLD).child(format!("{}%", pool.remaining_percent)))
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .text_size(r(14.))
                            .text_color(hsla(p.muted_foreground))
                            .child("left")
                            .children(pool.pace.map(|pace| pace_icon(pace, format!("usage-pace-{driver}-{}", pool.id).into(), cx))),
                    ),
            )
            .children(next_refill.map(|reset| div().text_size(r(12.)).font_weight(FontWeight::MEDIUM).child(format!("↻ +{}%", reset.restores_percent))));
        let bar = self.render_pool_bar(driver, pool, color, roomy, window, cx);
        let body = if wide {
            h_flex().items_center().gap(r(24.)).child(summary).child(div().flex_1().min_w_0().child(bar)).into_any_element()
        } else {
            v_flex().gap(r(12.)).child(summary).child(bar).into_any_element()
        };
        v_flex()
            .gap(r(12.))
            .p(r(16.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.6))
            .child(body)
            .children(details.map(|details| div().text_size(r(12.)).text_color(hsla(p.muted_foreground)).child(details.description)))
            .into_any_element()
    }

    fn render_pool_bar(&mut self, driver: &str, pool: &LimitPoolWindow, color: Hsla, roomy: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let multiple = pool.columns.len() > 1;
        let segments: Vec<AnyElement> = pool
            .columns
            .iter()
            .enumerate()
            .map(|(index, (account, member))| match member {
                Some(member) => self.render_segment(driver, pool, account, member, color, index + 1, multiple, roomy, window, cx),
                None => div().flex_1().min_w_0().into_any_element(),
            })
            .collect();
        let legend: Vec<AnyElement> = if roomy || !multiple {
            Vec::new()
        } else {
            pool.columns
                .iter()
                .enumerate()
                .filter_map(|(index, (account, member))| member.as_ref().map(|member| self.render_legend_row(driver, pool, account, member, color, index + 1, cx)))
                .collect()
        };
        // The popover closed before the confirm, so progress and the outcome show under the bar.
        let status = self.reset_status.as_ref().and_then(|(key, status)| {
            let account = pool.columns.iter().find(|(account, member)| account.key == *key && member.is_some())?.0.clone();
            let text = match status {
                None => "Using reset credit…".to_owned(),
                Some(text) if !text.is_empty() && self.confirm_reset.is_none() => text.clone(),
                Some(_) => return None,
            };
            let p = palette(cx);
            Some(
                h_flex()
                    .gap(r(4.))
                    .text_size(r(12.))
                    .text_color(hsla(p.muted_foreground))
                    .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(account_name(&account)))
                    .child(text),
            )
        });
        v_flex().min_w_0().gap(r(4.)).child(h_flex().gap(r(4.)).children(segments)).children(legend).children(status).into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_segment(
        &mut self,
        driver: &str,
        pool: &LimitPoolWindow,
        account: &Arc<LimitAccount>,
        member: &LimitPoolMember,
        color: Hsla,
        index: usize,
        show_name: bool,
        roomy: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let now = self.data.limits_now;
        let window_data = &member.window;
        let remaining = remaining_percent(window_data);
        let time_left = pace_remaining_percent(window_data, now);
        let resets = format_resets_in(window_data, now);
        let credits = account.reset_credit_count();
        let color = segment_color(account, color);
        let refill = pool.reset_for(&account.key).filter(|_| remaining < 100 && member.refill_percent > 0.).map(|_| (100 - remaining).min(member.refill_percent as i64));
        let key = SegmentKey { driver: driver.to_owned(), window: format!("{:?}:{}", pool.kind, pool.id), account: account.key.clone() };
        let open = self.segment.as_ref() == Some(&key);
        let id: SharedString = format!("usage-segment-{}-{}-{}", key.driver, key.window, key.account).into();
        let summary: SharedString = limits::segment_summary(account, window_data, now).into();
        let badge = (resets.is_some() || credits > 0).then(|| {
            h_flex()
                .ml_auto()
                .min_w_0()
                .gap(r(6.))
                .px(r(6.))
                .py(r(2.))
                .rounded(r(4.))
                .bg(hsla(p.background).opacity(0.85))
                .text_size(r(11.))
                .text_color(hsla(p.foreground))
                .children(resets.as_ref().map(|resets| div().truncate().child(resets.replace("resets in ", "↻ "))))
                .when(credits > 0 && resets.is_some(), |this| this.child(div().text_color(hsla(p.muted_foreground)).child("·")))
                .when(credits > 0, |this| this.child(h_flex().gap(r(2.)).font_weight(FontWeight::SEMIBOLD).child(icon(Icon::Ticket, 12., hsla(p.foreground))).child(credits.to_string())))
        });
        let toggle_key = key.clone();
        let segment = focusable(div().id(id.clone()), id.into(), true, window, cx)
            .relative()
            .flex_1()
            .min_w_0()
            .h(r(if roomy { 32. } else { 20. }))
            .rounded(r(6.))
            .bg(hsla(p.muted))
            .cursor_pointer()
            .when(open, |this| this.border_1().border_color(hsla(p.border)))
            .tooltip(move |window, cx| tooltip_view(summary.clone(), window, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.segment = if this.segment.as_ref() == Some(&toggle_key) { None } else { Some(toggle_key.clone()) };
                this.menu = None;
                cx.notify();
            }))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(r(6.))
                    .overflow_hidden()
                    .child(div().absolute().top_0().bottom_0().left_0().w(relative(remaining as f32 / 100.)).rounded(r(6.)).bg(color.opacity(0.35)))
                    .children(refill.map(|refill| div().absolute().top_0().bottom_0().right_0().w(relative(refill as f32 / 100.)).bg(color.opacity(0.2)))),
            )
            .children(time_left.map(|left| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(left as f32 / 100.))
                    .ml(r(-3.))
                    .w(r(6.))
                    .flex()
                    .justify_center()
                    .bg(hsla(p.background))
                    .child(div().w(px(2.)).my(px(2.)).rounded_full().bg(hsla(p.foreground)))
            }))
            .child(if roomy {
                h_flex()
                    .relative()
                    .size_full()
                    .min_w_0()
                    .items_center()
                    .gap(r(6.))
                    .px(r(8.))
                    .text_size(r(12.))
                    .when(show_name, |this| {
                        this.child(div().min_w_0().font_weight(FontWeight::MEDIUM).child(account_name(account)))
                            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(format!("{remaining}%")))
                    })
                    .children(badge)
                    .into_any_element()
            } else {
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(r(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(hsla(p.foreground).opacity(0.8))
                    .child(index.to_string())
                    .into_any_element()
            });
        let popover = open.then(|| self.render_segment_details(account, member, pool, cx));
        div().relative().flex_1().min_w_0().child(segment).children(popover).into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_legend_row(&mut self, driver: &str, pool: &LimitPoolWindow, account: &Arc<LimitAccount>, member: &LimitPoolMember, color: Hsla, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let now = self.data.limits_now;
        let remaining = remaining_percent(&member.window);
        let resets = format_resets_in(&member.window, now);
        let credits = account.reset_credit_count();
        let key = SegmentKey { driver: driver.to_owned(), window: format!("{:?}:{}", pool.kind, pool.id), account: account.key.clone() };
        let id: SharedString = format!("usage-legend-{}-{}-{}", key.driver, key.window, key.account).into();
        h_flex()
            .id(id)
            .h(r(28.))
            .px(r(7.))
            .gap(r(4.))
            .rounded(r(6.))
            .text_size(r(12.))
            .cursor_pointer()
            .hover(|style| style.bg(hsla(p.accent)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.segment = if this.segment.as_ref() == Some(&key) { None } else { Some(key.clone()) };
                cx.notify();
            }))
            .child(
                div()
                    .relative()
                    .size(r(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(r(4.))
                    .bg(segment_color(account, color).opacity(0.35))
                    .text_size(r(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(index.to_string()),
            )
            .child(div().min_w_0().font_weight(FontWeight::MEDIUM).child(account_name(account)))
            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(format!("{remaining}%")))
            .child(
                h_flex()
                    .ml_auto()
                    .flex_none()
                    .gap(r(6.))
                    .text_size(r(11.))
                    .text_color(hsla(p.muted_foreground))
                    .children(resets.map(|resets| resets.replace("resets in ", "↻ ")))
                    .when(credits > 0, |this| this.child(h_flex().gap(r(2.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(icon(Icon::Ticket, 12., hsla(p.foreground))).child(credits.to_string()))),
            )
            .into_any_element()
    }

    /// `SegmentPopover`.
    fn render_segment_details(&mut self, account: &Arc<LimitAccount>, member: &LimitPoolMember, pool: &LimitPoolWindow, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let now = self.data.limits_now;
        let window_data: &UsageWindow = &member.window;
        let remaining = remaining_percent(window_data);
        let time_left = pace_remaining_percent(window_data, now);
        let resets = format_resets_in(window_data, now);
        let restore = pool.reset_for(&account.key).filter(|reset| reset.restores_percent > 0);
        let row = |label: &str, value: String| {
            h_flex()
                .gap(r(12.))
                .child(div().w(r(72.)).flex_none().text_color(hsla(p.muted_foreground)).child(label.to_owned()))
                .child(div().min_w_0().text_color(hsla(p.foreground)).child(value))
        };
        let resets_at = window_data.resets_at.as_deref().map(|at| {
            timeline::format_upcoming(at, now, &crate::timestamp::viewer_zone(), crate::settings::store::client(cx).timestamp_format(), &crate::timestamp::SystemTimestampLocale)
        });
        let credits = account.limits.reset_credits.clone().filter(|credits| credits.available_count > 0 && account.redeem.is_some());
        let redeem = account.redeem.clone();
        let account_key = account.key.clone();
        let busy = self.reset_status.as_ref().is_some_and(|(key, status)| *key == account.key && status.is_none());
        let status = self.reset_status.as_ref().filter(|(key, _)| *key == account.key).and_then(|(_, status)| status.clone());
        let can_manage = self.can_manage_providers();
        let panel = v_flex()
            .id("usage-segment-details")
            .occlude()
            .w(r(288.))
            .gap(r(10.))
            .p(r(16.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.dropdown_border))
            .bg(hsla(p.popover))
            .text_size(r(12.))
            .shadow(vec![gpui::BoxShadow {
                color: gpui::black().opacity(0.55),
                offset: gpui::point(px(0.), px(16.)),
                blur_radius: px(40.),
                spread_radius: px(-18.),
                inset: false,
            }])
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .min_w_0()
                    .gap(r(2.))
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .text_size(r(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(if account.redeem.is_some() || account.email.is_none() { brand_mark(&account.driver, 16., hsla(p.foreground), cx) } else { account_chip(account.email.as_deref().unwrap_or("")).into_any_element() })
                            .child(div().truncate().child(account_title(account))),
                    )
                    .children(account.email.clone().map(|email| div().text_color(hsla(p.muted_foreground)).child(email))),
            )
            .child(
                v_flex()
                    .gap(r(4.))
                    .pt(r(10.))
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .children(account.plan.clone().map(|plan| row("Plan", plan)))
                    .children(account.location().map(|location| row(if account.environments.is_empty() { "Via" } else { "Signed in" }, location))),
            )
            .child(
                v_flex()
                    .gap(r(4.))
                    .pt(r(10.))
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .child(row("Left", format!("{remaining}%")))
                    .children(resets_at.map(|at| row("Resets", format!("{at}{}", resets.map(|resets| format!(" · {}", resets.replace("resets in ", "in "))).unwrap_or_default()))))
                    .children(time_left.map(|left| row("Even pace", format!("{left}% left by now"))))
                    .children(restore.map(|reset| row("Restores", format!("+{}% of pool", reset.restores_percent)))),
            )
            .when_some(credits.zip(redeem), |this, (credits, redeem)| {
                let summary = reset_credits_summary(credits.available_count, credits.next_expires_at.as_deref(), now, true);
                this.child(
                    h_flex()
                        .gap(r(12.))
                        .pt(r(10.))
                        .border_t_1()
                        .border_color(hsla(p.border).opacity(0.6))
                        .text_color(hsla(p.muted_foreground))
                        .child(summary)
                        .child(
                            div().ml_auto().child(
                                ActionButton::new("usage-use-reset", if busy { "Using…" } else { "Use reset" }, ButtonVariant::Outline, ButtonSize::Xs)
                                    .disabled(busy || !can_manage)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.segment = None;
                                        this.confirm_reset = Some(redeem.clone());
                                        this.reset_status = Some((account_key.clone(), Some(String::new())));
                                        cx.notify();
                                    })),
                            ),
                        ),
                )
            })
            .children(status.filter(|status| !status.is_empty()).map(|status| div().text_color(hsla(p.foreground)).child(status)));
        div().absolute().top_0().left_0().child(deferred(anchored().anchor(gpui::Anchor::BottomLeft).snap_to_window_with_margin(px(8.)).child(div().mb(r(6.)).child(panel))).with_priority(3))
    }

    /// The confirm a reset credit asks for (`ResetCreditDialog`): redemption spends a credit.
    pub(super) fn render_reset_confirm(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let redeem = self.confirm_reset.clone()?;
        let p = palette(cx);
        let account = self.reset_status.as_ref().map(|(key, _)| key.clone()).unwrap_or_default();
        Some(
            div()
                .id("usage-reset-confirm")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(r(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.confirm_reset = None;
                    cx.notify();
                }))
                .child(
                    v_flex()
                        .id("usage-reset-dialog")
                        .relative()
                        .w_full()
                        .max_w(r(512.))
                        .rounded(r(16.))
                        .overflow_hidden()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                        .child(
                            v_flex()
                                .relative()
                                .gap(r(8.))
                                .p(r(24.))
                                .child(div().text_size(r(20.)).line_height(r(20.)).font_weight(FontWeight::SEMIBOLD).child("Use a reset credit?"))
                                .child(div().text_size(r(14.)).text_color(hsla(p.muted_foreground)).child(
                                    "This redeems one credit on your account and clears the current rate-limit windows. It cannot be undone.",
                                )),
                        )
                        .child(
                            h_flex()
                                .relative()
                                .justify_end()
                                .gap(r(8.))
                                .px(r(24.))
                                .py(r(16.))
                                .border_t_1()
                                .border_color(hsla(p.border))
                                .bg(hsla(p.muted).opacity(0.72))
                                .child(ActionButton::new("usage-reset-cancel", "Cancel", ButtonVariant::Outline, ButtonSize::Default).on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_reset = None;
                                    this.reset_status = None;
                                    cx.notify();
                                })))
                                .child(
                                    ActionButton::new("usage-reset-use", "Use credit", ButtonVariant::Default, ButtonSize::Default)
                                        .disabled(!self.can_manage_providers())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.confirm_reset = None;
                                            this.redeem_reset(redeem.clone(), account.clone(), cx);
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}
