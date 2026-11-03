//! How a connection reads to the user: `presentConnectionState`, `connectionStatusText` and
//! `connectionStatusTitle` in `client-runtime/src/connection/presentation.ts`, the dot colors of
//! `apps/web/src/components/ConnectionStatusDot.tsx`, and the direct-route headlines and steps of
//! `client-runtime/src/connection/diagnosis.ts`. Relay and tunnel causes don't apply here.

use crate::supervisor::ConnectionStatus;

/// `EnvironmentConnectionPhase` for the states a direct connection reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionPhase {
    Connecting,
    Reconnecting,
    Connected,
    Unsupported,
    Error,
}

/// `connectionPhaseDotClassName`: `success`, `warning`, or `destructive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DotTone {
    Success,
    Warning,
    Destructive,
}

impl ConnectionStatus {
    pub fn phase(&self) -> ConnectionPhase {
        match self {
            ConnectionStatus::Connecting => ConnectionPhase::Connecting,
            ConnectionStatus::Reconnecting { .. } => ConnectionPhase::Reconnecting,
            ConnectionStatus::Connected => ConnectionPhase::Connected,
            ConnectionStatus::Unsupported { .. } => ConnectionPhase::Unsupported,
            ConnectionStatus::NeedsPairing { .. } | ConnectionStatus::Blocked { .. } => ConnectionPhase::Error,
        }
    }

    /// The last failure, if the state carries one.
    pub fn error(&self) -> Option<&str> {
        let reason = match self {
            ConnectionStatus::Reconnecting { reason, .. }
            | ConnectionStatus::Unsupported { reason, .. }
            | ConnectionStatus::NeedsPairing { reason }
            | ConnectionStatus::Blocked { reason } => reason.as_str(),
            ConnectionStatus::Connecting | ConnectionStatus::Connected => "",
        };
        Some(reason.trim()).filter(|reason| !reason.is_empty())
    }

    /// `connectionStatusText`.
    pub fn text(&self) -> String {
        match (self.phase(), self.error()) {
            (ConnectionPhase::Connecting, _) => "Connecting...".to_owned(),
            (ConnectionPhase::Reconnecting, Some(error)) => format!("Failed to connect. Reconnecting... Reason: {error}"),
            (ConnectionPhase::Reconnecting, None) => "Reconnecting...".to_owned(),
            (ConnectionPhase::Connected, _) => "Connected".to_owned(),
            (ConnectionPhase::Unsupported, _) => "Client not supported".to_owned(),
            (ConnectionPhase::Error, Some(error)) => format!("Connection failed. Reason: {error}"),
            (ConnectionPhase::Error, None) => "Connection failed".to_owned(),
        }
    }

    /// `connectionStatusTitle`: the text without its reason.
    pub fn title(&self) -> String {
        match self.phase() {
            ConnectionPhase::Reconnecting if self.error().is_some() => "Failed to connect. Reconnecting...".to_owned(),
            ConnectionPhase::Reconnecting => "Reconnecting...".to_owned(),
            ConnectionPhase::Error => "Connection failed".to_owned(),
            _ => self.text(),
        }
    }

    pub fn dot(&self) -> DotTone {
        match self.phase() {
            ConnectionPhase::Connected => DotTone::Success,
            ConnectionPhase::Connecting | ConnectionPhase::Reconnecting => DotTone::Warning,
            ConnectionPhase::Unsupported | ConnectionPhase::Error => DotTone::Destructive,
        }
    }

    /// A saved environment row's status and whether it reads as an error
    /// (`savedEnvironmentStatus` in `SavedEnvironmentRows.tsx`).
    pub fn summary(&self) -> (String, bool) {
        match (self.phase(), self.error()) {
            (ConnectionPhase::Connected, _) => ("Connected".to_owned(), false),
            (ConnectionPhase::Connecting, _) => ("Connecting".to_owned(), false),
            (ConnectionPhase::Reconnecting, Some(error)) => (format!("Reconnecting: {error}"), true),
            (ConnectionPhase::Reconnecting, None) => ("Reconnecting".to_owned(), true),
            (ConnectionPhase::Unsupported, _) => ("Client not supported".to_owned(), false),
            (ConnectionPhase::Error, Some(error)) => (format!("Connection failed: {error}"), true),
            (ConnectionPhase::Error, None) => ("Connection failed".to_owned(), true),
        }
    }

    /// Whether "Retry" can do anything: skip a reconnect wait, or check a blocked server again.
    pub fn can_retry(&self) -> bool {
        matches!(self, ConnectionStatus::Reconnecting { .. } | ConnectionStatus::Unsupported { .. } | ConnectionStatus::Blocked { .. })
    }

    /// The tooltip: what failed, why, and what to do (`presentConnectionDiagnosis` for a direct
    /// route). Falls back to the status text when nothing failed.
    pub fn details(&self, host: &str) -> String {
        let Some(diagnosis) = diagnose(self, host) else { return self.text() };
        let reason = self.error().filter(|reason| !diagnosis.steps.iter().any(|step| step == reason)).map(|reason| format!("Reason: {reason}"));
        std::iter::once(diagnosis.headline).chain(reason).chain(diagnosis.steps).collect::<Vec<_>>().join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub headline: String,
    pub steps: Vec<String>,
}

/// `headline` and `steps` of `diagnosis.ts` for the server hop of a direct route.
pub fn diagnose(status: &ConnectionStatus, host: &str) -> Option<Diagnosis> {
    let host = if host.trim().is_empty() { "this environment" } else { host };
    let (headline, steps) = match status {
        ConnectionStatus::Connecting | ConnectionStatus::Connected => return None,
        ConnectionStatus::Reconnecting { reason, .. } if reason.trim().is_empty() => return None,
        ConnectionStatus::Reconnecting { .. } => (format!("T3 Code on {host} did not answer"), vec![format!("Make sure T3 Code is running on {host}.")]),
        ConnectionStatus::Unsupported { server_outdated: true, .. } => (format!("{host} runs an older T3 Code"), vec![format!("Update T3 Code on {host}.")]),
        ConnectionStatus::Unsupported { reason, .. } => ("Client not supported".to_owned(), vec![reason.clone()]),
        ConnectionStatus::NeedsPairing { .. } => (format!("T3 Code on {host} did not accept this device"), vec!["Pair again to reconnect.".to_owned()]),
        // The device hop: this device could not ready the route (an SSH tunnel) to the server.
        ConnectionStatus::Blocked { reason } => ("This device could not prepare the connection".to_owned(), vec![reason.clone()]),
    };
    Some(Diagnosis { headline, steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reconnecting(reason: &str) -> ConnectionStatus {
        ConnectionStatus::Reconnecting { reason: reason.into(), attempt: 2 }
    }

    #[test]
    fn status_text_and_title_follow_the_web() {
        assert_eq!(ConnectionStatus::Connecting.text(), "Connecting...");
        assert_eq!(reconnecting("").text(), "Reconnecting...");
        assert_eq!(reconnecting("refused").text(), "Failed to connect. Reconnecting... Reason: refused");
        assert_eq!(reconnecting("refused").title(), "Failed to connect. Reconnecting...");
        assert_eq!(ConnectionStatus::NeedsPairing { reason: "revoked".into() }.text(), "Connection failed. Reason: revoked");
        let blocked = ConnectionStatus::Unsupported { reason: "Update T3 Code on Studio to connect.".into(), server_outdated: true };
        assert_eq!((blocked.title(), blocked.dot(), blocked.can_retry()), ("Client not supported".to_owned(), DotTone::Destructive, true));
        assert_eq!((ConnectionStatus::Connected.dot(), ConnectionStatus::Connected.can_retry()), (DotTone::Success, false));
        assert_eq!(reconnecting("").dot(), DotTone::Warning);
        assert_eq!(reconnecting("refused").summary(), ("Reconnecting: refused".to_owned(), true));
        assert_eq!(ConnectionStatus::Connecting.summary(), ("Connecting".to_owned(), false));
    }

    #[test]
    fn details_name_the_failing_part_the_reason_and_the_fix() {
        assert_eq!(
            reconnecting("Could not reach http://127.0.0.1:3773").details("MacBook Pro"),
            "T3 Code on MacBook Pro did not answer\nReason: Could not reach http://127.0.0.1:3773\nMake sure T3 Code is running on MacBook Pro."
        );
        let outdated = ConnectionStatus::Unsupported { reason: "This client requires a newer server.".into(), server_outdated: true };
        assert_eq!(outdated.details("Studio"), "Studio runs an older T3 Code\nReason: This client requires a newer server.\nUpdate T3 Code on Studio.");
        let newer = ConnectionStatus::Unsupported { reason: "Update your app.".into(), server_outdated: false };
        assert_eq!(newer.details("Studio"), "Client not supported\nUpdate your app.");
        assert_eq!(ConnectionStatus::Connecting.details("Studio"), "Connecting...");
        let blocked = ConnectionStatus::Blocked { reason: "SSH authentication cancelled for devbox.".into() };
        assert_eq!(blocked.details("devbox"), "This device could not prepare the connection\nSSH authentication cancelled for devbox.");
        assert_eq!((blocked.title(), blocked.can_retry(), blocked.dot()), ("Connection failed".to_owned(), true, DotTone::Destructive));
    }
}
