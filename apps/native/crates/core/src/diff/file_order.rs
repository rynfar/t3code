//! Shared review order for diff and pull request files (`packages/client-runtime/src/diffFileOrder.ts`,
//! `pingdotgg/t3code#26760`).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use super::{FileDiff, LineKind};

#[cfg(test)]
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffFileTier {
    Source,
    Test,
    Generated,
}

const GENERATED_FILE_NAMES: &[&str] = &[
    "pnpm-lock.yaml",
    "yarn.lock",
    "package-lock.json",
    "bun.lockb",
    "Cargo.lock",
    "go.sum",
    "composer.lock",
    "Gemfile.lock",
];

const GENERATED_DIRECTORIES: &[&str] = &[
    "__snapshots__",
    "__generated__",
    "dist",
    "build",
    "vendor",
];

const TEST_DIRECTORIES: &[&str] = &["__tests__", "tests", "test"];

// `.mts` also ends in `.ts`, so longer extensions come first.
const MODULE_EXTENSIONS: &[&str] = &[".mts", ".cts", ".mjs", ".cjs", ".tsx", ".jsx", ".ts", ".js"];

static GENERATED_NAME_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.generated\.").unwrap());
static TEST_NAME_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.(?:test|spec)\.").unwrap());
static TEST_SUFFIX_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.(?:test|spec)\..*$").unwrap());
static IMPORT_SPECIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:\bfrom|\bimport|\brequire\s*\()\s*\(?\s*["']([^"']+)["']"#).unwrap());
static QUERY_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\?.*$").unwrap());

fn cmp_locale(left: &str, right: &str) -> Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| left.cmp(right))
}

fn diff_file_tier(path: &str) -> DiffFileTier {
    let segments: Vec<&str> = path.split('/').collect();
    let name = segments.last().copied().unwrap_or("");
    let parent_segments = &segments[..segments.len().saturating_sub(1)];
    if GENERATED_FILE_NAMES.contains(&name)
        || name.ends_with(".snap")
        || name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || GENERATED_NAME_PATTERN.is_match(name)
        || parent_segments
            .iter()
            .any(|segment| GENERATED_DIRECTORIES.contains(segment))
    {
        return DiffFileTier::Generated;
    }
    if TEST_NAME_PATTERN.is_match(name)
        || parent_segments
            .iter()
            .any(|segment| TEST_DIRECTORIES.contains(segment))
    {
        return DiffFileTier::Test;
    }
    DiffFileTier::Source
}

fn strip_extension(path: &str) -> &str {
    let dot = path.rfind('.');
    let slash = path.rfind('/');
    match (dot, slash) {
        (Some(dot), Some(slash)) if dot > slash => &path[..dot],
        (Some(dot), None) => &path[..dot],
        _ => path,
    }
}

fn base_name(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or("");
    strip_extension(name)
}

fn strip_module_extension(path: &str) -> &str {
    for ext in MODULE_EXTENSIONS {
        if let Some(stripped) = path.strip_suffix(ext) {
            return stripped;
        }
    }
    path
}

fn module_name(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or("");
    strip_module_extension(name)
}

fn resolve_relative(from_path: &str, specifier: &str) -> String {
    let mut segments: Vec<&str> = from_path.split('/').collect();
    segments.pop();
    for part in specifier.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            segments.pop();
        } else {
            segments.push(part);
        }
    }
    segments.join("/")
}

fn imported_paths(
    path: &str,
    lines: &[String],
    by_module_path: &HashMap<String, String>,
    by_module_name: &HashMap<String, String>,
) -> HashSet<String> {
    let mut imported = HashSet::new();
    for line in lines {
        for captures in IMPORT_SPECIFIER.captures_iter(line) {
            let raw = captures.get(1).map_or("", |m| m.as_str());
            let specifier = QUERY_SUFFIX.replace(raw, "");
            let joined = if specifier.starts_with('.') {
                resolve_relative(path, &specifier)
            } else {
                specifier.into_owned()
            };
            let resolved = strip_module_extension(&joined);
            let index_key = format!("{resolved}/index");
            let target = by_module_path
                .get(resolved)
                .or_else(|| by_module_path.get(&index_key))
                .or_else(|| by_module_name.get(module_name(resolved)));
            if let Some(target) = target
                && target != path
            {
                imported.insert(target.clone());
            }
        }
    }
    imported
}

fn order_by_imports(paths: &[String], imports: &HashMap<String, HashSet<String>>) -> Vec<String> {
    let mut remaining: HashSet<String> = paths.iter().cloned().collect();
    let mut sorted = paths.to_vec();
    sorted.sort_by(|a, b| cmp_locale(a, b));
    let mut ordered = Vec::with_capacity(paths.len());
    while !remaining.is_empty() {
        let next = sorted
            .iter()
            .filter(|path| remaining.contains(*path))
            .find(|path| {
                imports
                    .get(*path)
                    .is_none_or(|deps| deps.iter().all(|dep| !remaining.contains(dep)))
            })
            .or_else(|| sorted.iter().find(|path| remaining.contains(*path)))
            .cloned();
        let Some(next) = next else { break };
        remaining.remove(&next);
        ordered.push(next);
    }
    ordered
}

fn tested_source_position(path: &str, source_positions: &HashMap<String, usize>) -> usize {
    let file_name = path.rsplit('/').next().unwrap_or("");
    let mut name = TEST_SUFFIX_PATTERN.replace(file_name, "").into_owned();
    loop {
        if let Some(&position) = source_positions.get(&name) {
            return position;
        }
        match name.rfind('.') {
            Some(dot) if dot > 0 => name.truncate(dot),
            _ => return usize::MAX,
        }
    }
}

/// Where each file, and each directory (with a trailing slash), first appears in `paths`
/// (`diffFileTreePositions`).
pub fn diff_file_tree_positions(paths: &[&str]) -> HashMap<String, usize> {
    let mut positions = HashMap::new();
    for (index, path) in paths.iter().enumerate() {
        positions.insert((*path).to_owned(), index);
        let segments: Vec<&str> = path.split('/').collect();
        let mut directory = String::new();
        for segment in &segments[..segments.len().saturating_sub(1)] {
            directory.push_str(segment);
            directory.push('/');
            positions.entry(directory.clone()).or_insert(index);
        }
    }
    positions
}

fn order_generic<T>(
    files: Vec<T>,
    path_of: impl Fn(&T) -> &str,
    lines_of: impl Fn(&T) -> Vec<String>,
) -> Vec<T> {
    if files.len() <= 1 {
        return files;
    }

    // Group preserving insertion order so multiple entries for one path (delete + add on type change) stay paired.
    let mut path_order: Vec<String> = Vec::new();
    let mut by_path: HashMap<String, Vec<T>> = HashMap::new();
    for file in files {
        let path = path_of(&file).to_owned();
        if !by_path.contains_key(&path) {
            path_order.push(path.clone());
        }
        by_path.entry(path).or_default().push(file);
    }

    let mut tiers: HashMap<String, DiffFileTier> = HashMap::with_capacity(path_order.len());
    for path in &path_order {
        tiers.insert(path.clone(), diff_file_tier(path));
    }

    let source_paths: Vec<String> = path_order
        .iter()
        .filter(|path| tiers.get(*path) == Some(&DiffFileTier::Source))
        .cloned()
        .collect();

    let mut by_module_path: HashMap<String, String> = HashMap::new();
    let mut by_module_name: HashMap<String, String> = HashMap::new();
    let mut ambiguous_names: HashSet<String> = HashSet::new();
    for path in &source_paths {
        by_module_path.insert(strip_module_extension(path).to_owned(), path.clone());
        let name = module_name(path).to_owned();
        if by_module_name.contains_key(&name) {
            ambiguous_names.insert(name.clone());
        }
        by_module_name.insert(name, path.clone());
    }
    for name in ambiguous_names {
        by_module_name.remove(&name);
    }

    let mut imports: HashMap<String, HashSet<String>> = HashMap::new();
    for path in &source_paths {
        let lines: Vec<String> = by_path
            .get(path)
            .map(|group| group.iter().flat_map(&lines_of).collect())
            .unwrap_or_default();
        imports.insert(
            path.clone(),
            imported_paths(path, &lines, &by_module_path, &by_module_name),
        );
    }
    let ordered_source = order_by_imports(&source_paths, &imports);

    let mut source_positions: HashMap<String, usize> = HashMap::new();
    for (index, path) in ordered_source.iter().enumerate() {
        let base = base_name(path).to_owned();
        source_positions.entry(base).or_insert(index);
    }

    let mut ordered_tests: Vec<String> = path_order
        .iter()
        .filter(|path| tiers.get(*path) == Some(&DiffFileTier::Test))
        .cloned()
        .collect();
    ordered_tests.sort_by(|left, right| {
        tested_source_position(left, &source_positions)
            .cmp(&tested_source_position(right, &source_positions))
            .then_with(|| cmp_locale(left, right))
    });

    let mut ordered_generated: Vec<String> = path_order
        .iter()
        .filter(|path| tiers.get(*path) == Some(&DiffFileTier::Generated))
        .cloned()
        .collect();
    ordered_generated.sort_by(|left, right| cmp_locale(left, right));

    let mut tiered: Vec<T> = Vec::new();
    for path in ordered_source
        .into_iter()
        .chain(ordered_tests)
        .chain(ordered_generated)
    {
        if let Some(group) = by_path.remove(&path) {
            tiered.extend(group);
        }
    }

    // Regroup by directory positions (`orderLikeTree`) so each directory stays contiguous.
    let path_strings: Vec<String> = tiered.iter().map(|file| path_of(file).to_owned()).collect();
    let path_refs: Vec<&str> = path_strings.iter().map(String::as_str).collect();
    let positions = diff_file_tree_positions(&path_refs);
    let mut indexed: Vec<(Vec<usize>, T)> = tiered
        .into_iter()
        .zip(path_strings)
        .map(|(file, path)| {
            let segments: Vec<&str> = path.split('/').collect();
            let mut key = Vec::with_capacity(segments.len());
            let mut directory = String::new();
            for segment in &segments[..segments.len().saturating_sub(1)] {
                directory.push_str(segment);
                directory.push('/');
                key.push(*positions.get(&directory).unwrap_or(&usize::MAX));
            }
            key.push(*positions.get(&path).unwrap_or(&usize::MAX));
            (key, file)
        })
        .collect();
    indexed.sort_by(|(left, _), (right, _)| left.cmp(right));
    indexed.into_iter().map(|(_, file)| file).collect()
}

/// Orders `FileDiff`s in the shared review order (`orderDiffFilesForReview`): dependencies before
/// dependents, tests after their source, generated files last, then regrouped so each directory
/// stays contiguous and matches `diff_file_tree_positions`.
pub fn order_file_diffs_for_review(files: Vec<FileDiff>) -> Vec<FileDiff> {
    order_generic(
        files,
        |file| file.path(),
        |file| {
            file.hunks
                .iter()
                .flat_map(|hunk| hunk.lines.iter())
                .filter(|line| matches!(line.kind, LineKind::Addition | LineKind::Deletion))
                .map(|line| line.text.as_ref().to_owned())
                .collect()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{ChangeKind, DiffLine, FileHunks, Hunk};

    fn make_diff(path: &str, lines: Vec<(LineKind, &str)>) -> FileDiff {
        let diff_lines: Vec<DiffLine> = lines
            .into_iter()
            .enumerate()
            .map(|(ix, (kind, text))| DiffLine {
                kind,
                old_line: (kind != LineKind::Addition).then_some(ix as u32 + 1),
                new_line: (kind != LineKind::Deletion).then_some(ix as u32 + 1),
                text: Arc::from(text),
                source_text: Arc::from(text),
                no_newline_at_eof: false,
            })
            .collect();
        FileDiff {
            old_path: Some(path.to_owned()),
            new_path: Some(path.to_owned()),
            change: ChangeKind::Modified,
            binary: false,
            hunks: FileHunks::from(vec![Hunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 1,
                context: String::new(),
                lines: diff_lines,
            }]),
            additions: 1,
            deletions: 0,
        }
    }

    fn paths(files: Vec<FileDiff>) -> Vec<String> {
        order_file_diffs_for_review(files)
            .into_iter()
            .map(|f| f.path().to_owned())
            .collect()
    }

    #[test]
    fn places_source_before_tests_and_generated_and_groups_by_directory() {
        let input = vec![
            make_diff("pnpm-lock.yaml", vec![]),
            make_diff("dist/app.js", vec![]),
            make_diff("src/app.test.ts.snap", vec![]),
            make_diff("src/api.generated.ts", vec![]),
            make_diff("src/__snapshots__/app.ts", vec![]),
            make_diff("tests/helpers/app.ts", vec![]),
            make_diff("test/app.ts", vec![]),
            make_diff("src/app.test.ts", vec![]),
            make_diff("src/app.spec.tsx", vec![]),
            make_diff("src/__tests__/app.ts", vec![]),
            make_diff("src/testing.ts", vec![]),
            make_diff("src/dist.ts", vec![]),
            make_diff("src/app.ts", vec![]),
        ];
        assert_eq!(
            paths(input),
            vec![
                "src/app.ts",
                "src/dist.ts",
                "src/testing.ts",
                "src/__tests__/app.ts",
                "src/app.spec.tsx",
                "src/app.test.ts",
                "src/__snapshots__/app.ts",
                "src/api.generated.ts",
                "src/app.test.ts.snap",
                "test/app.ts",
                "tests/helpers/app.ts",
                "dist/app.js",
                "pnpm-lock.yaml",
            ]
        );
    }

    #[test]
    fn keeps_both_entries_for_same_path_on_type_change_and_regroups_tests() {
        let deleted = FileDiff {
            change: ChangeKind::Deleted,
            ..make_diff("AGENTS.md", vec![(LineKind::Deletion, "# Agents")])
        };
        let added = FileDiff {
            change: ChangeKind::New,
            ..make_diff("AGENTS.md", vec![(LineKind::Addition, "CLAUDE.md")])
        };
        let ordered = order_file_diffs_for_review(vec![
            make_diff("CLAUDE.md", vec![]),
            deleted.clone(),
            added.clone(),
        ]);
        assert_eq!(ordered, vec![deleted, added, make_diff("CLAUDE.md", vec![])]);

        assert_eq!(
            paths(vec![
                make_diff("server/device/Host.ts", vec![]),
                make_diff("web/settings.ts", vec![]),
                make_diff("server/ws.ts", vec![]),
                make_diff("server/device/Host.test.ts", vec![]),
                make_diff("web/settings.test.ts", vec![]),
            ]),
            vec![
                "server/device/Host.ts",
                "server/device/Host.test.ts",
                "server/ws.ts",
                "web/settings.ts",
                "web/settings.test.ts",
            ]
        );
    }

    #[test]
    fn orders_dependencies_before_dependents_across_import_styles() {
        assert_eq!(
            paths(vec![
                make_diff("src/ui/a.ts", vec![(LineKind::Addition, r#"import { b } from "../lib/b";"#)]),
                make_diff("src/lib/b.ts", vec![]),
            ]),
            vec!["src/lib/b.ts", "src/ui/a.ts"]
        );

        assert_eq!(
            paths(vec![
                make_diff("src/a.ts", vec![(LineKind::Addition, r#"import { b } from "./lib";"#)]),
                make_diff("src/lib/index.ts", vec![]),
            ]),
            vec!["src/lib/index.ts", "src/a.ts"]
        );

        assert_eq!(
            paths(vec![
                make_diff("a.mts", vec![(LineKind::Addition, r#"import { z } from "./z.mjs";"#)]),
                make_diff("z.mts", vec![]),
            ]),
            vec!["z.mts", "a.mts"]
        );

        assert_eq!(
            paths(vec![
                make_diff("src/a.ts", vec![(LineKind::Addition, r#"import { b } from "~/z/b.logic";"#)]),
                make_diff("z/b.logic.ts", vec![]),
                make_diff("other/b.ts", vec![]),
            ]),
            vec!["other/b.ts", "z/b.logic.ts", "src/a.ts"]
        );

        // Ambiguous last segment does not force an incorrect edge.
        assert_eq!(
            paths(vec![
                make_diff("src/a.ts", vec![(LineKind::Addition, r#"import { b } from "~/somewhere/b";"#)]),
                make_diff("one/b.ts", vec![]),
                make_diff("two/b.ts", vec![]),
            ]),
            vec!["one/b.ts", "src/a.ts", "two/b.ts"]
        );

        // Query suffix stripped, subpath `#` kept, `require` and bare imports respected.
        assert_eq!(
            paths(vec![
                make_diff(
                    "src/a.ts",
                    vec![
                        (LineKind::Addition, r#"import Worker from "./z.js?worker";"#),
                        (LineKind::Addition, r#"const b = require("./b");"#),
                        (LineKind::Addition, r##"import { y } from "#internal/y";"##),
                    ],
                ),
                make_diff("src/b.ts", vec![]),
                make_diff("src/y.ts", vec![]),
                make_diff("src/z.ts", vec![]),
            ]),
            vec!["src/b.ts", "src/y.ts", "src/z.ts", "src/a.ts"]
        );
    }
}
