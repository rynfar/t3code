use super::*;
use serde_json::json;
#[test]
fn imports_validate_and_fill_both_halves_from_t3_chat() {
    let theme=decode(&json!({"version":1,"name":"Night sky","appearance":"dark","colors":{"canvas":"#1234","text":"oklch(0.9 0.02 270)"},"variants":{"light":{"canvas":"white"}}}),false).unwrap();
    assert_eq!(theme.id, "night-sky");
    assert_eq!(theme.colors["canvas"], "#1234");
    assert_eq!(theme.colors["border"], defaults(true)["border"]);
    assert_eq!(theme.half(false).unwrap()["border"], defaults(false)["border"]);
    for literal in [
        "color(display-p3 0.8 0.2 0.3 / 50%)",
        "color(srgb 0.1 0.2 0.3)",
        "color(rec2020 0.2 0.4 0.6)",
        "color(a98-rgb 0.2 0.4 0.6)",
        "color(prophoto-rgb 0.2 0.4 0.6)",
        "color(xyz-d50 0.2 0.3 0.4)",
        "color(xyz-d65 0.2 0.3 0.4)",
        "color(srgb-linear 0.2 0.3 0.4)",
    ] {
        let file = json!({"version":1,"name":"Wide gamut","appearance":"light","colors":{"canvas":literal}});
        let imported = decode(&file, false).unwrap();
        let published = environment(&[json!({"id":"wide-gamut","name":"Wide gamut","appearance":"light","colors":{"canvas":literal}})]);
        assert_eq!(published[0].colors["canvas"], imported.colors["canvas"]);
        palette(Some(&imported.colors), false, 100, 80).unwrap();
    }
    for (literal, expected) in [
        ("color(display-p3 0.8 0.2 0.3 / 50%)", 0xde184980),
        ("color(rec2020 0.2 0.4 0.6)", 0x0078a8ff),
        ("color(a98-rgb 0.2 0.4 0.6)", 0x00669cff),
        ("color(prophoto-rgb 0.2 0.4 0.6)", 0x0082b0ff),
        ("color(xyz-d50 0.2 0.3 0.4)", 0x00a8bdff),
        ("color(xyz-d65 0.2 0.3 0.4)", 0x00a7a4ff),
        ("color(srgb-linear 0.2 0.3 0.4)", 0x7c95aaff),
    ] {
        assert_eq!(color::packed(color::parse(literal).unwrap()), expected, "{literal}");
    }
    for invalid in [
        json!({"version":2}),
        json!({"version":1,"name":"X","id":"light","appearance":"dark","colors":{"canvas":"black"}}),
        json!({"version":1,"name":"X","appearance":"dark","colors":{"wrong":"red"}}),
        json!({"version":1,"name":"X","appearance":"dark","colors":{"canvas":"var(--foo)"}}),
        json!({"version":1,"name":"X","appearance":"dark","colors":{"canvas":"black"},"variants":{"dark":{"canvas":"white"}}}),
    ] {
        assert!(decode(&invalid, false).is_err());
    }
}
#[test]
fn contrast_and_glass_follow_css_and_preserve_alpha() {
    let normal = palette(None, true, 100, 80).unwrap();
    let high = palette(None, true, 200, 100).unwrap();
    let low = palette(None, true, 50, 0).unwrap();
    assert_eq!(normal.get("composer_surface"), Some(&0x111111cc));
    let mut roles = defaults(false);
    roles.insert("surface".into(), "#ffffff".into());
    roles.insert("surfaceRaised".into(), "#335577".into());
    roles.insert("toolbarBorder".into(), "#448866".into());
    let custom = palette(Some(&roles), false, 100, 80).unwrap();
    assert_eq!(custom.get("composer_surface"), Some(&0x335577cc));
    assert_eq!(custom.get("composer_outline"), Some(&0x448866ff));
    assert_eq!(high["foreground"], 0xffffffff);
    assert_eq!(low["glass_surface"] & 255, 102);
    assert_eq!(normal["glass_surface"] & 255, 204);
    assert_eq!(high["glass_surface"] & 255, 255);
    assert!(high["border"] & 255 > normal["border"] & 255);
    assert_eq!(low["border"] & 255, 10);
    assert_eq!(normal["foreground"], 0xf5f5f5ff);
    assert_ne!(low["foreground"], normal["foreground"]);
    for t in builtins() {
        for dark in [false, true] {
            palette(t.half(dark), dark, 50, 0).unwrap();
            palette(t.half(dark), dark, 200, 100).unwrap();
        }
    }
}
#[test]
fn library_preserves_unknown_entries_and_refuses_duplicate_ids() {
    let doc = ClientSettingsDoc::from_json(&json!({LIBRARY_KEY:[{"future":true}]}).to_string());
    let theme = decode(&json!({"version":1,"id":"night","name":"Night","appearance":"dark","colors":{"canvas":"#123"}}), false).unwrap();
    let installed = install(&doc, &theme).unwrap();
    assert_eq!(installed[0], json!({"future":true}));
    let doc = ClientSettingsDoc::from_json(&json!({LIBRARY_KEY:installed}).to_string());
    assert!(install(&doc, &theme).is_err());
    let copied = duplicate(&doc, &theme);
    assert_eq!(copied.label, "Night copy");
    assert_ne!(copied.id, theme.id);
    assert!(install(&doc, &copied).is_ok());
    assert!(install(&ClientSettingsDoc::from_json(&json!({LIBRARY_KEY:"broken"}).to_string()), &theme).is_err());
}
#[test]
fn environment_seeds_overrides_and_variants_are_lenient_and_not_persisted() {
    let themes = environment(&[
        json!({"id":"dark","name":"Bad","appearance":"dark","canvas":"#123","accent":"#abc"}),
        json!({"id":"office","name":"Office","appearance":"dark","canvas":"#123","accent":"#abc","colors":{"canvas":"#223344","future":"wat"},"variants":{"light":{"canvas":"white"}}}),
    ]);
    assert_eq!(themes.len(), 1);
    assert_eq!(themes[0].colors["canvas"], "#223344");
    assert_eq!(themes[0].half(false).unwrap()["canvas"], "white");
    assert_eq!(themes[0].half(false).unwrap()["border"], defaults(false)["border"]);
    assert_ne!(themes[0].colors["surface"], defaults(true)["surface"]);
}
#[test]
fn settings_clamp_defaults_and_keep_separate_halves() {
    use crate::client_settings::ClientSettingChange as C;
    let mut doc = ClientSettingsDoc::default();
    assert_eq!(doc.appearance_contrast(), 100);
    assert_eq!(doc.glass_opacity(), 80);
    doc.apply(&C::AppearanceContrast(250));
    doc.apply(&C::GlassOpacity(200));
    assert_eq!(doc.appearance_contrast(), 200);
    assert_eq!(doc.glass_opacity(), 100);
    doc.apply(&C::ThemeHalves(BTreeMap::from([("dark".into(), "office".into())])));
    assert_eq!(halves(&doc)["dark"], "office");
    doc.apply(&C::Theme(ThemePreference::Named("iris".into())));
    assert!(halves(&doc).is_empty());
}

#[test]
fn runtime_palettes_match_generated_web_css() {
    let pairs = std::iter::once(None).chain(builtins().iter().map(Some));
    for (pair, t) in pairs.enumerate() {
        for dark in [false, true] {
            let actual = palette_for(t.map(|t| t.id.as_str()), t.and_then(|t| t.half(dark)), dark, 100, 80).unwrap();
            let expected = &DATA.expected[pair * 2 + usize::from(dark)];
            for (key, a) in actual {
                let b = expected[&key];
                for shift in [0, 8, 16, 24] {
                    assert!(
                        ((a >> shift) & 255).abs_diff((b >> shift) & 255) <= 1,
                        "{} {dark} {key}: {a:08x} != {b:08x}",
                        t.map_or("default", |t| t.id.as_str())
                    );
                }
            }
        }
    }
}
#[test]
fn removal_and_default_half_preserve_the_other_raw_owner() {
    use crate::client_settings::ClientSettingChange as C;
    let mut doc = ClientSettingsDoc::from_json(
        &json!({"t3code:theme":"iris",HALVES_KEY:{"dark":"unstreamed"},LIBRARY_KEY:[{"id":"iris"},{"future":true}]}).to_string(),
    );
    let patch = assign_half(&doc, false, None);
    doc.apply(&C::ThemePatch(patch));
    assert_eq!(doc.theme(), ThemePreference::Default(AppearanceMode::Light));
    assert_eq!(halves(&doc)["dark"], "unstreamed");
    let patch = remove(&doc, &["iris".into()]).unwrap();
    doc.apply(&C::ThemePatch(patch));
    assert_eq!(halves(&doc)["dark"], "unstreamed");
    assert_eq!(doc.entries()[LIBRARY_KEY], json!([{"future":true}]));
}

#[test]
fn an_unavailable_half_falls_back_without_losing_its_stored_id() {
    let dark = decode(&json!({"version":1,"name":"Night","appearance":"dark","colors":{"canvas":"black"}}), false).unwrap();
    let library = vec![dark];
    let base = ThemePreference::Named("night".into());
    let halves = BTreeMap::from([("light".into(), "not-streamed".into())]);
    assert!(resolved_dark(&base, &halves, false, &library, &[]));
    let halves = BTreeMap::from([("light".into(), "iris".into())]);
    assert!(!resolved_dark(&base, &halves, false, &library, &[]));
    assert_eq!(half_theme(&base, &halves, false, &library, &[]).unwrap().id, "iris");
}

#[test]
fn environment_seed_palettes_match_web_derivation() {
    let fixtures: Vec<Value> = serde_json::from_str(include_str!("vivid-fixtures.json")).unwrap();
    for f in fixtures {
        let dark = f["appearance"] == "dark";
        let actual = vivid::derive(dark, f["canvas"].as_str().unwrap(), f["accent"].as_str().unwrap()).unwrap();
        for (role, expected) in f["colors"].as_object().unwrap() {
            let a = color::packed(color::parse(&actual[role]).unwrap());
            let b = color::packed(color::parse(expected.as_str().unwrap()).unwrap());
            for shift in [0, 8, 16, 24] {
                assert!(((a >> shift) & 255).abs_diff((b >> shift) & 255) <= 1, "{} {role}: {a:08x} != {b:08x}", f["canvas"]);
            }
        }
    }
}

#[test]
fn vscode_jsonc_import_flattens_overlays_and_keeps_controls_readable() {
    let theme = import(
        r##"// Workbench palette
    {"name":"office-night","type":"dark","colors":{
      "editor.background":"#102030","editor.foreground":"#eeeeee",
      "sideBar.background":"#ffffff80","focusBorder":"#77ccff",
      "button.background":"#102030","input.background":"#102030",
    },}"##,
    )
    .unwrap();
    let alpha = import(r##"{"name":"Alpha","colors":{"editor.background":"#000000","focusBorder":"#ff000080"}}"##).unwrap();
    let seed = vivid::derive(true, "#000000", "#330000").unwrap();
    assert_eq!(alpha.colors["sidebar"], seed["sidebar"]);
    let hidden=import(r##"{"name":"Hidden","colors":{"editor.background":"#000000","editorWidget.background":"#808080","focusBorder":"#808080","button.background":"#00ff00"}}"##).unwrap();
    assert_eq!(hidden.colors["accent"], "#00ff00");
    assert_eq!(theme.label, "Office Night");
    assert_eq!(theme.id, "office-night");
    assert_eq!(theme.appearance, "dark");
    assert_eq!(theme.colors["canvas"], "#102030");
    assert_eq!(theme.colors["sidebar"], "#889098");
    assert_eq!(theme.colors["messageAction"], "#77ccff");
    assert!(color::contrast(color::parse(&theme.colors["input"]).unwrap(), color::parse(&theme.colors["canvas"]).unwrap()) >= 1.1);
    assert!(color::contrast(color::parse(&theme.colors["text"]).unwrap(), color::parse(&theme.colors["canvas"]).unwrap()) >= 4.5);
    let p3 =
        import(r##"{"name":"dark","colors":{"editor.background":"#fff","focusBorder":"color(display-p3 0.2 0.4 0.8 / 50%)"}}"##).unwrap();
    assert_eq!(p3.id, "dark-vscode");
    assert_eq!(p3.appearance, "light");
    assert!(import(r##"{"tokenColors":[]}"##).unwrap_err().contains("editor.background"));
    assert!(import(r##"{"version":1,"name":"Strict","appearance":"dark","colors":{"canvas":"#123"},}"##).is_err());
}

#[test]
fn reimport_choices_preserve_selections_and_existing_collection() {
    use crate::client_settings::ClientSettingChange as C;
    let mut old = decode(
        &json!({"version":1,"name":"Night","appearance":"dark","colors":{"canvas":"#123"},"collection":{"id":"pkg","label":"Package"}}),
        false,
    )
    .unwrap();
    let mut doc = ClientSettingsDoc::from_json(
        &json!({LIBRARY_KEY:[old.clone(),{"future":true}],HALVES_KEY:{"dark":"night","light":"iris"},"t3code:theme":"ocean"}).to_string(),
    );
    old.collection = None;
    old.colors.insert("canvas".into(), "#456".into());
    let updated = resolve_import(&doc, &old, ImportChoice::Update).unwrap();
    doc.apply(&C::ThemeLibrary(updated));
    let saved = library(&doc);
    assert_eq!(saved[0].colors["canvas"], "#456");
    assert_eq!(saved[0].collection.as_ref().unwrap().id, "pkg");
    assert_eq!(halves(&doc), BTreeMap::from([("dark".into(), "night".into()), ("light".into(), "iris".into())]));
    assert_eq!(doc.theme().named_id(), Some("ocean"));
    assert_eq!(doc.entries()[LIBRARY_KEY][1], json!({"future":true}));
    let copied = resolve_import(&doc, &old, ImportChoice::KeepBoth).unwrap();
    doc.apply(&C::ThemeLibrary(copied));
    let copied = library(&doc);
    assert_eq!(copied.len(), 2);
    assert_eq!(copied[1].label, "Night (1)");
    assert_ne!(copied[0].id, copied[1].id);
    assert!(copied[1].collection.is_none());
    assert_eq!(halves(&doc)["dark"], "night");
}
#[test]
fn collection_use_assigns_first_variant_for_each_appearance_together() {
    use crate::client_settings::ClientSettingChange as C;
    let light = decode(&json!({"version":1,"name":"Day","appearance":"light","colors":{"canvas":"white"}}), false).unwrap();
    let dark = decode(&json!({"version":1,"name":"Night","appearance":"dark","colors":{"canvas":"black"}}), false).unwrap();
    let mut doc = ClientSettingsDoc::from_json(&json!({"t3code:theme":"ocean",HALVES_KEY:{"dark":"iris"}}).to_string());
    doc.apply(&C::ThemePatch(select_collection(&doc, &[light.clone(), dark])));
    assert_eq!(halves(&doc), BTreeMap::from([("light".into(), "day".into()), ("dark".into(), "night".into())]));
    assert_eq!(doc.theme().named_id(), Some("ocean"));
    doc.apply(&C::ThemePatch(select_collection(&doc, &[light])));
    assert_eq!(halves(&doc)["dark"], "night");
}
