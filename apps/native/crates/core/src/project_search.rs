//! Go-to-file and project content search: `projects.searchEntries` and `projects.searchContents`
//! payloads, `getProjectFilePickerMatches` (`apps/web/src/components/files/ProjectFilePicker.logic.ts`),
//! the content dialog's grouping and status line (`components/search/ProjectContentSearchDialog.tsx`),
//! and the preferred editor (`apps/web/src/editorPreferences.ts`, `EDITORS` order).

use serde::Deserialize;
use serde_json::{Value, json};

pub const SEARCH_ENTRIES: &str = "projects.searchEntries";
pub const SEARCH_CONTENTS: &str = "projects.searchContents";
pub const OPEN_IN_EDITOR: &str = "shell.openInEditor";
pub const FILE_PICKER_RESULT_LIMIT: usize = 200;
pub const CONTENT_SEARCH_LIMIT: usize = 500;
/// Both web queries debounce by this much.
pub const SEARCH_DEBOUNCE_MS: u64 = 120;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProjectEntry {
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct EntriesResult {
    #[serde(default)]
    pub entries: Vec<ProjectEntry>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct MatchRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMatch {
    pub path: String,
    pub line_number: u64,
    pub line_content: String,
    #[serde(default)]
    pub match_ranges: Vec<MatchRange>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentsResult {
    #[serde(default)]
    pub matches: Vec<ContentMatch>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub regex_fallback_error: Option<String>,
}

/// `useProjectPathSearch` for files: the query trimmed, an empty one lists files.
pub fn entries_request(cwd: &str, query: &str) -> Value {
    json!({ "cwd": cwd, "query": query.trim(), "limit": FILE_PICKER_RESULT_LIMIT, "kind": "file" })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContentOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
}

/// `useProjectContentSearch`: whitespace is significant, so the query is sent as typed; a blank
/// one sends nothing.
pub fn contents_request(cwd: &str, query: &str, options: ContentOptions) -> Option<Value> {
    (!query.trim().is_empty()).then(|| {
        json!({
            "cwd": cwd,
            "query": query,
            "limit": CONTENT_SEARCH_LIMIT,
            "caseSensitive": options.case_sensitive,
            "wholeWord": options.whole_word,
            "useRegex": options.use_regex,
        })
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePickerMatch {
    pub name: String,
    pub name_match_indices: Vec<usize>,
    pub path: String,
    pub path_match_indices: Vec<usize>,
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Char indices of `query` as a subsequence of `value`, case-insensitively; `None` when absent.
fn find_match_indices(value: &str, query: &[char]) -> Option<Vec<usize>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let mut indices = Vec::new();
    for (index, c) in value.chars().flat_map(char::to_lowercase).enumerate() {
        if c == query[indices.len()] {
            indices.push(index);
            if indices.len() == query.len() {
                return Some(indices);
            }
        }
    }
    None
}

/// `getProjectFilePickerMatches`: server order kept, files only, highlights where the folded query
/// (leading `@`, `.` and `/` dropped, whitespace removed) is a subsequence.
pub fn file_picker_matches(entries: &[ProjectEntry], raw_query: &str) -> Vec<FilePickerMatch> {
    let query: Vec<char> =
        raw_query.trim().trim_start_matches(['@', '.', '/']).to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    entries
        .iter()
        .filter(|entry| entry.kind == "file")
        .take(FILE_PICKER_RESULT_LIMIT)
        .map(|entry| {
            let name = file_name(&entry.path);
            FilePickerMatch {
                name: name.to_owned(),
                name_match_indices: find_match_indices(name, &query).unwrap_or_default(),
                path: entry.path.clone(),
                path_match_indices: find_match_indices(&entry.path, &query).unwrap_or_default(),
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchGroup {
    pub path: String,
    /// Indices into the flat match list.
    pub matches: Vec<usize>,
}

/// `groupMatches`: one group per file, in first-seen order.
pub fn group_matches(matches: &[ContentMatch]) -> Vec<MatchGroup> {
    let mut groups: Vec<MatchGroup> = Vec::new();
    for (index, found) in matches.iter().enumerate() {
        match groups.iter_mut().find(|group| group.path == found.path) {
            Some(group) => group.matches.push(index),
            None => groups.push(MatchGroup { path: found.path.clone(), matches: vec![index] }),
        }
    }
    groups
}

/// `splitPath`: the file name and its directory.
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(separator) => (&path[separator + 1..], &path[..separator]),
        None => (path, ""),
    }
}

fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The status line: "12+ results in 3 files".
pub fn results_summary(result: &ContentsResult) -> String {
    let files = group_matches(&result.matches).len();
    format!("{}{} results in {} files", thousands(result.matches.len()), if result.truncated { "+" } else { "" }, thousands(files))
}

/// `EDITORS` order, which picks the editor when none was chosen before.
pub const EDITOR_ORDER: [(&str, &str); 21] = [
    ("cursor", "Cursor"),
    ("trae", "Trae"),
    ("kiro", "Kiro"),
    ("vscode", "VS Code"),
    ("vscode-insiders", "VS Code Insiders"),
    ("vscodium", "VSCodium"),
    ("zed", "Zed"),
    ("antigravity", "Antigravity"),
    ("idea", "IntelliJ IDEA"),
    ("aqua", "Aqua"),
    ("clion", "CLion"),
    ("datagrip", "DataGrip"),
    ("dataspell", "DataSpell"),
    ("goland", "GoLand"),
    ("phpstorm", "PhpStorm"),
    ("pycharm", "PyCharm"),
    ("rider", "Rider"),
    ("rubymine", "RubyMine"),
    ("rustrover", "RustRover"),
    ("webstorm", "WebStorm"),
    ("file-manager", "File Manager"),
];

/// `resolveAndPersistPreferredEditor` without a stored choice: the first available editor.
pub fn preferred_editor(available: &[String], last: Option<&str>) -> Option<(&'static str, &'static str)> {
    if let Some(last) = last.filter(|last| available.iter().any(|id| id == last))
        && let Some(found) = EDITOR_ORDER.iter().find(|(id, _)| *id == last)
    {
        return Some(*found);
    }
    EDITOR_ORDER.iter().copied().find(|(id, _)| available.iter().any(|candidate| candidate == id))
}

/// The `shell.openInEditor` target: the file under `cwd`, with `:line` the launcher parses.
pub fn editor_target(cwd: &str, relative_path: &str, line: Option<u64>) -> String {
    let path = format!("{}/{}", cwd.trim_end_matches('/'), relative_path.trim_start_matches('/'));
    match line {
        Some(line) => format!("{path}:{line}"),
        None => path,
    }
}

pub fn open_in_editor_request(target: &str, editor: &str) -> Value {
    json!({ "cwd": target, "editor": editor })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: &str) -> ProjectEntry {
        ProjectEntry { path: path.into(), kind: kind.into() }
    }

    #[test]
    fn file_matches_keep_server_order_and_highlight_subsequences() {
        let entries = [entry("src/app.rs", "file"), entry("src", "directory"), entry("docs/apple.md", "file")];
        let matches = file_picker_matches(&entries, " @./ap ");
        assert_eq!(matches.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(), ["src/app.rs", "docs/apple.md"]);
        assert_eq!(matches[0].name, "app.rs");
        assert_eq!(matches[0].name_match_indices, [0, 1]);
        assert_eq!(matches[0].path_match_indices, [4, 5]);
        let unmatched = file_picker_matches(&[entry("src/zed.rs", "file")], "xyz");
        assert!(unmatched[0].name_match_indices.is_empty());
        assert!(file_picker_matches(&entries, "a p").iter().all(|m| !m.name_match_indices.is_empty()));
    }

    #[test]
    fn requests_follow_the_web_queries() {
        assert_eq!(entries_request("/w", "  app "), json!({"cwd": "/w", "query": "app", "limit": 200, "kind": "file"}));
        assert_eq!(contents_request("/w", "   ", ContentOptions::default()), None);
        let request = contents_request("/w", " fn ", ContentOptions { case_sensitive: true, whole_word: false, use_regex: true }).unwrap();
        assert_eq!(request["query"], " fn ");
        assert_eq!(request["caseSensitive"], true);
        assert_eq!(request["limit"], 500);
    }

    #[test]
    fn content_results_group_by_file_and_summarize() {
        let found = |path: &str, line: u64| ContentMatch { path: path.into(), line_number: line, line_content: String::new(), match_ranges: vec![] };
        let result = ContentsResult { matches: vec![found("a.rs", 1), found("b.rs", 2), found("a.rs", 9)], truncated: true, regex_fallback_error: None };
        let groups = group_matches(&result.matches);
        assert_eq!(groups, [MatchGroup { path: "a.rs".into(), matches: vec![0, 2] }, MatchGroup { path: "b.rs".into(), matches: vec![1] }]);
        assert_eq!(results_summary(&result), "3+ results in 2 files");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(split_path("src/ui/app.rs"), ("app.rs", "src/ui"));
        assert_eq!(split_path("README"), ("README", ""));
        let decoded: ContentsResult = serde_json::from_value(json!({
            "matches": [{"path": "a.rs", "lineNumber": 3, "lineContent": "let x", "matchRanges": [{"start": 4, "end": 5}]}],
            "truncated": false, "regexFallbackError": "bad"
        }))
        .unwrap();
        assert_eq!(decoded.matches[0].match_ranges, [MatchRange { start: 4, end: 5 }]);
        assert_eq!(decoded.regex_fallback_error.as_deref(), Some("bad"));
    }

    #[test]
    fn preferred_editor_follows_the_editor_order() {
        let available = vec!["zed".to_owned(), "vscode".to_owned()];
        assert_eq!(preferred_editor(&available, None), Some(("vscode", "VS Code")));
        assert_eq!(preferred_editor(&available, Some("zed")), Some(("zed", "Zed")));
        assert_eq!(preferred_editor(&available, Some("cursor")), Some(("vscode", "VS Code")));
        assert_eq!(preferred_editor(&[], None), None);
        assert_eq!(editor_target("/w/", "src/a.rs", Some(12)), "/w/src/a.rs:12");
        assert_eq!(editor_target("/w", "src/a.rs", None), "/w/src/a.rs");
    }
}
