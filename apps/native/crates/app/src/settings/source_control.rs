//! Source control: `SourceControlSettingsPanel` in `web/src/components/settings/SourceControlSettings.tsx`
//! at environment scope: the Repositories rows of `ProjectDefaultsSettings.tsx`, the server's
//! version control and hosting discovery, host credentials (`SourceControlAppSignInSettings`,
//! `SourceControlTokenSettings`, `SourceControlAccountSettings`, `SourceControlHostSettings`),
//! and `SourceControlWritingSettings.tsx`.

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::{Map, Value, json};
use t3_core::device::hosts::SecretValue;
use t3_core::server_config::ServerConfig;
use t3_core::settings::source_control::{
    self, AppSignInDef, DISCOVER, Discovery, DiscoveryItem, Dot, HostCredentialsDef, HostDefinition, HostFieldDef, MERGE_METHODS, SIGN_IN_CANCEL,
    SIGN_IN_START, SIGN_IN_WAIT, SignInPrompt, SourceControlChange, WritingMode, decode_discovery, decode_sign_in_prompt, group_host_accounts,
    has_secret_value, host_definition, merge_method_value, next_host_account_choices, parse_merge_method, read_host_credential_settings,
    read_host_fields,
};

use super::controls::{
    Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, group, muted_text, r, section, section_title, tooltip,
};
use super::projects::SETTINGS_WRITE_SCOPE;
use super::threads::{model_options, parse_selection, selection_label, selection_value};
use super::worktrees::advanced;
use super::{Block, SettingsView, store};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const STORED_SECRET: &str = "__stored__";
const ACTIVE_ACCOUNT: &str = "__active__";

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
    div()
        .flex_none()
        .h(r(16.))
        .px(r(4.))
        .flex()
        .items_center()
        .rounded(r(4.))
        .bg(fill)
        .text_size(r(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(text.into())
}

/// A section title with a control at its end (`SettingsSection`'s `headerAction`).
pub(super) fn titled(title: impl Into<SharedString>, action: Option<AnyElement>, rows: Vec<AnyElement>, cx: &App) -> gpui::Div {
    v_flex().gap(r(10.)).child(h_flex().justify_between().pr(r(8.)).child(section_title(title)).children(action)).child(group(rows, cx))
}

#[derive(Clone, Default)]
enum SignInState {
    #[default]
    Idle,
    Starting,
    Waiting { seq: u64, prompt: SignInPrompt },
    Failed(String),
}

struct TokenEditorState {
    host: Entity<InputState>,
    value: Entity<InputState>,
    source: String,
    saving: bool,
    synced_key: String,
    _subscriptions: Vec<Subscription>,
}

struct GenericHostState {
    inputs: HashMap<String, Entity<InputState>>,
    sources: HashMap<String, String>,
    cleared_fields: HashSet<String>,
    saving: bool,
    synced_signature: String,
    _subscriptions: Vec<Subscription>,
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
    expanded_providers: HashSet<String>,
    accounts_revealed: HashSet<String>,
    sign_in: HashMap<String, SignInState>,
    sign_in_seq: u64,
    token_editors: HashMap<String, TokenEditorState>,
    generic_editors: HashMap<String, GenericHostState>,
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
            expanded_providers: HashSet::new(),
            accounts_revealed: HashSet::new(),
            sign_in: HashMap::new(),
            sign_in_seq: 0,
            token_editors: HashMap::new(),
            generic_editors: HashMap::new(),
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
        if let Some(kind) = target.strip_prefix("source-control-host-") {
            self.expanded_providers.insert(kind.to_owned());
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

/// Prevents `/` typed inside an inline host input from stealing focus to Settings search.
fn stop_slash_shortcut(event: &KeyDownEvent, _: &mut Window, cx: &mut App) {
    if event.keystroke.key == "/" && !event.keystroke.modifiers.modified() {
        cx.stop_propagation();
    }
}

fn secret_source_options(config: &ServerConfig) -> Vec<SelectOption> {
    std::iter::once(SelectOption::new(STORED_SECRET, "Stored secret"))
        .chain(config.settings.secret_providers.0.iter().map(|provider| SelectOption::new(provider.id.clone(), provider.label.clone())))
        .collect()
}

fn secret_placeholder(source: &str, configured: bool, fallback: &str) -> String {
    if source != STORED_SECRET {
        "op://Vault/Item/field".to_owned()
    } else if configured {
        "Saved — enter a new value to replace".to_owned()
    } else {
        fallback.to_owned()
    }
}

fn mask_account(account: &str, revealed: bool) -> String {
    if revealed { account.to_owned() } else { "\u{2022}".repeat(8) }
}

/// Account pinning is read-only when the user cannot write settings or the host is disabled.
fn account_select_disabled(can_write: bool, enabled: bool) -> bool {
    !can_write || !enabled
}

/// Builds a partial `sourceControlHosts.<kind>` field patch for generic host forms (e.g. Bitbucket),
/// preserving untouched secrets while allowing any single saved secret or reference to be cleared
/// with `""` without wiping the remaining fields on that host.
fn build_generic_host_patch(
    fields_def: &[HostFieldDef],
    saved: &BTreeMap<String, SecretValue>,
    raw_values: &HashMap<String, String>,
    sources: &HashMap<String, String>,
    cleared: &HashSet<String>,
) -> Result<Map<String, Value>, String> {
    let mut fields = Map::new();
    for field in fields_def {
        let raw = raw_values.get(field.key).map(|s| s.trim()).unwrap_or_default();
        let source = sources.get(field.key).map(String::as_str).unwrap_or(STORED_SECRET);
        let saved_val = saved.get(field.key);
        if field.secret {
            if source != STORED_SECRET {
                let Some(secret) = SecretValue::reference(source, raw) else {
                    return Err(format!("Enter a valid secret reference for {}.", field.label));
                };
                if saved_val != Some(&secret) {
                    fields.insert(field.key.to_owned(), secret.encode());
                }
            } else if !raw.is_empty() {
                fields.insert(field.key.to_owned(), Value::String(raw.to_owned()));
            } else if cleared.contains(field.key) || saved_val.is_some_and(SecretValue::is_reference) {
                // Switching a saved secret-provider reference back to stored with an empty value,
                // or explicitly clearing a saved secret, sends "" for just this field.
                fields.insert(field.key.to_owned(), Value::String(String::new()));
            }
        } else {
            let saved_text = match saved_val {
                Some(SecretValue::Literal(text)) => text.as_str(),
                _ => "",
            };
            if raw != saved_text {
                fields.insert(field.key.to_owned(), Value::String(raw.to_owned()));
            }
        }
    }
    Ok(fields)
}

impl SettingsView {
    fn save_source_control(&self, change: SourceControlChange, cx: &mut App) {
        self.update_scoped_server(change.into_server(), cx);
    }

    fn save_source_control_and_rescan(&self, change: SourceControlChange, on_settled: impl FnOnce(&mut Self, Option<String>, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        self.update_scoped_single_env_then(
            change.into_server(),
            move |error, cx| {
                cx.defer(move |cx| {
                    let _ = view.update(cx, |this, cx| {
                        let ok = error.is_none();
                        on_settled(this, error, cx);
                        if ok {
                            this.scan_source_control(true, cx);
                        }
                        cx.notify();
                    });
                });
            },
            cx,
        );
    }

    fn commit_custom_instructions(&mut self, cx: &mut Context<Self>) {
        let value = self.source_control.custom_instructions.read(cx).value().trim().to_owned();
        let saved = self.scoped_representative_config(cx).map(|config| config.settings.source_control.source_control_writing_style.custom_instructions);
        if saved.is_some_and(|saved| saved != value) {
            self.save_source_control(SourceControlChange::CustomInstructions(value), cx);
        }
    }

    fn commit_review_instructions(&mut self, cx: &mut Context<Self>) {
        let value = self.source_control.review_instructions.read(cx).value().trim().to_owned();
        let saved = self.scoped_representative_config(cx).map(|config| config.settings.source_control.review_guide_instructions);
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
        let Some(task) = self.state.read(cx).request(DISCOVER, json!({}), |value| Ok(decode_discovery(&value))) else { return };
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

    fn ensure_token_editor(&mut self, def: &HostDefinition, config: &ServerConfig, window: &mut Window, cx: &mut Context<Self>) {
        let creds = read_host_credential_settings(&config.settings.source_control_hosts, def.kind);
        let default_host = def.public_host.unwrap_or_default().to_owned();
        if !self.source_control.token_editors.contains_key(def.kind) {
            let host = cx.new(|cx| InputState::new(window, cx).default_value(default_host.clone()).placeholder(default_host.clone()));
            let value = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Paste a personal access token"));
            let subscriptions = vec![
                cx.subscribe(&host, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                }),
                cx.subscribe(&value, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                }),
            ];
            self.source_control.token_editors.insert(
                def.kind.to_owned(),
                TokenEditorState {
                    host,
                    value,
                    source: STORED_SECRET.to_owned(),
                    saving: false,
                    synced_key: String::new(),
                    _subscriptions: subscriptions,
                },
            );
        }
        let Some(editor) = self.source_control.token_editors.get_mut(def.kind) else { return };
        let normalized_host = editor.host.read(cx).value().trim().to_ascii_lowercase();
        let saved = creds.tokens.get(&normalized_host);
        let key = format!("{normalized_host}:{:?}", saved);
        if editor.synced_key != key {
            editor.synced_key = key;
            match saved {
                Some(SecretValue::Reference { provider, reference }) => {
                    editor.source = provider.clone();
                    editor.value.update(cx, |input, cx| {
                        input.set_masked(false, window, cx);
                        input.set_value(reference.clone(), window, cx);
                    });
                }
                _ => {
                    editor.source = STORED_SECRET.to_owned();
                    editor.value.update(cx, |input, cx| {
                        input.set_masked(true, window, cx);
                        input.set_value(String::new(), window, cx);
                    });
                }
            }
        }
    }

    fn ensure_generic_editor(&mut self, def: &HostDefinition, config: &ServerConfig, window: &mut Window, cx: &mut Context<Self>) {
        let saved = read_host_fields(&config.settings.source_control_hosts, def.kind);
        let signature = format!("{:?}", saved);
        let mut new_subscriptions = Vec::new();
        let editor = self.source_control.generic_editors.entry(def.kind.to_owned()).or_insert_with(|| GenericHostState {
            inputs: HashMap::new(),
            sources: HashMap::new(),
            cleared_fields: HashSet::new(),
            saving: false,
            synced_signature: String::new(),
            _subscriptions: Vec::new(),
        });
        for field in def.fields {
            if !editor.inputs.contains_key(field.key) {
                let input = cx.new(|cx| {
                    let input = InputState::new(window, cx).placeholder(field.placeholder);
                    if field.secret { input.masked(true) } else { input }
                });
                let kind_for_edit = def.kind.to_owned();
                let key_for_edit = field.key.to_owned();
                new_subscriptions.push(cx.subscribe(&input, move |this, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        if !input.read(cx).value().trim().is_empty()
                            && let Some(ed) = this.source_control.generic_editors.get_mut(&kind_for_edit)
                        {
                            ed.cleared_fields.remove(&key_for_edit);
                        }
                        cx.notify();
                    }
                }));
                editor.inputs.insert(field.key.to_owned(), input);
            }
            editor.sources.entry(field.key.to_owned()).or_insert_with(|| STORED_SECRET.to_owned());
        }
        editor._subscriptions.extend(new_subscriptions);
        if editor.synced_signature != signature {
            editor.synced_signature = signature;
            editor.cleared_fields.clear();
            for field in def.fields {
                let Some(input) = editor.inputs.get(field.key) else { continue };
                match saved.get(field.key) {
                    Some(SecretValue::Reference { provider, reference }) if field.secret => {
                        editor.sources.insert(field.key.to_owned(), provider.clone());
                        input.update(cx, |input, cx| {
                            input.set_masked(false, window, cx);
                            input.set_value(reference.clone(), window, cx);
                        });
                    }
                    Some(SecretValue::Literal(text)) if !field.secret => {
                        editor.sources.insert(field.key.to_owned(), STORED_SECRET.to_owned());
                        input.update(cx, |input, cx| input.set_value(text.clone(), window, cx));
                    }
                    _ => {
                        editor.sources.insert(field.key.to_owned(), STORED_SECRET.to_owned());
                        input.update(cx, |input, cx| {
                            if field.secret {
                                input.set_masked(true, window, cx);
                            }
                            input.set_value(String::new(), window, cx);
                        });
                    }
                }
            }
        }
    }

    fn start_app_sign_in(&mut self, kind: String, host: String, cx: &mut Context<Self>) {
        self.source_control.sign_in_seq += 1;
        let seq = self.source_control.sign_in_seq;
        self.source_control.sign_in.insert(kind.clone(), SignInState::Starting);
        cx.notify();
        let payload = json!({ "provider": kind, "host": host });
        let Some(start_task) = self.state.read(cx).request(SIGN_IN_START, payload.clone(), |v| decode_sign_in_prompt(&v)) else {
            self.source_control.sign_in.insert(kind, SignInState::Failed("Not connected to server.".into()));
            cx.notify();
            return;
        };
        let state = self.state.clone();
        cx.spawn(async move |this, cx| {
            let started = start_task.await.unwrap_or_else(|e| Err(e.to_string()));
            let prompt = match started {
                Ok(prompt) => prompt,
                Err(message) => {
                    let _ = this.update(cx, |this, cx| {
                        this.source_control.sign_in.insert(kind, SignInState::Failed(message));
                        cx.notify();
                    });
                    return;
                }
            };
            let wait_task = state.read_with(cx, |app, _| app.request(SIGN_IN_WAIT, payload, |_| Ok(())));
            let _ = this.update(cx, |this, cx| {
                this.source_control.sign_in.insert(kind.clone(), SignInState::Waiting { seq, prompt: prompt.clone() });
                cx.notify();
            });
            let Some(wait_task) = wait_task else { return };
            let finished = wait_task.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update(cx, |this, cx| {
                let still_waiting = matches!(
                    this.source_control.sign_in.get(&kind),
                    Some(SignInState::Waiting { seq: active_seq, .. }) if *active_seq == seq
                );
                if !still_waiting {
                    return;
                }
                match finished {
                    Ok(()) => {
                        this.source_control.sign_in.insert(kind, SignInState::Idle);
                        this.scan_source_control(true, cx);
                    }
                    Err(message) => {
                        this.source_control.sign_in.insert(kind, SignInState::Failed(message));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel_app_sign_in(&mut self, kind: String, host: String, cx: &mut Context<Self>) {
        self.source_control.sign_in.insert(kind.clone(), SignInState::Idle);
        if let Some(task) = self.state.read(cx).request(SIGN_IN_CANCEL, json!({ "provider": kind, "host": host }), |_| Ok(())) {
            cx.spawn(async move |_, _| {
                let _ = task.await;
            })
            .detach();
        }
        cx.notify();
    }

    pub(super) fn source_control_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let Some(config) = self.scoped_representative_config(cx) else {
            return vec![Block::new(
                &["source-control"],
                section(
                    "Server environment",
                    vec![
                        div()
                            .px(r(16.))
                            .py(r(12.))
                            .child(muted_text("Connect an environment to inspect its version control tools and hosting integrations.", cx))
                            .into_any_element(),
                    ],
                    cx,
                ),
            )];
        };
        self.scan_source_control(false, cx);
        let settings = &config.settings.source_control;
        Self::sync_textarea(&self.source_control.custom_instructions, &settings.source_control_writing_style.custom_instructions, window, cx);
        Self::sync_textarea(&self.source_control.review_instructions, &settings.review_guide_instructions, window, cx);

        let mut blocks = vec![Block::new(&["automatic-pull", "remove-agent-credits-on-merge", "pull-request-merge-method"], self.repositories_section(&config, cx))];
        blocks.extend(self.discovery_blocks(&config, window, cx));
        blocks.push(Block::new(
            &[
                "source-control-text-generation",
                "source-control-writing-style",
                "review-guides",
                "review-guide-model",
                "review-guide-instructions",
                "follow-change-request-templates",
                "source-control-writer-model",
            ],
            self.writing_section(&config, cx),
        ));
        blocks
    }

    fn repositories_section(&self, config: &ServerConfig, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = &config.settings.source_control;
        let view = cx.entity().downgrade();
        let switch = |id: &'static str, checked: bool, change: fn(bool) -> SourceControlChange| {
            let view = view.clone();
            Switch::new(id, checked).on_change(move |checked, _, cx| {
                let _ = view.update(cx, |this, cx| this.update_scoped_server(change(checked).into_server(), cx));
            })
        };
        let merge_view = view.clone();
        let method = config.settings.pull_request_merge_method;
        section(
            "Repositories",
            vec![
                self.apply_scoped_reset(
                    Row::new("automatic-pull", "Automatically pull")
                        .description("Keeps the default branch current when the checkout has no local changes or commits. Projects can override it.")
                        .highlighted(self.is_target("automatic-pull")),
                    &["defaultAutoPull"],
                    settings.default_auto_pull,
                    SourceControlChange::AutoPull(false).into_server(),
                    cx,
                )
                .control(switch("automatic-pull-switch", settings.default_auto_pull, SourceControlChange::AutoPull))
                .into_any_element(),
                self.apply_scoped_reset(
                    Row::new("remove-agent-credits-on-merge", "Remove agent credits when merging")
                        .description("Remove recognized agent credit lines from merge and squash messages on hosts that support it, keeping human co-authors. Includes auto-merge. Excludes merge queues, stack merges, and existing commits.")
                        .highlighted(self.is_target("remove-agent-credits-on-merge")),
                    &["removeAgentCreditsOnMerge"],
                    settings.remove_agent_credits_on_merge,
                    SourceControlChange::RemoveAgentCredits(false).into_server(),
                    cx,
                )
                .control(switch("remove-agent-credits-switch", settings.remove_agent_credits_on_merge, SourceControlChange::RemoveAgentCredits))
                .into_any_element(),
                self.apply_scoped_reset(
                    Row::new("pull-request-merge-method", "Default merge method")
                        .description("Pull requests start with this method. Last selected reuses whatever you chose most recently on this device.")
                        .highlighted(self.is_target("pull-request-merge-method")),
                    &["pullRequestMergeMethod"],
                    method.is_some(),
                    SourceControlChange::MergeMethod(None).into_server(),
                    cx,
                )
                .control(
                    Select::new("pull-request-merge-method-select", MERGE_METHODS.iter().map(|(value, label)| SelectOption::new(*value, *label)).collect())
                        .selected(merge_method_value(method))
                        .width(196.)
                        .on_change(move |value, _, cx| {
                            if let Some(method) = parse_merge_method(&value) {
                                let _ = merge_view.update(cx, |this, cx| {
                                    this.update_scoped_server(SourceControlChange::MergeMethod(method).into_server(), cx);
                                });
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

    fn discovery_blocks(&mut self, config: &ServerConfig, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
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
            let rows = discovery.version_control_systems.iter().map(|item| self.discovery_row(item, config, window, cx)).collect();
            blocks.push(Block::new(&["source-control"], titled("Version Control", Some(self.rescan_button(cx)), rows, cx)));
        }
        if !discovery.source_control_providers.is_empty() {
            let rows = discovery.source_control_providers.iter().map(|item| self.discovery_row(item, config, window, cx)).collect();
            let action = (!has_vcs).then(|| self.rescan_button(cx));
            let anchors: &[&'static str] = if has_vcs {
                &["source-control-host-github", "source-control-host-bitbucket", "source-control-host-gitcafe"]
            } else {
                &["source-control", "source-control-host-github", "source-control-host-bitbucket", "source-control-host-gitcafe"]
            };
            blocks.push(Block::new(anchors, titled("Source control providers", action, rows, cx)));
        }
        blocks
    }

    fn discovery_row(&mut self, item: &DiscoveryItem, config: &ServerConfig, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let muted = hsla(p.muted_foreground);
        let can_write = self.may(SETTINGS_WRITE_SCOPE, cx);
        let definition = item.auth.as_ref().and_then(|_| host_definition(&item.kind));
        let has_details = definition.is_some();
        let expanded = has_details
            && (self.source_control.expanded_providers.contains(&item.kind) || self.is_target(&format!("source-control-host-{}", item.kind)));
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
                            .child(mask_account(&account, revealed))
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
        let mut right = h_flex().gap(r(8.)).flex_none();
        if has_details {
            let toggle_kind = item.kind.clone();
            right = right.child(
                Button::new(SharedString::from(format!("source-control-{}-expand", item.kind)), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                    .icon(if expanded { Icon::ChevronUp } else { Icon::ChevronDown })
                    .tooltip(format!("Toggle {} details", item.label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.source_control.expanded_providers.remove(&toggle_kind) {
                            this.source_control.expanded_providers.insert(toggle_kind.clone());
                        }
                        cx.notify();
                    })),
            );
        }
        if item.auth.is_some() {
            let offered = config.settings.source_control.is_provider_offered(&item.kind);
            let offer_kind = item.kind.clone();
            let offer_view = cx.entity().downgrade();
            let offer_disabled = !can_write || self.is_project_scope(cx);
            right = right.child(
                div()
                    .id(SharedString::from(format!("source-control-{}-offered-tip", item.kind)))
                    .tooltip(tooltip(
                        if self.is_project_scope(cx) {
                            "Environment-wide setting. Select an environment to change it."
                        } else {
                            "Offer when adding or publishing a project"
                        }
                        .into(),
                    ))
                    .child(
                        Switch::new(SharedString::from(format!("source-control-{}-offered", item.kind)), offered)
                            .disabled(offer_disabled)
                            .on_change(move |checked, _, cx| {
                                let _ = offer_view.update(cx, |this, cx| {
                                    this.update_scoped_single_env(
                                        SourceControlChange::OfferProvider { kind: offer_kind.clone(), offered: checked }.into_server(),
                                        cx,
                                    );
                                });
                            }),
                    ),
            );
        } else if !item.coming_soon() {
            right = right.child(Switch::new(SharedString::from(format!("source-control-{}-availability", item.kind)), item.enabled()).disabled(true));
        }
        let header = h_flex()
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
            .child(right);
        let mut container = v_flex().child(header);
        if expanded && let Some(def) = definition {
            let panel = self.host_settings_panel(item, def, config, can_write, window, cx);
            container = container.child(
                div()
                    .px(r(16.))
                    .pb(r(16.))
                    .pt(r(4.))
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.4))
                    .child(panel),
            );
        }
        container.into_any_element()
    }

    fn host_settings_panel(
        &mut self,
        item: &DiscoveryItem,
        def: &HostDefinition,
        config: &ServerConfig,
        can_write: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        if let Some(credentials) = def.credentials {
            let mut body = v_flex().gap(r(20.)).pt(r(8.));
            if let Some(app_sign_in) = credentials.app_sign_in
                && item.app_sign_in
            {
                body = body.child(self.app_sign_in_panel(item, def, app_sign_in, config, can_write, cx));
                body = body.child(
                    h_flex()
                        .gap(r(10.))
                        .text_size(r(11.))
                        .text_color(hsla(p.muted_foreground))
                        .child(div().h(r(1.)).flex_1().bg(hsla(p.border).opacity(0.7)))
                        .child("Or use your own credentials")
                        .child(div().h(r(1.)).flex_1().bg(hsla(p.border).opacity(0.7))),
                );
            }
            body = body.child(self.token_settings_panel(def, credentials, config, can_write, window, cx));
            if item.available
                && let Some(auth) = &item.auth
            {
                body = body.child(self.account_settings_panel(def, credentials, auth, config, can_write, cx));
            }
            body.into_any_element()
        } else {
            self.generic_host_panel(def, config, can_write, window, cx)
        }
    }

    fn app_sign_in_panel(
        &mut self,
        item: &DiscoveryItem,
        def: &HostDefinition,
        app_sign_in: AppSignInDef,
        config: &ServerConfig,
        can_write: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let creds = read_host_credential_settings(&config.settings.source_control_hosts, def.kind);
        let signed_in = creds.sign_ins.get(app_sign_in.host).is_some_and(|v| !v.is_empty());
        let token_overrides = has_secret_value(creds.tokens.get(app_sign_in.host));
        let account = item.auth.as_ref().and_then(|a| a.account.clone());
        let state = self.source_control.sign_in.get(def.kind).cloned().unwrap_or_default();

        let mut card = v_flex()
            .gap(r(12.))
            .p(r(12.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.7))
            .bg(hsla(p.card).opacity(0.5));

        let status_line: AnyElement = if signed_in {
            if let Some(account) = account.filter(|_| !token_overrides) {
                let reveal_key = format!("app-signin-{}", def.kind);
                let revealed = self.source_control.revealed_accounts.contains(&reveal_key);
                h_flex()
                    .gap(r(4.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child("Signed in as")
                    .child(
                        div()
                            .id(SharedString::from(format!("sc-signin-account-{}", def.kind)))
                            .cursor_pointer()
                            .text_color(hsla(p.foreground))
                            .child(mask_account(&account, revealed))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.source_control.revealed_accounts.remove(&reveal_key) {
                                    this.source_control.revealed_accounts.insert(reveal_key.clone());
                                }
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            } else if token_overrides {
                div()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child("Signed in (a saved token below currently overrides this session).")
                    .into_any_element()
            } else {
                div().text_xs().text_color(hsla(p.muted_foreground)).child("Signed in.").into_any_element()
            }
        } else {
            div()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(format!("Sign in with {} to work with pull requests without installing the CLI.", app_sign_in.app_name))
                .into_any_element()
        };

        let mut actions = h_flex().gap(r(8.)).flex_wrap();
        if signed_in {
            let install_url = app_sign_in.install_url;
            let revoke_url = app_sign_in.revoke_url;
            let kind = def.kind.to_owned();
            let host = app_sign_in.host.to_owned();
            let existing = creds.sign_ins.clone();
            actions = actions
                .child(
                    Button::new(SharedString::from(format!("sc-install-{}", def.kind)), ButtonVariant::Outline, ButtonSize::Sm)
                        .icon(Icon::ExternalLink)
                        .label("Manage repositories")
                        .on_click(cx.listener(move |_, _, _, cx| cx.open_url(install_url))),
                )
                .child(
                    Button::new(SharedString::from(format!("sc-revoke-{}", def.kind)), ButtonVariant::GhostMuted, ButtonSize::Sm)
                        .icon(Icon::ExternalLink)
                        .label("Revoke access")
                        .on_click(cx.listener(move |_, _, _, cx| cx.open_url(revoke_url))),
                )
                .child(
                    Button::new(SharedString::from(format!("sc-signout-{}", def.kind)), ButtonVariant::Outline, ButtonSize::Sm)
                        .label("Sign out")
                        .disabled(!can_write)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.source_control.sign_in.insert(kind.clone(), SignInState::Idle);
                            this.save_source_control_and_rescan(
                                SourceControlChange::SignOutApp {
                                    kind: kind.clone(),
                                    existing: existing.clone(),
                                    host: host.clone(),
                                },
                                |_, _, _| {},
                                cx,
                            );
                        })),
                );
        } else if !matches!(state, SignInState::Waiting { .. }) {
            let kind = def.kind.to_owned();
            let host = app_sign_in.host.to_owned();
            let starting = matches!(state, SignInState::Starting);
            actions = actions.child(
                Button::new(SharedString::from(format!("sc-signin-{}", def.kind)), ButtonVariant::Default, ButtonSize::Sm)
                    .label(if starting { "Starting…" } else { "Sign in with GitHub" })
                    .disabled(!can_write || starting)
                    .on_click(cx.listener(move |this, _, _, cx| this.start_app_sign_in(kind.clone(), host.clone(), cx))),
            );
        }

        card = card.child(
            h_flex()
                .justify_between()
                .gap(r(12.))
                .child(
                    v_flex()
                        .gap(r(2.))
                        .flex_1()
                        .min_w_0()
                        .child(
                            h_flex()
                                .gap(r(6.))
                                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(app_sign_in.app_name))
                                .when(signed_in, |row| row.child(tone_badge("Signed in", Tone::Success, cx))),
                        )
                        .child(status_line),
                )
                .child(actions),
        );

        match state {
            SignInState::Waiting { prompt, .. } if !signed_in => {
                let code_cells = h_flex().gap(r(4.)).children(prompt.user_code.chars().map(|ch| {
                    if ch == '-' {
                        div().px(r(2.)).text_sm().text_color(hsla(p.muted_foreground)).child("–").into_any_element()
                    } else {
                        div()
                            .w(r(28.))
                            .h(r(34.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(r(6.))
                            .border_1()
                            .border_color(hsla(p.border))
                            .bg(hsla(p.background))
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child(ch.to_string())
                            .into_any_element()
                    }
                }));
                let copy_code = prompt.user_code.clone();
                let open_uri = prompt.verification_uri.clone();
                let cancel_kind = def.kind.to_owned();
                let cancel_host = app_sign_in.host.to_owned();
                card = card.child(
                    v_flex()
                        .gap(r(10.))
                        .pt(r(8.))
                        .border_t_1()
                        .border_color(hsla(p.border).opacity(0.5))
                        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Enter this code on GitHub to finish signing in:"))
                        .child(code_cells)
                        .child(
                            h_flex()
                                .gap(r(8.))
                                .child(
                                    Button::new(SharedString::from(format!("sc-copy-code-{}", def.kind)), ButtonVariant::Outline, ButtonSize::Sm)
                                        .icon(Icon::Copy)
                                        .label("Copy code")
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(copy_code.clone()));
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("sc-open-device-{}", def.kind)), ButtonVariant::Default, ButtonSize::Sm)
                                        .icon(Icon::ExternalLink)
                                        .label("Open GitHub")
                                        .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&open_uri))),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("sc-cancel-signin-{}", def.kind)), ButtonVariant::GhostMuted, ButtonSize::Sm)
                                        .label("Cancel")
                                        .on_click(cx.listener(move |this, _, _, cx| this.cancel_app_sign_in(cancel_kind.clone(), cancel_host.clone(), cx))),
                                ),
                        ),
                );
            }
            SignInState::Failed(message) => {
                card = card.child(div().text_xs().text_color(hsla(p.error_foreground)).child(message));
            }
            _ => {}
        }

        card.into_any_element()
    }

    fn token_settings_panel(
        &mut self,
        def: &HostDefinition,
        credentials: HostCredentialsDef,
        config: &ServerConfig,
        can_write: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_token_editor(def, config, window, cx);
        let p = palette(cx);
        let creds = read_host_credential_settings(&config.settings.source_control_hosts, def.kind);
        let Some(editor) = self.source_control.token_editors.get(def.kind) else {
            return div().into_any_element();
        };
        let host_input = editor.host.clone();
        let value_input = editor.value.clone();
        let source = editor.source.clone();
        let saving = editor.saving;

        let normalized_host = host_input.read(cx).value().trim().to_ascii_lowercase();
        let saved_token = creds.tokens.get(&normalized_host);
        let is_saved = has_secret_value(saved_token);
        let saved_hosts: Vec<String> = creds
            .tokens
            .iter()
            .filter(|(_, val)| has_secret_value(Some(*val)))
            .map(|(h, _)| h.clone())
            .collect();
        let raw_value = value_input.read(cx).value().trim().to_owned();
        let submission: Option<SecretValue> = if source == STORED_SECRET {
            (!raw_value.is_empty()).then_some(SecretValue::Literal(raw_value.clone()))
        } else {
            SecretValue::reference(&source, &raw_value)
        };
        let can_save = can_write && !saving && !normalized_host.is_empty() && submission.is_some();
        let new_token_host = if normalized_host.is_empty() {
            def.public_host.unwrap_or_default().to_owned()
        } else {
            normalized_host.clone()
        };
        let token_url = credentials.new_token_url(&new_token_host);
        let has_secret_providers = !config.settings.secret_providers.0.is_empty();
        let placeholder = secret_placeholder(&source, is_saved && !saved_token.is_some_and(SecretValue::is_reference), "Paste a personal access token");

        let kind_for_source = def.kind.to_owned();
        let view_for_source = cx.entity().downgrade();
        let source_picker = has_secret_providers.then(|| {
            Select::new(SharedString::from(format!("sc-token-source-{}", def.kind)), secret_source_options(config))
                .selected(source.clone())
                .width(156.)
                .disabled(!can_write || saving)
                .on_change(move |next, window, cx| {
                    let next_str = next.to_string();
                    let _ = view_for_source.update(cx, |this, cx| {
                        if let Some(ed) = this.source_control.token_editors.get_mut(&kind_for_source) {
                            let is_stored = next_str == STORED_SECRET;
                            ed.source = next_str;
                            ed.value.update(cx, |input, cx| {
                                input.set_masked(is_stored, window, cx);
                                input.set_value(String::new(), window, cx);
                            });
                            cx.notify();
                        }
                    });
                })
        });

        let save_kind = def.kind.to_owned();
        let save_existing = creds.tokens.clone();
        let save_host = normalized_host.clone();
        let save_submission = submission.clone();

        let remove_kind = def.kind.to_owned();
        let remove_existing = creds.tokens.clone();
        let remove_host = normalized_host.clone();

        let saved_summary = if saved_hosts.is_empty() {
            "No token saved.".to_owned()
        } else {
            format!("Saved for {}", saved_hosts.join(", "))
        };

        v_flex()
            .gap(r(10.))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(r(4.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(format!(
                        "A token saved here is used before {} and the {} login, so {} works without the {}. Give it {}.",
                        credentials.token_env_var, credentials.cli_name, def.label, credentials.cli_label, credentials.token_access
                    ))
                    .when(!new_token_host.is_empty(), |row| {
                        row.child(
                            Button::new(SharedString::from(format!("sc-create-token-{}", def.kind)), ButtonVariant::GhostMuted, ButtonSize::Compact)
                                .icon(Icon::ExternalLink)
                                .label("Create a token")
                                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&token_url))),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap(r(10.))
                    .items_end()
                    .child(
                        v_flex()
                            .w(r(180.))
                            .gap(r(4.))
                            .on_key_down(stop_slash_shortcut)
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Host"))
                            .child(Input::new(&host_input).small().disabled(!can_write || saving)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(r(4.))
                            .on_key_down(stop_slash_shortcut)
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Token"))
                            .child(
                                h_flex()
                                    .gap(r(6.))
                                    .children(source_picker)
                                    .child(div().flex_1().min_w_0().child(Input::new(&value_input).small().disabled(!can_write || saving))),
                            )
                            .child(div().text_size(r(11.)).text_color(hsla(p.muted_foreground)).child(placeholder)),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap(r(12.))
                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(saved_summary))
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .when(is_saved, |row| {
                                row.child(
                                    Button::new(SharedString::from(format!("sc-token-remove-{}", def.kind)), ButtonVariant::Outline, ButtonSize::Sm)
                                        .label("Remove")
                                        .disabled(!can_write || saving)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(ed) = this.source_control.token_editors.get_mut(&remove_kind) {
                                                ed.saving = true;
                                            }
                                            cx.notify();
                                            let kind_done = remove_kind.clone();
                                            this.save_source_control_and_rescan(
                                                SourceControlChange::SaveHostToken {
                                                    kind: remove_kind.clone(),
                                                    existing: remove_existing.clone(),
                                                    host: remove_host.clone(),
                                                    token: SecretValue::Literal(String::new()),
                                                },
                                                move |this, _, _| {
                                                    if let Some(ed) = this.source_control.token_editors.get_mut(&kind_done) {
                                                        ed.saving = false;
                                                        ed.synced_key.clear();
                                                    }
                                                },
                                                cx,
                                            );
                                        })),
                                )
                            })
                            .child(
                                Button::new(SharedString::from(format!("sc-token-save-{}", def.kind)), ButtonVariant::Default, ButtonSize::Sm)
                                    .label(if saving { "Saving…" } else { "Save token" })
                                    .disabled(!can_save)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let Some(token) = save_submission.clone() else { return };
                                        if let Some(ed) = this.source_control.token_editors.get_mut(&save_kind) {
                                            ed.saving = true;
                                        }
                                        cx.notify();
                                        let kind_done = save_kind.clone();
                                        this.save_source_control_and_rescan(
                                            SourceControlChange::SaveHostToken {
                                                kind: save_kind.clone(),
                                                existing: save_existing.clone(),
                                                host: save_host.clone(),
                                                token,
                                            },
                                            move |this, _, _| {
                                                if let Some(ed) = this.source_control.token_editors.get_mut(&kind_done) {
                                                    ed.saving = false;
                                                    ed.synced_key.clear();
                                                }
                                            },
                                            cx,
                                        );
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn account_settings_panel(
        &mut self,
        def: &HostDefinition,
        credentials: HostCredentialsDef,
        auth: &source_control::ProviderAuth,
        config: &ServerConfig,
        can_write: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let groups = group_host_accounts(&auth.accounts);
        if groups.is_empty() {
            return div().into_any_element();
        }
        let creds = read_host_credential_settings(&config.settings.source_control_hosts, def.kind);
        let revealed = self.source_control.accounts_revealed.contains(def.kind);
        let toggle_kind = def.kind.to_owned();

        let mut list = v_flex()
            .gap(r(8.))
            .pt(r(8.))
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.5))
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child(format!("{} CLI accounts ({})", credentials.cli_label, credentials.cli_name)),
                    )
                    .child(
                        Button::new(SharedString::from(format!("sc-reveal-accounts-{}", def.kind)), ButtonVariant::GhostMuted, ButtonSize::Compact)
                            .label(if revealed { "Hide accounts" } else { "Reveal accounts" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.source_control.accounts_revealed.remove(&toggle_kind) {
                                    this.source_control.accounts_revealed.insert(toggle_kind.clone());
                                }
                                cx.notify();
                            })),
                    ),
            );

        for group_item in groups {
            let choice = creds.hosts.get(&group_item.host);
            let enabled = choice.is_none_or(|c| c.enabled);
            let pinned = choice.and_then(|c| c.account.clone());
            let stale_pin = pinned.as_ref().is_some_and(|acc| !group_item.selectable.contains(acc));

            let active_label = match &group_item.active_account {
                Some(active) => format!("Active login ({})", mask_account(active, revealed)),
                None => "Active login".to_owned(),
            };
            let mut options = vec![SelectOption::new(ACTIVE_ACCOUNT, active_label)];
            for acc in &group_item.selectable {
                options.push(SelectOption::new(acc.clone(), mask_account(acc, revealed)));
            }
            let selected_value = pinned.clone().filter(|acc| group_item.selectable.contains(acc)).unwrap_or_else(|| ACTIVE_ACCOUNT.to_owned());

            let enable_kind = def.kind.to_owned();
            let enable_host = group_item.host.clone();
            let enable_hosts = creds.hosts.clone();
            let view_enable = cx.entity().downgrade();

            let select_kind = def.kind.to_owned();
            let select_host = group_item.host.clone();
            let select_hosts = creds.hosts.clone();
            let view_select = cx.entity().downgrade();

            let mut row = v_flex()
                .gap(r(6.))
                .p(r(10.))
                .rounded(r(8.))
                .border_1()
                .border_color(hsla(p.border).opacity(0.6))
                .child(
                    h_flex()
                        .justify_between()
                        .gap(r(12.))
                        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(group_item.host.clone()))
                        .child(
                            h_flex()
                                .gap(r(8.))
                                .when(!group_item.selectable.is_empty(), |h| {
                                    let select_disabled = account_select_disabled(can_write, enabled);
                                    h.child(
                                        Select::new(SharedString::from(format!("sc-acct-{}-{}", def.kind, group_item.host)), options)
                                            .selected(selected_value)
                                            .width(196.)
                                            .disabled(select_disabled)
                                            .on_change(move |value, _, cx| {
                                                if select_disabled {
                                                    return;
                                                }
                                                let next_account = if value.as_ref() == ACTIVE_ACCOUNT { None } else { Some(value.to_string()) };
                                                let next = next_host_account_choices(&select_hosts, &select_host, None, Some(next_account));
                                                let _ = view_select.update(cx, |this, cx| {
                                                    this.save_source_control_and_rescan(
                                                        SourceControlChange::SaveHostAccounts { kind: select_kind.clone(), hosts: next },
                                                        |_, _, _| {},
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                })
                                .child(
                                    Switch::new(SharedString::from(format!("sc-host-enabled-{}-{}", def.kind, group_item.host)), enabled)
                                        .disabled(!can_write)
                                        .on_change(move |checked, _, cx| {
                                            let next = next_host_account_choices(&enable_hosts, &enable_host, Some(checked), None);
                                            let _ = view_enable.update(cx, |this, cx| {
                                                this.save_source_control_and_rescan(
                                                    SourceControlChange::SaveHostAccounts { kind: enable_kind.clone(), hosts: next },
                                                    |_, _, _| {},
                                                    cx,
                                                );
                                            });
                                        }),
                                ),
                        ),
                );

            if let Some(env_var) = &group_item.environment_variable {
                row = row.child(
                    div()
                        .text_size(r(11.))
                        .text_color(hsla(p.warning_foreground))
                        .child(format!("{env_var} is set in the server environment and overrides CLI account selection for this host.")),
                );
            }
            if stale_pin && let Some(pinned_name) = &pinned {
                let reset_kind = def.kind.to_owned();
                let reset_host = group_item.host.clone();
                let reset_hosts = creds.hosts.clone();
                row = row.child(
                    h_flex()
                        .gap(r(8.))
                        .child(
                            div()
                                .flex_1()
                                .text_size(r(11.))
                                .text_color(hsla(p.warning_foreground))
                                .child(format!(
                                    "Pinned account {} is no longer authenticated on {}.",
                                    mask_account(pinned_name, revealed),
                                    group_item.host
                                )),
                        )
                        .child(
                            Button::new(SharedString::from(format!("sc-reset-pin-{}-{}", def.kind, group_item.host)), ButtonVariant::Outline, ButtonSize::Compact)
                                .label("Use active login")
                                .disabled(!can_write)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let next = next_host_account_choices(&reset_hosts, &reset_host, None, Some(None));
                                    this.save_source_control_and_rescan(
                                        SourceControlChange::SaveHostAccounts { kind: reset_kind.clone(), hosts: next },
                                        |_, _, _| {},
                                        cx,
                                    );
                                })),
                        ),
                );
            }
            for broken in &group_item.broken {
                let detail = broken.detail.as_deref().unwrap_or("Not authenticated");
                row = row.child(
                    div()
                        .text_size(r(11.))
                        .text_color(hsla(p.muted_foreground))
                        .child(format!("{} — {detail}", mask_account(&broken.account, revealed))),
                );
            }
            list = list.child(row);
        }

        list.into_any_element()
    }

    fn generic_host_panel(
        &mut self,
        def: &HostDefinition,
        config: &ServerConfig,
        can_write: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_generic_editor(def, config, window, cx);
        let p = palette(cx);
        let saved = read_host_fields(&config.settings.source_control_hosts, def.kind);
        let has_saved = saved.values().any(|val| !val.is_empty());
        let has_secret_providers = !config.settings.secret_providers.0.is_empty();
        let Some(editor) = self.source_control.generic_editors.get(def.kind) else {
            return div().into_any_element();
        };
        let saving = editor.saving;
        let inputs = editor.inputs.clone();
        let sources = editor.sources.clone();
        let cleared_fields = editor.cleared_fields.clone();

        let mut body = v_flex().gap(r(12.)).pt(r(8.));
        for field in def.fields {
            let Some(input) = inputs.get(field.key) else { continue };
            let source = sources.get(field.key).cloned().unwrap_or_else(|| STORED_SECRET.to_owned());
            let field_saved = saved.get(field.key);
            let is_cleared = cleared_fields.contains(field.key);
            let field_has_saved = field.secret && has_secret_value(field_saved);
            let is_configured = field_has_saved && !is_cleared && !field_saved.is_some_and(SecretValue::is_reference);
            let kind_for_source = def.kind.to_owned();
            let key_for_source = field.key.to_owned();
            let view_for_source = cx.entity().downgrade();
            let source_picker = (field.secret && has_secret_providers).then(|| {
                Select::new(SharedString::from(format!("sc-field-source-{}-{}", def.kind, field.key)), secret_source_options(config))
                    .selected(source.clone())
                    .width(156.)
                    .disabled(!can_write || saving)
                    .on_change(move |next, window, cx| {
                        let next_str = next.to_string();
                        let _ = view_for_source.update(cx, |this, cx| {
                            if let Some(ed) = this.source_control.generic_editors.get_mut(&kind_for_source) {
                                let is_stored = next_str == STORED_SECRET;
                                ed.sources.insert(key_for_source.clone(), next_str);
                                if !is_stored {
                                    ed.cleared_fields.remove(&key_for_source);
                                }
                                if let Some(input) = ed.inputs.get(&key_for_source) {
                                    input.update(cx, |input, cx| {
                                        input.set_masked(is_stored, window, cx);
                                        input.set_value(String::new(), window, cx);
                                    });
                                }
                                cx.notify();
                            }
                        });
                    })
            });
            let clear_kind = def.kind.to_owned();
            let clear_key = field.key.to_owned();
            let clear_btn = (field_has_saved && !is_cleared).then(|| {
                Button::new(SharedString::from(format!("sc-field-clear-{}-{}", def.kind, field.key)), ButtonVariant::GhostMuted, ButtonSize::Compact)
                    .label("Clear")
                    .disabled(!can_write || saving)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(ed) = this.source_control.generic_editors.get_mut(&clear_kind) {
                            ed.cleared_fields.insert(clear_key.clone());
                            ed.sources.insert(clear_key.clone(), STORED_SECRET.to_owned());
                            if let Some(input) = ed.inputs.get(&clear_key) {
                                input.update(cx, |input, cx| {
                                    input.set_masked(true, window, cx);
                                    input.set_value(String::new(), window, cx);
                                });
                            }
                            cx.notify();
                        }
                    }))
            });
            let hint = if field.secret {
                if is_cleared {
                    "Will be cleared on Save.".to_owned()
                } else {
                    secret_placeholder(&source, is_configured, field.description)
                }
            } else {
                field.description.to_owned()
            };
            body = body.child(
                v_flex()
                    .gap(r(4.))
                    .on_key_down(stop_slash_shortcut)
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(field.label))
                    .child(
                        h_flex()
                            .gap(r(6.))
                            .children(source_picker)
                            .child(div().flex_1().min_w_0().child(Input::new(input).small().disabled(!can_write || saving)))
                            .children(clear_btn),
                    )
                    .child(div().text_size(r(11.)).text_color(hsla(p.muted_foreground)).child(hint)),
            );
        }

        let save_kind = def.kind.to_owned();
        let save_fields_def = def.fields;
        let save_saved = saved.clone();
        let remove_kind = def.kind.to_owned();
        let remove_keys: Vec<String> = saved.keys().cloned().collect();

        body = body.child(
            h_flex()
                .justify_end()
                .gap(r(8.))
                .when(has_saved, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("sc-generic-remove-{}", def.kind)), ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Remove")
                            .disabled(!can_write || saving)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let mut fields = Map::new();
                                for key in &remove_keys {
                                    fields.insert(key.clone(), Value::String(String::new()));
                                }
                                if let Some(ed) = this.source_control.generic_editors.get_mut(&remove_kind) {
                                    ed.saving = true;
                                }
                                cx.notify();
                                let kind_done = remove_kind.clone();
                                this.save_source_control_and_rescan(
                                    SourceControlChange::SaveHostFields { kind: remove_kind.clone(), fields },
                                    move |this, _, _| {
                                        if let Some(ed) = this.source_control.generic_editors.get_mut(&kind_done) {
                                            ed.saving = false;
                                            ed.cleared_fields.clear();
                                            ed.synced_signature.clear();
                                        }
                                    },
                                    cx,
                                );
                            })),
                    )
                })
                .child(
                    Button::new(SharedString::from(format!("sc-generic-save-{}", def.kind)), ButtonVariant::Default, ButtonSize::Sm)
                        .label(if saving { "Saving…" } else { "Save" })
                        .disabled(!can_write || saving)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let Some(ed) = this.source_control.generic_editors.get(&save_kind) else { return };
                            let raw_values: HashMap<String, String> = ed
                                .inputs
                                .iter()
                                .map(|(key, input)| (key.clone(), input.read(cx).value().to_string()))
                                .collect();
                            let fields = match build_generic_host_patch(save_fields_def, &save_saved, &raw_values, &ed.sources, &ed.cleared_fields) {
                                Ok(fields) => fields,
                                Err(message) => {
                                    store::show_error(message, cx);
                                    return;
                                }
                            };
                            if fields.is_empty() {
                                return;
                            }
                            if let Some(ed) = this.source_control.generic_editors.get_mut(&save_kind) {
                                ed.saving = true;
                            }
                            cx.notify();
                            let kind_done = save_kind.clone();
                            this.save_source_control_and_rescan(
                                SourceControlChange::SaveHostFields { kind: save_kind.clone(), fields },
                                move |this, _, _| {
                                    if let Some(ed) = this.source_control.generic_editors.get_mut(&kind_done) {
                                        ed.saving = false;
                                        ed.cleared_fields.clear();
                                        ed.synced_signature.clear();
                                    }
                                },
                                cx,
                            );
                        })),
                ),
        );

        body.into_any_element()
    }

    fn writing_section(&mut self, config: &ServerConfig, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_text_generator_models(config, cx);
        let settings = &config.settings.source_control;
        let style = &settings.source_control_writing_style;
        let env_only = self.env_only_unavailable(cx);
        let scope_project = self.is_project_scope(cx).then(|| self.scope_project_id(cx)).flatten();
        let writer = source_control::writer_model(&config.settings);
        let writer_seed = writer.clone().or_else(|| {
            t3_core::settings::resolve_default_text_generation_selection(
                config,
                scope_project.as_deref(),
                self.text_gen_models_for(config),
            )
        });
        let review_options = self.apply_scoped_model_availability(config, model_options(config, scope_project.as_deref()), false, cx);
        let writer_options = self.apply_scoped_model_availability(
            config,
            self.text_gen_select_options(
                config,
                scope_project.as_deref(),
                &[settings.source_control_writer_model_selection.as_ref(), writer_seed.as_ref()],
            ),
            true,
            cx,
        );
        let view = cx.entity().downgrade();
        let mode_view = view.clone();
        let custom = style.mode == WritingMode::Custom;
        let instructions = self.source_control.custom_instructions.clone();
        let is_project_scope = self.is_project_scope(cx);
        let has_style_override = self.has_project_override(&["sourceControlWritingStyle"], cx);
        let env_style = self
            .scoped_raw_representative_config(cx)
            .map(|cfg| cfg.settings.source_control.source_control_writing_style)
            .unwrap_or_default();
        let style_differs =
            style.mode != env_style.mode || style.custom_instructions != env_style.custom_instructions;
        let templates_differs =
            style.follow_change_request_templates != env_style.follow_change_request_templates;
        let writing_row = Row::new("source-control-writing-style", "Source control writing style")
            .description(style.mode.description())
            .highlighted(self.is_target("source-control-writing-style"));
        let writing = if is_project_scope {
            let reset_view = view.clone();
            let env_mode = env_style.mode;
            let env_instructions = env_style.custom_instructions.clone();
            writing_row.reset_tooltip("Reset to inherited value").reset(
                has_style_override && (style_differs || !templates_differs),
                move |_, _, cx| {
                    let _ = reset_view.update(cx, |this, cx| {
                        if templates_differs {
                            this.update_scoped_server(
                                SourceControlChange::WritingMode {
                                    mode: env_mode,
                                    custom_instructions: Some(env_instructions.clone()),
                                }
                                .into_server(),
                                cx,
                            );
                        } else {
                            this.clear_project_overrides(&["sourceControlWritingStyle"], cx);
                        }
                    });
                },
            )
        } else {
            let reset_view = view.clone();
            writing_row.reset(!style.is_default_style(), move |_, _, cx| {
                let _ = reset_view.update(cx, |this, cx| {
                    this.update_scoped_server(SourceControlChange::ResetWritingStyle.into_server(), cx);
                });
            })
        }
            .control(
                Select::new("source-control-writing-style-select", WritingMode::ALL.iter().map(|mode| SelectOption::new(mode.as_str(), mode.label())).collect())
                    .selected(style.mode.as_str())
                    .width(196.)
                    .on_change(move |value, _, cx| {
                        let Some(mode) = WritingMode::parse(&value) else { return };
                        let custom_instructions = custom.then(|| instructions.read(cx).value().trim().to_owned());
                        let _ = mode_view.update(cx, |this, cx| {
                            this.update_scoped_server(SourceControlChange::WritingMode { mode, custom_instructions }.into_server(), cx);
                        });
                    }),
            )
            .when(custom, |row| row.below(div().mt(r(12.)).pb(r(14.)).max_w(r(672.)).child(Textarea::new(&self.source_control.custom_instructions))));

        let guides_view = view.clone();
        let mut rows = vec![
            writing.into_any_element(),
            Row::new("review-guides", "Review guides (beta)")
                .description("Add a Guide tab to pull requests. An agent explains the changes and points out decisions that need review.")
                .highlighted(self.is_target("review-guides"))
                .unavailable(env_only.clone())
                .control(Switch::new("review-guides-switch", settings.review_guides_enabled).disabled(env_only.is_some()).on_change(move |on, _, cx| {
                    let _ = guides_view.update(cx, |this, cx| {
                        this.update_scoped_server(SourceControlChange::ReviewGuides(on).into_server(), cx);
                    });
                }))
                .into_any_element(),
        ];
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
                        allow_text_generators: false,
                        change: SourceControlChange::ReviewGuideModel,
                    },
                    &review_options,
                    cx,
                ),
            );
            rows.push(
                self.apply_scoped_reset(
                    Row::new("review-guide-instructions", "Review guide instructions")
                        .description("Your preferences for review guides: what to show first, dwell on, or skip. The guide's format and coverage stay the same."),
                    &["reviewGuideInstructions"],
                    !settings.review_guide_instructions.is_empty(),
                    SourceControlChange::ReviewGuideInstructions(String::new()).into_server(),
                    cx,
                )
                .below(div().mt(r(12.)).pb(r(14.)).max_w(r(672.)).child(Textarea::new(&self.source_control.review_instructions)))
                .into_any_element(),
            );
        }
        let templates_view = view.clone();
        let templates_row = Row::new("follow-change-request-templates", "Follow change request templates")
            .description("Use the repository's template for change request descriptions when available.")
            .highlighted(self.is_target("follow-change-request-templates"));
        let templates_row = if is_project_scope {
            let reset_view = view.clone();
            let env_templates = env_style.follow_change_request_templates;
            templates_row.reset_tooltip("Reset to inherited value").reset(
                has_style_override && templates_differs,
                move |_, _, cx| {
                    let _ = reset_view.update(cx, |this, cx| {
                        if style_differs {
                            this.update_scoped_server(
                                SourceControlChange::FollowTemplates(env_templates).into_server(),
                                cx,
                            );
                        } else {
                            this.clear_project_overrides(&["sourceControlWritingStyle"], cx);
                        }
                    });
                },
            )
        } else {
            let reset_view = view.clone();
            templates_row.reset(!style.follow_change_request_templates, move |_, _, cx| {
                let _ = reset_view.update(cx, |this, cx| {
                    this.update_scoped_server(SourceControlChange::FollowTemplates(true).into_server(), cx);
                });
            })
        };
        let advanced_rows = vec![
            templates_row
                .control(Switch::new("follow-change-request-templates-switch", style.follow_change_request_templates).on_change(move |on, _, cx| {
                    let _ = templates_view.update(cx, |this, cx| {
                        this.update_scoped_server(SourceControlChange::FollowTemplates(on).into_server(), cx);
                    });
                }))
                .into_any_element(),
            self.model_row(
                config,
                ModelRow {
                    id: "source-control-writer-model",
                    title: "Source control writer model",
                    description: "Model for source control text and branch or bookmark names. When disabled, use the text generation model configured for this environment.",
                    selection: settings.source_control_writer_model_selection.clone(),
                    seed: writer_seed,
                    allow_text_generators: true,
                    change: SourceControlChange::WriterModel,
                },
                &writer_options,
                cx,
            ),
        ];
        let toggle = cx.listener(|this, _, _, cx| {
            this.source_control.advanced = !this.source_control.advanced;
            cx.notify();
        });
        rows.push(advanced("source-control-writing-advanced", self.source_control.advanced, advanced_rows, toggle, cx));
        section("Commits & pull requests", rows, cx)
    }

    /// A "use a separate model" switch with the model select beside it while on.
    fn model_row(&self, config: &ServerConfig, row: ModelRow, options: &[SelectOption], cx: &Context<Self>) -> AnyElement {
        let ModelRow { id, title, description, selection, seed, allow_text_generators, change } = row;
        let switch_view = cx.entity().downgrade();
        let on = selection.is_some();
        let disabled_reason = selection.as_ref().and_then(|sel| {
            self.scoped_model_disabled_reason(config, &sel.instance_id, &sel.model, allow_text_generators, cx)
        });
        let toggle = Switch::new(SharedString::from(format!("{id}-switch")), on).on_change(move |on, _, cx| {
            let next = if on { seed.clone() } else { None };
            if on && next.is_none() {
                return;
            }
            let _ = switch_view.update(cx, |this, cx| {
                this.update_scoped_server(change(next).into_server(), cx);
            });
        });
        let picker = selection.filter(|_| !options.is_empty()).map(|selection| {
            let select_view = cx.entity().downgrade();
            let label = if allow_text_generators {
                self.text_gen_selection_label(config, &selection)
            } else {
                selection_label(config, &selection)
            };
            Select::new(SharedString::from(format!("{id}-select")), options.to_vec())
                .selected(selection_value(&selection))
                .label(label)
                .width(200.)
                .popup_width(260.)
                .show_check()
                .on_change(move |value, _, cx| {
                    if let Some(selection) = parse_selection(&value) {
                        let _ = select_view.update(cx, |this, cx| {
                            this.update_scoped_server(change(Some(selection)).into_server(), cx);
                        });
                    }
                })
        });
        let empty = on && options.is_empty();
        let mut row = Row::new(id, title)
            .description(description)
            .highlighted(self.is_target(id))
            .when(empty, |row| row.status("No text generation providers available."));
        if !empty && let Some(reason) = disabled_reason {
            row = row.status(reason);
        }
        row.control(h_flex().gap(r(8.)).children(picker).child(toggle)).into_any_element()
    }
}

struct ModelRow {
    id: &'static str,
    title: &'static str,
    description: &'static str,
    selection: Option<t3_core::model::ModelSelection>,
    /// What turning the switch on saves: the writer model in use.
    seed: Option<t3_core::model::ModelSelection>,
    /// Whether direct text-generation providers (configured under `textGenerators`) are allowed.
    allow_text_generators: bool,
    change: fn(Option<t3_core::model::ModelSelection>) -> SourceControlChange,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_select_is_disabled_when_read_only_or_host_disabled() {
        assert!(!account_select_disabled(true, true));
        assert!(account_select_disabled(false, true));
        assert!(account_select_disabled(true, false));
        assert!(account_select_disabled(false, false));
    }

    #[test]
    fn generic_host_patch_clears_single_saved_secret_reference_or_literal_without_wiping_siblings() {
        let def = host_definition("bitbucket").expect("bitbucket definition");
        let saved = BTreeMap::from([
            ("accessToken".to_owned(), SecretValue::Reference { provider: "1password".into(), reference: "op://Dev/Bitbucket/access".into() }),
            ("email".to_owned(), SecretValue::Literal("dev@example.com".into())),
            ("apiToken".to_owned(), SecretValue::Literal("saved-api-token".into())),
        ]);

        // Switching accessToken back to STORED_SECRET with empty input clears only accessToken.
        let raw_values = HashMap::from([
            ("accessToken".to_owned(), String::new()),
            ("email".to_owned(), "dev@example.com".to_owned()),
            ("apiToken".to_owned(), String::new()),
        ]);
        let sources = HashMap::from([
            ("accessToken".to_owned(), STORED_SECRET.to_owned()),
            ("apiToken".to_owned(), STORED_SECRET.to_owned()),
        ]);
        let patch = build_generic_host_patch(def.fields, &saved, &raw_values, &sources, &HashSet::new()).expect("valid patch");
        assert_eq!(patch.get("accessToken"), Some(&Value::String(String::new())));
        assert!(!patch.contains_key("email"));
        assert!(!patch.contains_key("apiToken"));

        // Explicitly clearing apiToken clears only apiToken while keeping accessToken's ref untouched.
        let raw_keep_ref = HashMap::from([
            ("accessToken".to_owned(), "op://Dev/Bitbucket/access".to_owned()),
            ("email".to_owned(), "dev@example.com".to_owned()),
            ("apiToken".to_owned(), String::new()),
        ]);
        let sources_keep_ref = HashMap::from([
            ("accessToken".to_owned(), "1password".to_owned()),
            ("apiToken".to_owned(), STORED_SECRET.to_owned()),
        ]);
        let cleared = HashSet::from(["apiToken".to_owned()]);
        let patch = build_generic_host_patch(def.fields, &saved, &raw_keep_ref, &sources_keep_ref, &cleared).expect("valid patch");
        assert_eq!(patch.get("apiToken"), Some(&Value::String(String::new())));
        assert!(!patch.contains_key("accessToken"));
        assert!(!patch.contains_key("email"));
    }
}
