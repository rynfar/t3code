//! External MCP client directory and handoff actions.
//! Ports `apps/web/src/components/mcp/mcpApps.ts`.

/// How the Connect step hands T3 Code's MCP URL to an app. Every `link` is `https`.
#[derive(Debug, Clone, Copy)]
pub enum McpAppHandoff {
    /// Opens the app's add form with T3 Code filled in.
    Open { link: fn(&str) -> String },
    /// Copies the URL, then opens the page it goes on.
    CopyUrl { link: &'static str },
    /// Copies a message for the app's chat, then opens the app.
    CopyMessage {
        link: &'static str,
        /// Opens the installed app first, given the message; `link` is the fallback.
        app_link: Option<fn(&str) -> String>,
        message: fn(&str) -> String,
    },
    /// Commands to run where the app runs.
    Commands { commands: fn(&str) -> Vec<String> },
}

#[derive(Debug, Clone, Copy)]
pub struct McpApp {
    pub id: &'static str,
    pub name: &'static str,
    pub handoff: McpAppHandoff,
    /// "Set up another way", next to the URL.
    pub steps: &'static [&'static str],
    /// One line under the primary action.
    pub note: Option<&'static str>,
    pub docs: Option<&'static str>,
    /// Kept out of the grid until its sign-in works.
    pub hidden: bool,
}

impl PartialEq for McpApp {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for McpApp {}

/// JavaScript `encodeURIComponent`: leaves RFC 2396 unreserved + `! ~ * ' ( )` unescaped.
pub fn encode_uri_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        let unescaped = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')');
        if unescaped {
            out.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn poke_link(url: &str) -> String {
    format!(
        "https://poke.com/integrations/new?name=T3%20Code&url={}",
        encode_uri_component(url)
    )
}

fn claude_link(url: &str) -> String {
    format!(
        "https://claude.ai/customize/connectors?modal=add-custom-connector&connectorName=T3%20Code&connectorUrl={}",
        encode_uri_component(url)
    )
}

fn grok_bot_app_link(message: &str) -> String {
    format!("grokbot://app/v1/prompt?text={}", encode_uri_component(message))
}

fn grok_bot_message(url: &str) -> String {
    format!("Add the MCP server {url} and connect it.")
}

fn muse_message(url: &str) -> String {
    format!("Create a custom connector for the MCP server {url}.")
}

fn instinct_message(url: &str) -> String {
    format!("Connect the MCP server {url}.")
}

fn openclaw_commands(url: &str) -> Vec<String> {
    vec![
        // Its MCP commands refuse to write while the Gateway runs.
        "openclaw gateway stop".to_owned(),
        format!("openclaw mcp add t3code --url {url} --transport streamable-http --auth oauth"),
        "openclaw mcp login t3code".to_owned(),
        "openclaw gateway start".to_owned(),
    ]
}

fn hermes_commands(url: &str) -> Vec<String> {
    vec![
        // `add` signs in on its first connection (30 s by default, too short to get a code);
        // `login` would clear that sign-in and need a second code.
        format!("hermes mcp add t3code --url {url} --auth oauth --connect-timeout 315"),
    ]
}

pub const MCP_APPS: &[McpApp] = &[
    McpApp {
        id: "poke",
        name: "Poke",
        handoff: McpAppHandoff::Open { link: poke_link },
        steps: &[
            "Open poke.com/integrations/new.",
            "Paste the URL and add the integration.",
        ],
        note: None,
        docs: Some("https://poke.com/docs/mcp-servers"),
        hidden: false,
    },
    McpApp {
        id: "claude",
        name: "Claude",
        handoff: McpAppHandoff::Open { link: claude_link },
        steps: &[
            "In Claude, open Customize, then Connectors.",
            "Choose Add custom connector and paste the URL.",
            "The Free plan allows one custom connector.",
        ],
        note: None,
        docs: Some("https://claude.com/docs/connectors/building/directory-vs-custom"),
        hidden: false,
    },
    McpApp {
        id: "chatgpt",
        name: "ChatGPT",
        handoff: McpAppHandoff::CopyUrl {
            link: "https://chatgpt.com/plugins",
        },
        steps: &[
            "Open chatgpt.com/plugins on the web, then + and Add custom MCP server.",
            "Paste the URL. Choose OAuth.",
            "Create it as a plugin, then install it.",
        ],
        note: None,
        docs: Some("https://developers.openai.com/api/docs/guides/custom-mcp-server"),
        hidden: false,
    },
    McpApp {
        id: "grok-bot",
        name: "Grok Bot",
        // No web chat. The app link prefills the message only into an empty box, so it is copied too.
        handoff: McpAppHandoff::CopyMessage {
            link: "https://x.ai/bot",
            app_link: Some(grok_bot_app_link),
            message: grok_bot_message,
        },
        steps: &["Send this message to any of your Grok Bots."],
        note: Some("Shared with all your Grok Bots."),
        docs: Some("https://docs.x.ai/grok-bot/computer-and-apps"),
        hidden: false,
    },
    McpApp {
        id: "gemini",
        name: "Gemini",
        handoff: McpAppHandoff::CopyUrl {
            link: "https://gemini.google.com/apps",
        },
        steps: &[
            "In Gemini, open Settings, then Connected apps.",
            "Choose Custom apps, then Add, and paste the URL.",
        ],
        note: None,
        docs: Some("https://support.google.com/gemini/answer/17209137"),
        // Gemini's sign-in and its tool schema checks do not work with T3 Code yet.
        hidden: true,
    },
    McpApp {
        id: "muse",
        name: "Muse",
        handoff: McpAppHandoff::CopyMessage {
            link: "https://muse.ai",
            app_link: None,
            message: muse_message,
        },
        steps: &["Send this message to Muse."],
        note: None,
        docs: Some("https://www.meta.com/help/artificial-intelligence/1687253048996149/"),
        hidden: false,
    },
    McpApp {
        id: "instinct",
        name: "Instinct",
        handoff: McpAppHandoff::CopyMessage {
            link: "https://app.instinct.com",
            app_link: None,
            message: instinct_message,
        },
        steps: &["In Instinct, open Settings, then MCP connections, if your account has it."],
        note: None,
        docs: None,
        hidden: false,
    },
    McpApp {
        id: "openclaw",
        name: "OpenClaw",
        handoff: McpAppHandoff::Commands {
            commands: openclaw_commands,
        },
        steps: &[
            "Open the Control UI, then + and Plugins, and choose Add MCP server.",
            "Paste the URL, and choose Streamable HTTP and OAuth.",
            "Select Sign in for it under Settings \u{2192} MCP.",
        ],
        note: None,
        docs: Some("https://docs.openclaw.ai/tools/mcp"),
        hidden: false,
    },
    McpApp {
        id: "hermes-agent",
        name: "Hermes Agent",
        handoff: McpAppHandoff::Commands {
            commands: hermes_commands,
        },
        steps: &[
            "Add t3code under mcp_servers in ~/.hermes/config.yaml, with the URL and auth: oauth.",
            "Run hermes mcp login t3code.",
        ],
        note: Some("On a remote host, paste the final URL back when asked."),
        docs: Some("https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp"),
        hidden: false,
    },
];

/// The "Something else…" choice: the URL and general steps only.
pub const OTHER_MCP_APP_STEPS: &[&str] = &["Add a remote MCP server with OAuth."];

/// Non-hidden apps shown in the wizard grid.
pub fn visible_mcp_apps() -> impl Iterator<Item = &'static McpApp> {
    MCP_APPS.iter().filter(|app| !app.hidden)
}

pub fn find_mcp_app(id: &str) -> Option<&'static McpApp> {
    MCP_APPS.iter().find(|app| app.id == id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpHandoffCopy {
    pub text: String,
    pub title: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpHandoffAction {
    pub label: String,
    pub link: String,
    pub app_link: Option<String>,
    pub copy: Option<McpHandoffCopy>,
}

/// The primary button for `app`: its label, the https page it opens, and what it copies first.
/// `None` for apps set up with commands. It never copies the approval code.
pub fn mcp_handoff_action(app: &McpApp, mcp_url: &str) -> Option<McpHandoffAction> {
    match app.handoff {
        McpAppHandoff::Open { link } => Some(McpHandoffAction {
            label: format!("Open in {}", app.name),
            link: link(mcp_url),
            app_link: None,
            copy: None,
        }),
        McpAppHandoff::CopyUrl { link } => Some(McpHandoffAction {
            label: format!("Copy URL and open {}", app.name),
            link: link.to_owned(),
            app_link: None,
            copy: Some(McpHandoffCopy {
                text: mcp_url.to_owned(),
                title: "URL copied",
            }),
        }),
        McpAppHandoff::CopyMessage {
            link,
            app_link,
            message,
        } => {
            let msg = message(mcp_url);
            Some(McpHandoffAction {
                label: format!("Copy message and open {}", app.name),
                link: link.to_owned(),
                app_link: app_link.map(|build| build(&msg)),
                copy: Some(McpHandoffCopy {
                    text: msg,
                    title: "Message copied",
                }),
            })
        }
        McpAppHandoff::Commands { .. } => None,
    }
}

/// Message shown in "Set up another way" for `copy-message` apps.
pub fn mcp_handoff_message(app: &McpApp, mcp_url: &str) -> Option<String> {
    match app.handoff {
        McpAppHandoff::CopyMessage { message, .. } => Some(message(mcp_url)),
        _ => None,
    }
}

/// Commands shown for `commands` apps.
pub fn mcp_handoff_commands(app: &McpApp, mcp_url: &str) -> Option<Vec<String>> {
    match app.handoff {
        McpAppHandoff::Commands { commands } => Some(commands(mcp_url)),
        _ => None,
    }
}

/// Whether `url` is an allowed external link shape (`https:` or the Grok Bot desktop launch URI).
pub fn is_safe_mcp_handoff_url(raw: &str) -> bool {
    let Ok(parsed) = url::Url::parse(raw) else {
        return false;
    };
    if parsed.scheme() == "https" {
        return parsed.host_str().is_some_and(|h| !h.is_empty());
    }
    parsed.scheme() == "grokbot"
        && parsed.host_str() == Some("app")
        && parsed.path() == "/v1/prompt"
}

#[cfg(test)]
mod tests {
    use super::*;

    const MCP_URL: &str = "https://prod-a1.t3coderelay.com/mcp?x=1&y=two words";

    #[test]
    fn mcp_app_links_fill_in_t3_code_and_encoded_url() {
        let opened = |id: &str| {
            let app = find_mcp_app(id).expect("app exists");
            let action = mcp_handoff_action(app, MCP_URL).expect("action exists");
            url::Url::parse(&action.link).expect("valid url")
        };
        let poke = opened("poke");
        let poke_params: std::collections::HashMap<_, _> = poke.query_pairs().into_owned().collect();
        assert_eq!(poke_params.get("name").map(String::as_str), Some("T3 Code"));
        assert_eq!(poke_params.get("url").map(String::as_str), Some(MCP_URL));

        let claude = opened("claude");
        let claude_params: std::collections::HashMap<_, _> =
            claude.query_pairs().into_owned().collect();
        assert_eq!(
            claude_params.get("connectorName").map(String::as_str),
            Some("T3 Code")
        );
        assert_eq!(
            claude_params.get("connectorUrl").map(String::as_str),
            Some(MCP_URL)
        );
    }

    #[test]
    fn every_mcp_app_opens_only_https_and_allowed_app_links() {
        for app in MCP_APPS {
            let action = mcp_handoff_action(app, MCP_URL);
            for url_str in [
                action.as_ref().map(|a| a.link.as_str()),
                app.docs,
            ]
            .into_iter()
            .flatten()
            {
                let parsed = url::Url::parse(url_str).expect("valid https url");
                assert_eq!(parsed.scheme(), "https", "app {}", app.name);
                assert!(is_safe_mcp_handoff_url(url_str));
            }
            let app_link = action
                .as_ref()
                .and_then(|a| a.app_link.as_deref())
                .map(|raw| {
                    assert!(is_safe_mcp_handoff_url(raw));
                    url::Url::parse(raw).expect("valid app_link")
                });
            assert_eq!(
                app_link.as_ref().map(url::Url::scheme),
                if app.id == "grok-bot" {
                    Some("grokbot")
                } else {
                    None
                }
            );
            if let Some(link) = app_link {
                let text = link
                    .query_pairs()
                    .find(|(k, _)| k == "text")
                    .map(|(_, v)| v.into_owned());
                assert_eq!(
                    text.as_deref(),
                    action
                        .as_ref()
                        .and_then(|a| a.copy.as_ref())
                        .map(|c| c.text.as_str())
                );
            }
        }
        assert!(!is_safe_mcp_handoff_url("javascript:alert(1)"));
        assert!(!is_safe_mcp_handoff_url("http://example.com"));
        assert!(!is_safe_mcp_handoff_url("grokbot://evil/path"));
    }

    #[test]
    fn visible_apps_exclude_hidden_gemini_and_keep_commands_apps() {
        let visible: Vec<&str> = visible_mcp_apps().map(|a| a.id).collect();
        assert_eq!(
            visible,
            vec![
                "poke",
                "claude",
                "chatgpt",
                "grok-bot",
                "muse",
                "instinct",
                "openclaw",
                "hermes-agent",
            ]
        );
        let openclaw = find_mcp_app("openclaw").unwrap();
        assert!(mcp_handoff_action(openclaw, "https://a.test/mcp").is_none());
        let cmds = mcp_handoff_commands(openclaw, "https://a.test/mcp").unwrap();
        assert_eq!(cmds.len(), 4);
        assert!(cmds[1].contains("https://a.test/mcp"));

        let hermes = find_mcp_app("hermes-agent").unwrap();
        let hcmds = mcp_handoff_commands(hermes, "https://a.test/mcp").unwrap();
        assert_eq!(
            hcmds,
            vec!["hermes mcp add t3code --url https://a.test/mcp --auth oauth --connect-timeout 315"]
        );
    }
}
