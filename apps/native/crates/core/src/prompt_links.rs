//! Ports `shared/src/promptLinks.ts`, `client-runtime/src/promptLinks.ts` and the web's
//! `components/promptLinks/copyPromptLink.ts`. Lengths use UTF-16 units, as JavaScript does.

use crate::{
    model::ModelSelection,
    server_config::ServerProvider,
    shell::{ProjectShell, ThreadShell},
};
use serde_json::Value;
use url::{Url, form_urlencoded};

pub const MAX_PROMPT_LENGTH: usize = 8_000;
pub const HOSTED_APP: &str = "https://app.t3.codes";
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub owner: String,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub repository: Repository,
    pub number: u64,
}
impl PullRequest {
    pub fn url(&self) -> String {
        format!("https://github.com/{}/{}/pull/{}", self.repository.owner, self.repository.name, self.number)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedPrompt {
    pub text: String,
    pub hidden_characters: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewThreadLink {
    New {
        prompt: LinkedPrompt,
        repository: Option<Repository>,
        provider: Option<String>,
        model: Option<String>,
    },
    PullRequest {
        pull_request: PullRequest,
        prompt: Option<LinkedPrompt>,
    },
}
impl NewThreadLink {
    pub fn prompt(&self) -> Option<&LinkedPrompt> {
        match self {
            Self::New { prompt, .. } => Some(prompt),
            Self::PullRequest { prompt, .. } => prompt.as_ref(),
        }
    }
    pub fn repository(&self) -> Option<&Repository> {
        match self {
            Self::New { repository, .. } => repository.as_ref(),
            Self::PullRequest { pull_request, .. } => Some(&pull_request.repository),
        }
    }
}
fn hidden(c: char) -> bool {
    matches!(c as u32, 0..=8|11|12|14..=31|127..=159|0xad|0x34f|0x61c|0x115f|0x1160|0x17b4|0x17b5|0x180b..=0x180f|0x200b|0x200c|0x200e|0x200f|0x202a..=0x202e|0x2060..=0x206f|0x3164|0xfe00..=0xfe0d|0xfeff|0xffa0|0xfff0..=0xfffb|0x1bca0..=0x1bca3|0x1d173..=0x1d17a|0xe0000..=0xe0fff)
}
fn emoji(c: Option<&char>) -> bool {
    c.is_some_and(|c| matches!(*c as u32, 0x1f000..=0x1faff|0x2190..=0x2bff|0xfe0f))
}
fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| matches!(c as u32, 9..=13 | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff))
}
pub fn sanitize_linked_prompt(raw: &str) -> LinkedPrompt {
    let normalized = raw.replace("\r\n", "\n").replace(['\r', '\u{2028}', '\u{2029}'], "\n");
    let mut count = 0;
    let visible: Vec<char> = normalized
        .chars()
        .filter(|&c| {
            if hidden(c) {
                count += 1;
                false
            } else {
                true
            }
        })
        .collect();
    let text: String = visible
        .iter()
        .enumerate()
        .filter_map(|(i, &c)| {
            if c == '\u{200d}' && !(emoji(i.checked_sub(1).and_then(|i| visible.get(i))) && emoji(visible.get(i + 1))) {
                count += 1;
                None
            } else {
                Some(c)
            }
        })
        .collect();
    LinkedPrompt {
        text: trim(&text).to_owned(),
        hidden_characters: count,
    }
}
fn repository(owner: &str, name: &str) -> Option<Repository> {
    let name = if name.to_ascii_lowercase().ends_with(".git") {
        &name[..name.len() - 4]
    } else {
        name
    };
    let valid_owner =
        !owner.is_empty() && owner.len() <= 39 && owner.as_bytes()[0].is_ascii_alphanumeric() && owner.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-');
    let valid_name =
        !name.is_empty() && name.len() <= 100 && name.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)) && !name.chars().all(|c| c == '.');
    (valid_owner && valid_name).then(|| Repository {
        owner: owner.to_owned(),
        name: name.to_owned(),
    })
}
fn github_path(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && matches!(url.host_str(), Some("github.com" | "www.github.com"))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.path().to_owned())
}
pub fn parse_repository(value: &str) -> Option<Repository> {
    let value = trim(value);
    let path = github_path(value);
    if path.is_some() && (value.contains('?') || value.contains('#')) {
        return None;
    }
    let value = path
        .as_deref()
        .map(|p| p.strip_prefix('/').unwrap_or(p).strip_suffix('/').unwrap_or(p.trim_start_matches('/')))
        .unwrap_or(value);
    let (owner, name) = value.split_once('/')?;
    repository(owner, name)
}
pub fn parse_pull_request(value: &str) -> Option<PullRequest> {
    let value = trim(value);
    let path = github_path(value);
    let (owner, name, digits) = if let Some(path) = path.as_deref() {
        let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
        if parts.get(2) != Some(&"pull") {
            return None;
        }
        (*parts.first()?, *parts.get(1)?, *parts.get(3)?)
    } else {
        let (owner, rest) = value.split_once('/')?;
        let (name, digits) = rest.split_once('/').or_else(|| rest.split_once('#'))?;
        (owner, name, digits)
    };
    let number = digits
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991 && digits.bytes().all(|c| c.is_ascii_digit()))?;
    Some(PullRequest {
        repository: repository(owner, name)?,
        number,
    })
}
fn params(search: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(search.trim_start_matches('?').as_bytes()).into_owned().collect()
}
fn get<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params.iter().find(|(key, _)| key == name).map(|(_, v)| v.as_str())
}
fn read_prompt(params: &[(String, String)]) -> Result<Option<LinkedPrompt>, String> {
    let Some(raw) = get(params, "prompt") else { return Ok(None) };
    if raw.encode_utf16().count() > MAX_PROMPT_LENGTH {
        return Err("This prompt is longer than 8,000 characters, so T3 Code did not open it.".into());
    }
    let prompt = sanitize_linked_prompt(raw);
    if prompt.text.is_empty() && prompt.hidden_characters > 0 {
        return Err("This prompt had only hidden characters, so it may be malicious. T3 Code did not open it.".into());
    }
    Ok((!prompt.text.is_empty()).then_some(prompt))
}
pub fn parse_thread_link_prompt(search: &str) -> Result<Option<LinkedPrompt>, String> {
    read_prompt(&params(search))
}
pub fn parse_new_thread_link(search: &str) -> Result<NewThreadLink, String> {
    let p = params(search);
    let prompt = read_prompt(&p)?;
    if let Some(pr) = get(&p, "pr") {
        return Ok(NewThreadLink::PullRequest {
            pull_request: parse_pull_request(pr).ok_or("This link names a pull request T3 Code cannot read.")?,
            prompt,
        });
    }
    let prompt = prompt.ok_or("This link has no prompt.")?;
    let repository = get(&p, "repo")
        .map(|v| parse_repository(v).ok_or("This link names a repository T3 Code cannot read."))
        .transpose()?;
    let optional = |key| get(&p, key).map(trim).filter(|v| !v.is_empty()).map(str::to_owned);
    Ok(NewThreadLink::New {
        prompt,
        repository,
        provider: optional("provider").map(|s| s.to_lowercase()),
        model: optional("model"),
    })
}
pub fn without_prompt_link_params(search: &str) -> String {
    let mut out = form_urlencoded::Serializer::new(String::new());
    for (k, v) in params(search) {
        if !["prompt", "repo", "pr", "provider", "model"].contains(&k.as_str()) {
            out.append_pair(&k, &v);
        }
    }
    let out = out.finish();
    if out.is_empty() { out } else { format!("?{out}") }
}
pub fn build_new_thread_link(prompt: &str, repo: Option<&Repository>, provider: Option<&str>, model: Option<&str>) -> String {
    let mut url = Url::parse(&format!("{HOSTED_APP}/new")).expect("hosted URL");
    {
        let mut p = url.query_pairs_mut();
        p.append_pair("prompt", prompt);
        if let Some(repo) = repo {
            p.append_pair("repo", &format!("{}/{}", repo.owner, repo.name));
        }
        if let Some(provider) = provider.filter(|s| !s.is_empty()) {
            p.append_pair("provider", provider);
            if let Some(model) = model.filter(|s| !s.is_empty()) {
                p.append_pair("model", model);
            }
        }
    }
    url.into()
}
fn attachment_only(prompt: &str) -> bool {
    let prefix = "Ultrathink:";
    let text = if prompt.get(..prefix.len()).is_some_and(|s| s.eq_ignore_ascii_case(prefix)) {
        prompt[prefix.len()..].trim_start()
    } else {
        prompt
    };
    text == "[User attached one or more files without additional text. Respond using the conversation context and the attached files.]"
}
pub fn prompt_link_for(prompt: &str, repo: Option<&Repository>, provider: Option<&str>, model: Option<&str>) -> Option<String> {
    if attachment_only(prompt) {
        return None;
    }
    let prompt = trim(prompt);
    (!prompt.is_empty() && prompt.encode_utf16().count() <= MAX_PROMPT_LENGTH).then(|| build_new_thread_link(prompt, repo, provider, model))
}
fn thread_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    let parts: Vec<_> = rest.strip_suffix('/').unwrap_or(rest).split('/').collect();
    parts.len() == 2 && parts.iter().all(|s| !s.is_empty())
}

pub fn prompt_link_app_path(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    let scheme = url.scheme();
    let app_scheme = scheme == "t3code"
        || scheme
            .strip_prefix("t3code-")
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_lowercase()));
    if app_scheme && url.port().is_some() {
        return None;
    }
    let path = if app_scheme {
        match url.host_str()? {
            "new" if url.path().is_empty() || url.path() == "/" => "/new",
            "threads" if thread_path(url.path()) => url.path().trim_end_matches('/'),
            _ => return None,
        }
    } else {
        if url.origin() != Url::parse(HOSTED_APP).ok()?.origin() {
            return None;
        }
        if url.path() == "/new" {
            "/new"
        } else if thread_path(url.path()) && url.query_pairs().any(|(k, _)| k == "prompt") {
            url.path().trim_end_matches('/')
        } else {
            return None;
        }
    };
    Some(format!("{path}{}", url.query().map(|q| format!("?{q}")).unwrap_or_default()))
}
pub fn prompt_link_deep_link(path: &str) -> Option<String> {
    let url = Url::parse(HOSTED_APP).ok()?.join(path).ok()?;
    let host = if url.path() == "/new" {
        "new".to_owned()
    } else {
        format!("threads{}", url.path())
    };
    Some(format!("t3code-native://{host}{}", url.query().map(|q| format!("?{q}")).unwrap_or_default()))
}
pub fn repository_key(identity: &Value) -> Option<&str> {
    identity
        .get("origin")
        .and_then(|v| v.get("canonicalKey"))
        .or_else(|| identity.get("canonicalKey"))
        .and_then(Value::as_str)
}
pub fn repository_of(project: &ProjectShell) -> Option<Repository> {
    parse_repository(repository_key(project.repository_identity.as_ref()?)?.strip_prefix("github.com/")?)
}
pub fn find_prompt_link_projects<'a>(projects: impl IntoIterator<Item = &'a ProjectShell>, repo: &Repository) -> Vec<&'a ProjectShell> {
    let path = format!("{}/{}", repo.owner, repo.name).to_lowercase();
    projects
        .into_iter()
        .filter(|p| {
            p.repository_identity.as_ref().and_then(repository_key).is_some_and(|k| {
                ["github.com", "git.cafe", "staging.git.cafe"]
                    .iter()
                    .any(|h| k.eq_ignore_ascii_case(&format!("{h}/{path}")))
            })
        })
        .collect()
}
pub fn find_prompt_link_pull_request_thread<'a>(threads: &'a [ThreadShell], pr: &PullRequest) -> Option<&'a ThreadShell> {
    let repo = format!("{}/{}", pr.repository.owner, pr.repository.name);
    threads
        .iter()
        .filter(|t| {
            t.lineage.side_chat != Some(true)
                && t.pull_requests.iter().any(|l| {
                    l.source != "stack-dismissed"
                        && l.host.eq_ignore_ascii_case("github.com")
                        && l.repository.eq_ignore_ascii_case(&repo)
                        && l.number == pr.number
                })
        })
        .max_by(|a, b| a.updated_at.cmp(&b.updated_at).then(std::cmp::Ordering::Greater))
}
#[derive(Debug, Clone, PartialEq)]
pub struct ModelResolution {
    pub selection: ModelSelection,
    pub exact: bool,
}
pub fn resolve_prompt_link_model_selection(providers: &[ServerProvider], provider: &str, model: Option<&str>) -> Option<ModelResolution> {
    let driver = if provider == "claude" { "claudeagent" } else { provider };
    let usable = |p: &&ServerProvider| p.driver.eq_ignore_ascii_case(driver) && p.enabled && p.availability.as_deref() != Some("unavailable");
    let instance = providers
        .iter()
        .filter(usable)
        .find(|p| p.status == "ready")
        .or_else(|| providers.iter().filter(usable).find(|p| p.status == "warning"))?;
    let requested = model.map(str::trim).filter(|v| !v.is_empty()).and_then(|v| {
        instance
            .models
            .iter()
            .find(|m| m.slug == v)
            .or_else(|| instance.models.iter().find(|m| m.name.eq_ignore_ascii_case(v)))
            .or_else(|| instance.models.iter().find(|m| m.aliases.iter().any(|a| a.eq_ignore_ascii_case(v))))
    });
    let requested = requested.or_else(|| {
        let v = model?.trim();
        let alias = match (instance.driver.as_str(), v) {
            ("codex", "gpt-5-codex" | "5.4") => "gpt-5.4",
            ("codex", "5.3" | "gpt-5.3") => "gpt-5.3-codex",
            ("codex", "5.3-spark" | "gpt-5.3-spark") => "gpt-5.3-codex-spark",
            ("cursor", "composer") => "composer-2",
            ("cursor", "composer-1") => "composer-1.5",
            ("cursor", "opus-4.6-thinking" | "opus-4.6") => "claude-opus-4-6",
            ("cursor", "sonnet-4.6-thinking" | "sonnet-4.6") => "claude-sonnet-4-6",
            ("cursor", "opus-4.5-thinking" | "opus-4.5") => "claude-opus-4-5",
            _ => v,
        };
        instance.models.iter().find(|m| m.slug == alias)
    });
    let selected = requested
        .or_else(|| instance.models.iter().find(|m| m.is_default))
        .or_else(|| instance.models.first())?;
    Some(ModelResolution {
        selection: ModelSelection {
            instance_id: instance.instance_id.clone(),
            model: selected.slug.clone(),
            options: None,
        },
        exact: model.is_none() || requested.is_some(),
    })
}
/// `placeLinkedPrompt`: a nonempty, different draft requires an explicit replacement.
pub fn needs_draft_replacement(existing: &str, prompt: &LinkedPrompt) -> bool {
    !trim(existing).is_empty() && existing != prompt.text
}

/// `pickPromptLinkPullRequestThread`: pending earlier lookups hold back later answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRequestLookup {
    Pending,
    Failed,
    Answer { environment: String, threads: Vec<LinkedThread> },
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedThread {
    pub id: String,
    pub archived_at: Option<String>,
}
pub fn pick_prompt_link_pull_request_thread(lookups: &[PullRequestLookup]) -> Option<Option<(&str, &str)>> {
    for lookup in lookups {
        match lookup {
            PullRequestLookup::Pending => return None,
            PullRequestLookup::Failed => {}
            PullRequestLookup::Answer { environment, threads } => {
                if let Some(thread) = threads.iter().find(|t| t.archived_at.is_none()) {
                    return Some(Some((environment, &thread.id)));
                }
            }
        }
    }
    Some(None)
}

#[cfg(test)]
mod tests;
