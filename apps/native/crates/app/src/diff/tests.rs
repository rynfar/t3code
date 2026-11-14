use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::{AppContext as _, Entity, IntoElement as _, VisualTestContext, point, px, size};
use t3_core::composer::review::{CommentDictationState, Selection, Side};
use t3_core::diff::{
    CompletionOptions, CompletionScope, CompletionSupport, FileRevealTarget, MentionCandidate,
    ReferenceCandidate, ReferenceKind, ReferenceSigil, ReferenceState,
};

use super::{DiffPanel, External, Parsed};
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

    // Pending reveal for an unloaded file cancels when user interacts.
    panel.update(&mut cx, |panel, cx| {
        panel.external = Some(External {
            key: "scope:reveal".into(),
            parsed: panel.external.as_ref().and_then(|e| e.parsed.clone()),
            pending: true,
            error: None,
        });
        panel.reveal_file_path(2, FileRevealTarget::line("file_missing.rs", 10), cx);
        assert!(panel.file_reveal.has_pending());
        panel.cancel_reveal_on_interaction();
        assert!(!panel.file_reveal.has_pending());
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

            // Dictation inserts at caret and blocks submit while active.
            panel.set_comment_dictation_state(CommentDictationState::Active, cx);
            assert!(panel.insert_comment_text("about #42", window, cx));
            assert_eq!(panel.read_comment_draft(cx).as_deref(), Some("Ask @alice about #42"));
            panel.submit_comment(cx);
            assert!(submitted.lock().unwrap().is_empty());

            // Clearing dictation lets the comment submit cleanly through the external hook.
            panel.set_comment_dictation_state(CommentDictationState::Idle, cx);
            panel.submit_comment(cx);
        });
    });

    let captured = submitted.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].text, "Ask @alice about #42".into());
    assert_eq!(captured[0].file_path, "src/alpha.rs");
}
