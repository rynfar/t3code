//! What the branch picker accepts as a pull request to check out (`parsePullRequestReference` in
//! `packages/client-runtime/src/pullRequestLinkInput.ts`) over each source control definition's
//! `checkoutCommandArgument` and `isChangeRequestReference`
//! (`packages/source-control-*/src/shared/definition.ts`), in the registry's order.
//!
//! GitHub Enterprise Server runs on any hostname, so any HTTP(S) host with the
//! `<owner>/<repo>/pull/<n>` route reads as GitHub (`#29892`); legacy Azure DevOps hosts take an
//! optional `DefaultCollection` segment (`#29763`).

use std::sync::LazyLock;

use regex::Regex;

fn pattern(source: &str) -> Regex {
    Regex::new(source).expect("change request reference pattern")
}

/// The CLI checkout commands, each capturing its arguments.
static GITHUB_CHECKOUT: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^gh\s+pr\s+checkout\s+(.+)$"));
static CURSOR_ORIGIN_CHECKOUT: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^origin\s+pr\s+checkout\s+(.+)$"));
static GITLAB_CHECKOUT: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^glab\s+mr\s+checkout\s+(.+)$"));
static FORGEJO_CHECKOUT: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^tea\s+(?:pr|pulls?)\s+(?:checkout|co)\s+(.+)$"));
static AZURE_CHECKOUT: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^az\s+repos\s+pr\s+checkout\s+(.+)$"));
static TANSTACK_SOURCE_CHECKOUT: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^src\s+pr\s+checkout\s+(.+)$"));

/// The definitions' `CHANGE_REQUEST_REFERENCE` patterns for hosts that run on any hostname.
static GITHUB_REFERENCE: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^https?://[^/?#\s]+/[^/\s]+/[^/\s]+/pull/([0-9]+)(?:[/?#].*)?$"));
static GITLAB_REFERENCE: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^https?://[^/?#\s]+/[^/?#\s]+(?:/[^/?#\s]+)+/-/merge_requests/([0-9]+)(?:[/?#].*)?$"));
static FORGEJO_REFERENCE: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^https?://[^/\s]+/(?:[^/\s]+/)+[^/\s]+/pulls/([0-9]+)(?:[/?#].*)?$"));
static AZURE_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"(?i)^https://(?:dev\.azure\.com/[^/\s]+(?:/[^/\s]+)?/|[^/\s]+\.visualstudio\.com/(?:DefaultCollection/)?(?:[^/\s]+/)?)_git/[^/\s]+/pullrequest/([0-9]+)(?:[/?#].*)?$")
});

/// The `CHANGE_REQUEST_PATH`s of hosts that also check the hostname.
static CURSOR_ORIGIN_PATH: LazyLock<Regex> = LazyLock::new(|| pattern(r"^/codebase/([^/]+/[^/]+)/pull/([0-9]+)(?:/|$)"));
static GITCAFE_PATH: LazyLock<Regex> = LazyLock::new(|| pattern(r"^/([^/]+/[^/]+)/pulls?/([0-9]+)(?:/|$)"));
static BITBUCKET_PATH: LazyLock<Regex> = LazyLock::new(|| pattern(r"^/([^/]+/[^/]+)/pull-requests/([0-9]+)(?:/|$)"));
static TANSTACK_SOURCE_PATH: LazyLock<Regex> = LazyLock::new(|| pattern(r"^/(?:@|%40)?([^/@%]+/[^/]+)/pull/([0-9]+)(?:/|$)"));

const GITCAFE_HOSTS: &[&str] = &["git.cafe", "staging.git.cafe"];
const TANSTACK_SOURCE_HOST: &str = "source.tanstack.com";
/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

fn captured(pattern: &Regex, input: &str) -> Option<String> {
    pattern.captures(input).and_then(|captures| captures.get(1)).map(|argument| argument.as_str().trim().to_owned())
}

/// Azure's `checkoutCommandId`: `--id <n>`, `-i <n>`, `--id=<n>`, else the first positional.
fn azure_checkout_id(args: &str) -> Option<String> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    for (index, part) in parts.iter().enumerate() {
        if *part == "--id" || *part == "-i" {
            return parts.get(index + 1).map(|id| (*id).to_owned());
        }
        if let Some(id) = part.strip_prefix("--id=") {
            return (!id.is_empty()).then(|| id.to_owned());
        }
    }
    parts.into_iter().find(|part| !part.starts_with('-')).map(str::to_owned)
}

/// The first definition's `checkoutCommandArgument` that reads `input`.
fn checkout_command_argument(input: &str) -> Option<String> {
    captured(&GITHUB_CHECKOUT, input)
        .or_else(|| captured(&CURSOR_ORIGIN_CHECKOUT, input))
        .or_else(|| captured(&GITLAB_CHECKOUT, input))
        .or_else(|| captured(&FORGEJO_CHECKOUT, input))
        .or_else(|| AZURE_CHECKOUT.captures(input).and_then(|captures| azure_checkout_id(captures.get(1)?.as_str())))
        .or_else(|| captured(&TANSTACK_SOURCE_CHECKOUT, input))
}

/// `parseChangeRequestPath`'s acceptance: an HTTP(S) URL whose path matches with a positive, safe
/// change request number.
fn path_names_change_request(url: &url::Url, route: &Regex) -> bool {
    if url.scheme() != "https" && url.scheme() != "http" {
        return false;
    }
    route.captures(url.path()).and_then(|captures| captures.get(2)).and_then(|number| number.as_str().parse::<u64>().ok()).is_some_and(|number| number > 0 && number <= MAX_SAFE_INTEGER)
}

/// `isBitbucketHost`.
fn is_bitbucket_host(host: &str) -> bool {
    host == "bitbucket.org" || host.ends_with(".bitbucket.org") || host.split('.').any(|label| label == "bitbucket")
}

/// Whether any definition's `isChangeRequestReference` accepts `reference`.
pub fn is_change_request_reference(reference: &str) -> bool {
    let Ok(url) = url::Url::parse(reference) else { return false };
    let Some(host) = url.host_str().filter(|host| !host.is_empty()).map(str::to_lowercase) else {
        return false;
    };
    if url.scheme() != "https" && url.scheme() != "http" {
        return false;
    }
    if host == TANSTACK_SOURCE_HOST {
        return path_names_change_request(&url, &TANSTACK_SOURCE_PATH);
    }
    if [&GITHUB_REFERENCE, &GITLAB_REFERENCE, &FORGEJO_REFERENCE, &AZURE_REFERENCE].iter().any(|pattern| pattern.is_match(reference)) {
        return true;
    }
    (matches!(host.as_str(), "cursor.com" | "www.cursor.com") && path_names_change_request(&url, &CURSOR_ORIGIN_PATH))
        || (GITCAFE_HOSTS.contains(&host.as_str()) && path_names_change_request(&url, &GITCAFE_PATH))
        || (is_bitbucket_host(&host) && path_names_change_request(&url, &BITBUCKET_PATH))
}

/// `parsePullRequestReference`: a change request URL any definition reads, a checkout command's
/// argument, `123`, or `#123` (as `123`).
pub fn parse_pull_request_reference(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = checkout_command_argument(trimmed).unwrap_or_else(|| trimmed.to_owned());
    if normalized.is_empty() {
        return None;
    }
    if is_change_request_reference(&normalized) {
        return Some(normalized);
    }
    let digits = normalized.strip_prefix('#').unwrap_or(&normalized);
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())).then(|| digits.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_azure_devops_urls_with_or_without_default_collection_check_out() {
        for url in [
            "https://acme.visualstudio.com/project/_git/t3code/pullrequest/42",
            "https://acme.visualstudio.com/_git/t3code/pullrequest/42",
            "https://acme.visualstudio.com/DefaultCollection/_git/t3code/pullrequest/42",
            "https://acme.visualstudio.com/DefaultCollection/project/_git/t3code/pullrequest/42",
            "https://dev.azure.com/acme/_git/t3code/pullrequest/42",
            "https://dev.azure.com/acme/project/_git/t3code/pullrequest/42?_a=files",
        ] {
            assert_eq!(parse_pull_request_reference(url).as_deref(), Some(url));
        }
        // Azure DevOps reads only HTTPS, and only below `_git`.
        assert!(!is_change_request_reference("http://acme.visualstudio.com/project/_git/t3code/pullrequest/42"));
        assert!(!is_change_request_reference("https://acme.visualstudio.com/project/t3code/pullrequest/42"));
    }

    #[test]
    fn self_hosted_gitlab_merge_requests_check_out() {
        for url in ["https://code.example.com/group/project/-/merge_requests/42", "http://git.internal/group/sub/project/-/merge_requests/7"] {
            assert_eq!(parse_pull_request_reference(url).as_deref(), Some(url));
        }
        assert!(!is_change_request_reference("https://gitlab.com/g//-/merge_requests/5"));
    }

    #[test]
    fn github_enterprise_server_urls_check_out_on_any_host() {
        for url in ["https://github.acme.test/acme/web/pull/42", "https://git.corp.example/acme/web/pull/42/files?diff=split"] {
            assert_eq!(parse_pull_request_reference(url).as_deref(), Some(url));
            assert_eq!(parse_pull_request_reference(&format!("gh pr checkout {url}")).as_deref(), Some(url));
        }
        // The host must be one URL authority: no path, query or fragment before the repository.
        for url in [
            "https://github.com?x/acme/web/pull/42",
            "https://github.com#/acme/web/pull/42",
            "https://:8080/acme/web/pull/42",
            "https:///acme/web/pull/42",
            "ftp://github.com/acme/web/pull/42",
            "https://github.com/acme/web/pull/42x",
        ] {
            assert!(!is_change_request_reference(url), "{url}");
        }
    }

    #[test]
    fn hostname_bound_definitions_read_only_their_own_hosts() {
        assert!(is_change_request_reference("https://bitbucket.org/workspace/repo/pull-requests/5/diff"));
        assert!(is_change_request_reference("https://code.bitbucket.example.test/team/repo/pull-requests/6"));
        assert!(!is_change_request_reference("https://scm.example.test/workspace/repo/pull-requests/5"));
        assert!(!is_change_request_reference("https://bitbucket.org/workspace/repo/src/main"));
        assert!(is_change_request_reference("https://git.cafe/acme/web/pulls/3"));
        assert!(is_change_request_reference("https://cursor.com/codebase/acme/web/pull/3"));
        assert!(!is_change_request_reference("https://cursor.com/codebase/acme/web/pull/0"));
        assert!(is_change_request_reference("https://forge.example/git/team/repo/pulls/9"));
        assert!(is_change_request_reference("https://source.tanstack.com/@TanStack/router/pull/8882"));
        assert!(is_change_request_reference("https://source.tanstack.com/%40TanStack/router/pull/8882"));
        assert!(!is_change_request_reference("https://source.tanstack.com/@TanStack/router/pulls/8882"));
    }

    #[test]
    fn checkout_commands_yield_their_pull_request() {
        assert_eq!(parse_pull_request_reference("gh pr checkout 42").as_deref(), Some("42"));
        assert_eq!(parse_pull_request_reference("glab mr checkout 42").as_deref(), Some("42"));
        assert_eq!(parse_pull_request_reference("az repos pr checkout --id=42").as_deref(), Some("42"));
        assert_eq!(parse_pull_request_reference("az repos pr checkout --id 42 --remote-name origin").as_deref(), Some("42"));
        assert_eq!(parse_pull_request_reference("az repos pr checkout -i 7").as_deref(), Some("7"));
        assert_eq!(parse_pull_request_reference("tea pull co 7").as_deref(), Some("7"));
        assert_eq!(parse_pull_request_reference("origin pr checkout #5").as_deref(), Some("5"));
        assert_eq!(parse_pull_request_reference("src pr checkout 88").as_deref(), Some("88"));
        assert_eq!(parse_pull_request_reference(" #12 ").as_deref(), Some("12"));
    }

    #[test]
    fn non_pull_request_input_is_rejected() {
        for input in ["feature/my-branch", "", "   ", "#", "az repos pr checkout --id", "gh pr checkout feature"] {
            assert_eq!(parse_pull_request_reference(input), None, "{input}");
        }
    }
}
