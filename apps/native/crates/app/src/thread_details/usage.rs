//! `ThreadDetailsUsage` (`apps/web/src/components/chat/ThreadDetailsUsage.tsx`):
//! provider usage limit windows and active thread context window bar inside the `"Usage"` section.

use gpui::{AnyElement, App, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px, relative};
use gpui_component::{h_flex, v_flex};
use t3_core::providers::driver_label;
use t3_core::usage::context_window::{compaction_message, context_window_snapshot, format_context_cost, format_context_tokens, format_meter_percentage};
use t3_core::usage::limits::{LimitEnvironment, collect_limit_accounts, collect_limit_pools, display_limit_windows, format_resets_in, pace_of};

use super::controls::section_with_options;
use crate::app_state::AppState;
use crate::right_panel::tooltip_view;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

pub(super) fn limit_row_count(app: &AppState) -> usize {
    let Some(thread) = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id)) else { return 0 };
    let Some(config) = app.server_config.config.as_ref() else { return 0 };
    let driver = config.provider(&thread.model_selection.instance_id).map(|p| p.driver.as_str());
    let env_id = config.environment.environment_id.as_str();
    let label = if config.environment.label.is_empty() { "This environment" } else { config.environment.label.as_str() };
    let envs = [LimitEnvironment { environment_id: env_id, label, providers: &config.providers, sources: &config.usage_limit_sources }];
    let pools = collect_limit_pools(collect_limit_accounts(&envs), now_ms());
    pools
        .into_iter()
        .filter(|pool| driver.is_none_or(|d| pool.driver == d))
        .map(|pool| display_limit_windows(&pool).len())
        .sum()
}

pub(super) fn context_available(app: &AppState) -> bool {
    let Some(thread_id) = app.selected_thread.as_deref() else { return false };
    app.thread.projection.as_ref().filter(|p| p.thread.id == thread_id).and_then(context_window_snapshot).is_some()
}

pub(super) fn usage_section(
    app: &AppState,
    show_usage: bool,
    show_context: bool,
    separated: bool,
    essential: bool,
    cx: &App,
) -> Option<AnyElement> {
    if !show_usage && !show_context {
        return None;
    }
    let thread = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id))?;
    let p = palette(cx);
    let mut rows: Vec<AnyElement> = Vec::new();

    if show_usage && let Some(config) = app.server_config.config.as_ref() {
        let now = now_ms();
        let driver = config.provider(&thread.model_selection.instance_id).map(|p| p.driver.as_str());
        let env_id = config.environment.environment_id.as_str();
        let label = if config.environment.label.is_empty() { "This environment" } else { config.environment.label.as_str() };
        let envs = [LimitEnvironment { environment_id: env_id, label, providers: &config.providers, sources: &config.usage_limit_sources }];
        let pools = collect_limit_pools(collect_limit_accounts(&envs), now);
        for pool in pools.into_iter().filter(|pool| driver.is_none_or(|d| pool.driver == d)) {
            let windows = display_limit_windows(&pool);
            if windows.is_empty() {
                continue;
            }
            for win in windows {
                let pct = win.remaining_percent.clamp(0, 100) as f32 / 100.;
                let resets = win.members.iter().find_map(|m| format_resets_in(&m.window, now));
                let pace = win.members.iter().find_map(|m| pace_of(&m.window, now));
                let provider_name = driver_label(&pool.driver);
                let mut tip_parts = vec![format!("{provider_name} {} · {}% left", win.label, win.remaining_percent)];
                if let Some(pace) = pace {
                    tip_parts.push(pace.label().to_owned());
                }
                if let Some(resets) = &resets {
                    tip_parts.push(resets.clone());
                }
                let tip: SharedString = tip_parts.join(" · ").into();
                let bar_color = if win.remaining_percent <= 15 { hsla(p.error) } else { hsla(p.primary) };
                let right_text = resets.unwrap_or_else(|| format!("{}% left", win.remaining_percent));
                rows.push(
                    v_flex()
                        .id(SharedString::from(format!("details-usage-{}-{}", pool.driver, win.id)))
                        .w_full()
                        .gap(px(4.))
                        .px(px(10.))
                        .py(px(4.))
                        .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
                        .child(
                            h_flex()
                                .justify_between()
                                .gap(px(8.))
                                .text_size(px(12.))
                                .child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground).opacity(0.85)).child(win.label.clone()))
                                .child(div().flex_none().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(right_text)),
                        )
                        .child(
                            div()
                                .w_full()
                                .h(px(5.))
                                .rounded_full()
                                .bg(hsla(p.muted_foreground).opacity(0.18))
                                .overflow_hidden()
                                .child(div().h_full().w(relative(pct)).rounded_full().bg(bar_color)),
                        )
                        .into_any_element(),
                );
            }
        }
    }

    if show_context
        && let Some(projection) = app.thread.projection.as_ref().filter(|p| p.thread.id == thread.id)
        && let Some(snapshot) = context_window_snapshot(projection)
    {
        let used_pct = snapshot.used_percentage.unwrap_or(0.).clamp(0., 100.);
        let bar_color = if used_pct > 90. { hsla(p.error) } else { hsla(p.primary) };
        let summary = match snapshot.max_tokens {
            Some(max) if max > 0. => format!("{} / {}", format_context_tokens(Some(snapshot.used_tokens)), format_context_tokens(Some(max))),
            _ => format_context_tokens(Some(snapshot.used_tokens)),
        };
        let threshold_frac = snapshot
            .auto_compact_threshold
            .zip(snapshot.max_tokens)
            .filter(|(t, max)| *max > 0. && *t > 0. && *t <= *max)
            .map(|(t, max)| (t / max) as f32);
        let mut tip_lines = vec![format!("Context: {}", format_meter_percentage(Some(used_pct)).unwrap_or_else(|| format!("{used_pct:.0}%")))];
        let compaction = compaction_message(None, snapshot.auto_compact_threshold);
        if !compaction.is_empty() {
            tip_lines.push(compaction);
        }
        if let Some((amount, currency)) = &snapshot.cost {
            tip_lines.push(format_context_cost(*amount, currency));
        }
        let tip: SharedString = tip_lines.join(" · ").into();

        rows.push(
            v_flex()
                .id("details-context-window")
                .w_full()
                .gap(px(4.))
                .px(px(10.))
                .py(px(4.))
                .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
                .child(
                    h_flex()
                        .justify_between()
                        .gap(px(8.))
                        .text_size(px(12.))
                        .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground).opacity(0.85)).child("Context"))
                        .child(div().flex_none().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(summary)),
                )
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h(px(5.))
                        .rounded_full()
                        .bg(hsla(p.muted_foreground).opacity(0.18))
                        .overflow_hidden()
                        .child(div().h_full().w(relative((used_pct / 100.) as f32)).rounded_full().bg(bar_color))
                        .when_some(threshold_frac, |bar, frac| {
                            bar.child(div().absolute().top_0().bottom_0().left(relative(frac)).w(px(1.5)).bg(hsla(p.foreground).opacity(0.45)))
                        }),
                )
                .into_any_element(),
        );
    }

    if rows.is_empty() {
        return None;
    }

    let heading = (!essential).then_some("Usage".into());
    Some(section_with_options(separated, heading, essential, cx).child(v_flex().gap(px(4.)).children(rows)).into_any_element())
}
