//! MCP app references, sizing and JSON-RPC host. Ports `shared/src/mcpApp.ts`,
//! `shared/src/toolOutput.ts` and `client-runtime/src/mcpApps/host.ts` (inline first slice).

mod host;
pub use host::{Action, Host, Operation, normalize_result};

use serde_json::{Value, json};

#[cfg(test)]
mod tests;

pub const PROTOCOL_VERSION: &str = "2026-01-26";
pub const DEFAULT_HEIGHT: u32 = 320;
pub const MAX_HEIGHT: u32 = 2000;
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
pub const MAX_HTML_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Reference {
    pub attachment_id: String,
    pub server: String,
    pub tool: String,
    pub resource_uri: String,
    pub csp: Value,
    pub prefers_border: bool,
}

pub fn clamp_height(height: f64) -> u32 {
    if height.is_finite() {
        height.clamp(80., f64::from(MAX_HEIGHT)).round() as u32
    } else {
        DEFAULT_HEIGHT
    }
}

pub fn from_tool_item(name: Option<&str>, output: Option<&Value>) -> Option<Reference> {
    let data = crate::html_render::compact_data(output?)?;
    let app = read_reference(data.get("t3McpApp")?)?;
    (name? == format!("{}.{}", app.server, app.tool)).then_some(app)
}

pub fn read_reference(value: &Value) -> Option<Reference> {
    let name = |key| {
        value
            .get(key)?
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.encode_utf16().count() <= 256)
            .map(str::to_owned)
    };
    let uri = value
        .get("resourceUri")?
        .as_str()
        .filter(|s| s.starts_with("ui://") && s.encode_utf16().count() <= 4096)?;
    let mut csp = json!({});
    for key in ["connectDomains", "resourceDomains", "frameDomains", "baseUriDomains"] {
        let Some(domains) = value.get("csp").and_then(|csp| csp.get(key)).and_then(Value::as_array) else {
            continue;
        };
        let domains: Vec<_> = domains.iter().filter_map(Value::as_str).filter(|s| valid_domain(s)).take(32).collect();
        if !domains.is_empty() {
            csp[key] = json!(domains);
        }
    }
    Some(Reference {
        attachment_id: name("attachmentId")?,
        server: name("server")?,
        tool: name("tool")?,
        resource_uri: uri.into(),
        csp,
        prefers_border: value.get("prefersBorder") == Some(&Value::Bool(true)),
    })
}

fn valid_domain(domain: &str) -> bool {
    if domain.len() > 256 {
        return false;
    }
    let Some((scheme, host)) = domain.split_once("://") else { return false };
    if !["https", "http", "ws", "wss"].iter().any(|s| scheme.eq_ignore_ascii_case(s)) {
        return false;
    }
    let host = host.strip_prefix("*.").unwrap_or(host);
    let host = if let Some((host, port)) = host.split_once(':') {
        if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        host
    } else {
        host
    };
    host.split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

impl Reference {
    pub fn resource(&self) -> Value {
        let mut invalid = false;
        let stem: String = self
            .tool
            .chars()
            .filter_map(|c| {
                if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                    invalid = false;
                    Some(c)
                } else if invalid {
                    None
                } else {
                    invalid = true;
                    Some('-')
                }
            })
            .take(80)
            .collect();
        json!({"_tag":"attachment", "attachmentId":self.attachment_id, "fileName":format!("{}.html", if stem.is_empty() { "app" } else { &stem }), "mimeType":"text/html", "disposition":"inline", "renderIntent":"mcp-app"})
    }

    pub fn content_security_policy(&self) -> String {
        let domains = |key, fallback: &str| {
            self.csp
                .get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| fallback.into())
        };
        let resources = domains("resourceDomains", "");
        format!(
            "default-src 'none'; script-src 'unsafe-inline' {resources}; style-src 'unsafe-inline' {resources}; img-src data: blob: {resources}; font-src data: {resources}; media-src data: blob: {resources}; connect-src {}; frame-src {}; object-src 'none'; base-uri {}; form-action 'none'",
            domains("connectDomains", "'none'"),
            domains("frameDomains", "'none'"),
            domains("baseUriDomains", "'none'")
        )
    }
}
