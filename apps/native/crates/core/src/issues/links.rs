//! Inline issue URL parsing, project/tracker target resolution, and bidirectional thread↔issue
//! linking helpers (`packages/client-runtime/src/issueLinks.ts`,
//! `packages/issue-tracker-linear/src/shared/issueUrl.ts`, `packages/source-control-*/definition.ts`,
//! `packages/contracts/src/orchestrationV2.ts`, `pingdotgg/t3code#19270`, `#38263`, `#38269`,
//! `#38771`, `#38854`).

use std::collections::HashSet;

use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;
use url::Url;

use super::list::repository_issue_bindings;
use super::wire::{
    IssueLinkedThread, IssueRef, IssueTrackerConnectionList, IssueTrackerKind, ThreadIssueKey,
    ThreadIssueLinkSource, thread_issue_keys_equal,
};
use crate::shell::ThreadShell;

pub const LINEAR_HOST: &str = "linear.app";
pub const LINEAR_KIND: &str = "linear";

/// Minimal project descriptor needed to resolve an issue URL to a project's credentials.
#[derive(Debug, Clone, PartialEq)]
pub struct IssueLinkProject {
    pub id: String,
    pub environment_id: String,
    pub repository_identity: Option<Value>,
}

/// Parsed structure of an issue URL before matching against workspace projects or bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedIssueUrl {
    pub tracker: IssueTrackerKind,
    /// Lowercase hostname (without port).
    pub host: String,
    /// Lowercase `host[:port]` authority from the URL.
    pub authority: String,
    /// Lowercase tracker scope (`owner/repo`, `org/project`, Linear team key, etc.).
    pub scope: String,
    /// Optional alternate scope spelling (e.g. Azure DevOps legacy `defaultcollection/<project>`).
    pub scope_alias: Option<String>,
    pub key: String,
    /// Linear workspace slug from `https://linear.app/<workspace>/issue/<TEAM>-<n>`.
    pub workspace: Option<String>,
}

/// Resolved issue link target ready to open in the right panel or preview card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIssueLink {
    pub environment_id: String,
    pub project_id: String,
    pub reference: IssueRef,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectIssueBinding {
    tracker: String,
    host: String,
    scope: String,
}

fn project_issue_bindings(project: &IssueLinkProject) -> Vec<ProjectIssueBinding> {
    let Some(identity) = project.repository_identity.as_ref() else {
        return Vec::new();
    };
    repository_issue_bindings(identity)
        .into_iter()
        .map(|binding| ProjectIssueBinding {
            tracker: binding.tracker.to_lowercase(),
            host: binding.host.to_lowercase(),
            scope: binding.scope.to_lowercase(),
        })
        .collect()
}

fn url_authority(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_lowercase();
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn decode_url_path_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| segment.to_owned())
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_positive_u64(raw: &str) -> Option<u64> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<u64>().ok().filter(|n| *n > 0)
}

fn is_github_host(hostname: &str) -> bool {
    let host = hostname.to_lowercase();
    host == "github.com"
        || host.ends_with(".ghe.com")
        || host.split('.').any(|label| label == "github")
}

fn is_forgejo_host(hostname: &str) -> bool {
    let host = hostname.to_lowercase();
    host == "codeberg.org"
        || host
            .split('.')
            .any(|label| label == "forgejo" || label == "gitea")
}

/// Parses `https://linear.app/<workspace>/issue/<TEAM>-<n>[/<slug>]` (`parseLinearIssueUrl`).
pub fn parse_linear_issue_url(raw: &str) -> Option<ParsedIssueUrl> {
    let url = Url::parse(raw.trim()).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let authority = url_authority(&url)?;
    if authority != LINEAR_HOST {
        return None;
    }
    let parts: Vec<&str> = url.path().split('/').collect();
    // parts[0] is "" before leading '/'
    let workspace = *parts.get(1)?;
    let resource = *parts.get(2)?;
    let key = *parts.get(3)?;
    if workspace.is_empty() || resource != "issue" || key.is_empty() {
        return None;
    }
    let (team, num) = key.split_once('-')?;
    let team_ok = !team.is_empty()
        && team.as_bytes()[0].is_ascii_uppercase()
        && team
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    let num_ok = !num.is_empty()
        && num.as_bytes()[0] != b'0'
        && num.bytes().all(|b| b.is_ascii_digit());
    if !team_ok || !num_ok {
        return None;
    }
    Some(ParsedIssueUrl {
        tracker: IssueTrackerKind::Linear,
        host: LINEAR_HOST.to_owned(),
        authority: LINEAR_HOST.to_owned(),
        scope: team.to_owned(),
        scope_alias: None,
        key: key.to_owned(),
        workspace: Some(workspace.to_owned()),
    })
}

/// Whether `url` is a standalone tracker's issue link (`isStandaloneIssueLink`).
pub fn is_standalone_issue_link(url: &str) -> bool {
    parse_linear_issue_url(url).is_some()
}

/// Cheap pre-check so text without a standalone tracker URL never fetches `issueTrackers.list`
/// (`mayContainStandaloneIssueLink`).
pub fn may_contain_standalone_issue_link(text: &str) -> bool {
    text.contains("linear.app/")
}

fn parse_github_issue(url: &Url, known: bool) -> Option<ParsedIssueUrl> {
    let host = url.host_str()?.to_lowercase();
    if !known && !is_github_host(&host) {
        return None;
    }
    let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 4 || segments[2] != "issues" {
        return None;
    }
    let number = parse_positive_u64(segments[3])?;
    let scope = format!("{}/{}", segments[0], segments[1]).to_lowercase();
    Some(ParsedIssueUrl {
        tracker: IssueTrackerKind::GitHub,
        host,
        authority: url_authority(url)?,
        scope,
        scope_alias: None,
        key: number.to_string(),
        workspace: None,
    })
}

fn parse_gitlab_issue(url: &Url) -> Option<ParsedIssueUrl> {
    let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    let dash = segments.iter().position(|s| *s == "-")?;
    // GitLab projects live under at least group/project before `/-/`.
    if dash < 2 || segments.len() < dash + 3 {
        return None;
    }
    if !matches!(segments[dash + 1], "issues" | "work_items") {
        return None;
    }
    let number = parse_positive_u64(segments[dash + 2])?;
    let scope = segments[..dash].join("/").to_lowercase();
    Some(ParsedIssueUrl {
        tracker: IssueTrackerKind::GitLab,
        host: url.host_str()?.to_lowercase(),
        authority: url_authority(url)?,
        scope,
        scope_alias: None,
        key: number.to_string(),
        workspace: None,
    })
}

fn parse_azure_devops_issue(url: &Url) -> Option<ParsedIssueUrl> {
    let host = url.host_str()?.to_lowercase();
    let legacy = host.ends_with(".visualstudio.com");
    let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    // Standard: /{org}/{project}/_workitems/edit/{id}
    // Legacy:   /{project}/_workitems/edit/{id} or /DefaultCollection/{project}/_workitems/edit/{id}
    let workitems_idx = segments.iter().position(|s| *s == "_workitems")?;
    if segments.get(workitems_idx + 1).copied() != Some("edit") {
        return None;
    }
    let number = parse_positive_u64(segments.get(workitems_idx + 2).copied()?)?;
    let scope_segments = match workitems_idx {
        2 => &segments[..2],
        1 if legacy => &segments[..1],
        _ => return None,
    };
    let scope = scope_segments
        .iter()
        .map(|seg| decode_url_path_segment(seg))
        .collect::<Vec<_>>()
        .join("/")
        .to_lowercase();
    let scope_alias = if legacy {
        if let Some(rest) = scope.strip_prefix("defaultcollection/")
            && !rest.is_empty()
            && !rest.contains('/')
        {
            Some(rest.to_owned())
        } else if !scope.contains('/') {
            Some(format!("defaultcollection/{scope}"))
        } else {
            None
        }
    } else {
        None
    };
    Some(ParsedIssueUrl {
        tracker: IssueTrackerKind::AzureDevOps,
        host,
        authority: url_authority(url)?,
        scope,
        scope_alias,
        key: number.to_string(),
        workspace: None,
    })
}

fn parse_forgejo_issue(url: &Url, known: bool) -> Option<ParsedIssueUrl> {
    let host = url.host_str()?.to_lowercase();
    if !known && !is_forgejo_host(&host) {
        return None;
    }
    let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    let issues_idx = segments.iter().rposition(|s| *s == "issues")?;
    if issues_idx < 2 || segments.len() <= issues_idx + 1 {
        return None;
    }
    let number = parse_positive_u64(segments[issues_idx + 1])?;
    let scope = segments[..issues_idx].join("/").to_lowercase();
    Some(ParsedIssueUrl {
        tracker: IssueTrackerKind::Forgejo,
        host,
        authority: url_authority(url)?,
        scope,
        scope_alias: None,
        key: number.to_string(),
        workspace: None,
    })
}

fn parse_tanstack_source_issue(url: &Url, known: bool) -> Option<ParsedIssueUrl> {
    let host = url.host_str()?.to_lowercase();
    if !known && host != "source.tanstack.com" {
        return None;
    }
    let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 4 || segments[2] != "issues" {
        return None;
    }
    let owner = segments[0]
        .strip_prefix('@')
        .or_else(|| {
            segments[0]
                .get(..3)
                .filter(|p| p.eq_ignore_ascii_case("%40"))
                .map(|_| &segments[0][3..])
        })
        .unwrap_or(segments[0]);
    if owner.is_empty() || owner.contains('@') || owner.contains('%') {
        return None;
    }
    let number = parse_positive_u64(segments[3])?;
    let scope = format!("{owner}/{}", segments[1]).to_lowercase();
    Some(ParsedIssueUrl {
        tracker: IssueTrackerKind::TanStackSource,
        host,
        authority: url_authority(url)?,
        scope,
        scope_alias: None,
        key: number.to_string(),
        workspace: None,
    })
}

/// Parses a git-host issue URL (`GitHub`, `GitLab`, `Azure DevOps`, `Forgejo`, `TanStack Source`),
/// honoring `known_provider` when a workspace project vouches for a custom hostname
/// (`pingdotgg/t3code#38269`).
pub fn parse_git_host_issue_url(raw: &str, known_provider: Option<&str>) -> Option<ParsedIssueUrl> {
    let url = Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if let Some(provider) = known_provider {
        return match provider {
            "github" => parse_github_issue(&url, true),
            "gitlab" => parse_gitlab_issue(&url),
            "azure-devops" => parse_azure_devops_issue(&url),
            "forgejo" => parse_forgejo_issue(&url, true),
            "tanstack-source" => parse_tanstack_source_issue(&url, true),
            _ => None,
        };
    }
    parse_tanstack_source_issue(&url, false)
        .or_else(|| parse_github_issue(&url, false))
        .or_else(|| parse_gitlab_issue(&url))
        .or_else(|| parse_azure_devops_issue(&url))
        .or_else(|| parse_forgejo_issue(&url, false))
}

/// Parses any recognized issue URL (git-host or Linear) without requiring project context.
pub fn parse_issue_url(raw: &str) -> Option<ParsedIssueUrl> {
    parse_git_host_issue_url(raw, None).or_else(|| parse_linear_issue_url(raw))
}

fn prefer_first_project<'a, T>(
    candidates: Vec<(&'a IssueLinkProject, T)>,
    preferred_project_id: Option<&str>,
) -> Option<(&'a IssueLinkProject, T)> {
    if let Some(pref) = preferred_project_id
        && let Some(pos) = candidates.iter().position(|(p, _)| p.id == pref)
    {
        return Some(candidates.into_iter().nth(pos).unwrap());
    }
    candidates.into_iter().next()
}

/// Resolves an issue URL to the project and `IssueRef` through which it should be read
/// (`resolveIssueLinkTarget` in `packages/client-runtime/src/issueLinks.ts`).
pub fn resolve_issue_link_target(
    url: &str,
    projects: &[IssueLinkProject],
    preferred_project_id: Option<&str>,
    trackers: Option<&IssueTrackerConnectionList>,
) -> Option<ResolvedIssueLink> {
    let parsed_url = Url::parse(url.trim()).ok()?;
    let authority = url_authority(&parsed_url);
    let hostname = parsed_url.host_str().map(str::to_lowercase);

    let git_parsed = parse_git_host_issue_url(url, None).or_else(|| {
        let authority_str = authority.as_deref()?;
        let hostname_str = hostname.as_deref()?;
        for project in projects {
            for binding in project_issue_bindings(project) {
                if binding.host != hostname_str && binding.host != authority_str {
                    continue;
                }
                if let Some(known) = parse_git_host_issue_url(url, Some(&binding.tracker)) {
                    return Some(known);
                }
            }
        }
        None
    });

    if let Some(link) = git_parsed {
        let on_host: Vec<(&IssueLinkProject, ProjectIssueBinding)> = projects
            .iter()
            .flat_map(|project| {
                project_issue_bindings(project)
                    .into_iter()
                    .filter(|binding| {
                        let matches_tracker = binding.tracker == link.tracker.as_str();
                        let matches_host = binding.host == link.host
                            || authority.as_deref() == Some(binding.host.as_str());
                        matches_tracker && matches_host
                    })
                    .map(move |binding| (project, binding))
            })
            .collect();
        let on_authority: Vec<(&IssueLinkProject, ProjectIssueBinding)> = on_host
            .iter()
            .filter(|(_, binding)| authority.as_deref() == Some(binding.host.as_str()))
            .cloned()
            .collect();
        let bound = if on_authority.is_empty() {
            on_host
        } else {
            on_authority
        };
        let own = prefer_first_project(
            bound
                .iter()
                .filter(|(_, binding)| binding.scope == link.scope)
                .cloned()
                .collect(),
            preferred_project_id,
        )
        .or_else(|| {
            let alias = link.scope_alias.as_deref()?;
            prefer_first_project(
                bound
                    .iter()
                    .filter(|(_, binding)| binding.scope == alias)
                    .cloned()
                    .collect(),
                preferred_project_id,
            )
        });
        let (lender_project, lender_binding, used_own) = match own {
            Some((project, binding)) => (project, binding, true),
            None => {
                let (project, binding) = prefer_first_project(bound, preferred_project_id)?;
                (project, binding, false)
            }
        };
        return Some(ResolvedIssueLink {
            environment_id: lender_project.environment_id.clone(),
            project_id: lender_project.id.clone(),
            reference: IssueRef {
                project_id: lender_project.id.clone(),
                tracker: IssueTrackerKind::from_wire(&lender_binding.tracker),
                host: Some(lender_binding.host.clone()),
                scope: if used_own {
                    lender_binding.scope
                } else {
                    link.scope
                },
                key: link.key,
            },
            url: url.trim().to_owned(),
        });
    }

    resolve_standalone_issue_link(url, projects, trackers?, preferred_project_id)
}

fn resolve_standalone_issue_link(
    url: &str,
    projects: &[IssueLinkProject],
    trackers: &IssueTrackerConnectionList,
    preferred_project_id: Option<&str>,
) -> Option<ResolvedIssueLink> {
    let link = parse_linear_issue_url(url)?;
    let workspace = trackers
        .trackers
        .iter()
        .find(|conn| conn.tracker == link.tracker && conn.account.is_some())
        .and_then(|conn| conn.workspace.as_deref())?;
    let link_workspace = link.workspace.as_deref()?;
    if !workspace.eq_ignore_ascii_case(link_workspace) {
        return None;
    }

    let on_tracker: Vec<(&IssueLinkProject, &super::wire::IssueTrackerProjectBinding)> = projects
        .iter()
        .filter_map(|project| {
            let binding = trackers.bindings.get(&project.id)?;
            (binding.tracker == link.tracker).then_some((project, binding))
        })
        .collect();
    let scope_lower = link.scope.to_lowercase();
    let matched_scope = |binding: &super::wire::IssueTrackerProjectBinding| {
        binding
            .scopes()
            .into_iter()
            .find(|key| key.to_lowercase() == scope_lower)
    };
    let own = prefer_first_project(
        on_tracker
            .iter()
            .filter_map(|candidate| {
                matched_scope(candidate.1).map(|scope| (candidate.0, (candidate.1, scope)))
            })
            .collect(),
        preferred_project_id,
    );
    match own {
        Some((lender_project, (lender_binding, bound_scope))) => Some(ResolvedIssueLink {
            environment_id: lender_project.environment_id.clone(),
            project_id: lender_project.id.clone(),
            reference: IssueRef {
                project_id: lender_project.id.clone(),
                tracker: lender_binding.tracker.clone(),
                host: Some(link.host),
                scope: bound_scope,
                key: link.key,
            },
            url: url.trim().to_owned(),
        }),
        None => {
            let (lender_project, lender_binding) =
                prefer_first_project(on_tracker, preferred_project_id)?;
            Some(ResolvedIssueLink {
                environment_id: lender_project.environment_id.clone(),
                project_id: lender_project.id.clone(),
                reference: IssueRef {
                    project_id: lender_project.id.clone(),
                    tracker: lender_binding.tracker.clone(),
                    host: Some(link.host),
                    scope: link.scope,
                    key: link.key,
                },
                url: url.trim().to_owned(),
            })
        }
    }
}

/// Builds a `thread.issue.link` command payload (`packages/contracts/src/orchestrationV2.ts`).
#[allow(clippy::too_many_arguments)]
pub fn link_issue_command(
    command_id: &str,
    thread_id: &str,
    key: &ThreadIssueKey,
    project_id: &str,
    url: &str,
    display_key: &str,
    title: &str,
    source: ThreadIssueLinkSource,
) -> Value {
    json!({
        "type": "thread.issue.link",
        "commandId": command_id,
        "threadId": thread_id,
        "tracker": key.tracker,
        "host": key.host.trim(),
        "scope": key.scope.trim(),
        "key": key.key.trim(),
        "projectId": project_id,
        "url": url.trim(),
        "displayKey": display_key.trim(),
        "title": title.trim(),
        "source": source,
    })
}

/// Builds a `thread.issue.unlink` command payload (`packages/contracts/src/orchestrationV2.ts`).
pub fn unlink_issue_command(command_id: &str, thread_id: &str, key: &ThreadIssueKey) -> Value {
    json!({
        "type": "thread.issue.unlink",
        "commandId": command_id,
        "threadId": thread_id,
        "tracker": key.tracker,
        "host": key.host.trim(),
        "scope": key.scope.trim(),
        "key": key.key.trim(),
    })
}

/// Builds the `issues.linkedThreads` RPC payload (`ThreadIssueKey`).
pub fn linked_threads_input(key: &ThreadIssueKey) -> Value {
    json!({
        "tracker": key.tracker,
        "host": key.host.trim(),
        "scope": key.scope.trim(),
        "key": key.key.trim(),
    })
}

/// Returns `true` when `thread` is already linked to `key` (`threadIssueKeysEqual`).
pub fn is_thread_linked_to_issue(thread: &ThreadShell, key: &ThreadIssueKey) -> bool {
    thread
        .issues
        .iter()
        .any(|existing| thread_issue_keys_equal(&existing.issue_key(), key))
}

/// Filters `issues.linkedThreads` against hidden-tag filtering (`visibleLinkedThreads`).
pub fn visible_issue_linked_threads<'a>(
    threads: &'a [IssueLinkedThread],
    loaded_thread_ids: &HashSet<String>,
    hidden_thread_ids: &HashSet<String>,
    hiding_anywhere: bool,
) -> Vec<&'a IssueLinkedThread> {
    if !hiding_anywhere {
        return threads.iter().collect();
    }
    threads
        .iter()
        .filter(|thread| {
            loaded_thread_ids.contains(&thread.id) && !hidden_thread_ids.contains(&thread.id)
        })
        .collect()
}

/// Formats the linked-threads badge tooltip label (`IssueThreadLinks.tsx`).
pub fn linked_threads_label(count: usize) -> String {
    match count {
        0 => "Linked threads".to_owned(),
        1 => "Linked from 1 thread".to_owned(),
        n => format!("Linked from {n} threads"),
    }
}

/// Folds accents via Unicode NFD decomposition (`normalizeSearchText` in `apps/web/src/lib/utils.ts`).
pub fn normalize_search_text(input: &str) -> String {
    input
        .nfd()
        .filter(|ch| !is_combining_mark(*ch))
        .collect::<String>()
        .to_lowercase()
        .trim()
        .to_owned()
}

/// Accent-folding thread-link matcher (`threadLinkMatcher` in `ThreadLinkPicker.logic.ts`).
pub fn thread_link_matches(query: &str, text: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return true;
    }
    let local = trimmed.to_lowercase();
    let folded = normalize_search_text(query);
    text.to_lowercase().contains(&local) || normalize_search_text(text).contains(&folded)
}
