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

/// Clipboard attachments are copied into a fresh upload; the source upload is never adopted.
pub async fn copy_attachment_owned(client: ClientHandle, attachment: &t3_core::composer::attachments::Attachment, source_id: &str) -> Result<UploadedAttachment,String> {
    let resource=t3_core::media::AssetResource::Attachment{attachment_id:source_id.to_owned()};
    let minted=client.request(t3_core::media::CREATE_ASSET_URL,resource.create_url_request()).await.map_err(|_|"The original attachment is no longer available.".to_owned())?;
    let asset=t3_core::media::decode_asset_url(&minted)?;
    let limit=match attachment.kind {attachments::AttachmentKind::Image=>attachments::MAX_IMAGE_BYTES,attachments::AttachmentKind::File=>attachments::MAX_FILE_BYTES};
    let bytes=crate::asset::fetch_asset(client.environment(),&asset.relative_url,limit as usize).await?;
    let mut request=attachment.upload_request(); request["sizeBytes"]=serde_json::json!(bytes.len());
    upload_attachment_owned(client,request,&attachment.mime_type,bytes).await
}

#[cfg(test)]
mod copy_tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn attachment_copies_upload_the_bytes_and_release_only_their_own_asset() {
        let mut server=crate::test_server::FakeServer::start().await;
        let (client,_events)=crate::supervisor::start(&tokio::runtime::Handle::current(),server.credentials());
        let mut connection=server.connection().await;
        let config=connection.request(t3_core::commands::method::SUBSCRIBE_SERVER_CONFIG).await;
        connection.send(json!({"_tag":"Exit","requestId":config["id"],"exit":{"_tag":"Failure","cause":[{"_tag":"Fail","error":{"message":"no config"}}]}})).await;
        for id in ["copy-one","copy-two"] {
            let client=client.clone();
            let task=tokio::spawn(async move {
                let attachment=attachments::Attachment {id:"local".into(),kind:attachments::AttachmentKind::File,name:"a.txt".into(),mime_type:"text/plain".into(),size_bytes:12,pasted_text:false,upload:attachments::Upload::Uploading};
                copy_attachment_owned(client,&attachment,"source").await.unwrap()
            });
            let asset=connection.request(t3_core::media::CREATE_ASSET_URL).await;
            assert_eq!(asset["payload"]["resource"]["attachmentId"],"source");
            connection.send(json!({"_tag":"Exit","requestId":asset["id"],"exit":{"_tag":"Success","value":{"relativeUrl":"/test-copy-source"}}})).await;
            let upload=connection.request(CREATE_UPLOAD_URL).await;
            assert_eq!(upload["payload"]["sizeBytes"],12);
            connection.send(json!({"_tag":"Exit","requestId":upload["id"],"exit":{"_tag":"Success","value":{"attachmentId":id,"relativeUrl":"/test-copy-destination"}}})).await;
            drop(task.await.unwrap());
            let deleted=connection.request(attachments::DELETE).await;
            assert_eq!(deleted["payload"]["attachmentId"],id);
            connection.send(json!({"_tag":"Exit","requestId":deleted["id"],"exit":{"_tag":"Success","value":null}})).await;
        }
        assert_eq!(server.http_requests(),[("/test-copy-source".into(),vec![]),("/test-copy-destination".into(),b"source bytes".to_vec()),("/test-copy-source".into(),vec![]),("/test-copy-destination".into(),b"source bytes".to_vec())]);
    }
}
