use serde_json::json;

use super::*;

#[test]
fn image_attachments_include_legacy_image_files_and_skip_annotations() {
    let attachments = vec![
        json!({ "type": "image", "id": "a", "name": "shot.png", "mimeType": "image/png", "sizeBytes": 10 }),
        json!({ "type": "file", "id": "b", "name": "old.jpg", "mimeType": "application/octet-stream", "sizeBytes": 10 }),
        json!({ "type": "file", "id": "c", "name": "notes.txt", "mimeType": "text/plain", "sizeBytes": 10 }),
        json!({ "type": "image", "id": "d", "name": "preview-annotation-1.png", "mimeType": "image/png", "sizeBytes": 10 }),
        json!({ "type": "sticker", "id": "e", "name": "cat.png", "mimeType": "image/png", "sizeBytes": 10 }),
    ];
    let ids: Vec<_> = image_attachments(&attachments).into_iter().map(|image| image.id).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(other_attachments(&attachments), [FileAttachment { name: "notes.txt".into(), size_bytes: Some(10) }, FileAttachment { name: "cat.png".into(), size_bytes: Some(10) }]);
    let future = vec![json!({"type":"future", "name":"new.bin", "sizeBytes":2049, "futureField":true}), json!({"name":"old.bin"}), json!({"name":"invalid.bin", "sizeBytes":-1})];
    assert_eq!(other_attachments(&future).iter().map(|file| file.size_bytes).collect::<Vec<_>>(), [Some(2049), None, None]);
    assert_eq!(attachment_size_label(&attachments[0], &json!({"downloadable":false})).as_deref(), Some("1 KB"));
    assert_eq!(attachment_size_label(&attachments[2], &json!({"downloadable":false})).as_deref(), Some("Sending…"));
    assert_eq!(attachment_size_label(&attachments[2], &json!({})).as_deref(), Some("1 KB"));
    assert_eq!(attachment_size_label(&json!({"kind":"file"}), &json!({})), None);
    assert_eq!(image_attachments(&attachments)[1].mime_type, "image/jpeg");
}

#[test]
fn placement_follows_the_web_rules() {
    assert_eq!(media_placement(0, true, 0, false), None);
    assert_eq!(media_placement(1, true, 0, false), Some(MediaPlacement::Alone));
    assert_eq!(media_placement(2, false, 0, false), Some(MediaPlacement::AboveText));
    assert_eq!(media_placement(1, true, 1, false), Some(MediaPlacement::AboveText));
    assert_eq!(media_placement(1, true, 0, true), Some(MediaPlacement::InBubble));
}

#[test]
fn classifies_markdown_image_sources() {
    let root = Some("/repo");
    assert_eq!(classify_markdown_image_source("https://x.dev/a.png", root), MarkdownImageSource::Direct("https://x.dev/a.png".into()));
    assert_eq!(classify_markdown_image_source("<docs/a b.png>", root), MarkdownImageSource::WorkspaceFile("/repo/docs/a b.png".into()));
    assert_eq!(classify_markdown_image_source("docs/a%20b.png", root), MarkdownImageSource::WorkspaceFile("/repo/docs/a b.png".into()));
    assert_eq!(classify_markdown_image_source("/tmp/shot.png?x=1#y", root), MarkdownImageSource::WorkspaceFile("/tmp/shot.png".into()));
    assert_eq!(classify_markdown_image_source("file:///tmp/a%23b.png", root), MarkdownImageSource::WorkspaceFile("/tmp/a#b.png".into()));
    assert_eq!(classify_markdown_image_source("file:///C:/shots/a.png", root), MarkdownImageSource::WorkspaceFile("C:/shots/a.png".into()));
    assert_eq!(classify_markdown_image_source("docs/a.png", None), MarkdownImageSource::Blocked);
    assert_eq!(classify_markdown_image_source("~/a.png", root), MarkdownImageSource::Blocked);
    assert_eq!(classify_markdown_image_source("javascript:alert(1)", root), MarkdownImageSource::Blocked);
    assert_eq!(classify_markdown_image_source("#frag", root), MarkdownImageSource::Blocked);
    assert_eq!(classify_markdown_image_source("//server/share/a.png", root), MarkdownImageSource::Direct("//server/share/a.png".into()));
    assert_eq!(classify_markdown_image_source("\\\\server\\share\\a.png", root), MarkdownImageSource::Blocked);
    assert_eq!(
        classify_markdown_image_source("\\\\server\\share\\a.png", Some("\\\\server\\share")),
        MarkdownImageSource::WorkspaceFile("\\\\server\\share\\a.png".into())
    );
    assert_eq!(classify_markdown_image_source("img\\a.png", Some("C:\\repo")), MarkdownImageSource::WorkspaceFile("C:\\repo\\img\\a.png".into()));
}

#[test]
fn resolves_media_sources_to_assets_or_direct_urls() {
    let resolved = resolve_media_source("shots/a.png", Some("t1"), Some("/repo"), true).unwrap();
    assert_eq!(resolved.kind, MediaKind::Image);
    assert_eq!(resolved.name, "a.png");
    assert_eq!(resolved.source, Some(MediaSource::Asset(AssetResource::MediaFile { thread_id: "t1".into(), path: "/repo/shots/a.png".into() })));
    assert_eq!(resolve_media_source("/tmp/a.png", None, None, true).unwrap().source, None);
    assert_eq!(resolve_media_source("//cdn.dev/a.png", None, None, true).unwrap().source, Some(MediaSource::Direct("https://cdn.dev/a.png".into())));
    assert_eq!(resolve_media_source("https://x.dev/clip.mp4", None, None, true).unwrap().kind, MediaKind::Video);
    assert_eq!(resolve_media_source("https://x.dev/song.mp3", None, None, true), None);
    assert_eq!(resolve_media_source("https://x.dev/render", None, None, true).unwrap().name, "render");
    assert_eq!(resolve_media_source("https://x.dev/render", None, None, false), None);
    let data = resolve_media_source("data:image/png;base64,AAAA", None, None, true).unwrap();
    assert_eq!((data.kind, data.name.as_str()), (MediaKind::Image, "image"));
}

#[test]
fn decodes_data_urls_and_asset_results() {
    assert_eq!(decode_data_url("data:image/png;base64,aGVsbG8="), Some(b"hello".to_vec()));
    assert_eq!(decode_data_url("data:image/svg+xml,%3Csvg%3E"), Some(b"<svg>".to_vec()));
    assert_eq!(decode_data_url("data:image/png;base64,***"), None);
    let url = decode_asset_url(&json!({ "relativeUrl": "/api/assets/tok/a.png", "expiresAt": 1, "imageDimensions": { "width": 640, "height": 480 } })).unwrap();
    assert_eq!(url, AssetUrl { relative_url: "/api/assets/tok/a.png".into(), dimensions: Some((640, 480)), expires_at: 1., file_revision: None });
    let revisioned = decode_asset_url(&json!({ "relativeUrl": "/a", "expiresAt": 1234, "fileRevision": "inode-size-mtime", "future": {} })).unwrap();
    assert_eq!(revisioned.file_revision.as_deref(), Some("inode-size-mtime"));
    assert_eq!(revisioned.expires_at, 1234.);
    let resigned = decode_asset_url(&json!({ "relativeUrl": "/signed-again", "expiresAt": 9999, "fileRevision": "inode-size-mtime" })).unwrap();
    assert_eq!(revisioned.content_key(), resigned.content_key());
    assert_ne!(url.content_key(), resigned.content_key());
    let mut changed = resigned.clone();
    changed.file_revision = Some("changed".into());
    assert_ne!(changed.content_key(), resigned.content_key());
    for file_revision in [json!(null), json!(42), json!(""), json!("x".repeat(257)), json!("😀".repeat(129))] {
        let url = decode_asset_url(&json!({ "relativeUrl": "/a", "fileRevision": file_revision })).unwrap();
        assert_eq!(url.file_revision, None);
        assert_eq!(url.expires_at, 0.);
    }
    assert!(decode_asset_url(&json!({ "expiresAt": 1 })).is_err());
    assert_eq!(
        AssetResource::Attachment { attachment_id: "x".into() }.create_url_request(),
        json!({ "resource": { "_tag": "attachment", "attachmentId": "x" } })
    );
}

#[test]
fn fits_images_within_an_edge() {
    assert_eq!(fit_within(4000, 3000, 1000), (1000, 750));
    assert_eq!(fit_within(300, 9000, 1200), (40, 1200));
    assert_eq!(fit_within(800, 600, 1000), (800, 600));
    assert_eq!(fit_within(10, 100_000, 100), (1, 100));
}

#[test]
fn percent_decoding_keeps_invalid_input() {
    assert_eq!(percent_decode("a%20b"), "a b");
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%E2%82%AC"), "€");
    assert_eq!(percent_decode("%FF"), "%FF");
}
