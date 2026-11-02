use super::*;
use serde_json::json;

fn staged(kind: AttachmentKind, upload: Upload) -> Attachment {
    Attachment {
        id: "a1".into(),
        kind,
        name: "shot.png".into(),
        mime_type: "image/png".into(),
        size_bytes: 2048,
        pasted_text: false,
        upload,
    }
}

#[test]
fn files_classify_by_extension() {
    assert_eq!(classify("Shot.PNG"), Classified::Image { mime_type: "image/png" });
    assert_eq!(classify("a.jpg"), Classified::Image { mime_type: "image/jpeg" });
    assert_eq!(classify("photo.heic"), Classified::UnsupportedImage);
    assert_eq!(classify("logo.svg"), Classified::File { mime_type: "image/svg+xml".into() });
    assert_eq!(classify("notes.md"), Classified::File { mime_type: "text/markdown".into() });
    assert_eq!(classify("Makefile"), Classified::File { mime_type: "application/octet-stream".into() });
    assert_eq!(classify(".png"), Classified::File { mime_type: "application/octet-stream".into() });
    assert_eq!(image_mime_for_format("Webp"), Some("image/webp"));
    assert_eq!(image_mime_for_format("tiff"), None);
}

#[test]
fn admission_follows_the_web_limits() {
    let image = classify("a.png");
    let file = classify("a.txt");
    assert_eq!(admission_error(0, "a.png", &image, MAX_IMAGE_BYTES, None), None);
    assert!(admission_error(0, "a.png", &image, MAX_IMAGE_BYTES + 1, None).unwrap().contains("too large"));
    assert_eq!(admission_error(MAX_ATTACHMENTS, "a.png", &image, 1, None).unwrap(), "You can attach up to 100 files per message.");
    assert_eq!(admission_error(0, "a.txt", &file, 10, None).unwrap(), "This server does not support file attachments.");
    assert_eq!(admission_error(0, "a.txt", &file, 0, Some(100)).unwrap(), "'a.txt' is empty or could not be read.");
    assert_eq!(admission_error(0, "a.txt", &file, 3 * 1024 * 1024, Some(2 * 1024 * 1024)).unwrap(), "'a.txt' exceeds the 2 MB attachment limit.");
    assert_eq!(admission_error(0, "a.heic", &classify("a.heic"), 1, None).unwrap(), unsupported_image_message("a.heic"));
    assert_eq!(file_staging_limit(Some(u64::MAX)), Some(MAX_FILE_BYTES));
    assert_eq!(file_staging_limit(None), None);
    assert_eq!(too_large_message("x", 1536), "'x' exceeds the 1536 bytes attachment limit.");
    assert_eq!(too_large_message("x", 2048), "'x' exceeds the 2 KB attachment limit.");
    assert_eq!(too_large_message("x", 1), "'x' exceeds the 1 byte attachment limit.");
}

#[test]
fn sizes_format_like_the_strip() {
    assert_eq!(format_size(1), "1 KB");
    assert_eq!(format_size(1025), "2 KB");
    assert_eq!(format_size(3 * 1024 * 1024 + 512 * 1024), "3.5 MB");
}

#[test]
fn uploads_request_a_ticket_and_resolve_against_the_origin() {
    let image = staged(AttachmentKind::Image, Upload::Uploading);
    assert_eq!(image.upload_request(), json!({ "name": "shot.png", "mimeType": "image/png", "sizeBytes": 2048 }));
    let file = Attachment { kind: AttachmentKind::File, ..image.clone() };
    assert_eq!(file.upload_request()["type"], "file");
    let ticket = decode_ticket(&json!({ "attachmentId": "att-1", "relativeUrl": "/api/attachments/upload/abc.def", "expiresAt": 1 })).unwrap();
    assert_eq!(ticket.attachment_id, "att-1");
    assert_eq!(upload_url("http://127.0.0.1:14658", &ticket.relative_url).as_deref(), Some("http://127.0.0.1:14658/api/attachments/upload/abc.def"));
    assert_eq!(upload_url("https://host.ts.net/base/", "/api/x").as_deref(), Some("https://host.ts.net/api/x"));
    assert_eq!(decode_ticket(&json!({ "attachmentId": "x" })), None);
    assert!(is_local_server("http://127.0.0.1:14658"));
    assert!(is_local_server("http://localhost:3000"));
    assert!(is_local_server("http://[::1]:3000"));
    assert!(!is_local_server("https://box.tail1234.ts.net"));
}

#[test]
fn sends_carry_uploaded_attachments_and_their_records() {
    let uploading = staged(AttachmentKind::Image, Upload::Uploading);
    assert_eq!(uploading.chat_attachment(), None);
    assert_eq!(chat_attachments(std::slice::from_ref(&uploading)), None);
    assert_eq!(send_block_reason(std::slice::from_ref(&uploading)).as_deref(), Some("Wait for attachments to finish uploading"));
    let failed = staged(AttachmentKind::Image, Upload::Failed { reason: "Upload failed".into() });
    assert_eq!(send_block_reason(&[failed]).as_deref(), Some("Upload failed. Remove the attachment to send."));
    let ready = staged(AttachmentKind::Image, Upload::Ready { attachment_id: "att-1".into() });
    assert_eq!(send_block_reason(std::slice::from_ref(&ready)), None);
    assert_eq!(
        chat_attachments(std::slice::from_ref(&ready)).unwrap(),
        [json!({ "type": "image", "id": "att-1", "name": "shot.png", "mimeType": "image/png", "sizeBytes": 2048 })]
    );
    let pasted = Attachment { kind: AttachmentKind::File, pasted_text: true, id: "p1".into(), ..ready.clone() };
    assert_eq!(pasted.chat_attachment().unwrap()["source"], json!({ "_tag": "pasted-text" }));
    let thread = ThreadContext::new("env", "t1", "Other");
    let text = format!("see {}", thread.reference());
    let context = message_context(&text, &[thread], std::slice::from_ref(&ready)).unwrap();
    let records = context["records"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["kind"], "thread");
    assert_eq!(records[1], json!({
        "version": 1, "kind": "image", "contextId": "image_a1", "label": "shot.png",
        "attachmentId": "att-1", "name": "shot.png", "mimeType": "image/png", "sizeBytes": 2048,
    }));
    assert_eq!(message_context("plain", &[], &[]), None);
}

#[test]
fn large_pastes_fold_into_numbered_files() {
    let small = "x".repeat(PASTED_TEXT_THRESHOLD_BYTES - 1);
    assert!(!folds_into_file(&small, false, false));
    assert!(folds_into_file(&format!("{small}x"), false, false));
    assert!(folds_into_file(&"é".repeat(PASTED_TEXT_THRESHOLD_BYTES / 2), false, false), "bytes count, not chars");
    assert!(!folds_into_file(&format!("{small}x"), true, false), "paste as text keeps it inline");
    assert!(folds_into_file("short", false, true));
    assert!(!folds_into_file("", false, true));
    assert!(paste_exceeds_limit(&"a".repeat(MAX_INPUT_CHARS), 0..0, "b"));
    assert!(!paste_exceeds_limit(&"a".repeat(MAX_INPUT_CHARS), 0..1, "b"));
    assert_eq!(next_pasted_text_name([]), "pasted-text.txt");
    assert_eq!(next_pasted_text_name(["Pasted-Text.txt", "pasted-text-2.txt"]), "pasted-text-3.txt");
}

#[test]
fn attachment_only_messages_still_ask() {
    assert_eq!(text_for_send("  ", true), ATTACHMENT_ONLY_PROMPT);
    assert_eq!(text_for_send("Ultrathink: ", true), format!("Ultrathink:\n{ATTACHMENT_ONLY_PROMPT}"));
    assert_eq!(text_for_send("look", true), "look");
    assert_eq!(text_for_send("", false), "");
}

#[test]
fn image_totals_cap_at_80_mib() {
    let big = Attachment { size_bytes: MAX_IMAGE_BYTES, ..staged(AttachmentKind::Image, Upload::Uploading) };
    assert_eq!(limit_error(&vec![big.clone(); 8]), None);
    assert!(limit_error(&vec![big; 9]).unwrap().starts_with("Images can total up to 80 MiB"));
}
