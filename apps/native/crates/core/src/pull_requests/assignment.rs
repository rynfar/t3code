//! Multi-environment project assignment and host availability tracking for the Pull Requests page.
//! Ports `apps/web/src/components/pullRequest/pullRequestProjectAssignment.logic.ts`.

use std::collections::HashMap;

use super::wire::ProviderSummary;

/// `PULL_REQUEST_LIST_PROJECT_LIMIT` from `packages/contracts/src/pullRequest.ts`.
pub const LIST_PROJECT_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignableProject {
    pub id: String,
    pub environment_id: String,
    pub title: String,
    pub workspace_root: String,
    pub repository_key: Option<String>,
    pub host: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStatus {
    pub configured: bool,
    pub observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectHostBinding {
    pub host: String,
    pub observed_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostRead {
    pub observed_at: i64,
    pub reset_at: Option<i64>,
    pub hosts: HashMap<String, HostStatus>,
    pub project_hosts: HashMap<String, ProjectHostBinding>,
}

pub type HostAvailability = HashMap<String, HostRead>;

pub fn reset_hosts(previous: &HostAvailability) -> HostAvailability {
    previous
        .iter()
        .map(|(id, read)| {
            (
                id.clone(),
                HostRead {
                    observed_at: read.observed_at,
                    reset_at: Some(read.observed_at),
                    hosts: HashMap::new(),
                    project_hosts: HashMap::new(),
                },
            )
        })
        .collect()
}

pub fn remember_hosts(previous: &HostAvailability, observations: &[(&str, &[ProviderSummary], i64)]) -> HostAvailability {
    let mut active_hosts: HashMap<String, HashMap<String, HostStatus>> = HashMap::new();
    for &(environment_id, providers, observed_at) in observations {
        if previous.get(environment_id).and_then(|r| r.reset_at).is_some_and(|reset| observed_at <= reset) {
            continue;
        }
        let hosts = active_hosts.entry(environment_id.to_owned()).or_default();
        for provider in providers {
            let host = provider.host.to_lowercase();
            let held = hosts.get(&host);
            hosts.insert(
                host,
                HostStatus {
                    configured: provider.configured || held.is_some_and(|h| h.configured),
                    observed_at: held.map_or(observed_at, |h| observed_at.max(h.observed_at)),
                },
            );
        }
    }

    let mut next = previous.clone();
    for &(environment_id, providers, observed_at) in observations {
        let before = next.get(environment_id).cloned();
        if before.as_ref().and_then(|r| r.reset_at).is_some_and(|reset| observed_at <= reset) {
            continue;
        }
        let mut hosts = before.as_ref().map(|b| b.hosts.clone()).unwrap_or_default();
        let mut project_hosts = before.as_ref().map(|b| b.project_hosts.clone()).unwrap_or_default();
        let mut changed = false;
        let mut latest_observed = observed_at;
        for provider in providers {
            let host = provider.host.to_lowercase();
            if let Some(active) = active_hosts.get(environment_id).and_then(|map| map.get(&host)) {
                let held = hosts.get(&host);
                let newer = held.is_none_or(|h| active.observed_at > h.observed_at || (active.observed_at == h.observed_at && active.configured && !h.configured));
                if newer {
                    hosts.insert(host.clone(), active.clone());
                    latest_observed = latest_observed.max(active.observed_at);
                    changed = true;
                }
            }
            if let Some(project_ids) = &provider.project_ids {
                for project_id in project_ids {
                    if project_hosts.get(project_id).is_none_or(|h| observed_at > h.observed_at) {
                        project_hosts.insert(project_id.clone(), ProjectHostBinding { host: host.clone(), observed_at });
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            continue;
        }
        let prev_observed = before.as_ref().map_or(i64::MIN, |b| b.observed_at);
        next.insert(
            environment_id.to_owned(),
            HostRead {
                observed_at: prev_observed.max(latest_observed),
                reset_at: before.and_then(|b| b.reset_at),
                hosts,
                project_hosts,
            },
        );
    }
    next
}

fn repository_key(project: &AssignableProject) -> Option<String> {
    project.repository_key.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_lowercase)
}

/// Assigns each repository canonical key to a single owning environment so the same repository
/// cloned on multiple machines is queried once, preferring readable hosts and `preferred_environment_id`.
pub fn assign_projects_to_environments(
    projects: &[AssignableProject],
    environment_ids: &[String],
    preferred_environment_id: Option<&str>,
    host_availability: Option<&HostAvailability>,
) -> HashMap<String, Vec<String>> {
    let rank: HashMap<&str, usize> = environment_ids.iter().enumerate().map(|(ix, id)| (id.as_str(), ix)).collect();
    let mut owner: HashMap<String, &AssignableProject> = HashMap::new();
    let unreadable = |project: &AssignableProject| -> bool {
        let Some(availability) = host_availability.and_then(|map| map.get(&project.environment_id)) else {
            return false;
        };
        let host = availability
            .project_hosts
            .get(&project.id)
            .map(|binding| binding.host.as_str())
            .or(project.host.as_deref());
        host.and_then(|h| availability.hosts.get(&h.to_lowercase())).is_some_and(|status| !status.configured)
    };
    for project in projects {
        let Some(key) = repository_key(project) else { continue };
        let Some(&env_rank) = rank.get(project.environment_id.as_str()) else { continue };
        let Some(&current) = owner.get(&key) else {
            owner.insert(key, project);
            continue;
        };
        let current_unreadable = unreadable(current);
        let candidate_unreadable = unreadable(project);
        if current_unreadable != candidate_unreadable {
            if current_unreadable {
                owner.insert(key, project);
            }
            continue;
        }
        if preferred_environment_id.is_some_and(|pref| current.environment_id == pref) {
            continue;
        }
        let current_rank = rank.get(current.environment_id.as_str()).copied().unwrap_or(usize::MAX);
        if preferred_environment_id.is_some_and(|pref| project.environment_id == pref) || env_rank < current_rank {
            owner.insert(key, project);
        }
    }

    let mut assignment: HashMap<String, Vec<String>> = HashMap::new();
    for project in projects {
        if !rank.contains_key(project.environment_id.as_str()) {
            continue;
        }
        if let Some(key) = repository_key(project)
            && owner.get(&key).is_some_and(|picked| picked.environment_id != project.environment_id)
        {
            continue;
        }
        assignment.entry(project.environment_id.clone()).or_default().push(project.id.clone());
    }
    assignment
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectQuerySlice {
    pub environment_id: String,
    pub project_ids: Option<Vec<String>>,
}

/// Splits assigned projects per environment into `LIST_PROJECT_LIMIT` slices, omitting `project_ids`
/// when an environment owns every one of its projects.
pub fn pull_request_project_queries(
    projects: &[AssignableProject],
    environment_ids: &[String],
    host_availability: Option<&HostAvailability>,
) -> Vec<ProjectQuerySlice> {
    let preferred = environment_ids.first().map(String::as_str);
    let assignment = assign_projects_to_environments(projects, environment_ids, preferred, host_availability);
    let mut totals: HashMap<&str, usize> = HashMap::new();
    for project in projects {
        *totals.entry(project.environment_id.as_str()).or_default() += 1;
    }
    let mut slices = Vec::new();
    for environment_id in environment_ids {
        let Some(project_ids) = assignment.get(environment_id) else { continue };
        if Some(project_ids.len()) == totals.get(environment_id.as_str()).copied() {
            slices.push(ProjectQuerySlice { environment_id: environment_id.clone(), project_ids: None });
            continue;
        }
        for chunk in project_ids.chunks(LIST_PROJECT_LIMIT) {
            slices.push(ProjectQuerySlice { environment_id: environment_id.clone(), project_ids: Some(chunk.to_vec()) });
        }
    }
    slices
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentInfo {
    pub environment_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickableEnvironment {
    pub environment_id: String,
    pub project_id: String,
    pub workspace_root: String,
    pub label: String,
}

/// `resolvePickableEnvironments`: returns the environments that hold a checkout of the same
/// repository as `(current_env, current_project)`, or empty when only one environment has it.
pub fn resolve_pickable_environments(
    current_env: &str,
    current_project: &str,
    projects: &[AssignableProject],
    environments: &[EnvironmentInfo],
) -> Vec<PickableEnvironment> {
    let Some(own) = projects.iter().find(|p| p.environment_id == current_env && p.id == current_project) else {
        return Vec::new();
    };
    let Some(key) = repository_key(own) else { return Vec::new() };
    let Some(own_env) = environments.iter().find(|env| env.environment_id == current_env) else {
        return Vec::new();
    };
    let mut others = Vec::new();
    for env in environments {
        if env.environment_id == current_env {
            continue;
        }
        if let Some(copy) = projects.iter().find(|p| p.environment_id == env.environment_id && repository_key(p).as_deref() == Some(key.as_str())) {
            others.push(PickableEnvironment {
                environment_id: env.environment_id.clone(),
                project_id: copy.id.clone(),
                workspace_root: copy.workspace_root.clone(),
                label: env.label.clone(),
            });
        }
    }
    if others.is_empty() {
        return Vec::new();
    }
    let mut all = vec![PickableEnvironment {
        environment_id: current_env.to_owned(),
        project_id: own.id.clone(),
        workspace_root: own.workspace_root.clone(),
        label: own_env.label.clone(),
    }];
    all.extend(others);
    all
}

/// Stacked project-assignment column summary for the PR page: each project with its assigned
/// environment label and count of open/shown pull requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedProjectColumnRow {
    pub project_id: String,
    pub project_title: String,
    pub environment_id: String,
    pub environment_label: String,
    pub pr_count: usize,
    pub unavailable: bool,
}

pub fn project_assignment_rows(
    projects: &[AssignableProject],
    environment_ids: &[String],
    environments: &[EnvironmentInfo],
    host_availability: Option<&HostAvailability>,
    pr_counts: &HashMap<String, usize>,
    unavailable_projects: &std::collections::HashSet<String>,
) -> Vec<AssignedProjectColumnRow> {
    let preferred = environment_ids.first().map(String::as_str);
    let assignment = assign_projects_to_environments(projects, environment_ids, preferred, host_availability);
    let env_labels: HashMap<&str, &str> = environments.iter().map(|e| (e.environment_id.as_str(), e.label.as_str())).collect();
    let mut rows = Vec::new();
    for env_id in environment_ids {
        let Some(ids) = assignment.get(env_id) else { continue };
        for id in ids {
            if let Some(project) = projects.iter().find(|p| p.environment_id == *env_id && p.id == *id) {
                rows.push(AssignedProjectColumnRow {
                    project_id: project.id.clone(),
                    project_title: project.title.clone(),
                    environment_id: env_id.clone(),
                    environment_label: env_labels.get(env_id.as_str()).copied().unwrap_or(env_id.as_str()).to_owned(),
                    pr_count: pr_counts.get(&project.id).copied().unwrap_or(0),
                    unavailable: unavailable_projects.contains(&project.id),
                });
            }
        }
    }
    rows.sort_by(|left, right| {
        left.unavailable
            .cmp(&right.unavailable)
            .then(right.pr_count.cmp(&left.pr_count))
            .then_with(|| left.project_title.to_lowercase().cmp(&right.project_title.to_lowercase()))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(id: &str, env: &str, repo: Option<&str>, host: Option<&str>) -> AssignableProject {
        AssignableProject {
            id: id.into(),
            environment_id: env.into(),
            title: id.into(),
            workspace_root: format!("/work/{id}"),
            repository_key: repo.map(str::to_owned),
            host: host.map(str::to_owned),
        }
    }

    #[test]
    fn assigns_duplicate_repositories_to_readable_or_preferred_environment() {
        let projects = vec![
            project("p1", "env-1", Some("github.com/acme/app"), Some("github.com")),
            project("p2", "env-2", Some("github.com/acme/app"), Some("github.com")),
            project("p3", "env-2", Some("github.com/acme/other"), Some("github.com")),
        ];
        let envs = vec!["env-1".into(), "env-2".into()];
        let assigned = assign_projects_to_environments(&projects, &envs, Some("env-1"), None);
        assert_eq!(assigned.get("env-1").map(Vec::as_slice), Some(&["p1".to_owned()][..]));
        assert_eq!(assigned.get("env-2").map(Vec::as_slice), Some(&["p3".to_owned()][..]));

        let summary = ProviderSummary {
            host: "github.com".into(),
            kind: "github".into(),
            searches_on_host: true,
            actions: None,
            project_count: 1,
            project_ids: Some(vec!["p1".into()]),
            configured: false,
            detail: None,
        };
        let availability = remember_hosts(&HashMap::new(), &[("env-1", &[summary], 10)]);
        let fallback = assign_projects_to_environments(&projects, &envs, Some("env-1"), Some(&availability));
        assert!(!fallback.contains_key("env-1"), "unconfigured env yields repo to configured env");
        assert_eq!(fallback.get("env-2").map(Vec::as_slice), Some(&["p2".to_owned(), "p3".to_owned()][..]));

        let reset = reset_hosts(&availability);
        let stale = remember_hosts(&reset, &[("env-1", &[], 10)]);
        assert!(stale.get("env-1").unwrap().hosts.is_empty());
    }

    #[test]
    fn project_queries_omit_ids_when_environment_owns_all_its_projects() {
        let projects = vec![
            project("p1", "env-1", Some("github.com/acme/a"), Some("github.com")),
            project("p2", "env-2", Some("github.com/acme/a"), Some("github.com")),
            project("p3", "env-2", Some("github.com/acme/b"), Some("github.com")),
        ];
        let slices = pull_request_project_queries(&projects, &["env-1".into(), "env-2".into()], None);
        assert_eq!(
            slices,
            vec![
                ProjectQuerySlice { environment_id: "env-1".into(), project_ids: None },
                ProjectQuerySlice { environment_id: "env-2".into(), project_ids: Some(vec!["p3".into()]) },
            ]
        );
    }

    #[test]
    fn resolves_pickable_environments_for_shared_repositories() {
        let projects = vec![
            project("p1", "env-1", Some("github.com/acme/a"), Some("github.com")),
            project("p2", "env-2", Some("github.com/acme/a"), Some("github.com")),
        ];
        let envs = vec![
            EnvironmentInfo { environment_id: "env-1".into(), label: "Local".into() },
            EnvironmentInfo { environment_id: "env-2".into(), label: "Remote".into() },
        ];
        let pickable = resolve_pickable_environments("env-1", "p1", &projects, &envs);
        assert_eq!(pickable.len(), 2);
        assert_eq!(pickable[0].label, "Local");
        assert_eq!(pickable[1].project_id, "p2");
    }
}
