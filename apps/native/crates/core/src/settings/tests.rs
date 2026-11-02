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
fn update_reply_replaces_settings_unless_a_newer_change_streamed_first() {
    let event = |value: serde_json::Value| serde_json::from_value::<ServerConfigStreamEvent>(value).unwrap();
    let mut state = ServerConfigState::default();
    state.apply(event(json!({"type": "snapshot", "config": {"settings": {}}})));
    let sent = state.settings_revision();
    let reply = decode_update_reply(json!({"sidebarAutoSettleOnMerge": false})).unwrap();
    assert!(state.apply_settings_reply(reply, sent));
    assert!(!state.config.as_ref().unwrap().settings.threads.sidebar_auto_settle_on_merge);
    assert!(decode_update_reply(json!("nope")).is_none());

    // Another client's save streams in after ours was sent; our older reply must not undo it.
    let sent = state.settings_revision();
    state.apply(event(json!({"type": "settingsUpdated", "payload": {"settings": {"sidebarAutoSettleOnMerge": false, "snoozeLimitedThreads": true}}})));
    let stale = decode_update_reply(json!({"sidebarAutoSettleOnMerge": false})).unwrap();
    assert!(!state.apply_settings_reply(stale, sent));
    assert!(state.config.as_ref().unwrap().settings.threads.snooze_limited_threads);
}

#[test]
fn snooze_draft_builds_each_edit_on_the_unsaved_ones() {
    let saved = snooze::default_presets();
    let mut draft = snooze::SnoozeDraft::new(&saved);
    let mut first = saved.clone();
    first[0] = SnoozePreset::After { minutes: 90 };
    assert!(draft.edit(first.clone()));
    let mut second = draft.presets().to_vec();
    second[1] = SnoozePreset::After { minutes: 240 };
    assert!(draft.edit(second.clone()));
    assert!(!draft.edit(second.clone()), "an unchanged list is not saved again");

    // The first save echoes back while the second is in flight: both edits stay.
    draft.observe(&first);
    assert_eq!(draft.presets(), second.as_slice());
    draft.observe(&second);
    assert_eq!(draft.presets(), second.as_slice());

    // A change made elsewhere is adopted.
    let other = vec![SnoozePreset::After { minutes: 15 }];
    draft.observe(&other);
    assert_eq!(draft.presets(), other.as_slice());

    // A failed save puts the saved list back even though it did not change.
    draft.edit(vec![SnoozePreset::After { minutes: 15 }, SnoozePreset::After { minutes: 30 }]);
    draft.failed();
    draft.observe(&other);
    assert_eq!(draft.presets(), other.as_slice());
}

#[test]
fn saved_snooze_options_become_menu_settings() {
    use crate::sidebar::settled::{DEFAULT_SNOOZE_PRESETS, PresetDay, SnoozePresetSetting, snooze_preset_id};
    assert_eq!(snooze::menu_settings(&snooze::default_presets()), DEFAULT_SNOOZE_PRESETS.to_vec());
    let saved = vec![
        SnoozePreset::After { minutes: 30 },
        SnoozePreset::At { day: SnoozeDay::Friday, time: "17:30".into() },
        SnoozePreset::At { day: SnoozeDay::Sunday, time: "08:05".into() },
    ];
    let menu = snooze::menu_settings(&saved);
    assert_eq!(
        menu,
        vec![
            SnoozePresetSetting::After { minutes: 30 },
            SnoozePresetSetting::At { day: PresetDay::Weekday(5), hour: 17, minute: 30 },
            SnoozePresetSetting::At { day: PresetDay::Weekday(0), hour: 8, minute: 5 },
        ]
    );
    assert_eq!(menu.iter().map(snooze_preset_id).collect::<Vec<_>>(), saved.iter().map(SnoozePreset::id).collect::<Vec<_>>());
}

#[test]
fn quit_shortcut_follows_the_confirmation_mode() {
    use super::quit_hold::{QuitEffect, QuitHint, QuitKeyDown, QuitShortcut};
    use crate::client_settings::QuitConfirmationMode::{DoubleClick, Direct, Hold};
    let cmd_q = |repeat: bool| QuitKeyDown { key: "q", modifier: true, alt: false, shift: false, repeat };

    let mut quit = QuitShortcut::default();
    assert_eq!(quit.key_down(cmd_q(false), Direct, 0), (true, vec![QuitEffect::Quit]));

    // Hold: auto-repeat past the hold time arms the quit, which waits for the release.
    let mut quit = QuitShortcut::default();
    assert_eq!(quit.key_down(cmd_q(false), Hold, 0), (true, vec![QuitEffect::ShowHint(QuitHint::Hold)]));
    for at in (400..=1300).step_by(100) {
        assert_eq!(quit.key_down(cmd_q(true), Hold, at), (true, vec![]));
    }
    assert!(quit.modifier_released(1350).is_empty());
    assert!(quit.tick(1949).is_empty());
    assert_eq!(quit.tick(1950), vec![QuitEffect::HideHint, QuitEffect::Quit]);

    // Hold released early only hides the hint; a second press soon after quits.
    let mut quit = QuitShortcut::default();
    quit.key_down(cmd_q(false), Hold, 0);
    quit.key_down(cmd_q(true), Hold, 500);
    assert_eq!(quit.modifier_released(700), vec![QuitEffect::HideHint]);
    assert_eq!(quit.deadline(), None);
    let mut quit = QuitShortcut::default();
    quit.key_down(cmd_q(false), Hold, 0);
    quit.modifier_released(100);
    assert_eq!(quit.key_down(cmd_q(false), Hold, 300), (true, vec![QuitEffect::Quit]));

    // Another key cancels; unrelated keys pass through.
    let mut quit = QuitShortcut::default();
    quit.key_down(cmd_q(false), Hold, 0);
    let w = QuitKeyDown { key: "w", modifier: true, alt: false, shift: false, repeat: false };
    assert_eq!(quit.key_down(w, Hold, 100), (false, vec![QuitEffect::HideHint]));

    // Double press: the hint outlives the release until the window closes.
    let mut quit = QuitShortcut::default();
    assert_eq!(quit.key_down(cmd_q(false), DoubleClick, 0), (true, vec![QuitEffect::ShowHint(QuitHint::DoublePress)]));
    assert!(quit.modifier_released(100).is_empty());
    assert_eq!(quit.tick(500), vec![QuitEffect::HideHint]);
    quit.key_down(cmd_q(false), DoubleClick, 1000);
    assert_eq!(quit.key_down(cmd_q(false), DoubleClick, 1400), (true, vec![QuitEffect::HideHint, QuitEffect::Quit]));
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
            Section::Providers,
            Section::Threads,
            Section::ScheduledTasks,
            Section::SourceControl,
            Section::Worktrees,
            Section::Connections,
            Section::Access,
            Section::Labs,
            Section::About,
            Section::Archived
        ]
    );
    assert_eq!(groups.iter().map(|(heading, _)| *heading).collect::<Vec<_>>(), vec![None, Some("Agents"), Some("Code"), Some("Connections"), None]);
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
    assert_eq!(keybindings::command_label("sidebar.toggleProjectFilter"), "Sidebar: Toggle Project Filter");
    assert_eq!(keybindings::command_label("script.build-app.run"), "Run Script: Build App");
    assert_eq!(keybindings::command_label("composer.sendAlternate"), "Composer: Opposite Queue or Steer Action");
}

fn shortcut(key: &str, modifiers: &[&str]) -> serde_json::Value {
    let has = |name: &str| modifiers.contains(&name);
    json!({"key": key, "metaKey": has("meta"), "ctrlKey": has("ctrl"), "shiftKey": has("shift"), "altKey": has("alt"), "modKey": has("mod")})
}

#[test]
fn binding_rows_mark_sources_defaults_and_conflicts() {
    let not_terminal = json!({"type": "not", "node": {"type": "identifier", "name": "terminalFocus"}});
    let config = json!([
        // A default, untouched.
        {"command": "sidebar.toggle", "shortcut": shortcut("b", &["mod"])},
        // chat.new rebound: its defaults drop out and the custom binding remembers them.
        {"command": "chat.new", "shortcut": shortcut("k", &["mod"]), "whenAst": not_terminal},
        {"command": "script.build.run", "shortcut": shortcut("r", &["mod", "shift"])},
    ]);
    let rows = keybindings::binding_rows(&config, true);
    let row = |command: &str| rows.iter().find(|row| row.command == command).unwrap();
    assert_eq!(row("sidebar.toggle").source, keybindings::BindingSource::Default);
    assert!(!row("sidebar.toggle").can_remove() && !row("sidebar.toggle").can_reset());
    let custom = row("chat.new");
    assert_eq!((custom.key.as_str(), custom.when.as_str()), ("mod+k", "!terminalFocus"));
    assert_eq!(custom.source, keybindings::BindingSource::Custom);
    assert_eq!((custom.default_key.as_deref(), custom.default_when.as_str()), (Some("mod+n"), "!terminalFocus"));
    assert!(custom.can_reset() && custom.can_remove());
    assert_eq!(rows.iter().filter(|row| row.command == "chat.new").count(), 1);
    assert_eq!(row("script.build.run").source, keybindings::BindingSource::Project);
    // mod+k now also opens the palette where the terminal is not focused.
    assert_eq!(custom.conflicts, vec!["Command Palette: Toggle"]);
    assert_eq!(row("commandPalette.toggle").conflicts, vec!["Chat: New"]);
    // terminal.new (terminalFocus) and the default chat.new (!terminalFocus) both use mod+n but never overlap.
    assert!(row("terminal.new").conflicts.is_empty());
    // Usage commands keep their own order; everything else sorts by command id.
    let usage: Vec<&str> = rows.iter().map(|row| row.command.as_str()).filter(|command| command.starts_with("usage.")).collect();
    assert_eq!(&usage[..3], ["usage.open", "usage.cost", "usage.tokens"]);
    assert!(rows.windows(2).all(|pair| pair[0].command.starts_with("usage.") || pair[1].command.starts_with("usage.") || pair[0].command <= pair[1].command));

    assert_eq!(keybindings::filter_binding_rows(&rows, "custom").len(), 1);
    assert_eq!(keybindings::filter_binding_rows(&rows, "MOD+K").len(), 2);
    let grouped = keybindings::group_binding_rows(&keybindings::filter_binding_rows(&rows, "mod+k"));
    assert_eq!(grouped.iter().map(|(title, rows)| (*title, rows.len())).collect::<Vec<_>>(), vec![("Navigation", 1), ("Threads", 1)]);
    let options = keybindings::command_options(&config);
    assert!(options.contains(&"script.build.run".to_owned()) && options.contains(&"chat.newLocal".to_owned()));
}

#[test]
fn conflicts_compare_resolved_chords_per_platform() {
    let config = json!([{"command": "chat.find", "shortcut": shortcut("f", &["ctrl"])}]);
    let rows = keybindings::binding_rows(&config, false);
    // Off macOS, mod is Ctrl: a ctrl+f binding collides with mod+f, but meta+f does not.
    assert_eq!(keybindings::conflict_labels(&rows, "new", "mod+f", "", false), vec!["Chat: Find"]);
    assert!(keybindings::conflict_labels(&rows, "new", "meta+f", "", false).is_empty());
    assert!(keybindings::conflict_labels(&rows, "new", "", "", false).is_empty());
    assert_eq!(keybindings::conflict_description(&["A".into(), "B".into(), "C".into(), "D".into()]).unwrap(), "Conflicts with A, B, C, and more.");
}

#[test]
fn recording_turns_keystrokes_into_keybinding_inputs() {
    let us = |key, platform, control, alt, shift, mac| keybindings::keybinding_from_keystroke(key, platform, control, alt, shift, mac, None::<&std::collections::HashMap<char, char>>);
    assert_eq!(us("k", true, false, false, true, true).as_deref(), Some("mod+shift+k"));
    assert_eq!(us("k", false, true, false, false, true).as_deref(), Some("ctrl+k"));
    assert_eq!(us("k", true, true, true, false, false).as_deref(), Some("mod+meta+alt+k"));
    // macOS reports Cmd+Shift+[ as cmd-{ without Shift.
    assert_eq!(us("{", true, false, false, false, true).as_deref(), Some("mod+shift+["));
    assert_eq!(us("up", false, false, true, false, true).as_deref(), Some("alt+arrowup"));
    assert_eq!(us("space", true, false, false, false, true).as_deref(), Some("mod+space"));
    assert_eq!(us("f6", false, false, false, true, true).as_deref(), Some("shift+f6"));
    assert_eq!(us("capslock", false, false, false, false, true), None);
    assert_eq!(us("shift", false, false, false, true, true), None);
    // Other layouts keep their glyph, as the web's `recordedLayoutShortcutKey` does. GPUI's key
    // equivalents map each US character to the layout's at the same position.
    let layout = |pairs: &[(char, char)]| pairs.iter().copied().collect::<std::collections::HashMap<char, char>>();
    let british = layout(&[('#', '£')]);
    let record = |key, shift, layout| keybindings::keybinding_from_keystroke(key, true, false, false, shift, true, Some(layout));
    assert_eq!(record("£", false, &british).as_deref(), Some("mod+shift+£"));
    assert_eq!(record("@", false, &british).as_deref(), Some("mod+shift+2"));
    let unshifted_hash = layout(&[('\\', '#'), ('#', '§')]);
    assert_eq!(record("#", false, &unshifted_hash).as_deref(), Some("mod+#"));
    assert_eq!(record("§", false, &unshifted_hash).as_deref(), Some("mod+shift+§"));
    // GPUI clears Shift on a shifted non-ASCII letter and reports it upper-cased.
    assert_eq!(record("Ö", false, &layout(&[(';', 'ö')])).as_deref(), Some("mod+shift+ö"));
    assert_eq!(keybindings::key_pill_parts("mod+shift+k", true), vec!["⌘", "⇧", "K"]);
    assert_eq!(keybindings::key_pill_parts("mod+alt+arrowup", false), vec!["Ctrl", "Alt", "arrowup"]);
}

#[test]
fn edit_payloads_replace_the_row_they_came_from() {
    let config = json!([{"command": "chat.new", "shortcut": shortcut("k", &["mod"]), "whenAst": {"type": "identifier", "name": "isDesktop"}}]);
    let rows = keybindings::binding_rows(&config, true);
    let row = rows.iter().find(|row| row.command == "chat.new").unwrap();
    assert_eq!(
        keybindings::upsert_payload(&row.command, " mod+j ", " ", Some(row)),
        json!({"command": "chat.new", "key": "mod+j", "replace": {"command": "chat.new", "key": "mod+k", "when": "isDesktop"}})
    );
    assert_eq!(keybindings::upsert_payload("terminal.toggle", "ctrl+`", "terminalOpen", None), json!({"command": "terminal.toggle", "key": "ctrl+`", "when": "terminalOpen"}));
    assert_eq!(keybindings::remove_payload(row), json!({"command": "chat.new", "key": "mod+k", "when": "isDesktop"}));
    // No default shares `isDesktop`, so reset falls back to the command's first default.
    assert_eq!(
        keybindings::reset_payload(row).unwrap(),
        json!({"command": "chat.new", "key": "mod+n", "when": "!terminalFocus", "replace": {"command": "chat.new", "key": "mod+k", "when": "isDesktop"}})
    );
}

#[test]
fn when_builder_edits_round_trip_through_expressions() {
    use crate::keybindings::parse_keybinding_when_expression as parse;
    use keybindings::{WhenEdit, WhenOperator, apply_when_edit, when_ast_to_expression as show};
    let expr = |node: Option<crate::keybindings::KeybindingWhenNode>| show(node.as_ref());
    let root = parse("a && (b || !c) && !(d && e)").unwrap();
    // Left-folded chains print with their inner fold parenthesized, as `whenAstToExpression` does.
    assert_eq!(show(Some(&root)), "(a && (b || !c)) && !(d && e)");
    assert_eq!(keybindings::when_children(&root).len(), 3);
    assert_eq!(show(keybindings::node_at(&root, &[1, 1])), "!c");
    assert_eq!(show(keybindings::node_at(&root, &[2, 0, 1])), "e");
    assert!(keybindings::node_at(&root, &[3]).is_none());
    assert!(keybindings::when_can_remove(&root, &[1, 1]));
    assert!(!keybindings::when_can_remove(&root, &[2, 0]), "a negated group's operand goes with the group");

    let edit = |path: &[usize], edit: WhenEdit| expr(apply_when_edit(Some(&root), path, &edit));
    assert_eq!(edit(&[1, 1], WhenEdit::SetNegated(false)), "(a && (b || c)) && !(d && e)");
    assert_eq!(edit(&[0], WhenEdit::SetIdentifier("x".into())), "(x && (b || !c)) && !(d && e)");
    assert_eq!(edit(&[1], WhenEdit::SetOperator(WhenOperator::And)), "(a && (b && !c)) && !(d && e)");
    assert_eq!(edit(&[2], WhenEdit::SetNegated(false)), "(a && (b || !c)) && (d && e)");
    assert_eq!(edit(&[1, 0], WhenEdit::Remove), "(a && !c) && !(d && e)");
    assert_eq!(edit(&[2, 0], WhenEdit::AddCondition), "(a && (b || !c)) && !((d && e) && terminalFocus)");
    assert_eq!(edit(&[1], WhenEdit::AddGroup), "(a && ((b || !c) || (terminalFocus && !terminalFocus))) && !(d && e)");
    assert_eq!(edit(&[], WhenEdit::Remove), "");
    let single = parse("a").unwrap();
    assert_eq!(expr(apply_when_edit(Some(&single), &[], &WhenEdit::SetNegated(true))), "!a");
    assert_eq!(expr(apply_when_edit(None, &[], &WhenEdit::AddCondition)), "terminalFocus");
    assert_eq!(show(Some(&keybindings::add_to_root(Some(&single), true))), "a && (terminalFocus || !terminalFocus)");
    assert_eq!(keybindings::when_remove_label(&root, 0), "Clear all conditions");
    assert_eq!(keybindings::when_remove_label(keybindings::node_at(&root, &[1]).unwrap(), 1), "Remove group and its conditions");

    assert_eq!(keybindings::parse_when_draft(" "), Ok(None));
    assert!(keybindings::parse_when_draft("a &&").is_err());
    assert_eq!(keybindings::unknown_when_variables(Some(&root)), vec!["a", "b", "c", "d", "e"]);
    assert!(keybindings::unknown_when_variables(parse("terminalFocus && !rightPanelOpen").as_ref()).is_empty());
    let options = keybindings::when_variable_options();
    assert_eq!(&options[..6], ["terminalFocus", "terminalOpen", "isWeb", "isDesktop", "true", "false"]);
    assert!(options.contains(&"composerFocus".to_owned()));
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
    use archive::ArchiveEntry::{Project, Thread};
    assert_eq!(archive::entries(&groups), vec![Project(0), Thread(0, 0), Thread(0, 1), Project(1), Thread(1, 0)]);
    archive::remove_thread(&mut groups, "t3");
    assert_eq!(groups.len(), 1);
    assert_eq!(archive::entries(&groups), vec![Project(0), Thread(0, 0), Thread(0, 1)]);
    let now = crate::timeline::parse_iso_ms("2026-10-05T03:00:00Z").unwrap();
    assert_eq!(archive::description(&groups[0].threads[0], now), "Archived 3h ago \u{00b7} Created 4d ago");
    assert_eq!(archive::relative_label("2026-10-05T02:59:30Z", now), "just now");
    assert!(archive::groups(&json!(null)).is_empty());
}
