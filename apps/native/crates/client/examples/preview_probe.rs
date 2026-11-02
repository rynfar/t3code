//! Headless preview browser check against a live server: lists a thread's preview sessions and,
//! unless `--list-only`, opens a server-browser tab on `--url`, streams it (control, viewport,
//! first frame, an agent-free navigation to `--then`), and closes the tab again. Uses the saved
//! connection, like `probe`. Opening launches headless Chromium on the server: use a dev server.
//!
//!   cargo run -p t3-client --example preview_probe -- [--thread <id>] [--url <url>] [--then <url>]
//!   cargo run -p t3-client --example preview_probe -- --list-only
//!   cargo run -p t3-client --example preview_probe -- --url <url> --keep   # then --close <tab>

use std::time::Duration;

use serde_json::json;
use t3_client::preview_stream::{StreamEvent, StreamTarget};
use t3_client::store;
use t3_client::supervisor::{ClientEvent, ConnectionStatus, start};
use t3_core::preview::stream::{Controller, StreamInput, StreamMessage};
use t3_core::preview::{ListResult, PreviewEvent, SessionSnapshot, method};
use t3_core::shell::ShellState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection; pair with the probe first"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut shell = ShellState::default();
    let mut connected = false;
    while !(connected && shell.synchronized) {
        match tokio::time::timeout(Duration::from_secs(15), events.recv()).await? {
            Some(ClientEvent::Status(ConnectionStatus::Connected)) => connected = true,
            Some(ClientEvent::Status(ConnectionStatus::NeedsPairing { reason })) => anyhow::bail!(reason),
            Some(ClientEvent::Shell(item)) => {
                shell.apply(item);
            }
            Some(_) => {}
            None => anyhow::bail!("client stopped"),
        }
    }
    let snapshot = shell.snapshot.as_ref().ok_or_else(|| anyhow::anyhow!("no shell snapshot"))?;
    let thread_id = match flag("--thread") {
        Some(id) => id,
        None => snapshot.threads.first().ok_or_else(|| anyhow::anyhow!("the server has no threads"))?.id.clone(),
    };
    let listed = ListResult::decode(client.request(method::LIST, json!({ "threadId": thread_id })).await?).map_err(anyhow::Error::msg)?;
    println!("list: thread={thread_id} epoch={} revision={} sessions={}", listed.server_epoch, listed.revision, listed.sessions.len());
    for session in &listed.sessions {
        println!("  tab={} runtime={:?} headless={:?} owner={:?} url={:?}", session.tab_id, session.runtime, session.headless, session.automation_owner, session.nav_status.url());
    }
    if args.iter().any(|a| a == "--list-only") {
        return Ok(());
    }
    if let Some(tab_id) = flag("--close") {
        client.request(method::CLOSE, json!({ "threadId": thread_id, "tabId": tab_id })).await?;
        println!("closed: tab={tab_id}");
        return Ok(());
    }
    let mut preview_events = client.stream(method::SUBSCRIBE_EVENTS, json!({})).await?;
    let url = flag("--url").unwrap_or_else(|| "https://example.com/".to_owned());
    let opened = SessionSnapshot::decode(client.request(method::OPEN, json!({ "threadId": thread_id, "url": url, "runtime": "server" })).await?).map_err(anyhow::Error::msg)?;
    println!("opened: tab={} runtime={:?} status={:?}", opened.tab_id, opened.runtime, opened.nav_status);
    let tab_id = opened.tab_id.clone();
    let result = stream(&client, &thread_id, &tab_id, flag("--then"), &mut preview_events.receiver).await;
    if args.iter().any(|a| a == "--keep") {
        println!("kept open: tab={tab_id} (close it with --close {tab_id})");
        return result;
    }
    client.request(method::CLOSE, json!({ "threadId": thread_id, "tabId": tab_id })).await?;
    println!("closed: tab={tab_id}");
    result
}

async fn stream(
    client: &t3_client::supervisor::ClientHandle,
    thread_id: &str,
    tab_id: &str,
    then: Option<String>,
    preview_events: &mut tokio::sync::mpsc::Receiver<Result<serde_json::Value, t3_client::session::RpcError>>,
) -> anyhow::Result<()> {
    let target = StreamTarget { thread_id: thread_id.to_owned(), tab_id: tab_id.to_owned(), max_width: 1200, max_height: 900 };
    let mut viewer = client.preview_stream(&tokio::runtime::Handle::current(), target);
    let mut frames = 0usize;
    let mut navigated = then.is_none();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => anyhow::bail!("timed out after {frames} frames"),
            changed = viewer.frames.changed() => {
                changed?;
                let Some(frame) = viewer.frames.borrow_and_update().clone() else { continue };
                frames += 1;
                if frames == 1 {
                    println!("first frame: {} bytes, jpeg size {:?}", frame.len(), jpeg_size(&frame));
                }
            }
            event = viewer.events.recv() => match event {
                Some(StreamEvent::Message(StreamMessage::Control(control))) => {
                    println!("control: {} (generation {})", t3_core::preview::stream::control_label(Some(&control)), control.generation);
                    if control.controller == Controller::You && let Some(url) = &then {
                        viewer.input.send(StreamInput::Navigate { url: url.clone() });
                        println!("sent navigate {url}");
                    }
                }
                Some(StreamEvent::Message(message)) => println!("message: {message:?}"),
                Some(other) => {
                    println!("event: {other:?}");
                    if matches!(other, StreamEvent::Gone | StreamEvent::Refused | StreamEvent::HostSetup(_)) {
                        anyhow::bail!("stream ended: {other:?}");
                    }
                }
                None => anyhow::bail!("stream stopped"),
            },
            event = preview_events.recv() => {
                let Some(Ok(value)) = event else { anyhow::bail!("preview events ended") };
                let event = PreviewEvent::decode(value).map_err(anyhow::Error::msg)?;
                if event.tab_id == tab_id {
                    println!("preview event: revision={} {:?}", event.revision, event_summary(&event));
                    if let Some(url) = &then && matches!(&event.kind, t3_core::preview::EventKind::Navigated(s) if s.nav_status.url() == Some(url.as_str())) {
                        navigated = true;
                    }
                }
            }
        }
        if frames >= 2 && navigated {
            println!("ok: {frames} frames");
            return Ok(());
        }
    }
}

fn event_summary(event: &PreviewEvent) -> String {
    match &event.kind {
        t3_core::preview::EventKind::Opened(s) | t3_core::preview::EventKind::Navigated(s) | t3_core::preview::EventKind::Resized(s) => {
            format!("{:?} -> {:?}", std::mem::discriminant(&event.kind), s.nav_status)
        }
        other => format!("{other:?}"),
    }
}

/// Width and height from the first SOF marker.
fn jpeg_size(bytes: &[u8]) -> Option<(u16, u16)> {
    let mut at = 2;
    while at + 9 < bytes.len() {
        if bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if matches!(marker, 0xC0..=0xC3) {
            return Some((u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]), u16::from_be_bytes([bytes[at + 5], bytes[at + 6]])));
        }
        at += 2 + length;
    }
    None
}
