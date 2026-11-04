//! Deterministic offscreen diff transport; never forwards fixture requests to a server.
use super::{DiffPanel, Fetch, Loaded, source};
use gpui::Context;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use t3_core::diff::{DiffScope, PreviewSourceKind, ReviewScope};

pub fn install(panel: &mut DiffPanel, cx: &mut Context<DiffPanel>) -> bool {
    let Some(path) = std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|pair| pair[0] == "--diff-fixture")
        .map(|p| p[1].clone())
    else {
        return false;
    };
    let fixture: Value = serde_json::from_slice(&std::fs::read(&path).expect("diff fixture file")).expect("diff fixture JSON");
    let fixture = Arc::new(fixture);
    let attempts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
    let scope = ReviewScope {
        environment: "offscreen-diff".into(),
        connection: 1,
        cwd: "/fixture".into(),
        kind: PreviewSourceKind::WorkingTree,
        base_ref: None,
        ignore_whitespace: true,
    };
    panel.fixture = true;
    panel.thread_id = panel.state.read(cx).selected_thread.clone().or_else(|| {
        std::env::args()
            .collect::<Vec<_>>()
            .windows(2)
            .find(|p| p[0] == "--thread")
            .map(|p| p[1].clone())
    });
    panel.scope = DiffScope::Unstaged;
    panel.request = Some(source::RequestKey::Git {
        environment: scope.environment.clone(),
        kind: scope.kind,
        cwd: scope.cwd.clone(),
        base_ref: None,
        ignore_whitespace: true,
        revision: 0,
    });
    panel.visible = true;
    panel.pending = true;
    let (send, commands) = tokio::sync::mpsc::channel(16);
    let (updates, mut receive) = tokio::sync::mpsc::channel(1);
    panel.patch_commands = Some(send);
    let task = panel.state.read(cx).runtime().spawn(source::run_with(
        move |_, input: Value| {
            let fixture = fixture.clone();
            let attempts = attempts.clone();
            async move {
                eprintln!("diff-fixture read-only request {input}");
                let response = if let Some(path) = input["file"]["path"].as_str() {
                    let mut attempts = attempts.lock().expect("fixture attempts");
                    let attempt = attempts.entry(path.to_owned()).or_default();
                    let values = fixture["patches"][path].as_array();
                    let result = values
                        .and_then(|values| values.get((*attempt).min(values.len().saturating_sub(1))))
                        .cloned();
                    *attempt += 1;
                    result
                } else {
                    fixture
                        .get(if input["trackedOnly"] == true { "tracked" } else { "complete" })
                        .cloned()
                };
                let Some(response) = response else {
                    return std::future::pending().await;
                };
                if let Some(error) = response["error"].as_str() {
                    return Err(error.into());
                }
                Ok(response)
            }
        },
        scope,
        None,
        panel.patch_reads.clone(),
        commands,
        updates,
    ));
    let completion = cx.spawn(async move |this, cx| {
        while let Some(result) = receive.recv().await {
            if this
                .update(cx, |this, cx| {
                    match result {
                        Ok(loaded) => {
                            if let Loaded::Git { review, .. } = &loaded {
                                this.pending = review.incomplete && this.error.is_none();
                                eprintln!(
                                    "diff-fixture state revision={} incomplete={} files={} in-flight={}",
                                    review.revision,
                                    review.incomplete,
                                    review.files().len(),
                                    review.states.values().filter(|s| s.pending).count()
                                );
                            }
                            this.loaded = Some((this.request.clone().expect("fixture request"), loaded));
                        }
                        Err(error) => {
                            this.pending = false;
                            this.error = Some(error);
                        }
                    }
                    this.rebuild(cx);
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        }
    });
    panel._fetch = Some(Fetch {
        network: task.abort_handle(),
        _completion: completion,
    });
    true
}
