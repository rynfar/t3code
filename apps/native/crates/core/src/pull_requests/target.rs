//! Change-request link destinations: `client-runtime/src/changeRequestLinks.ts` and the shared source-control definitions.
use super::PrRef;
use serde_json::Value;

pub struct Project<'a> {
    pub id: &'a str,
    pub environment: &'a str,
    pub identity: Option<&'a Value>,
    pub config_loaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub environment: String,
    pub reference: PrRef,
}

fn text<'a>(identity: &'a Value, key: &str) -> Option<&'a str> {
    identity.get(key)?.as_str().filter(|v| !v.is_empty())
}

fn authority(url: &url::Url) -> String {
    url[url::Position::BeforeHost..url::Position::AfterPort].to_lowercase()
}

fn azure_key(key: &str) -> String {
    let parts: Vec<_> = key.split('/').collect();
    match parts.as_slice() {
        ["ssh.dev.azure.com" | "vs-ssh.visualstudio.com", "v3", org, project, repo] => format!("dev.azure.com/{org}/{project}/_git/{repo}"),
        [host, tail @ ..] if host.ends_with(".visualstudio.com") => {
            let org = host.trim_end_matches(".visualstudio.com");
            let tail = tail.strip_prefix(&["defaultcollection"]).unwrap_or(tail);
            if !org.contains('.')
                && let [project, "_git", repo] = tail
            {
                format!("dev.azure.com/{org}/{project}/_git/{repo}")
            } else {
                key.into()
            }
        }
        _ => key.into(),
    }
}

fn matches(identity: &Value, link: &super::link::ChangeRequestLink, url: &url::Url) -> Option<(bool, bool)> {
    let provider = text(identity, "provider")?;
    let remote = identity.get("locator").and_then(|v| v.get("remoteUrl")).and_then(Value::as_str).and_then(|v| url::Url::parse(v).ok());
    let link_authority = authority(url);
    let authority_matches = !((provider == "forgejo" || url.port().is_some())
        && remote.as_ref().is_some_and(|remote| matches!(remote.scheme(), "http" | "https") && authority(remote) != link_authority));
    if provider == "forgejo"
        && let Some(web) = text(identity, "webUrl").and_then(|v| url::Url::parse(v).ok()).filter(|v| matches!(v.scheme(), "http" | "https"))
    {
        if authority(&web) != link_authority {
            return None;
        }
        let repository = web.path().trim_matches('/').to_lowercase();
        let parts: Vec<_> = repository.split('/').collect();
        let mount = parts[..parts.len().saturating_sub(2)].join("/");
        return Some((authority_matches && repository == link.repository, mount.is_empty() || link.repository.starts_with(&format!("{mount}/"))));
    }
    if !authority_matches {
        return None;
    }
    let key = text(identity, "canonicalKey").unwrap_or_default().to_lowercase();
    if provider == "azure-devops" {
        return Some((azure_key(&key) == azure_key(&format!("{}/{}", link.host, link.repository)), false));
    }
    let remote_host = remote.as_ref().filter(|remote| provider == "forgejo" && matches!(remote.scheme(), "http" | "https")).map(authority);
    let host = remote_host.as_deref().unwrap_or_else(|| key.split('/').next().filter(|v| !v.trim().is_empty()).map(str::trim).unwrap_or(provider));
    let on_host = host == link.host || host == link_authority;
    let repository = text(identity, "displayName").map(str::to_owned).or_else(|| Some(format!("{}/{}", text(identity, "owner")?, text(identity, "name")?)));
    Some((on_host && repository.is_some_and(|r| r.eq_ignore_ascii_case(&link.repository)), on_host))
}

pub fn resolve(url: &str, projects: &[Project<'_>], environment: Option<&str>, project: Option<&str>, primary: Option<&str>, lend_host: bool) -> Option<Target> {
    let link = super::link::parse_change_request_url(url)?;
    if link.number > 9_007_199_254_740_991 {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    let mut candidates: Vec<_> = projects.iter().filter(|p| environment.map_or(p.config_loaded, |env| env == p.environment)).collect();
    candidates.sort_by_key(|p| (std::cmp::Reverse(Some(p.id) == project), std::cmp::Reverse(Some(p.environment) == primary)));
    let readable = |p: &&Project<'_>| p.identity.and_then(|id| matches(id, &link, &parsed));
    let target = candidates
        .iter()
        .find(|p| readable(p).is_some_and(|(repository, _)| repository))
        .or_else(|| lend_host.then(|| candidates.iter().find(|p| readable(p).is_some_and(|(_, host)| host))).flatten())?;
    if !target.config_loaded {
        return None;
    }
    Some(Target {
        environment: target.environment.into(),
        reference: PrRef { project_id: target.id.into(), host: Some(authority(&parsed)), repository: link.repository, number: link.number },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity(provider: &str, host: &str, repository: &str) -> Value {
        json!({"provider":provider,"canonicalKey":format!("{host}/{repository}"),"displayName":repository,"locator":{"remoteUrl":format!("https://{host}/{repository}.git")}})
    }

    #[test]
    fn change_request_target_requires_a_readable_project_in_the_links_environment() {
        let a = identity("github", "github.com", "other/repo");
        let b = identity("github", "github.com", "acme/app");
        let projects = [
            Project { id: "main-A", environment: "e", identity: Some(&a), config_loaded: true },
            Project { id: "side-B", environment: "e", identity: Some(&b), config_loaded: true },
            Project { id: "elsewhere", environment: "other", identity: Some(&b), config_loaded: true },
        ];
        let url = "https://github.com/acme/app/pull/7";
        let target = resolve(url, &projects, Some("e"), Some("main-A"), Some("e"), true).unwrap();
        assert_eq!(target.reference.project_id, "side-B");
        assert_eq!(target.environment, "e");
        assert_eq!(target.reference.repository, "acme/app");
        assert!(resolve("https://unconfigured.example/acme/app/pull/7", &projects, Some("e"), None, None, true).is_none());
        assert!(resolve(url, &projects, Some("missing"), None, None, true).is_none());
        let unloaded = [Project { id: "b", environment: "e", identity: Some(&b), config_loaded: false }];
        assert!(resolve(url, &unloaded, Some("e"), None, None, true).is_none());
        assert!(resolve(url, &unloaded, None, None, None, true).is_none());
        assert_eq!(resolve("https://github.com/sibling/repo/pull/8", &projects, Some("e"), Some("side-B"), None, true).unwrap().reference.project_id, "side-B");
        assert!(resolve("https://github.com/sibling/repo/pull/8", &projects, Some("e"), None, None, false).is_none());
        assert_eq!(resolve(url, &projects, None, None, Some("other"), true).unwrap().environment, "other");
    }

    #[test]
    fn change_request_target_keeps_provider_mount_port_and_azure_constraints() {
        let mut forge = identity("forgejo", "forge.example", "acme/app");
        forge["webUrl"] = json!("https://forge.example:4000/git/acme/app");
        forge["locator"]["remoteUrl"] = json!("https://forge.example:4000/git/acme/app.git");
        let p = [Project { id: "forge", environment: "e", identity: Some(&forge), config_loaded: true }];
        let target = resolve("https://forge.example:4000/git/acme/app/pulls/9", &p, Some("e"), None, None, true).unwrap();
        assert_eq!(target.reference.host.as_deref(), Some("forge.example:4000"));
        assert!(resolve("https://forge.example:5000/git/acme/app/pulls/9", &p, Some("e"), None, None, true).is_none());
        assert!(resolve("https://forge.example:4000/elsewhere/acme/app/pulls/9", &p, Some("e"), None, None, true).is_none());
        assert!(resolve("https://forge.example:4000/git/sibling/repo/pulls/9", &p, Some("e"), None, None, true).is_some());
        let mut forge_without_web = identity("forgejo", "forge.example", "acme/app");
        forge_without_web["locator"]["remoteUrl"] = json!("http://forge.example:4000/acme/app.git");
        let p = [Project { id: "forge", environment: "e", identity: Some(&forge_without_web), config_loaded: true }];
        for (repository, lend_host) in [("acme/app", false), ("sibling/repo", true)] {
            assert!(resolve(&format!("http://forge.example/{repository}/pulls/7"), &p, Some("e"), None, None, lend_host).is_none(), "{repository}");
            let target = resolve(&format!("http://forge.example:4000/{repository}/pulls/7"), &p, Some("e"), None, None, lend_host).unwrap();
            assert_eq!(target.reference.host.as_deref(), Some("forge.example:4000"));
        }
        forge["webUrl"] = json!("https://forge.example/git/acme/app");
        forge["locator"]["remoteUrl"] = json!("http://forge.example:4000/acme/app.git");
        let p = [Project { id: "forge", environment: "e", identity: Some(&forge), config_loaded: true }];
        for repository in ["acme/app", "sibling/repo"] {
            let url = format!("https://forge.example/git/{repository}/pulls/7");
            assert!(resolve(&url, &p, Some("e"), None, None, true).is_some(), "{repository}");
            assert!(resolve(&url, &p, Some("e"), None, None, false).is_none(), "{repository}");
        }
        assert!(resolve("https://forge.example/elsewhere/sibling/repo/pulls/7", &p, Some("e"), None, None, true).is_none());
        assert!(resolve("https://forge.example:4000/git/sibling/repo/pulls/7", &p, Some("e"), None, None, true).is_none());
        let azure = identity("azure-devops", "ssh.dev.azure.com", "v3/acme/project/app");
        let p = [Project { id: "azure", environment: "e", identity: Some(&azure), config_loaded: true }];
        assert!(resolve("https://dev.azure.com/acme/project/_git/app/pullrequest/3", &p, Some("e"), None, None, true).is_some());
        assert!(resolve("https://dev.azure.com/acme/project/_git/other/pullrequest/3", &p, Some("e"), None, None, true).is_none());
        for provider in ["gitlab", "bitbucket", "gitcafe", "github"] {
            let host = if provider == "gitcafe" { "git.cafe" } else { "host.example" };
            let route = match provider {
                "gitlab" => "-/merge_requests",
                "bitbucket" => "pull-requests",
                "gitcafe" => "pulls",
                _ => "pull",
            };
            let repo = identity(provider, host, "acme/app");
            let p = [Project { id: provider, environment: "e", identity: Some(&repo), config_loaded: true }];
            assert!(resolve(&format!("https://{host}/acme/app/{route}/4"), &p, Some("e"), None, None, true).is_some(), "{provider}");
        }
        let missing_provider = json!({"canonicalKey":"github.com/acme/app","displayName":"acme/app"});
        let p = [Project { id: "unknown", environment: "e", identity: Some(&missing_provider), config_loaded: true }];
        assert!(resolve("https://github.com/acme/app/pull/1", &p, Some("e"), None, None, true).is_none());
    }
}
