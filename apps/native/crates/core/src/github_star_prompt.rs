//! `web/src/components/GitHubStarPrompt.tsx`: session gate, focused idle, and toast actions.
use crate::auth::{AuthSessionState, SOURCE_CONTROL_WRITE_SCOPE};
use serde_json::{Value, json};

pub const IDLE_MS: u64 = 30_000;
pub const URL: &str = "https://github.com/pingdotgg/t3code";
pub const CHECK: &str = "sourceControl.starred";
pub const STAR: &str = "sourceControl.star";
pub fn input() -> Value {
    json!({"provider":"github", "host":"github.com", "repository":"pingdotgg/t3code"})
}
pub fn can_star(session: Option<&AuthSessionState>) -> bool {
    session.is_some_and(|s| s.grants(SOURCE_CONTROL_WRITE_SCOPE))
}

/// The send plan may contain only approval feedback, which the web returns from before counting.
pub fn plan_sends_prompt<'a>(commands: impl IntoIterator<Item = &'a Value>) -> bool {
    commands
        .into_iter()
        .any(|command| command.get("type").and_then(Value::as_str) == Some("message.dispatch"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Destination<'a> {
    pub id: &'a str,
    pub endpoint: &'a str,
    pub credential: Option<&'a str>,
    pub generation: u64,
}
impl Destination<'_> {
    pub fn is_current(self, current: Option<Destination<'_>>, connected: bool) -> bool {
        connected && self.credential.is_some() && current == Some(self)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    #[default]
    Counting,
    Checking,
    Waiting,
    Prompt,
    Pending,
    Fallback,
    Thanks,
    Suppressed,
}
#[derive(Debug, Default)]
pub struct Prompt {
    pub stage: Stage,
    due: Option<u64>,
}
impl Prompt {
    pub fn record(&mut self, stored: Result<(u64, bool), String>) -> bool {
        if self.stage != Stage::Counting {
            return false;
        }
        match stored {
            Ok((_, true)) | Err(_) => self.suppress(),
            Ok((sent, false)) if sent > 5 => {
                self.stage = Stage::Checking;
                return true;
            }
            _ => {}
        }
        false
    }
    /// Returns whether an already-starred result needs a durable dismissal.
    pub fn checked(&mut self, starred: Option<bool>) -> bool {
        if self.stage != Stage::Checking {
            return false;
        }
        match starred {
            None | Some(true) => self.suppress(),
            Some(false) => self.stage = Stage::Waiting,
        }
        starred == Some(true)
    }
    pub fn activity(&mut self, now: u64, eligible: bool) {
        if self.stage == Stage::Waiting {
            self.due = eligible.then(|| now.saturating_add(IDLE_MS));
        }
    }
    pub fn due(&self) -> Option<u64> {
        self.due
    }
    pub fn idle(&mut self, now: u64, eligible: bool, dismissed: Result<bool, String>) -> bool {
        if self.stage != Stage::Waiting {
            return false;
        }
        if dismissed != Ok(false) {
            self.suppress();
            return false;
        }
        if !eligible {
            self.due = None;
            return false;
        }
        if self.due.is_none_or(|due| now < due) {
            return false;
        }
        self.stage = Stage::Prompt;
        self.due = None;
        true
    }
    pub fn start_star(&mut self) -> bool {
        if self.stage != Stage::Prompt {
            return false;
        }
        self.stage = Stage::Pending;
        true
    }
    pub fn starred(&mut self, result: bool) -> bool {
        if self.stage != Stage::Pending {
            return false;
        }
        self.stage = if result {
            Stage::Thanks
        } else {
            Stage::Fallback
        };
        result
    }
    pub fn suppress(&mut self) {
        self.stage = Stage::Suppressed;
        self.due = None;
    }
}

/// One-burst confetti parameters matching `GitHubStarPrompt.tsx` (`pingdotgg/t3code#29987`).
pub const CONFETTI_PARTICLE_COUNT: usize = 160;
pub const CONFETTI_TICKS: u32 = 140;
pub const CONFETTI_ORIGIN_X: f32 = 0.83;
pub const CONFETTI_ORIGIN_Y: f32 = 0.15;

pub fn should_celebrate(reduced_motion: bool) -> bool {
    !reduced_motion
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfettiParticle {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub size: f32,
    pub color_index: usize,
    pub alpha: f32,
}

#[derive(Debug, Clone)]
pub struct ConfettiBurst {
    pub particles: Vec<ConfettiParticle>,
    pub tick: u32,
    pub total_ticks: u32,
}

impl ConfettiBurst {
    pub fn spawn(width: f32, height: f32) -> Self {
        let origin_x = width * CONFETTI_ORIGIN_X;
        let origin_y = height * CONFETTI_ORIGIN_Y;
        let mut seed: u64 = 0x51a4_c0de_2998_7001;
        let mut next_f32 = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 40) as f32) / ((1u64 << 24) as f32)
        };
        let mut particles = Vec::with_capacity(CONFETTI_PARTICLE_COUNT);
        for index in 0..CONFETTI_PARTICLE_COUNT {
            let angle = next_f32() * std::f32::consts::TAU;
            let speed = (0.35 + next_f32() * 0.65) * 14.0;
            particles.push(ConfettiParticle {
                x: origin_x,
                y: origin_y,
                vx: angle.cos() * speed,
                vy: angle.sin() * speed - 3.5,
                size: 4.0 + next_f32() * 4.0,
                color_index: index % 6,
                alpha: 1.0,
            });
        }
        Self { particles, tick: 0, total_ticks: CONFETTI_TICKS }
    }

    pub fn is_done(&self) -> bool {
        self.tick >= self.total_ticks
    }

    pub fn step(&mut self) -> bool {
        if self.is_done() {
            return false;
        }
        self.tick += 1;
        let remaining = 1.0 - (self.tick as f32 / self.total_ticks as f32);
        for particle in &mut self.particles {
            particle.x += particle.vx;
            particle.y += particle.vy;
            particle.vx *= 0.94;
            particle.vy = particle.vy * 0.94 + 0.55;
            particle.alpha = remaining.clamp(0.0, 1.0);
        }
        !self.is_done()
    }
}

#[cfg(test)]
mod confetti_tests {
    use super::*;

    #[test]
    fn confetti_respects_reduced_motion_and_terminates_cleanly() {
        assert!(!should_celebrate(true));
        assert!(should_celebrate(false));
        let mut burst = ConfettiBurst::spawn(1200.0, 800.0);
        assert_eq!(burst.particles.len(), CONFETTI_PARTICLE_COUNT);
        assert!(!burst.is_done());
        let mut steps = 0;
        while burst.step() {
            steps += 1;
        }
        assert_eq!(steps + 1, CONFETTI_TICKS);
        assert!(burst.is_done());
        assert!(!burst.step());
        assert!(burst.particles.iter().all(|p| p.alpha == 0.0));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn waiting() -> Prompt {
        let mut p = Prompt::default();
        assert!(p.record(Ok((6, false))));
        assert!(!p.checked(Some(false)));
        p.activity(0, true);
        p
    }
    #[test]
    fn six_successes_claim_once_and_unavailable_does_not_dismiss() {
        let mut p = Prompt::default();
        for sent in 1..=5 {
            assert!(!p.record(Ok((sent, false))));
        }
        assert!(p.record(Ok((6, false))));
        assert!(!p.record(Ok((7, false))));
        assert!(!p.checked(None));
        assert_eq!(p.stage, Stage::Suppressed);
        assert!(!p.record(Ok((8, false))));
        let mut next_session = Prompt::default();
        assert!(next_session.record(Ok((9, false))));
        assert!(next_session.checked(Some(true)));
        for storage in [Err("unwritable".into()), Ok((9, true))] {
            let mut p = Prompt::default();
            assert!(!p.record(storage));
            assert_eq!(p.stage, Stage::Suppressed);
        }
    }
    #[test]
    fn approval_feedback_is_not_a_prompt_but_message_plans_are() {
        use crate::commands::{self, ApprovalDecision, ComposerDispatch, ComposerMessage};
        let feedback = commands::respond_to_approval(
            "cmd",
            "thread",
            "approval",
            ApprovalDecision::Decline,
            Some("feedback"),
        );
        assert!(
            !plan_sends_prompt([&feedback]),
            "approval-only feedback must not increment the successful-prompt count"
        );
        let model = crate::traits::model_selection("codex", "synthetic", &[]);
        for dispatch in [
            ComposerDispatch::Auto,
            ComposerDispatch::Queue,
            ComposerDispatch::Steer,
        ] {
            let message = commands::composer_message(&ComposerMessage {
                command_id: "cmd",
                thread_id: "thread",
                message_id: "message",
                text: "prompt",
                title_seed: "Prompt",
                model_selection: &model,
                dispatch,
                context: None,
                attachments: &[],
                fresh_context: false,
                idle_compaction: None,
            });
            assert!(plan_sends_prompt([&message]));
            let decline = commands::respond_to_approval(
                "decline",
                "thread",
                "approval",
                ApprovalDecision::Decline,
                None,
            );
            assert!(plan_sends_prompt([&decline, &message]));
        }
        assert!(!plan_sends_prompt(std::iter::empty()));
    }

    #[test]
    fn input_navigation_focus_and_embeds_restart_a_single_deadline() {
        let mut p = waiting();
        assert!(!p.idle(IDLE_MS - 1, true, Ok(false)));
        p.activity(IDLE_MS - 1, true);
        assert!(!p.idle(IDLE_MS, true, Ok(false)));
        p.activity(IDLE_MS, false);
        assert!(!p.idle(3 * IDLE_MS, false, Ok(false)));
        assert_eq!(p.due(), None);
        p.activity(3 * IDLE_MS, true);
        assert!(p.idle(4 * IDLE_MS, true, Ok(false)));
        assert!(!p.idle(5 * IDLE_MS, true, Ok(false)));
    }
    #[test]
    fn cross_window_dismissal_storage_failure_and_disposal_retire_pending_work() {
        for dismissed in [Ok(true), Err("unreadable".into())] {
            let mut p = waiting();
            assert!(!p.idle(IDLE_MS, true, dismissed));
            assert_eq!(p.stage, Stage::Suppressed);
        }
        for stage in [Stage::Checking, Stage::Waiting, Stage::Pending] {
            let mut p = Prompt {
                stage,
                due: Some(0),
            };
            p.suppress();
            assert!(!p.checked(Some(false)));
            assert!(!p.idle(IDLE_MS, true, Ok(false)));
            assert!(!p.starred(true));
            assert_eq!(p.stage, Stage::Suppressed);
        }
    }
    #[test]
    fn pending_clicks_are_deduplicated_and_false_or_error_needs_explicit_fallback() {
        for result in [false, true] {
            let mut p = waiting();
            p.idle(IDLE_MS, true, Ok(false));
            assert!(p.start_star());
            assert!(!p.start_star());
            assert_eq!(p.starred(result), result);
            assert_eq!(
                p.stage,
                if result {
                    Stage::Thanks
                } else {
                    Stage::Fallback
                }
            );
            assert!(!p.start_star());
        }
    }
    #[test]
    fn exact_wire_input_and_destination_session_permission() {
        assert_eq!(
            input(),
            json!({"provider":"github","host":"github.com","repository":"pingdotgg/t3code"})
        );
        assert_eq!(
            (CHECK, STAR),
            ("sourceControl.starred", "sourceControl.star")
        );
        let source = Destination {
            id: "source",
            endpoint: "https://source",
            credential: Some("synthetic"),
            generation: 1,
        };
        assert!(source.is_current(Some(source), true));
        for current in [
            None,
            Some(Destination {
                id: "current-environment",
                ..source
            }),
            Some(Destination {
                endpoint: "https://relocated",
                ..source
            }),
            Some(Destination {
                credential: Some("repaired"),
                ..source
            }),
            Some(Destination {
                credential: None,
                ..source
            }),
            Some(Destination {
                generation: 2,
                ..source
            }),
        ] {
            assert!(!source.is_current(current, true));
        }
        assert!(
            !source.is_current(Some(source), false),
            "disconnect invalidates before the generation advances on reconnect"
        );
        assert!(!can_star(None));
        for (permissions, allowed) in [
            (vec!["source-control:write"], true),
            (vec!["orchestration:operate"], false),
            (vec![], false),
        ] {
            let session =
                serde_json::from_value(json!({"authenticated":true,"permissions":permissions}))
                    .unwrap();
            assert_eq!(can_star(Some(&session)), allowed);
        }
    }
}
