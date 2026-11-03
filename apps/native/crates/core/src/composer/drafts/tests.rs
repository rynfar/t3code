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
    let discarded = draft("original", 6);
    assert!(store.restore(&new_thread_key("p1"), discarded.clone()));
    assert!(!store.restore(&new_thread_key("p1"), draft("replacement", 7)));
    assert_eq!(store.get(&new_thread_key("p1")).unwrap().prompt, "original");
    store.remove(&new_thread_key("p1"));
    let pending = Draft { pending_attachment_count: 2, ..Draft::default() };
    assert!(store.set(&new_thread_key("upload"), pending));
    assert!(store.get(&new_thread_key("upload")).unwrap().has_user_content());
    assert!(DraftStore::from_value(Some(&store.to_value())).get(&new_thread_key("upload")).is_none(), "unuploaded bytes do not persist");
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
    let excerpt = TerminalContext {
        id: "x1".into(),
        thread_id: "t1".into(),
        created_at: "2026-11-02T00:00:00.000Z".into(),
        terminal_id: "term-1".into(),
        terminal_label: "Terminal 1".into(),
        line_start: 2,
        line_end: 3,
        text: "$ cargo test\nok".into(),
    };
    let unlinked = TerminalContext { id: "x2".into(), ..excerpt.clone() };
    let prompt = format!("see {} and {}", thread.reference(), excerpt.reference());
    let full = Draft {
        prompt: prompt.clone(),
        model_selection: Some(ModelSelection { instance_id: "codex".into(), model: "gpt-5".into(), options: None }),
        runtime_mode: Some(RuntimeMode::FullAccess),
        interaction_mode: Some(InteractionMode::Default),
        threads: Draft::saved_threads(&[thread.clone(), ThreadContext::new("env", "t3", "Unlinked")], &prompt),
        terminals: Draft::saved_terminals(&[unlinked, excerpt.clone()], &prompt),
        attachments: vec![SavedAttachment::of(&attachment).unwrap()],
        pending_attachment_count: 0,
        created_at: Some(7),
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
    assert_eq!(full.terminals, std::slice::from_ref(&excerpt), "excerpts keep their text, and only linked ones are kept");
    assert_eq!(store.to_value()["drafts"]["thread:t1"]["terminals"][0]["terminalLabel"], "Terminal 1");
    // An entry from before excerpts kept their text restores as expired; a placeholder becomes its link.
    let mut legacy = store.to_value();
    legacy["drafts"]["thread:t1"]["prompt"] = json!("look \u{FFFC}");
    legacy["drafts"]["thread:t1"]["terminals"][0].as_object_mut().unwrap().remove("text");
    let legacy = DraftStore::from_value(Some(&legacy));
    let legacy = legacy.get(&thread_key("t1")).unwrap();
    assert!(legacy.terminals[0].is_expired());
    assert_eq!(legacy.restored_prompt(), format!("look {}", excerpt.reference()));
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
