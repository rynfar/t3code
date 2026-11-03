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
    let session = Session::connect(&format!("ws://127.0.0.1:{}", port.trim())).await.unwrap();
    let scan: model::Scan = serde_json::from_value(session.request(model::SCAN, json!({})).await.unwrap()).unwrap();
    let candidate = &scan.candidates[0];
    assert_eq!(candidate.sources, ["claudeAgent", "codex"]);
    let payload = model::create_payload("stable-project", candidate);
    let ambiguous = session.request(PROJECTS_MUTATE, payload.clone()).await.unwrap_err();
    assert!(!super::projects::RequestFailure::from(ambiguous).definitive);
    let session = Session::connect(&format!("ws://127.0.0.1:{}", port.trim())).await.unwrap();
    session.request(PROJECTS_MUTATE, payload).await.unwrap();
    let mut progress = model::Progress {
        selection: vec![model::key("fixture", &candidate.path)],
        ..Default::default()
    };
    for _ in 0..2 {
        let result = serde_json::from_value(session.request(model::IMPORT, model::import_payload("stable-project", candidate)).await.unwrap()).unwrap();
        progress.record(progress.selection[0].clone(), "stable-project".into(), result);
    }
    assert_eq!(progress.imported, 2);
    assert_eq!(progress.completed.len(), 1);
    assert_eq!(progress.landing(), Some("stable-project"));
    let rejected = session.request("unsupported.onboarding", json!({})).await.unwrap_err();
    assert!(super::projects::RequestFailure::from(rejected).definitive);
    let mut held = Box::pin(session.request(model::SCAN, json!({"hold":true})));
    assert!(matches!(futures::poll!(held.as_mut()), std::task::Poll::Pending));
    drop(held);
    let stats = session.request("fixture.inspect", json!({})).await.unwrap();
    assert_eq!(stats["interrupts"], 1);
    assert_eq!(stats["creates"], 1);
    assert_eq!(stats["createCalls"], 2);
    assert_eq!(stats["imports"], 2);
    println!("scan: Claude/Codex candidate; create: ambiguous commit/retry -> 1 project; import: 2 imported/1 skipped -> retry 0/0; unsupported RPC: recovered; no provider turns");
}
