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
