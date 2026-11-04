use super::*;
use std::io::Write;
fn extension() -> Extension {
    detail(include_bytes!("fixtures/detail.json"))
        .unwrap()
        .unwrap()
}
fn manifest() -> Value {
    json!({"publisher":"demo","name":"theme","version":"1.0.0","license":"MIT","contributes":{"themes":[{"label":"Demo Light","uiTheme":"vs","path":"./light.json"},{"label":"Demo Dark","uiTheme":"vs-dark","path":"./dark.json"}]}})
}
fn archive(m: &Value, files: &[(&str, &str)]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let o = zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    z.start_file("extension/package.json", o).unwrap();
    z.write_all(m.to_string().as_bytes()).unwrap();
    for (p, s) in files {
        z.start_file(*p, o).unwrap();
        z.write_all(s.as_bytes()).unwrap();
    }
    z.set_comment("Comment with misleading PK\u{5}\u{6} record");
    z.finish().unwrap().into_inner()
}
fn extract_fixture(m: Value, files: &[(&str, &str)]) -> Result<Vec<ThemeDefinition>, String> {
    let b = archive(&m, files);
    extract(
        &extension(),
        m.to_string().as_bytes(),
        &b,
        format!("{:x}", Sha256::digest(&b)).as_bytes(),
    )
}
#[test]
fn search_contract_filters_and_preserves_server_ranking() {
    let u = search_url("  Tokyo Night ", "rating").unwrap();
    let p: BTreeMap<_, _> = u.query_pairs().collect();
    assert_eq!(p["query"], "Tokyo Night");
    assert_eq!(p["category"], "Themes");
    assert_eq!(p["sortBy"], "rating");
    assert_eq!(p["size"], "16");
    assert_eq!(p["sortOrder"], "desc");
    assert!(search_url(" ", "rating").is_none());
    assert_eq!(
        identities(include_bytes!("fixtures/search.json"))
            .unwrap()
            .len(),
        2
    );
    let e = extension();
    assert!(eligible(&e, manifest().to_string().as_bytes(), Some(1024)).unwrap());
    assert!(
        !eligible(
            &e,
            manifest().to_string().as_bytes(),
            Some(MAX_VSIX_BYTES as u64 + 1)
        )
        .unwrap()
    );
    assert!(!eligible(&e, b"{\"contributes\":{\"iconThemes\":[{}]}}", None).unwrap());
    assert_eq!(
        ranked_results(vec![Ok(Some(e.clone())), Err("bad".into()), Ok(None)]).unwrap(),
        [e]
    );
    assert!(ranked_results(vec![Err("bad".into())]).is_err());
    assert!(ranked_results(vec![Ok(None)]).unwrap().is_empty());
    let raw = String::from_utf8(include_bytes!("fixtures/detail.json").to_vec()).unwrap();
    assert!(
        detail(raw.replace("MIT", "Proprietary").as_bytes())
            .unwrap()
            .is_none()
    );
    assert!(
        detail(
            raw.replace("https://open-vsx.org", "http://evil.test")
                .as_bytes()
        )
        .is_err()
    );
    assert_eq!(compact_downloads(123456.), "123.5K");
    assert_eq!(compact_downloads(1200000.), "1.2M");
    assert_eq!(compact_downloads(999_950.), "1M");
    assert_eq!(compact_downloads(999.99), "1K");
}
#[test]
fn verified_archive_strips_comments_resolves_includes_pairs_and_names_stably() {
    let files = [
        (
            "extension/light.json",
            r##"{// comment
 "include":"./base.json","colors":{"editor.background":"#ffffff",},}"##,
        ),
        (
            "extension/dark.json",
            r##"{"colors":{"editor.background":"#101014"}}"##,
        ),
        (
            "extension/base.json",
            r##"{"colors":{"focusBorder":"#123abc"}}"##,
        ),
    ];
    let t = extract_fixture(manifest(), &files).unwrap();
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].label, "Demo");
    assert!(t[0].half(true).is_some());
    assert!(t[0].id.starts_with("ovx-theme-"));
    assert_eq!(t[0].collection.as_ref().unwrap().id, "open-vsx:demo.theme");
    let mut renamed = manifest();
    renamed["contributes"]["themes"][0]["label"] = json!("Renamed Light");
    renamed["contributes"]["themes"][1]["label"] = json!("Renamed Dark");
    assert_eq!(extract_fixture(renamed, &files).unwrap()[0].id, t[0].id);
    let b = archive(&manifest(), &files);
    assert!(
        extract(
            &extension(),
            manifest().to_string().as_bytes(),
            &b,
            "0".repeat(64).as_bytes()
        )
        .unwrap_err()
        .contains("integrity")
    );
}
#[test]
fn malformed_archives_paths_bombs_identity_and_includes_are_rejected() {
    for p in [
        "../outside.json",
        "/tmp/file",
        "C:/file",
        "a/../../file",
        "a\0b",
    ] {
        assert!(package_path(p, "extension/").is_err(), "{p}");
    }
    assert_eq!(
        package_path("..\\base.json", "extension/themes/dark.json").unwrap(),
        "extension/base.json"
    );
    for b in [b"".as_slice(), b"PK", b"not a zip"] {
        assert!(inspect_directory(b).is_err());
    }
    for (expanded, message) in [
        (101u32 * 1024 * 1024, "expands"),
        (0xffffffff, "ZIP64"),
        (1000000, "ratio"),
    ] {
        let mut b = archive(&manifest(), &[]);
        let c = b.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        b[c + 24..c + 28].copy_from_slice(&expanded.to_le_bytes());
        assert!(inspect_directory(&b).unwrap_err().contains(message));
    }
    let cycle = [
        (
            "extension/light.json",
            r##"{"include":"light.json","colors":{"editor.background":"#ffffff"}}"##,
        ),
        (
            "extension/dark.json",
            r##"{"colors":{"editor.background":"#101014"}}"##,
        ),
    ];
    assert!(extract_fixture(manifest(), &cycle).is_err());
    let mut m = manifest();
    m["version"] = json!("2.0.0");
    assert!(
        extract_fixture(m, &cycle)
            .unwrap_err()
            .contains("does not match")
    );
}
#[test]
fn collection_replace_checks_concurrent_edits_and_removes_retired_variants() {
    let e = extension();
    let mut a = super::super::import(
        r##"{"version":1,"name":"One","appearance":"dark","colors":{"accent":"#123456"}}"##,
    )
    .unwrap();
    a.collection = Some(Collection {
        id: e.collection_id.clone(),
        label: e.name.clone(),
    });
    let mut b = a.clone();
    b.id = "two".into();
    let mut doc = crate::client_settings::ClientSettingsDoc::from_json("{}");
    doc.apply_change(&crate::client_settings::ClientSettingChange::ThemeLibrary(
        json!([a, b]),
    ));
    let expected = vec![a.clone(), b];
    let patch = replace_collection(&doc, &e.collection_id, vec![a.clone()], &expected).unwrap();
    assert_eq!(
        patch[super::super::LIBRARY_KEY].as_array().unwrap().len(),
        1
    );
    assert!(
        replace_collection(&doc, &e.collection_id, vec![a], &[])
            .unwrap_err()
            .contains("changed")
    );
}
#[test]
fn search_session_reuses_shown_query_holds_during_install_and_retires_work() {
    let mut s = SearchSession::new();
    assert!(s.set_query(" Nord "));
    assert!(s.settle(false));
    s.shown = Some(s.key());
    assert!(!s.settle(false));
    assert!(s.set_query("Tokyo"));
    assert!(s.settle(false));
    assert!(s.set_query("Nord"));
    assert!(!s.settle(false));
    assert!(s.cancel_request);
    assert!(s.settle(true));
    s.sort = "rating".into();
    assert!(s.settle(false));
    let old = s.epoch;
    s.begin_install();
    assert_ne!(old, s.epoch);
    assert!(!s.settle(true));
    s.end_install();
    assert!(s.settle(false));
    s.clear();
    assert!(s.shown.is_none());
}
#[test]
fn recorded_open_vsx_responses_are_eligible() {
    let e = detail(include_bytes!("fixtures/dracula-detail.json"))
        .unwrap()
        .unwrap();
    assert_eq!(e.id, "dracula-theme.theme-dracula");
    assert!(eligible(&e, include_bytes!("fixtures/dracula-manifest.json"), None).unwrap());
    assert_eq!(
        identities(include_bytes!("fixtures/dracula-search.json"))
            .unwrap()
            .len(),
        16
    );
}
#[test]
fn first_install_and_keep_both_preserve_the_library() {
    let e = extension();
    let mut a = super::super::import(
        r##"{"version":1,"name":"One","appearance":"dark","colors":{"accent":"#123456"}}"##,
    )
    .unwrap();
    a.collection = Some(Collection {
        id: e.collection_id.clone(),
        label: e.name.clone(),
    });
    let mut doc = crate::client_settings::ClientSettingsDoc::from_json("{}");
    let import = MarketplaceImport {
        collection_id: e.collection_id,
        expected: Vec::new(),
        themes: vec![a.clone()],
    };
    let (patch, _) = import
        .prepare(&doc, super::super::ImportChoice::Update)
        .unwrap();
    doc.apply_change(&crate::client_settings::ClientSettingChange::ThemePatch(
        patch,
    ));
    let (patch, copies) = import
        .prepare(&doc, super::super::ImportChoice::KeepBoth)
        .unwrap();
    assert_eq!(copies[0].label, "One (1)");
    assert_eq!(
        patch[super::super::LIBRARY_KEY].as_array().unwrap().len(),
        2
    );
}
#[test]
fn small_vsix_fixture_imports_without_network() {
    let b = include_bytes!("fixtures/theme.vsix");
    let t = extract(
        &extension(),
        include_bytes!("fixtures/package.json"),
        b,
        format!("{:x}", Sha256::digest(b)).as_bytes(),
    )
    .unwrap();
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].label, "Demo Dark");
}
#[test]
fn archive_limits_cover_entry_count_theme_count_and_path_depth() {
    let mut m = manifest();
    m["contributes"]["themes"] = json!(vec![m["contributes"]["themes"][0].clone(); 41]);
    assert!(
        extract_fixture(m, &[])
            .unwrap_err()
            .contains("too many color themes")
    );
    assert!(package_path(&"a".repeat(1025), "extension/").is_err());
    let mut files = Vec::new();
    for i in 0..9 {
        files.push((
            format!("extension/{i}.json"),
            format!(
                r##"{{"include":"{}.json","colors":{{"editor.background":"#ffffff"}}}}"##,
                i + 1
            ),
        ));
    }
    let mut m = manifest();
    m["contributes"]["themes"][0]["path"] = json!("0.json");
    let refs = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect::<Vec<_>>();
    assert!(extract_fixture(m, &refs).is_err());
    let mut b = inspect_directory(&archive(&manifest(), &[])).unwrap();
    let central = b.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let end = b.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
    let entry = b[central..end].to_vec();
    let mut out = b[..central].to_vec();
    for _ in 0..5001 {
        out.extend_from_slice(&entry);
    }
    let directory_size = (out.len() - central) as u32;
    out.extend_from_slice(&b[end..]);
    let end = out.len() - b[end..].len();
    out[end + 12..end + 16].copy_from_slice(&directory_size.to_le_bytes());
    assert!(
        inspect_directory(&out)
            .unwrap_err()
            .contains("too many files")
    );
    b = archive(&manifest(), &[("extension/light.json", "{bad json}")]);
    assert!(
        extract(
            &extension(),
            manifest().to_string().as_bytes(),
            &b,
            format!("{:x}", Sha256::digest(&b)).as_bytes()
        )
        .is_err()
    );
}
#[test]
fn oversized_theme_reads_and_duplicate_source_ids_are_guarded() {
    let large = (0..MAX_JSON_BYTES + 1)
        .scan(1u32, |state, _| {
            *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            Some((b' ' + (*state >> 24) as u8 % 90) as char)
        })
        .collect::<String>();
    let b = archive(&manifest(), &[("extension/large.json", &large)]);
    let mut zip = zip::ZipArchive::new(Cursor::new(inspect_directory(&b).unwrap())).unwrap();
    assert!(
        read_text(&mut zip, "extension/large.json")
            .unwrap_err()
            .contains("too large")
    );
    let mut m = manifest();
    m["contributes"]["themes"] =
        json!([{"label":"Same","path":"dark.json"},{"label":"Same","path":"dark.json"}]);
    let t = extract_fixture(
        m,
        &[(
            "extension/dark.json",
            r##"{"colors":{"editor.background":"#101014"}}"##,
        )],
    )
    .unwrap();
    assert_eq!(t.len(), 2);
    assert_ne!(t[0].id, t[1].id);
}
