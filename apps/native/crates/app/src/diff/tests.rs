use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::{AppContext as _, Entity, IntoElement as _, VisualTestContext, point, px, size};
use t3_core::composer::review::{CommentDictationState, Selection, Side};
use t3_core::diff::{
    CompletionOptions, CompletionScope, CompletionSupport, DiffFileContents, ExpandDirection,
    FileRevealTarget, FileViewedState, InlineReviewComment, InlineReviewThread, MentionCandidate,
    ReferenceCandidate, ReferenceKind, ReferenceSigil, ReferenceState, ReviewActor,
};

use super::{DiffPanel, External, Parsed, ViewedTarget};
use crate::app_state::AppState;

const SAMPLE_PATCH: &str = "\
diff --git a/src/alpha.rs b/src/alpha.rs
index 1111111..2222222 100644
--- a/src/alpha.rs
+++ b/src/alpha.rs
@@ -1,4 +1,5 @@
 fn one() {}
+fn two() {}
 fn three() {}
 fn four() {}
 fn five() {}
";

fn big_patch(prefix: &str, files: usize, lines_per_file: usize) -> String {
    let mut out = String::new();
    for f in 0..files {
        out.push_str(&format!(
            "diff --git a/{prefix}_{f}.rs b/{prefix}_{f}.rs\n\
             index 1111111..2222222 100644\n\
             --- a/{prefix}_{f}.rs\n\
             +++ b/{prefix}_{f}.rs\n\
             @@ -1,{lines_per_file} +1,{lines_per_file} @@\n"
        ));
        for l in 1..=lines_per_file {
            out.push_str(&format!("+fn {prefix}_{f}_{l}() {{}}\n"));
        }
    }
    out
}

fn setup_panel(
    cx: &mut gpui::TestAppContext,
) -> (tokio::runtime::Runtime, Entity<DiffPanel>, VisualTestContext) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let state = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        crate::settings::store::init(cx);
        cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
    });
    let (panel, window_cx) = cx.add_window_view(|_, cx| DiffPanel::detached(state.clone(), cx));
    (runtime, panel, window_cx.clone())
}

#[gpui::test]
fn per_scope_scroll_restores_across_switches_and_waits_for_settled_patch(
    cx: &mut gpui::TestAppContext,
) {
    let (_runtime, panel, mut cx) = setup_panel(cx);
    let patch_a = Parsed::new(&big_patch("alpha", 1, 80), false, None, None, None);
    let patch_b = Parsed::new(&big_patch("beta", 1, 80), false, None, None, None);
    let patch_a_small = Parsed::new(&big_patch("alpha", 1, 10), false, None, None, None);

    // Open scope A with file unfolded and draw so ListState lays out all 80 lines.
    panel.update(&mut cx, |panel, cx| {
        panel.show_external(
            External { key: "scope:a".into(), parsed: Some(patch_a.clone()), pending: false, error: None },
            cx,
        );
        panel.set_folded(0, false, cx);
    });
    cx.draw(point(px(0.), px(0.)), size(px(800.), px(240.)), |_, _| panel.clone().into_any_element());

    // Simulate user scrolling down on scope A and record scroll position.
    panel.update(&mut cx, |panel, _cx| {
        panel.list.set_offset_from_scrollbar(point(px(0.), px(-280.)));
        panel.on_list_scrolled();
    });
    let recorded = panel.read_with(&cx, |panel, _| panel.scroll_restore.saved_offset("scope:a"));
    assert!(recorded.is_some_and(|y| y > 200.0), "expected saved scroll for scope:a, got {recorded:?}");

    // Switch to scope B.
    panel.update(&mut cx, |panel, cx| {
        panel.show_external(
            External { key: "scope:b".into(), parsed: Some(patch_b), pending: false, error: None },
            cx,
        );
        panel.set_folded(0, false, cx);
    });
    cx.draw(point(px(0.), px(0.)), size(px(800.), px(240.)), |_, _| panel.clone().into_any_element());

    // Switch back to scope A while still pending with shorter partial patch -> stays pending.
    panel.update(&mut cx, |panel, cx| {
        panel.show_external(
            External {
                key: "scope:a".into(),
                parsed: Some(patch_a_small),
                pending: true,
                error: None,
            },
            cx,
        );
    });
    cx.draw(point(px(0.), px(0.)), size(px(800.), px(240.)), |_, _| panel.clone().into_any_element());
    assert!(panel.read_with(&cx, |panel, _| panel.scroll_restore.has_pending()));

    // Full patch A settles -> restores saved scroll offset.
    panel.update(&mut cx, |panel, cx| {
        panel.show_external(
            External { key: "scope:a".into(), parsed: Some(patch_a), pending: false, error: None },
            cx,
        );
    });
    cx.draw(point(px(0.), px(0.)), size(px(800.), px(240.)), |_, _| panel.clone().into_any_element());
    cx.run_until_parked();
    cx.draw(point(px(0.), px(0.)), size(px(800.), px(240.)), |_, _| panel.clone().into_any_element());

    let restored = panel.read_with(&cx, |panel, _| {
        f32::from(-panel.list.scroll_px_offset_for_scrollbar().y)
    });
    assert!((restored - recorded.unwrap()).abs() < 2.0, "expected ~{recorded:?}, got {restored}");

    // First manual scroll after programmatic restore must not be swallowed.
    panel.update(&mut cx, |panel, _| {
        panel.list.set_offset_from_scrollbar(point(px(0.), px(-95.)));
        panel.on_list_scrolled();
        assert_eq!(panel.scroll_restore.saved_offset("scope:a"), Some(95.0));
    });
}

#[gpui::test]
fn line_reveal_unfolds_target_scrolls_and_user_interaction_cancels_pending_reveal(
    cx: &mut gpui::TestAppContext,
) {
    let (_runtime, panel, mut cx) = setup_panel(cx);
    let patch = Parsed::new(&big_patch("file", 2, 40), false, None, None, None);

    panel.update(&mut cx, |panel, cx| {
        panel.show_external(
            External { key: "scope:reveal".into(), parsed: Some(patch), pending: false, error: None },
            cx,
        );
        panel.set_folded(1, true, cx);
    });
    assert!(panel.read_with(&cx, |panel, _| panel.is_collapsed(1)));
    cx.draw(point(px(0.), px(0.)), size(px(800.), px(240.)), |_, _| panel.clone().into_any_element());

    panel.update(&mut cx, |panel, cx| {
        panel.reveal_file_path(1, FileRevealTarget::line("file_1.rs", 25), cx);
    });

    assert!(!panel.read_with(&cx, |panel, _| panel.is_collapsed(1)));
    assert_eq!(
        panel.read_with(&cx, |panel, _| panel.selected_file.as_deref().map(str::to_owned)),
        Some("file_1.rs".to_owned())
    );
    assert_eq!(
        panel.read_with(&cx, |panel, _| panel.file_reveal.highlight_for("file_1.rs")),
        Some((25, 25, None))
    );
    panel.read_with(&cx, |panel, _| {
        let view = panel.view.as_ref().expect("view");
        let base = view.header_row(1).expect("header row");
        let local_row = t3_core::diff::find_selection_row(
            view.file_rows_slice(1).expect("file rows"),
            0,
            &view.parsed.files()[1],
            25,
            None,
        )
        .expect("target row");
        let target_row = base + local_row;
        // Centered below the sticky header, never scrolled with target_row pinned to row 0.
        assert!(panel.list.logical_scroll_top().item_ix < target_row);
        let deletion_line = t3_core::diff::DiffLine {
            kind: t3_core::diff::LineKind::Deletion,
            old_line: Some(25),
            new_line: None,
            text: Arc::from("old"),
            source_text: Arc::from("old"),
            no_newline_at_eof: false,
        };
        assert!(panel.is_revealed_line(1, &deletion_line, None));
    });

    // Pending reveal and pending scroll restore both cancel when user interacts.
    panel.update(&mut cx, |panel, cx| {
        panel.external = Some(External {
            key: "scope:reveal".into(),
            parsed: panel.external.as_ref().and_then(|e| e.parsed.clone()),
            pending: true,
            error: None,
        });
        panel.reveal_file_path(2, FileRevealTarget::line("file_missing.rs", 10), cx);
        assert!(panel.file_reveal.has_pending());
        panel.cancel_navigation_on_interaction();
        assert!(!panel.file_reveal.has_pending());
        assert!(!panel.scroll_restore.has_pending());
    });
}

#[gpui::test]
fn comment_completion_and_dictation_hooks_work_end_to_end(cx: &mut gpui::TestAppContext) {
    let (_runtime, panel, mut cx) = setup_panel(cx);
    let patch = Parsed::new(SAMPLE_PATCH, false, None, None, None);
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let submitted_log = submitted.clone();

    panel.update(&mut cx, |panel, cx| {
        panel.set_external_comment_handler(
            Some(Rc::new(move |comment, _| {
                submitted_log.lock().unwrap().push(comment);
            })),
            cx,
        );
        panel.set_completion_scope(
            Some(CompletionScope {
                scope_key: "pr:1".into(),
                project_id: "proj-1".into(),
                support: CompletionSupport {
                    mentions: true,
                    reference_sigils: vec![ReferenceSigil::Hash, ReferenceSigil::Bang],
                },
            }),
            cx,
        );
        panel.set_completion_options(
            CompletionOptions {
                authors: vec![
                    MentionCandidate {
                        login: "alice".into(),
                        name: Some("Alice".into()),
                        is_bot: false,
                        avatar_url: None,
                    },
                    MentionCandidate {
                        login: "bob".into(),
                        name: Some("Bob".into()),
                        is_bot: false,
                        avatar_url: None,
                    },
                ],
                references: vec![ReferenceCandidate {
                    reference: "#42".into(),
                    kind: ReferenceKind::ChangeRequest,
                    number: 42,
                    title: "Refactor diff".into(),
                    state: ReferenceState::Open,
                }],
                truncated: false,
            },
            cx,
        );
        panel.show_external(
            External { key: "pr:1".into(), parsed: Some(patch), pending: false, error: None },
            cx,
        );
    });

    cx.update(|window, cx| {
        panel.update(cx, |panel, cx| {
            panel.open_comment(
                0,
                Selection { start: 2, side: Side::Additions, end: 2, end_side: Side::Additions },
                window,
                cx,
            );
            assert!(panel.commit_comment_draft("Ask @al".into(), Some(7), window, cx));
            let items = panel.active_completion_items();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].insert_text(), "@alice");
            assert!(panel.accept_completion(None, window, cx));
            assert_eq!(panel.read_comment_draft(cx).as_deref(), Some("Ask @alice "));

            // Dictation inserts at caret, suppresses completion, and blocks submit while active.
            panel.set_comment_dictation_state(CommentDictationState::Active, cx);
            assert!(panel.insert_comment_text("about #42", window, cx));
            assert_eq!(panel.read_comment_draft(cx).as_deref(), Some("Ask @alice about #42"));
            assert!(panel.active_completion_items().is_empty());
            panel.submit_comment(cx);
            assert!(submitted.lock().unwrap().is_empty());

            // Clearing dictation restores completion on #42, and Escape + editing can reopen it.
            panel.set_comment_dictation_state(CommentDictationState::Idle, cx);
            let ref_items = panel.active_completion_items();
            assert_eq!(ref_items.len(), 1);
            assert_eq!(ref_items[0].insert_text(), "#42");
            assert!(panel.comments.completion.dismiss());
            assert!(panel.active_completion_items().is_empty());
            assert!(panel.commit_comment_draft("Ask @alice about #4".into(), Some(19), window, cx));
            assert_eq!(panel.active_completion_items().len(), 1);
            assert!(panel.commit_comment_draft("Ask @alice about #42".into(), Some(20), window, cx));
            panel.submit_comment(cx);
        });
    });

    let captured = submitted.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].text, "Ask @alice about #42".into());
    assert_eq!(captured[0].file_path, "src/alpha.rs");
}

#[gpui::test]
fn hunk_expansion_viewed_tracking_and_resolved_review_avatars_work_in_diff_panel(
    cx: &mut gpui::TestAppContext,
) {
    let (_runtime, panel, mut cx) = setup_panel(cx);
    let patch_text = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..aaaaaaa 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -5,3 +5,3 @@
 line 5
-line 6 old
+line 6 new
 line 7
diff --git a/src/b.rs b/src/b.rs
index 3333333..bbbbbbb 100644
--- a/src/b.rs
+++ b/src/b.rs
@@ -1,2 +1,2 @@
-old b
+new b
 keep b
";
    let parsed = Parsed::new(patch_text, false, None, None, None);

    panel.update(&mut cx, |panel, cx| {
        panel.show_external(
            External { key: "pr:77".into(), parsed: Some(parsed), pending: false, error: None },
            cx,
        );
        panel.set_folded(0, false, cx);
        panel.set_folded(1, false, cx);
        panel.set_viewed_target(
            Some(ViewedTarget {
                project_id: "proj-1".into(),
                number: 77,
                host: Some("github.com".into()),
                repository: Some("org/repo".into()),
                commit: None,
                enabled: true,
            }),
            HashMap::from([("src/a.rs".into(), FileViewedState::Stale)]),
            cx,
        );
        panel.set_review_threads(
            vec![InlineReviewThread {
                id: "rt-1".into(),
                path: "src/a.rs".into(),
                side: Side::Additions,
                line: 6,
                is_resolved: true,
                comments: vec![InlineReviewComment {
                    id: "c-1".into(),
                    author: Some(ReviewActor { login: "alice".into(), avatar_url: None }),
                    body: "Looks good now.".into(),
                    created_at: None,
                }],
            }],
            cx,
        );
    });

    // Draw once with stale viewed badge & folded resolved avatar cluster.
    cx.draw(point(px(0.), px(0.)), size(px(800.), px(320.)), |_, _| panel.clone().into_any_element());

    panel.update(&mut cx, |panel, cx| {
        // 1. Hunk expansion splices context above hunk 0 and reveals trailing hidden lines.
        let before_rows = panel.view.as_ref().unwrap().row_count();
        let file = panel.view.as_ref().unwrap().parsed.files()[0].clone();
        let file_key = panel.view.as_ref().unwrap().key(0).unwrap().to_owned();
        let old_content = (1..=10)
            .map(|i| if i == 6 { "line 6 old".into() } else { format!("line {i}") })
            .collect::<Vec<_>>()
            .join("\n");
        let new_content = (1..=10)
            .map(|i| if i == 6 { "line 6 new".into() } else { format!("line {i}") })
            .collect::<Vec<_>>()
            .join("\n");
        panel.apply_hunk_expansion(
            0,
            &file_key,
            &file,
            &DiffFileContents {
                old_contents: old_content,
                new_contents: new_content,
            },
            0,
            ExpandDirection::Up,
            2,
            cx,
        );
        let after_rows = panel.view.as_ref().unwrap().row_count();
        assert!(after_rows > before_rows, "expected expanded context rows ({before_rows} -> {after_rows})");

        // 2. Resolved review avatar toggles inline conversation visibility cleanly.
        let diff = &panel.view.as_ref().unwrap().parsed.files()[0];
        let layout = panel.view.as_ref().unwrap().layout;
        assert!(panel.review_threads.visible_threads(diff, layout, Side::Additions, 6).is_empty());
        panel.toggle_resolved_review_threads(0, Side::Additions, 6, cx);
        let diff = &panel.view.as_ref().unwrap().parsed.files()[0];
        assert_eq!(panel.review_threads.visible_threads(diff, layout, Side::Additions, 6).len(), 1);

        // 3. Revision-tracked Viewed toggle marks viewed, auto-folds, and captures new_object_id.
        assert_eq!(panel.file_viewed_state("src/a.rs"), FileViewedState::Stale);
        panel.toggle_file_viewed(0, false, cx);
        assert_eq!(panel.file_viewed_state("src/a.rs"), FileViewedState::Viewed);
        assert!(panel.is_collapsed(0));
        let (_, batch) = panel.viewed_tracker.take_batch().expect("queued batch");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0]["path"], "src/a.rs");
        assert_eq!(batch[0]["viewed"], true);
        assert_eq!(batch[0]["revision"], "aaaaaaa");

        // Shift-click range from 0..=1 marks both files viewed and collapses file 1 too.
        panel.toggle_file_viewed(1, true, cx);
        assert_eq!(panel.file_viewed_state("src/b.rs"), FileViewedState::Viewed);
        assert!(panel.is_collapsed(1));
    });

    // Re-draw after expansion, avatar toggle, and viewed fold to confirm no layout panics.
    cx.draw(point(px(0.), px(0.)), size(px(800.), px(320.)), |_, _| panel.clone().into_any_element());
}

#[cfg(feature = "snapshot")]
#[gpui::test]
fn switching_viewed_target_flushes_pending_debounced_presses_before_reset(
    cx: &mut gpui::TestAppContext,
) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, mut fixture) = cx.update(|cx| {
        crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
    });
    let (panel, vcx) = cx.add_window_view(|_, cx| DiffPanel::detached(state.clone(), cx));
    let parsed = Parsed::new(SAMPLE_PATCH, false, None, None, None);

    panel.update(vcx, |panel, cx| {
        panel.show_external(
            External { key: "pr:77".into(), parsed: Some(parsed), pending: false, error: None },
            cx,
        );
        panel.set_viewed_target(
            Some(ViewedTarget {
                project_id: "proj-1".into(),
                number: 77,
                host: Some("github.com".into()),
                repository: Some("org/repo".into()),
                commit: None,
                enabled: true,
            }),
            HashMap::new(),
            cx,
        );
        panel.toggle_file_viewed(0, false, cx);
        // Switch PR scope immediately within the debounce window — must flush PR #77's press.
        panel.set_viewed_target(
            Some(ViewedTarget {
                project_id: "proj-1".into(),
                number: 78,
                host: Some("github.com".into()),
                repository: Some("org/repo".into()),
                commit: None,
                enabled: true,
            }),
            HashMap::new(),
            cx,
        );
    });

    vcx.run_until_parked();
    let (method, payload, reply) = runtime.block_on(fixture.next_request());
    assert_eq!(method, t3_core::diff::SET_FILES_VIEWED_METHOD);
    assert_eq!(payload["projectId"], "proj-1");
    assert_eq!(payload["number"], 77);
    assert_eq!(payload["files"][0]["path"], "src/alpha.rs");
    assert_eq!(payload["files"][0]["viewed"], true);
    assert_eq!(payload["files"][0]["revision"], "2222222");
    let _ = reply.send(Ok(serde_json::json!({})));
}

#[gpui::test]
fn pr_viewed_auto_folds_respects_explicit_unfold_and_collapse_all_preserves_hidden_tests(
    cx: &mut gpui::TestAppContext,
) {
    let (_runtime, panel, mut vcx) = setup_panel(cx);
    let patch = "\
diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
diff --git a/src/app.test.ts b/src/app.test.ts
--- a/src/app.test.ts
+++ b/src/app.test.ts
@@ -1 +1 @@
-old test
+new test
";
    let parsed = Parsed::new(patch, false, None, None, None);
    let target = ViewedTarget {
        project_id: "proj-1".into(),
        number: 12,
        host: Some("github.com".into()),
        repository: Some("acme/app".into()),
        commit: None,
        enabled: true,
    };

    panel.update(&mut vcx, |panel, cx| {
        panel.show_external(
            External { key: "pr:12".into(), parsed: Some(parsed), pending: false, error: None },
            cx,
        );
        panel.set_viewed_target(Some(target.clone()), HashMap::new(), cx);
        // Explicitly unfold src/app.rs before server Viewed marks arrive (`pingdotgg/t3code#37605`).
        panel.set_folded(0, false, cx);
        assert!(!panel.is_collapsed(0));

        // Server Viewed marks arrive for both files: src/app.rs stays open (explicit choice),
        // while src/app.test.ts auto-folds because it is Viewed with no explicit override.
        let mut states = HashMap::new();
        states.insert("src/app.rs".into(), FileViewedState::Viewed);
        states.insert("src/app.test.ts".into(), FileViewedState::Viewed);
        panel.set_viewed_target(Some(target.clone()), states, cx);
        assert!(!panel.is_collapsed(0), "explicit reader unfold survives late Viewed state");
        assert!(panel.is_collapsed(1), "viewed file without reader override auto-folds");

        // Unfold the test file, hide test files, then Collapse All -> hidden test file keeps its fold (`pingdotgg/t3code#39213`).
        panel.set_folded(1, false, cx);
        assert!(!panel.is_collapsed(1));
        panel.toggle_hide_test_files(cx);
        panel.toggle_all_files(cx);
        assert!(panel.is_collapsed(0));
        panel.toggle_hide_test_files(cx);
        assert!(!panel.is_collapsed(1), "hidden test file kept its open choice across Collapse All");
    });
}

#[gpui::test]
fn withheld_file_banner_triggers_callback_and_cut_lines_disable_expansion(
    cx: &mut gpui::TestAppContext,
) {
    let (_runtime, panel, mut vcx) = setup_panel(cx);
    let loaded_paths: Rc<Mutex<Vec<String>>> = Rc::new(Mutex::new(Vec::new()));
    let loaded_clone = loaded_paths.clone();
    let placeholder = t3_core::diff::ReviewFile {
        path: "dist/bundle.js".into(),
        previous_path: None,
        additions: 5000,
        deletions: 0,
        change_type: Some(t3_core::diff::ChangeKind::New),
    }
    .placeholder();
    let parsed = Parsed::from_patch(
        t3_core::diff::ParsedPatch::Files(vec![placeholder]),
        true,
        None,
        None,
        None,
    );

    panel.update(&mut vcx, |panel, cx| {
        panel.set_on_load_withheld(move |path, _| loaded_clone.lock().unwrap().push(path));
        panel.show_external(
            External { key: "pr:99".into(), parsed: Some(parsed), pending: false, error: None },
            cx,
        );
        let mut withheld = HashMap::new();
        withheld.insert("dist/bundle.js".into(), super::WithheldFileStatus::Idle);
        let cut = std::collections::HashSet::from(["dist/bundle.js".into()]);
        panel.set_withheld_state(withheld, true, cut, cx);
        panel.set_folded(0, false, cx);
        assert!(!panel.can_expand_file(0));
        panel.trigger_load_withheld("dist/bundle.js".into(), cx);
    });
    vcx.draw(point(px(0.), px(0.)), size(px(800.), px(240.)), |_, _| panel.clone().into_any_element());
    assert_eq!(*loaded_paths.lock().unwrap(), vec!["dist/bundle.js".to_owned()]);
}
