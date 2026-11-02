//! Writes the raw `orchestration.subscribeThread` frames of one thread as a JSON array, for
//! building transcript fixtures. Uses the saved connection (`T3_NATIVE_HOME`).
//!
//!   cargo run -p t3-client --example dump_thread -- <thread-id> <out.json> [--seconds 5]

use std::time::Duration;

use serde_json::Value;
use t3_client::session::Session;
use t3_client::{auth, store};
use t3_core::commands::{self, method};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(thread_id), Some(out)) = (args.get(1), args.get(2)) else {
        anyhow::bail!("usage: dump_thread <thread-id> <out.json> [--seconds N]");
    };
    let seconds: u64 = args
        .iter()
        .position(|a| a == "--seconds")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection"))?;
    let ticket = auth::issue_rpc_ticket(&credentials).await?;
    let session = Session::connect(&auth::socket_url(&credentials, &ticket))
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
    let mut stream = session
        .stream(method::SUBSCRIBE_THREAD, commands::subscribe_thread(thread_id, None))
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut frames: Vec<Value> = Vec::new();
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            frame = stream.receiver.recv() => match frame {
                Some(Ok(value)) => frames.push(value),
                Some(Err(error)) => anyhow::bail!("stream failed: {error}"),
                None => break,
            }
        }
    }
    std::fs::write(out, serde_json::to_string_pretty(&frames)?)?;
    println!("wrote {} frames to {out}", frames.len());
    Ok(())
}
