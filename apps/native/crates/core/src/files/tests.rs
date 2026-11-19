use serde_json::json;

use super::*;

fn file(path: &str) -> Entry {
    Entry { path: path.to_owned(), kind: EntryKind::File, ignored: false }
}

fn dir(path: &str) -> Entry {
    Entry { path: path.to_owned(), kind: EntryKind::Directory, ignored: false }
}

fn ignored_dir(path: &str) -> Entry {
    Entry { ignored: true, ..dir(path) }
}

fn labels(rows: &[TreeRow]) -> Vec<String> {
    rows.iter().map(|row| format!("{}{}{}", "  ".repeat(row.depth), row.label, if row.kind == EntryKind::Directory { "/" } else { "" })).collect()
}

fn load(tree: &mut FileTree, directory: &str, entries: Vec<Entry>) -> Vec<String> {
    let generation = tree.begin_load(directory, true).expect("a refresh always starts");
    tree.finish_load(directory, generation, entries)
}

#[test]
fn listing_keeps_direct_children_and_reads_ignored() {
    let value = json!({
        "entries": [
            { "path": "src/main.rs", "kind": "file" },
            { "path": "src/deep/x.rs", "kind": "file" },
            { "path": "src/target", "kind": "directory", "ignored": true },
            { "path": "src/odd", "kind": "socket" },
        ],
        "truncated": true,
    });
    let listing = decode_listing(&value, "src").unwrap();
    assert!(listing.truncated);
    assert_eq!(listing.entries, vec![file("src/main.rs"), ignored_dir("src/target")]);
    assert!(decode_listing(&json!({}), "").is_err());
}

#[test]
fn read_file_moves_contents_and_failures_map_to_web_copy() {
    let read = decode_read_file(json!({ "relativePath": "a.txt", "contents": "hi", "byteLength": 2_000_000, "truncated": true })).unwrap();
    assert_eq!((read.contents.as_str(), read.byte_length, read.truncated), ("hi", 2_000_000, true));
    assert!(decode_read_file(json!({ "relativePath": "a" })).is_err());

    let binary = ReadFailure::from_error(&json!({ "_tag": "ProjectReadFileError", "failure": "binary_file", "message": "x" }), "fallback");
    assert_eq!(binary.failure, Some(FileFailure::Binary));
    assert_eq!(binary.describe(), "The file is binary and cannot be displayed as text.");
    let root = ReadFailure::from_error(
        &json!({ "_tag": "ProjectReadFileError", "failure": "operation_failed", "operation": "realpath-workspace-root", "resolvedPath": "/r/x", "message": "m" }),
        "fallback",
    );
    assert_eq!(root.describe(), "The workspace folder could not be accessed.");
    assert_eq!(root.attempted_path.as_deref(), Some("/r/x"));
    let other = ReadFailure::from_error(&json!({ "_tag": "EnvironmentAuthorizationError", "message": "nope" }), "Not allowed");
    assert_eq!((other.failure, other.describe()), (None, "Not allowed".to_owned()));
}

#[test]
fn workspace_relative_paths_follow_shared_path_rules() {
    assert_eq!(workspace_relative_path("/repo/src/a.rs", "/repo/").as_deref(), Some("src/a.rs"));
    assert_eq!(workspace_relative_path("/repo", "/repo").as_deref(), Some("."));
    assert_eq!(workspace_relative_path("/repository/a", "/repo"), None);
    assert_eq!(workspace_relative_path("/repo/../etc/passwd", "/repo"), None);
    assert_eq!(workspace_relative_path("c:\\Repo\\Src\\A.rs", "C:\\repo").as_deref(), Some("Src/A.rs"));
    assert!(is_host_path("/a") && is_host_path("~/a") && is_host_path("C:\\a") && !is_host_path("src/a"));
}

#[test]
fn file_surface_paths_normalize_like_open_file() {
    assert_eq!(file_surface_path(".", Some("/repo")), None);
    assert_eq!(file_surface_path("/repo", Some("/repo")), None);
    assert_eq!(file_surface_path("/repo/src/a.rs", Some("/repo")).as_deref(), Some("src/a.rs"));
    assert_eq!(file_surface_path("src/", Some("/repo")).as_deref(), Some("src"));
    assert_eq!(file_surface_path("/elsewhere/b.rs", Some("/repo")).as_deref(), Some("/elsewhere/b.rs"));
    assert_eq!(file_surface_path("C:/", None).as_deref(), Some("C:/"));
    assert_eq!(absolute_path("src/a.rs", "/repo/"), "/repo/src/a.rs");
    assert_eq!(absolute_path("src/a.rs", "C:\\repo"), "C:\\repo\\src\\a.rs");
    assert_eq!(absolute_path("/tmp/x", "/repo"), "/tmp/x");
    assert_eq!(clipboard_path("src/a.rs", "C:\\repo\\src\\a.rs"), "src\\a.rs");
    assert_eq!(clipboard_path("src/a.rs", "/repo/src/a.rs"), "src/a.rs");
}

#[test]
fn breadcrumbs_lead_with_the_project_except_for_host_paths() {
    let crumbs = breadcrumbs("t3", "src/lib/a.rs");
    let summary: Vec<(&str, &str, bool)> = crumbs.iter().map(|c| (c.label.as_str(), c.path.as_str(), c.is_last)).collect();
    assert_eq!(summary, vec![("t3", "", false), ("src", "src", false), ("lib", "src/lib", false), ("a.rs", "src/lib/a.rs", true)]);
    let host = breadcrumbs("t3", "/etc/hosts");
    assert_eq!(host.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), vec!["/etc", "/etc/hosts"]);
}

#[test]
fn mentions_match_serialize_composer_file_link() {
    assert_eq!(composer_file_link("src/a b.rs"), "[a b.rs](src/a%20b.rs)");
    assert_eq!(composer_file_link("docs/[x](y)#?.md"), "[\\[x\\](y)#?.md](docs/%5Bx%5D%28y%29%23%3F.md)");
    assert_eq!(composer_file_link("é.md"), "[é.md](%C3%A9.md)");
    assert_eq!((group_thousands(0), group_thousands(999), group_thousands(1_048_576)), ("0".into(), "999".into(), "1,048,576".into()));
}

#[test]
fn file_kinds_pick_previews_and_grammars() {
    assert!(is_markdown("README.MD") && !is_markdown("md"));
    assert_eq!(image_kind("a/b.PNG"), Some(ImageKind::Raster));
    assert_eq!(image_kind("logo.svg"), Some(ImageKind::Svg));
    assert_eq!(image_kind("photo.heic"), Some(ImageKind::Unsupported));
    assert_eq!(image_kind("a.rs"), None);
    assert_eq!(syntax_language("src/main.rs"), Some("rust"));
    assert_eq!(syntax_language("Cargo.lock"), Some("toml"));
    assert_eq!(syntax_language("x.tsx"), Some("tsx"));
    assert_eq!(syntax_language(".gitignore"), None);
    assert_eq!(syntax_language("Makefile"), None);
}

#[test]
fn line_index_splits_every_terminator_and_clamps_reveals() {
    let index = LineIndex::new("a\r\nbb\rccc\n");
    assert_eq!(index.len(), 3);
    assert_eq!((index.range(0), index.range(1), index.range(2)), (Some(0..1), Some(3..5), Some(6..9)));
    assert_eq!(index.reveal_index(0), 0);
    assert_eq!(index.reveal_index(2), 1);
    assert_eq!(index.reveal_index(99), 2);
    for (height, expected) in [(20., 91), (10., 81), (30., 94)] {
        assert_eq!(reveal_top_line(100, 400., height), expected);
    }
    assert_eq!(reveal_top_line(2, 400., 10.), 0);
    assert_eq!(LineIndex::new("").len(), 1);
    assert_eq!(LineIndex::new("x\n\n").len(), 2);
    assert_eq!(LineIndex::new(&"0123456789\n".repeat(120)).digits(), 3);
}

#[test]
fn display_lines_expand_tabs_and_remap_runs() {
    let (text, runs) = expand_tabs("\tfn x", &[(1..3, 'k'), (4..5, 'v')]);
    assert_eq!(text, "  fn x");
    assert_eq!(runs, vec![(2..4, 'k'), (5..6, 'v')]);
    let (text, _) = expand_tabs::<()>("a\tb", &[]);
    assert_eq!(text, "a b");
    let long = "é".repeat(MAX_LINE_CHARS + 10);
    let (text, runs) = expand_tabs(&long, &[(0..long.len(), 's')]);
    assert_eq!(text.chars().count(), MAX_LINE_CHARS + 1);
    assert!(text.ends_with('…'));
    assert_eq!(runs, vec![(0..MAX_LINE_CHARS * 2, 's')]);
    assert_eq!(LineIndex::new("\tab").max_columns, 4);
}

#[test]
fn tree_lists_lazily_sorts_folders_first_and_flattens_lone_folders() {
    let mut tree = FileTree::default();
    assert!(!tree.ready());
    load(&mut tree, "", vec![file("b.rs"), file(".env"), dir("src"), file("A.md"), dir(".github")]);
    assert_eq!(labels(&tree.rows()), vec![".github/", "src/", ".env", "A.md", "b.rs"]);
    assert_eq!(tree.toggle("src"), Some("src".to_owned()));
    assert!(tree.begin_load("src", false).is_some());
    assert!(tree.rows().iter().any(|row| row.path == "src" && row.loading));
    // `src` holds only `src/app`, so it opens with its parent and shows on one row.
    assert_eq!(load(&mut tree, "src", vec![dir("src/app")]), vec!["src/app".to_owned()]);
    load(&mut tree, "src/app", vec![file("src/app/main.rs")]);
    assert_eq!(labels(&tree.rows()), vec![".github/", "src/app/", "  main.rs", ".env", "A.md", "b.rs"]);
    assert_eq!(tree.kind("src/app/main.rs"), Some(EntryKind::File));
    assert_eq!(tree.toggle("src/app"), None);
    assert_eq!(labels(&tree.rows()), vec![".github/", "src/app/", ".env", "A.md", "b.rs"]);
}

#[test]
fn tree_drops_stale_listings_and_reports_reachable_errors() {
    let mut tree = FileTree::default();
    let first = tree.begin_load("", false).unwrap();
    assert_eq!(tree.begin_load("", false), None);
    let second = tree.begin_load("", true).unwrap();
    tree.finish_load("", first, vec![file("old.rs")]);
    assert!(!tree.ready());
    tree.finish_load("", second, vec![dir("a"), file("new.rs")]);
    assert_eq!(labels(&tree.rows()), vec!["a/", "new.rs"]);
    tree.toggle("a");
    let generation = tree.begin_load("a", false).unwrap();
    tree.fail_load("a", generation, "Unable to load folder.".to_owned());
    assert_eq!(tree.error(), Some("Unable to load folder."));
    assert_eq!(tree.failed_directories(), vec!["a".to_owned()]);
    assert_eq!(tree.refresh_targets(), vec![String::new(), "a".to_owned()]);
    load(&mut tree, "", vec![file("new.rs")]);
    assert_eq!(tree.error(), None);
    assert_eq!(tree.refresh_targets(), vec![String::new()]);
}

#[test]
fn reveal_opens_every_ancestor_and_lists_the_missing_ones() {
    let mut tree = FileTree::default();
    load(&mut tree, "", vec![dir("a"), file("z")]);
    assert_eq!(tree.reveal("a/b/c.rs"), vec!["a".to_owned(), "a/b".to_owned()]);
    assert!(tree.is_expanded("a") && tree.is_expanded("a/b"));
    load(&mut tree, "a", vec![dir("a/b"), file("a/x")]);
    load(&mut tree, "a/b", vec![file("a/b/c.rs")]);
    assert_eq!(labels(&tree.rows()), vec!["a/", "  b/", "    c.rs", "  x", "z"]);
    assert_eq!(tree.expand("a/b"), None);
    tree.collapse("a/b");
    assert_eq!(tree.expand("a/b"), None);
    assert!(tree.is_expanded("a/b"));
    assert_eq!(tree.expand("z/new"), Some("z/new".to_owned()));
}

#[test]
fn expand_all_skips_ignored_folders_and_follows_new_listings() {
    let mut tree = FileTree::default();
    load(&mut tree, "", vec![dir("src"), ignored_dir("node_modules"), file("a")]);
    assert!(tree.has_directories() && !tree.all_expanded());
    assert_eq!(tree.expand_all(), vec!["src".to_owned()]);
    assert!(tree.all_expanded());
    let more = load(&mut tree, "src", vec![dir("src/a"), dir("src/b"), ignored_dir("src/target")]);
    assert_eq!(more, vec!["src/a".to_owned(), "src/b".to_owned()]);
    assert!(!tree.is_expanded("node_modules") && !tree.is_expanded("src/target"));
    tree.collapse("src/a");
    assert!(!tree.expanding_all());
    tree.collapse_all();
    assert_eq!(labels(&tree.rows()), vec!["node_modules/", "src/", "a"]);
    assert!(tree.rows()[0].ignored);
}

#[test]
fn search_shows_matches_with_their_folders_open_and_merges_server_results() {
    let mut tree = FileTree::default();
    load(&mut tree, "", vec![dir("src"), file("readme.md"), file("Cargo.toml")]);
    let rows = tree.search_rows("CARGO", &[]);
    assert_eq!(labels(&rows), vec!["Cargo.toml"]);
    let found = [file("src/deep/cargo_util.rs"), file("src/other.rs")];
    let rows = tree.search_rows("cargo", &found);
    assert_eq!(labels(&rows), vec!["src/deep/", "  cargo_util.rs", "Cargo.toml"]);
    assert!(rows.iter().all(|row| row.kind == EntryKind::File || row.expanded));
    assert_eq!(labels(&tree.search_rows("  ", &found)), labels(&tree.rows()));
    assert!(tree.search_rows("nothing", &found).is_empty());
}

#[test]
fn image_assets_use_workspace_files_inside_the_root() {
    let inside = image_asset_request("t1", "/repo", "/repo/a.png");
    assert_eq!(inside["resource"]["_tag"], "workspace-file");
    assert_eq!(image_asset_request("t1", "/repo", "/tmp/a.png")["resource"]["_tag"], "media-file");
    assert_eq!(decode_asset_url(&json!({ "relativeUrl": "/api/assets/x/y" })).as_deref(), Ok("/api/assets/x/y"));
    assert!(decode_asset_url(&json!({ "relativeUrl": "https://evil" })).is_err());
}

#[test]
fn frontmatter_masks_to_blank_lines_and_keeps_offsets() {
    let text = "---\ntitle: x\n---\n# Body\n";
    let masked = mask_frontmatter(text);
    assert_eq!(masked, "   \n        \n   \n# Body\n");
    assert_eq!(masked.len(), text.len());
    assert_eq!(mask_frontmatter("---\r\na: 1\r\n...\r\nrest"), "   \r\n    \r\n   \r\nrest");
    assert_eq!(mask_frontmatter("# No\n---\n"), "# No\n---\n");
    assert_eq!(mask_frontmatter("---\nunterminated"), "---\nunterminated");
    assert_eq!(mask_frontmatter("---"), "---");
}

#[test]
fn file_links_carry_paths_and_lines() {
    assert_eq!(file_link_target("src/a%20b.rs:12"), Some(("src/a b.rs".to_owned(), Some(12))));
    assert_eq!(file_link_target("src/a.rs:12:4"), Some(("src/a.rs".to_owned(), Some(12))));
    assert_eq!(file_link_target("/repo/a.rs#L7C2"), Some(("/repo/a.rs".to_owned(), Some(7))));
    assert_eq!(file_link_target("file:///repo/a.rs#L3"), Some(("/repo/a.rs".to_owned(), Some(3))));
    assert_eq!(file_link_target("file:///C:/repo/a.rs"), Some(("C:/repo/a.rs".to_owned(), None)));
    assert_eq!(file_link_target("file://server/share/a.rs"), Some(("\\\\server\\share\\a.rs".to_owned(), None)));
    assert_eq!(file_link_target("README.md"), Some(("README.md".to_owned(), None)));
    assert_eq!(file_link_target("a.rs:0"), Some(("a.rs".to_owned(), None)));
    assert_eq!(file_link_target("https://example.com/a.rs"), None);
    assert_eq!(file_link_target("mailto:x@y.z"), None);
    assert_eq!(file_link_target(""), None);
}

#[test]
fn metadata_query_paths_follow_get_file_metadata_query_atom() {
    use super::metadata::query_path;
    assert_eq!(query_path("src/main.rs:12:3", Some("/repo")), Some("/repo/src/main.rs".to_owned()));
    assert_eq!(query_path("/abs/file.txt", None), Some("/abs/file.txt".to_owned()));
    assert_eq!(query_path("~/notes.md", Some("/repo")), Some("~/notes.md".to_owned()));
    assert_eq!(query_path(r"C:\Repo\Main.rs", None), Some(r"c:\repo\main.rs".to_owned()));
    assert_eq!(query_path("/repo/dir/", None), Some("/repo/dir".to_owned()));
    // Relative without a cwd, network paths and very long paths are never asked about.
    assert_eq!(query_path("src/main.rs", None), None);
    assert_eq!(query_path(r"\\host\share\file", None), None);
    assert_eq!(query_path("//host/share/file", None), None);
    assert_eq!(query_path(&format!("/{}", "a".repeat(600)), None), None);
}

#[test]
fn metadata_decodes_in_request_order() {
    use super::metadata::{EntryMetadata, MetadataKind, decode, request};
    assert_eq!(request(&["/a".to_owned()]), json!({ "paths": ["/a"] }));
    let reply = json!({ "entries": [{ "kind": "file", "byteLength": 3 }, null, { "kind": "socket" }, { "kind": 1 }] });
    assert_eq!(
        decode(&reply, 5).unwrap(),
        vec![
            Some(EntryMetadata { kind: MetadataKind::File, byte_length: Some(3), mime_type: None }),
            None,
            Some(EntryMetadata { kind: MetadataKind::Other, byte_length: None, mime_type: None }),
            None,
            None,
        ]
    );
    assert!(decode(&json!({}), 1).is_err());
}

#[test]
fn thread_metadata_keeps_hits_and_expires_misses() {
    use super::metadata::{EntryMetadata, Lookup, MetadataCache, MetadataKind, STALE_MS};
    let file = Some(EntryMetadata { kind: MetadataKind::File, byte_length: None, mime_type: None });
    let mut cache = MetadataCache::default();
    cache.retain_thread("t1");
    assert_eq!(cache.lookup("/repo/a.rs", Some("t1"), 0), Lookup::Fetch);
    cache.remember("/repo/a.rs", file.clone(), Some("t1"), 0);
    cache.remember("/repo/later.rs", None, Some("t1"), 0);
    // Within the thread a hit outlives the shared cache's staleness.
    assert_eq!(cache.lookup("/repo/a.rs", Some("t1"), STALE_MS * 5), Lookup::Known(file.clone()));
    // A miss is believed only until it expires: agents mention paths before creating them.
    assert_eq!(cache.lookup("/repo/later.rs", Some("t1"), STALE_MS - 1), Lookup::Known(None));
    assert_eq!(cache.lookup("/repo/later.rs", Some("t1"), STALE_MS), Lookup::Fetch);
    cache.remember("/repo/later.rs", file.clone(), Some("t1"), STALE_MS);
    assert_eq!(cache.lookup("/repo/later.rs", Some("t1"), STALE_MS * 9), Lookup::Known(file.clone()));
    // Without a retaining thread, everything expires.
    assert_eq!(cache.lookup("/repo/a.rs", None, STALE_MS * 5), Lookup::Fetch);
    // Another thread reads the fresh shared entry and retains it from then on.
    cache.retain_thread("t2");
    cache.remember("/repo/b.rs", file.clone(), None, 100);
    assert_eq!(cache.lookup("/repo/b.rs", Some("t2"), 200), Lookup::Known(file.clone()));
    assert_eq!(cache.lookup("/repo/b.rs", Some("t2"), STALE_MS * 3), Lookup::Known(file.clone()));
    // A later answer replaces what threads that already held the path retained.
    cache.remember("/repo/b.rs", None, None, STALE_MS * 3);
    assert_eq!(cache.lookup("/repo/b.rs", Some("t2"), STALE_MS * 3 + 1), Lookup::Known(None));
    // Releasing the last reader drops the thread's retention.
    cache.release_thread("t1");
    assert_eq!(cache.lookup("/repo/a.rs", Some("t1"), STALE_MS * 5), Lookup::Fetch);
}

#[test]
fn metadata_cache_drops_the_oldest_write_past_its_limit() {
    use super::metadata::{CACHE_LIMIT, Lookup, MetadataCache};
    let mut cache = MetadataCache::default();
    for index in 0..=CACHE_LIMIT {
        cache.remember(&format!("/f{index}"), None, None, 0);
    }
    assert_eq!(cache.lookup("/f0", None, 1), Lookup::Fetch);
    assert_eq!(cache.lookup("/f1", None, 1), Lookup::Known(None));
    // Rewriting a path makes it the newest.
    cache.remember("/f1", None, None, 0);
    cache.remember("/extra", None, None, 0);
    assert_eq!(cache.lookup("/f1", None, 1), Lookup::Known(None));
    assert_eq!(cache.lookup("/f2", None, 1), Lookup::Fetch);
}


#[test]
fn write_file_wire_shapes_and_preview_classifiers() {
    assert_eq!(
        write_file_request("/repo", "src/a.rs", "fn main() {}"),
        json!({ "cwd": "/repo", "relativePath": "src/a.rs", "contents": "fn main() {}" })
    );
    assert_eq!(decode_write_file(&json!({ "relativePath": "src/a.rs" })).unwrap(), "src/a.rs");
    assert!(decode_write_file(&json!({})).is_err());

    let err = WriteFailure::from_error(
        &json!({ "_tag": "ProjectWriteFileError", "failure": "workspace_path_outside_root", "message": "bad" }),
        "fallback",
    );
    assert_eq!(err.failure, Some(FileFailure::OutsideRoot));
    assert_eq!(err.describe(), "The requested path is outside the workspace.");

    assert!(is_mermaid("diagram.mmd") && is_mermaid("arch.MERMAID") && !is_mermaid("a.md"));
    assert!(is_notebook("analysis.ipynb") && !is_notebook("data.json"));
    assert_eq!(delimited_separator("data.CSV"), Some(','));
    assert_eq!(delimited_separator("rows.tsv"), Some('\t'));
    assert_eq!(delimited_separator("notes.txt"), None);
    assert!(is_audio("clip.mp3") && is_audio("song.wav") && is_audio("voice.m4a") && !is_audio("clip.mp4"));
    assert!(is_font("Inter.ttf") && is_font("Mono.OTF") && is_font("ui.woff2") && !is_font("ui.svg"));
    assert_eq!(binary_container_kind("pkg.zip"), Some(BinaryContainerKind::Archive));
    assert_eq!(binary_container_kind("app.tar"), Some(BinaryContainerKind::Archive));
    assert_eq!(binary_container_kind("state.sqlite3"), Some(BinaryContainerKind::Sqlite));
    assert_eq!(binary_container_kind("main.rs"), None);
}

#[test]
fn content_revision_and_save_coordinator_follow_web_semantics() {
    use super::save::{
        DEFAULT_SAVE_DEBOUNCE_MS, DismissalOutcome, FileSaveCoordinator, FileSaveSessionStore, FileSaveTarget,
        SaveEffect, decide_escape_dismissal, decide_leave_editor, decide_outside_pointer_dismissal,
        file_content_revision, project_file_cache_key,
    };

    assert_ne!(
        project_file_cache_key("/repo", "file.json", "nodeVersion"),
        project_file_cache_key("/repo", "file.json", "nodeVeasdrs")
    );
    assert_eq!(file_content_revision(""), "0:ztntfp");

    // Keeps pending state until an edit made during a write is also saved.
    let mut coord = FileSaveCoordinator::new(DEFAULT_SAVE_DEBOUNCE_MS);
    let effects = coord.change("first".into(), 0);
    let token = match effects.as_slice() {
        [SaveEffect::PendingChanged(true), SaveEffect::ScheduleDebounce { delay_ms: 500, token }] => *token,
        other => panic!("unexpected effects: {other:?}"),
    };
    let effects = coord.on_debounce_fired(token, true);
    assert_eq!(effects, vec![SaveEffect::Persist { revision: 1, contents: "first".into() }]);
    assert!(coord.is_saving());

    // Edit during in-flight write schedules debounce but does not start a second write yet.
    let effects = coord.change("latest".into(), 600);
    assert!(effects.iter().any(|e| matches!(e, SaveEffect::ScheduleDebounce { delay_ms: 500, .. })));
    // First write completes; remaining debounce is scheduled for the newer edit.
    let effects = coord.on_persist_result(1, true, true, true, 700);
    let token2 = match effects.as_slice() {
        [SaveEffect::CancelDebounce, SaveEffect::ScheduleDebounce { delay_ms: 400, token }] => *token,
        other => panic!("unexpected effects after first write: {other:?}"),
    };
    let effects = coord.on_debounce_fired(token2, true);
    assert_eq!(effects, vec![SaveEffect::Persist { revision: 2, contents: "latest".into() }]);
    let effects = coord.on_persist_result(2, true, true, true, 1200);
    assert_eq!(effects, vec![SaveEffect::PendingChanged(false), SaveEffect::Settled]);
    assert!(!coord.is_dirty());
    assert_eq!(coord.unconfirmed_contents(), None);

    // Edits after disposal are ignored.
    let mut disposed = FileSaveCoordinator::new(500);
    assert!(disposed.dispose(true).is_empty());
    assert!(disposed.change("stale".into(), 10).is_empty());

    // Permission removed keeps draft pending without persisting.
    let mut ro = FileSaveCoordinator::new(500);
    let effects = ro.change("unsaved".into(), 0);
    let tok = match effects.last() {
        Some(SaveEffect::ScheduleDebounce { token, .. }) => *token,
        other => panic!("expected debounce: {other:?}"),
    };
    let effects = ro.on_debounce_fired(tok, false);
    assert_eq!(effects, vec![SaveEffect::Settled]);
    assert_eq!(ro.unconfirmed_contents(), Some("unsaved"));

    // Session store: unsaved edits survive release when read-only and recover when writable returns.
    let mut store = FileSaveSessionStore::new(500);
    let target = FileSaveTarget::new("env-1", "/repo", "a.txt");
    store.acquire(&target, false, 0);
    let _ = store.change(&target, "draft v1".into(), 10);
    assert_eq!(store.unsaved_contents(&target), Some("draft v1"));
    let _ = store.release(&target, false);
    assert_eq!(store.active_session_count(), 0);
    assert_eq!(store.unsaved_contents(&target), Some("draft v1"));

    // Reacquire when writable recovers unsaved draft automatically.
    let recovered = store.acquire(&target, true, 100);
    assert!(recovered.iter().any(|e| matches!(e.effect, SaveEffect::ScheduleDebounce { .. })));

    // Dismissal decisions.
    assert_eq!(decide_escape_dismissal(false, false), DismissalOutcome::Ignore);
    assert_eq!(decide_escape_dismissal(true, false), DismissalOutcome::DismissNow);
    assert_eq!(decide_escape_dismissal(true, true), DismissalOutcome::PromptUnsaved);
    assert_eq!(decide_outside_pointer_dismissal(true, true), DismissalOutcome::Ignore);
    assert_eq!(decide_outside_pointer_dismissal(false, true), DismissalOutcome::PromptUnsaved);
    assert_eq!(decide_leave_editor(false, false), DismissalOutcome::DismissNow);
    assert_eq!(decide_leave_editor(true, false), DismissalOutcome::PromptUnsaved);
}

#[test]
fn delimited_table_parses_rfc4180_sorts_and_resizes() {
    use super::delimited::{DelimitedLimits, DelimitedTable, MAX_COLUMN_WIDTH, MIN_COLUMN_WIDTH, SortDirection, column_label};

    assert_eq!(column_label(0), "A");
    assert_eq!(column_label(25), "Z");
    assert_eq!(column_label(26), "AA");

    let csv = "\u{feff}name,score,note\r\nAlice,100,\"hello, \"\"world\"\"\"\r\nBob,20,\"line1\nline2\"\r\ncarol,5,\r\n";
    let mut table = DelimitedTable::parse(csv, ',', DelimitedLimits::default());
    assert!(!table.truncated);
    assert_eq!(table.header, vec!["name", "score", "note"]);
    assert_eq!(table.row_count(), 3);
    assert_eq!(table.display_row(0).unwrap().1[2], "hello, \"world\"");
    assert_eq!(table.display_row(1).unwrap().1[2], "line1\nline2");

    // Numeric sort ascending: 5, 20, 100.
    table.toggle_sort(1);
    assert_eq!(table.sort.unwrap().direction, SortDirection::Ascending);
    assert_eq!(table.display_row(0).unwrap().1[0], "carol");
    assert_eq!(table.display_row(1).unwrap().1[0], "Bob");
    assert_eq!(table.display_row(2).unwrap().1[0], "Alice");

    // Descending: 100, 20, 5.
    table.toggle_sort(1);
    assert_eq!(table.sort.unwrap().direction, SortDirection::Descending);
    assert_eq!(table.display_row(0).unwrap().1[0], "Alice");

    // Third click resets to natural order.
    table.toggle_sort(1);
    assert!(table.sort.is_none());
    assert_eq!(table.display_row(0).unwrap().1[0], "Alice");
    assert_eq!(table.display_row(1).unwrap().1[0], "Bob");

    // Resize clamps to bounds.
    table.resize_column(0, 10.0);
    assert_eq!(table.column_widths[0], MIN_COLUMN_WIDTH);
    table.resize_column(0, 2000.0);
    assert_eq!(table.column_widths[0], MAX_COLUMN_WIDTH);
}

#[test]
fn binary_hex_signature_and_archive_inspection() {
    use super::binary::{
        ArchiveEntry, HexDocument, archive_children, archive_parent_folder, detect_binary_signature, format_hex_row,
        list_zip_entries, parse_sqlite_header, read_zip_entry,
    };
    use std::io::{Cursor, Write};

    let row = format_hex_row(0x10, b"Hello, world!\x00\x7f\x01");
    assert_eq!(row.offset_label, "00000010");
    assert_eq!(row.hex_left, "48 65 6c 6c 6f 2c 20 77");
    assert_eq!(row.hex_right, "6f 72 6c 64 21 00 7f 01");
    assert_eq!(row.ascii, "Hello, world!...");

    let doc = HexDocument::from_bytes(b"\x89PNG\r\n\x1a\nrest", None);
    assert_eq!(doc.signature, "PNG image");
    assert_eq!(doc.row_count(), 1);
    assert!(!doc.truncated);

    // SQLite 3 header detection.
    let mut sqlite = vec![0u8; 100];
    sqlite[..16].copy_from_slice(b"SQLite format 3\0");
    sqlite[16..18].copy_from_slice(&4096u16.to_be_bytes());
    sqlite[28..32].copy_from_slice(&12u32.to_be_bytes());
    sqlite[56..60].copy_from_slice(&1u32.to_be_bytes());
    assert_eq!(detect_binary_signature(&sqlite), "SQLite 3 database");
    let header = parse_sqlite_header(&sqlite).unwrap();
    assert_eq!((header.page_size, header.page_count, header.text_encoding), (4096, 12, "UTF-8"));

    // ZIP creation, listing, folder navigation, and entry extraction.
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::FileOptions::default();
        writer.start_file("dir/hello.txt", opts).unwrap();
        writer.write_all(b"hi from zip").unwrap();
        writer.start_file("README.md", opts).unwrap();
        writer.write_all(b"# Title").unwrap();
        writer.finish().unwrap();
    }
    let zip_bytes = cursor.into_inner();
    let entries = list_zip_entries(&zip_bytes).unwrap();
    assert_eq!(entries.len(), 2);
    let root_children = archive_children(&entries, "");
    assert_eq!(root_children.len(), 2);
    assert!(root_children[0].directory && root_children[0].name == "dir");
    assert!(!root_children[1].directory && root_children[1].name == "README.md");
    assert_eq!(archive_parent_folder("a/b/"), "a/");
    assert_eq!(archive_parent_folder("a/"), "");
    assert_eq!(read_zip_entry(&zip_bytes, "dir/hello.txt").unwrap(), b"hi from zip");

    let synthetic = vec![
        ArchiveEntry { path: "sub/a.txt".into(), size: 10, directory: false, modified: None },
        ArchiveEntry { path: "sub/b.txt".into(), size: 25, directory: false, modified: None },
    ];
    let top = archive_children(&synthetic, "");
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].size, 35);
}

#[test]
fn notebook_parser_and_inert_markdown_sanitizer() {
    use super::notebook::{NotebookCellKind, NotebookOutput, parse_notebook, sanitize_notebook_markdown, strip_ansi};
    use base64::Engine as _;

    let png_b64 = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\n");
    let nb = json!({
        "nbformat": 4,
        "metadata": { "language_info": { "name": "python" } },
        "cells": [
            {
                "cell_type": "markdown",
                "source": ["# Heading\n", "[click](https://evil.test) ![pic](https://evil.test/a.png) <script>x</script>"]
            },
            {
                "cell_type": "code",
                "execution_count": 3,
                "source": ["print('ok')\n"],
                "outputs": [
                    { "output_type": "stream", "text": ["\x1b[31mok\x1b[0m\n"] },
                    { "output_type": "display_data", "data": { "image/png": png_b64 } },
                    { "output_type": "display_data", "data": { "text/html": "<table><tr><td>A</td><td>B</td></tr></table>" } }
                ]
            }
        ]
    });
    let doc = parse_notebook(nb.to_string().as_bytes()).unwrap();
    assert_eq!(doc.language, "python");
    assert_eq!(doc.syntax_language(), Some("python"));
    assert_eq!(doc.cells.len(), 2);
    assert_eq!(doc.cells[0].kind, NotebookCellKind::Markdown);
    assert_eq!(doc.cells[1].execution_count, Some(3));
    assert_eq!(doc.cells[1].outputs.len(), 3);
    assert_eq!(doc.cells[1].outputs[0], NotebookOutput::Text("ok\n".into()));
    assert!(matches!(doc.cells[1].outputs[1], NotebookOutput::Image { .. }));
    assert!(matches!(&doc.cells[1].outputs[2], NotebookOutput::Html(t) if t.contains("A") && t.contains("B")));

    let inert = sanitize_notebook_markdown("See [docs](https://x.test) and ![logo](https://x.test/a.png) <b>bold</b>\n```mermaid\ngraph TD\n```");
    assert!(!inert.contains("https://x.test"));
    assert!(!inert.contains("<b>"));
    assert!(inert.contains("See docs and logo bold"));
    assert!(inert.contains("```text"));
    assert_eq!(strip_ansi("\x1b[1;31merr\x1b[0m"), "err");

    assert!(parse_notebook(b"{\"nbformat\": 3, \"cells\": []}").is_err());
}

#[test]
fn mermaid_guard_audio_font_and_drag_mention_helpers() {
    use super::media_preview::{
        DragMentionController, FontFormat, format_audio_time, inspect_audio_bytes, parse_font_info,
    };
    use super::mermaid::{MermaidFileOutcome, contains_unsafe_mermaid_directives, render_mermaid_file};

    assert!(!contains_unsafe_mermaid_directives("flowchart LR\n  A[Start] --> B[End]"));
    assert!(contains_unsafe_mermaid_directives("%%{init: {'theme': 'dark'}}%%\nflowchart LR\n  A --> B"));
    assert!(contains_unsafe_mermaid_directives("flowchart LR\n  A[https://evil.test] --> B"));
    assert!(contains_unsafe_mermaid_directives("---\ntitle: front\n---\nflowchart LR\n  A --> B"));
    assert!(matches!(
        render_mermaid_file("flowchart LR\n  A --> B", true),
        MermaidFileOutcome::Rendered(_)
    ));
    assert!(matches!(
        render_mermaid_file("flowchart LR\n  A[<img src=x>] --> B", false),
        MermaidFileOutcome::UnsafeDirective(_)
    ));

    assert_eq!(format_audio_time(0.0), "0:00");
    assert_eq!(format_audio_time(65.9), "1:05");
    assert_eq!(format_audio_time(-5.0), "0:00");

    // Minimal valid 44-byte WAV header (44100 Hz, 2ch, 16-bit, 1 second = 176400 bytes).
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&36u32.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&2u16.to_le_bytes()); // channels
    wav.extend_from_slice(&44100u32.to_le_bytes()); // sample rate
    wav.extend_from_slice(&176400u32.to_le_bytes()); // byte rate
    wav.extend_from_slice(&4u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&176400u32.to_le_bytes());
    let audio = inspect_audio_bytes("clip.wav", &wav);
    assert_eq!(audio.format, "WAV");
    assert_eq!((audio.sample_rate, audio.channels, audio.bits_per_sample), (Some(44100), Some(2), Some(16)));
    assert_eq!(audio.duration_secs, Some(1.0));

    // Minimal TrueType header with maxp & head.
    let mut ttf = vec![0u8; 64];
    ttf[0..4].copy_from_slice(&[0x00, 0x01, 0x00, 0x00]);
    let font = parse_font_info(&ttf).unwrap();
    assert_eq!(font.format, FontFormat::TrueType);
    assert!(font.format.native_loadable());
    assert!(parse_font_info(b"not-a-font-at-all").is_err());

    // Drag-to-mention controller matches fileTreeDragMention.test.ts.
    let mut drag = DragMentionController::default();
    let single = drag.start_drag(Some("docs/index.md")).unwrap();
    assert_eq!(single.mention, "[index.md](docs/index.md)");
    assert!(drag.is_drag_in_progress());
    assert_eq!(drag.end_drag(), vec!["docs/index.md"]);
    assert!(!drag.is_drag_in_progress());

    drag.set_selection(vec!["docs/index.md".into(), "docs/api.md".into(), "src/app.ts".into()]);
    let multi = drag.start_drag(Some("docs/api.md")).unwrap();
    assert_eq!(multi.mention, "[index.md](docs/index.md) [api.md](docs/api.md) [app.ts](src/app.ts)");
    assert_eq!(multi.label, "3 files");
    assert_eq!(drag.end_drag(), vec!["docs/index.md", "docs/api.md", "src/app.ts"]);
}
