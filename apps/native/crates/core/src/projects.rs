//! Adding projects: the browse-path helpers from `client-runtime/src/state/projects.ts` and
//! `filesystem.ts` (POSIX and Windows drive paths), and the `projects.mutate`,
//! `projects.createNew` and `filesystem.browse` payloads (`operations/commands.ts`).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::sidebar::grouping::normalize_project_path;

pub const PROJECTS_CREATE_FROM_THREAD: &str = "projects.createFromThread";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFromThreadResult { pub project_id: String }

pub fn create_from_thread(thread_id: &str) -> Value { json!({"threadId":thread_id}) }

pub const PROJECTS_MUTATE: &str = "projects.mutate";
pub const PROJECTS_CREATE_NEW: &str = "projects.createNew";
pub const FILESYSTEM_BROWSE: &str = "filesystem.browse";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseEntry {
    pub name: String,
    pub full_path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseResult {
    #[serde(default)]
    pub parent_path: String,
    #[serde(default)]
    pub entries: Vec<BrowseEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateNewResult {
    pub project_id: String,
    pub workspace_root: String,
    #[serde(default)]
    pub commit_error: Option<String>,
}

fn is_windows_drive(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes.len() == 2 || matches!(bytes[2], b'/' | b'\\'))
}

fn is_unix_absolute(value: &str) -> bool {
    value.starts_with('/')
}

/// `isFilesystemBrowseQuery`: a path-shaped query switches the palette to browsing.
pub fn is_browse_query(value: &str, windows: bool) -> bool {
    ["./", "../", ".\\", "..\\", "/", "~/"].iter().any(|prefix| value.starts_with(prefix)) || (windows && (is_windows_drive(value) || value.starts_with("\\\\")))
}

pub fn has_trailing_separator(value: &str) -> bool {
    if is_unix_absolute(value) { value.ends_with('/') } else { value.ends_with(['/', '\\']) }
}

fn last_separator(value: &str) -> Option<usize> {
    if is_unix_absolute(value) { value.rfind('/') } else { value.rfind(['/', '\\']) }
}

fn preferred_separator(value: &str) -> char {
    if is_windows_drive(value) || value.starts_with("\\\\") {
        '\\'
    } else if is_unix_absolute(value) || !value.contains('\\') {
        '/'
    } else {
        '\\'
    }
}

/// `getBrowseDirectoryPath`: the folder part of a typed path, with its trailing separator.
pub fn browse_directory_path(value: &str) -> &str {
    if has_trailing_separator(value) {
        return value;
    }
    match last_separator(value) {
        Some(index) => &value[..=index],
        None => value,
    }
}

/// `getBrowseLeafPathSegment`: what is typed after the last separator.
pub fn browse_leaf(value: &str) -> &str {
    last_separator(value).map_or(value, |index| &value[index + 1..])
}

/// `appendBrowsePathSegment`: enter a folder.
pub fn append_browse_segment(current: &str, segment: &str) -> String {
    format!("{}{segment}{}", browse_directory_path(current), preferred_separator(current))
}

/// `getBrowseParentPath` for POSIX paths: `None` at the root.
pub fn browse_parent_path(current: &str) -> Option<String> {
    let trimmed = current.trim().trim_end_matches('/');
    if !current.trim().starts_with('/') {
        let index = trimmed.rfind(['/', '\\'])?;
        return Some(trimmed[..=index].to_owned());
    }
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    match segments.len() {
        0 => None,
        1 => Some("/".to_owned()),
        n => Some(format!("/{}/", segments[..n - 1].join("/"))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowsePath {
    pub directory: String,
    pub filter: String,
}

/// `getFilesystemBrowsePath`: the folder to list and the leaf to filter its entries by.
pub fn browse_path(query: &str, windows: bool) -> Option<BrowsePath> {
    is_browse_query(query, windows).then(|| BrowsePath {
        directory: browse_directory_path(query).to_owned(),
        filter: if has_trailing_separator(query) { String::new() } else { browse_leaf(query).to_owned() },
    })
}

/// `filterFilesystemBrowseEntries`: prefix match, hidden folders only when the leaf starts with a dot.
pub fn filter_browse_entries<'a>(entries: &'a [BrowseEntry], filter: &str) -> Vec<&'a BrowseEntry> {
    let lower = filter.to_lowercase();
    let show_hidden = filter.starts_with('.');
    entries.iter().filter(|entry| entry.name.to_lowercase().starts_with(&lower) && (show_hidden || !entry.name.starts_with('.'))).collect()
}

/// `getFilesystemBrowseTabCompletion`: one match enters it, several extend to their common prefix.
pub fn browse_tab_completion(visible: &[&BrowseEntry], filter: &str) -> Option<(String, bool)> {
    let (first, rest) = visible.split_first()?;
    if rest.is_empty() {
        return Some((first.name.clone(), true));
    }
    let mut prefix: Vec<char> = first.name.chars().collect();
    for entry in rest {
        let chars: Vec<char> = entry.name.chars().collect();
        let common = prefix.iter().zip(&chars).take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase())).count();
        prefix.truncate(common);
    }
    let name: String = prefix.into_iter().collect();
    let lower = name.to_lowercase();
    (lower.chars().count() > filter.chars().count() && visible.iter().all(|entry| entry.name.to_lowercase().starts_with(&lower))).then_some((name, false))
}

/// `inferProjectTitleFromPath`: the last path segment.
pub fn infer_project_title(path: &str) -> String {
    let trimmed = path.trim();
    trimmed.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).find(|segment| !segment.is_empty()).unwrap_or(trimmed).to_owned()
}

/// `findProjectByPath`.
pub fn find_project_by_path<'a, T>(projects: &'a [T], path: &str, root: impl Fn(&T) -> &str) -> Option<&'a T> {
    let candidate = normalize_project_path(path);
    if candidate.is_empty() {
        return None;
    }
    projects.iter().find(|project| normalize_project_path(root(project)) == candidate)
}

/// The path a browse query adds: the typed folder without its trailing separator.
pub fn add_target(query: &str) -> String {
    let trimmed = query.trim();
    if trimmed.len() > 1 { trimmed.trim_end_matches(['/', '\\']).to_owned() } else { trimmed.to_owned() }
}

pub fn browse_request(partial_path: &str, cwd: Option<&str>) -> Value {
    let mut payload = json!({ "partialPath": partial_path });
    if let Some(cwd) = cwd {
        payload["cwd"] = json!(cwd);
    }
    payload
}

pub fn create_project(command_id: &str, project_id: &str, title: &str, workspace_root: &str) -> Value {
    json!({ "type": "project.create", "commandId": command_id, "projectId": project_id, "title": title, "workspaceRoot": workspace_root })
}

/// `project.update` with `pinned`; older servers without `projectOrganization` drop it.
pub fn set_project_pinned(command_id: &str, project_id: &str, pinned: bool) -> Value {
    json!({ "type": "project.update", "commandId": command_id, "projectId": project_id, "pinned": pinned })
}

pub fn create_new_project(name: &str) -> Value {
    json!({ "name": name.trim() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> BrowseEntry {
        BrowseEntry { name: name.into(), full_path: format!("/w/{name}") }
    }

    #[test]
    fn browse_queries_split_into_folder_and_leaf() {
        assert!(is_browse_query("~/code", false));
        assert!(is_browse_query("../x", false));
        assert!(!is_browse_query("code", false));
        assert!(!is_browse_query("C:\\code", false));
        assert!(is_browse_query("C:\\code", true));
        assert_eq!(browse_path("~/code/t3", false), Some(BrowsePath { directory: "~/code/".into(), filter: "t3".into() }));
        assert_eq!(browse_path("/Users/me/", false), Some(BrowsePath { directory: "/Users/me/".into(), filter: String::new() }));
        assert_eq!(append_browse_segment("~/code/t", "t3code"), "~/code/t3code/");
        assert_eq!(browse_parent_path("/Users/me/code/"), Some("/Users/me/".into()));
        assert_eq!(browse_parent_path("/Users/"), Some("/".into()));
        assert_eq!(browse_parent_path("/"), None);
        assert_eq!(browse_leaf("C:\\code\\t3"), "t3");
    }

    #[test]
    fn entries_filter_by_prefix_and_hide_dotfiles() {
        let entries = [entry("src"), entry(".git"), entry("Scripts"), entry("docs")];
        let names = |filter: &str| filter_browse_entries(&entries, filter).iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("s"), ["src", "Scripts"]);
        assert_eq!(names(""), ["src", "Scripts", "docs"]);
        assert_eq!(names("."), [".git"]);
    }

    #[test]
    fn tab_completion_enters_one_match_and_extends_several() {
        let entries = [entry("packages"), entry("package-lock"), entry("docs")];
        let visible = filter_browse_entries(&entries, "p");
        assert_eq!(browse_tab_completion(&visible, "p"), Some(("package".into(), false)));
        let one = filter_browse_entries(&entries, "d");
        assert_eq!(browse_tab_completion(&one, "d"), Some(("docs".into(), true)));
        let done = filter_browse_entries(&entries, "package");
        assert_eq!(browse_tab_completion(&done, "package"), None);
    }

    #[test]
    fn titles_targets_and_commands() {
        assert_eq!(infer_project_title("/Users/me/code/t3code/"), "t3code");
        assert_eq!(infer_project_title("~/x"), "x");
        assert_eq!(add_target("~/code/t3code/"), "~/code/t3code");
        assert_eq!(add_target("/"), "/");
        let roots = ["/w/a".to_owned(), "/w/b".to_owned()];
        assert_eq!(find_project_by_path(&roots, "/w/b/", |p| p.as_str()), Some(&roots[1]));
        assert_eq!(create_project("c", "p", "T", "/w")["type"], "project.create");
        assert_eq!(set_project_pinned("c", "p", true)["pinned"], true);
        assert_eq!(create_new_project("  Demo "), json!({"name": "Demo"}));
        assert_eq!(browse_request("~/", None), json!({"partialPath": "~/"}));
    }
}
