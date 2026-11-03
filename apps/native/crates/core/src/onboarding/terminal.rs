//! `AgentInstallTerminal` in `web/src/components/onboarding/WelcomeWizard.tsx`.
//! Operations are queued by the client; generations invalidate results immediately, before
//! cleanup reaches that queue. Permission is read again at each operation, including cleanup.

use serde::Deserialize;
use serde_json::{Value, json};

pub const THREAD_ID: &str = "onboarding-agent-setup";
#[derive(Debug, Clone)]
pub struct Session {
    pub driver: String,
    pub command: String,
    pub cwd: String,
    pub provider_instance_id: String,
}

pub const PERMISSION_MESSAGE: &str = "This connection cannot control terminals.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SetupState {
    Preparing,
    Ready,
    OpenFailed,
    WriteFailed,
}

impl SetupState {
    pub fn terminal_ready(self) -> bool {
        matches!(self, Self::Ready | Self::WriteFailed)
    }

    pub fn message(self, command: &str, can_operate: bool) -> String {
        if !can_operate {
            return PERMISSION_MESSAGE.to_owned();
        }
        match self {
            Self::Preparing => "Preparing command...".to_owned(),
            Self::Ready => "Review the command, then press Enter to run it.".to_owned(),
            Self::OpenFailed => "Could not open the setup terminal.".to_owned(),
            Self::WriteFailed => format!("Run {command} in this terminal."),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Setup {
    generation: u64,
    active: Option<u64>,
    pub state: SetupState,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            generation: 0,
            active: None,
            state: SetupState::Preparing,
        }
    }
}

impl Setup {
    pub fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.active = Some(self.generation);
        self.state = SetupState::Preparing;
        self.generation
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.active == Some(generation)
    }

    pub fn cancel(&mut self) {
        self.active = None;
    }

    pub fn can_open(&mut self, generation: u64, can_operate: bool) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        if !can_operate {
            self.state = SetupState::OpenFailed;
        }
        can_operate
    }

    /// Returns whether this open may pre-type the command.
    pub fn opened(&mut self, generation: u64, success: bool, can_operate: bool) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        self.state = if !success {
            SetupState::OpenFailed
        } else if !can_operate {
            SetupState::WriteFailed
        } else {
            SetupState::Preparing
        };
        success && can_operate
    }

    pub fn wrote(&mut self, generation: u64, success: bool) {
        if !self.is_current(generation) {
            return;
        }
        self.state = if success {
            SetupState::Ready
        } else {
            SetupState::WriteFailed
        };
    }
}

pub fn terminal_id(driver: &str, uuid: &str) -> String {
    format!("onboarding-{driver}-{uuid}")
}

pub fn open_input(terminal_id: &str, cwd: &str, provider_instance_id: &str) -> Value {
    json!({"threadId": THREAD_ID, "terminalId": terminal_id, "cwd": cwd, "providerInstanceId": provider_instance_id})
}

pub fn write_input(terminal_id: &str, command: &str) -> Value {
    crate::terminal::write_input(THREAD_ID, terminal_id, command)
}

pub fn close_input(terminal_id: &str, can_operate: bool) -> Option<Value> {
    can_operate.then(|| crate::terminal::close_input(THREAD_ID, terminal_id, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_open_and_write_cannot_touch_a_replacement() {
        let mut setup = Setup::default();
        let old = setup.begin();
        setup.cancel();
        let replacement = setup.begin();
        assert!(!setup.opened(old, true, true));
        setup.wrote(old, false);
        assert_eq!(setup.state, SetupState::Preparing);
        assert!(setup.opened(replacement, true, true));
        setup.wrote(replacement, true);
        assert_eq!(setup.state, SetupState::Ready);
        assert!(!setup.opened(old, false, true));
        assert_eq!(setup.state, SetupState::Ready);
        setup.cancel();
        setup.wrote(replacement, false);
        assert_eq!(setup.state, SetupState::Ready);
        assert!(!setup.can_open(replacement, true));
    }

    #[test]
    fn failures_retry_and_permission_loss_follow_web_states() {
        let mut setup = Setup::default();
        let denied = setup.begin();
        assert!(!setup.can_open(denied, false));
        assert_eq!(setup.state, SetupState::OpenFailed);
        let failed = setup.begin();
        assert!(setup.can_open(failed, true));
        assert!(!setup.opened(failed, false, true));
        assert_eq!(setup.state, SetupState::OpenFailed);
        let revoked = setup.begin();
        assert!(!setup.opened(revoked, true, false));
        assert_eq!(setup.state, SetupState::WriteFailed);
        assert!(setup.state.terminal_ready());
        assert_eq!(
            setup.state.message("codex login", false),
            PERMISSION_MESSAGE
        );
        assert_eq!(close_input("setup", false), None);
        let retry = setup.begin();
        assert_eq!(setup.state, SetupState::Preparing);
        assert!(!setup.state.terminal_ready());
        assert!(setup.opened(retry, true, true));
        setup.wrote(retry, false);
        assert_eq!(setup.state, SetupState::WriteFailed);
        assert_eq!(
            setup.state.message("codex login", true),
            "Run codex login in this terminal."
        );
    }

    #[test]
    fn setup_payloads_target_the_instance_and_never_execute_the_command() {
        let id = terminal_id("codex", "uuid");
        assert_eq!(id, "onboarding-codex-uuid");
        assert_eq!(
            open_input(&id, "/workspace", "codex-work"),
            json!({
                "threadId":"onboarding-agent-setup", "terminalId":id, "cwd":"/workspace", "providerInstanceId":"codex-work"
            })
        );
        let command = "curl -fsSL https://chatgpt.com/codex/install.sh | sh";
        assert_eq!(write_input(&id, command)["data"], command);
        assert!(
            !write_input(&id, command)["data"]
                .as_str()
                .unwrap()
                .contains(['\n', '\r'])
        );
        assert_eq!(
            close_input(&id, true).unwrap(),
            json!({"threadId": THREAD_ID, "terminalId":id, "deleteHistory":true})
        );
    }
}
