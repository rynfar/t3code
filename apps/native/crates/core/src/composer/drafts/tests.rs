use super::*;
use serde_json::json;

fn draft(prompt: &str, at: i64) -> Draft {
    Draft { prompt: prompt.into(), updated_at: at, ..Draft::default() }
}

#[test]
fn empty_drafts_are_dropped_and_unchanged_ones_not_rewritten() {
    let mut store = DraftStore::default();
    assert!(store.set(&thread_key("t1"), draft("hello", 1)));
    assert!(!store.set(&thread_key("t1"), draft("hello", 2)), "only the timestamp moved");
    assert!(store.set(&thread_key("t1"), draft("hello there", 3)));
    assert!(store.set(&thread_key("t1"), draft("  ", 4)), "clearing the prompt drops the draft");
    assert!(store.get(&thread_key("t1")).is_none());
    assert!(!store.set(&thread_key("t1"), draft("", 5)));
    let choice = Draft { interaction_mode: Some(InteractionMode::Plan), ..Draft::default() };
    assert!(store.set(&new_thread_key("p1"), choice), "a choice alone is worth keeping");
}

#[test]
fn the_store_round_trips_and_skips_bad_entries() {
    let mut store = DraftStore::default();
    let attachment = Attachment {
        id: "a1".into(),
        kind: AttachmentKind::File,
        name: "notes.md".into(),
        mime_type: "text/markdown".into(),
        size_bytes: 12,
        pasted_text: true,
        upload: Upload::Ready { attachment_id: "att-1".into() },
    };
    let thread = ThreadContext::new("env", "t2", "Other");
    let prompt = format!("see {}", thread.reference());
    let full = Draft {
        prompt: prompt.clone(),
        model_selection: Some(ModelSelection { instance_id: "codex".into(), model: "gpt-5".into(), options: None }),
        runtime_mode: Some(RuntimeMode::FullAccess),
        interaction_mode: Some(InteractionMode::Default),
        threads: Draft::saved_threads(&[thread.clone(), ThreadContext::new("env", "t3", "Unlinked")], &prompt),
        attachments: vec![SavedAttachment::of(&attachment).unwrap()],
        updated_at: 7,
    };
    store.set(&thread_key("t1"), full.clone());
    let mut value = store.to_value();
    value["drafts"]["thread:bad"] = json!({ "prompt": 42 });
    let restored = DraftStore::from_value(Some(&value));
    assert_eq!(restored.get(&thread_key("t1")), Some(&full));
    assert_eq!(restored.keys().count(), 1);
    assert_eq!(full.threads.len(), 1, "only threads the prompt still links are kept");
    assert_eq!(full.thread_contexts(), [thread]);
    assert_eq!(full.attachments[0].restore(), attachment);
    let uploading = Attachment { upload: Upload::Uploading, ..attachment };
    assert_eq!(SavedAttachment::of(&uploading), None, "bytes do not survive a restart");
    assert_eq!(DraftStore::from_value(Some(&json!({ "version": 99, "drafts": {} }))), DraftStore::default());
    assert_eq!(DraftStore::from_value(None), DraftStore::default());
}

#[test]
fn the_oldest_drafts_go_first() {
    let mut store = DraftStore::default();
    for index in 0..=MAX_DRAFTS {
        store.set(&thread_key(&index.to_string()), draft("x", index as i64 + 1));
    }
    assert_eq!(store.keys().count(), MAX_DRAFTS);
    assert!(store.get(&thread_key("0")).is_none());
    assert!(store.get(&thread_key(&MAX_DRAFTS.to_string())).is_some());
}
