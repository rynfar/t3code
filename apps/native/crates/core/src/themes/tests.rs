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

#[test]
fn editor_defaults_seeds_and_single_appearance_locks_match_web() {
    use editor::Session;
    let doc=ClientSettingsDoc::default();
    let s=Session::new(None,None,None,false);
    assert_eq!(s.colors(),&standard(false)); assert!(!s.advanced);assert_eq!(s.name,"");
    let mut imported=builtins()[0].clone(); imported.id="imported".into();imported.variants.clear();imported.managed=false;
    let s=Session::new(Some(imported.clone()),None,None,true);
    assert!(!s.dark);assert!(s.advanced);assert!(s.lock_reason(true,&doc).unwrap().contains("has no dark palette"));
    let mut duplicate=Session::new(None,Some(&imported),Some("Imported copy".into()),true);
    assert!(!duplicate.dark);duplicate.set_appearance(true,&doc);assert!(duplicate.dark);
    assert_eq!(duplicate.colors(),&standard(true));
}
#[test]
fn editor_guided_advanced_and_picker_cancel_restore_entire_draft() {
    use editor::{Session,picker};
    let mut s=Session::new(None,None,Some("Aurora".into()),false);
    let original=s.colors().clone();s.start_color_edit();s.update_color("canvas","#203040".into());
    assert_ne!(s.colors()["text"],original["text"]);s.end_color_edit(true);assert_eq!(s.colors(),&original);
    s.set_advanced(true);s.update_color("surfaceRaised","#1234".into());assert_eq!(picker::hex(&s.colors()["surfaceRaised"]).unwrap(),"#11223344");
    s.set_advanced(false);assert_ne!(s.colors()["surfaceRaised"],"#1234");
    s.set_appearance(true,&ClientSettingsDoc::default());assert_eq!(s.colors(),&standard(true));
    s.set_appearance(false,&ClientSettingsDoc::default());s.update_color("canvas","bad color".into());
    assert_eq!(s.colors()["canvas"],"bad color");
    assert!(s.prepare_save(&ClientSettingsDoc::default()).is_ok());
}
fn editor_doc(themes:&[ThemeDefinition])->ClientSettingsDoc { ClientSettingsDoc::from_json(&json!({LIBRARY_KEY:themes,crate::client_settings::THEME_KEY:"grove",HALVES_KEY:{"dark":"ocean"},"unrelated":42}).to_string()) }
fn editor_theme(name:&str,dark:bool)->ThemeDefinition {
    decode(&json!({"version":1,"name":name,"appearance":if dark{"dark"}else{"light"},"colors":standard(dark),"managed":true}),false).unwrap()
}
#[test]
fn editor_save_is_one_transaction_and_edit_preserves_mix_collection_and_unknown_entries() {
    use editor::Session;
    let mut theme=editor_theme("Aurora",false);theme.variants.insert("dark".into(),standard(true));theme.collection=Some(Collection{id:"pack".into(),label:"Pack".into()});
    let mut doc=editor_doc(std::slice::from_ref(&theme));
    doc.apply_change(&crate::client_settings::ClientSettingChange::ThemePatch(serde_json::Map::from_iter([(LIBRARY_KEY.into(),json!([theme,{"future":true}]))])));
    let mut s=Session::new(Some(theme.clone()),None,None,true);s.set_name("Renamed".into(),&doc);s.update_color("canvas","#102030".into());
    let save=s.prepare_save(&doc).unwrap();assert!(!save.created);assert_eq!(save.theme.id,theme.id);assert_eq!(save.theme.collection,theme.collection);
    assert_eq!(save.theme.appearance,"light");assert_eq!(save.theme.colors,theme.colors);assert!(save.theme.half(true).is_some());
    assert!(!save.patch.contains_key(HALVES_KEY));assert!(!save.patch.contains_key(crate::client_settings::THEME_KEY));
    assert_eq!(save.patch[LIBRARY_KEY][1],json!({"future":true}));
    let created=Session::new(None,Some(&theme),Some("Another".into()),true).prepare_save(&doc).unwrap();
    assert!(created.created);assert!(created.theme.variants.is_empty());assert_eq!(created.theme.appearance,"dark");assert_eq!(created.patch[HALVES_KEY],json!({}));
    assert_eq!(created.patch[crate::client_settings::THEME_KEY],"another");
}
#[test]
fn editor_names_merge_complementary_halves_and_retire_renamed_theme() {
    use editor::Session;
    let light=editor_theme("Aurora",false);let dark=editor_theme("Night",true);let doc=editor_doc(&[light.clone(),dark.clone()]);
    let mut s=Session::new(None,None,None,false);s.set_name(" AURORA ".into(),&doc);
    assert!(s.dark);assert!(s.lock_reason(false,&doc).unwrap().contains("already has a light palette"));
    let save=s.prepare_save(&doc).unwrap();assert_eq!(save.theme.id,light.id);assert_eq!(save.theme.colors,light.colors);assert_eq!(save.merged_dark,Some(true));assert_eq!(save.theme.half(true),Some(&standard(true)));
    let mut s=Session::new(Some(dark),None,None,true);s.set_name("Aurora".into(),&doc);
    let save=s.prepare_save(&doc).unwrap();assert_eq!(save.patch[LIBRARY_KEY].as_array().unwrap().len(),1);
    let mut s=Session::new(None,None,None,false);let merged_doc=editor_doc(&[save.theme]);s.set_name("Aurora".into(),&merged_doc);
    assert_eq!(s.prepare_save(&merged_doc).unwrap_err(),"“Aurora” already has light and dark palettes. Pick another name.");
    let mut edit=Session::new(Some(light.clone()),None,None,false);edit.set_name("Aurora".into(),&editor_doc(&[]));
    assert!(edit.prepare_save(&editor_doc(&[])).unwrap_err().contains("not installed"));
}
#[test]
fn editor_validates_name_colors_and_keeps_library_unchanged_until_commit() {
    use editor::Session;
    let doc=ClientSettingsDoc::default();let mut s=Session::new(None,None,None,false);
    assert_eq!(s.prepare_save(&doc).unwrap_err(),"Name your theme first.");
    s.set_name("x".repeat(49),&doc);assert!(s.prepare_save(&doc).unwrap_err().contains("48 characters"));
    s.set_name("grove".into(),&doc);assert!(s.prepare_save(&doc).unwrap_err().contains("reserved"));
    s.set_name("Valid".into(),&doc);s.set_advanced(true);s.update_color("border","var(--foo)".into());
    assert!(s.prepare_save(&doc).unwrap_err().contains("literal CSS color"));assert!(library(&doc).is_empty());
}
#[test]
fn editor_family_derives_linked_roles_readability_and_filter_aliases() {
    for dark in [false,true] {
        let original=standard(dark);
        for f in editor::FAMILIES {
            let colors=editor::update_family(dark,&original,f.role,"#734fa080");
            assert_ne!(colors[f.role],original[f.role]);assert_eq!(colors.len(),original.len());
            palette(Some(&colors),dark,100,80).unwrap();
        }
        let colors=editor::update_family(dark,&original,"text","red");assert_eq!(colors["text"],colors["toolbarControlForeground"]);
        let colors=editor::update_family(dark,&original,"codeBackground","#f8f8f8");
        assert!(color::contrast(color::parse(&colors["codeForeground"]).unwrap(),color::parse(&colors["codeBackground"]).unwrap())>=4.5);
    }
    assert!(editor::FAMILIES.iter().find(|f|f.role=="mutedForeground").unwrap().matches("sidebar muted foreground"));
    for (role,alias) in [("error","error text"),("warning","warning background"),("accent","accent color"),("canvas","toolbar background")] {
        assert!(editor::FAMILIES.iter().find(|f|f.role==role).unwrap().matches(alias),"{alias}");
    }
}
#[test]
fn editor_picker_rgb_hex_hsv_preserve_alpha_and_roundtrip() {
    use editor::picker::*;
    assert_eq!(from_rgb("rgb(1, 2, 255)","#1234"),Some("#0102ff44".into()));
    for invalid in ["-1 2 3","1 2 256","1.1 2 3","1 2","1 2 3 4"] {assert!(from_rgb(invalid,"red").is_none());}
    assert_eq!(from_hex("#ABCDEF","#1234"),Some("#abcdef44".into()));assert!(from_hex("#fff","red").is_none());
    for value in ["#000000","#ffffff","#ff0000","#abcd0180","#173f9a"] {assert_eq!(from_hsv(hsv(value),value),value);}
    assert!(hex("oklch(0.62 0.2 280 / 0.4)").unwrap().ends_with("66"));
    let red=hsv("red");assert_eq!(key(red,Axis::Hue,"left",false).unwrap().h,359.);
    assert_eq!(key(red,Axis::Hue,"end",false).unwrap().h,360.);
    assert_eq!(key(red,Axis::Saturation,"left",true).unwrap().s,0.9);
    assert_eq!(key(red,Axis::Brightness,"end",false).unwrap().v,1.);
    assert!(key(red,Axis::Hue,"escape",false).is_none());
}

#[test]
fn editor_families_match_web_color_fixtures() {
    #[derive(serde::Deserialize)]
    struct Fixture {dark:bool,role:String,value:String,#[serde(default)]background:Colors,expected:std::collections::BTreeMap<String,String>}
    let fixtures:Vec<Fixture>=serde_json::from_str(include_str!("editor/fixtures.json")).unwrap();
    for f in fixtures {
        let mut input=standard(f.dark);input.extend(f.background);
        let colors=editor::update_family(f.dark,&input,&f.role,&f.value);
        for (role,expected) in f.expected {
            let actual=editor::picker::swatch(&colors[&role]).unwrap();let expected=editor::picker::swatch(&expected).unwrap();
            for shift in [24,16,8,0] {assert!((((actual>>shift)&255) as i32-((expected>>shift)&255) as i32).abs()<=1,"{} {} dark={} input={} actual={} expected={expected:08x}",f.role,role,f.dark,f.value,colors[&role]);}
        }
    }
}

#[test]
fn editor_picker_drags_have_one_owner_and_frame_commit_keeps_latest() {
    use editor::picker::{Drag,Surface,Hsv,FrameCommit};
    let initial=Hsv{h:120.,s:0.4,v:0.5};let mut drag=Drag::default();
    assert!(drag.sample(initial,1.,1.).is_none());
    drag.start(Surface::SaturationValue);
    let next=drag.sample(initial,1.5,-0.2).unwrap();assert_eq!(next,Hsv{h:120.,s:1.,v:1.});
    assert_eq!(drag.surface(),Some(Surface::SaturationValue));
    drag.end();assert!(drag.sample(initial,0.,0.).is_none());
    drag.start(Surface::Hue);assert_eq!(drag.sample(initial,-1.,0.).unwrap().h,0.);
    let mut pending=FrameCommit::default();assert!(pending.schedule(initial));assert!(!pending.schedule(next));
    assert_eq!(pending.take(),Some(next));assert_eq!(pending.take(),None);assert!(pending.schedule(initial));
    assert_eq!(pending.take_if(false),None);assert_eq!(pending.take_if(true),None);
}
#[test]
fn editor_preview_ignores_name_and_invalid_color_changes() {
    let mut session=editor::Session::new(None,None,None,false);
    let mut preview=editor::Preview::new(session.dark,session.colors());
    session.set_name("Aurora".into(),&ClientSettingsDoc::default());
    assert!(!preview.update(session.dark,session.colors()));
    session.set_advanced(true);session.update_color("canvas","bad color".into());
    assert!(!preview.update(session.dark,session.colors()));
    session.update_color("canvas","#112233".into());assert!(preview.update(session.dark,session.colors()));
    assert!(!preview.update(session.dark,session.colors()));
    session.set_appearance(true,&ClientSettingsDoc::default());assert!(preview.update(session.dark,session.colors()));
}

#[test]
fn theme_css_round_trips_validates_length_and_applies_to_palettes() {
    let raw_css = ":root { --background: #102030; } .dark { --app-theme-canvas: #050607; --primary: #abcdef; }";
    let theme = decode(
        &json!({
            "version": 1,
            "name": "Styled",
            "appearance": "dark",
            "colors": {"canvas": "#111111"},
            "css": raw_css
        }),
        false,
    )
    .unwrap();
    assert_eq!(theme.css.as_deref(), Some(raw_css));

    let exported = serialize(&theme);
    let reimported = import(&exported).unwrap();
    assert_eq!(reimported.css.as_deref(), Some(raw_css));

    let oversized = "a".repeat(css::MAX_CSS_LEN + 1);
    assert!(
        decode(
            &json!({"version": 1, "name": "TooLong", "appearance": "dark", "colors": {"canvas": "#111111"}, "css": oversized}),
            false
        )
        .is_err()
    );

    let env = environment(&[json!({
        "id": "env-styled",
        "name": "Env Styled",
        "appearance": "dark",
        "canvas": "#111111",
        "accent": "#2266aa",
        "css": raw_css
    })]);
    assert_eq!(env.len(), 1);
    assert!(env[0].managed);
    assert_eq!(env[0].css.as_deref(), Some(raw_css));

    let dark_pal = palette_for_with_css(
        Some(&theme.id),
        Some(&theme.colors),
        theme.css.as_deref(),
        true,
        100,
        80,
    )
    .unwrap();
    assert_eq!(dark_pal["background"], 0x102030ff);
    assert_eq!(dark_pal["primary"], 0xabcdefff);
}

#[test]
fn editor_css_merges_and_detects_conflicts_matching_web() {
    let mut doc = ClientSettingsDoc::default();
    let mut create = editor::Session::new(None, None, None, false);
    create.set_name("Aurora".into(), &doc);
    create.set_css(":root { --primary: #112233; }".into());
    let saved = create.prepare_save(&doc).unwrap();
    assert_eq!(saved.theme.css.as_deref(), Some(":root { --primary: #112233; }"));
    doc.apply(&crate::client_settings::ClientSettingChange::ThemePatch(saved.patch));

    // Merging with empty draft CSS keeps the target's CSS.
    let mut merge_keep = editor::Session::new(None, None, None, true);
    merge_keep.set_name("Aurora".into(), &doc);
    assert_eq!(merge_keep.effective_css(&doc), ":root { --primary: #112233; }");
    let merged = merge_keep.prepare_save(&doc).unwrap();
    assert_eq!(merged.theme.css.as_deref(), Some(":root { --primary: #112233; }"));

    // Merging with conflicting draft CSS fails with the web's message.
    let mut merge_conflict = editor::Session::new(None, None, None, true);
    merge_conflict.set_name("Aurora".into(), &doc);
    merge_conflict.set_css(":root { --primary: #ff0000; }".into());
    let err = merge_conflict.prepare_save(&doc).unwrap_err();
    assert_eq!(err, "“Aurora” has different custom CSS. Use the same CSS or pick another name.");
}

#[test]
fn resolve_supports_legacy_builtin_theme_aliases() {
    assert_eq!(resolve("t3-grove", &[], &[]).map(|t| t.id.as_str()), Some("grove"));
    assert_eq!(resolve("t3-chat-dark", &[], &[]).map(|t| t.id.as_str()), Some("t3-chat"));
    assert_eq!(canonical_theme_preference("t3-grove"), "grove");
    assert_eq!(canonical_theme_preference("t3-chat-dark"), "t3-chat-dark");
    // Published theme IDs that are not aliases are preserved verbatim (`pingdotgg/t3code#31318`).
    assert_eq!(normalize_theme_id("constructor"), "constructor");
    assert_eq!(normalize_theme_id("to-string"), "to-string");
}

#[test]
fn decode_uses_top_level_canvas_and_accent_seeds_when_present() {
    let seeded = decode(
        &json!({
            "version": 1,
            "name": "Seeded",
            "appearance": "dark",
            "canvas": "#141824",
            "accent": "#ff5599",
            "colors": { "text": "#f0f4ff" }
        }),
        false,
    )
    .unwrap();
    let unseeded = decode(
        &json!({
            "version": 1,
            "name": "Unseeded",
            "appearance": "dark",
            "colors": { "text": "#f0f4ff" }
        }),
        false,
    )
    .unwrap();
    assert_ne!(seeded.colors["canvas"], unseeded.colors["canvas"]);
    assert_eq!(seeded.colors["text"], "#f0f4ff");
    // Invalid seed color is rejected (`pingdotgg/t3code#37982`).
    let err = decode(
        &json!({
            "version": 1,
            "name": "Bad Seed",
            "appearance": "dark",
            "canvas": "not-a-color",
            "accent": "#ff5599",
            "colors": { "text": "#f0f4ff" }
        }),
        false,
    )
    .unwrap_err();
    assert!(err.contains("canvas"));
}

#[test]
fn slot_engine_parses_all_29_theme_css_hooks_and_scrollbar_aliases() {
    use slots::{SlotBackground, SlotLength, ThemeSlot};

    assert_eq!(ThemeSlot::ALL.len(), 29);

    let mut css = String::new();
    for (idx, slot) in ThemeSlot::ALL.iter().enumerate() {
        let color = format!("#{:02x}{:02x}{:02x}", (idx * 7 + 10) & 0xff, (idx * 11 + 20) & 0xff, (idx * 13 + 30) & 0xff);
        css.push_str(&format!("{} {{ background-color: {}; padding: {}px; }}\n", slot.selector(), color, idx + 1));
    }

    let parsed = css::ParsedCss::parse(&css);
    let diag = parsed.diagnostics();
    assert_eq!(diag.applied_slot_rules, 58);
    assert_eq!(diag.unmapped_dom_rules, 0);

    let roles = standard(true);
    let pal = palette(Some(&roles), true, 100, 80).unwrap();
    let sheet = parsed.slot_stylesheet(true, &roles, &pal);
    assert!(!sheet.is_empty());
    assert!(!sheet.has_scoped_background_image());

    for (idx, slot) in ThemeSlot::ALL.iter().enumerate() {
        let set = sheet.get(*slot).unwrap_or_else(|| panic!("missing compiled slot {slot:?}"));
        let r = ((idx * 7 + 10) & 0xff) as u32;
        let g = ((idx * 11 + 20) & 0xff) as u32;
        let b = ((idx * 13 + 30) & 0xff) as u32;
        let expected_color = (r << 24) | (g << 16) | (b << 8) | 0xff;
        assert_eq!(set.base.background, Some(SlotBackground::Solid(expected_color)), "{slot:?}");
        assert_eq!(set.base.padding.top, Some(SlotLength::Px((idx + 1) as f32)), "{slot:?}");
    }

    // WebKit scrollbar aliases map onto ScrollAreaScrollbar and ScrollAreaThumb.
    let alias_css = r#"
        ::-webkit-scrollbar { width: 10px; }
        ::-webkit-scrollbar-thumb { background: #445566; border-radius: 50%; }
        ::-webkit-scrollbar-thumb:hover { background: #667788; }
    "#;
    let alias_parsed = css::ParsedCss::parse(alias_css);
    assert_eq!(alias_parsed.diagnostics().applied_slot_rules, 4);
    assert_eq!(alias_parsed.diagnostics().unmapped_dom_rules, 0);
    let alias_sheet = alias_parsed.slot_stylesheet(true, &roles, &pal);
    assert_eq!(
        alias_sheet.get(ThemeSlot::ScrollAreaScrollbar).unwrap().base.width,
        Some(SlotLength::Px(10.0))
    );
    let thumb = alias_sheet.get(ThemeSlot::ScrollAreaThumb).unwrap();
    assert_eq!(thumb.base.background, Some(SlotBackground::Solid(0x4455_66ff)));
    assert_eq!(thumb.base.border_radii.top_left, Some(SlotLength::Px(slots::PILL_RADIUS_PX)));
    assert_eq!(thumb.hover.background, Some(SlotBackground::Solid(0x6677_88ff)));
}

#[test]
fn slot_engine_handles_appearance_cascade_specificity_important_pseudo_states_and_row_kinds() {
    use slots::{SlotBackground, SlotLength, ThemeSlot};

    let css = r#"
        .dark [data-slot="button"] {
            background: #111111;
            padding-left: 4px;
        }
        html.dark [data-slot="button"] {
            border-color: #222222;
        }
        [data-slot="button"] {
            background: #aaaaaa;
            border-color: #bbbbbb;
            padding: 8px 16px;
            opacity: 0.7 !important;
        }
        .dark [data-slot="button"] {
            opacity: 0.9;
        }
        :root:not(.dark) [data-slot="button"] {
            background: #f0f0f0;
        }
        [data-slot="button"], [data-slot="select-trigger"] {
            border-radius: 10px;
        }
        [data-slot="button"]:hover {
            background: #333333;
        }
        [data-slot="button"]:active {
            background: #444444;
        }
        [data-timeline-row-kind] {
            padding: 6px;
        }
        [data-timeline-row-kind="message"] {
            padding-left: 18px;
            background: #123456;
        }
        [data-timeline-row-kind="work"] {
            display: none;
        }
    "#;
    let parsed = css::ParsedCss::parse(css);
    let dark_roles = standard(true);
    let dark_pal = palette(Some(&dark_roles), true, 100, 80).unwrap();
    let dark_sheet = parsed.slot_stylesheet(true, &dark_roles, &dark_pal);

    let btn = dark_sheet.get(ThemeSlot::Button).unwrap();
    // `.dark [data-slot="button"]` (0,2,0) beats later `[data-slot="button"]` (0,1,0).
    assert_eq!(btn.base.background, Some(SlotBackground::Solid(0x1111_11ff)));
    // `html.dark [data-slot="button"]` (0,2,1) beats later `[data-slot="button"]` (0,1,0).
    assert_eq!(btn.base.border_color, Some(0x2222_22ff));
    // Higher-specificity `padding-left: 4px` survives lower-specificity `padding: 8px 16px` shorthand.
    assert_eq!(btn.base.padding.top, Some(SlotLength::Px(8.0)));
    assert_eq!(btn.base.padding.right, Some(SlotLength::Px(16.0)));
    assert_eq!(btn.base.padding.bottom, Some(SlotLength::Px(8.0)));
    assert_eq!(btn.base.padding.left, Some(SlotLength::Px(4.0)));
    // `!important` wins over higher-specificity non-important declaration.
    assert_eq!(btn.base.opacity, Some(0.7));
    // Comma-separated list applies to both Button and SelectTrigger.
    assert_eq!(btn.base.border_radii.top_left, Some(SlotLength::Px(10.0)));
    assert_eq!(
        dark_sheet.get(ThemeSlot::SelectTrigger).unwrap().base.border_radii.top_left,
        Some(SlotLength::Px(10.0))
    );
    // Pseudo-states stay isolated from base.
    assert_eq!(btn.hover.background, Some(SlotBackground::Solid(0x3333_33ff)));
    assert_eq!(btn.active.background, Some(SlotBackground::Solid(0x4444_44ff)));

    // Light mode activates `:root:not(.dark)` instead of `.dark`.
    let light_roles = standard(false);
    let light_pal = palette(Some(&light_roles), false, 100, 80).unwrap();
    let light_sheet = parsed.slot_stylesheet(false, &light_roles, &light_pal);
    let light_btn = light_sheet.get(ThemeSlot::Button).unwrap();
    assert_eq!(light_btn.base.background, Some(SlotBackground::Solid(0xf0f0_f0ff)));

    // Timeline row kind matching inherits generic rules and falls back cleanly.
    let msg_row = dark_sheet.timeline_row("message").unwrap();
    assert_eq!(msg_row.base.padding.top, Some(SlotLength::Px(6.0)));
    assert_eq!(msg_row.base.padding.left, Some(SlotLength::Px(18.0)));
    assert_eq!(msg_row.base.background, Some(SlotBackground::Solid(0x1234_56ff)));
    let work_row = dark_sheet.timeline_row("work").unwrap();
    assert_eq!(work_row.base.display_none, Some(true));
    assert_eq!(work_row.base.padding.top, Some(SlotLength::Px(6.0)));
    let other_row = dark_sheet.timeline_row("checkpoint").unwrap();
    assert_eq!(other_row.base.padding.top, Some(SlotLength::Px(6.0)));
    assert_eq!(other_row.base.display_none, None);
}

#[test]
fn slot_engine_parses_vars_gradients_shadows_layout_typography_and_wallpaper_scoping() {
    use slots::{
        BackgroundImageSize, SlotAlignItems, SlotBackground, SlotFlexDirection,
        SlotJustifyContent, SlotLength, SlotLineHeight, SlotSizeConstraint, ThemeSlot,
    };

    let css = r#"
        :root {
            --card-accent: #ff5599;
            --card-radius: 1rem;
        }
        [data-user-message-bubble] {
            --local-border: var(--card-accent);
            background: linear-gradient(to right, #112233 10%, #445566, #778899 90%);
            color: var(--app-theme-text);
            border: 2px solid var(--local-border);
            border-top: 4px dashed #aabbcc;
            border-radius: var(--card-radius) 8px 4px 2px;
            padding: 4px 8px 12px 16px;
            margin: 1px 2px auto -4px;
            gap: 6px 12px;
            width: 50%;
            min-width: 120px;
            max-width: none;
            height: auto;
            min-height: 2rem;
            max-height: 400px;
            box-shadow: inset 0 1px 4px rgba(255, 255, 255, 0.2), 0 8px 24px -4px #00000080;
            font-family: "JetBrains Mono", monospace;
            font-size: 1.25rem;
            font-weight: bold;
            line-height: 1.6;
            flex-direction: column-reverse;
            align-items: flex-end;
            justify-content: space-evenly;
        }
        [data-slot="dialog-popup"] {
            box-shadow: none;
            background: transparent;
        }
        [data-slot="popover-popup"] {
            background: linear-gradient(90deg, #ff0066, #6600ff) no-repeat;
        }
        [data-slot="menu-popup"] {
            background: linear-gradient(180deg, #112233, #445566), #111111;
        }
        [data-slot="tooltip-popup"] {
            background: linear-gradient(not-a-valid-gradient), #223344;
        }
        [data-workspace-area="conversation"] {
            background: linear-gradient(rgba(0, 0, 0, 0.5), rgba(0, 0, 0, 0.5)), var(--theme-background-image) center / contain no-repeat;
        }
    "#;

    let (_, sheet) = palette_and_slots_for_with_css(None, None, Some(css), true, 100, 80).unwrap();
    assert!(sheet.has_scoped_background_image());

    let bubble = &sheet.get(ThemeSlot::UserMessageBubble).unwrap().base;
    match &bubble.background {
        Some(SlotBackground::LinearGradient { angle_deg, stops }) => {
            assert_eq!(*angle_deg, 90.0);
            assert_eq!(stops.len(), 3);
            assert_eq!(stops[0].color, 0x1122_33ff);
            assert_eq!(stops[0].position, Some(0.1));
            assert_eq!(stops[1].color, 0x4455_66ff);
            assert!((stops[1].position.unwrap() - 0.5).abs() < 1e-5);
            assert_eq!(stops[2].color, 0x7788_99ff);
            assert_eq!(stops[2].position, Some(0.9));
        }
        other => panic!("expected linear gradient, got {other:?}"),
    }
    assert!(bubble.color.is_some());
    assert_eq!(bubble.border_color, Some(0xaabb_ccff));
    assert_eq!(bubble.border_widths.top, Some(4.0));
    assert_eq!(bubble.border_widths.right, Some(2.0));
    assert_eq!(bubble.border_widths.bottom, Some(2.0));
    assert_eq!(bubble.border_widths.left, Some(2.0));
    assert_eq!(bubble.border_radii.top_left, Some(SlotLength::Rem(1.0)));
    assert_eq!(bubble.border_radii.top_right, Some(SlotLength::Px(8.0)));
    assert_eq!(bubble.border_radii.bottom_right, Some(SlotLength::Px(4.0)));
    assert_eq!(bubble.border_radii.bottom_left, Some(SlotLength::Px(2.0)));
    assert_eq!(bubble.padding.top, Some(SlotLength::Px(4.0)));
    assert_eq!(bubble.padding.right, Some(SlotLength::Px(8.0)));
    assert_eq!(bubble.padding.bottom, Some(SlotLength::Px(12.0)));
    assert_eq!(bubble.padding.left, Some(SlotLength::Px(16.0)));
    assert_eq!(bubble.margin.top, Some(SlotLength::Px(1.0)));
    assert_eq!(bubble.margin.right, Some(SlotLength::Px(2.0)));
    assert_eq!(bubble.margin.bottom, Some(SlotLength::Auto));
    assert_eq!(bubble.margin.left, Some(SlotLength::Px(-4.0)));
    assert_eq!(bubble.row_gap, Some(SlotLength::Px(6.0)));
    assert_eq!(bubble.column_gap, Some(SlotLength::Px(12.0)));
    assert_eq!(bubble.width, Some(SlotLength::Fraction(0.5)));
    assert_eq!(bubble.min_width, Some(SlotLength::Px(120.0)));
    assert_eq!(bubble.max_width, Some(SlotSizeConstraint::None));
    assert_eq!(bubble.height, Some(SlotLength::Auto));
    assert_eq!(bubble.min_height, Some(SlotLength::Rem(2.0)));
    assert_eq!(bubble.max_height, Some(SlotSizeConstraint::Px(400.0)));
    let shadows = bubble.box_shadows.as_ref().unwrap();
    assert_eq!(shadows.len(), 2);
    assert!(shadows[0].inset);
    assert_eq!(shadows[0].blur_radius, 4.0);
    assert!(!shadows[1].inset);
    assert_eq!(shadows[1].spread_radius, -4.0);
    assert_eq!(shadows[1].color, 0x0000_0080);
    assert_eq!(bubble.font_family.as_deref(), Some("\"JetBrains Mono\", monospace"));
    assert_eq!(bubble.font_size, Some(20.0));
    assert_eq!(bubble.font_weight, Some(700));
    assert_eq!(bubble.line_height, Some(SlotLineHeight::Relative(1.6)));
    assert_eq!(bubble.flex_direction, Some(SlotFlexDirection::ColumnReverse));
    assert_eq!(bubble.align_items, Some(SlotAlignItems::FlexEnd));
    assert_eq!(bubble.justify_content, Some(SlotJustifyContent::SpaceEvenly));

    let dialog = &sheet.get(ThemeSlot::DialogPopup).unwrap().base;
    assert_eq!(dialog.box_shadows, Some(Vec::new()));
    assert_eq!(dialog.background, Some(SlotBackground::Transparent));

    // Gradient shorthand with trailing keywords and comma-separated layers/fallback colors.
    match &sheet.get(ThemeSlot::PopoverPopup).unwrap().base.background {
        Some(SlotBackground::LinearGradient { angle_deg, stops }) => {
            assert_eq!(*angle_deg, 90.0);
            assert_eq!(stops.len(), 2);
            assert_eq!(stops[0].color, 0xff00_66ff);
            assert_eq!(stops[1].color, 0x6600_ffff);
        }
        other => panic!("expected gradient with trailing no-repeat, got {other:?}"),
    }
    match &sheet.get(ThemeSlot::MenuPopup).unwrap().base.background {
        Some(SlotBackground::LinearGradient { angle_deg, stops }) => {
            assert_eq!(*angle_deg, 180.0);
            assert_eq!(stops[0].color, 0x1122_33ff);
            assert_eq!(stops[1].color, 0x4455_66ff);
        }
        other => panic!("expected gradient from comma-separated layer, got {other:?}"),
    }
    assert_eq!(
        sheet.get(ThemeSlot::TooltipPopup).unwrap().base.background,
        Some(SlotBackground::Solid(0x2233_44ff))
    );

    let conv = &sheet.get(ThemeSlot::ConversationArea).unwrap().base;
    assert_eq!(
        conv.background,
        Some(SlotBackground::ThemeBackgroundImage {
            size: BackgroundImageSize::Contain,
            overlay_color: Some(0x0000_0080),
        })
    );
}

#[test]
fn slot_engine_enforces_safety_clamps_blocks_remote_fetches_and_separates_diagnostics() {
    use slots::ThemeSlot;

    let css = r#"
        @import url("https://evil.example/a.css");
        :root {
            --sneaky: url(https://evil.example/leak.png);
        }
        [data-slot="button"] {
            border-width: 999px;
            box-shadow: 0 0 900px #000000;
            font-size: 400px;
            opacity: 2.5;
            background: url(https://evil.example/bg.png);
            background-image: image-set(url(https://evil.example/x.png) 1x);
            color: var(--sneaky);
            backdrop-filter: blur(12px);
        }
        [data-slot="input"] {
            font-size: 2px;
            opacity: -0.5;
        }
        [data-slot="button"] svg {
            color: #ff0000;
        }
        [data-slot="button"]::before {
            background: #ff0000;
        }
        svg.lucide {
            stroke-width: 2px;
        }
    "#;

    let parsed = css::ParsedCss::parse(css);
    let diag = parsed.diagnostics();
    assert!(diag.loads_remote_files);
    // Supported declarations on Button (border-width, box-shadow, font-size, opacity, color) + Input (font-size, opacity) = 7.
    // Remote `background: url(...)` & `background-image: image-set(...)` are dropped before counting.
    assert_eq!(diag.applied_slot_rules, 7);
    // Unmapped: `backdrop-filter` on Button + `[data-slot="button"] svg` + `::before` + `svg.lucide` = 4.
    assert_eq!(diag.unmapped_dom_rules, 4);

    let roles = standard(true);
    let pal = palette(Some(&roles), true, 100, 80).unwrap();
    let sheet = parsed.slot_stylesheet(true, &roles, &pal);

    let btn = &sheet.get(ThemeSlot::Button).unwrap().base;
    assert_eq!(btn.border_widths.top, Some(slots::MAX_BORDER_WIDTH_PX));
    assert_eq!(btn.box_shadows.as_ref().unwrap()[0].blur_radius, slots::MAX_BLUR_PX);
    assert_eq!(btn.font_size, Some(slots::MAX_FONT_SIZE_PX));
    assert_eq!(btn.opacity, Some(1.0));
    // Remote fetch declarations were completely dropped.
    assert_eq!(btn.background, None);
    assert_eq!(btn.color, None);

    let input = &sheet.get(ThemeSlot::Input).unwrap().base;
    assert_eq!(input.font_size, Some(slots::MIN_FONT_SIZE_PX));
    assert_eq!(input.opacity, Some(0.0));
}
