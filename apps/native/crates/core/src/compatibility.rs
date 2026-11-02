//! Orchestration protocol negotiation, as `client-runtime/src/connection/compatibility.ts` decides
//! it from the server's `/.well-known/t3/environment` descriptor.

use crate::server_config::EnvironmentDescriptor;

/// `ORCHESTRATION_PROTOCOL_VERSION` in `packages/contracts/src/environment.ts`.
pub const ORCHESTRATION_PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolMismatch {
    /// The server speaks an older protocol: updating it fixes the connection.
    ServerOutdated,
    /// The server speaks a newer protocol than this app.
    ClientOutdated,
}

impl ProtocolMismatch {
    /// The `ConnectionBlockedError` detail the web shows.
    pub fn message(self, label: &str) -> String {
        let label = if label.trim().is_empty() { "this environment" } else { label };
        match self {
            ProtocolMismatch::ServerOutdated => format!("This client requires a newer server. Update T3 Code on {label} to connect."),
            ProtocolMismatch::ClientOutdated => {
                format!("This client is not supported by this server. Update your app or use a compatible release to connect to {label}.")
            }
        }
    }
}

/// `orchestrationProtocolCompatibilityError`. Servers shipped before negotiation speak protocol 1.
pub fn protocol_mismatch(descriptor: &EnvironmentDescriptor) -> Option<ProtocolMismatch> {
    mismatch_with(descriptor.orchestration_protocol_version.unwrap_or(1))
}

/// How a server speaking `server` relates to this client's protocol.
pub fn mismatch_with(server: u32) -> Option<ProtocolMismatch> {
    match server.cmp(&ORCHESTRATION_PROTOCOL_VERSION) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Less => Some(ProtocolMismatch::ServerOutdated),
        std::cmp::Ordering::Greater => Some(ProtocolMismatch::ClientOutdated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(version: Option<u32>) -> EnvironmentDescriptor {
        EnvironmentDescriptor { label: "Studio".into(), orchestration_protocol_version: version, ..Default::default() }
    }

    #[test]
    fn only_the_same_protocol_is_compatible_and_a_missing_version_is_protocol_one() {
        assert_eq!(protocol_mismatch(&descriptor(Some(ORCHESTRATION_PROTOCOL_VERSION))), None);
        assert_eq!(protocol_mismatch(&descriptor(None)), Some(ProtocolMismatch::ServerOutdated));
        assert_eq!(protocol_mismatch(&descriptor(Some(ORCHESTRATION_PROTOCOL_VERSION + 1))), Some(ProtocolMismatch::ClientOutdated));
        assert_eq!(ProtocolMismatch::ServerOutdated.message("Studio"), "This client requires a newer server. Update T3 Code on Studio to connect.");
    }
}
