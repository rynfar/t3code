//! Linking pull requests to threads: what a thread carries (`ThreadPullRequestLink` in
//! `packages/contracts/src/threadPullRequest.ts`), parsing a pasted URL or `#42`
//! (`client-runtime/src/pullRequestLinkInput.ts` over the source control definitions' change
//! request routes) and the commands `planThreadPullRequestMutation` sends.

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::wire::{Actor, ChecksState, Mergeability, PrState, ReviewDecision, lenient};
use crate::timeline::parse_iso_ms;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkSnapshot {
    pub state: PrState,
    pub title: String,
    pub head_branch: String,
    pub base_branch: String,
    pub is_draft: bool,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub author: Option<Actor>,
    #[serde(default)]
    pub additions: Option<u64>,
    #[serde(default)]
    pub deletions: Option<u64>,
    #[serde(default)]
    pub review_decision: Option<ReviewDecision>,
    #[serde(default)]
    pub checks_state: Option<ChecksState>,
    #[serde(default)]
    pub mergeability: Option<Mergeability>,
}

/// `ThreadPullRequestLink`, as the shell's thread rows carry it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
    pub source: String,
    pub linked_at: String,
    #[serde(default)]
    pub snapshot: Option<LinkSnapshot>,
    #[serde(default)]
    pub watch: Option<Value>,
}

impl ThreadLink {
    /// `SOURCE_LABELS`.
    pub fn source_label(&self) -> &'static str {
        match self.source.as_str() {
            "manual" => "Linked by you",
            "created" => "Created from this thread",
            "agent" => "Linked by the agent",
            "stack" => "Found in the stack",
            "stack-dismissed" => "Dismissed",
            _ => "Linked",
        }
    }
}

/// Decodes `pullRequests` on a thread row, dropping links that do not decode.
pub fn thread_links<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<ThreadLink>, D::Error> {
    lenient(deserializer)
}

/// `visibleThreadPullRequests`, newest activity first (`pullRequestListLines` without stacks).
pub fn visible_links(links: &[ThreadLink]) -> Vec<&ThreadLink> {
    let mut visible: Vec<&ThreadLink> = links.iter().filter(|link| link.source != "stack-dismissed").collect();
    let updated = |link: &ThreadLink| parse_iso_ms(link.snapshot.as_ref().and_then(|s| s.updated_at.as_deref()).unwrap_or(&link.linked_at)).unwrap_or(0);
    visible.sort_by_key(|link| std::cmp::Reverse(updated(link)));
    visible
}

/// `ChangeRequestLink`: host and repository lowercased, as projects' identities spell them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRequestLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
}

/// The change request routes of the source control definitions, in claim order.
const ROUTES: &[(&str, &str)] = &[
    ("github", "/pull/"),
    ("gitlab", "/-/merge_requests/"),
    ("bitbucket", "/pull-requests/"),
    ("azure-devops", "/pullrequest/"),
    ("forgejo", "/pulls/"),
];

/// The repository path before `route` in `path`, checked against the definition's pattern.
fn repository_before<'a>(kind: &str, path: &'a str, route: &str) -> Option<(&'a str, &'a str)> {
    let index = path.find(route)?;
    let repository = path[..index].strip_prefix('/')?;
    let segments: Vec<&str> = repository.split('/').collect();
    if segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    let valid = match kind {
        "github" | "bitbucket" => segments.len() == 2,
        "gitlab" | "forgejo" => segments.len() >= 2,
        "azure-devops" => segments.len() >= 2 && segments[segments.len() - 2] == "_git",
        _ => false,
    };
    valid.then(|| (repository, &path[index + route.len()..]))
}

/// `sourceControlClients.parseChangeRequestUrl`.
pub fn parse_change_request_url(value: &str) -> Option<ChangeRequestLink> {
    let url = url::Url::parse(value.trim()).ok()?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    let host = url.host_str()?.to_lowercase();
    ROUTES.iter().find_map(|(kind, route)| {
        let (repository, rest) = repository_before(kind, url.path(), route)?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let tail = &rest[digits.len()..];
        let number: u64 = digits.parse().ok().filter(|number| *number > 0)?;
        (tail.is_empty() || tail.starts_with('/')).then(|| ChangeRequestLink {
            host: host.clone(),
            repository: repository.to_lowercase(),
            number,
            url: value.trim().to_owned(),
        })
    })
}

/// `parsePullRequestReference`: a change request URL, `gh pr checkout <ref>`, `123` or `#123`.
pub fn parse_reference(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut words = trimmed.split_whitespace();
    let normalized = match (words.next(), words.next(), words.next()) {
        (Some(gh), Some(pr), Some(checkout)) if gh.eq_ignore_ascii_case("gh") && pr.eq_ignore_ascii_case("pr") && checkout.eq_ignore_ascii_case("checkout") => {
            words.collect::<Vec<_>>().join(" ")
        }
        _ => trimmed.to_owned(),
    };
    if normalized.is_empty() {
        return None;
    }
    if parse_change_request_url(&normalized).is_some() {
        return Some(normalized);
    }
    let digits = normalized.strip_prefix('#').unwrap_or(&normalized);
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then(|| digits.to_owned())
}

/// The thread project's repository, for resolving a bare number (`pullRequestLinkProject`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkProject {
    pub provider: String,
    pub host: String,
    pub repository: String,
    pub web_url: Option<String>,
}

impl LinkProject {
    /// From a project's `repositoryIdentity`.
    pub fn from_identity(identity: &Value) -> Option<LinkProject> {
        let text = |key: &str| identity.get(key).and_then(Value::as_str).map(str::to_owned).filter(|s| !s.trim().is_empty());
        let repository = text("displayName").or_else(|| Some(format!("{}/{}", text("owner")?, text("name")?)))?;
        let web_url = text("webUrl");
        let host = web_url
            .as_deref()
            .and_then(|web| url::Url::parse(web).ok())
            .and_then(|url| url.host_str().map(str::to_lowercase))
            .or_else(|| text("canonicalKey").and_then(|key| key.split('/').next().map(str::to_lowercase)))?;
        Some(LinkProject { provider: text("provider").unwrap_or_else(|| "github".into()), host, repository, web_url })
    }

    /// `changeRequestUrl` for the hosts whose routes are known from a repository path.
    pub fn change_request_url(&self, number: u64) -> Option<String> {
        let below = |route: &str| match &self.web_url {
            Some(web) => format!("{}/{route}/{number}", web.trim_end_matches('/')),
            None => format!("https://{}/{}/{route}/{number}", self.host, self.repository),
        };
        match self.provider.as_str() {
            "github" => Some(below("pull")),
            "gitlab" => Some(below("-/merge_requests")),
            "bitbucket" => Some(below("pull-requests")),
            "forgejo" => Some(below("pulls")),
            _ => None,
        }
    }

    pub fn term(&self) -> &'static str {
        super::detail::change_request_term(&self.provider)
    }
}

/// `resolveLinkPullRequestInput`: `None` when the text is not a reference yet.
pub fn resolve_link_input(reference: &str, project: Option<&LinkProject>) -> Option<Result<ChangeRequestLink, String>> {
    let trimmed = reference.trim();
    let parsed = if parse_change_request_url(trimmed).is_some() { Some(trimmed.to_owned()) } else { parse_reference(trimmed) }?;
    if let Some(link) = parse_change_request_url(&parsed) {
        return Some(Ok(link));
    }
    let number: u64 = parsed.parse().ok().filter(|number| *number >= 1)?;
    let Some(project) = project else {
        return Some(Err("Paste the full pull request URL; this thread has no repository to resolve the number against.".into()));
    };
    match project.change_request_url(number).as_deref().and_then(parse_change_request_url) {
        Some(link) => Some(Ok(link)),
        None => Some(Err(format!("Paste a full URL; this project's host has no known {} URL.", project.term()))),
    }
}

/// The link dialog's validation line (`LinkPullRequestDialog.tsx`).
pub fn link_validation(reference: &str, resolved: Option<&Result<ChangeRequestLink, String>>) -> Option<String> {
    if reference.trim().is_empty() {
        return Some("Paste a pull request URL or enter 123 / #123.".into());
    }
    match resolved {
        None => Some("Use a pull request URL, 123, or #123.".into()),
        Some(Err(error)) => Some(error.clone()),
        Some(Ok(_)) => None,
    }
}

/// `thread.pull-request.link` with `source: "manual"`.
pub fn link_command(command_id: &str, thread_id: &str, link: &ChangeRequestLink) -> Value {
    json!({
        "type": "thread.pull-request.link",
        "commandId": command_id,
        "threadId": thread_id,
        "host": link.host,
        "repository": link.repository,
        "number": link.number,
        "url": link.url,
        "source": "manual",
    })
}

/// `thread.pull-request.unlink`.
pub fn unlink_command(command_id: &str, thread_id: &str, link: &ThreadLink) -> Value {
    json!({
        "type": "thread.pull-request.unlink",
        "commandId": command_id,
        "threadId": thread_id,
        "host": link.host.trim().to_lowercase(),
        "repository": link.repository.trim().to_lowercase(),
        "number": link.number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_change_request_urls_by_route() {
        let github = parse_change_request_url("https://github.com/Acme/App/pull/42/files").unwrap();
        assert_eq!((github.host.as_str(), github.repository.as_str(), github.number), ("github.com", "acme/app", 42));
        let gitlab = parse_change_request_url("https://gitlab.com/group/sub/repo/-/merge_requests/7").unwrap();
        assert_eq!(gitlab.repository, "group/sub/repo");
        assert!(parse_change_request_url("https://github.com/acme/app/pull/0").is_none());
        assert!(parse_change_request_url("https://github.com/acme/app/issues/4").is_none());
        assert!(parse_change_request_url("ftp://github.com/acme/app/pull/4").is_none());
        assert!(parse_change_request_url("https://github.com/acme/app/pull/4x").is_none());
    }

    #[test]
    fn parses_numbers_and_checkout_commands() {
        assert_eq!(parse_reference("#42").as_deref(), Some("42"));
        assert_eq!(parse_reference(" 7 ").as_deref(), Some("7"));
        assert_eq!(parse_reference("gh pr checkout 12").as_deref(), Some("12"));
        assert_eq!(parse_reference("abc"), None);
        assert_eq!(parse_reference(""), None);
    }

    #[test]
    fn resolves_numbers_against_the_project_repository() {
        let project = LinkProject::from_identity(&serde_json::json!({
            "canonicalKey": "github.com/acme/app", "provider": "github", "owner": "acme", "name": "app",
            "locator": { "remoteUrl": "git@github.com:acme/app.git" }
        }))
        .unwrap();
        let link = resolve_link_input("#9", Some(&project)).unwrap().unwrap();
        assert_eq!(link.url, "https://github.com/acme/app/pull/9");
        assert!(resolve_link_input("#9", None).unwrap().is_err());
        assert!(resolve_link_input("nope", Some(&project)).is_none());
        assert_eq!(link_validation("nope", None).as_deref(), Some("Use a pull request URL, 123, or #123."));
        assert_eq!(link_validation(" ", None).as_deref(), Some("Paste a pull request URL or enter 123 / #123."));
        let command = link_command("c", "t", &link);
        assert_eq!(command["type"], "thread.pull-request.link");
        assert_eq!(command["source"], "manual");
        assert_eq!(command["repository"], "acme/app");
    }

    #[test]
    fn thread_links_skip_bad_entries_and_hide_dismissed() {
        #[derive(Deserialize)]
        struct Row {
            #[serde(default, deserialize_with = "thread_links")]
            links: Vec<ThreadLink>,
        }
        let row: Row = serde_json::from_value(serde_json::json!({ "links": [
            { "host": "github.com", "repository": "a/b", "number": 1, "url": "u1", "source": "manual", "linkedAt": "2026-01-01T00:00:00Z", "snapshot": null },
            { "host": "github.com", "repository": "a/b", "number": 2, "url": "u2", "source": "stack-dismissed", "linkedAt": "2026-01-03T00:00:00Z", "snapshot": null },
            { "host": "github.com", "number": "bad" },
            { "host": "github.com", "repository": "a/b", "number": 3, "url": "u3", "source": "agent", "linkedAt": "2026-01-02T00:00:00Z", "snapshot": null }
        ]}))
        .unwrap();
        assert_eq!(row.links.len(), 3);
        let visible: Vec<u64> = visible_links(&row.links).iter().map(|link| link.number).collect();
        assert_eq!(visible, vec![3, 1]);
        let unlink = unlink_command("c", "t", &row.links[0]);
        assert_eq!(unlink["type"], "thread.pull-request.unlink");
    }
}
