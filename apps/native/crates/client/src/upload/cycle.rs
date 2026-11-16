//! Raw-byte HTTP streaming upload with chunk progress callbacks, persisted attachment
//! verification (`assets.createUrl`), and single-attempt `run_attachment_upload_cycle`
//! matching `packages/client-runtime/src/state/attachments.ts`.

use std::time::Duration;

use serde_json::Value;
use t3_core::composer::attachments::queue::{
    PersistedAttachmentVerification, UploadCycleOutcome, UploadCycleStep,
    is_asset_attachment_not_found_tag, progress_step,
};
use t3_core::composer::attachments::{self, CREATE_UPLOAD_URL, decode_ticket};
use t3_core::media::{AssetResource, CREATE_ASSET_URL};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::watch;

use crate::session::RpcError;
use crate::supervisor::ClientHandle;

/// Default streaming chunk size (64 KiB) for `http://` body transfers so progress updates and
/// cancellation remain responsive across slow connections.
pub const DEFAULT_UPLOAD_CHUNK_SIZE: usize = 64 * 1024;

/// Failure returned by the raw HTTP byte transfer stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadTransferError {
    Cancelled,
    TimedOut,
    HttpStatus(u16),
    Network(String),
}

impl UploadTransferError {
    pub fn user_message(&self) -> String {
        match self {
            Self::Cancelled => "Upload cancelled".to_owned(),
            Self::TimedOut => "Upload timed out".to_owned(),
            Self::HttpStatus(code) => format!("Upload rejected ({code})"),
            Self::Network(_) => "Upload failed".to_owned(),
        }
    }
}

/// Best-effort `attachments.delete` for cancelled/superseded/retired pending uploads.
pub async fn delete_pending_attachment(client: &ClientHandle, attachment_id: &str) {
    let _ = client
        .request(attachments::DELETE, attachments::delete_request(attachment_id))
        .await;
}

/// Verifies whether a persisted `attachment_id` still exists on the server via
/// `assets.createUrl` (`verifyPersistedAttachmentUpload` in `client-runtime/src/state/attachments.ts`).
pub async fn verify_persisted_attachment_upload(
    client: &ClientHandle,
    attachment_id: &str,
) -> PersistedAttachmentVerification {
    let request = AssetResource::Attachment {
        attachment_id: attachment_id.to_owned(),
    }
    .create_url_request();

    match client.request(CREATE_ASSET_URL, request).await {
        Ok(_) => PersistedAttachmentVerification::Verified,
        Err(RpcError::Failure(failure)) => {
            if is_asset_attachment_not_found_tag(failure.tag.as_deref()) {
                PersistedAttachmentVerification::Missing {
                    reason: failure.message,
                }
            } else {
                PersistedAttachmentVerification::Failed {
                    reason: failure.message,
                }
            }
        }
        Err(RpcError::Closed) => PersistedAttachmentVerification::Failed {
            reason: "Connection closed while verifying attachment.".to_owned(),
        },
    }
}

/// Streams raw `bytes` as an HTTP POST body with `Content-Type: mime_type`, reporting normalized
/// `[0.0, 1.0]` progress after each chunk is flushed and honoring immediate abort via `cancel`.
pub async fn upload_bytes_with_progress<F>(
    upload_url: &str,
    mime_type: &str,
    bytes: &[u8],
    chunk_size: usize,
    timeout: Duration,
    cancel: &mut watch::Receiver<bool>,
    mut on_progress: F,
) -> Result<(), UploadTransferError>
where
    F: FnMut(f64) + Send,
{
    if *cancel.borrow() {
        return Err(UploadTransferError::Cancelled);
    }

    let parsed = url::Url::parse(upload_url)
        .map_err(|err| UploadTransferError::Network(err.to_string()))?;

    let transfer = async {
        if parsed.scheme() == "http" {
            stream_http_upload(
                &parsed,
                mime_type,
                bytes,
                chunk_size.max(1),
                cancel,
                &mut on_progress,
            )
            .await
        } else {
            reqwest_upload(upload_url, mime_type, bytes, timeout, cancel, &mut on_progress).await
        }
    };

    match tokio::time::timeout(timeout, transfer).await {
        Ok(result) => result,
        Err(_) => Err(UploadTransferError::TimedOut),
    }
}

async fn stream_http_upload<F>(
    parsed: &url::Url,
    mime_type: &str,
    bytes: &[u8],
    chunk_size: usize,
    cancel: &mut watch::Receiver<bool>,
    on_progress: &mut F,
) -> Result<(), UploadTransferError>
where
    F: FnMut(f64) + Send,
{
    let host = parsed
        .host_str()
        .ok_or_else(|| UploadTransferError::Network("Missing upload host".into()))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| UploadTransferError::Network("Missing upload port".into()))?;
    let addr = format!("{host}:{port}");
    let host_header = match parsed.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_owned(),
    };
    let path_and_query = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_owned(),
    };

    let stream = tokio::select! {
        res = TcpStream::connect(&addr) => {
            res.map_err(|err| UploadTransferError::Network(err.to_string()))?
        }
        changed = cancel.changed() => {
            let _ = changed;
            if *cancel.borrow() {
                return Err(UploadTransferError::Cancelled);
            }
            return Err(UploadTransferError::Cancelled);
        }
    };

    let (read_half, mut write_half) = stream.into_split();

    let header = format!(
        "POST {path_and_query} HTTP/1.1\r\nHost: {host_header}\r\nContent-Type: {mime_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );

    tokio::select! {
        res = write_half.write_all(header.as_bytes()) => {
            res.map_err(|err| UploadTransferError::Network(err.to_string()))?;
        }
        _ = wait_cancelled(cancel) => {
            return Err(UploadTransferError::Cancelled);
        }
    }

    on_progress(0.0);

    if bytes.is_empty() {
        on_progress(1.0);
    } else {
        let mut sent = 0usize;
        for chunk in bytes.chunks(chunk_size) {
            if *cancel.borrow() {
                return Err(UploadTransferError::Cancelled);
            }
            tokio::select! {
                res = write_half.write_all(chunk) => {
                    res.map_err(|err| UploadTransferError::Network(err.to_string()))?;
                }
                _ = wait_cancelled(cancel) => {
                    return Err(UploadTransferError::Cancelled);
                }
            }
            sent += chunk.len();
            on_progress(sent as f64 / bytes.len() as f64);
        }
    }

    tokio::select! {
        res = write_half.flush() => {
            res.map_err(|err| UploadTransferError::Network(err.to_string()))?;
        }
        _ = wait_cancelled(cancel) => {
            return Err(UploadTransferError::Cancelled);
        }
    }

    let mut reader = BufReader::new(read_half);
    let mut status_line = String::new();
    tokio::select! {
        res = reader.read_line(&mut status_line) => {
            let n = res.map_err(|err| UploadTransferError::Network(err.to_string()))?;
            if n == 0 {
                return Err(UploadTransferError::Network("Unexpected EOF reading upload response".into()));
            }
        }
        _ = wait_cancelled(cancel) => {
            return Err(UploadTransferError::Cancelled);
        }
    }

    let status_code = parse_http_status(&status_line)
        .ok_or_else(|| UploadTransferError::Network(format!("Malformed HTTP status line: {status_line}")))?;
    if !(200..300).contains(&status_code) {
        return Err(UploadTransferError::HttpStatus(status_code));
    }

    Ok(())
}

async fn reqwest_upload<F>(
    upload_url: &str,
    mime_type: &str,
    bytes: &[u8],
    timeout: Duration,
    cancel: &mut watch::Receiver<bool>,
    on_progress: &mut F,
) -> Result<(), UploadTransferError>
where
    F: FnMut(f64) + Send,
{
    let http = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|err| UploadTransferError::Network(err.to_string()))?;

    on_progress(0.0);
    let send = http
        .post(upload_url)
        .header(reqwest::header::CONTENT_TYPE, mime_type)
        .body(bytes.to_vec())
        .send();

    let response = tokio::select! {
        res = send => res.map_err(|err| {
            if err.is_timeout() {
                UploadTransferError::TimedOut
            } else {
                UploadTransferError::Network(err.to_string())
            }
        })?,
        _ = wait_cancelled(cancel) => {
            return Err(UploadTransferError::Cancelled);
        }
    };

    if !response.status().is_success() {
        return Err(UploadTransferError::HttpStatus(response.status().as_u16()));
    }
    on_progress(1.0);
    Ok(())
}

async fn wait_cancelled(cancel: &mut watch::Receiver<bool>) {
    while !*cancel.borrow() {
        if cancel.changed().await.is_err() {
            futures::future::pending::<()>().await;
        }
    }
}

fn parse_http_status(status_line: &str) -> Option<u16> {
    let mut parts = status_line.split_whitespace();
    let proto = parts.next()?;
    if !proto.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse::<u16>().ok()
}

/// Runs one upload cycle matching `runAttachmentUploadCycle` (`packages/client-runtime/src/state/attachments.ts`):
///
/// 1. Mint signed upload ticket via `attachments.createUploadUrl`.
/// 2. Call `on_minted(attachment_id)`: if cancelled before transfer starts, immediately delete the
///    minted attachment on the server and return `UploadCycleOutcome::Cancelled`.
/// 3. Resolve signed URL against `client.environment()`.
/// 4. Stream raw bytes with 5%-step progress throttling.
///    - If cancelled mid-transfer, cleans up the minted ID via `attachments.delete` and returns
///      `UploadCycleOutcome::Cancelled { minted_attachment_id: None }`.
///    - If transfer fails, preserves `attachment_id: Some(id)` in `UploadCycleOutcome::Failed`
///      WITHOUT deleting it on the server, so the caller decides whether to retry or release.
#[allow(clippy::too_many_arguments)]
pub async fn run_attachment_upload_cycle<M, P>(
    client: &ClientHandle,
    request: Value,
    mime_type: &str,
    bytes: &[u8],
    chunk_size: usize,
    timeout: Duration,
    cancel: &mut watch::Receiver<bool>,
    on_minted: M,
    mut on_progress: P,
) -> UploadCycleOutcome
where
    M: FnOnce(&str) -> bool,
    P: FnMut(f64) + Send,
{
    if *cancel.borrow() {
        return UploadCycleOutcome::Cancelled {
            minted_attachment_id: None,
        };
    }

    let reply = match client.request(CREATE_UPLOAD_URL, request).await {
        Ok(value) => value,
        Err(_) => {
            return UploadCycleOutcome::Failed {
                step: UploadCycleStep::CreateUploadUrl,
                attachment_id: None,
                error: "Upload could not start".to_owned(),
            };
        }
    };

    let Some(ticket) = decode_ticket(&reply) else {
        return UploadCycleOutcome::Failed {
            step: UploadCycleStep::CreateUploadUrl,
            attachment_id: None,
            error: "Upload could not start".to_owned(),
        };
    };

    if *cancel.borrow() || !on_minted(&ticket.attachment_id) {
        delete_pending_attachment(client, &ticket.attachment_id).await;
        return UploadCycleOutcome::Cancelled {
            minted_attachment_id: None,
        };
    }

    let Some(url) = attachments::upload_url(client.environment(), &ticket.relative_url) else {
        return UploadCycleOutcome::Failed {
            step: UploadCycleStep::ResolveUploadUrl,
            attachment_id: Some(ticket.attachment_id),
            error: "Not connected".to_owned(),
        };
    };

    let mut last_step: i32 = -1;
    let transfer = upload_bytes_with_progress(
        &url,
        mime_type,
        bytes,
        chunk_size,
        timeout,
        cancel,
        |progress| {
            let step = progress_step(progress);
            if step > last_step {
                last_step = step;
                on_progress(progress);
            }
        },
    )
    .await;

    match transfer {
        Ok(()) => UploadCycleOutcome::Uploaded {
            attachment_id: ticket.attachment_id,
        },
        Err(UploadTransferError::Cancelled) => {
            delete_pending_attachment(client, &ticket.attachment_id).await;
            UploadCycleOutcome::Cancelled {
                minted_attachment_id: None,
            }
        }
        Err(err) => UploadCycleOutcome::Failed {
            step: UploadCycleStep::UploadBytes,
            attachment_id: Some(ticket.attachment_id),
            error: err.user_message(),
        },
    }
}
