use super::*;
use crate::session::RpcError;
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use t3_core::rpc::RpcFailure;

enum Step {
    Request(&'static str, Result<Value, RpcError>),
    Reconnect(u64),
    Ready(Value),
}
struct Script {
    steps: Mutex<VecDeque<Step>>,
    generation: AtomicU64,
}
impl Script {
    fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().collect()),
            generation: AtomicU64::new(1),
        }
    }
    fn next(&self) -> Option<Step> {
        self.steps.lock().unwrap().pop_front()
    }
    fn finished(&self) {
        assert!(self.steps.lock().unwrap().is_empty());
    }
}
impl UpdateTransport for Script {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
    async fn request(&self, method: &'static str, input: Value) -> Result<Value, RpcError> {
        match self.next() {
            Some(Step::Request(expected, result)) => {
                assert_eq!(method, expected);
                if method == "server.commitDesktopUpdate" {
                    assert_eq!(input, json!({"requestId":"prepared"}));
                }
                result
            }
            _ => panic!("unexpected RPC {method}"),
        }
    }
    async fn reconnected(&self, after: u64) -> Result<(), RpcError> {
        match self.next() {
            Some(Step::Reconnect(generation)) => {
                assert_eq!(after, self.generation());
                assert!(generation > after);
                self.generation.store(generation, Ordering::Relaxed);
                Ok(())
            }
            _ => panic!("must observe reconnect before accepting readiness"),
        }
    }
    async fn next_ready(&self, _result: &Value) -> Result<Value, RpcError> {
        match self.next() {
            Some(Step::Ready(value)) => Ok(value),
            None => Err(RpcError::Closed),
            _ => panic!("readiness observed before reconnect or commit"),
        }
    }
}
fn interrupted() -> RpcError {
    RpcError::Failure(RpcFailure {
        tag: None,
        message: "Interrupted".into(),
        error: Value::Null,
    })
}
fn denied() -> RpcError {
    RpcError::Failure(RpcFailure {
        tag: Some("Denied".into()),
        message: "Denied".into(),
        error: json!({"_tag":"Denied"}),
    })
}
fn ready(version: &str) -> Value {
    json!({"type":"ready","payload":{"environment":{"serverVersion":version}}})
}
fn prepared() -> Step {
    Step::Request(
        "server.updateServer",
        Ok(json!({"method":"desktop-app","targetVersion":"2.0.0","desktopUpdateToken":"prepared"})),
    )
}

#[tokio::test]
async fn legacy_acknowledgement_losses_wait_for_reconnect_and_prove_the_target_version() {
    for capability in ["boot-service", "respawn"] {
        for error in [RpcError::Closed, interrupted()] {
            let script = Script::new([
                Step::Request("server.updateServer", Err(error)),
                Step::Reconnect(2),
                Step::Ready(ready("2.0.0")),
            ]);
            assert_eq!(update_with(&script, "2.0.0", capability).await, Ok(()));
            script.finished();
        }
    }
    for (capability, error) in [
        ("desktop-managed", RpcError::Closed),
        ("boot-service", denied()),
    ] {
        let script = Script::new([Step::Request("server.updateServer", Err(error))]);
        assert!(update_with(&script, "2.0.0", capability).await.is_err());
        script.finished();
    }
}
#[tokio::test]
async fn lost_desktop_commits_retry_the_same_prepared_token_after_old_version_readiness() {
    for error in [RpcError::Closed, interrupted()] {
        let script = Script::new([
            prepared(),
            Step::Request("server.commitDesktopUpdate", Err(error)),
            Step::Reconnect(2),
            Step::Ready(ready("1.0.0")),
            Step::Request("server.commitDesktopUpdate", Err(interrupted())),
            Step::Reconnect(3),
            Step::Ready(ready("2.0.0")),
        ]);
        assert_eq!(
            update_with(&script, "2.0.0", "desktop-managed").await,
            Ok(())
        );
        script.finished();
    }
}
#[tokio::test]
async fn undelivered_desktop_commits_fail_after_bounded_retries() {
    let script = Script::new([
        prepared(),
        Step::Request("server.commitDesktopUpdate", Err(RpcError::Closed)),
        Step::Reconnect(2),
        Step::Ready(ready("1.0.0")),
        Step::Request("server.commitDesktopUpdate", Err(RpcError::Closed)),
        Step::Reconnect(3),
        Step::Ready(ready("1.0.0")),
        Step::Request("server.commitDesktopUpdate", Err(RpcError::Closed)),
        Step::Reconnect(4),
        Step::Ready(ready("1.0.0")),
    ]);
    assert_eq!(
        update_with(&script, "2.0.0", "desktop-managed").await,
        Err("The desktop app resumed without installing the prepared update.".into())
    );
    script.finished();
}
#[tokio::test]
async fn completed_desktop_commit_and_terminal_errors_do_not_retry_forever() {
    for result in [Ok(json!({})), Err(denied())] {
        let script = Script::new([
            prepared(),
            Step::Request("server.commitDesktopUpdate", Err(RpcError::Closed)),
            Step::Reconnect(2),
            Step::Ready(ready("1.0.0")),
            Step::Request("server.commitDesktopUpdate", result),
        ]);
        assert!(
            update_with(&script, "2.0.0", "desktop-managed")
                .await
                .is_err()
        );
        script.finished();
    }
    let script = Script::new([
        prepared(),
        Step::Request("server.commitDesktopUpdate", Err(interrupted())),
        Step::Reconnect(2),
        Step::Ready(ready("2.0.0")),
    ]);
    assert_eq!(
        update_with(&script, "2.0.0", "desktop-managed").await,
        Ok(())
    );
    script.finished();
}

#[tokio::test]
async fn malformed_desktop_preparation_never_commits_a_token() {
    for result in [
        json!({"method":"desktop-app","targetVersion":"2.0.0","desktopUpdateToken":""}),
        json!({"method":"desktop-app","targetVersion":" ","desktopUpdateToken":"prepared"}),
    ] {
        let script = Script::new([Step::Request("server.updateServer",Ok(result))]);
        assert!(update_with(&script,"2.0.0","desktop-managed").await.is_err());
        script.finished();
    }
}

struct ChannelScript(Script);
impl UpdateTransport for ChannelScript {
    fn generation(&self) -> u64 {
        self.0.generation()
    }
    async fn request(&self, method: &'static str, input: Value) -> Result<Value, RpcError> {
        match self.0.next() {
            Some(Step::Request(expected, result)) => {
                assert_eq!(method, expected);
                if method == "server.updateServer" {
                    assert_eq!(input["channel"], "latest");
                    assert_eq!(input["targetDatabaseMigration"], 69);
                }
                result
            }
            _ => panic!("unexpected RPC {method}"),
        }
    }
    async fn reconnected(&self, after: u64) -> Result<(), RpcError> {
        self.0.reconnected(after).await
    }
    async fn next_ready(&self, result: &Value) -> Result<Value, RpcError> {
        self.0.next_ready(result).await
    }
}

#[tokio::test]
async fn channel_switch_passes_channel_migration_and_track_only_without_restarting() {
    let script = ChannelScript(Script::new([Step::Request(
        "server.updateServer",
        Ok(json!({"targetVersion":"0.1.3","method":"desktop-app"})),
    )]));
    let mut stages = Vec::new();
    let options = ServerRequestOptions {
        channel: Some(t3_core::updates::UpdateTrack::Latest),
        release_channel: Some(t3_core::updates::UpdateTrack::Latest),
        target_database_migration: Some(69),
        track_only: true,
    };
    assert_eq!(
        update_with_options(&script, "0.1.3", "desktop-managed", &options, |stage, _| {
            stages.push(stage)
        })
        .await,
        Ok(())
    );
    script.0.finished();
    assert_eq!(stages, [t3_core::updates::ServerUpdateStage::Downloading]);
}

#[tokio::test]
async fn update_with_options_reports_server_response_version_on_handoff() {
    let script = Script::new([
        Step::Request(
            "server.updateServer",
            Ok(json!({
                "method": "desktop-app",
                "targetVersion": "0.2.1-feed",
                "desktopUpdateToken": "prepared"
            })),
        ),
        Step::Request("server.commitDesktopUpdate", Ok(json!({}))),
        Step::Ready(json!({
            "type": "ready",
            "payload": { "environment": { "serverVersion": "0.2.1-feed" } }
        })),
    ]);
    let mut handed_off = None;
    assert_eq!(
        update_with_options(
            &script,
            "0.2.0",
            "desktop-managed",
            &ServerRequestOptions::default(),
            |stage, version| {
                if stage == ServerUpdateStage::Resuming {
                    handed_off = version.map(str::to_owned);
                }
            },
        )
        .await,
        Ok(())
    );
    script.finished();
    assert_eq!(handed_off.as_deref(), Some("0.2.1-feed"));
}
