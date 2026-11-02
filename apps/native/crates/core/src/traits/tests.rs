use super::*;

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
    assert_eq!(effort_suffix("claudeAgent", &fast), EffortSuffix { word: Some("High".into()), fast: true, ultrafast: false });
    let tiers: Vec<OptionDescriptor> = serde_json::from_value(json!([
        {"id": "serviceTier", "label": "Speed", "type": "select", "currentValue": "flex",
         "options": [{"id": "default", "label": "Standard"}, {"id": "flex", "label": "Ultrafast"}]}
    ]))
    .unwrap();
    assert!(effort_suffix("codex", &tiers).ultrafast);
    assert_eq!(effort_suffix("codex", &tiers).word, None);
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
