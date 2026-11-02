//! Model traits: reasoning effort, speed, and the other options a model's descriptors offer.
//! Ports option descriptors and selections from `shared/src/model.ts`, reasoning detection and
//! the trigger label from `client-runtime/src/modelTraits.ts`, and the effort column split from
//! `web/src/components/chat/ComposerModelEffort.tsx`.

use serde_json::{Map, Value, json};

use crate::model::ModelSelection;
use crate::server_config::{BooleanDescriptor, OptionChoice, OptionDescriptor, ProviderModel, SelectDescriptor};

/// `REASONING_EFFORT_OPTION_IDS` plus `thinking`.
const REASONING_IDS: [&str; 7] =
    ["reasoningEffort", "effort", "reasoning", "variant", "reasoning_effort", "thinking_level", "thinking"];

#[derive(Debug, Clone, PartialEq)]
pub enum OptionValue {
    Str(String),
    Bool(bool),
}

impl OptionValue {
    fn to_json(&self) -> Value {
        match self {
            OptionValue::Str(value) => json!(value),
            OptionValue::Bool(value) => json!(value),
        }
    }

    fn from_json(value: &Value) -> Option<OptionValue> {
        match value {
            Value::String(text) => Some(text.trim()).filter(|text| !text.is_empty()).map(|text| OptionValue::Str(text.to_owned())),
            Value::Bool(flag) => Some(OptionValue::Bool(*flag)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OptionSelection {
    pub id: String,
    pub value: OptionValue,
}

/// `ProviderOptionSelections`: the canonical `[{id, value}]` array, or the legacy `{id: value}`.
pub fn selections(options: Option<&Value>) -> Vec<OptionSelection> {
    let pair = |id: &str, value: &Value| {
        let id = id.trim();
        let value = OptionValue::from_json(value)?;
        (!id.is_empty()).then(|| OptionSelection { id: id.to_owned(), value })
    };
    match options {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| pair(item.get("id")?.as_str()?, item.get("value")?))
            .collect(),
        Some(Value::Object(entries)) => entries.iter().filter_map(|(id, value)| pair(id, value)).collect(),
        _ => Vec::new(),
    }
}

/// `createModelSelection`: options only when there are some.
pub fn model_selection(instance_id: &str, model: &str, selections: &[OptionSelection]) -> ModelSelection {
    let options = (!selections.is_empty()).then(|| {
        Value::Array(
            selections
                .iter()
                .map(|selection| {
                    let mut entry = Map::new();
                    entry.insert("id".into(), json!(selection.id));
                    entry.insert("value".into(), selection.value.to_json());
                    Value::Object(entry)
                })
                .collect(),
        )
    });
    ModelSelection { instance_id: instance_id.to_owned(), model: model.to_owned(), options }
}

fn raw_value<'a>(selections: &'a [OptionSelection], id: &str) -> Option<&'a OptionValue> {
    selections.iter().find(|selection| selection.id == id).map(|selection| &selection.value)
}

fn default_choice(select: &SelectDescriptor) -> Option<String> {
    select.options.iter().find(|option| option.is_default).map(|option| option.id.clone())
}

/// `resolveDescriptorChoiceValue`.
fn resolve_choice(select: &SelectDescriptor, raw: Option<&str>) -> Option<String> {
    let fallback = || select.current_value.clone().or_else(|| default_choice(select));
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else { return fallback() };
    if select.options.is_empty() {
        return Some(raw.to_owned());
    }
    let offered = select.options.iter().any(|option| option.id == raw);
    if offered && select.prompt_injected_values.iter().any(|value| value == raw) {
        return default_choice(select);
    }
    if offered { Some(raw.to_owned()) } else { fallback() }
}

/// `getProviderOptionDescriptors`: the model's descriptors with current values from `selections`.
pub fn resolve_descriptors(descriptors: &[OptionDescriptor], selections: &[OptionSelection]) -> Vec<OptionDescriptor> {
    descriptors
        .iter()
        .map(|descriptor| match descriptor {
            OptionDescriptor::Boolean(boolean) => {
                let mut boolean = boolean.clone();
                if let Some(OptionValue::Bool(value)) = raw_value(selections, &boolean.id) {
                    boolean.current_value = Some(*value);
                }
                OptionDescriptor::Boolean(boolean)
            }
            OptionDescriptor::Select(select) => {
                let raw = match raw_value(selections, &select.id) {
                    Some(OptionValue::Str(value)) => Some(value.as_str()),
                    _ => select.current_value.as_deref(),
                };
                let mut resolved = select.clone();
                resolved.current_value = resolve_choice(select, raw);
                OptionDescriptor::Select(resolved)
            }
        })
        .collect()
}

/// `getProviderOptionCurrentValue` without a reported selection.
pub fn current_value(descriptor: &OptionDescriptor) -> Option<OptionValue> {
    match descriptor {
        OptionDescriptor::Boolean(boolean) => boolean.current_value.map(OptionValue::Bool),
        OptionDescriptor::Select(select) => select.current_value.clone().or_else(|| default_choice(select)).map(OptionValue::Str),
    }
}

pub fn select_value(select: &SelectDescriptor) -> Option<String> {
    select.current_value.clone().or_else(|| default_choice(select))
}

/// `getProviderOptionCurrentLabel`.
pub fn current_label(descriptor: &OptionDescriptor) -> Option<String> {
    match descriptor {
        OptionDescriptor::Boolean(boolean) => boolean.current_value.map(|on| if on { "On" } else { "Off" }.to_owned()),
        OptionDescriptor::Select(select) => {
            let value = select_value(select)?;
            select.options.iter().find(|option| option.id == value).map(|option| option.label.clone())
        }
    }
}

/// `buildProviderOptionSelectionsFromDescriptors`.
pub fn build_selections(descriptors: &[OptionDescriptor]) -> Vec<OptionSelection> {
    descriptors
        .iter()
        .filter_map(|descriptor| Some(OptionSelection { id: descriptor.id().to_owned(), value: current_value(descriptor)? }))
        .collect()
}

/// `buildExplicitProviderOptionSelectionsFromDescriptors`: what a send carries, which is only the
/// options the user chose, each normalized against the model's descriptors.
pub fn explicit_selections(resolved: &[OptionDescriptor], explicit: &[OptionSelection]) -> Vec<OptionSelection> {
    build_selections(resolved)
        .into_iter()
        .filter(|selection| explicit.iter().any(|chosen| chosen.id == selection.id))
        .collect()
}

/// `withoutPlanAgentOption`: with plan mode off, an agent picker cannot choose `plan`.
fn without_plan_agent(descriptors: &[OptionDescriptor]) -> Vec<OptionDescriptor> {
    descriptors
        .iter()
        .filter_map(|descriptor| match descriptor {
            OptionDescriptor::Select(select) if select.id == "agent" => {
                let options: Vec<OptionChoice> = select.options.iter().filter(|option| option.id != "plan").cloned().collect();
                if options.is_empty() {
                    return None;
                }
                let current_value = select
                    .current_value
                    .clone()
                    .filter(|value| options.iter().any(|option| option.id == *value))
                    .or_else(|| options.iter().find(|option| option.is_default).or(options.first()).map(|option| option.id.clone()));
                Some(OptionDescriptor::Select(SelectDescriptor { options, current_value, ..select.clone() }))
            }
            other => Some(other.clone()),
        })
        .collect()
}

/// `withImplicitFastModeDefault`: a model with a Fast mode switch dispatches it off unless the
/// user turned it on (Cursor's agent can otherwise default it on).
fn with_implicit_fast_mode(descriptors: &[OptionDescriptor], explicit: &[OptionSelection]) -> Vec<OptionSelection> {
    let has_switch = descriptors.iter().any(|d| matches!(d, OptionDescriptor::Boolean(b) if b.id == "fastMode"));
    let mut selections = explicit.to_vec();
    if has_switch && !explicit.iter().any(|selection| selection.id == "fastMode") {
        selections.push(OptionSelection { id: "fastMode".into(), value: OptionValue::Bool(false) });
    }
    selections
}

/// What the composer shows and sends for a model (`getComposerProviderState`): its descriptors
/// with current values, and the options a turn carries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComposerOptions {
    pub descriptors: Vec<OptionDescriptor>,
    pub dispatch: Vec<OptionSelection>,
}

/// `model` is the catalog entry for the chosen slug, if the provider lists it.
pub fn composer_options(driver: &str, model: Option<&ProviderModel>, plan_mode_enabled: bool, explicit: &[OptionSelection]) -> ComposerOptions {
    if driver == "opencode" && model.is_none() {
        // A model outside OpenCode's catalog has no descriptors to normalize against, so the
        // options it was given go out as they are.
        let dispatch = explicit
            .iter()
            .filter(|selection| plan_mode_enabled || !(selection.id == "agent" && selection.value == OptionValue::Str("plan".into())))
            .cloned()
            .collect();
        return ComposerOptions { descriptors: Vec::new(), dispatch };
    }
    let caps = model.and_then(|model| model.capabilities.as_ref()).map(|caps| caps.option_descriptors.as_slice()).unwrap_or_default();
    let caps = if plan_mode_enabled { caps.to_vec() } else { without_plan_agent(caps) };
    let selections = with_implicit_fast_mode(&caps, explicit);
    let descriptors = resolve_descriptors(&caps, &selections);
    let dispatch = explicit_selections(&descriptors, &selections);
    ComposerOptions { descriptors, dispatch }
}

/// The selections after picking `value` for one descriptor, as `getTraitSelectChange` builds them.
/// Prompt-injected levels (Ultrathink) are typed into the prompt, never picked.
pub fn with_value(descriptors: &[OptionDescriptor], id: &str, value: OptionValue) -> Option<Vec<OptionSelection>> {
    let mut changed = false;
    let next: Vec<OptionDescriptor> = descriptors
        .iter()
        .map(|descriptor| match (descriptor, &value) {
            (OptionDescriptor::Select(select), OptionValue::Str(choice)) if select.id == id => {
                if choice.is_empty() || select.prompt_injected_values.contains(choice) {
                    return descriptor.clone();
                }
                changed = true;
                OptionDescriptor::Select(SelectDescriptor { current_value: Some(choice.clone()), ..select.clone() })
            }
            (OptionDescriptor::Boolean(boolean), OptionValue::Bool(flag)) if boolean.id == id => {
                changed = true;
                OptionDescriptor::Boolean(BooleanDescriptor { current_value: Some(*flag), ..boolean.clone() })
            }
            _ => descriptor.clone(),
        })
        .collect();
    changed.then(|| build_selections(&next))
}

/// `isReasoningControl`.
pub fn is_reasoning_control(descriptor: &OptionDescriptor) -> bool {
    matches!(descriptor, OptionDescriptor::Select(select) if select.role.as_deref() == Some("reasoning"))
        || REASONING_IDS.contains(&descriptor.id())
}

/// `selectableProviderOptionChoices`.
pub fn selectable_choices(select: &SelectDescriptor) -> impl Iterator<Item = &OptionChoice> {
    select.options.iter().filter(|option| !select.prompt_injected_values.contains(&option.id))
}

/// The footer's speed control: Codex service tiers or a Fast mode switch (`splitSpeed`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Speed<'a> {
    Tier(&'a SelectDescriptor),
    FastMode(&'a BooleanDescriptor),
}

/// How the model picker lays out a model's descriptors: the reasoning control heads the effort
/// column, the other options sit below it, and speed goes to the footer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TraitLayout<'a> {
    pub effort: Option<&'a OptionDescriptor>,
    pub others: Vec<&'a OptionDescriptor>,
    pub speed: Option<Speed<'a>>,
}

pub fn layout<'a>(driver: &str, descriptors: &'a [OptionDescriptor]) -> TraitLayout<'a> {
    let is_speed = |descriptor: &OptionDescriptor| match descriptor {
        OptionDescriptor::Select(select) => driver == "codex" && select.id == "serviceTier",
        OptionDescriptor::Boolean(boolean) => boolean.id == "fastMode",
    };
    let speed = descriptors
        .iter()
        .find(|d| matches!(d, OptionDescriptor::Select(_)) && is_speed(d))
        .or_else(|| descriptors.iter().find(|d| matches!(d, OptionDescriptor::Boolean(_)) && is_speed(d)))
        .map(|descriptor| match descriptor {
            OptionDescriptor::Select(select) => Speed::Tier(select),
            OptionDescriptor::Boolean(boolean) => Speed::FastMode(boolean),
        });
    // Selects lead the column, then switches, as `columnControls` orders them.
    let column: Vec<&OptionDescriptor> = descriptors
        .iter()
        .filter(|d| matches!(d, OptionDescriptor::Select(_)))
        .chain(descriptors.iter().filter(|d| matches!(d, OptionDescriptor::Boolean(_))))
        .filter(|descriptor| !is_speed(descriptor))
        .collect();
    let effort = column.iter().copied().find(|descriptor| is_reasoning_control(descriptor));
    let others = column.into_iter().filter(|descriptor| !effort.is_some_and(|effort| std::ptr::eq(effort, *descriptor))).collect();
    TraitLayout { effort, others, speed }
}

/// What the model trigger shows after the model name (`describeComposerEffort` with the default
/// "label" effort display): the reasoning level's name and whether a fast speed is on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffortSuffix {
    pub word: Option<String>,
    pub fast: bool,
    pub ultrafast: bool,
}

pub fn effort_suffix(driver: &str, descriptors: &[OptionDescriptor]) -> EffortSuffix {
    let layout = layout(driver, descriptors);
    let word = layout.effort.and_then(|descriptor| match descriptor {
        OptionDescriptor::Boolean(boolean) => {
            Some(format!("{} {}", boolean.label, if boolean.current_value == Some(true) { "On" } else { "Off" }))
        }
        OptionDescriptor::Select(_) => current_label(descriptor),
    });
    let speed = speed_label(driver, descriptors);
    EffortSuffix { word, fast: speed == Some("Fast"), ultrafast: speed == Some("Ultrafast") }
}

/// The speed half of `buildTraitsTriggerDisplay`: "Fast" or "Ultrafast" when a speed is on.
fn speed_label(driver: &str, descriptors: &[OptionDescriptor]) -> Option<&'static str> {
    let mut speed = None;
    for descriptor in descriptors {
        match descriptor {
            OptionDescriptor::Boolean(boolean) if boolean.id == "fastMode" => {
                speed = (boolean.current_value == Some(true)).then_some("Fast");
            }
            OptionDescriptor::Select(select) if driver == "codex" && select.id == "serviceTier" => {
                let current = select_value(select);
                let tier = |label: &str| select.options.iter().find(|option| option.label == label).map(|option| &option.id);
                speed = match (current.as_ref(), tier("Fast"), tier("Ultrafast")) {
                    (Some(current), _, Some(ultra)) if current == ultra => Some("Ultrafast"),
                    (Some(current), Some(fast), _) if current == fast => Some("Fast"),
                    _ => None,
                };
            }
            _ => {}
        }
    }
    speed
}

#[cfg(test)]
mod tests;
