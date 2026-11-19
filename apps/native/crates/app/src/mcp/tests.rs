//! GPUI integration tests for the "Use in another app" MCP wizard, Command Palette wiring,
//! and OAuth agent sign-in approval dialog.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use gpui::{AppContext as _, px};
use serde_json::{Value, json};
use t3_client::store::{SavedEnvironment, SavedEnvironments};
use t3_client::supervisor::{ClientFixture, ConnectionStatus};
use t3_core::mcp::McpClientAccess;
use t3_core::mcp::apps::find_mcp_app;
use t3_core::mcp::gate::{
    CloudLinkState, ConnectReachability, McpCapabilities, McpGate, ReachabilityCheckState,
};
use t3_core::server_config::ServerConfigState;
use t3_core::shell::ShellState;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::app_state::{AppEvent, AppState};
use crate::mcp::approval::ApprovalLoadState;
use crate::mcp::wizard::{AppChoice, WizardStep};
use crate::mcp::{McpDialogs, is_open, open_agent_approval_url};
use crate::palette::{CommandPalette, Mode as PaletteMode};

struct MockHttp {
    base: String,
    recorded: Arc<Mutex<Vec<(String, Value)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockHttp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockHttp {
    async fn start(replies: Vec<(u16, Value)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let recorded_bg = recorded.clone();
        let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let n = socket.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(head) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header_str = String::from_utf8_lossy(&buf[..head]);
                        let content_len = header_str
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if buf.len() >= head + 4 + content_len {
                            break;
                        }
                    }
                }
                let raw = String::from_utf8_lossy(&buf);
                let first_line = raw.lines().next().unwrap_or("").to_owned();
                let body_str = raw.split("\r\n\r\n").nth(1).unwrap_or("");
                let body_json = serde_json::from_str::<Value>(body_str).unwrap_or(Value::Null);
                recorded_bg.lock().unwrap().push((first_line, body_json));
                let (status, reply) = queue
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or((200, json!({})));
                let body = reply.to_string();
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            }
        });
        Self {
            base,
            recorded,
            task,
        }
    }
}

#[gpui::test]
async fn palette_action_opens_use_in_another_app_wizard_and_walks_steps_and_mints_code(
    cx: &mut gpui::TestAppContext,
) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mock = runtime.block_on(MockHttp::start(vec![
        // 1. GET /api/connect/link-state
        (
            200,
            json!({
                "linked": true,
                "cloudUserId": "user_1",
                "relayUrl": "https://relay.example.test",
                "relayIssuer": "https://relay.example.test",
                "managedTunnelActive": true,
                "publishAgentActivity": true,
                "tunnelHostname": "studio.t3coderelay.com"
            }),
        ),
        // 2. POST /api/connect/reachability
        (
            200,
            json!({
                "status": "reachable",
                "checkedAt": "2026-11-19T00:00:00.000Z",
                "readyConnections": 4,
                "protocol": "quic"
            }),
        ),
        // 3. GET /api/connect/link-state (post-check refresh)
        (
            200,
            json!({
                "linked": true,
                "cloudUserId": "user_1",
                "relayUrl": "https://relay.example.test",
                "relayIssuer": "https://relay.example.test",
                "managedTunnelActive": true,
                "publishAgentActivity": true,
                "tunnelHostname": "studio.t3coderelay.com",
                "reachability": {
                    "status": "reachable",
                    "checkedAt": "2026-11-19T00:00:00.000Z",
                    "readyConnections": 4,
                    "protocol": "quic"
                }
            }),
        ),
        // 4. POST /api/auth/pairing-token
        (
            200,
            json!({
                "id": "cred-1",
                "credential": "AUTO-Q8RZ3NWD5KTC",
                "label": "App sign-in",
                "expiresAt": "2099-01-01T00:00:00.000Z",
                "access": "auto"
            }),
        ),
    ]));

    let session = serde_json::from_value(json!({
        "authenticated": true,
        "permissions": [
            "orchestration:read",
            "orchestration:operate",
            "access:write",
            "relay:read",
            "relay:write"
        ]
    }))
    .unwrap();
    let (client, mut fixture) = ClientFixture::handle_with_http_base(session, mock.base.clone());
    let rpc_responder = runtime.spawn(async move {
        for _ in 0..4 {
            let (method, _payload, reply) = fixture.next_request().await;
            let value = match method {
                "server.getConfig" => json!({
                    "environment": {
                        "capabilities": {
                            "mcpApprovalCodes": true,
                            "mcpHostedApproval": false,
                            "peerLinks": true
                        }
                    }
                }),
                "peerLinks.list" => json!({
                    "links": [
                        {
                            "environmentId": "peer-1",
                            "label": "Build Box",
                            "access": "full-access",
                            "status": "reachable"
                        }
                    ]
                }),
                _ => json!({}),
            };
            let _ = reply.send(Ok(value));
        }
    });
    let handle = runtime.handle().clone();

    let state = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        crate::composer::init(cx);
        crate::keymap::init(cx);
        let state = cx.new(|cx| AppState::new(handle.clone(), cx));
        crate::settings::init(state.clone(), cx);
        state
    });
    // Let AppState::new's initial cloud refresh settle before installing test cloud state.
    for _ in 0..20 {
        cx.run_until_parked();
        if cx.update(|cx| !state.read(cx).cloud.busy) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    cx.update(|cx| {
        state.update(cx, |state, cx| {
            state.cloud.identity = Some("trevor@example.test".to_owned());
            state.cloud.discovery.listed = true;
            state.environments = SavedEnvironments {
                active: Some("env-local".to_owned()),
                environments: vec![SavedEnvironment {
                    id: "env-local".to_owned(),
                    environment_id: Some("canon-local".to_owned()),
                    label: "Mac Studio".to_owned(),
                    disabled: false,
                    http_base: mock.base.clone(),
                    access_token: Some("tok".to_owned()),
                    server_version: Some("1.0.0".to_owned()),
                    local: true,
                    ssh: None,
                }],
            };
            state.install_active_environment(
                client,
                ShellState::default(),
                ServerConfigState::default(),
                ConnectionStatus::Connected,
                cx,
            );
        });
    });

    let window = cx.open_window(gpui::size(px(1000.), px(760.)), |_, _| gpui::EmptyView);
    let (dialogs, palette) = cx
        .update_window(window.into(), |_, window, cx| {
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let dialogs = cx.new(|cx| McpDialogs::new(state.clone(), window, cx));
            let palette =
                cx.new(|cx| CommandPalette::new(state.clone(), composer.clone(), window, cx));
            (dialogs, palette)
        })
        .unwrap();

    // Activate "action:use-in-another-app" through the Command Palette itself.
    cx.update_window(window.into(), |_, window, cx| {
        palette.update(cx, |palette, cx| {
            palette.toggle_mode(PaletteMode::Command, window, cx);
            assert!(!is_open(cx));
            palette.activate("action:use-in-another-app", window, cx);
        });
        assert!(is_open(cx));
    })
    .unwrap();

    // Wait for initial capabilities + link-state + automatic reachability check to open the gate.
    for _ in 0..40 {
        cx.run_until_parked();
        let opened = cx.update(|cx| {
            matches!(
                dialogs.read(cx).wizard.read(cx).test_gate(cx),
                Some(McpGate::Open { .. })
            )
        });
        if opened {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    cx.update(|cx| {
        assert_eq!(
            dialogs.read(cx).wizard.read(cx).test_gate(cx),
            Some(McpGate::Open {
                mcp_url: "https://studio.t3coderelay.com/mcp".to_owned()
            })
        );
    });

    // Walk App -> Access -> Connect and mint an approval code over HTTP.
    cx.update_window(window.into(), |_, _, cx| {
        let wizard = dialogs.read(cx).wizard.clone();
        wizard.update(cx, |wizard, cx| {
            wizard.choice = Some(AppChoice::App(find_mcp_app("claude").unwrap()));
            wizard.step = WizardStep::Access;
            wizard.picked_access = Some(McpClientAccess::Auto);
            wizard.step = WizardStep::Connect;
            wizard.test_mint_code(McpClientAccess::Auto, cx);
        });
    })
    .unwrap();

    for _ in 0..40 {
        cx.run_until_parked();
        let minted = cx.update(|cx| {
            dialogs
                .read(cx)
                .wizard
                .read(cx)
                .test_minted_credential()
                .map(str::to_owned)
        });
        if minted.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let recorded = mock.recorded.lock().unwrap().clone();
    assert!(
        recorded
            .iter()
            .any(|(req, _)| req.starts_with("GET /api/connect/link-state")),
        "expected GET /api/connect/link-state, got: {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|(req, _)| req.starts_with("POST /api/connect/reachability")),
        "expected POST /api/connect/reachability, got: {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|(req, body)| req.starts_with("POST /api/auth/pairing-token")
                && body["access"] == "auto"
                && body["label"] == "App sign-in"),
        "expected POST /api/auth/pairing-token with access=auto, got: {recorded:?}"
    );
    cx.update(|cx| {
        assert_eq!(
            dialogs.read(cx).wizard.read(cx).test_minted_credential(),
            Some("AUTO-Q8RZ3NWD5KTC")
        );
    });

    // Also verify hosted-approval mode sets 2-step flow (skips Access).
    cx.update_window(window.into(), |_, _, cx| {
        let wizard = dialogs.read(cx).wizard.clone();
        wizard.update(cx, |wizard, cx| {
            wizard.set_test_state(
                Some(McpCapabilities {
                    mcp_approval_codes: true,
                    mcp_hosted_approval: true,
                    peer_links: false,
                }),
                Some(CloudLinkState {
                    linked: true,
                    managed_tunnel_active: Some(true),
                    tunnel_hostname: Some("studio.t3coderelay.com".to_owned()),
                    ..Default::default()
                }),
                ReachabilityCheckState::Done {
                    result: Some(ConnectReachability::Reachable {
                        checked_at: "2026-11-19T00:00:00.000Z".to_owned(),
                        ready_connections: Some(4),
                        protocol: Some("quic".to_owned()),
                    }),
                    since: Some("2026-11-19T00:00:00.000Z".to_owned()),
                },
                Vec::new(),
                cx,
            );
            wizard.step = WizardStep::App;
            wizard.choice = Some(AppChoice::App(find_mcp_app("poke").unwrap()));
        });
    })
    .unwrap();

    rpc_responder.abort();
}

#[gpui::test]
async fn approve_agent_url_intercepted_from_prompt_links_and_loads_oauth_details(
    cx: &mut gpui::TestAppContext,
) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mock = runtime.block_on(MockHttp::start(vec![(
        200,
        json!({
            "clientName": "Poke",
            "redirectHost": "poke.com",
            "environmentHost": "studio.t3coderelay.com",
            "csrfToken": "csrf-tok",
            "oneClickAccess": ["read-only", "approval-required", "auto-accept-edits", "auto"]
        }),
    )]));

    let session = serde_json::from_value(json!({
        "authenticated": true,
        "permissions": ["orchestration:read", "orchestration:operate", "access:write"]
    }))
    .unwrap();
    let (client, _fixture) = ClientFixture::handle_with_http_base(session, mock.base.clone());
    let handle = runtime.handle().clone();

    let state = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        let state = cx.new(|cx| AppState::new(handle.clone(), cx));
        crate::settings::init(state.clone(), cx);
        state.update(cx, |state, cx| {
            state.environments = SavedEnvironments {
                active: Some("env-local".to_owned()),
                environments: vec![SavedEnvironment {
                    id: "env-local".to_owned(),
                    environment_id: Some("canon-local".to_owned()),
                    label: "Mac Studio".to_owned(),
                    disabled: false,
                    http_base: mock.base.clone(),
                    access_token: Some("tok".to_owned()),
                    server_version: Some("1.0.0".to_owned()),
                    local: true,
                    ssh: None,
                }],
            };
            state.install_active_environment(
                client,
                ShellState::default(),
                ServerConfigState::default(),
                ConnectionStatus::Connected,
                cx,
            );
        });
        state
    });

    let window = cx.open_window(gpui::size(px(1000.), px(760.)), |_, _| gpui::EmptyView);
    let dialogs = cx
        .update_window(window.into(), |_, window, cx| {
            cx.new(|cx| McpDialogs::new(state.clone(), window, cx))
        })
        .unwrap();

    let url = "https://app.t3.codes/approve-agent?environment=canon-local&client_id=poke-1&redirect_uri=https%3A%2F%2Fpoke.com%2Fcb&response_type=code&code_challenge=abc&code_challenge_method=S256";
    // Queue through AppState's prompt_link_urls to verify McpDialogs intercepts it cleanly.
    cx.update_window(window.into(), |_, _, cx| {
        state.update(cx, |state, cx| {
            state.prompt_link_urls.push_back(url.to_owned());
            cx.emit(AppEvent::PromptLink);
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(state.read(cx).prompt_link_urls.is_empty());
        assert!(is_open(cx));
    });

    for _ in 0..40 {
        cx.run_until_parked();
        let ready = cx.update(|cx| {
            matches!(
                dialogs.read(cx).approval.read(cx).load_state,
                ApprovalLoadState::Ready(_)
            )
        });
        if ready {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    cx.update(|cx| {
        let approval = dialogs.read(cx).approval.read(cx);
        match &approval.load_state {
            ApprovalLoadState::Ready(details) => {
                assert_eq!(details.client_name, "Poke");
                assert_eq!(details.redirect_host, "poke.com");
                assert_eq!(details.csrf_token.as_deref(), Some("csrf-tok"));
            }
            other => panic!("expected Ready state, got {other:?}"),
        }
    });

    let recorded = mock.recorded.lock().unwrap().clone();
    assert!(
        recorded
            .iter()
            .any(|(req, body)| req.starts_with("POST /oauth/mcp/approval")
                && body["client_id"] == "poke-1"
                && body["response_type"] == "code"),
        "expected POST /oauth/mcp/approval, got: {recorded:?}"
    );

    // Direct open_agent_approval_url ignores non-approval URLs, and handles incomplete hosted links.
    cx.update_window(window.into(), |_, window, cx| {
        assert!(!open_agent_approval_url(
            "https://app.t3.codes/new?prompt=hi",
            window.window_handle(),
            cx
        ));
        assert!(open_agent_approval_url(
            "https://app.t3.codes/approve-agent?client_id=poke-1",
            window.window_handle(),
            cx
        ));
        assert_eq!(
            dialogs.read(cx).approval.read(cx).load_state,
            ApprovalLoadState::Incomplete
        );
        // Unsaved environment falls back to NeedsCloudSignIn when signed out of T3 Connect.
        assert!(open_agent_approval_url(
            "https://app.t3.codes/approve-agent?environment=remote-env&client_id=poke-1",
            window.window_handle(),
            cx
        ));
        assert!(matches!(
            dialogs.read(cx).approval.read(cx).load_state,
            ApprovalLoadState::NeedsCloudSignIn { .. }
        ));
    })
    .unwrap();
}
