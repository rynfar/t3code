//! How a connection reads to the user: `presentConnectionState`, `connectionStatusText` and
//! `connectionStatusTitle` in `client-runtime/src/connection/presentation.ts`, the dot colors of
//! `apps/web/src/components/ConnectionStatusDot.tsx`, `hostFreeze.ts`, and the direct-route
//! headlines, hops, facts, and steps of `client-runtime/src/connection/diagnosis.ts`.

use serde::Deserialize;

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

/// `ServerEventLoopStall` (`packages/contracts/src/server.ts` / `hostFreeze.ts`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerEventLoopStall {
    #[serde(default)]
    pub detected_at: Option<String>,
    pub duration_ms: f64,
    #[serde(default)]
    pub load_average: Option<f64>,
    #[serde(default)]
    pub cpu_count: u32,
}

impl ServerEventLoopStall {
    /// Stable deduplication key matching `HostFreezeToastCoordinator.ts` (`${environmentId}:${stall.detectedAt}`),
    /// with a structural fallback for older servers omitting `detectedAt`.
    pub fn dedup_key(&self, environment_id: &str) -> String {
        if let Some(detected_at) = self.detected_at.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            format!("{environment_id}:{detected_at}")
        } else {
            format!(
                "{environment_id}:{}:{:?}:{}",
                self.duration_ms.to_bits(),
                self.load_average.map(f64::to_bits),
                self.cpu_count
            )
        }
    }
}

/// `HostFreezeNotice` (`packages/client-runtime/src/connection/hostFreeze.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFreezeNotice {
    pub title: String,
    pub description: String,
}

/// Says why a host stopped answering, from the recent stall it reports on connect.
pub fn host_freeze_notice(host_label: &str, stall: Option<&ServerEventLoopStall>) -> Option<HostFreezeNotice> {
    let stall = stall?;
    let host = if host_label.trim().is_empty() { "This host" } else { host_label.trim() };
    let seconds = (stall.duration_ms / 1000.0).round().max(1.0) as u64;
    let description = match stall.load_average {
        Some(load) if stall.cpu_count > 0 && load > stall.cpu_count as f64 => {
            format!("Its CPU is overloaded (load {} on {} cores).", load.round() as u64, stall.cpu_count)
        }
        _ => format!("T3 Code on {host} was too busy to answer."),
    };
    Some(HostFreezeNotice {
        title: format!("{host} froze for {seconds}s"),
        description,
    })
}

impl ConnectionStatus {
    pub fn phase(&self) -> ConnectionPhase {
        match self {
            ConnectionStatus::Connecting => ConnectionPhase::Connecting,
            ConnectionStatus::Reconnecting { .. } | ConnectionStatus::AwaitingApproval { .. } => ConnectionPhase::Reconnecting,
            ConnectionStatus::Connected => ConnectionPhase::Connected,
            ConnectionStatus::Unsupported { .. } => ConnectionPhase::Unsupported,
            ConnectionStatus::NeedsPairing { .. } | ConnectionStatus::Blocked { .. } => ConnectionPhase::Error,
        }
    }

    pub fn is_awaiting_approval(&self) -> bool {
        matches!(self, ConnectionStatus::AwaitingApproval { .. })
    }

    pub fn attempt(&self) -> u32 {
        match self {
            ConnectionStatus::Reconnecting { attempt, .. } | ConnectionStatus::AwaitingApproval { attempt, .. } => *attempt,
            _ => 0,
        }
    }

    /// The last failure, if the state carries one.
    pub fn error(&self) -> Option<&str> {
        let reason = match self {
            ConnectionStatus::Reconnecting { reason, .. }
            | ConnectionStatus::AwaitingApproval { reason, .. }
            | ConnectionStatus::Unsupported { reason, .. }
            | ConnectionStatus::NeedsPairing { reason }
            | ConnectionStatus::Blocked { reason } => reason.as_str(),
            ConnectionStatus::Connecting | ConnectionStatus::Connected => "",
        };
        Some(reason.trim()).filter(|reason| !reason.is_empty())
    }

    /// `connectionStatusText`.
    pub fn text(&self) -> String {
        if self.is_awaiting_approval() {
            return self
                .error()
                .map_or_else(|| "Waiting for approval on the host".to_owned(), str::to_owned);
        }
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
        if self.is_awaiting_approval() {
            return "Waiting for approval".to_owned();
        }
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
        if self.is_awaiting_approval() {
            return (
                self.error()
                    .map_or_else(|| "Waiting for approval".to_owned(), str::to_owned),
                false,
            );
        }
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
        matches!(
            self,
            ConnectionStatus::Reconnecting { .. }
                | ConnectionStatus::AwaitingApproval { .. }
                | ConnectionStatus::Unsupported { .. }
                | ConnectionStatus::Blocked { .. }
        )
    }

    /// The tooltip: what failed, why, and what to do (`presentConnectionDiagnosis` for a direct
    /// route). Falls back to the status text when nothing failed.
    pub fn details(&self, host: &str) -> String {
        let Some(diagnosis) = diagnose(self, host) else { return self.text() };
        let reason = self.error().filter(|reason| !diagnosis.steps.iter().any(|step| step == reason)).map(|reason| format!("Reason: {reason}"));
        std::iter::once(diagnosis.headline).chain(reason).chain(diagnosis.steps).collect::<Vec<_>>().join("\n")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HopResult {
    Ok,
    Failed,
    Unchecked,
}

impl HopResult {
    pub fn mark(self) -> &'static str {
        match self {
            HopResult::Ok => "✓",
            HopResult::Failed => "✕",
            HopResult::Unchecked => "–",
        }
    }

    pub fn note(self) -> &'static str {
        match self {
            HopResult::Ok => "",
            HopResult::Failed => "Failed",
            HopResult::Unchecked => "Not checked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosisHop {
    pub label: String,
    pub result: HopResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub headline: String,
    pub hops: Vec<DiagnosisHop>,
    pub steps: Vec<String>,
    pub facts: Vec<(String, String)>,
}

/// Extracts a rejected request's trace ID when encoded in an error reason (`(Trace ID: <id>)` or
/// `Trace ID: <id>`, `pingdotgg/t3code#38041`).
pub fn extract_trace_id(reason: &str) -> Option<&str> {
    let after = reason.split("Trace ID:").nth(1)?;
    let trimmed = after.trim_start();
    let end = trimmed
        .find(|c: char| c.is_whitespace() || matches!(c, ')' | ',' | ';'))
        .unwrap_or(trimmed.len());
    let id = &trimmed[..end];
    (!id.is_empty()).then_some(id)
}

/// `headline`, `hops`, `facts`, and `steps` of `diagnosis.ts` for the server hop of a direct route.
pub fn diagnose(status: &ConnectionStatus, host: &str) -> Option<Diagnosis> {
    let host = if host.trim().is_empty() { "this environment" } else { host };
    let device_ok = DiagnosisHop { label: "This device".into(), result: HopResult::Ok };
    let device_failed = DiagnosisHop { label: "This device".into(), result: HopResult::Failed };
    let server_failed = DiagnosisHop { label: format!("T3 Code on {host}"), result: HopResult::Failed };
    let server_unchecked = DiagnosisHop { label: format!("T3 Code on {host}"), result: HopResult::Unchecked };

    let mut facts = Vec::new();
    let attempt = status.attempt();
    if attempt > 0 {
        facts.push(("Attempt".to_owned(), attempt.to_string()));
    }
    if let Some(reason) = status.error() {
        if let Some(trace_id) = extract_trace_id(reason) {
            facts.push(("Trace ID".to_owned(), trace_id.to_owned()));
        }
        facts.push(("Detail".to_owned(), reason.to_owned()));
    }

    let (headline, hops, steps) = match status {
        ConnectionStatus::Connecting | ConnectionStatus::Connected => return None,
        ConnectionStatus::Reconnecting { reason, .. } if reason.trim().is_empty() => return None,
        ConnectionStatus::AwaitingApproval { .. } => (
            format!("Waiting for approval on {host}"),
            vec![device_ok, server_failed],
            vec![format!("Approve this device in Settings \u{2192} Sharing & access on {host}.")],
        ),
        ConnectionStatus::Reconnecting { .. } => (
            format!("T3 Code on {host} did not answer"),
            vec![device_ok, server_failed],
            vec![format!("Make sure T3 Code is running on {host}.")],
        ),
        ConnectionStatus::Unsupported { server_outdated: true, .. } => (
            format!("{host} runs an older T3 Code"),
            vec![device_ok, server_failed],
            vec![format!("Update T3 Code on {host}.")],
        ),
        ConnectionStatus::Unsupported { reason, .. } => (
            "Client not supported".to_owned(),
            vec![device_ok, server_failed],
            vec![reason.clone()],
        ),
        ConnectionStatus::NeedsPairing { .. } => (
            format!("T3 Code on {host} did not accept this device"),
            vec![device_ok, server_failed],
            vec![format!(
                "This device's sign-in to {host} expired or was revoked. Pair it again with a new pairing link from {host}."
            )],
        ),
        // The device hop: this device could not ready the route (an SSH tunnel) to the server.
        ConnectionStatus::Blocked { reason } => (
            "This device could not prepare the connection".to_owned(),
            vec![device_failed, server_unchecked],
            vec![reason.clone()],
        ),
    };
    Some(Diagnosis { headline, hops, steps, facts })
}

/// Plain-text report copied by "Copy diagnostics" (`connectionDiagnosisText` in `diagnosis.ts`).
pub fn connection_diagnosis_text(host: &str, diagnosis: &Diagnosis) -> String {
    let mut lines = vec![format!("{host}: {}", diagnosis.headline)];
    for hop in &diagnosis.hops {
        let suffix = if hop.result.note().is_empty() { String::new() } else { format!(" ({})", hop.result.note()) };
        lines.push(format!("  {} {}{suffix}", hop.result.mark(), hop.label));
    }
    if !diagnosis.facts.is_empty() {
        for (key, value) in &diagnosis.facts {
            lines.push(format!("  {key}: {value}"));
        }
    }
    for step in &diagnosis.steps {
        lines.push(format!("  - {step}"));
    }
    lines.join("\n")
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

        let approval = ConnectionStatus::AwaitingApproval { reason: "This device is waiting for approval on the host.".into(), attempt: 1 };
        assert_eq!(approval.title(), "Waiting for approval");
        assert_eq!(approval.dot(), DotTone::Warning);
        assert_eq!(approval.summary(), ("This device is waiting for approval on the host.".to_owned(), false));
        assert!(approval.can_retry());
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

        let diag = diagnose(&blocked, "devbox").expect("diagnosis");
        assert_eq!(diag.hops[0].result, HopResult::Failed);
        assert_eq!(diag.hops[1].result, HopResult::Unchecked);
        let copied = connection_diagnosis_text("devbox", &diag);
        assert!(copied.contains("devbox: This device could not prepare the connection"));
        assert!(copied.contains("✕ This device (Failed)"));

        // `pingdotgg/t3code#38041`: expired-session diagnosis asks to pair again and names the rejected request's trace ID.
        let expired = ConnectionStatus::NeedsPairing {
            reason: "This device is no longer authorized. Pair again. (Trace ID: trace-1)".into(),
        };
        let expired_diag = diagnose(&expired, "Studio").expect("expired diagnosis");
        let joined_steps = expired_diag.steps.join(" ");
        assert!(!joined_steps.contains("Make sure T3 Code is running"));
        assert!(joined_steps.contains("Pair it again with a new pairing link from Studio."));
        assert!(expired_diag.facts.iter().any(|(k, v)| k == "Trace ID" && v == "trace-1"));
        assert!(connection_diagnosis_text("Studio", &expired_diag).contains("Trace ID: trace-1"));
    }

    #[test]
    fn host_freeze_notice_explains_cpu_overload_or_busy_host() {
        assert_eq!(host_freeze_notice("Studio", None), None);
        let stall = ServerEventLoopStall {
            detected_at: Some("2026-11-19T01:02:03.000Z".into()),
            duration_ms: 12_400.0,
            load_average: Some(18.2),
            cpu_count: 8,
        };
        assert_eq!(stall.dedup_key("env-1"), "env-1:2026-11-19T01:02:03.000Z");
        let overloaded = host_freeze_notice("Studio", Some(&stall)).expect("notice");
        assert_eq!(overloaded.title, "Studio froze for 12s");
        assert_eq!(overloaded.description, "Its CPU is overloaded (load 18 on 8 cores).");

        let busy = host_freeze_notice(
            "Studio",
            Some(&ServerEventLoopStall { detected_at: None, duration_ms: 4_800.0, load_average: Some(2.0), cpu_count: 8 }),
        )
        .expect("notice");
        assert_eq!(busy.title, "Studio froze for 5s");
        assert_eq!(busy.description, "T3 Code on Studio was too busy to answer.");
    }
}
