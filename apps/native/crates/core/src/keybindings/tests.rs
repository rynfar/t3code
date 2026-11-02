use super::*;
use serde_json::json;

const MAC: bool = true;
const WIN: bool = false;
const LINUX: bool = false;

fn sc(value: &str) -> KeybindingShortcut {
    parse_keybinding_shortcut(value).unwrap()
}

fn compile(rules: &[(&str, &str, &str)]) -> Vec<ResolvedKeybindingRule> {
    rules
        .iter()
        .map(|&(key, command, when)| ResolvedKeybindingRule {
            command: command.to_owned(),
            shortcut: sc(key),
            when_ast: (!when.is_empty()).then(|| parse_keybinding_when_expression(when).unwrap()),
        })
        .collect()
}

fn ev<'a>(key: &'a str, modifiers: &str) -> ShortcutEvent<'a> {
    let held = |name| modifiers.split('+').any(|modifier| modifier == name);
    ShortcutEvent {
        key,
        meta_key: held("meta"),
        ctrl_key: held("ctrl"),
        shift_key: held("shift"),
        alt_key: held("alt"),
        ..ShortcutEvent::default()
    }
}

fn ctx(flags: &[(&'static str, bool)]) -> ShortcutMatchContext {
    flags.iter().fold(ShortcutMatchContext::default(), |context, &(name, value)| context.with(name, value))
}

fn defaults() -> &'static [ResolvedKeybindingRule] {
    default_resolved_keybindings()
}

fn resolve<'a>(
    event: ShortcutEvent,
    keybindings: &'a [ResolvedKeybindingRule],
    flags: &[(&'static str, bool)],
    mac: bool,
) -> Option<&'a str> {
    resolve_shortcut_command(&event, keybindings, &ctx(flags), mac)
}

fn ident(name: &str) -> KeybindingWhenNode {
    KeybindingWhenNode::Identifier { name: name.to_owned() }
}

fn not(node: KeybindingWhenNode) -> KeybindingWhenNode {
    KeybindingWhenNode::Not { node: Box::new(node) }
}

fn and(left: KeybindingWhenNode, right: KeybindingWhenNode) -> KeybindingWhenNode {
    KeybindingWhenNode::And { left: Box::new(left), right: Box::new(right) }
}

fn or(left: KeybindingWhenNode, right: KeybindingWhenNode) -> KeybindingWhenNode {
    KeybindingWhenNode::Or { left: Box::new(left), right: Box::new(right) }
}

/// `DEFAULT_BINDINGS` from `apps/web/src/keybindings.test.ts`.
fn web_fixture() -> Vec<ResolvedKeybindingRule> {
    compile(&[
        ("mod+b", "sidebar.toggle", ""),
        ("mod+j", "terminal.toggle", ""),
        ("mod+alt+b", "rightPanel.toggle", ""),
        ("mod+d", "terminal.split", "terminalFocus"),
        ("mod+shift+d", "terminal.splitVertical", "terminalFocus"),
        ("mod+n", "terminal.new", "terminalFocus"),
        ("mod+w", "terminal.close", "terminalFocus"),
        ("mod+w", "rightPanel.close", "!terminalFocus"),
        ("mod+d", "diff.toggle", "!terminalFocus"),
        ("mod+k", "commandPalette.toggle", "!terminalFocus"),
        ("mod+p", "filePicker.toggle", "!terminalFocus"),
        ("mod+shift+f", "projectSearch.toggle", "!terminalFocus"),
        ("mod+alt+shift+t", "themeEditor.toggle", ""),
        ("mod+f", "chat.find", "!terminalFocus && !previewFocus"),
        ("mod+shift+m", "modelPicker.toggle", "!terminalFocus"),
        ("mod+shift+o", "chat.new", ""),
        ("mod+shift+n", "chat.newLocal", ""),
        ("mod+o", "editor.openFavorite", ""),
        ("mod+shift+[", "thread.previous", ""),
        ("mod+shift+]", "thread.next", ""),
        ("mod+shift+c", "thread.copyReference", "!terminalFocus"),
        ("mod+shift+s", "thread.settle", "!terminalFocus"),
        ("mod+1", "thread.jump.1", ""),
        ("mod+2", "thread.jump.2", ""),
        ("mod+3", "thread.jump.3", ""),
        ("mod+1", "modelPicker.jump.1", "modelPickerOpen"),
        ("mod+2", "modelPicker.jump.2", "modelPickerOpen"),
        ("mod+3", "modelPicker.jump.3", "modelPickerOpen"),
    ])
}

#[test]
fn passes_only_effective_preview_shortcuts_to_the_desktop_bridge() {
    let keybindings = compile(&[
        ("mod+shift+t", "view.reopenClosed", ""),
        ("mod+shift+y", "view.reopenClosed", "previewFocus"),
        ("mod+shift+t", "preview.toggle", "previewFocus"),
    ]);
    let focused = ctx(&[("previewFocus", true), ("previewOpen", true)]);
    assert_eq!(
        effective_shortcuts_for_command(&keybindings, "view.reopenClosed", &focused, MAC),
        vec![sc("mod+shift+y")]
    );
    assert_eq!(
        effective_shortcuts_for_command(&keybindings, "view.reopenClosed", &ctx(&[]), MAC),
        vec![sc("mod+shift+t")]
    );
}

#[test]
fn settle_thread_shortcut_does_not_intercept_the_terminal() {
    let fixture = web_fixture();
    assert_eq!(resolve(ev("s", "ctrl+shift"), &fixture, &[("terminalFocus", true)], WIN), None);
}

#[test]
fn thread_undo_leaves_native_undo_alone_inside_text_fields_and_terminals() {
    assert_eq!(resolve(ev("z", "ctrl"), defaults(), &[("editableFocus", true)], WIN), None);
    assert_eq!(resolve(ev("z", "ctrl"), defaults(), &[("terminalFocus", true)], WIN), None);
    assert_eq!(resolve(ev("z", "ctrl"), defaults(), &[], WIN), Some("thread.undo"));
}

#[test]
fn split_and_new_terminal_shortcuts_support_when_expressions() {
    let keybindings = compile(&[
        ("mod+\\", "terminal.split", "terminalOpen && !terminalFocus"),
        ("mod+shift+n", "terminal.new", "terminalOpen && !terminalFocus"),
        ("mod+j", "terminal.toggle", ""),
    ]);
    let open = [("terminalOpen", true), ("terminalFocus", false)];
    assert_eq!(resolve(ev("\\", "ctrl"), &keybindings, &open, WIN), Some("terminal.split"));
    assert_eq!(resolve(ev("\\", "ctrl"), &keybindings, &[("terminalOpen", false)], WIN), None);
    assert_eq!(resolve(ev("n", "ctrl+shift"), &keybindings, &open, WIN), Some("terminal.new"));
}

#[test]
fn label_is_none_for_commands_shadowed_by_a_later_conflicting_shortcut() {
    let bindings = compile(&[("mod+shift+1", "thread.jump.1", ""), ("mod+shift+1", "thread.jump.7", "")]);
    assert_eq!(shortcut_label_for_command(&bindings, "thread.jump.1", &ctx(&[]), MAC), None);
    assert_eq!(shortcut_label_for_command(&bindings, "thread.jump.7", &ctx(&[]), MAC).as_deref(), Some("⇧⌘1"));
}

#[test]
fn label_respects_when_context() {
    let bindings = compile(&[("mod+d", "diff.toggle", ""), ("mod+d", "terminal.split", "terminalFocus")]);
    let unfocused = ctx(&[("terminalFocus", false)]);
    let focused = ctx(&[("terminalFocus", true)]);
    assert_eq!(shortcut_label_for_command(&bindings, "diff.toggle", &unfocused, LINUX).as_deref(), Some("Ctrl+D"));
    assert_eq!(shortcut_label_for_command(&bindings, "diff.toggle", &focused, LINUX), None);
    assert_eq!(shortcut_label_for_command(&bindings, "terminal.split", &focused, LINUX).as_deref(), Some("Ctrl+D"));
}

#[test]
fn never_shows_jump_hints_while_the_terminal_is_focused() {
    let fixture = web_fixture();
    let meta = ev("Meta", "meta").modifiers();
    let hints = |flags: &[(&'static str, bool)]| {
        should_show_thread_jump_hints_for_modifiers(&meta, &fixture, &ctx(flags), MAC)
    };
    assert!(!hints(&[("terminalFocus", true)]));
    assert!(hints(&[("terminalFocus", false)]));
}

#[test]
fn keeps_default_thread_jumps_off_the_web() {
    let input = ev("1", "meta");
    assert_eq!(resolve(input, defaults(), &[("isDesktop", false)], MAC), None);
    assert_eq!(resolve(input, defaults(), &[("isDesktop", true)], MAC), Some("thread.jump.1"));
    let meta = input.modifiers();
    let hints = |flags: &[(&'static str, bool)]| {
        should_show_thread_jump_hints_for_modifiers(&meta, defaults(), &ctx(flags), MAC)
    };
    assert!(!hints(&[("isDesktop", false)]));
    assert!(hints(&[("isDesktop", true)]));
    assert!(hints(&[]));
}

#[test]
fn keeps_default_model_jumps_off_the_web_even_while_the_picker_is_open() {
    let input = ev("3", "meta");
    assert_eq!(resolve(input, defaults(), &[("isDesktop", false), ("modelPickerOpen", true)], MAC), None);
    assert_eq!(
        resolve(input, defaults(), &[("isDesktop", true), ("modelPickerOpen", true)], MAC),
        Some("modelPicker.jump.3")
    );
    assert_eq!(
        resolve(input, defaults(), &[("isDesktop", true), ("modelPickerOpen", false)], MAC),
        Some("thread.jump.3")
    );
}

#[test]
fn toggles_the_thread_panel_by_default_without_taking_terminal_keystrokes() {
    for mac in [true, false] {
        let input = ev("b", if mac { "meta+shift" } else { "ctrl+shift" });
        let flags = [("terminalFocus", false), ("editableFocus", true)];
        assert_eq!(resolve(input, defaults(), &flags, mac), Some("threadPanel.toggle"));
        assert_eq!(resolve(input, defaults(), &[("terminalFocus", true)], mac), None);
    }
}

#[test]
fn preserves_a_user_configured_thread_panel_shortcut_instead_of_its_default() {
    let bindings = merge_with_default_keybindings(&compile(&[("mod+alt+shift+b", "threadPanel.toggle", "")]));
    assert_eq!(resolve(ev("b", "meta+alt+shift"), &bindings, &[], MAC), Some("threadPanel.toggle"));
    assert_eq!(resolve(ev("b", "meta+shift"), &bindings, &[], MAC), None);
}

#[test]
fn a_later_focused_rule_overrides_a_global_rule() {
    let keybindings = compile(&[("mod+n", "chat.new", ""), ("mod+n", "terminal.new", "terminalFocus")]);
    assert_eq!(resolve(ev("n", "meta"), &keybindings, &[("terminalFocus", true)], MAC), Some("terminal.new"));
    assert_eq!(resolve(ev("n", "meta"), &keybindings, &[("terminalFocus", false)], MAC), Some("chat.new"));
}

#[test]
fn a_later_global_rule_still_wins_when_both_rules_match() {
    let keybindings = compile(&[("mod+n", "terminal.new", "terminalFocus"), ("mod+n", "chat.new", "")]);
    assert_eq!(resolve(ev("n", "ctrl"), &keybindings, &[("terminalFocus", true)], LINUX), Some("chat.new"));
}

fn chat<'a>(
    event: ShortcutEvent,
    primary: &'a [ResolvedKeybindingRule],
    active: &'a [ResolvedKeybindingRule],
    flags: &[(&'static str, bool)],
) -> Option<&'a str> {
    resolve_chat_shortcut_command(&event, primary, active, &ctx(flags), LINUX)
}

#[test]
fn chat_runs_the_active_environments_script_shortcut_instead_of_the_primarys() {
    let primary = compile(&[("mod+r", "script.setup.run", "")]);
    let active = compile(&[("mod+t", "script.setup.run", "")]);
    assert_eq!(chat(ev("t", "ctrl"), &primary, &active, &[]), Some("script.setup.run"));
    assert_eq!(chat(ev("r", "ctrl"), &primary, &active, &[]), None);
    assert_eq!(chat(ev("r", "ctrl"), &primary, &[], &[]), None);
}

#[test]
fn chat_keeps_primary_app_commands_ahead_of_active_scripts_and_remaps() {
    let primary = compile(&[("mod+k", "commandPalette.toggle", "")]);
    let active = compile(&[("mod+k", "script.setup.run", "")]);
    assert_eq!(chat(ev("k", "ctrl"), &primary, &active, &[]), Some("commandPalette.toggle"));

    let primary = compile(&[("mod+j", "terminal.toggle", "")]);
    let active = compile(&[("mod+t", "terminal.toggle", "")]);
    assert_eq!(chat(ev("j", "ctrl"), &primary, &active, &[]), Some("terminal.toggle"));
    assert_eq!(chat(ev("t", "ctrl"), &primary, &active, &[]), None);
}

#[test]
fn chat_preserves_active_binding_precedence() {
    let primary = compile(&[("mod+r", "script.setup.run", "")]);
    let active = compile(&[("mod+r", "script.deploy.run", ""), ("mod+r", "terminal.new", "terminalFocus")]);
    for terminal_focus in [false, true] {
        let flags = [("terminalFocus", terminal_focus)];
        let input = ev("r", "ctrl");
        let expected = if terminal_focus { None } else { Some("script.deploy.run") };
        assert_eq!(chat(input, &primary, &active, &flags), expected);
        assert_eq!(chat(input, &active, &active, &flags), resolve(input, &active, &flags, LINUX));
    }
}

#[test]
fn routes_mod_w_to_the_terminal_while_focused_and_to_the_right_panel_otherwise() {
    let fixture = web_fixture();
    let close = ev("w", "meta");
    assert_eq!(resolve(close, &fixture, &[("terminalFocus", true)], MAC), Some("terminal.close"));
    assert_eq!(resolve(close, &fixture, &[("terminalFocus", false)], MAC), Some("rightPanel.close"));
}

#[test]
fn navigates_history_with_mod_brackets_outside_the_terminal() {
    let back = ShortcutEvent { code: Some("BracketLeft"), ..ev("[", "meta") };
    let forward = ShortcutEvent { code: Some("BracketRight"), ..ev("]", "ctrl") };
    assert_eq!(resolve(back, defaults(), &[], MAC), Some("navigation.back"));
    assert_eq!(resolve(forward, defaults(), &[], LINUX), Some("navigation.forward"));
    assert_eq!(resolve(back, defaults(), &[("terminalFocus", true)], MAC), None);
}

#[test]
fn matches_punctuation_shortcuts_by_physical_key_across_layouts() {
    let keybindings = compile(&[("mod+shift+'", "diff.toggle", "")]);
    let quote_position = ShortcutEvent { code: Some("Quote"), ..ev("@", "meta+shift") };
    let digit_position = ShortcutEvent { code: Some("Digit2"), ..ev("\"", "meta+shift") };
    assert_eq!(resolve(quote_position, &keybindings, &[], MAC), Some("diff.toggle"));
    assert_eq!(resolve(digit_position, &keybindings, &[], MAC), None);
}

#[test]
fn a_punctuation_position_does_not_shadow_a_latin_layout_key() {
    let keybindings = compile(&[("mod+m", "diff.toggle", ""), ("mod+;", "sidebar.toggle", "")]);
    let input = ShortcutEvent { code: Some("Semicolon"), ..ev("m", "meta") };
    assert_eq!(resolve(input, &keybindings, &[], MAC), Some("diff.toggle"));
}

#[test]
fn matches_option_modified_and_non_latin_letters_by_physical_code() {
    let fixture = web_fixture();
    let option_b = ShortcutEvent { code: Some("KeyB"), ..ev("∫", "meta+alt") };
    assert_eq!(resolve(option_b, &fixture, &[], MAC), Some("rightPanel.toggle"));

    let keybindings = compile(&[("mod+d", "diff.toggle", "")]);
    let cyrillic_d = ShortcutEvent { code: Some("KeyD"), ..ev("в", "meta") };
    assert_eq!(resolve(cyrillic_d, &keybindings, &[], MAC), Some("diff.toggle"));

    let cyrillic_b = ShortcutEvent { code: Some("KeyB"), ..ev("и", "ctrl") };
    assert_eq!(resolve(cyrillic_b, &fixture, &[], WIN), Some("sidebar.toggle"));
}

#[test]
fn ignores_the_physical_code_when_the_layout_types_a_different_latin_letter() {
    let keybindings = compile(&[("mod+d", "diff.toggle", "")]);
    let a_on_d = ShortcutEvent { code: Some("KeyD"), ..ev("a", "meta") };
    let d_on_l = ShortcutEvent { code: Some("KeyL"), ..ev("d", "meta") };
    assert_eq!(resolve(a_on_d, &keybindings, &[], MAC), None);
    assert_eq!(resolve(d_on_l, &keybindings, &[], MAC), Some("diff.toggle"));
}

#[test]
fn fills_missing_number_shortcuts_without_replacing_the_saved_url_binding() {
    let mut custom: Vec<_> = defaults()
        .iter()
        .filter(|binding| binding.command != "pullRequest.copyNumber" && binding.command != "thread.copyReference")
        .cloned()
        .collect();
    custom.extend(compile_resolved_keybindings_config(&[KeybindingRule {
        key: "mod+shift+8".into(),
        command: "thread.copyReference".into(),
        when: Some("!terminalFocus".into()),
    }]));
    let bindings = merge_with_default_keybindings(&custom);
    for (key, command) in
        [("k", Some("pullRequest.copyNumber")), ("8", Some("thread.copyReference")), ("c", None), ("y", None)]
    {
        assert_eq!(resolve(ev(key, "meta+shift"), &bindings, &[], MAC), command, "key {key}");
    }
}

#[test]
fn reserves_the_alternate_send_chord_for_the_focused_composer() {
    for mac in [true, false] {
        let shifted_enter = ev("Enter", if mac { "meta+shift" } else { "ctrl+shift" });
        for requires_modifier in [false, true] {
            let flags =
                [("composerFocus", true), ("turnRunning", true), ("composerRequiresModifier", requires_modifier)];
            let expected = if requires_modifier { "composer.sendAlternate" } else { "thread.steerQueuedMessage" };
            assert_eq!(resolve(shifted_enter, defaults(), &flags, mac), Some(expected));
            let label = shortcut_label_for_command(defaults(), "composer.sendAlternate", &ctx(&flags), mac);
            let shortcut = if requires_modifier { sc("mod+shift+enter") } else { sc("mod+enter") };
            assert_eq!(label, Some(format_shortcut_label(&shortcut, mac)));
            let mut unfocused = flags;
            unfocused[0].1 = false;
            assert_eq!(resolve(shifted_enter, defaults(), &unfocused, mac), Some("thread.steerQueuedMessage"));
        }
    }
}

fn alt_effort_bindings() -> Vec<ResolvedKeybindingRule> {
    compile(&[("mod+alt+e", "composer.effort", "!terminalFocus")])
}

#[test]
fn leaves_altgr_text_entry_alone_with_a_custom_alt_binding() {
    let bindings = alt_effort_bindings();
    let altgr = ShortcutEvent { code: Some("KeyE"), alt_graph: true, ..ev("€", "ctrl+alt") };
    for mac in [WIN, LINUX] {
        assert_eq!(resolve(altgr, &bindings, &[], mac), None);
        let plain = ShortcutEvent { alt_graph: false, ..altgr };
        assert_eq!(resolve(plain, &bindings, &[], mac), Some("composer.effort"));
    }
}

#[test]
fn keeps_firefox_modifier_reporting_usable_on_windows_and_macos() {
    let bindings = alt_effort_bindings();
    let windows = ShortcutEvent { alt_graph: true, ..ev("e", "ctrl+alt") };
    assert_eq!(resolve(windows, &bindings, &[], WIN), Some("composer.effort"));
    let mac = ShortcutEvent { code: Some("KeyE"), alt_graph: true, ..ev("´", "meta+alt") };
    assert_eq!(resolve(mac, &bindings, &[], MAC), Some("composer.effort"));
}

#[test]
fn preserves_desktop_numbered_thread_shortcuts_on_usage() {
    for mac in [false, true] {
        let input = ev("2", if mac { "meta" } else { "ctrl" });
        let flags = [("usagePageOpen", true), ("isDesktop", true)];
        assert_eq!(resolve(input, defaults(), &flags, mac), Some("thread.jump.2"));
        assert!(shortcut_label_for_command(defaults(), "thread.jump.2", &ctx(&flags), mac).is_some());
    }
}

#[test]
fn matches_shifted_number_keys_for_usage_periods_only_on_usage() {
    let input = ShortcutEvent { code: Some("Digit1"), ..ev("!", "ctrl+shift") };
    assert_eq!(resolve(input, defaults(), &[("usagePageOpen", true)], LINUX), Some("usage.period.day"));
    assert_eq!(resolve(input, defaults(), &[], LINUX), None);
}

#[test]
fn parses_shortcuts_including_plus_key() {
    let plain = |key: &str| KeybindingShortcut {
        key: key.to_owned(),
        meta_key: false,
        ctrl_key: false,
        shift_key: false,
        alt_key: false,
        mod_key: false,
    };
    assert_eq!(parse_keybinding_shortcut("mod+j"), Some(KeybindingShortcut { mod_key: true, ..plain("j") }));
    assert_eq!(parse_keybinding_shortcut("mod++"), Some(KeybindingShortcut { mod_key: true, ..plain("+") }));
    assert_eq!(parse_keybinding_shortcut("+"), Some(plain("+")));
    assert_eq!(parse_keybinding_shortcut("space"), Some(plain(" ")));
    assert_eq!(parse_keybinding_shortcut("Esc"), Some(plain("escape")));
    assert_eq!(
        parse_keybinding_shortcut(" Shift + Cmd + R "),
        Some(KeybindingShortcut { shift_key: true, meta_key: true, ..plain("r") })
    );
    assert_eq!(
        parse_keybinding_shortcut("control+option+meta+x"),
        Some(KeybindingShortcut { ctrl_key: true, alt_key: true, meta_key: true, ..plain("x") })
    );
    assert_eq!(parse_keybinding_shortcut("mod+shift+d+o"), None);
    assert_eq!(parse_keybinding_shortcut("a++b"), None);
    assert_eq!(parse_keybinding_shortcut("mod+shift"), None);
}

#[test]
fn compiles_valid_rule_with_parsed_when_ast() {
    let compiled = compile_resolved_keybinding_rule(&KeybindingRule {
        key: "mod+d".into(),
        command: "terminal.split".into(),
        when: Some("terminalOpen && !terminalFocus".into()),
    });
    assert_eq!(
        compiled,
        Some(ResolvedKeybindingRule {
            command: "terminal.split".into(),
            shortcut: sc("mod+d"),
            when_ast: Some(and(ident("terminalOpen"), not(ident("terminalFocus")))),
        })
    );
}

#[test]
fn rejects_invalid_rules() {
    let rule =
        |key: &str, when: Option<String>| KeybindingRule { key: key.into(), command: "terminal.new".into(), when };
    assert_eq!(compile_resolved_keybinding_rule(&rule("mod+shift+d+o", None)), None);
    assert_eq!(compile_resolved_keybinding_rule(&rule("mod+d", Some("terminalFocus && (".into()))), None);
    let many_nots = format!("{}terminalFocus", "!".repeat(300));
    assert_eq!(compile_resolved_keybinding_rule(&rule("mod+d", Some(many_nots))), None);
}

#[test]
fn parses_when_expressions_with_precedence_and_parentheses() {
    let parse = |expression| parse_keybinding_when_expression(expression);
    assert_eq!(parse("a || b && !c"), Some(or(ident("a"), and(ident("b"), not(ident("c"))))));
    assert_eq!(parse("(a || b) && c"), Some(and(or(ident("a"), ident("b")), ident("c"))));
    assert_eq!(parse("a && b && c"), Some(and(and(ident("a"), ident("b")), ident("c"))));
    assert_eq!(parse("terminalFocus&&!previewFocus"), parse("terminalFocus && !previewFocus"));
    assert_eq!(parse("!!x.y-z_1"), Some(not(not(ident("x.y-z_1")))));
}

#[test]
fn limits_when_expression_depth() {
    let nested = |depth: usize| format!("{}a{}", "(".repeat(depth), ")".repeat(depth));
    assert!(parse_keybinding_when_expression(&nested(MAX_WHEN_EXPRESSION_DEPTH)).is_some());
    assert_eq!(parse_keybinding_when_expression(&nested(MAX_WHEN_EXPRESSION_DEPTH + 1)), None);
    let nots = |count: usize| format!("{}a", "!".repeat(count));
    assert!(parse_keybinding_when_expression(&nots(MAX_WHEN_EXPRESSION_DEPTH)).is_some());
    assert_eq!(parse_keybinding_when_expression(&nots(MAX_WHEN_EXPRESSION_DEPTH + 1)), None);
}

#[test]
fn rejects_invalid_when_expressions() {
    for expression in [
        "",
        "   ",
        "a &&",
        "&& a",
        "a & b",
        "a | b",
        "(a",
        "a)",
        "a b",
        "1a",
        "a || || b",
        "!",
        "()",
        "a == b",
        "ä",
    ] {
        assert_eq!(parse_keybinding_when_expression(expression), None, "{expression:?}");
    }
}

#[test]
fn literal_true_and_false_identifiers_ignore_context() {
    let keybindings = compile(&[("mod+k", "chat.find", "true"), ("mod+k", "diff.toggle", "false")]);
    let context = [("true", false), ("false", true)];
    assert_eq!(resolve(ev("k", "meta"), &keybindings, &context, MAC), Some("chat.find"));
}

#[test]
fn builds_conflict_keys_with_mod_resolved_per_platform() {
    assert_eq!(shortcut_conflict_key(&sc("mod+shift+k"), MAC), "k|meta||shift|");
    assert_eq!(shortcut_conflict_key(&sc("mod+shift+k"), LINUX), "k||ctrl|shift|");
    assert_eq!(shortcut_conflict_key(&sc("cmd+k"), MAC), shortcut_conflict_key(&sc("mod+k"), MAC));
    assert_eq!(shortcut_conflict_key(&sc("alt+arrowup"), LINUX), "arrowup||||alt");
}

#[test]
fn formats_labels_on_mac_and_elsewhere() {
    assert_eq!(format_shortcut_label(&sc("ctrl+alt+shift+meta+k"), MAC), "⌃⌥⇧⌘K");
    assert_eq!(format_shortcut_label(&sc("ctrl+alt+shift+meta+k"), LINUX), "Ctrl+Alt+Shift+Meta+K");
    assert_eq!(format_shortcut_label(&sc("mod+shift+["), MAC), "⇧⌘[");
    assert_eq!(format_shortcut_label(&sc("mod+shift+["), WIN), "Ctrl+Shift+[");
    assert_eq!(format_shortcut_label(&sc("f6"), WIN), "F6");
    for (key, label) in [
        (" ", "Space"),
        ("escape", "Esc"),
        ("arrowup", "Up"),
        ("arrowdown", "Down"),
        ("arrowleft", "Left"),
        ("arrowright", "Right"),
        ("enter", "Enter"),
        ("в", "В"),
    ] {
        assert_eq!(format_shortcut_key_label(key), label);
    }
}

#[test]
fn maps_jump_and_traversal_commands() {
    assert_eq!(thread_jump_command_for_index(0), Some("thread.jump.1"));
    assert_eq!(thread_jump_command_for_index(8), Some("thread.jump.9"));
    assert_eq!(thread_jump_command_for_index(9), None);
    assert_eq!(thread_jump_index_from_command("thread.jump.9"), Some(8));
    assert_eq!(thread_jump_index_from_command("thread.jump.10"), None);
    assert_eq!(model_picker_jump_command_for_index(2), Some("modelPicker.jump.3"));
    assert_eq!(model_picker_jump_index_from_command("modelPicker.jump.3"), Some(2));
    assert_eq!(model_picker_jump_index_from_command("thread.jump.3"), None);
    assert_eq!(
        thread_traversal_direction_from_command("thread.previous"),
        Some(ThreadTraversalDirection::Previous)
    );
    assert_eq!(thread_traversal_direction_from_command("thread.next"), Some(ThreadTraversalDirection::Next));
    assert_eq!(thread_traversal_direction_from_command("thread.jump.1"), None);
}

#[test]
fn defaults_compile_with_known_commands() {
    let raw = default_keybindings();
    assert_eq!(defaults().len(), raw.len());
    assert!(raw.iter().all(|rule| is_keybinding_command(&rule.command)));
    assert_eq!(raw[0], KeybindingRule { key: "mod+b".into(), command: "sidebar.toggle".into(), when: None });
    let jump = raw.iter().find(|rule| rule.command == "thread.jump.9").unwrap();
    assert_eq!((jump.key.as_str(), jump.when.as_deref()), ("mod+9", Some("!modelPickerOpen && isDesktop")));
    assert_eq!(raw.last().map(|rule| rule.command.as_str()), Some("usage.period.quarter"));
    for command in THREAD_JUMP_KEYBINDING_COMMANDS.iter().chain(&MODEL_PICKER_JUMP_KEYBINDING_COMMANDS) {
        assert!(STATIC_KEYBINDING_COMMANDS.contains(command));
    }
}

#[test]
fn validates_script_commands() {
    assert!(is_keybinding_command("script.run-tests.run"));
    assert!(is_keybinding_command("script.0.run"));
    assert!(is_keybinding_command(&format!("script.{}.run", "a".repeat(MAX_SCRIPT_ID_LENGTH))));
    let too_long = format!("script.{}.run", "a".repeat(MAX_SCRIPT_ID_LENGTH + 1));
    for command in [
        "script..run",
        "script.-a.run",
        "script.Setup.run",
        "script.a.b.run",
        "script.a.start",
        "invalid.command",
        too_long.as_str(),
    ] {
        assert!(!is_keybinding_command(command), "{command}");
    }
    assert_eq!(project_script_id_from_command(" script.setup.run "), Some("setup"));
    assert_eq!(project_script_id_from_command("terminal.toggle"), None);
}

#[test]
fn merges_custom_rules_over_defaults() {
    assert_eq!(merge_with_default_keybindings(&[]), defaults());

    let custom = compile(&[("mod+shift+y", "terminal.toggle", "")]);
    let merged = merge_with_default_keybindings(&custom);
    assert_eq!(merged.len(), defaults().len());
    assert_eq!(merged.last(), custom.last());
    assert_eq!(merged.iter().filter(|binding| binding.command == "terminal.toggle").count(), 1);

    let fillers: Vec<_> = (0..MAX_KEYBINDINGS_COUNT)
        .map(|index| ResolvedKeybindingRule {
            command: format!("script.filler-{index}.run"),
            shortcut: sc("mod+alt+f1"),
            when_ast: None,
        })
        .collect();
    assert_eq!(merge_with_default_keybindings(&fillers), fillers);
}

#[test]
fn compiling_keeps_the_last_rules_past_the_limit() {
    let rules: Vec<_> = (0..MAX_KEYBINDINGS_COUNT + 2)
        .map(|index| KeybindingRule { key: "mod+x".into(), command: format!("script.s{index}.run"), when: None })
        .collect();
    let compiled = compile_resolved_keybindings_config(&rules);
    assert_eq!(compiled.len(), MAX_KEYBINDINGS_COUNT);
    assert_eq!(compiled[0].command, "script.s2.run");
}

#[test]
fn decodes_wire_rules_and_skips_entries_it_cannot_decode() {
    let shortcut = json!({ "key": "d", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true });
    let wire = json!([
        { "command": "terminal.split", "shortcut": shortcut, "whenAst": {
            "type": "and",
            "left": { "type": "identifier", "name": "terminalOpen" },
            "right": { "type": "not", "node": { "type": "identifier", "name": "terminalFocus" } }
        }, "futureField": 1 },
        { "command": "command.from-a-newer-build", "shortcut": shortcut },
        { "command": "diff.toggle", "shortcut": { "key": "d" } },
        { "command": "diff.toggle", "shortcut": shortcut, "whenAst": { "type": "xor", "left": {}, "right": {} } },
        { "command": "diff.toggle", "shortcut": shortcut, "whenAst": { "type": "identifier", "name": "" } },
        { "command": "diff.toggle", "shortcut": { "key": "   ", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true } },
        { "command": "diff.toggle", "shortcut": { "key": "x".repeat(65), "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true } },
        "not an object",
        { "command": "script.run-tests.run", "shortcut": { "key": " r ", "metaKey": true, "ctrlKey": false, "shiftKey": true, "altKey": false, "modKey": false } },
    ]);
    let decoded = decode_resolved_keybindings(&wire);
    assert_eq!(
        decoded,
        vec![
            ResolvedKeybindingRule {
                command: "terminal.split".into(),
                shortcut: sc("mod+d"),
                when_ast: Some(and(ident("terminalOpen"), not(ident("terminalFocus")))),
            },
            ResolvedKeybindingRule {
                command: "script.run-tests.run".into(),
                shortcut: sc("meta+shift+r"),
                when_ast: None
            },
        ]
    );
    assert!(decode_resolved_keybindings(&json!({ "not": "an array" })).is_empty());
}

#[test]
fn decoding_keeps_the_last_rules_past_the_limit() {
    let shortcut = json!({ "key": "x", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true });
    let wire: Vec<_> = (0..MAX_KEYBINDINGS_COUNT + 1)
        .map(|index| json!({ "command": format!("script.s{index}.run"), "shortcut": shortcut }))
        .collect();
    let decoded = decode_resolved_keybindings(&Value::Array(wire));
    assert_eq!(decoded.len(), MAX_KEYBINDINGS_COUNT);
    assert_eq!(decoded[0].command, "script.s1.run");
}

#[test]
fn translates_shortcuts_to_gpui_keystrokes() {
    let cases: &[(&str, bool, &[&str])] = &[
        ("mod+shift+[", MAC, &["cmd-shift-[", "cmd-{"]),
        ("mod+shift+[", LINUX, &["ctrl-shift-["]),
        ("mod+shift+1", MAC, &["cmd-shift-1", "cmd-!"]),
        ("mod+shift+p", MAC, &["cmd-shift-p"]),
        ("mod+k", LINUX, &["ctrl-k"]),
        ("mod+k", MAC, &["cmd-k"]),
        ("meta+k", LINUX, &["cmd-k"]),
        ("alt+arrowup", MAC, &["alt-up"]),
        ("mod+shift+arrowdown", WIN, &["ctrl-shift-down"]),
        ("ctrl+alt+shift+cmd+arrowleft", MAC, &["ctrl-alt-cmd-shift-left"]),
        ("mod+alt+shift+t", MAC, &["alt-cmd-shift-t"]),
        ("mod+-", MAC, &["cmd--"]),
        ("mod++", MAC, &["cmd-+"]),
        ("mod+=", LINUX, &["ctrl-="]),
        ("space", MAC, &["space"]),
        ("esc", MAC, &["escape"]),
        ("mod+enter", MAC, &["cmd-enter"]),
        ("shift+f6", MAC, &["shift-f6"]),
    ];
    for &(shortcut, mac, expected) in cases {
        assert_eq!(gpui_keystrokes(&sc(shortcut), mac), expected, "{shortcut} mac={mac}");
    }
}

#[test]
fn translates_when_clauses_to_gpui_predicates() {
    let predicate = |expression| gpui_predicate(&parse_keybinding_when_expression(expression).unwrap());
    assert_eq!(predicate("!terminalFocus && isDesktop"), "(!(!(!terminalFocus)) && !(!isDesktop))");
    assert_eq!(predicate("a || !(b && c)"), "(!(!a) || !((!(!b) && !(!c))))");
    assert_eq!(predicate("true"), "!__t3_never__");
    assert_eq!(predicate("false"), "!(!__t3_never__)");
}
