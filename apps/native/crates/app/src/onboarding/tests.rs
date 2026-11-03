//! Real native RPC against a fake server; no T3 data or provider processes.
use serde_json::json;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};
use t3_client::session::Session;
use t3_core::{onboarding::projects as model, projects::PROJECTS_MUTATE};
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[tokio::test]
async fn onboarding_rpc_retries_ambiguous_creates_and_partial_history_without_duplicates() {
    let mut child = Command::new("node").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/src/onboarding/tests/rpc-server.cjs")).stdout(Stdio::piped()).spawn().unwrap();
    let mut port = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut port).unwrap();
    let _server = Server(child);
    let url = format!("ws://127.0.0.1:{}", port.trim());
    let mut session = Session::connect(&url).await.unwrap();
    let scan: model::Scan = serde_json::from_value(session.request(model::SCAN, json!({})).await.unwrap()).unwrap();
    let candidate = &scan.candidates[0];
    let key = model::key("fixture", &candidate.path);
    let mut progress = model::Progress::default();
    progress.begin_attempt(vec![key.clone()]);
    progress.attempts.insert(key.clone(), "stable-project".into());
    for _ in 0..4 {
        let id = progress.attempts[&key].clone();
        let failure = session.request(PROJECTS_MUTATE, model::create_payload(&id, candidate)).await.unwrap_err();
        let failure = super::projects::RequestFailure::from(failure);
        assert!(!failure.definitive, "{failure}");
        progress.failed_attempt(&key, failure.definitive);
        assert_eq!(progress.attempts[&key], "stable-project");
        session = Session::connect(&url).await.unwrap();
    }
    session.request(PROJECTS_MUTATE, model::create_payload(&progress.attempts[&key], candidate)).await.unwrap();
    let rejected = session.request(PROJECTS_MUTATE, model::create_payload("rejected-project", candidate)).await.unwrap_err();
    assert!(super::projects::RequestFailure::from(rejected).definitive);
    for expected in [2, 3] {
        progress.begin_attempt(vec![key.clone()]);
        let result = serde_json::from_value(session.request(model::IMPORT, model::import_payload("stable-project", candidate)).await.unwrap()).unwrap();
        progress.record(key.clone(), "stable-project".into(), result);
        assert_eq!(progress.imported, expected);
    }
    assert_eq!(progress.skipped, 0);
    assert_eq!(progress.completed.len(), 1);
    assert_eq!(progress.landing(), Some("stable-project"));
    let rejected = session.request("unsupported.onboarding", json!({})).await.unwrap_err();
    assert!(!super::projects::RequestFailure::from(rejected).definitive);
    let mut demand = model::Demand::default();
    let token = demand.begin(7);
    let mut held = Box::pin(session.request(model::IMPORT, json!({"projectId":"held-project","expectedWorkspaceRoot":"/fixture/repo"})));
    assert!(matches!(futures::poll!(held.as_mut()), std::task::Poll::Pending));
    assert_eq!(progress.completion(model::FinishIntent::Import, false, |_| true), model::Completion::Denied);
    assert_eq!(progress.completion(model::FinishIntent::Skip, false, |id| id == "stable-project"), model::Completion::Ready(Some("stable-project".into())));
    demand.cancel();
    drop(held);
    assert!(!demand.accepts(token, Some(7), true));
    let stats = session.request("fixture.inspect", json!({})).await.unwrap();
    assert_eq!(stats["interrupts"], 1);
    assert_eq!(stats["creates"], 1);
    assert_eq!(stats["createCalls"], 6);
    assert_eq!(stats["imports"], 2);
    println!(
        "create committed once across Defect, ClientProtocolError, post-commit ProjectMutationError and unknown failure; typed conflict confirmed; retry 2/1 -> 3/0 reports 3; revoked automatic completion denied; local skip retains landing, interrupts pending import, no extra writes"
    );
}
