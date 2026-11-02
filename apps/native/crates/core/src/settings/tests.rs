use serde_json::json;

use super::archive;
use super::keybindings;
use super::search::{self, Section};
use super::snooze::{self, DurationUnit, SnoozeDay, SnoozePreset};
use super::*;
use crate::server_config::{ServerConfigState, ServerConfigStreamEvent};

fn settings(value: serde_json::Value) -> ServerSettings {
    serde_json::from_value(value).unwrap()
}

#[test]
fn thread_settings_take_contract_defaults() {
    let decoded = settings(json!({}));
    assert_eq!(decoded.threads, ThreadSettings::default());
    assert_eq!(decoded.threads.sidebar_auto_settle_after_days, Some(3));
    assert!(decoded.threads.sidebar_auto_settle_on_merge);
    assert_eq!(decoded.threads.snooze_presets.len(), 5);
    assert_eq!(effective_env_mode(&decoded), ThreadEnvMode::Local);
    assert_eq!(decoded.threads.response_streaming_mode, ResponseStreamingMode::Paragraph);
}

#[test]
fn thread_settings_decode_values_and_drop_bad_ones_alone() {
    let decoded = settings(json!({
        "defaultRuntimeMode": "approval-required",
        "sidebarAutoSettleAfterDays": null,
        "sidebarAutoSettleOnMerge": "yes",
        "snoozeLimitedThreads": true,
        "snoozePresets": [{"kind": "after", "minutes": 30}, {"kind": "at", "day": "friday", "time": "17:30"}],
        "defaultThreadEnvMode": "worktree",
        "fallbackModelSelection": {"instanceId": "codex", "model": "gpt-5"},
        "addProjectBaseDirectory": "~/code",
        "responseStreamingMode": "turn",
        "futureKey": {"anything": 1},
    }));
    assert_eq!(decoded.default_runtime_mode, RuntimeMode::ApprovalRequired);
    let threads = &decoded.threads;
    assert_eq!(threads.sidebar_auto_settle_after_days, None);
    assert!(!threads.sidebar_auto_settle_on_merge);
    assert!(threads.snooze_limited_threads);
    assert_eq!(threads.snooze_presets[1], SnoozePreset::At { day: SnoozeDay::Friday, time: "17:30".into() });
    assert_eq!(effective_env_mode(&decoded), ThreadEnvMode::Worktree);
    assert_eq!(decoded.fallback_model_selection.as_ref().map(|s| s.model.as_str()), Some("gpt-5"), "the composer's field still decodes beside the flattened ones");
    assert_eq!(threads.add_project_base_directory, "~/code");
    assert_eq!(threads.response_streaming_mode, ResponseStreamingMode::Turn);

    let out_of_range = settings(json!({"sidebarAutoSettleAfterDays": 400, "snoozePresets": [{"kind": "at", "day": "today", "time": "25:00"}]}));
    assert_eq!(out_of_range.threads.sidebar_auto_settle_after_days, Some(3));
    assert_eq!(out_of_range.threads.snooze_presets, snooze::default_presets());
}

#[test]
fn every_flattened_key_reaches_thread_settings() {
    // A key `ServerSettings` also declares is consumed before the flattened struct sees it.
    let non_default = json!({
        "sidebarAutoSettleAfterDays": 7,
        "sidebarAutoSettleOnMerge": false,
        "sidebarAutoSettleKeepPullRequests": true,
        "autoResumeLimitedThreads": true,
        "snoozeLimitedThreads": true,
        "snoozePresets": [{"kind": "after", "minutes": 15}],
        "addProjectBaseDirectory": "~/src",
        "responseStreamingMode": "turn",
    });
    let decoded = settings(non_default.clone()).threads;
    let defaults = ThreadSettings::default();
    for key in non_default.as_object().unwrap().keys() {
        let mut only = serde_json::Map::new();
        only.insert(key.clone(), non_default[key].clone());
        assert_ne!(settings(serde_json::Value::Object(only)).threads, defaults, "{key} is shadowed");
    }
    assert_eq!(decoded.sidebar_auto_settle_after_days, Some(7));
}

#[test]
fn changes_serialize_as_server_settings_patches() {
    assert_eq!(
        ServerSettingChange::DefaultRuntimeMode(RuntimeMode::AutoAcceptEdits).payload(),
        json!({"patch": {"defaultRuntimeMode": "auto-accept-edits"}})
    );
    assert_eq!(ServerSettingChange::AutoSettleAfterDays(None).payload(), json!({"patch": {"sidebarAutoSettleAfterDays": null}}));
    assert_eq!(ServerSettingChange::AutoSettleAfterDays(Some(500)).payload(), json!({"patch": {"sidebarAutoSettleAfterDays": 90}}));
    assert_eq!(ServerSettingChange::DefaultThreadEnvMode(None).payload(), json!({"patch": {"defaultThreadEnvMode": null}}));
    assert_eq!(
        ServerSettingChange::DefaultModelSelection(Some(ModelSelection { instance_id: "codex".into(), model: "gpt-5".into(), options: None }))
            .payload(),
        json!({"patch": {"defaultModelSelection": {"instanceId": "codex", "model": "gpt-5"}}})
    );
    assert_eq!(
        ServerSettingChange::SnoozePresets(vec![SnoozePreset::After { minutes: 90 }, SnoozePreset::At { day: SnoozeDay::Monday, time: "09:00".into() }])
            .payload(),
        json!({"patch": {"snoozePresets": [{"kind": "after", "minutes": 90}, {"kind": "at", "day": "monday", "time": "09:00"}]}})
    );
    assert_eq!(ServerSettingChange::AddProjectBaseDirectory("  ~/src ".into()).payload(), json!({"patch": {"addProjectBaseDirectory": "~/src"}}));
}

#[test]
fn update_reply_replaces_settings_through_the_reducer() {
    let mut state = ServerConfigState::default();
    state.apply(serde_json::from_value::<ServerConfigStreamEvent>(json!({"type": "snapshot", "config": {"settings": {}}})).unwrap());
    let reply = decode_update_reply(json!({"sidebarAutoSettleOnMerge": false})).unwrap();
    let event = ServerConfigStreamEvent::SettingsUpdated { payload: Box::new(crate::server_config::SettingsPayload { settings: reply }) };
    assert!(state.apply(event));
    assert!(!state.config.unwrap().settings.threads.sidebar_auto_settle_on_merge);
    assert!(decode_update_reply(json!("nope")).is_none());
}

#[test]
fn snooze_labels_follow_the_day_gap() {
    let monday = SnoozePreset::At { day: SnoozeDay::Monday, time: "09:00".into() };
    assert_eq!(monday.days_ahead(0), 1);
    assert_eq!(monday.label(monday.days_ahead(0)), "Tomorrow");
    assert_eq!(monday.days_ahead(1), 7);
    assert_eq!(monday.label(7), "Next week");
    let friday = SnoozePreset::At { day: SnoozeDay::Friday, time: "09:00".into() };
    assert_eq!(friday.days_ahead(1), 4);
    assert_eq!(friday.label(4), "Friday");
    assert_eq!(friday.label(friday.days_ahead(5)), "Next Friday");
    let evening = SnoozePreset::At { day: SnoozeDay::Today, time: "18:00".into() };
    assert_eq!(evening.label(0), "This evening");
    assert!(!evening.passed(17 * 60 + 59));
    assert!(evening.passed(18 * 60));
    assert_eq!(SnoozePreset::At { day: SnoozeDay::Today, time: "08:00".into() }.label(0), "This morning");
    assert_eq!(SnoozePreset::After { minutes: 180 }.label(0), "In 3 hours");
    assert_eq!(SnoozePreset::After { minutes: 1440 }.label(0), "In 1 day");
    assert_eq!(SnoozePreset::After { minutes: 90 }.label(0), "In 90 minutes");
}

#[test]
fn snooze_editor_rules() {
    let defaults = snooze::default_presets();
    assert_eq!(snooze::next_candidate(&defaults), SnoozePreset::After { minutes: 30 });
    let mut reordered = defaults.clone();
    reordered.reverse();
    assert!(snooze::same_presets(&reordered, &defaults));
    assert!(!snooze::same_presets(&defaults[1..], &defaults));
    assert_eq!(snooze::split_minutes(120), (2, DurationUnit::Hours));
    assert_eq!(snooze::step_amount(60, 1), 120);
    assert_eq!(snooze::step_amount(60, -5), 60);
    assert_eq!(snooze::step_amount(snooze::MAX_PRESET_MINUTES, 1), snooze::MAX_PRESET_MINUTES);
    assert_eq!(snooze::with_unit(120, DurationUnit::Days), 2 * 1440);
    let after = SnoozePreset::After { minutes: 45 };
    assert_eq!(snooze::with_when(&after, Some(SnoozeDay::Friday), 60, "09:00"), SnoozePreset::At { day: SnoozeDay::Friday, time: "09:00".into() });
    assert_eq!(snooze::with_when(&SnoozePreset::At { day: SnoozeDay::Today, time: "18:00".into() }, None, 45, "09:00"), after);
    assert_eq!(snooze::parse_time("09:30"), Some((9, 30)));
    assert_eq!(snooze::parse_time("9:30"), None);
    assert_eq!(snooze::parse_time("24:00"), None);
}

#[test]
fn nav_keeps_the_web_order_and_drops_unported_sections() {
    let groups = search::nav_groups();
    let flat: Vec<Section> = groups.iter().flat_map(|(_, sections)| sections.iter().copied()).collect();
    assert_eq!(
        flat,
        vec![
            Section::General,
            Section::Appearance,
            Section::Chat,
            Section::Keybindings,
            Section::Threads,
            Section::Connections,
            Section::Labs,
            Section::About,
            Section::Archived
        ]
    );
    assert_eq!(groups.iter().map(|(heading, _)| *heading).collect::<Vec<_>>(), vec![None, Some("Agents"), Some("Connections"), None]);
    assert!(search::items().iter().all(|item| item.section.is_available()));
}

#[test]
fn search_ranks_titles_before_terms_and_keybindings_last() {
    let mut items = search::items();
    items.push(search::keybinding_item("chat.new", "Chat: New", &["⌘N".into()]));
    let titles = |query: &str| search::search(query, &items).iter().map(|item| item.title.clone()).collect::<Vec<_>>();
    assert_eq!(titles("time format").first().map(String::as_str), Some("Time format"));
    assert_eq!(titles("  CHAT   width ").first().map(String::as_str), Some("Chat width"));
    let chat = titles("chat");
    assert_eq!(chat.last().map(String::as_str), Some("Chat: New"));
    assert!(chat.contains(&"Send shortcut".to_owned()), "section label matches");
    assert!(titles("worktree").contains(&"New threads".to_owned()));
    assert!(titles("").is_empty());
    assert!(titles("zzzz").is_empty());
    let days = search::search("days of inactivity", &items);
    assert_eq!(days[0].anchor(), "auto-settle-inactive-threads");
    assert_eq!(search::normalize("Caf\u{0065}\u{0301}  Mode"), "cafe mode");
}

#[test]
fn keybinding_rows_group_by_command_and_label_like_the_web() {
    let config = json!([
        {"command": "chat.new", "shortcut": {"key": "n", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true}},
        {"command": "chat.new", "shortcut": {"key": "o", "metaKey": false, "ctrlKey": false, "shiftKey": true, "altKey": false, "modKey": true}, "whenAst": {"type": "identifier", "name": "x"}},
        {"command": "terminal.toggle", "shortcut": {"key": "j", "metaKey": false, "ctrlKey": true, "shiftKey": false, "altKey": true, "modKey": false}},
        {"command": "broken"}
    ]);
    let rows = keybindings::rows(&config, true);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].label, "Chat: New");
    assert_eq!(rows[0].shortcuts, vec!["⌘N", "⇧⌘O"]);
    assert!(rows[0].conditional);
    assert_eq!(rows[1].shortcuts, vec!["⌃⌥J"]);
    let other = keybindings::rows(&config, false);
    assert_eq!(other[0].shortcuts, vec!["Ctrl+N", "Ctrl+Shift+O"]);
    assert_eq!(keybindings::filter(&rows, "terminal").len(), 1);
    let grouped = keybindings::groups(&keybindings::filter(&rows, ""));
    assert_eq!(grouped.iter().map(|(title, rows)| (*title, rows.len())).collect::<Vec<_>>(), vec![("Threads", 1), ("Terminal", 1)]);
    assert_eq!(keybindings::filter(&rows, "⌘n").len(), 1);
    assert_eq!(keybindings::command_label("sidebar.toggleProjectFilter"), "Sidebar: Toggle Project Filter");
    assert_eq!(keybindings::command_label("script.build-app.run"), "Run Script: Build App");
    assert_eq!(keybindings::command_label("composer.sendAlternate"), "Composer: Opposite Queue or Steer Action");
}

#[test]
fn archive_groups_follow_snapshot_projects_newest_archive_first() {
    let snapshot = json!({
        "schemaVersion": 1, "snapshotSequence": 4,
        "projects": [{"id": "p1", "title": "Alpha"}, {"id": "p2", "title": "Beta"}, {"id": "p3", "title": "Empty"}, {"nope": 1}],
        "threads": [
            {"id": "t1", "projectId": "p1", "title": "Old", "createdAt": "2026-10-01T00:00:00Z", "archivedAt": "2026-10-02T00:00:00Z"},
            {"id": "t2", "projectId": "p1", "title": "New", "createdAt": "2026-10-01T00:00:00Z", "archivedAt": "2026-10-05T00:00:00Z"},
            {"id": "t3", "projectId": "p2", "title": "Legacy", "createdAt": "2026-09-01T00:00:00Z"},
            {"id": "bad"}
        ]
    });
    let mut groups = archive::groups(&snapshot);
    assert_eq!(groups.iter().map(|g| g.title.as_str()).collect::<Vec<_>>(), vec!["Alpha", "Beta"]);
    assert_eq!(groups[0].threads.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["t2", "t1"]);
    archive::remove_thread(&mut groups, "t3");
    assert_eq!(groups.len(), 1);
    let now = crate::timeline::parse_iso_ms("2026-10-05T03:00:00Z").unwrap();
    assert_eq!(archive::description(&groups[0].threads[0], now), "Archived 3h ago \u{00b7} Created 4d ago");
    assert_eq!(archive::relative_label("2026-10-05T02:59:30Z", now), "just now");
    assert!(archive::groups(&json!(null)).is_empty());
}
