//! `contracts/src/guestShare.ts` and `client-runtime/src/operations/commands.ts`.
use crate::commands::{DispatchMode, send_message};
use crate::server_config::ServerConfig;
use crate::sidebar::time::{Ms, parse_iso};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CREATE: &str = "guestShare.create";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestShare {
    pub id: String,
    pub host: String,
    pub created_at: String,
    pub expires_at: String,
    pub guest_name: Option<String>,
    #[serde(default)]
    pub messages: Vec<GuestMessage>,
}
impl GuestShare {
    pub fn pending_messages(&self) -> impl Iterator<Item = &GuestMessage> {
        self.messages
            .iter()
            .filter(|message| message.status == GuestMessageStatus::Pending)
    }

    pub fn status(&self, now: Ms) -> String {
        if parse_iso(&self.expires_at).is_some_and(|expiry| expiry / 60_000 <= now / 60_000) {
            "Guest link expired".into()
        } else {
            self.guest_name.as_ref().map_or_else(
                || "Guest link active".into(),
                |name| format!("{name} is a guest"),
            )
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestMessage {
    pub id: String,
    pub text: String,
    pub status: GuestMessageStatus,
    pub submitted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<GuestAttachment>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestMessageStatus {
    Pending,
    Sent,
    Dismissed,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestAttachment {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct CreateResult {
    pub url: String,
    pub share: GuestShare,
}

pub fn available(config: Option<&ServerConfig>, access_write: bool) -> bool {
    access_write
        && config.is_some_and(|config| {
            config.settings.guest_sharing_enabled == Some(true)
                && config
                    .managed_policy
                    .as_ref()
                    .and_then(|policy| policy.get("limits"))
                    .and_then(|limits| limits.get("connect"))
                    .and_then(Value::as_bool)
                    != Some(false)
        })
}
pub fn failure_message(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "disabled" => "Guest links are off. Turn them on in Settings > Labs.",
        "connect_unavailable" => "Guest links need a running T3 Connect tunnel.",
        "thread_unavailable" => "This thread cannot be shared.",
        "read_failed" => "Failed to read the shared thread.",
        "changed" => "The guest link changed while it was being created.",
        _ => return None,
    })
}

pub fn create(thread_id: &str) -> Value {
    json!({"threadId":thread_id})
}
pub fn close(command_id: &str, thread_id: &str) -> Value {
    json!({"type":"thread.guest-share.close", "commandId":command_id, "threadId":thread_id})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMessageDecision {
    Send,
    Dismiss,
}

pub fn resolve_guest_message(
    command_id: &str,
    thread_id: &str,
    guest_message_id: &str,
    decision: GuestMessageDecision,
) -> Value {
    match decision {
        GuestMessageDecision::Dismiss => json!({
            "type": "thread.guest-message.dismiss",
            "commandId": command_id,
            "threadId": thread_id,
            "guestMessageId": guest_message_id,
        }),
        GuestMessageDecision::Send => {
            let mut command = send_message(command_id, thread_id, "", &DispatchMode::QueueAfterActive);
            command["guestMessageId"] = json!(guest_message_id);
            command
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_links_require_their_own_grant_beta_and_connect_policy() {
        for (access, enabled, connect, expected) in [
            (true, Some(true), None, true),
            (true, Some(true), Some(true), true),
            (false, Some(true), None, false),
            (true, Some(false), None, false),
            (true, None, None, false),
            (true, Some(true), Some(false), false),
        ] {
            let config: ServerConfig = serde_json::from_value(json!({"environment":{"environmentId":"e"},"cwd":"/fixture", "settings":{"guestSharingEnabled":enabled},"managedPolicy":{"limits":{"connect":connect}}})).unwrap();
            assert_eq!(available(Some(&config), access), expected);
        }
        assert!(!available(None, true));
        for value in [Value::Null, json!("true"), json!({})] {
            let config: ServerConfig = serde_json::from_value(json!({"environment":{"environmentId":"e"}, "cwd":"/fixture", "environmentCapabilities":{}, "settings":{"guestSharingEnabled":value}})).unwrap();
            assert!(!available(Some(&config), true));
        }
    }
    #[test]
    fn share_status_and_existing_contract_payloads() {
        let mut share: GuestShare = serde_json::from_value(json!({"id":"share", "host":"fixture.invalid", "createdAt":"2026-01-01T00:00:00Z", "expiresAt":"2026-01-02T00:00:00Z", "guestName":null, "messages":[], "future":true})).unwrap();
        assert_eq!(share.status(0), "Guest link active");
        share.guest_name = Some("Ada".into());
        assert_eq!(share.status(0), "Ada is a guest");
        assert_eq!(
            share.status(parse_iso(&share.expires_at).unwrap()),
            "Guest link expired"
        );
        assert_eq!(create("t"), json!({"threadId":"t"}));
        assert_eq!(
            close("c", "t"),
            json!({"type":"thread.guest-share.close", "commandId":"c", "threadId":"t"})
        );
    }

    #[test]
    fn pending_messages_preserve_attachments_and_skip_other_statuses() {
        let submitted_at = "2026-01-01T01:00:00Z";
        let attachment = json!({
            "type": "file", "id": "file-1", "name": "readme.md", "mimeType": "text/markdown",
            "sizeBytes": 42, "source": {"_tag": "pasted-text"}
        });
        let share: GuestShare = serde_json::from_value(json!({
            "id": "share", "host": "fixture.invalid", "guestName": "Ada",
            "createdAt": "2026-01-01T00:00:00Z", "expiresAt": "2026-01-02T00:00:00Z",
            "messages": [
                {"id":"g1", "text":"", "status":"pending", "submittedAt":submitted_at, "attachments":[attachment.clone()]},
                {"id":"g2", "text":"Sent", "status":"sent", "submittedAt":submitted_at},
                {"id":"g3", "text":"Dismissed", "status":"dismissed", "submittedAt":submitted_at},
                {"id":"g4", "text":"Future", "status":"future", "submittedAt":submitted_at}
            ]
        })).unwrap();
        let pending: Vec<_> = share.pending_messages().collect();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "g1");
        assert_eq!(pending[0].submitted_at, submitted_at);
        assert_eq!(serde_json::to_value(&pending[0].attachments).unwrap(), json!([attachment]));
        assert_eq!(share.messages[1].attachments, None);
        assert_eq!(share.messages[3].status, GuestMessageStatus::Unknown);
    }

    #[test]
    fn resolve_guest_message_matches_both_dispatch_commands() {
        assert_eq!(
            resolve_guest_message("c", "t", "g", GuestMessageDecision::Dismiss),
            json!({
                "type": "thread.guest-message.dismiss", "commandId": "c", "threadId": "t",
                "guestMessageId": "g"
            }),
        );
        assert_eq!(
            resolve_guest_message("c", "t", "g", GuestMessageDecision::Send),
            json!({
                "type": "message.dispatch", "commandId": "c", "threadId": "t", "messageId": "c",
                "guestMessageId": "g", "text": "", "attachments": [],
                "dispatchMode": {"type": "queue_after_active"},
                "createdBy": "user", "creationSource": "web"
            }),
        );
    }
}
