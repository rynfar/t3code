//! `ThreadDetailsUsage` (`apps/web/src/components/chat/ThreadDetailsUsage.tsx`):
//! provider usage limit windows (`ThreadDetailsLimits`) and active thread context window bar
//! (`ThreadDetailsContext`), rendered as separate headingless `ThreadDetailsSection`s.

use gpui::{AnyElement, App, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px, relative};
use gpui_component::{h_flex, v_flex};
use t3_core::server_config::UsageLimits;
use t3_core::sidebar::time::MINUTE_MS;
use t3_core::timeline::{format_upcoming, parse_iso_ms};
use t3_core::usage::context_window::{compaction_message, context_window_snapshot, format_context_tokens};
use t3_core::usage::format::format_count;
use t3_core::usage::limits::{collect_provider_usage_limits, format_duration, limits_notice, pace_of, remaining_percent};

use super::controls::section_with_options;
use crate::app_state::AppState;
use crate::right_panel::tooltip_view;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

fn thread_provider_limits(app: &AppState) -> Option<UsageLimits> {
    let thread = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id))?;
    let config = app.server_config.config.as_ref()?;
    let instance_id = thread.model_selection.instance_id.as_str();
    let now = now_ms();
    collect_provider_usage_limits(instance_id, &config.providers, &config.usage_limit_sources, now)
        .and_then(|report| report.accounts.into_iter().find(|account| account.instance_id.as_deref() == Some(instance_id)).map(|account| account.limits))
        .or_else(|| config.provider(instance_id).and_then(|provider| provider.usage_limits.clone()))
}

pub(super) fn limit_row_count(app: &AppState) -> usize {
    let Some(limits) = thread_provider_limits(app) else { return 0 };
    if limits_notice(&limits).is_some() { 1 } else { limits.windows.len() }
}

pub(super) fn context_available(app: &AppState) -> bool {
    let Some(thread_id) = app.selected_thread.as_deref() else { return false };
    app.thread.projection.as_ref().filter(|p| p.thread.id == thread_id).and_then(context_window_snapshot).is_some()
}

struct BarSpec {
    fill_pct: f32,
    color: gpui::Hsla,
    tick_frac: Option<f32>,
}

fn usage_bar(bar: &BarSpec, cx: &App) -> AnyElement {
    let p = palette(cx);
    let track = hsla(p.muted_foreground).opacity(if gpui_component::ActiveTheme::theme(cx).is_dark() { 0.2 } else { 0.3 });
    div()
        .relative()
        .w_full()
        .h(px(3.))
        .rounded_full()
        .bg(track)
        .overflow_hidden()
        .when(bar.fill_pct > 0., |el| {
            el.child(
                div()
                    .h_full()
                    .w(relative((bar.fill_pct / 100.).clamp(0., 1.)))
                    .min_w(px(6.))
                    .rounded_full()
                    .bg(bar.color),
            )
        })
        .when_some(bar.tick_frac, |el, frac| {
            el.child(div().absolute().top(px(-1.)).bottom(px(-1.)).left(relative(frac)).w(px(1.)).bg(hsla(p.foreground).opacity(0.6)))
        })
        .into_any_element()
}

fn usage_row(
    id: impl Into<SharedString>,
    label: String,
    bar: Option<BarSpec>,
    value: String,
    tooltip: String,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let id = id.into();
    let tip: SharedString = tooltip.into();
    h_flex()
        .id(id)
        .w_full()
        .h(px(24.))
        .items_center()
        .gap(px(12.))
        .px(px(10.))
        .rounded(px(8.))
        .text_xs()
        .hover(|style| style.bg(super::controls::hover_fill(cx)))
        .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
        .child(div().max_w(px(112.)).flex_none().truncate().text_color(hsla(p.muted_foreground)).child(label))
        .child(div().flex_1().min_w(px(48.)).children(bar.map(|spec| usage_bar(&spec, cx))))
        .child(div().flex_none().text_color(hsla(p.foreground)).child(value))
        .into_any_element()
}

pub(super) fn usage_limits_section(app: &AppState, separated: bool, essential: bool, cx: &App) -> Option<AnyElement> {
    let limits = thread_provider_limits(app)?;
    let p = palette(cx);
    let now = now_ms();
    let timestamp_format = crate::settings::store::client(cx).timestamp_format();
    let zone = crate::timestamp::viewer_zone();
    let rows: Vec<AnyElement> = if let Some(notice) = limits_notice(&limits) {
        vec![usage_row("details-usage-unavailable", "Limits".to_owned(), None, "Unavailable".to_owned(), notice, cx)]
    } else if limits.windows.is_empty() {
        return None;
    } else {
        limits
            .windows
            .iter()
            .map(|win| {
                let left = remaining_percent(win);
                let tone = if left <= 10 {
                    hsla(p.error)
                } else if left <= 20 {
                    hsla(p.warning)
                } else {
                    hsla(p.foreground).opacity(0.8)
                };
                let mut tip_lines = vec![format!("{}: {left}% left", win.label)];
                if let Some(resets_at) = win.resets_at.as_deref()
                    && let Some(resets_ms) = parse_iso_ms(resets_at)
                    && resets_ms > now
                {
                    let duration = format_duration((resets_ms - now).max(MINUTE_MS));
                    let upcoming = format_upcoming(resets_at, now, &zone, timestamp_format, &crate::timestamp::SystemTimestampLocale);
                    if upcoming.is_empty() {
                        tip_lines.push(format!("Resets in {duration}"));
                    } else {
                        tip_lines.push(format!("Resets in {duration} \u{00b7} {upcoming}"));
                    }
                }
                if let Some(pace) = pace_of(win, now) {
                    tip_lines.push(pace.label().to_owned());
                }
                usage_row(
                    format!("details-usage-{}", win.id),
                    win.label.clone(),
                    Some(BarSpec { fill_pct: left as f32, color: tone, tick_frac: None }),
                    format!("{left}%"),
                    tip_lines.join("\n"),
                    cx,
                )
            })
            .collect()
    };

    Some(section_with_options(separated, None, essential, cx).child(v_flex().children(rows)).into_any_element())
}

pub(super) fn context_window_section(app: &AppState, separated: bool, essential: bool, cx: &App) -> Option<AnyElement> {
    let thread_id = app.selected_thread.as_deref()?;
    let projection = app.thread.projection.as_ref().filter(|p| p.thread.id == thread_id)?;
    let snapshot = context_window_snapshot(projection)?;
    let p = palette(cx);

    let max_tokens = snapshot.max_tokens.filter(|m| *m > 0.);
    let used = max_tokens.and(snapshot.used_percentage).map(|pct| pct.clamp(0., 100.).round());
    let tone = match used {
        Some(pct) if pct >= 90. => hsla(p.error),
        Some(pct) if pct >= 80. => hsla(p.warning),
        _ => hsla(p.muted_foreground),
    };
    let tick = max_tokens.zip(snapshot.auto_compact_threshold).filter(|(max, t)| *t > 0. && *t < *max).map(|(max, t)| (t / max) as f32);
    let used_tokens = format_context_tokens(Some(snapshot.used_tokens));
    let value = match max_tokens {
        Some(max) => format!("{used_tokens} / {}", format_context_tokens(Some(max))),
        None => used_tokens.clone(),
    };
    let mut tip_lines = vec![match used {
        Some(pct) => format!("Context: {pct:.0}% used"),
        None => format!("Context: {used_tokens} tokens used"),
    }];
    if let Some(max) = max_tokens {
        tip_lines.push(format!("{} of {} tokens", format_count(snapshot.used_tokens), format_count(max)));
    }
    let compaction = compaction_message(None, snapshot.auto_compact_threshold);
    if !compaction.is_empty() {
        tip_lines.push(compaction);
    }

    let bar = used.map(|pct| BarSpec { fill_pct: pct as f32, color: tone, tick_frac: tick });
    let row = usage_row("details-context-window", "Context".to_owned(), bar, value, tip_lines.join("\n"), cx);
    Some(section_with_options(separated, None, essential, cx).child(row).into_any_element())
}
