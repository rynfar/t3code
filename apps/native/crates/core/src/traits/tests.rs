use super::*;
use crate::server_config::ProviderModel;

fn descriptors() -> Vec<OptionDescriptor> {
    serde_json::from_value(json!([
        {"id": "reasoningEffort", "label": "Reasoning", "type": "select", "promptInjectedValues": ["ultrathink"],
         "options": [{"id": "low", "label": "Low"}, {"id": "high", "label": "High", "isDefault": true},
                     {"id": "ultrathink", "label": "Ultrathink"}]},
        {"id": "fastMode", "label": "Fast mode", "type": "boolean", "currentValue": false},
        {"id": "agent", "label": "Agent", "type": "select", "options": [{"id": "build", "label": "Build"}]}
    ]))
    .unwrap()
}

#[test]
fn selections_read_the_array_and_the_legacy_object() {
    let array = selections(Some(&json!([{"id": "effort", "value": " high "}, {"id": "fast", "value": true}, {"id": "", "value": "x"}])));
    assert_eq!(array.len(), 2);
    assert_eq!(array[0].value, OptionValue::Str("high".into()));
    let legacy = selections(Some(&json!({"effort": "low", "bad": 3})));
    assert_eq!(legacy, vec![OptionSelection { id: "effort".into(), value: OptionValue::Str("low".into()) }]);
    assert!(selections(None).is_empty());
}

#[test]
fn resolved_descriptors_take_valid_selections_and_fall_back_to_defaults() {
    let picked = resolve_descriptors(&descriptors(), &selections(Some(&json!([{"id": "reasoningEffort", "value": "low"}]))));
    assert_eq!(current_label(&picked[0]).as_deref(), Some("Low"));
    let unknown = resolve_descriptors(&descriptors(), &selections(Some(&json!([{"id": "reasoningEffort", "value": "turbo"}]))));
    assert_eq!(current_label(&unknown[0]).as_deref(), Some("High"));
    let injected = resolve_descriptors(&descriptors(), &selections(Some(&json!([{"id": "reasoningEffort", "value": "ultrathink"}]))));
    assert_eq!(current_label(&injected[0]).as_deref(), Some("High"));
}

#[test]
fn picking_a_value_rebuilds_every_selection_and_refuses_injected_levels() {
    let resolved = resolve_descriptors(&descriptors(), &[]);
    let next = with_value(&resolved, "reasoningEffort", OptionValue::Str("low".into())).unwrap();
    assert_eq!(
        next,
        vec![
            OptionSelection { id: "reasoningEffort".into(), value: OptionValue::Str("low".into()) },
            OptionSelection { id: "fastMode".into(), value: OptionValue::Bool(false) },
        ]
    );
    assert!(with_value(&resolved, "reasoningEffort", OptionValue::Str("ultrathink".into())).is_none());
    let fast = with_value(&resolved, "fastMode", OptionValue::Bool(true)).unwrap();
    assert_eq!(fast[1].value, OptionValue::Bool(true));
    let selection = model_selection("claudeAgent", "opus", &fast);
    assert_eq!(selection.options, Some(json!([{"id": "reasoningEffort", "value": "high"}, {"id": "fastMode", "value": true}])));
    assert_eq!(model_selection("codex", "gpt", &[]).options, None);
}

#[test]
fn layout_puts_reasoning_in_the_column_and_speed_in_the_footer() {
    let resolved = resolve_descriptors(&descriptors(), &[]);
    let layout = layout("claudeAgent", &resolved);
    assert_eq!(layout.effort.map(OptionDescriptor::id), Some("reasoningEffort"));
    assert_eq!(layout.others.iter().map(|d| d.id()).collect::<Vec<_>>(), vec!["agent"]);
    assert!(matches!(layout.speed, Some(Speed::FastMode(_))));
    assert_eq!(selectable_choices(match &resolved[0] {
        OptionDescriptor::Select(select) => select,
        OptionDescriptor::Boolean(_) => unreachable!(),
    })
    .count(), 2);
}

#[test]
fn effort_suffix_names_the_level_and_flags_fast() {
    let fast = resolve_descriptors(&descriptors(), &selections(Some(&json!([{"id": "fastMode", "value": true}]))));
    assert_eq!(effort_suffix("claudeAgent", &fast), EffortSuffix { word: Some("High".into()), fast: true, ultrafast: false, ultracode: false, context_window: None });
    let tiers: Vec<OptionDescriptor> = serde_json::from_value(json!([
        {"id": "serviceTier", "label": "Speed", "type": "select", "currentValue": "flex",
         "options": [{"id": "default", "label": "Standard"}, {"id": "flex", "label": "Ultrafast"}]}
    ]))
    .unwrap();
    assert!(effort_suffix("codex", &tiers).ultrafast);
    assert_eq!(effort_suffix("codex", &tiers).word, None);
    // #33177: Pi and other drivers offering `serviceTier` render speed tiers in the footer and suffix too.
    assert!(effort_suffix("pi", &tiers).ultrafast);
    assert_eq!(effort_suffix("pi", &tiers).word, None);
    assert!(matches!(layout("pi", &tiers).speed, Some(Speed::Tier(_))));

    let with_window: Vec<OptionDescriptor> = serde_json::from_value(json!([
        {"id": "effort", "label": "Effort", "type": "select", "options": [{"id": "high", "label": "High", "isDefault": true}]},
        {"id": "contextWindow", "label": "Context window", "type": "select", "currentValue": "1m",
         "options": [{"id": "200k", "label": "200k"}, {"id": "1m", "label": "1M"}]}
    ]))
    .unwrap();
    assert_eq!(
        effort_suffix("claudeAgent", &with_window),
        EffortSuffix { word: Some("High".into()), fast: false, ultrafast: false, ultracode: false, context_window: Some("1M".into()) }
    );
}

#[test]
fn explicit_selections_keep_only_what_the_user_chose() {
    let explicit = selections(Some(&json!([{"id": "reasoningEffort", "value": "turbo"}, {"id": "gone", "value": true}])));
    let resolved = resolve_descriptors(&descriptors(), &explicit);
    assert_eq!(
        explicit_selections(&resolved, &explicit),
        vec![OptionSelection { id: "reasoningEffort".into(), value: OptionValue::Str("high".into()) }]
    );
    assert!(explicit_selections(&resolved, &[]).is_empty());
}

fn model(descriptors: Value) -> ProviderModel {
    serde_json::from_value(json!({"slug": "m", "name": "M", "isCustom": false, "capabilities": {"optionDescriptors": descriptors}})).unwrap()
}

#[test]
fn composer_options_send_fast_mode_off_unless_chosen() {
    let cursor = model(json!([{"id": "fastMode", "label": "Fast", "type": "boolean", "currentValue": true}]));
    let options = composer_options("cursor", Some(&cursor), false, &[]);
    assert_eq!(options.dispatch, vec![OptionSelection { id: "fastMode".into(), value: OptionValue::Bool(false) }]);
    let chosen = [OptionSelection { id: "fastMode".into(), value: OptionValue::Bool(true) }];
    assert_eq!(composer_options("cursor", Some(&cursor), false, &chosen).dispatch, chosen.to_vec());
    let plain = model(json!([{"id": "effort", "label": "Effort", "type": "select", "options": [{"id": "low", "label": "Low", "isDefault": true}]}]));
    assert!(composer_options("codex", Some(&plain), false, &[]).dispatch.is_empty());
}

#[test]
fn composer_options_hide_the_plan_agent_while_plan_mode_is_off() {
    let opencode = model(json!([{"id": "agent", "label": "Agent", "type": "select", "currentValue": "plan",
        "options": [{"id": "build", "label": "Build", "isDefault": true}, {"id": "plan", "label": "Plan"}]}]));
    let off = composer_options("opencode", Some(&opencode), false, &[]);
    assert!(matches!(&off.descriptors[0], OptionDescriptor::Select(select) if select.options.len() == 1 && select.current_value.as_deref() == Some("build")));
    let on = composer_options("opencode", Some(&opencode), true, &[]);
    assert!(matches!(&on.descriptors[0], OptionDescriptor::Select(select) if select.options.len() == 2));
}

#[test]
fn uncatalogued_opencode_models_keep_their_options() {
    let kept = selections(Some(&json!([{"id": "variant", "value": "high"}, {"id": "agent", "value": "plan"}])));
    let off = composer_options("opencode", None, false, &kept);
    assert_eq!(off.dispatch, vec![OptionSelection { id: "variant".into(), value: OptionValue::Str("high".into()) }]);
    assert!(off.descriptors.is_empty());
    assert_eq!(composer_options("opencode", None, true, &kept).dispatch.len(), 2);
}

#[test]
fn ultracode_boolean_normalizes_legacy_effort_and_layouts_in_footer() {
    let modern: Vec<OptionDescriptor> = serde_json::from_value(json!([
        {"id": "effort", "label": "Effort", "type": "select", "options": [
            {"id": "high", "label": "High", "isDefault": true},
            {"id": "xhigh", "label": "XHigh"}
        ]},
        {"id": "ultracode", "label": "Ultracode", "type": "boolean", "currentValue": false}
    ]))
    .unwrap();
    let legacy = selections(Some(&json!([{"id": "effort", "value": "ultracode"}])));
    let resolved = resolve_descriptors(&modern, &legacy);
    assert_eq!(current_label(&resolved[0]).as_deref(), Some("XHigh"));
    assert_eq!(current_value(&resolved[1]), Some(OptionValue::Bool(true)));
    assert_eq!(
        explicit_selections(&resolved, &legacy),
        vec![
            OptionSelection { id: "effort".into(), value: OptionValue::Str("xhigh".into()) },
            OptionSelection { id: "ultracode".into(), value: OptionValue::Bool(true) },
        ]
    );

    // Explicit `ultracode: false` is preserved even alongside legacy `effort: ultracode`.
    let explicit_off = selections(Some(&json!([{"id": "effort", "value": "ultracode"}, {"id": "ultracode", "value": false}])));
    let resolved_off = resolve_descriptors(&modern, &explicit_off);
    assert_eq!(current_label(&resolved_off[0]).as_deref(), Some("XHigh"));
    assert_eq!(current_value(&resolved_off[1]), Some(OptionValue::Bool(false)));
    assert_eq!(
        explicit_selections(&resolved_off, &explicit_off),
        vec![
            OptionSelection { id: "effort".into(), value: OptionValue::Str("xhigh".into()) },
            OptionSelection { id: "ultracode".into(), value: OptionValue::Bool(false) },
        ]
    );

    // Older servers without the `ultracode` boolean retain legacy `effort: ultracode`.
    let old_server: Vec<OptionDescriptor> = serde_json::from_value(json!([
        {"id": "effort", "label": "Effort", "type": "select", "options": [
            {"id": "high", "label": "High", "isDefault": true},
            {"id": "ultracode", "label": "Ultracode"}
        ]}
    ]))
    .unwrap();
    let old_resolved = resolve_descriptors(&old_server, &legacy);
    assert_eq!(current_label(&old_resolved[0]).as_deref(), Some("Ultracode"));
    assert_eq!(explicit_selections(&old_resolved, &legacy), legacy);

    // Independent effort + ultracode places ultracode in footer layout & suffix flag without hiding effort.
    let both = resolve_descriptors(&modern, &selections(Some(&json!([{"id": "effort", "value": "high"}, {"id": "ultracode", "value": true}]))));
    let parts = layout("claudeAgent", &both);
    assert_eq!(parts.effort.map(OptionDescriptor::id), Some("effort"));
    assert!(parts.others.is_empty());
    assert_eq!(parts.ultracode.map(|u| (u.id.as_str(), u.current_value)), Some(("ultracode", Some(true))));
    assert_eq!(effort_suffix("claudeAgent", &both), EffortSuffix { word: Some("High".into()), fast: false, ultrafast: false, ultracode: true, context_window: None });
}
