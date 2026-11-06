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
            _ => parse_iso(&project.updated_at),
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


#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRef {
    pub environment_id: String,
    pub project_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct EnvironmentOwner {
    pub saved_id: String,
    pub canonical_id: Option<String>,
    pub generation: u64,
    pub connection_generation: u64,
    pub shell_generation: u64,
}

impl EnvironmentOwner {
    pub fn identity(&self) -> String {
        self.canonical_id.as_ref().filter(|id| !id.is_empty()).cloned().unwrap_or_else(|| format!("\0saved:{}", self.saved_id))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopedProject {
    pub owner: EnvironmentOwner,
    pub project: ProjectShell,
    pub is_scratch: bool,
    pub environment_label: String,
}

impl ScopedProject {
    pub fn reference(&self) -> ProjectRef {
        ProjectRef { environment_id: self.owner.saved_id.clone(), project_id: self.project.id.clone() }
    }

    pub fn physical_key(&self) -> String {
        physical_project_key(&self.owner.identity(), &self.project.workspace_root)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopedProjectMember {
    pub physical_key: String,
    pub project: ScopedProject,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopedProjectGroup {
    pub key: String,
    pub label: String,
    pub representative: ProjectRef,
    pub members: Vec<ScopedProjectMember>,
    pub member_refs: Vec<ProjectRef>,
    // All registrations are needed to retain exact checkout context and deduplicate saved aliases.
    pub registrations: Vec<ScopedProject>,
    pub scope_aliases: Vec<String>,
    pub pinned: bool,
    pub archived: bool,
    pub is_scratch: bool,
}

impl ScopedProjectGroup {
    pub fn mutation_refs(&self) -> Vec<ProjectRef> {
        let mut seen = HashSet::new();
        self.registrations.iter().filter(|project| project.owner.saved_id == self.representative.environment_id)
            .chain(self.registrations.iter().filter(|project| project.owner.saved_id != self.representative.environment_id))
            .filter(|project| seen.insert((project.owner.identity(), project.project.id.clone())))
            .map(ScopedProject::reference).collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectPreferences {
    pub project_order: Vec<String>,
    pub project_expanded: HashMap<String, bool>,
    pub scope_keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupConsumer { Sidebar, Palette, DraftPicker, Settings }

pub struct GroupInput<'a> {
    pub projects: &'a [ScopedProject],
    pub threads: &'a [super::fleet::ScopedThread],
    pub settings: &'a GroupingSettings,
    pub preferences: &'a ProjectPreferences,
    pub preferred_environment: Option<&'a str>,
    pub consumer: GroupConsumer,
    pub sort: ProjectSortOrder,
}

pub const SCRATCH_SCOPE_KEY: &str = "\0no-project";

pub fn legacy_project_cwd_key(path: &str) -> String {
    format!("legacy-project-cwd:{}", normalize_project_path(path))
}

fn scoped_logical_key(project: &ScopedProject, settings: &GroupingSettings, scratch: bool) -> String {
    if scratch && project.is_scratch { return SCRATCH_SCOPE_KEY.into(); }
    let identity = project.owner.identity();
    logical_key(&identity, &project.project, resolve_mode(&identity, &project.project, settings))
}

pub fn build_scoped_project_groups(input: &GroupInput<'_>) -> Vec<ScopedProjectGroup> {
    let mut projects = input.projects.iter().collect::<Vec<_>>();
    if input.sort == ProjectSortOrder::Manual && input.consumer != GroupConsumer::Settings {
        let mut remaining = projects;
        projects = Vec::new();
        for key in &input.preferences.project_order {
            if let Some(index) = remaining.iter().position(|project| project.physical_key() == *key || legacy_project_cwd_key(&project.project.workspace_root) == *key) {
                projects.push(remaining.remove(index));
            }
        }
        projects.extend(remaining);
    }
    let scratch = input.consumer == GroupConsumer::Sidebar;
    let mut physical: Vec<(String, Vec<&ScopedProject>)> = Vec::new();
    for project in &projects {
        let key = project.physical_key();
        match physical.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, members)) => members.push(project),
            None => physical.push((key, vec![project])),
        }
    }
    let mut groups: Vec<ScopedProjectGroup> = Vec::new();
    for (physical_key, registrations) in physical {
        let winner = *registrations.iter().reduce(|existing, candidate| {
            if replaces_winner(&existing.project, &candidate.project)
                || (existing.project.id == candidate.project.id && freshness(&existing.project) == freshness(&candidate.project) && candidate.owner.saved_id < existing.owner.saved_id) { candidate } else { existing }
        }).unwrap();
        let source = if winner.project.repository_identity.is_some() { winner } else {
            registrations.iter().copied().filter(|project| project.project.repository_identity.is_some())
                .reduce(|a, b| if replaces_winner(&a.project, &b.project) { b } else { a }).unwrap_or(winner)
        };
        let mode = resolve_mode(&winner.owner.identity(), &winner.project, input.settings);
        let key = if scratch && source.is_scratch { SCRATCH_SCOPE_KEY.into() } else { logical_key(&source.owner.identity(), &source.project, mode) };
        let index = groups.iter().position(|group| group.key == key).unwrap_or_else(|| {
            groups.push(ScopedProjectGroup { key: key.clone(), label: String::new(), representative: winner.reference(), members: Vec::new(), member_refs: Vec::new(), registrations: Vec::new(), scope_aliases: Vec::new(), pinned: false, archived: true, is_scratch: scratch && source.is_scratch });
            groups.len() - 1
        });
        let group = &mut groups[index];
        group.members.push(ScopedProjectMember { physical_key, project: winner.clone() });
        group.pinned |= winner.project.pinned_at.is_some();
        group.archived &= winner.project.archived_at.is_some();
    }
    let group_of_physical = groups.iter().enumerate().flat_map(|(index, group)| group.members.iter().map(move |member| (member.physical_key.clone(), index))).collect::<HashMap<_, _>>();
    for project in projects {
        let group = &mut groups[group_of_physical[&project.physical_key()]];
        let reference = project.reference();
        if !group.member_refs.contains(&reference) {
            group.member_refs.push(reference);
            group.scope_aliases.push(project.physical_key());
            group.scope_aliases.push(scoped_logical_key(project, input.settings, false));
            group.registrations.push(project.clone());
        }
    }
    for group in &mut groups {
        let preferred = input.preferred_environment.and_then(|id| group.registrations.iter().find(|project| project.owner.saved_id == id))
            .map(|project| group.registrations.iter().filter(|candidate| candidate.owner.saved_id == project.owner.saved_id && candidate.physical_key() == project.physical_key())
                .reduce(|a, b| if replaces_winner(&a.project, &b.project) { b } else { a }).unwrap());
        let representative = preferred.unwrap_or(&group.members[0].project);
        group.representative = representative.reference();
        let display = group.members.iter().find(|member| member.physical_key == representative.physical_key()).unwrap();
        group.label = if group.is_scratch { "No project".into() } else if group.members.len() > 1 {
            group_label(&display.project.project, &group.members.iter().map(|member| &member.project.project).collect::<Vec<_>>())
        } else { display.project.project.title.clone() };
    }
    if input.consumer == GroupConsumer::Settings {
        groups.sort_by(|a, b| crate::thread::locale_compare(&a.label, &b.label));
    } else if input.sort != ProjectSortOrder::Manual {
        let order = if input.sort == ProjectSortOrder::CreatedAt { ThreadSortOrder::CreatedAt } else { ThreadSortOrder::UpdatedAt };
        let group_of = groups.iter().enumerate().flat_map(|(index, group)| group.member_refs.iter().map(move |reference| (reference.clone(), index))).collect::<HashMap<_, _>>();
        let mut latest: Vec<Option<Option<Ms>>> = vec![None; groups.len()];
        for thread in input.threads.iter().filter(|thread| !thread.thread.archived && !thread.thread.deleted && thread.thread.listed) {
            if let Some(&index) = group_of.get(&thread.project_ref()) {
                latest[index] = Some(latest[index].flatten().max(thread_sort_timestamp(&thread.thread, order)));
            }
        }
        let mut keyed = groups.into_iter().enumerate().map(|(index, group)| {
            let project = &group.registrations.iter().find(|project| project.reference() == group.representative).unwrap().project;
            let fallback = if input.sort == ProjectSortOrder::CreatedAt { parse_iso(&project.created_at) } else { parse_iso(&project.updated_at) };
            (latest[index].unwrap_or(fallback), project.title.clone(), group)
        }).collect::<Vec<_>>();
        keyed.sort_by(|(at, a_title, a), (bt, b_title, b)| bt.cmp(at).then_with(|| crate::thread::locale_compare(a_title, b_title)).then_with(|| crate::thread::locale_compare(&a.key, &b.key)));
        groups = keyed.into_iter().map(|(_, _, group)| group).collect();
    }
    if input.consumer == GroupConsumer::DraftPicker {
        groups.retain(|group| !group.archived);
    }
    if matches!(input.consumer, GroupConsumer::Sidebar | GroupConsumer::DraftPicker) {
        groups.sort_by_key(|group| !group.pinned);
    }
    groups
}

pub fn pick_project_target(group: &ScopedProjectGroup, preferred: Option<&ProjectRef>) -> Option<ScopedProjectMember> {
    let context = preferred.and_then(|reference| group.registrations.iter().find(|project| project.reference() == *reference))
        .or_else(|| preferred.and_then(|reference| group.registrations.iter().find(|project| project.owner.saved_id == reference.environment_id)))
        .or_else(|| group.registrations.iter().find(|project| project.reference() == group.representative));
    let Some(context) = context else { return group.members.first().cloned() };
    let physical_key = context.physical_key();
    let project = group.registrations.iter().filter(|project| project.owner.saved_id == context.owner.saved_id && project.physical_key() == physical_key)
        .reduce(|a, b| if replaces_winner(&a.project, &b.project) { b } else { a }).unwrap_or(context);
    Some(ScopedProjectMember { physical_key, project: project.clone() })
}

pub fn latest_group_thread(group: &ScopedProjectGroup, threads: &[super::fleet::ScopedThread], order: ThreadSortOrder) -> Option<super::tag_filter::ThreadKey> {
    let mut eligible = threads.iter().filter(|thread| !thread.thread.archived && !thread.thread.deleted && thread.thread.listed && group.member_refs.contains(&thread.project_ref())).collect::<Vec<_>>();
    eligible.sort_by(|a, b| thread_sort_timestamp(&b.thread, order).cmp(&thread_sort_timestamp(&a.thread, order)).then_with(|| b.thread.id.cmp(&a.thread.id)).then_with(|| a.owner.saved_id.cmp(&b.owner.saved_id)));
    eligible.first().map(|thread| thread.key())
}

impl ProjectPreferences {
    pub fn resolved_scope(&self, groups: &[ScopedProjectGroup], ready: bool) -> Vec<String> {
        let mut result = Vec::new();
        for key in &self.scope_keys {
            let group = groups.iter().find(|group| group.key == *key).or_else(|| groups.iter().find(|group| group.is_scratch && group.scope_aliases.contains(key)));
            let resolved = match group { Some(group) if !group.archived => Some(group.key.clone()), Some(_) => None, None if !ready => Some(key.clone()), None => None };
            if let Some(key) = resolved && !result.contains(&key) { result.push(key); }
        }
        result
    }

    pub fn scoped_members(&self, groups: &[ScopedProjectGroup], ready: bool) -> Option<HashSet<ProjectRef>> {
        let keys = self.resolved_scope(groups, ready);
        let members = groups.iter().filter(|group| keys.contains(&group.key)).flat_map(|group| group.member_refs.iter().cloned()).collect::<HashSet<_>>();
        (!members.is_empty()).then_some(members)
    }

    pub fn expanded(&self, group: &ScopedProjectGroup) -> bool {
        std::iter::once(group.key.clone()).chain(group.members.iter().map(|member| member.physical_key.clone()))
            .chain(group.members.iter().map(|member| legacy_project_cwd_key(&member.project.project.workspace_root)))
            .find_map(|key| self.project_expanded.get(&key).copied()).or_else(|| self.project_expanded.get("legacy-project-expansion-default").copied()).unwrap_or(true)
    }

    pub fn set_expanded(&mut self, group: &ScopedProjectGroup, expanded: bool) {
        self.project_expanded.insert(group.key.clone(), expanded);
        for member in &group.members {
            self.project_expanded.insert(member.physical_key.clone(), expanded);
            self.project_expanded.insert(legacy_project_cwd_key(&member.project.project.workspace_root), expanded);
        }
    }

    pub fn reorder(&mut self, current: &[String], dragged: &[String], target: &[String]) {
        if dragged.is_empty() || dragged.iter().all(|id| target.contains(id)) { return; }
        let Some(index) = current.iter().position(|id| target.contains(id)) else { return };
        let before = current[..index].iter().filter(|id| dragged.contains(id)).count();
        let removed = current.iter().filter(|id| dragged.contains(id)).cloned().collect::<Vec<_>>();
        if removed.is_empty() { return; }
        let mut order = current.iter().filter(|id| !dragged.contains(id)).cloned().collect::<Vec<_>>();
        let index = index.saturating_sub(before.saturating_sub(1)).min(order.len());
        order.splice(index..index, removed);
        self.project_order = order;
    }

    pub fn reconcile(&mut self, previous: &[ScopedProjectGroup], next: &[ScopedProjectGroup]) {
        for old in previous {
            for member in &old.registrations {
                let Some(new) = next.iter().flat_map(|group| &group.registrations).find(|project| project.reference() == member.reference() && normalize_project_path(&project.project.workspace_root) == normalize_project_path(&member.project.workspace_root)) else { continue };
                let old_key = member.physical_key();
                let new_key = new.physical_key();
                if old_key == new_key { continue; }
                for key in &mut self.project_order { if *key == old_key { *key = new_key.clone(); } }
                for key in &mut self.scope_keys { if *key == old_key { *key = new_key.clone(); } }
                if let Some(value) = self.project_expanded.get(&old_key).copied() { self.project_expanded.entry(new_key).or_insert(value); }
            }
        }
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
    fn project_sort_does_not_replace_an_invalid_updated_timestamp_with_creation() {
        let mut invalid = project("new", "/new");
        invalid.repository_identity = None;
        invalid.created_at = "2026-01-03T00:00:00Z".into();
        invalid.updated_at = "invalid".into();
        let mut valid = project("old", "/old");
        valid.repository_identity = None;
        let projects = [invalid, valid];
        let mut groups = build_project_groups("env", &projects, &GroupingSettings::default());
        sort_project_groups(&mut groups, &projects, &[], ProjectSortOrder::UpdatedAt);
        assert_eq!(groups.iter().map(|group| group.representative_id.as_str()).collect::<Vec<_>>(), ["old", "new"]);
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
