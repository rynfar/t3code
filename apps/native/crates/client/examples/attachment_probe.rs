//! Upload/delete smoke for attachment metadata. Restricted to the Replay smoke environment;
//! it never starts a provider turn and deletes its disposable upload before exiting.
use serde_json::json;
use t3_client::{store, supervisor::{ClientEvent, start}};
use t3_core::{composer::attachments::{self, Attachment, AttachmentKind, Upload}, server_config::ServerConfigState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection"))?;
    anyhow::ensure!(credentials.http_base == "http://127.0.0.1:14658", "use only smoke server 14658");
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut config = ServerConfigState::default();
    while config.config.is_none() {
        match events.recv().await {
            Some(ClientEvent::ServerConfig(event)) => { config.apply(*event); }
            Some(_) => {}
            None => anyhow::bail!("connection closed"),
        }
    }
    anyhow::ensure!(config.config.unwrap().providers.iter().any(|provider| provider.instance_id == "devReplay" && provider.enabled), "Replay (dev) must be available");
    let bytes = b"native attachment size smoke\n".to_vec();
    let mut attachment = Attachment { id: "local-smoke".into(), kind: AttachmentKind::File, name: "native-175-smoke.txt".into(), mime_type: "text/plain".into(), size_bytes: bytes.len() as u64, pasted_text: false, upload: Upload::Uploading };
    println!("upload request: {}", attachment.upload_request());
    let uploaded = t3_client::upload::upload_attachment_owned(client.clone(), attachment.upload_request(), &attachment.mime_type, bytes).await.map_err(anyhow::Error::msg)?;
    let id = uploaded.into_id();
    attachment.upload = Upload::Ready { attachment_id: id.clone() };
    let result = client.request(attachments::DELETE, attachments::delete_request(&id)).await;
    println!("wire attachment: {}", attachment.chat_attachment().unwrap());
    println!("cleanup: {}", json!({"deleted": result.is_ok(), "providerTurns": 0}));
    result?;
    Ok(())
}
