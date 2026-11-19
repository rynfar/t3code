//! MCP server access ("Use T3 Code in another app") and OAuth agent sign-in approval flow.
//! Ports `apps/web/src/components/mcp/UseInAnotherAppDialog.tsx`,
//! `apps/web/src/components/auth/ApproveAgentSurface.tsx`,
//! `apps/web/src/components/auth/ConnectAgentSurface.tsx`,
//! `apps/web/src/routes/approve-agent.tsx`,
//! and `apps/web/src/state/connectMcpApproval.ts`.

mod approval;
mod wizard;

#[cfg(all(test, feature = "snapshot"))]
mod tests;

use gpui::{
    AnyElement, App, Context, ElementId, Entity, FontWeight, Global, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, WeakEntity, Window, div, img, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::mcp::McpClientAccess;
use t3_core::mcp::approval::parse_approval_url;

use crate::app_state::{AppEvent, AppState};
use crate::icons::{Icon, ProviderLogo, provider_logo};
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};

pub use approval::AgentApprovalDialog;
pub use wizard::UseInAnotherAppDialog;

struct Host(WeakEntity<McpDialogs>);
impl Global for Host {}

/// Opens the "Use T3 Code in another app (MCP)" wizard for `environment_id`
/// (or for the environment Sharing & access picks when `None`).
pub fn open_use_in_another_app(
    environment_id: Option<String>,
    window: gpui::AnyWindowHandle,
    cx: &mut App,
) {
    let Some(host) = cx.try_global::<Host>().and_then(|h| h.0.upgrade()) else {
        return;
    };
    host.update(cx, |dialogs, cx| {
        dialogs.wizard.update(cx, |wizard, cx| {
            wizard.open(environment_id, window, cx);
        });
    });
}

/// Opens the native agent sign-in approval dialog if `url` is an `/approve-agent` or
/// `/connect-agent` link. Returns `true` if handled.
#[cfg(all(test, feature = "snapshot"))]
pub fn open_agent_approval_url(url: &str, window: gpui::AnyWindowHandle, cx: &mut App) -> bool {
    let Some(parsed) = parse_approval_url(url) else {
        return false;
    };
    let Some(host) = cx.try_global::<Host>().and_then(|h| h.0.upgrade()) else {
        return false;
    };
    host.update(cx, |dialogs, cx| {
        dialogs.approval.update(cx, |approval, cx| {
            approval.open(parsed, window, cx);
        });
    });
    true
}

/// Whether either MCP dialog is currently open.
#[cfg(all(test, feature = "snapshot"))]
pub fn is_open(cx: &App) -> bool {
    cx.try_global::<Host>()
        .and_then(|h| h.0.upgrade())
        .is_some_and(|dialogs| dialogs.read(cx).is_open(cx))
}

/// Hosts both the "Use in another app" wizard and the OAuth agent sign-in approval dialog,
/// and intercepts incoming `/approve-agent` and `/connect-agent` URLs from `AppState`.
pub struct McpDialogs {
    state: Entity<AppState>,
    pub(crate) wizard: Entity<UseInAnotherAppDialog>,
    pub(crate) approval: Entity<AgentApprovalDialog>,
    _subscription: Subscription,
}

impl McpDialogs {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.set_global(Host(cx.weak_entity()));
        let wizard = cx.new(|cx| UseInAnotherAppDialog::new(state.clone(), window, cx));
        let approval = cx.new(|cx| AgentApprovalDialog::new(state.clone(), window, cx));
        let subscription = cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
            if matches!(event, AppEvent::PromptLink) {
                this.consume_approval_urls(window, cx);
            }
        });
        let mut this = Self {
            state,
            wizard,
            approval,
            _subscription: subscription,
        };
        this.consume_approval_urls(window, cx);
        this
    }

    pub fn is_open(&self, cx: &App) -> bool {
        self.wizard.read(cx).is_open() || self.approval.read(cx).is_open()
    }

    /// Consumes leading `/approve-agent` or `/connect-agent` URLs before `PromptLinks` reads them.
    fn consume_approval_urls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut latest = None;
        self.state.update(cx, |state, _| {
            while let Some(front) = state.prompt_link_urls.front() {
                if let Some(parsed) = parse_approval_url(front) {
                    state.prompt_link_urls.pop_front();
                    latest = Some(parsed);
                } else {
                    break;
                }
            }
        });
        if let Some(parsed) = latest {
            let handle = window.window_handle();
            self.approval.update(cx, |approval, cx| {
                approval.open(parsed, handle, cx);
            });
        }
    }
}

impl Render for McpDialogs {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .child(self.wizard.clone())
            .child(self.approval.clone())
    }
}

/// Icon for each `McpClientAccess` level (`accessConfig` in `ConnectAgentSurface.tsx`).
pub(crate) fn access_icon(access: McpClientAccess) -> Icon {
    match access {
        McpClientAccess::ReadOnly => Icon::Eye,
        McpClientAccess::ApprovalRequired => Icon::Lock,
        McpClientAccess::AutoAcceptEdits => Icon::PenLine,
        McpClientAccess::Auto => Icon::Sparkles,
        McpClientAccess::FullAccess => Icon::ShieldOff,
    }
}

/// One access-level radio card (`AccessOption` in `ConnectAgentSurface.tsx`).
pub(crate) fn access_option_card(
    id: impl Into<ElementId>,
    access: McpClientAccess,
    selected: bool,
    disabled_reason: Option<&str>,
    description_override: Option<Option<&str>>,
    on_select: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let disabled = disabled_reason.is_some();
    let detail: Option<&str> = match disabled_reason {
        Some(reason) => Some(reason),
        None => match description_override {
            Some(override_desc) => override_desc,
            None => Some(access.description()),
        },
    };
    let icon_color = if selected {
        hsla(p.primary)
    } else {
        hsla(p.muted_foreground)
    };
    let border_color = if selected {
        hsla(p.primary)
    } else {
        hsla(p.border)
    };
    let bg_color = if selected {
        if dark {
            hsla(p.primary).opacity(0.10)
        } else {
            hsla(p.background)
        }
    } else if dark {
        hsla(p.foreground).opacity(0.035)
    } else {
        hsla(p.background)
    };

    div()
        .id(id.into())
        .flex()
        .items_start()
        .gap(px(12.))
        .rounded(px(8.))
        .border_1()
        .border_color(border_color)
        .bg(bg_color)
        .px(px(12.))
        .py(px(10.))
        .when(disabled, |this| this.opacity(0.55))
        .when(!disabled, |this| {
            this.cursor_pointer()
                .hover(|style| {
                    if selected {
                        style
                    } else {
                        style.bg(hsla(p.accent))
                    }
                })
                .on_click(move |_, window, cx| on_select(window, cx))
        })
        .child(div().pt(px(2.)).child(access_icon(access).render(px(16.), icon_color)))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground))
                        .child(access.label()),
                )
                .when_some(detail, |this, text| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(text.to_owned()),
                    )
                }),
        )
        .into_any_element()
}

/// Monospace copyable value row (`CopyRow` in `UseInAnotherAppDialog.tsx`).
pub(crate) fn copy_row(
    id: impl Into<ElementId>,
    label: &'static str,
    value: &str,
    copied: bool,
    on_copy: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let mono_font = cx.theme().mono_font_family.clone();
    let button_id = SharedString::from(format!("copy-{label}"));
    h_flex()
        .id(id.into())
        .min_w_0()
        .items_center()
        .gap(px(8.))
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.border))
        .bg(hsla(p.muted).opacity(0.5))
        .px(px(12.))
        .py(px(8.))
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(mono_font)
                .text_sm()
                .text_color(hsla(p.foreground))
                .child(value.to_owned()),
        )
        .child(
            Button::new(button_id, ButtonVariant::Ghost, ButtonSize::IconXs)
                .icon(if copied { Icon::Check } else { Icon::Copy })
                .tooltip(if copied { "Copied" } else { "Copy" })
                .on_click(move |_, window, cx| on_copy(window, cx)),
        )
        .into_any_element()
}

/// Copyable shell command block (`CommandBlock` in `UseInAnotherAppDialog.tsx`).
pub(crate) fn command_block(
    id: impl Into<ElementId>,
    command: &str,
    copied: bool,
    on_copy: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let mono_font = cx.theme().mono_font_family.clone();
    let elem_id: ElementId = id.into();
    let button_id = SharedString::from(format!("copy-cmd-{}", command.len()));
    h_flex()
        .id(elem_id)
        .min_w_0()
        .items_center()
        .gap(px(8.))
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.border))
        .bg(hsla(p.code_block_background))
        .px(px(12.))
        .py(px(8.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(mono_font)
                .text_xs()
                .text_color(hsla(p.code_foreground))
                .child(command.to_owned()),
        )
        .child(
            Button::new(button_id, ButtonVariant::Ghost, ButtonSize::IconXs)
                .icon(if copied { Icon::Check } else { Icon::Copy })
                .tooltip(if copied { "Copied" } else { "Copy command" })
                .on_click(move |_, window, cx| on_copy(window, cx)),
        )
        .into_any_element()
}

/// App badge rendered in the 4-column MCP app picker grid.
/// Uses vendored provider logos where present and crisp semantic monogram badges for the rest.
pub(crate) fn mcp_app_badge(app_id: &str, app_name: &str, cx: &App) -> AnyElement {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let driver = match app_id {
        "claude" => Some("claudeAgent"),
        "chatgpt" => Some("codex"),
        "grok-bot" => Some("grok"),
        "muse" => Some("muse"),
        _ => None,
    };
    let inner = if let Some(driver) = driver
        && let Some(logo) = provider_logo(driver)
    {
        match logo {
            ProviderLogo::Mono(path) => gpui::svg()
                .path(path)
                .size(px(22.))
                .text_color(hsla(p.foreground))
                .into_any_element(),
            ProviderLogo::Image { light, dark: dark_path } => img(if dark { dark_path } else { light })
                .size(px(22.))
                .into_any_element(),
        }
    } else {
        let glyph = match app_id {
            "poke" => Icon::RadioTower,
            "instinct" => Icon::Sparkles,
            "openclaw" => Icon::Terminal,
            "hermes-agent" => Icon::Bot,
            _ => Icon::Blocks,
        };
        let _ = app_name;
        glyph.render(px(20.), hsla(p.foreground)).into_any_element()
    };

    div()
        .size(px(40.))
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.border))
        .bg(hsla(p.card))
        .flex()
        .items_center()
        .justify_center()
        .child(inner)
        .into_any_element()
}
