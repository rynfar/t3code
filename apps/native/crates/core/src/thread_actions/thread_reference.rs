//! Hosted copy contract from `client-runtime/src/threadReference.ts` and `shared/src/threadLinks.ts`.
use crate::composer::{menu::ThreadContext, records};
use serde_json::json;

const PREFIX: &str = "https://app.t3.codes/threads/";

pub struct ThreadReference {
    pub text: String,
    pub fragment: String,
    pub description: String,
}

fn identity(id: &str) -> bool {
    !id.is_empty() && id.trim() == id
}

fn segment(id: &str) -> String {
    let mut encoded = String::new();
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || b"_-".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

pub fn thread_reference(
    title: &str,
    environment: Option<&str>,
    thread_id: &str,
) -> Option<ThreadReference> {
    let environment = environment.filter(|id| identity(id))?;
    if !identity(thread_id) {
        return None;
    }
    let mut context = ThreadContext::new(environment, thread_id, title);
    let mut units = 0;
    context.label = context
        .label
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= 200
        })
        .collect();
    let record = context.to_record();
    Some(ThreadReference {
        text: format!("{PREFIX}{}/{}", segment(environment), segment(thread_id)),
        description: records::field(&record, "title").to_owned(),
        fragment: json!({"version":1,"source":{"environmentId":environment},"records":[record]})
            .to_string(),
    })
}

fn decode(segment: &str) -> Option<String> {
    let mut result = Vec::new();
    let mut bytes = segment.bytes();
    while let Some(byte) = bytes.next() {
        result.push(if byte == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?
        } else {
            byte
        });
    }
    let id = String::from_utf8(result).ok()?;
    identity(&id).then_some(id)
}

pub(crate) struct HostedReference {
    pub start: usize,
    pub end: usize,
    pub environment: String,
    pub thread: String,
}

// Rust regex has no backreferences; consume code delimiters before searching for more links.
pub(crate) fn hosted_references(text: &str) -> Vec<HostedReference> {
    use std::sync::LazyLock;
    static TOKENS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
        r"(?P<code>`+|~{3,})|\[[^\]\n]*\]\((?P<link>https://app\.t3\.codes/threads/[A-Za-z0-9_%-]+/[A-Za-z0-9_%-]+)\)|<(?P<auto>https://app\.t3\.codes/threads/[A-Za-z0-9_%-]+/[A-Za-z0-9_%-]+)>|(?P<bare>https://app\.t3\.codes/threads/[A-Za-z0-9_%-]+/[A-Za-z0-9_%-]+)"
    ).expect("hosted thread pattern")
    });
    let mut offset = 0;
    let mut found = Vec::new();
    while let Some(captures) = TOKENS.captures(&text[offset..]) {
        let whole = captures.get(0).expect("whole token");
        let start = offset + whole.start();
        let end = offset + whole.end();
        offset = end;
        if let Some(code) = captures.name("code") {
            let remaining = &text[end..];
            let fence = code.as_str().len() >= 3;
            if let Some(close) = remaining.find(code.as_str()) {
                if fence || !remaining[..close].contains('\n') {
                    offset = end + close + code.as_str().len();
                }
            } else if fence {
                break;
            }
            continue;
        }
        if captures.name("bare").is_some()
            && text[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || "%/?#-".contains(c))
        {
            continue;
        }
        let href = captures
            .name("link")
            .or_else(|| captures.name("auto"))
            .or_else(|| captures.name("bare"))
            .expect("link token")
            .as_str();
        let Some((environment, thread)) = href
            .strip_prefix(PREFIX)
            .and_then(|p| p.split_once('/'))
            .and_then(|(e, t)| Some((decode(e)?, decode(t)?)))
        else {
            continue;
        };
        found.push(HostedReference {
            start,
            end,
            environment,
            thread,
        });
    }
    found
}
