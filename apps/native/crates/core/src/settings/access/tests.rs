use serde_json::json;

use super::*;
use crate::sidebar::time::format_iso;

const NOW: i64 = 1_793_000_000_000;

fn link(id: &str, created: i64, expires: i64) -> Value {
    json!({ "id": id, "scopes": ["orchestration:read"], "subject": "one-time-token", "createdAt": format_iso(created), "expiresAt": format_iso(expires) })
}

fn client(id: &str, current: bool, connected: bool, issued: i64) -> Value {
    json!({
        "sessionId": id, "subject": "browser", "scopes": ["orchestration:read"], "permissions": ["orchestration:read", "terminal:read"],
        "method": "bearer-access-token", "client": { "deviceType": "desktop", "os": "macOS", "browser": "Safari", "ipAddress": "10.0.0.2" },
        "issuedAt": format_iso(issued), "expiresAt": format_iso(issued + 86_400_000), "lastConnectedAt": null, "connected": connected, "current": current,
    })
}

#[test]
fn the_access_stream_snapshots_upserts_and_removes() {
    let mut state = AccessState::default();
    assert!(state.can_revoke_others(), "revoke others is on until the list loads");
    state.apply(&json!({ "version": 1, "revision": 1, "type": "snapshot", "payload": {
        "pairingLinks": [link("old", NOW - 60_000, NOW + 240_000), link("expired", NOW - 600_000, NOW - 1), { "id": "broken" }],
        "clientSessions": [client("me", true, true, NOW - 1_000), client("phone", false, false, NOW - 5_000), client("laptop", false, true, NOW - 9_000)],
    } }));
    assert!(state.loaded);
    assert_eq!(state.pairing_links.len(), 2, "an undecodable link is skipped alone");
    assert_eq!(state.visible_links(NOW).iter().map(|link| link.id.as_str()).collect::<Vec<_>>(), ["old"]);
    assert_eq!(state.sorted_clients().iter().map(|client| client.session_id.as_str()).collect::<Vec<_>>(), ["me", "laptop", "phone"]);
    assert_eq!(state.summary(NOW), "3 clients \u{00b7} 1 pairing link");

    state.apply(&json!({ "type": "pairingLinkUpserted", "payload": link("new", NOW, NOW + 300_000) }));
    state.apply(&json!({ "type": "clientRemoved", "payload": { "sessionId": "phone" } }));
    state.apply(&json!({ "type": "clientRemoved", "payload": { "sessionId": "laptop" } }));
    state.apply(&json!({ "type": "pairingLinkRemoved", "payload": { "id": "old" } }));
    state.apply(&json!({ "type": "somethingNew", "payload": {} }));
    assert_eq!(state.visible_links(NOW).iter().map(|link| link.id.as_str()).collect::<Vec<_>>(), ["new"]);
    assert_eq!(state.summary(NOW), "1 client \u{00b7} 1 pairing link");
    assert!(!state.can_revoke_others(), "only this device is left");

    let me = &state.clients[0];
    assert_eq!(me.title(), "macOS \u{00b7} Safari");
    assert_eq!(me.subline(), "Desktop \u{00b7} macOS \u{00b7} Safari \u{00b7} 10.0.0.2");
    assert_eq!(scope_count(shown_scopes(&me.permissions, &me.scopes)), "2 scopes");
}

#[test]
fn scopes_presets_and_coupling_follow_the_dialog() {
    let mut selected = standard_scopes();
    assert_eq!(selected.len(), 16);
    toggle_scope(&mut selected, "terminal:read", false);
    assert!(!selected.iter().any(|scope| scope.starts_with("terminal:")), "turning off viewing terminals turns off using them");
    toggle_scope(&mut selected, "terminal:operate", true);
    assert!(selected.iter().any(|scope| scope == "terminal:read"), "using terminals needs viewing them");
    assert_eq!(selected, standard_scopes(), "order follows the checkboxes");

    let grants = |scope: &str| scope != "filesystem:read";
    assert_eq!(preset(&READ_ONLY_SCOPES, Some(&grants)), ["orchestration:read", "diagnostics:read", "terminal:read"]);
    assert_eq!(preset(&READ_ONLY_SCOPES, None).len(), 4);
    assert_eq!(scopes_notice(&[]), Some(("Select at least one permission.", true)));
    assert_eq!(scopes_notice(&standard_scopes()).map(|(_, error)| error), Some(false));
    assert_eq!(scopes_notice(&["orchestration:read".to_owned()]), None);

    assert_eq!(pairing_token_body("  ", &["orchestration:read".to_owned()]), json!({ "scopes": ["orchestration:read"] }));
    assert_eq!(pairing_token_body(" iPad ", &[]), json!({ "scopes": [], "label": "iPad" }));
}

#[test]
fn expiry_labels_and_pairing_urls_match_the_web() {
    let at = |seconds: i64| format_iso(NOW + seconds * 1000);
    assert_eq!(expires_label(&at(0), NOW), "Expired");
    assert_eq!(expires_label(&at(3), NOW), "Expires in a moment");
    assert_eq!(expires_label(&at(42), NOW), "Expires in 42s");
    assert_eq!(expires_label(&at(240), NOW), "Expires in 4m");
    assert_eq!(expires_label(&at(252), NOW), "Expires in 4m 12s");
    assert_eq!(expires_label(&at(3600 + 5), NOW), "Expires in 1h 5s");
    assert_eq!(expires_label(&at(86_400 * 2), NOW), "Expires in 2d");
    assert_eq!(expires_label(&at(86_400 + 61), NOW), "Expires in 1d 1m 1s");
    assert_eq!(revoked_others_label(1), "Revoked 1 other client");
    assert_eq!(revoked_others_label(3), "Revoked 3 clients");

    assert_eq!(pairing_url("http://mac.tailnet.ts.net:3773/", "ABC234").as_deref(), Some("http://mac.tailnet.ts.net:3773/pair#token=ABC234"));
    assert_eq!(pairing_url("http://127.0.0.1:3773", "ABC234"), None);
    assert_eq!(pairing_url("http://[::1]:3773", "ABC234"), None);
    assert_eq!(pairing_url("http://localhost:3773", "ABC234"), None);
}
