//! Source control: `SourceControlSettingsPanel` in `web/src/components/settings/SourceControlSettings.tsx`
//! at environment scope: the Repositories rows of `ProjectDefaultsSettings.tsx`, the server's
//! version control and hosting discovery, and `SourceControlWritingSettings.tsx`. Host token and
//! account editing (`SourceControlTokenSettings`, `SourceControlAccountSettings`,
//! `SourceControlHostSettings`, app sign-in) is not ported: their rows show discovery only.

use std::collections::HashSet;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder,
};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::server_config::ServerConfig;
use t3_core::settings::source_control::{
    self, DISCOVER, Discovery, DiscoveryItem, Dot, MERGE_METHODS, SourceControlChange, WritingMode, decode_discovery, merge_method_value,
    parse_merge_method,
};

use super::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, group, muted_text, r, section, section_title, tooltip};
use super::threads::{model_options, parse_selection, selection_label, selection_value};
use super::worktrees::advanced;
use super::{Block, SettingsView, store};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// `badge.tsx` tints the settings pages use.
#[derive(Clone, Copy)]
pub(super) enum Tone {
    Secondary,
    Warning,
    Success,
    Error,
    Info,
}

/// A `sm` badge: 16 tall, 10px medium text, radius 4.
pub(super) fn tone_badge(text: impl Into<SharedString>, tone: Tone, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let tint = if cx.theme().is_dark() { 0.16 } else { 0.08 };
    let (fill, color) = match tone {
        Tone::Secondary => (hsla(p.secondary), hsla(p.secondary_foreground)),
        Tone::Warning => (hsla(p.warning).opacity(tint), hsla(p.warning_foreground)),
        Tone::Success => (hsla(p.success).opacity(tint), hsla(p.success_foreground)),
        Tone::Error => (hsla(p.destructive).opacity(tint), hsla(p.destructive_foreground)),
        Tone::Info => (hsla(p.info).opacity(tint), hsla(p.info_foreground)),
    };
    div().flex_none().h(r(16.)).px(r(4.)).flex().items_center().rounded(r(4.)).bg(fill).text_size(r(10.)).font_weight(FontWeight::MEDIUM).text_color(color).child(text.into())
}

/// A section title with a control at its end (`SettingsSection`'s `headerAction`).
pub(super) fn titled(title: impl Into<SharedString>, action: Option<AnyElement>, rows: Vec<AnyElement>, cx: &App) -> gpui::Div {
    v_flex().gap(r(10.)).child(h_flex().justify_between().pr(r(8.)).child(section_title(title)).children(action)).child(group(rows, cx))
}

pub(crate) struct SourceControlState {
    pub(crate) custom_instructions: Entity<TextareaState>,
    pub(crate) review_instructions: Entity<TextareaState>,
    /// The last scan, kept across visits; `None` until one answers.
    discovery: Option<Result<Discovery, String>>,
    /// Which connection the scan belongs to, so a reconnect scans again.
    scanned_for: Option<u64>,
    scanning: bool,
    advanced: bool,
    revealed_accounts: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

impl SourceControlState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let custom_instructions =
            cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8).placeholder("Keep titles concise. Use short bullet points in descriptions."));
        let review_instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("Show service interfaces before implementations. Skip test files unless they change behavior. Keep summaries to two sentences.")
        });
        let subscriptions = vec![
            cx.subscribe_in(&custom_instructions, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Blur) {
                    this.commit_custom_instructions(cx);
                }
            }),
            cx.subscribe_in(&review_instructions, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Blur) {
                    this.commit_review_instructions(cx);
                }
            }),
        ];
        SourceControlState {
            custom_instructions,
            review_instructions,
            discovery: None,
            scanned_for: None,
            scanning: false,
            advanced: false,
            revealed_accounts: HashSet::new(),
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn textareas(&self) -> [&Entity<TextareaState>; 2] {
        [&self.custom_instructions, &self.review_instructions]
    }

    pub(super) fn reveal(&mut self, target: &str) {
        if matches!(target, "follow-change-request-templates" | "source-control-writer-model") {
            self.advanced = true;
        }
    }
}

fn dot_color(dot: Dot, cx: &App) -> gpui::Hsla {
    let p = palette(cx);
    match dot {
        Dot::Success => hsla(p.success),
        Dot::Warning => hsla(p.warning),
        Dot::Muted => hsla(p.muted_foreground).opacity(0.35),
    }
}

impl SettingsView {
    fn save_source_control(&self, change: SourceControlChange, cx: &mut App) {
        store::update_server(&self.state, change.into_server(), cx);
    }

    fn commit_custom_instructions(&mut self, cx: &mut Context<Self>) {
        let value = self.source_control.custom_instructions.read(cx).value().trim().to_owned();
        let saved = self.thread_settings(cx).map(|config| config.settings.source_control.source_control_writing_style.custom_instructions.clone());
        if saved.is_some_and(|saved| saved != value) {
            self.save_source_control(SourceControlChange::CustomInstructions(value), cx);
        }
    }

    fn commit_review_instructions(&mut self, cx: &mut Context<Self>) {
        let value = self.source_control.review_instructions.read(cx).value().trim().to_owned();
        let saved = self.thread_settings(cx).map(|config| config.settings.source_control.review_guide_instructions.clone());
        if saved.is_some_and(|saved| saved != value) {
            self.save_source_control(SourceControlChange::ReviewGuideInstructions(value), cx);
        }
    }

    pub(super) fn commit_focused_source_control_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source_control.custom_instructions.focus_handle(cx).is_focused(window) {
            self.commit_custom_instructions(cx);
        }
        if self.source_control.review_instructions.focus_handle(cx).is_focused(window) {
            self.commit_review_instructions(cx);
        }
    }

    /// `sourceControlEnvironment.discovery`, run when the page opens on a connection and on Rescan.
    pub(super) fn scan_source_control(&mut self, force: bool, cx: &mut Context<Self>) {
        let connection = self.state.read(cx).connection_id();
        if self.source_control.scanning || (!force && connection.is_some() && self.source_control.scanned_for == connection) {
            return;
        }
        let Some(task) = self.state.read(cx).request(DISCOVER, serde_json::json!({}), |value| Ok(decode_discovery(&value))) else { return };
        self.source_control.scanning = true;
        self.source_control.scanned_for = connection;
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.source_control.scanning = false;
                this.source_control.discovery = Some(result);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn source_control_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let Some(config) = self.thread_settings(cx).cloned() else {
            return vec![Block::new(
                &["source-control"],
                section("Server environment", vec![div().px(r(16.)).py(r(12.)).child(muted_text("Connect an environment to inspect its version control tools and hosting integrations.", cx)).into_any_element()], cx),
            )];
        };
        self.scan_source_control(false, cx);
        let settings = &config.settings.source_control;
        Self::sync_textarea(&self.source_control.custom_instructions, &settings.source_control_writing_style.custom_instructions, window, cx);
        Self::sync_textarea(&self.source_control.review_instructions, &settings.review_guide_instructions, window, cx);

        let mut blocks = vec![Block::new(&["automatic-pull", "remove-agent-credits-on-merge", "pull-request-merge-method"], self.repositories_section(&config, cx))];
        blocks.extend(self.discovery_blocks(cx));
        blocks.push(Block::new(
            &["source-control-writing-style", "review-guides", "review-guide-model", "review-guide-instructions", "follow-change-request-templates", "source-control-writer-model"],
            self.writing_section(&config, cx),
        ));
        blocks
    }

    fn repositories_section(&self, config: &ServerConfig, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = &config.settings.source_control;
        let switch = |id: &'static str, checked: bool, change: fn(bool) -> SourceControlChange| {
            let state = self.state.clone();
            Switch::new(id, checked).on_change(move |checked, _, cx| store::update_server(&state, change(checked).into_server(), cx))
        };
        let reset = |change: SourceControlChange| {
            let state = self.state.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| store::update_server(&state, change.clone().into_server(), cx)
        };
        let merge_state = self.state.clone();
        let method = config.settings.pull_request_merge_method;
        section(
            "Repositories",
            vec![
                Row::new("automatic-pull", "Automatically pull")
                    .description("Keeps the default branch current when the checkout has no local changes or commits. Projects can override it.")
                    .highlighted(self.is_target("automatic-pull"))
                    .reset(settings.default_auto_pull, reset(SourceControlChange::AutoPull(false)))
                    .control(switch("automatic-pull-switch", settings.default_auto_pull, SourceControlChange::AutoPull))
                    .into_any_element(),
                Row::new("remove-agent-credits-on-merge", "Remove agent credits when merging")
                    .description("Remove recognized agent credit lines from merge and squash messages on hosts that support it, keeping human co-authors. Includes auto-merge. Excludes merge queues, stack merges, and existing commits.")
                    .highlighted(self.is_target("remove-agent-credits-on-merge"))
                    .reset(settings.remove_agent_credits_on_merge, reset(SourceControlChange::RemoveAgentCredits(false)))
                    .control(switch("remove-agent-credits-switch", settings.remove_agent_credits_on_merge, SourceControlChange::RemoveAgentCredits))
                    .into_any_element(),
                Row::new("pull-request-merge-method", "Default merge method")
                    .description("Pull requests start with this method. Last selected reuses whatever you chose most recently on this device.")
                    .highlighted(self.is_target("pull-request-merge-method"))
                    .reset(method.is_some(), reset(SourceControlChange::MergeMethod(None)))
                    .control(
                        Select::new("pull-request-merge-method-select", MERGE_METHODS.iter().map(|(value, label)| SelectOption::new(*value, *label)).collect())
                            .selected(merge_method_value(method))
                            .width(196.)
                            .on_change(move |value, _, cx| {
                                if let Some(method) = parse_merge_method(&value) {
                                    store::update_server(&merge_state, SourceControlChange::MergeMethod(method).into_server(), cx);
                                }
                            }),
                    )
                    .into_any_element(),
            ],
            cx,
        )
    }

    fn rescan_button(&self, cx: &mut Context<Self>) -> AnyElement {
        Button::new("rescan-source-control", ButtonVariant::GhostMuted, ButtonSize::IconMicro)
            .icon(if self.source_control.scanning { Icon::LoaderCircle } else { Icon::RefreshCw })
            .tooltip("Rescan Git and hosting integrations")
            .disabled(self.source_control.scanning)
            .on_click(cx.listener(|this, _, _, cx| this.scan_source_control(true, cx)))
            .into_any_element()
    }

    fn discovery_blocks(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let discovery = match &self.source_control.discovery {
            None => {
                let loading = vec![div().px(r(16.)).py(r(12.)).child(muted_text("Scanning\u{2026}", cx)).into_any_element()];
                return vec![Block::new(&["source-control"], titled("Version Control", Some(self.rescan_button(cx)), loading, cx))];
            }
            Some(Err(error)) => Err(error.clone()),
            Some(Ok(discovery)) if discovery.is_empty() => Err(String::new()),
            Some(Ok(discovery)) => Ok(discovery.clone()),
        };
        let discovery = match discovery {
            Ok(discovery) => discovery,
            Err(error) => {
                let p = palette(cx);
                let (title, description) = if error.is_empty() {
                    ("Nothing detected yet", "Install Git on the server, add optional hosting integrations or credentials your workspace needs, then rescan.".to_owned())
                } else {
                    ("Could not scan the server environment", error)
                };
                let empty = v_flex()
                    .items_center()
                    .gap(r(8.))
                    .px(r(16.))
                    .py(r(24.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
                    .child(div().text_xs().text_center().text_color(hsla(p.muted_foreground)).child(description))
                    .child(
                        Button::new("scan-source-control", ButtonVariant::Outline, ButtonSize::Sm)
                            .icon(Icon::RefreshCw)
                            .label("Scan")
                            .disabled(self.source_control.scanning)
                            .on_click(cx.listener(|this, _, _, cx| this.scan_source_control(true, cx))),
                    );
                return vec![Block::new(&["source-control"], section("Server environment", vec![empty.into_any_element()], cx))];
            }
        };
        let mut blocks = Vec::new();
        let has_vcs = !discovery.version_control_systems.is_empty();
        if has_vcs {
            let rows = discovery.version_control_systems.iter().map(|item| self.discovery_row(item, cx)).collect();
            blocks.push(Block::new(&["source-control"], titled("Version Control", Some(self.rescan_button(cx)), rows, cx)));
        }
        if !discovery.source_control_providers.is_empty() {
            let rows = discovery.source_control_providers.iter().map(|item| self.discovery_row(item, cx)).collect();
            let action = (!has_vcs).then(|| self.rescan_button(cx));
            let anchors: &[&'static str] = if has_vcs { &[] } else { &["source-control"] };
            blocks.push(Block::new(anchors, titled("Source control providers", action, rows, cx)));
        }
        blocks
    }

    /// `DiscoveryItemRow` without the host settings panel.
    fn discovery_row(&self, item: &DiscoveryItem, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let muted = hsla(p.muted_foreground);
        let account = item.auth.as_ref().filter(|auth| auth.status == source_control::AuthStatus::Authenticated).and_then(|auth| auth.account.clone());
        let summary: AnyElement = match account {
            Some(account) => {
                let revealed = self.source_control.revealed_accounts.contains(&item.kind);
                let kind = item.kind.clone();
                let detail = item.auth.as_ref().and_then(|auth| auth.detail.clone());
                h_flex()
                    .flex_wrap()
                    .gap(r(4.))
                    .child("Authenticated")
                    .child("as")
                    .child(
                        div()
                            .id(SharedString::from(format!("source-control-account-{}", item.kind)))
                            .cursor_pointer()
                            .text_color(hsla(p.foreground).opacity(0.8))
                            .child(if revealed { account } else { "\u{2022}".repeat(8) })
                            .tooltip(tooltip(SharedString::new_static(if revealed { "Click to hide account" } else { "Click to reveal account" })))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.source_control.revealed_accounts.remove(&kind) {
                                    this.source_control.revealed_accounts.insert(kind.clone());
                                }
                                cx.notify();
                            })),
                    )
                    .children(detail.map(|detail| format!("\u{00b7} {detail}")))
                    .into_any_element()
            }
            None => div().child(item.summary()).into_any_element(),
        };
        let dot = div().size(r(8.)).flex_none().rounded_full().bg(dot_color(item.dot(), cx));
        h_flex()
            .px(r(16.))
            .py(r(12.))
            .gap(r(16.))
            .when(item.coming_soon(), |this| this.opacity(0.8))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(r(4.))
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .child(dot)
                            .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(item.label.clone()))
                            .children(item.version.clone().map(|version| div().text_xs().text_color(muted).child(version)))
                            .when(item.coming_soon(), |this| this.child(tone_badge("Coming Soon", Tone::Warning, cx)))
                            .children(item.auth_badge().map(|badge| tone_badge(badge, Tone::Secondary, cx))),
                    )
                    .child(div().text_xs().line_height(r(18.)).text_color(muted.opacity(0.8)).child(summary)),
            )
            .when(!item.coming_soon(), |this| this.child(Switch::new(SharedString::from(format!("source-control-{}-availability", item.kind)), item.enabled()).disabled(true)))
            .into_any_element()
    }

    fn writing_section(&mut self, config: &ServerConfig, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = &config.settings.source_control;
        let style = &settings.source_control_writing_style;
        let options = model_options(config, None);
        let mode_state = self.state.clone();
        let custom = style.mode == WritingMode::Custom;
        let instructions = self.source_control.custom_instructions.clone();
        let reset_state = self.state.clone();
        let writing = Row::new("source-control-writing-style", "Source control writing style")
            .description(style.mode.description())
            .highlighted(self.is_target("source-control-writing-style"))
            .reset(!style.is_default_style(), move |_, _, cx| store::update_server(&reset_state, SourceControlChange::ResetWritingStyle.into_server(), cx))
            .control(
                Select::new("source-control-writing-style-select", WritingMode::ALL.iter().map(|mode| SelectOption::new(mode.as_str(), mode.label())).collect())
                    .selected(style.mode.as_str())
                    .width(196.)
                    .on_change(move |value, _, cx| {
                        let Some(mode) = WritingMode::parse(&value) else { return };
                        let custom_instructions = custom.then(|| instructions.read(cx).value().trim().to_owned());
                        store::update_server(&mode_state, SourceControlChange::WritingMode { mode, custom_instructions }.into_server(), cx);
                    }),
            )
            .when(custom, |row| row.below(div().mt(r(12.)).pb(r(14.)).max_w(r(672.)).child(Textarea::new(&self.source_control.custom_instructions))));

        let guides_state = self.state.clone();
        let mut rows = vec![
            writing.into_any_element(),
            Row::new("review-guides", "Review guides (beta)")
                .description("Add a Guide tab to pull requests. An agent explains the changes and points out decisions that need review.")
                .highlighted(self.is_target("review-guides"))
                .control(Switch::new("review-guides-switch", settings.review_guides_enabled).on_change(move |on, _, cx| {
                    store::update_server(&guides_state, SourceControlChange::ReviewGuides(on).into_server(), cx)
                }))
                .into_any_element(),
        ];
        let writer = source_control::writer_model(&config.settings);
        if settings.review_guides_enabled {
            rows.push(
                self.model_row(
                    config,
                    ModelRow {
                        id: "review-guide-model",
                        title: "Review guide model",
                        description: "Model that reads a pull request to write its review guide. It reads the change itself over many steps, so a stronger model helps. Off uses the source control writer model.",
                        selection: settings.review_guide_model_selection.clone(),
                        seed: writer.clone(),
                        change: SourceControlChange::ReviewGuideModel,
                    },
                    &options,
                ),
            );
            let reset_state = self.state.clone();
            rows.push(
                Row::new("review-guide-instructions", "Review guide instructions")
                    .description("Your preferences for review guides: what to show first, dwell on, or skip. The guide's format and coverage stay the same.")
                    .reset(!settings.review_guide_instructions.is_empty(), move |_, _, cx| {
                        store::update_server(&reset_state, SourceControlChange::ReviewGuideInstructions(String::new()).into_server(), cx)
                    })
                    .below(div().mt(r(12.)).pb(r(14.)).max_w(r(672.)).child(Textarea::new(&self.source_control.review_instructions)))
                    .into_any_element(),
            );
        }
        let templates_state = self.state.clone();
        let reset_templates = self.state.clone();
        let advanced_rows = vec![
            Row::new("follow-change-request-templates", "Follow change request templates")
                .description("Use the repository's template for change request descriptions when available.")
                .highlighted(self.is_target("follow-change-request-templates"))
                .reset(!style.follow_change_request_templates, move |_, _, cx| {
                    store::update_server(&reset_templates, SourceControlChange::FollowTemplates(true).into_server(), cx)
                })
                .control(Switch::new("follow-change-request-templates-switch", style.follow_change_request_templates).on_change(move |on, _, cx| {
                    store::update_server(&templates_state, SourceControlChange::FollowTemplates(on).into_server(), cx)
                }))
                .into_any_element(),
            self.model_row(
                config,
                ModelRow {
                    id: "source-control-writer-model",
                    title: "Source control writer model",
                    description: "Model for source control text and branch or bookmark names. When disabled, use the text generation model configured for this environment.",
                    selection: settings.source_control_writer_model_selection.clone(),
                    seed: writer,
                    change: SourceControlChange::WriterModel,
                },
                &options,
            ),
        ];
        let toggle = cx.listener(|this, _, _, cx| {
            this.source_control.advanced = !this.source_control.advanced;
            cx.notify();
        });
        rows.push(advanced("source-control-writing-advanced", self.source_control.advanced, advanced_rows, toggle, cx));
        section("Writing & review", rows, cx)
    }

    /// A "use a separate model" switch with the model select beside it while on.
    fn model_row(&self, config: &ServerConfig, row: ModelRow, options: &[SelectOption]) -> AnyElement {
        let ModelRow { id, title, description, selection, seed, change } = row;
        let switch_state = self.state.clone();
        let on = selection.is_some();
        let toggle = Switch::new(SharedString::from(format!("{id}-switch")), on).on_change(move |on, _, cx| {
            let next = if on { seed.clone() } else { None };
            if on && next.is_none() {
                return;
            }
            store::update_server(&switch_state, change(next).into_server(), cx);
        });
        let picker = selection.filter(|_| !options.is_empty()).map(|selection| {
            let select_state = self.state.clone();
            Select::new(SharedString::from(format!("{id}-select")), options.to_vec())
                .selected(selection_value(&selection))
                .label(selection_label(config, &selection))
                .width(200.)
                .popup_width(260.)
                .show_check()
                .on_change(move |value, _, cx| {
                    if let Some(selection) = parse_selection(&value) {
                        store::update_server(&select_state, change(Some(selection)).into_server(), cx);
                    }
                })
        });
        let empty = on && options.is_empty();
        Row::new(id, title)
            .description(description)
            .highlighted(self.is_target(id))
            .when(empty, |row| row.status("No text generation providers available."))
            .control(h_flex().gap(r(8.)).children(picker).child(toggle))
            .into_any_element()
    }
}

struct ModelRow {
    id: &'static str,
    title: &'static str,
    description: &'static str,
    selection: Option<t3_core::model::ModelSelection>,
    /// What turning the switch on saves: the writer model in use.
    seed: Option<t3_core::model::ModelSelection>,
    change: fn(Option<t3_core::model::ModelSelection>) -> SourceControlChange,
}
