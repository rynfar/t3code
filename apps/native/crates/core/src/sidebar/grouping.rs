//! Project grouping: `client-runtime/src/state/projectGrouping.ts`, the path rules from
//! `packages/shared/src/path.ts`, and the project sort from `apps/web/src/components/Sidebar.logic.ts`
//! (`sortLogicalProjectsForSidebar`).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::sort::{ThreadSortOrder, thread_sort_timestamp};
use super::thread::SidebarThread;
use super::time::{Ms, parse_iso};
use crate::shell::ProjectShell;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupingMode {
    #[default]
    Repository,
    RepositoryPath,
    Separate,
}

impl GroupingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repository => "repository",
            Self::RepositoryPath => "repository_path",
            Self::Separate => "separate",
        }
    }

    pub fn parse(value: &str) -> Option<GroupingMode> {
        match value {
            "repository" => Some(GroupingMode::Repository),
            "repository_path" => Some(GroupingMode::RepositoryPath),
            "separate" => Some(GroupingMode::Separate),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupingSettings {
    pub mode: GroupingMode,
    /// The connected server's identity; thread navigation still uses the local scope.
    pub environment_id: Option<String>,
    /// Keyed by physical project key (`<environmentId>:<normalized path>`).
    pub overrides: HashMap<String, GroupingMode>,
}

fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes.len() == 2 || matches!(bytes[2], b'/' | b'\\'))
}

fn is_root_path(value: &str) -> bool {
    value == "/" || value == "\\" || (value.len() == 3 && is_windows_drive_path(value))
}

fn trim_trailing_separators(value: &str) -> String {
    if value.is_empty() || is_root_path(value) {
        return value.to_owned();
    }
    let trimmed = if value.starts_with('/') { value.trim_end_matches('/') } else { value.trim_end_matches(['/', '\\']) };
    if trimmed.is_empty() {
        return value.to_owned();
    }
    if trimmed.len() == 2 && is_windows_drive_path(trimmed) { format!("{trimmed}\\") } else { trimmed.to_owned() }
}

/// `normalizeProjectPathForComparison`.
pub fn normalize_project_path(value: &str) -> String {
    let normalized = trim_trailing_separators(value.trim());
    if is_windows_drive_path(&normalized) || normalized.starts_with("\\\\") {
        normalized.replace('/', "\\").to_lowercase()
    } else {
        normalized
    }
}

pub fn physical_project_key(environment_id: &str, workspace_root: &str) -> String {
    format!("{environment_id}:{}", normalize_project_path(workspace_root))
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().map(str::trim).filter(|text| !text.is_empty())
}

fn grouping_key_of(identity: &Value) -> Option<&str> {
    identity.get("origin").and_then(|origin| text(origin, "canonicalKey")).or_else(|| text(identity, "canonicalKey"))
}

fn grouping_display_name_of(identity: &Value) -> Option<&str> {
    match identity.get("origin").filter(|origin| origin.is_object()) {
        Some(origin) => text(origin, "displayName").or_else(|| text(origin, "canonicalKey")),
        None => text(identity, "displayName"),
    }
}

fn identity(project: &ProjectShell) -> Option<&Value> {
    project.repository_identity.as_ref().filter(|value| value.is_object())
}

fn repository_relative_path(project: &ProjectShell) -> Option<String> {
    let root = text(identity(project)?, "rootPath")?;
    let (project_path, root_path) = (normalize_project_path(&project.workspace_root), normalize_project_path(root));
    if project_path.is_empty() || root_path.is_empty() {
        return None;
    }
    if project_path == root_path {
        return Some(String::new());
    }
    let separator = if root_path.contains('\\') { '\\' } else { '/' };
    project_path.strip_prefix(&format!("{root_path}{separator}")).map(|rest| rest.replace('\\', "/"))
}

fn repository_scoped_key(project: &ProjectShell, mode: GroupingMode) -> Option<String> {
    let canonical = grouping_key_of(identity(project)?).filter(|key| !key.contains('\0'))?;
    if mode == GroupingMode::Repository {
        return Some(canonical.to_owned());
    }
    Some(match repository_relative_path(project) {
        Some(relative) if !relative.is_empty() => format!("{canonical}::{relative}"),
        _ => canonical.to_owned(),
    })
}

fn resolve_mode(environment_id: &str, project: &ProjectShell, settings: &GroupingSettings) -> GroupingMode {
    settings.overrides.get(&physical_project_key(environment_id, &project.workspace_root)).copied().unwrap_or(settings.mode)
}

fn logical_key(environment_id: &str, project: &ProjectShell, mode: GroupingMode) -> String {
    if mode == GroupingMode::Separate {
        return physical_project_key(environment_id, &project.workspace_root);
    }
    repository_scoped_key(project, mode).unwrap_or_else(|| physical_project_key(environment_id, &project.workspace_root))
}

fn unique_non_empty<'a>(values: impl Iterator<Item = Option<&'a str>>) -> Vec<&'a str> {
    let mut unique = Vec::new();
    for value in values.flatten().map(str::trim).filter(|value| !value.is_empty()) {
        if !unique.contains(&value) {
            unique.push(value);
        }
    }
    unique
}

/// `deriveProjectGroupLabel`.
pub fn group_label(representative: &ProjectShell, members: &[&ProjectShell]) -> String {
    let titles = unique_non_empty(members.iter().map(|member| Some(member.title.as_str())));
    let display_names = unique_non_empty(members.iter().map(|member| identity(member).and_then(grouping_display_name_of)));
    let repository_names = unique_non_empty(members.iter().map(|member| identity(member).and_then(|identity| text(identity, "name"))));
    if let [title] = titles.as_slice()
        && !display_names.contains(title)
        && !repository_names.contains(title)
    {
        return (*title).to_owned();
    }
    if let [name] = display_names.as_slice() {
        return (*name).to_owned();
    }
    if let [name] = repository_names.as_slice() {
        return (*name).to_owned();
    }
    representative.title.clone()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGroup {
    pub key: String,
    pub label: String,
    pub representative_id: String,
    pub member_project_ids: Vec<String>,
    pub pinned: bool,
    pub archived: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectScope {
    keys: HashSet<String>,
}

impl ProjectScope {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn toggle(&mut self, group: &ProjectGroup, groups: &[ProjectGroup], ready: bool) {
        self.keys = self.resolved_keys(groups, ready);
        if !self.keys.remove(&group.key) {
            self.keys.insert(group.key.clone());
        }
    }

    pub fn is_selected(&self, group: &ProjectGroup) -> bool {
        !group.archived && self.keys.contains(&group.key)
    }

    /// `resolveSidebarProjectScopeKeys`: missing stored keys survive loading and can return later.
    pub fn resolved_keys(&self, groups: &[ProjectGroup], ready: bool) -> HashSet<String> {
        self.keys.iter().filter(|key| groups.iter().find(|group| group.key == **key).map_or(!ready, |group| !group.archived)).cloned().collect()
    }

    pub fn members(&self, groups: &[ProjectGroup], ready: bool) -> Option<HashSet<String>> {
        let keys = self.resolved_keys(groups, ready);
        let members: HashSet<String> =
            groups.iter().filter(|group| keys.contains(&group.key)).flat_map(|group| group.member_project_ids.iter().cloned()).collect();
        (!members.is_empty()).then_some(members)
    }
}

fn freshness(project: &ProjectShell) -> Ms {
    parse_iso(&project.updated_at).or_else(|| parse_iso(&project.created_at)).unwrap_or(0)
}

fn replaces_winner(existing: &ProjectShell, candidate: &ProjectShell) -> bool {
    let delta = freshness(candidate) - freshness(existing);
    delta > 0 || (delta == 0 && candidate.id > existing.id)
}

/// `buildProjectGroups` for one environment.
pub fn build_project_groups(environment_id: &str, projects: &[ProjectShell], settings: &GroupingSettings) -> Vec<ProjectGroup> {
    let environment_id = settings.environment_id.as_deref().unwrap_or(environment_id);
    let mut by_physical: Vec<(String, Vec<&ProjectShell>)> = Vec::new();
    for project in projects {
        let key = physical_project_key(environment_id, &project.workspace_root);
        match by_physical.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, members)) => members.push(project),
            None => by_physical.push((key, vec![project])),
        }
    }
    let mut groups: Vec<(String, Vec<&ProjectShell>)> = Vec::new();
    let mut logical_by_physical = HashMap::new();
    for (physical, members) in &by_physical {
        let Some(mut winner) = members.first().copied() else { continue };
        for candidate in members.iter().skip(1) {
            if replaces_winner(winner, candidate) {
                winner = candidate;
            }
        }
        let identity_source = if winner.repository_identity.is_some() {
            winner
        } else {
            members.iter().copied().filter(|member| member.repository_identity.is_some()).reduce(|a, b| if replaces_winner(a, b) { b } else { a }).unwrap_or(winner)
        };
        let key = logical_key(environment_id, identity_source, resolve_mode(environment_id, winner, settings));
        logical_by_physical.insert(physical.clone(), key.clone());
        match groups.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, group)) => group.push(winner),
            None => groups.push((key, vec![winner])),
        }
    }
    let mut refs: HashMap<String, Vec<String>> = HashMap::new();
    let mut seen = HashSet::new();
    for project in projects {
        if !seen.insert(project.id.as_str()) {
            continue;
        }
        let physical = physical_project_key(environment_id, &project.workspace_root);
        let key = logical_by_physical.get(&physical).cloned().unwrap_or_else(|| logical_key(environment_id, project, resolve_mode(environment_id, project, settings)));
        refs.entry(key).or_default().push(project.id.clone());
    }
    groups
        .into_iter()
        .filter_map(|(key, members)| {
            let representative = *members.first()?;
            Some(ProjectGroup {
                label: if members.len() > 1 { group_label(representative, &members) } else { representative.title.clone() },
                representative_id: representative.id.clone(),
                member_project_ids: refs.remove(&key).unwrap_or_default(),
                pinned: members.iter().any(|member| member.pinned_at.is_some()),
                archived: members.iter().all(|member| member.archived_at.is_some()),
                key,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectSortOrder {
    #[default]
    UpdatedAt,
    CreatedAt,
    Manual,
}

/// `sortLogicalProjectsForSidebar`: by newest thread activity, ties by label then key.
pub fn sort_project_groups(groups: &mut [ProjectGroup], projects: &[ProjectShell], threads: &[SidebarThread], order: ProjectSortOrder) {
    let thread_order = match order {
        ProjectSortOrder::Manual => return,
        ProjectSortOrder::CreatedAt => ThreadSortOrder::CreatedAt,
        ProjectSortOrder::UpdatedAt => ThreadSortOrder::UpdatedAt,
    };
    let group_of: HashMap<&str, usize> =
        groups.iter().enumerate().flat_map(|(index, group)| group.member_project_ids.iter().map(move |id| (id.as_str(), index))).collect();
    let mut latest: Vec<Option<Option<Ms>>> = vec![None; groups.len()];
    for thread in threads.iter().filter(|thread| !thread.archived) {
        if let Some(&index) = group_of.get(thread.project_id.as_str()) {
            let timestamp = thread_sort_timestamp(thread, thread_order);
            latest[index] = Some(latest[index].flatten().max(timestamp));
        }
    }
    let project_time = |group: &ProjectGroup| {
        let project = projects.iter().find(|project| project.id == group.representative_id)?;
        match order {
            ProjectSortOrder::CreatedAt => parse_iso(&project.created_at),
            _ => parse_iso(&project.updated_at).or_else(|| parse_iso(&project.created_at)),
        }
    };
    let mut keyed: Vec<(Option<Ms>, ProjectGroup)> =
        groups.iter().enumerate().map(|(index, group)| (latest[index].unwrap_or_else(|| project_time(group)), group.clone())).collect();
    keyed.sort_by(|(left_time, left), (right_time, right)| {
        right_time.cmp(left_time).then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase())).then_with(|| left.key.cmp(&right.key))
    });
    for (slot, (_, group)) in groups.iter_mut().zip(keyed) {
        *slot = group;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_settings::ClientSettingsDoc;
    use serde_json::json;

    fn project(id: &str, path: &str) -> ProjectShell {
        serde_json::from_value(json!({
            "id": id, "title": id, "workspaceRoot": path,
            "repositoryIdentity": {"canonicalKey": "github.com/t3/code", "rootPath": "/repo"},
            "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z"
        }))
        .unwrap()
    }

    #[test]
    fn persisted_physical_override_keeps_siblings_and_duplicate_refs() {
        let projects = [project("first", "/repo"), project("second", "/copy/"), project("third", "/third"), project("stale", "/copy")];
        let mut doc = ClientSettingsDoc::from_json(r#"{"sidebarProjectGroupingOverrides":{" env:/copy ":"separate"}}"#);
        let groups = build_project_groups("env", &projects, &doc.project_grouping_settings());
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].member_project_ids, ["first", "third"]);
        assert_eq!(groups[1].key, "env:/copy");
        assert_eq!(groups[1].member_project_ids, ["second", "stale"]);
        assert_eq!(groups[1].representative_id, "stale");
        assert_eq!(build_project_groups("other-env", &projects, &doc.project_grouping_settings()).len(), 1);
        doc.apply(&crate::client_settings::ClientSettingChange::ProjectGroupingOverride { key: "env:/copy".into(), mode: None });
        assert_eq!(build_project_groups("env", &projects, &doc.project_grouping_settings()).len(), 1);
    }

    #[test]
    fn overrides_take_precedence_over_each_global_mode() {
        let projects = [project("web", "/repo/apps/web"), project("root", "/repo"), project("copy", "/copy")];
        for mode in ["repository", "repository_path", "separate"] {
            let doc = ClientSettingsDoc::from_json(
                &json!({
                    "sidebarProjectGroupingMode": mode,
                    "sidebarProjectGroupingOverrides": {"env:/repo/apps/web": "repository_path", "env:/repo": "repository", "env:/copy": "repository"}
                })
                .to_string(),
            );
            let groups = build_project_groups("env", &projects, &doc.project_grouping_settings());
            assert_eq!(groups.iter().map(|g| g.key.as_str()).collect::<Vec<_>>(), ["github.com/t3/code::apps/web", "github.com/t3/code"]);
            assert_eq!(groups[1].member_project_ids, ["root", "copy"]);
        }
    }
    #[test]
    fn configured_environment_scopes_overrides_even_with_local_thread_keys() {
        let projects = [project("first", "/repo"), project("second", "/copy")];
        let doc = ClientSettingsDoc::from_json(r#"{"sidebarProjectGroupingOverrides":{"server:/copy":"separate","other:/repo":"separate"}}"#);
        let settings = GroupingSettings { environment_id: Some("server".into()), ..doc.project_grouping_settings() };
        let groups = build_project_groups("local", &projects, &settings);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1].key, "server:/copy");
    }

    #[test]
    fn physical_duplicates_use_freshest_identity_but_keep_every_registration() {
        let mut stale = project("stale", "/repo/");
        stale.repository_identity = Some(json!({"canonicalKey":"old-repo"}));
        let mut fresh = project("fresh", "/repo");
        fresh.updated_at = "2026-01-02T00:00:00.000Z".into();
        let mut winner = project("winner", "/repo");
        winner.updated_at = "2026-01-03T00:00:00.000Z".into();
        winner.repository_identity = None;
        winner.pinned_at = Some("2026-01-03T00:00:00.000Z".into());
        let sibling = project("sibling", "/copy");
        let projects = [stale, fresh, winner, sibling];
        let groups = build_project_groups("env", &projects, &GroupingSettings::default());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].key, "github.com/t3/code");
        assert_eq!(groups[0].representative_id, "winner");
        assert_eq!(groups[0].member_project_ids, ["stale", "fresh", "winner", "sibling"]);
        assert!(groups[0].pinned);
        let doc = ClientSettingsDoc::from_json(r#"{"sidebarProjectGroupingOverrides":{"env:/repo":"separate"}}"#);
        let groups = build_project_groups("env", &projects, &doc.project_grouping_settings());
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].key, "env:/repo");
        assert_eq!(groups[0].member_project_ids, ["stale", "fresh", "winner"]);
    }

    #[test]
    fn override_uses_normalized_windows_path_and_origin_identity() {
        let mut first = project("first", "C:/Work/Code/");
        let mut copy = project("copy", "D:/Code");
        let identity = json!({"canonicalKey":"legacy-repo", "origin":{"canonicalKey":"fork", "displayName":"My fork"}});
        first.repository_identity = Some(identity.clone());
        copy.repository_identity = Some(identity);
        let doc = ClientSettingsDoc::from_json(
            r#"{"sidebarProjectGroupingMode":"separate","sidebarProjectGroupingOverrides":{"env:c:\\work\\code":"repository","env:d:\\code":"repository"}}"#,
        );
        let groups = build_project_groups("env", &[first, copy], &doc.project_grouping_settings());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].key, "fork");
        assert_eq!(groups[0].label, "My fork");
    }
    #[test]
    fn project_scope_follows_logical_groups_when_a_checkout_splits_and_rejoins() {
        let projects = [project("a", "/repo"), project("b", "/copy")];
        let grouped = build_project_groups("env", &projects, &GroupingSettings::default());
        let separate = GroupingSettings { overrides: [("env:/copy".into(), GroupingMode::Separate)].into(), ..Default::default() };
        let split = build_project_groups("env", &projects, &separate);
        let mut scope = ProjectScope::default();
        scope.toggle(&grouped[0], &grouped, true);
        assert_eq!(scope.members(&split, true), Some(["a".into()].into()));
        assert_eq!(scope.members(&grouped, true), Some(["a".into(), "b".into()].into()));
        let all_separate = build_project_groups("env", &projects, &GroupingSettings { mode: GroupingMode::Separate, ..Default::default() });
        assert_eq!(scope.members(&all_separate, true), None);
    }

    #[test]
    fn project_scope_clears_a_missing_physical_group_on_inherit() {
        let projects = [project("a", "/repo"), project("b", "/copy")];
        let grouped = build_project_groups("env", &projects, &GroupingSettings::default());
        let split = build_project_groups("env", &projects, &GroupingSettings { mode: GroupingMode::Separate, ..Default::default() });
        let mut scope = ProjectScope::default();
        scope.toggle(&split[1], &split, true);
        assert_eq!(scope.members(&grouped, true), None);
        assert_eq!(scope.resolved_keys(&grouped, false), ["env:/copy".into()].into());
        assert!(scope.resolved_keys(&grouped, true).is_empty());
        assert_eq!(scope.members(&split, true), Some(["b".into()].into()));
        let mut archived = split.clone();
        archived[1].archived = true;
        assert_eq!(scope.members(&archived, true), None);
    }
}
