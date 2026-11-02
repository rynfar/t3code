//! Pairing links (`https://host:port/pair#token=…`) and the endpoints derived from a host.

use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingLink {
    /// `scheme://host[:port]`, no trailing slash.
    pub http_base: String,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    #[error("Enter a pairing link like https://host:3773/pair#token=…")]
    Invalid,
    #[error("That link has no pairing token.")]
    MissingToken,
}

/// Accepts a full pairing URL, or `host[:port]` plus a separate token.
pub fn parse_pairing_link(input: &str, separate_token: Option<&str>) -> Result<PairingLink, PairingError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(PairingError::Invalid);
    }
    let with_scheme = if trimmed.contains("://") { trimmed.to_owned() } else { format!("http://{trimmed}") };
    let url = Url::parse(&with_scheme).map_err(|_| PairingError::Invalid)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(PairingError::Invalid);
    }
    let token = token_from_url(&url)
        .or_else(|| separate_token.map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned))
        .ok_or(PairingError::MissingToken)?;
    Ok(PairingLink { http_base: origin(&url), token })
}

fn token_from_url(url: &Url) -> Option<String> {
    let from_pairs = |pairs: url::form_urlencoded::Parse<'_>| {
        pairs.into_iter().find(|(key, _)| key == "token").map(|(_, value)| value.into_owned())
    };
    url.fragment()
        .and_then(|fragment| from_pairs(url::form_urlencoded::parse(fragment.as_bytes())))
        .or_else(|| from_pairs(url.query_pairs()))
        .filter(|token| !token.is_empty())
}

fn origin(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    let host = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

/// `ws(s)://host[:port]/ws` for an `http(s)` base.
pub fn websocket_base(http_base: &str) -> String {
    let rest = http_base.trim_end_matches('/');
    let ws = if let Some(rest) = rest.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = rest.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        rest.to_owned()
    };
    format!("{ws}/ws")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fragment_query_and_bare_host_forms() {
        let link = parse_pairing_link("https://box.example.ts.net:5230/pair#token=abc", None).unwrap();
        assert_eq!(link, PairingLink { http_base: "https://box.example.ts.net:5230".into(), token: "abc".into() });
        let link = parse_pairing_link("http://127.0.0.1:3773/pair?token=xyz", None).unwrap();
        assert_eq!(link.token, "xyz");
        let link = parse_pairing_link("localhost:3773", Some(" tok ")).unwrap();
        assert_eq!(link, PairingLink { http_base: "http://localhost:3773".into(), token: "tok".into() });
        assert_eq!(parse_pairing_link("localhost:3773", None), Err(PairingError::MissingToken));
        assert_eq!(parse_pairing_link("", None), Err(PairingError::Invalid));
    }

    #[test]
    fn websocket_base_swaps_scheme() {
        assert_eq!(websocket_base("https://h:1"), "wss://h:1/ws");
        assert_eq!(websocket_base("http://h"), "ws://h/ws");
    }
}
