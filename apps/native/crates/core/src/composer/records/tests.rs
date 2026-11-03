use super::*;

#[test]
fn environment_record_and_send_keep_exact_reference_and_record_order() {
    let machine = environment_record("env-a", "Build [Mac]\n mini");
    assert_eq!(
        reference(&machine).unwrap(),
        "[Build Mac mini](t3-context://v1/environment/environment_env-a)"
    );
    let thread =
        super::super::menu::ThreadContext::new("env-a", "thread-a", "Fix chips").to_record();
    let prompt = format!(
        "Compare {} with {}",
        reference(&machine).unwrap(),
        reference(&thread).unwrap()
    );
    let context =
        message_context(&prompt, &[machine.clone(), thread.clone(), machine.clone()]).unwrap();
    assert_eq!(
        prompt,
        "Compare [Build Mac mini](t3-context://v1/environment/environment_env-a) with [Fix chips](t3-context://v1/thread/thread_thread-a)"
    );
    assert_eq!(context["records"], json!([thread.clone(), machine.clone()]));
    let repeated = format!(
        "{prompt} {} [Missing](t3-context://v1/terminal/missing)",
        reference(&machine).unwrap()
    );
    assert_eq!(
        presentation_records(&repeated, &[thread, machine])
            .iter()
            .map(|r| field(r, "kind"))
            .collect::<Vec<_>>(),
        ["environment", "thread", "unresolved"]
    );
    assert_eq!(
        display_text(&prompt),
        "Compare Build Mac mini with Fix chips"
    );
}

#[test]
fn clipboard_selects_linked_records_and_remints_payload_collisions() {
    let terminal = json!({"version":1,"kind":"terminal","contextId":"terminal_a","label":"Terminal 1 line 1","terminalId":"one","terminalLabel":"Terminal 1","lineStart":1,"lineEnd":1,"text":"old"});
    let mut changed = terminal.clone();
    changed["text"] = json!("new");
    let markdown = reference(&terminal).unwrap();
    let fragment = copy_fragment(
        &markdown,
        &[changed.clone(), environment_record("unselected", "Other")],
        "env-a",
        Some("thread-a"),
        Some("message-a"),
    )
    .unwrap();
    assert_eq!(
        decode_fragment(&fragment).unwrap()["records"],
        json!([changed.clone()])
    );
    let mut held = vec![terminal.clone()];
    let pasted =
        import_fragment(&markdown, &fragment, &mut held, "env-a", || "fresh".into()).unwrap();
    assert_eq!(
        pasted,
        "[Terminal 1 line 1](t3-context://v1/terminal/terminal_fresh)"
    );
    assert_eq!(held.len(), 2);
    assert_eq!(held[1]["text"], "new");
    let repeated = import_fragment(
        &pasted,
        &copy_fragment(&pasted, &held, "env-a", None, None).unwrap(),
        &mut held,
        "env-a",
        || panic!("same payload must dedup"),
    )
    .unwrap();
    assert_eq!(pasted, repeated);
    assert_eq!(held.len(), 2);
    assert!(decode_fragment("{\"version\":2}").is_none());
    assert_eq!(
        decode_fragment("{\"version\":1,\"source\":{\"environmentId\":\"x\"},\"records\":[{}]}")
            .unwrap()["records"],
        json!([])
    );
}

#[test]
fn clipboard_does_not_import_unselected_or_foreign_threads_or_foreign_attachments() {
    let thread = super::super::menu::ThreadContext::new("foreign", "a", "Remote").to_record();
    let file = json!({"version":1,"kind":"file","contextId":"file_a","label":"a","attachmentId":"a","name":"a","mimeType":"text/plain","sizeBytes":1});
    let prompt = format!(
        "{} {}",
        reference(&thread).unwrap(),
        reference(&file).unwrap()
    );
    let fragment = copy_fragment(&prompt, &[thread, file], "foreign", None, None).unwrap();
    let mut held = Vec::new();
    assert_eq!(
        import_fragment(&prompt, &fragment, &mut held, "local", || "fresh".into()).unwrap(),
        prompt
    );
    assert!(held.is_empty());
}

#[test]
fn machine_search_matches_web_and_excludes_current_machine() {
    let machines = [("a", "Current Mac"), ("b", "Build Mac"), ("c", "Linux")];
    assert!(environment_items(machines.into_iter(), "a", " ").is_empty());
    let found = environment_items(machines.into_iter(), "a", " MAC ");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].label, "Build Mac");
    assert_eq!(found[0].description, "Machine");
}

#[test]
fn all_portable_kinds_survive_copy_paste_and_send_without_changing_prompt_text() {
    let records = vec![
        json!({"version":1,"kind":"terminal","contextId":"terminal_a","label":"Terminal 1 line 1","terminalId":"one","terminalLabel":"Terminal 1","lineStart":1,"lineEnd":1,"text":"$ cargo test\nok"}),
        json!({"version":1,"kind":"review-comment","contextId":"review-comment_a","label":"main.rs L1","sectionId":"diff","sectionTitle":"Changes","filePath":"main.rs","startIndex":0,"endIndex":0,"rangeLabel":"+1","text":"Use this","diff":"+ok"}),
        json!({"version":1,"kind":"review-comment","contextId":"review-comment_pr","label":"#77","sectionId":"pull-request:77","sectionTitle":"PR","filePath":"PR #77","startIndex":0,"endIndex":0,"rangeLabel":"","text":"Fix chips","diff":"","pullRequest":{"number":77,"title":"Fix context chips","url":"https://github.com/rynfar/t3code/pull/77","state":"open","isDraft":true,"headBranch":"chips","baseBranch":"native"}}),
        super::super::menu::ThreadContext::new("env", "a", "Thread").to_record(),
        environment_record("other", "Build Mac"),
        json!({"version":1,"kind":"preview-annotation","contextId":"preview-annotation_a","label":"Button","annotationId":"a","pageUrl":"http://localhost","pageTitle":"App","comment":"Fix button","targetSummary":"1 selected element","styleChanges":[]}),
        json!({"version":1,"kind":"image","contextId":"image_a","label":"shot.png","attachmentId":"image-server","name":"shot.png","mimeType":"image/png","sizeBytes":8}),
        json!({"version":1,"kind":"file","contextId":"file_a","label":"notes.txt","attachmentId":"file-server","name":"notes.txt","mimeType":"text/plain","sizeBytes":8}),
    ];
    let prompt = records
        .iter()
        .map(|r| reference(r).unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    let fragment =
        copy_fragment(&prompt, &records, "env", Some("thread"), Some("message")).unwrap();
    let mut held = Vec::new();
    let text = import_fragment(&prompt, &fragment, &mut held, "env", || {
        panic!("no collisions")
    })
    .unwrap();
    assert_eq!(text, prompt);
    assert_eq!(held.len(), records.len());
    for record in &records {
        assert!(held.contains(record));
    }
    assert_eq!(pr_state(&records[2]), "draft");
    assert_eq!(label(&records[2]), "#77");
    assert_eq!(
        terminal_from_record(&records[0], "destination", "now")
            .unwrap()
            .record(),
        records[0]
    );
    assert_eq!(
        attachment_from_record(&records[6])
            .unwrap()
            .context_record()
            .unwrap(),
        records[6]
    );
    let context = message_context(&text, &held).unwrap();
    let model = crate::model::ModelSelection {
        instance_id: "devReplay".into(),
        model: "showcase".into(),
        options: None,
    };
    let command = crate::commands::composer_message(&crate::commands::ComposerMessage {
        command_id: "command",
        thread_id: "thread",
        message_id: "message",
        text: &text,
        title_seed: "Context",
        model_selection: &model,
        dispatch: crate::commands::ComposerDispatch::Auto,
        context: Some(&context),
        attachments: &[],
    });
    assert_eq!(
        command["text"],
        "[Terminal 1 line 1](t3-context://v1/terminal/terminal_a) [main.rs L1](t3-context://v1/review-comment/review-comment_a) [#77](t3-context://v1/review-comment/review-comment_pr) [Thread](t3-context://v1/thread/thread_a) [Build Mac](t3-context://v1/environment/environment_other) [Button](t3-context://v1/preview-annotation/preview-annotation_a) ![shot.png](t3-context://v1/image/image_a) [notes.txt](t3-context://v1/file/file_a)"
    );
    assert_eq!(command["context"]["records"], json!(records));
}

#[test]
fn clipboard_html_round_trips_web_fragments_without_losing_unicode_or_quotes() {
    let record = environment_record("env", "Trevor's Mac & 测试");
    let text = reference(&record).unwrap();
    let fragment = copy_fragment(&text, &[record], "env", None, None).unwrap();
    let html = clipboard_html(&text, &fragment);
    assert_eq!(fragment_from_html(&html), Some(fragment.clone()));
    assert_eq!(
        fragment_from_html(
            &html
                .replace("data-t3-context-fragment=\"", "data-t3-context-fragment='")
                .replacen("\">", "'>", 1)
                .replace("Trevor's", "Trevor%27s")
        ),
        Some(fragment)
    );
    assert!(fragment_from_html("<div data-t3-context-fragment=\"%zz\"></div>").is_none());
    assert!(fragment_from_html("<p>plain</p>").is_none());
}

#[test]
fn clipboard_keeps_annotation_screenshots_and_rewrites_colliding_bindings() {
    let annotation = json!({"version":1,"kind":"preview-annotation","contextId":"preview-annotation_a","label":"Button","annotationId":"a","pageUrl":"http://localhost","pageTitle":"App","comment":"Fix button","targetSummary":"","styleChanges":[],"screenshotContextId":"image_a"});
    let image = json!({"version":1,"kind":"image","contextId":"image_a","label":"shot.png","attachmentId":"old","name":"shot.png","mimeType":"image/png","sizeBytes":8});
    let text = reference(&annotation).unwrap();
    let mut changed = annotation.clone();
    changed["comment"] = json!("Change button");
    let mut changed_image = image.clone();
    changed_image["attachmentId"] = json!("new");
    let fragment = copy_fragment(&text, &[changed, changed_image], "env", None, None).unwrap();
    assert_eq!(
        decode_fragment(&fragment).unwrap()["records"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let mut held = vec![annotation, image];
    let pasted = import_fragment(&text, &fragment, &mut held, "env", || "fresh".into()).unwrap();
    assert_eq!(
        pasted,
        "[Button](t3-context://v1/preview-annotation/preview-annotation_fresh)"
    );
    assert_eq!(held.len(), 4);
    let pasted_annotation = held
        .iter()
        .find(|r| field(r, "contextId") == "preview-annotation_fresh")
        .unwrap();
    assert_eq!(pasted_annotation["screenshotContextId"], "image_fresh");
    assert_eq!(pasted_annotation["annotationId"], "fresh");
}
