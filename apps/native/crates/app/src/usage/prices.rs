//! Model prices dialog (`UsagePriceOverrides.tsx`, `usagePriceTable.ts`, `usagePriceTargets.ts`):
//! reads `usagePriceOverrides` and `usageModelAliases` via `server.getConfig` from every selected
//! environment, validates and diffs edits per target with `settings:write` and `usageModelAliases`
//! capability guards, writes patches independently through `server.updateSettings`, and keeps the
//! dialog open with per-environment errors + Retry on partial failure.

use std::collections::{BTreeMap, BTreeSet};

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, Subscription, Window, div, prelude::*};
use gpui_component::Sizable;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use serde_json::Value;
use t3_core::commands::method::SERVER_UPDATE_SETTINGS;
use t3_core::device::SETTINGS_WRITE_SCOPE;
use t3_core::usage::ConnectionPhase;
use t3_core::usage::price_overrides::{
    GET_CONFIG, UsagePriceChange, UsagePriceField, UsagePriceForm, UsagePriceRowDraft, UsagePriceSettings,
    decode_usage_price_settings, usage_price_patch_payload, usage_price_table_changes,
};

use super::UsageView;
use super::controls::{ActionButton, ButtonSize, ButtonVariant, r};
use crate::app_state::{AppState, uuid_v4};
use crate::i18n::{t, t_fmt};
use t3_core::i18n::IcuArg;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TargetSaveStatus {
    Saving,
    Saved,
    Failed(String),
}

#[derive(Debug, Clone)]
pub(super) struct PriceTarget {
    pub(super) saved_id: String,
    pub(super) label: String,
    pub(super) settings: Option<UsagePriceSettings>,
    pub(super) unavailable: Option<String>,
}

struct PriceRowInputs {
    id: String,
    is_new: bool,
    removed: bool,
    /// Immutable model ID for existing (`!is_new`) rows so saved server keys are never orphaned.
    fixed_model: String,
    model: Option<Entity<InputState>>,
    input: Entity<InputState>,
    output: Entity<InputState>,
    cache_read: Entity<InputState>,
    cache_write: Entity<InputState>,
    alias: Entity<InputState>,
    /// Initial value displayed per price field (`[Input, Output, CacheRead, CacheWrite]`).
    initial_fields: [String; 4],
    /// True when the cell started as `"Mixed"` or `"Unavailable"`, so leaving it blank preserves
    /// each target environment's existing value instead of overwriting it.
    keep_field_if_blank: [bool; 4],
    /// True when the alias cell started as `"Mixed"` or `"Unavailable"`.
    keep_alias_if_blank: bool,
    initial_alias: String,
    _subscriptions: Vec<Subscription>,
}

impl PriceRowInputs {
    fn field_input(&self, field: UsagePriceField) -> &Entity<InputState> {
        match field {
            UsagePriceField::Input => &self.input,
            UsagePriceField::Output => &self.output,
            UsagePriceField::CacheRead => &self.cache_read,
            UsagePriceField::CacheWrite => &self.cache_write,
        }
    }

    fn field_index(field: UsagePriceField) -> usize {
        match field {
            UsagePriceField::Input => 0,
            UsagePriceField::Output => 1,
            UsagePriceField::CacheRead => 2,
            UsagePriceField::CacheWrite => 3,
        }
    }
}

pub(super) struct PriceOverridesDialog {
    pub(super) loading: bool,
    pub(super) saving: bool,
    pub(super) error: Option<String>,
    pub(super) row_errors: BTreeMap<String, String>,
    pub(super) targets: Vec<PriceTarget>,
    pub(super) results: BTreeMap<String, TargetSaveStatus>,
    rows: Vec<PriceRowInputs>,
    next_row_id: usize,
    seed_model: Option<String>,
    selected_ids: Option<BTreeSet<String>>,
    read_id: String,
}

fn decode_target_settings(config: &Value) -> UsagePriceSettings {
    let mut settings = decode_usage_price_settings(config);
    if let Some(cap) = config.pointer("/environment/capabilities/usageModelAliases").and_then(Value::as_bool) {
        settings.aliases_supported = cap;
    }
    settings
}

fn environment_can_write(state: &AppState, active_session: Option<&t3_core::auth::AuthSessionState>, saved_id: &str) -> bool {
    if !saved_id.is_empty()
        && let Some(env) = state.fleet.environments.iter().find(|e| e.summary.owner.saved_id == saved_id)
    {
        let is_active = state.environments.active.as_deref() == Some(saved_id);
        let session = env.grants.as_ref().or_else(|| is_active.then_some(active_session).flatten());
        return session.is_some_and(|session| session.grants(SETTINGS_WRITE_SCOPE));
    }
    if saved_id.is_empty() || state.environments.active.as_deref() == Some(saved_id) {
        return active_session.is_some_and(|session| session.grants(SETTINGS_WRITE_SCOPE));
    }
    false
}

fn environment_connected(state: &AppState, saved_id: &str) -> bool {
    if !saved_id.is_empty()
        && let Some(env) = state.fleet.environments.iter().find(|e| e.summary.owner.saved_id == saved_id)
    {
        return env.status == t3_client::supervisor::ConnectionStatus::Connected;
    }
    if saved_id.is_empty() || state.environments.active.as_deref() == Some(saved_id) {
        return matches!(state.phase, crate::app_state::Phase::Online(_));
    }
    false
}

fn cell_for_price_field(targets: &[PriceTarget], model: &str, field: UsagePriceField) -> (String, &'static str, bool) {
    if targets.iter().any(|t| t.settings.is_none()) {
        return (String::new(), "Unavailable", true);
    }
    let values: Vec<Option<String>> = targets
        .iter()
        .map(|t| {
            t.settings
                .as_ref()
                .and_then(|s| s.prices.get(model))
                .map(|override_price| UsagePriceForm::from_override(model, Some(override_price)).field(field).to_owned())
        })
        .collect();
    let Some(first) = values.first() else {
        return (String::new(), if field.optional() { "Input rate" } else { "0.00" }, false);
    };
    if values.iter().any(|v| v != first) {
        return (String::new(), "Mixed", true);
    }
    match first {
        Some(value) => (value.clone(), if field.optional() { "Input rate" } else { "0.00" }, false),
        None => (String::new(), "Automatic", false),
    }
}

fn cell_for_alias(targets: &[PriceTarget], model: &str) -> (String, &'static str, bool) {
    if targets.iter().any(|t| t.settings.as_ref().is_none_or(|s| !s.aliases_supported)) {
        return (String::new(), "Unavailable", true);
    }
    let values: Vec<&str> = targets
        .iter()
        .map(|t| t.settings.as_ref().and_then(|s| s.aliases.get(model)).map(String::as_str).unwrap_or(""))
        .collect();
    let Some(first) = values.first() else {
        return (String::new(), "None", false);
    };
    if values.iter().any(|v| v != first) {
        return (String::new(), "Mixed", true);
    }
    ((*first).to_owned(), "None", false)
}

impl PriceOverridesDialog {
    pub(super) fn is_saving(&self) -> bool {
        self.saving
    }

    pub(super) fn has_failed_destinations(&self) -> bool {
        self.results.values().any(|status| matches!(status, TargetSaveStatus::Failed(_)))
    }

    fn new(seed_model: Option<String>, targets: Vec<PriceTarget>, selected_ids: Option<BTreeSet<String>>) -> Self {
        Self {
            loading: true,
            saving: false,
            error: None,
            row_errors: BTreeMap::new(),
            targets,
            results: BTreeMap::new(),
            rows: Vec::new(),
            next_row_id: 1,
            seed_model,
            selected_ids,
            read_id: uuid_v4(),
        }
    }

    fn has_edits(&self, cx: &gpui::App) -> bool {
        self.rows.iter().any(|row| {
            row.removed
                || row.model.as_ref().is_some_and(|model| {
                    let model = model.read(cx).value();
                    !model.trim().is_empty() && self.seed_model.as_deref() != Some(model.as_ref())
                })
                || row.alias.read(cx).value().as_ref() != row.initial_alias
                || UsagePriceField::ALL.iter().any(|field| {
                    row.field_input(*field).read(cx).value().as_ref() != row.initial_fields[PriceRowInputs::field_index(*field)]
                })
        })
    }

    fn push_existing_row(&mut self, model: &str, window: &mut Window, cx: &mut Context<UsageView>) {
        let id = format!("price-row-{}", self.next_row_id);
        self.next_row_id += 1;
        let (in_val, in_ph, in_keep) = cell_for_price_field(&self.targets, model, UsagePriceField::Input);
        let (out_val, out_ph, out_keep) = cell_for_price_field(&self.targets, model, UsagePriceField::Output);
        let (cr_val, cr_ph, cr_keep) = cell_for_price_field(&self.targets, model, UsagePriceField::CacheRead);
        let (cw_val, cw_ph, cw_keep) = cell_for_price_field(&self.targets, model, UsagePriceField::CacheWrite);
        let (alias_val, alias_ph, alias_keep) = cell_for_alias(&self.targets, model);
        let mk = |value: &str, placeholder: &'static str, window: &mut Window, cx: &mut Context<UsageView>| {
            let value = value.to_owned();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                if !value.is_empty() {
                    state.set_value(value, window, cx);
                }
                state
            })
        };
        let mut subscriptions = Vec::new();
        let input = mk(&in_val, in_ph, window, cx);
        let output = mk(&out_val, out_ph, window, cx);
        let cache_read = mk(&cr_val, cr_ph, window, cx);
        let cache_write = mk(&cw_val, cw_ph, window, cx);
        let alias = mk(&alias_val, alias_ph, window, cx);
        for entity in [&input, &output, &cache_read, &cache_write, &alias] {
            let row_id = id.clone();
            subscriptions.push(cx.subscribe(entity, move |view, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(dialog) = view.prices.as_mut()
                {
                    dialog.results.clear();
                    dialog.row_errors.remove(&row_id);
                    cx.notify();
                }
            }));
        }
        self.rows.push(PriceRowInputs {
            id,
            is_new: false,
            removed: false,
            fixed_model: model.to_owned(),
            model: None,
            input,
            output,
            cache_read,
            cache_write,
            alias,
            initial_fields: [in_val, out_val, cr_val, cw_val],
            keep_field_if_blank: [in_keep, out_keep, cr_keep, cw_keep],
            keep_alias_if_blank: alias_keep,
            initial_alias: alias_val,
            _subscriptions: subscriptions,
        });
    }

    fn push_new_row(&mut self, model: &str, window: &mut Window, cx: &mut Context<UsageView>) {
        let id = format!("price-row-{}", self.next_row_id);
        self.next_row_id += 1;
        let mk = |value: &str, placeholder: &'static str, window: &mut Window, cx: &mut Context<UsageView>| {
            let value = value.to_owned();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                if !value.is_empty() {
                    state.set_value(value, window, cx);
                }
                state
            })
        };
        let mut subscriptions = Vec::new();
        let model_input = mk(model, "model-id", window, cx);
        let input = mk("", "0.00", window, cx);
        let output = mk("", "0.00", window, cx);
        let cache_read = mk("", "Input rate", window, cx);
        let cache_write = mk("", "Input rate", window, cx);
        let alias = mk("", "None", window, cx);
        for entity in [&model_input, &input, &output, &cache_read, &cache_write, &alias] {
            let row_id = id.clone();
            subscriptions.push(cx.subscribe(entity, move |view, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(dialog) = view.prices.as_mut()
                {
                    dialog.results.clear();
                    dialog.row_errors.remove(&row_id);
                    cx.notify();
                }
            }));
        }
        self.rows.push(PriceRowInputs {
            id,
            is_new: true,
            removed: false,
            fixed_model: model.to_owned(),
            model: Some(model_input),
            input,
            output,
            cache_read,
            cache_write,
            alias,
            initial_fields: Default::default(),
            keep_field_if_blank: [false; 4],
            keep_alias_if_blank: false,
            initial_alias: String::new(),
            _subscriptions: subscriptions,
        });
    }

    /// Builds per-target row drafts preserving each target's saved values for untouched
    /// `"Mixed"`/`"Unavailable"` cells, and returns any cross-target alias conflict errors.
    fn drafts_for_target(&self, target: &PriceTarget, cx: &gpui::App) -> (Vec<UsagePriceRowDraft>, BTreeMap<String, String>) {
        let mut drafts = Vec::with_capacity(self.rows.len());
        let mut errors = BTreeMap::new();
        let Some(settings) = target.settings.as_ref() else {
            return (drafts, errors);
        };
        for row in &self.rows {
            let model = match &row.model {
                Some(input) if row.is_new => input.read(cx).value().to_string(),
                _ => row.fixed_model.clone(),
            };
            let trimmed_model = model.trim();
            if row.is_new {
                drafts.push(UsagePriceRowDraft {
                    id: row.id.clone(),
                    model: model.clone(),
                    is_new: true,
                    form: UsagePriceForm {
                        model,
                        input: row.input.read(cx).value().to_string(),
                        output: row.output.read(cx).value().to_string(),
                        cache_read: row.cache_read.read(cx).value().to_string(),
                        cache_write: row.cache_write.read(cx).value().to_string(),
                    },
                    alias: row.alias.read(cx).value().to_string(),
                    removed: row.removed,
                });
                continue;
            }

            let target_original_form = UsagePriceForm::from_override(trimmed_model, settings.prices.get(trimmed_model));
            let target_original_alias = settings.aliases.get(trimmed_model).cloned().unwrap_or_default();
            let mut form = UsagePriceForm { model: model.clone(), ..Default::default() };
            let mut any_price_edited = false;
            for field in UsagePriceField::ALL {
                let idx = PriceRowInputs::field_index(field);
                let typed = row.field_input(field).read(cx).value().to_string();
                let keep_original = row.keep_field_if_blank[idx] && typed.is_empty();
                let resolved = if keep_original {
                    target_original_form.field(field).to_owned()
                } else {
                    if row.keep_field_if_blank[idx] || typed != row.initial_fields[idx] {
                        any_price_edited = true;
                    }
                    typed
                };
                form.set_field(field, resolved);
            }
            let typed_alias = row.alias.read(cx).value().to_string();
            let alias_edited = if row.keep_alias_if_blank {
                !typed_alias.is_empty()
            } else {
                typed_alias != target_original_alias
            };
            let alias = if row.keep_alias_if_blank && typed_alias.is_empty() {
                if !row.removed && any_price_edited && !target_original_alias.is_empty() {
                    errors.insert(
                        row.id.clone(),
                        format!("{} maps this model. Clear its mapping first.", target.label),
                    );
                }
                target_original_alias
            } else {
                typed_alias
            };

            // If this existing model is neither present on this target nor edited/removed by the
            // user, skip emitting a draft for this target so we don't require missing fields on
            // environments that never configured this model.
            let target_has_model = settings.prices.contains_key(trimmed_model) || settings.aliases.contains_key(trimmed_model);
            if !row.removed && !target_has_model && !any_price_edited && !alias_edited {
                continue;
            }

            drafts.push(UsagePriceRowDraft {
                id: row.id.clone(),
                model,
                is_new: false,
                form,
                alias,
                removed: row.removed,
            });
        }
        (drafts, errors)
    }
}

impl UsageView {
    pub(super) fn open_price_overrides(&mut self, seed_model: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.load_price_overrides(seed_model, self.data.selected_ids.clone(), window, cx);
    }

    fn load_price_overrides(
        &mut self,
        seed_model: Option<String>,
        selected_ids: Option<BTreeSet<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial_targets: Vec<(String, String, bool)> = if self.data.envs.is_empty() {
            vec![(String::new(), self.data.primary_label(), true)]
        } else {
            self.data.envs.iter()
                .filter(|env| selected_ids.as_ref().is_none_or(|ids| ids.contains(&env.saved_id)))
                .map(|env| (env.saved_id.clone(), env.label.clone(), env.phase == ConnectionPhase::Connected))
                .collect()
        };
        let state = self.state.read(cx);
        let runtime = state.runtime().clone();
        let dialog_targets: Vec<PriceTarget> = initial_targets
            .iter()
            .map(|(saved_id, label, connected)| PriceTarget {
                saved_id: saved_id.clone(),
                label: label.clone(),
                settings: None,
                unavailable: (!connected).then(|| "Offline".to_owned()),
            })
            .collect();
        let dialog = PriceOverridesDialog::new(seed_model, dialog_targets, selected_ids);
        let read_id = dialog.read_id.clone();
        self.prices = Some(dialog);

        let active_session = self.data.active_session.as_ref();
        let jobs: Vec<_> = initial_targets
            .into_iter()
            .map(|(saved_id, label, connected)| {
                let can_write = environment_can_write(state, active_session, &saved_id);
                let client_fut =
                    (connected && !saved_id.is_empty()).then(|| super::data::client_future(state, &saved_id));
                let fallback_rpc = (connected && saved_id.is_empty()).then(|| state.rpc(GET_CONFIG, serde_json::json!({})));
                (saved_id, label, connected, can_write, client_fut, fallback_rpc)
            })
            .collect();

        cx.spawn_in(window, async move |this, cx| {
            let loaded: Vec<PriceTarget> = futures::future::join_all(jobs.into_iter().map(
                |(saved_id, label, connected, can_write, client_fut, fallback_rpc)| {
                    let runtime = runtime.clone();
                    async move {
                        if !connected {
                            return PriceTarget { saved_id, label, settings: None, unavailable: Some("Offline".to_owned()) };
                        }
                        let reply = if let Some(client_fut) = client_fut {
                            match client_fut.await {
                                Some(client) => runtime
                                    .spawn(async move { client.request(GET_CONFIG, serde_json::json!({})).await })
                                    .await
                                    .unwrap_or(Err(t3_client::session::RpcError::Closed)),
                                None => Err(t3_client::session::RpcError::Closed),
                            }
                        } else if let Some(rpc) = fallback_rpc {
                            rpc.await
                        } else {
                            Err(t3_client::session::RpcError::Closed)
                        };
                        match reply {
                            Ok(config) => PriceTarget {
                                saved_id,
                                label,
                                settings: Some(decode_target_settings(&config)),
                                unavailable: (!can_write).then(|| "Read-only access".to_owned()),
                            },
                            Err(_) => PriceTarget {
                                saved_id,
                                label,
                                settings: None,
                                unavailable: Some("Prices not loaded".to_owned()),
                            },
                        }
                    }
                },
            ))
            .await;

            let _ = this.update_in(cx, |view, window, cx| {
                if view.prices.as_ref().is_none_or(|dialog| dialog.read_id != read_id) {
                    return;
                }
                let Some(mut dialog) = view.prices.take() else { return };
                dialog.loading = false;
                dialog.targets = loaded;
                let any_loaded = dialog.targets.iter().any(|t| t.settings.is_some());
                if !any_loaded {
                    dialog.error = Some("Could not load model prices.".to_owned());
                } else {
                    let mut models = BTreeSet::new();
                    for target in &dialog.targets {
                        if let Some(settings) = target.settings.as_ref() {
                            models.extend(settings.prices.keys().cloned());
                            models.extend(settings.aliases.keys().cloned());
                        }
                    }
                    for model in &models {
                        dialog.push_existing_row(model, window, cx);
                    }
                    if let Some(seed) = dialog.seed_model.clone()
                        && !models.contains(&seed)
                    {
                        dialog.push_new_row(&seed, window, cx);
                    }
                    if dialog.rows.is_empty() {
                        dialog.push_new_row("", window, cx);
                    }
                }
                view.prices = Some(dialog);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn add_price_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prices.as_ref().is_none_or(|d| d.saving) {
            return;
        }
        let Some(mut dialog) = self.prices.take() else { return };
        dialog.results.clear();
        dialog.push_new_row("", window, cx);
        self.prices = Some(dialog);
        cx.notify();
    }

    pub(super) fn save_price_overrides(&mut self, retry: bool, cx: &mut Context<Self>) {
        struct PlannedTarget {
            saved_id: String,
            unavailable: Option<String>,
            changes: Vec<UsagePriceChange>,
        }

        let (planned, row_errors) = {
            let Some(dialog) = self.prices.as_ref() else { return };
            if dialog.saving || dialog.loading {
                return;
            }
            let state = self.state.read(cx);
            let destinations: Vec<&PriceTarget> = if retry {
                dialog
                    .targets
                    .iter()
                    .filter(|t| matches!(dialog.results.get(&t.saved_id), Some(TargetSaveStatus::Failed(_))))
                    .collect()
            } else {
                dialog.targets.iter().collect()
            };
            if destinations.is_empty() {
                return;
            }

            let mut all_row_errors = BTreeMap::new();
            let mut planned = Vec::with_capacity(destinations.len());
            let active_session = self.data.active_session.as_ref();
            for target in destinations {
                let connected = environment_connected(state, &target.saved_id);
                let can_write = environment_can_write(state, active_session, &target.saved_id);
                let base_unavailable = if !connected {
                    Some("Offline".to_owned())
                } else if target.settings.is_none() {
                    Some("Prices not loaded".to_owned())
                } else if !can_write {
                    Some("Read-only access".to_owned())
                } else {
                    None
                };

                let Some(settings) = target.settings.as_ref() else {
                    planned.push(PlannedTarget {
                        saved_id: target.saved_id.clone(),
                        unavailable: base_unavailable.or_else(|| Some("Prices not loaded".to_owned())),
                        changes: Vec::new(),
                    });
                    continue;
                };

                let (drafts, conflict_errors) = dialog.drafts_for_target(target, cx);
                let (changes, target_errors) = usage_price_table_changes(&target.label, settings, &drafts);
                for (row_id, msg) in conflict_errors {
                    all_row_errors.entry(row_id).or_insert(msg);
                }
                for (row_id, msg) in target_errors {
                    all_row_errors.entry(row_id).or_insert(msg);
                }
                let has_alias_change = changes.iter().any(|c| matches!(c, UsagePriceChange::Alias { .. }));
                let unavailable = base_unavailable.or_else(|| {
                    (has_alias_change && !settings.aliases_supported).then(|| "Update server to map models".to_owned())
                });
                planned.push(PlannedTarget { saved_id: target.saved_id.clone(), unavailable, changes });
            }
            (planned, all_row_errors)
        };

        if !row_errors.is_empty() {
            if let Some(dialog) = self.prices.as_mut() {
                dialog.row_errors = row_errors;
            }
            cx.notify();
            return;
        }

        // If nothing needs writing and nothing is unavailable, close immediately.
        if planned.iter().all(|p| p.unavailable.is_none() && p.changes.is_empty()) {
            self.prices = None;
            cx.notify();
            return;
        }

        if let Some(dialog) = self.prices.as_mut() {
            dialog.saving = true;
            dialog.error = None;
            dialog.row_errors.clear();
            if !retry {
                dialog.results.clear();
            }
            for item in &planned {
                dialog.results.insert(item.saved_id.clone(), TargetSaveStatus::Saving);
            }
        }

        let state = self.state.read(cx);
        let runtime = state.runtime().clone();
        let jobs: Vec<_> = planned
            .into_iter()
            .map(|item| {
                let payload = item.unavailable.is_none().then(|| usage_price_patch_payload(&item.changes)).flatten();
                let client_fut = (payload.is_some() && !item.saved_id.is_empty())
                    .then(|| super::data::client_future(state, &item.saved_id));
                let fallback_rpc = (payload.is_some() && item.saved_id.is_empty())
                    .then(|| state.rpc(SERVER_UPDATE_SETTINGS, payload.clone().unwrap()));
                (item.saved_id, item.unavailable, payload, client_fut, fallback_rpc)
            })
            .collect();

        cx.spawn(async move |this, cx| {
            let outcomes: Vec<(String, TargetSaveStatus)> = futures::future::join_all(jobs.into_iter().map(
                |(saved_id, unavailable, payload, client_fut, fallback_rpc)| {
                    let runtime = runtime.clone();
                    async move {
                        if let Some(reason) = unavailable {
                            return (saved_id, TargetSaveStatus::Failed(reason));
                        }
                        let Some(payload) = payload else {
                            return (saved_id, TargetSaveStatus::Saved);
                        };
                        let result = if let Some(client_fut) = client_fut {
                            match client_fut.await {
                                Some(client) => runtime
                                    .spawn(async move { client.request(SERVER_UPDATE_SETTINGS, payload).await })
                                    .await
                                    .unwrap_or(Err(t3_client::session::RpcError::Closed)),
                                None => Err(t3_client::session::RpcError::Closed),
                            }
                        } else if let Some(rpc) = fallback_rpc {
                            rpc.await
                        } else {
                            Err(t3_client::session::RpcError::Closed)
                        };
                        match result {
                            Ok(_) => (saved_id, TargetSaveStatus::Saved),
                            Err(error) => (saved_id, TargetSaveStatus::Failed(error.to_string())),
                        }
                    }
                },
            ))
            .await;

            let _ = this.update(cx, |view, cx| {
                let Some(dialog) = view.prices.as_mut() else { return };
                for (saved_id, status) in outcomes {
                    dialog.results.insert(saved_id, status);
                }
                let any_saved = dialog.results.values().any(|s| matches!(s, TargetSaveStatus::Saved));
                let any_failed = dialog.has_failed_destinations();
                if !any_failed {
                    view.prices = None;
                } else {
                    dialog.saving = false;
                }
                if any_saved {
                    view.load(true, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn price_scope_picker(&self, dialog: &PriceOverridesDialog, cx: &Context<Self>) -> Option<AnyElement> {
        if self.data.envs.len() < 2 {
            return None;
        }
        let selected = dialog.selected_ids.clone();
        let summary = if selected.is_none() {
            t(cx, "All environments").to_owned()
        } else if let [target] = dialog.targets.as_slice() {
            target.label.clone()
        } else {
            t_fmt(cx, "{count, plural, one {{count} environment} other {{count} environments}}",
                &[("count", IcuArg::Int(dialog.targets.len() as i64))])
        };
        let environments: Vec<_> = self.data.envs.iter().map(|env| (env.saved_id.clone(), env.label.clone())).collect();
        let view = cx.weak_entity();
        let seed = dialog.seed_model.clone();
        let disabled = dialog.loading || dialog.saving || dialog.has_failed_destinations() || dialog.has_edits(cx);
        use crate::pull_requests::controls::{Button as ScopeButton, Size as ScopeSize, Variant as ScopeVariant};
        let trigger = ScopeButton::new("usage-price-scope", ScopeVariant::Outline, ScopeSize::Sm)
            .label(summary).icon(Icon::ChevronDown).disabled(disabled)
            .dropdown_menu(move |menu, _, cx| {
                let all_view = view.clone();
                let all_seed = seed.clone();
                let mut menu = menu.item(PopupMenuItem::new(t(cx, "All environments"))
                    .checked(selected.is_none())
                    .on_click(move |_, window, cx| {
                        let _ = all_view.update(cx, |view, cx| view.load_price_overrides(all_seed.clone(), None, window, cx));
                    })).separator();
                let picked: BTreeSet<_> = selected.clone().unwrap_or_else(|| environments.iter().map(|(id, _)| id.clone()).collect());
                for (id, label) in &environments {
                    let chosen = picked.contains(id);
                    let mut next = picked.clone();
                    if chosen {
                        next.remove(id);
                    } else {
                        next.insert(id.clone());
                    }
                    let count = environments.len();
                    let view = view.clone();
                    let seed = seed.clone();
                    menu = menu.item(PopupMenuItem::new(label.clone()).checked(chosen).disabled(next.is_empty())
                        .on_click(move |_, window, cx| {
                            let scope = (next.len() != count).then(|| next.clone());
                            let _ = view.update(cx, |view, cx| view.load_price_overrides(seed.clone(), scope, window, cx));
                        }));
                }
                menu
            });
        Some(h_flex().gap(r(8.)).child(div().text_size(r(12.)).child(t(cx, "Apply to"))).child(trigger).into_any_element())
    }

    pub(super) fn render_price_overrides_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.prices.as_ref()?;
        let scope = self.price_scope_picker(dialog, cx);
        let p = palette(cx);
        let loading = dialog.loading;
        let saving = dialog.saving;
        let has_failed = dialog.has_failed_destinations();
        let error = dialog.error.clone();
        let target_statuses: Vec<(String, TargetSaveStatus)> = if !dialog.results.is_empty() {
            dialog
                .targets
                .iter()
                .filter_map(|target| dialog.results.get(&target.saved_id).map(|status| (target.label.clone(), status.clone())))
                .collect()
        } else {
            dialog
                .targets
                .iter()
                .filter_map(|target| target.unavailable.clone().map(|reason| (target.label.clone(), TargetSaveStatus::Failed(reason))))
                .collect()
        };
        let row_views: Vec<AnyElement> = dialog
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let row_err = dialog.row_errors.get(&row.id).cloned();
                if row.removed {
                    let model = row.model.as_ref().map_or_else(|| row.fixed_model.clone(), |input| input.read(cx).value().to_string());
                    return h_flex().gap(r(6.)).min_h(r(28.)).text_size(r(12.)).text_color(hsla(p.muted_foreground))
                        .child(div().flex_1().truncate().child(model))
                        .child(ActionButton::new(("usage-price-undo", index), crate::i18n::t(cx, "Undo"), ButtonVariant::Outline, ButtonSize::Xs)
                            .disabled(saving)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if let Some(dialog) = view.prices.as_mut()
                                    && !dialog.saving
                                    && let Some(row) = dialog.rows.get_mut(index)
                                {
                                    row.removed = false;
                                    dialog.results.clear();
                                    cx.notify();
                                }
                            })))
                        .into_any_element();
                }
                let mapped = !row.alias.read(cx).value().trim().is_empty();
                v_flex()
                    .gap(r(4.))
                    .child(
                        h_flex()
                            .gap(r(6.))
                            .items_center()
                            .child(match &row.model {
                                Some(input) if row.is_new => {
                                    div().flex_1().min_w(r(140.)).child(Input::new(input).small()).into_any_element()
                                }
                                _ => div()
                                    .flex_1()
                                    .min_w(r(140.))
                                    .px(r(8.))
                                    .py(r(5.))
                                    .rounded(r(6.))
                                    .bg(hsla(p.muted).opacity(0.45))
                                    .text_size(r(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.foreground))
                                    .truncate()
                                    .child(row.fixed_model.clone())
                                    .into_any_element(),
                            })
                            .when(!mapped, |this| this
                                .child(div().w(r(76.)).child(Input::new(&row.input).small()))
                                .child(div().w(r(76.)).child(Input::new(&row.output).small()))
                                .child(div().w(r(84.)).child(Input::new(&row.cache_read).small()))
                                .child(div().w(r(84.)).child(Input::new(&row.cache_write).small())))
                            .child(div().w(r(if mapped { 460. } else { 116. })).child(Input::new(&row.alias).small()))
                            .child(
                                Button::new(SharedString::from(format!("usage-price-remove-{index}")))
                                    .variant(Variant::Ghost)
                                    .size(Size::IconSm)
                                    .icon(Icon::X)
                                    .disabled(saving)
                                    .tooltip("Remove override")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(d) = this.prices.as_mut()
                                            && !d.saving
                                            && let Some(r) = d.rows.get_mut(index)
                                        {
                                            r.removed = true;
                                            d.row_errors.remove(&r.id);
                                            d.results.clear();
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when_some(row_err, |this, msg| {
                        this.child(div().text_size(r(11.)).text_color(hsla(p.destructive)).child(msg))
                    })
                    .into_any_element()
            })
            .collect();

        let status_rows: Vec<AnyElement> = target_statuses
            .into_iter()
            .map(|(label, status)| {
                let (detail, color) = match status {
                    TargetSaveStatus::Saving => ("Saving…".to_owned(), hsla(p.muted_foreground)),
                    TargetSaveStatus::Saved => ("Saved".to_owned(), hsla(p.success_foreground)),
                    TargetSaveStatus::Failed(err) => (err, hsla(p.destructive)),
                };
                h_flex()
                    .justify_between()
                    .gap(r(8.))
                    .text_size(r(12.))
                    .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(label))
                    .child(div().text_color(color).child(detail))
                    .into_any_element()
            })
            .collect();

        let card = v_flex()
            .id("usage-prices-dialog")
            .relative()
            .w_full()
            .max_w(r(860.))
            .max_h_full()
            .rounded(r(16.))
            .overflow_hidden()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .shadow(vec![gpui::BoxShadow {
                color: gpui::black().opacity(0.65),
                offset: gpui::point(gpui::px(0.), gpui::px(24.)),
                blur_radius: gpui::px(64.),
                spread_radius: gpui::px(-24.),
                inset: false,
            }])
            .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
            .child(
                v_flex()
                    .relative()
                    .gap(r(4.))
                    .p(r(20.))
                    .border_b_1()
                    .border_color(hsla(p.border))
                    .child(div().text_size(r(18.)).font_weight(FontWeight::SEMIBOLD).child("Model prices"))
                    .child(
                        div()
                            .text_size(r(12.))
                            .text_color(hsla(p.muted_foreground))
                            .child("Override USD per 1M tokens or map a model ID to another model's rates. Blank cache fields inherit Input."),
                    ),
            )
            .child(
                v_flex()
                    .id("usage-prices-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(r(10.))
                    .p(r(20.))
                    .children(scope)
                    .when_some(error, |this, msg| {
                        this.child(div().text_size(r(12.)).text_color(hsla(p.destructive)).child(msg))
                    })
                    .when(!status_rows.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap(r(4.))
                                .p(r(10.))
                                .rounded(r(8.))
                                .border_1()
                                .border_color(hsla(p.border))
                                .bg(hsla(p.muted).opacity(0.35))
                                .children(status_rows),
                        )
                    })
                    .when(loading, |this| {
                        this.child(div().text_size(r(13.)).text_color(hsla(p.muted_foreground)).child("Loading model prices…"))
                    })
                    .when(!loading, |this| {
                        this.child(
                            h_flex()
                                .gap(r(6.))
                                .text_size(r(11.))
                                .text_color(hsla(p.muted_foreground))
                                .child(div().flex_1().min_w(r(140.)).child("Model"))
                                .child(div().w(r(76.)).child("Input"))
                                .child(div().w(r(76.)).child("Output"))
                                .child(div().w(r(84.)).child("Cache read"))
                                .child(div().w(r(84.)).child("Cache write"))
                                .child(div().w(r(116.)).child("Map to"))
                                .child(div().w(r(28.))),
                        )
                        .children(row_views)
                        .child(
                            h_flex().pt(r(4.)).child(
                                ActionButton::new("usage-prices-add", "Add model", ButtonVariant::Outline, ButtonSize::Xs)
                                    .disabled(saving)
                                    .on_click(cx.listener(|this, _, window, cx| this.add_price_row(window, cx))),
                            ),
                        )
                    }),
            )
            .child(
                h_flex()
                    .relative()
                    .justify_end()
                    .gap(r(8.))
                    .px(r(20.))
                    .py(r(14.))
                    .border_t_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.muted))
                    .child(
                        ActionButton::new("usage-prices-cancel", if has_failed { "Close" } else { "Cancel" }, ButtonVariant::Outline, ButtonSize::Default)
                            .disabled(saving)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.prices.as_ref().is_some_and(|d| d.saving) {
                                    return;
                                }
                                this.prices = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        ActionButton::new(
                            "usage-prices-save",
                            if saving {
                                "Saving…"
                            } else if has_failed {
                                "Retry failed"
                            } else {
                                "Save"
                            },
                            ButtonVariant::Default,
                            ButtonSize::Default,
                        )
                        .disabled(loading || saving)
                        .on_click(cx.listener(move |this, _, _, cx| this.save_price_overrides(has_failed, cx))),
                    ),
            );

        Some(
            div()
                .id("usage-prices-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(r(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        if this.prices.as_ref().is_some_and(|d| d.saving) {
                            return;
                        }
                        this.prices = None;
                        cx.notify();
                    }),
                )
                .child(card)
                .into_any_element(),
        )
    }
}

#[cfg(all(test, feature = "snapshot"))]
impl PriceOverridesDialog {
    pub(super) fn row_models_for_test(&self, cx: &gpui::App) -> Vec<String> {
        self.rows
            .iter()
            .filter(|r| !r.removed)
            .map(|r| match &r.model {
                Some(input) if r.is_new => input.read(cx).value().to_string(),
                _ => r.fixed_model.clone(),
            })
            .collect()
    }

    pub(super) fn set_row_alias_for_test(&self, model: &str, alias: &str, window: &mut Window, cx: &mut gpui::App) {
        let row = self.rows.iter().find(|r| r.fixed_model == model).expect("row for model");
        row.alias.update(cx, |input, cx| input.set_value(alias.to_owned(), window, cx));
    }

    pub(super) fn set_row_price_for_test(&self, model: &str, input_rate: &str, output_rate: &str, window: &mut Window, cx: &mut gpui::App) {
        let row = self.rows.iter().find(|r| r.fixed_model == model).expect("row for model");
        row.input.update(cx, |input, cx| input.set_value(input_rate.to_owned(), window, cx));
        row.output.update(cx, |input, cx| input.set_value(output_rate.to_owned(), window, cx));
    }
}
