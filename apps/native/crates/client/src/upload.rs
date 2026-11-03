//! Attachment bytes go to the signed URL `attachments.createUploadUrl` mints, as
//! `uploadBytes` in `web/src/lib/attachmentUploadQueue.ts` posts them: the raw body with its
//! content type. The URL carries its own signature, so no bearer token rides along.

use std::time::Duration;

mod owned;
pub use owned::UploadedAttachment;

use t3_core::composer::attachments::{self, CREATE_UPLOAD_URL, decode_ticket};

use crate::supervisor::ClientHandle;

/// `UPLOAD_TIMEOUT_MS`.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Mints an upload for `request` (`AttachmentCreateUploadUrlInput`), posts `bytes` to it, and
/// returns the server's attachment id. Errors read as the web's upload failure reasons.
pub async fn upload_attachment(client: &ClientHandle, request: serde_json::Value, mime_type: &str, bytes: Vec<u8>) -> Result<String, String> {
    upload_attachment_owned(client.clone(), request, mime_type, bytes).await.map(UploadedAttachment::into_id)
}

/// The upload result retains cleanup ownership until its caller adopts the asset id.
pub async fn upload_attachment_owned(client: ClientHandle, request: serde_json::Value, mime_type: &str, bytes: Vec<u8>) -> Result<UploadedAttachment, String> {
    let reply = client.request(CREATE_UPLOAD_URL, request).await.map_err(|_| "Upload could not start".to_owned())?;
    let ticket = decode_ticket(&reply).ok_or_else(|| "Upload could not start".to_owned())?;
    let runtime = tokio::runtime::Handle::current();
    let cleanup_client = client.clone();
    let owned = UploadedAttachment::new(ticket.attachment_id, move |id| {
        drop(runtime.spawn(async move { let _ = cleanup_client.request(attachments::DELETE, attachments::delete_request(&id)).await; }));
    });
    let url = attachments::upload_url(client.environment(), &ticket.relative_url).ok_or_else(|| "Not connected".to_owned())?;
    let http = reqwest::Client::builder().timeout(UPLOAD_TIMEOUT).build().map_err(|_| "Upload failed".to_owned())?;
    let response = http.post(url).header(reqwest::header::CONTENT_TYPE, mime_type).body(bytes).send().await.map_err(|error| {
        if error.is_timeout() { "Upload timed out".to_owned() } else { "Upload failed".to_owned() }
    })?;
    if !response.status().is_success() {
        return Err(format!("Upload rejected ({})", response.status().as_u16()));
    }
    Ok(owned)
}
