//! Clipboard payload transport for `composerInlineTokenPaste.ts`; rules live in core.
use super::{Composer, Target};
use gpui::{App, ClipboardItem, Context, InteractiveElement, Window};
use serde_json::Value;
use t3_core::composer::{context, records};

impl Composer {
    pub(super) fn records_for(&self, target: &Target) -> &[std::sync::Arc<Value>] {
        self.context
            .records
            .get(target)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
    pub(super) fn seed_records(
        &mut self,
        target: &Target,
        records: impl IntoIterator<Item = impl Into<std::sync::Arc<Value>>>,
    ) {
        context::restore_unsent(
            self.context.records.entry(target.clone()).or_default(),
            records.into_iter().map(Into::into).collect(),
        );
    }
    pub(super) fn take_records(
        &mut self,
        target: &Target,
        text: &str,
    ) -> Vec<std::sync::Arc<Value>> {
        self.context
            .records
            .get_mut(target)
            .map(|r| context::take_for_send(r, text))
            .unwrap_or_default()
    }
    pub(super) fn all_context_records(&self) -> Vec<Value> {
        let Some(target) = &self.snapshot.target else {
            return Vec::new();
        };
        self.terminals_for(target)
            .iter()
            .map(context::TerminalContext::record)
            .chain(
                self.reviews_for(target)
                    .iter()
                    .map(t3_core::composer::review::ReviewComment::record),
            )
            .chain(
                self.mentions(target)
                    .iter()
                    .map(t3_core::composer::menu::ThreadContext::to_record),
            )
            .chain(self.records_for(target).iter().map(|r| (**r).clone()))
            .chain(
                self.staged_for(target)
                    .iter()
                    .filter_map(|s| s.attachment.context_record()),
            )
            .collect()
    }
    pub(super) fn apply_import(
        &mut self,
        target: &Target,
        plan: super::imports::ImportPlan,
        cx: &mut Context<Self>,
    ) {
        let mut held = Vec::new();
        for record in plan.records {
            if let Some(terminal) = records::terminal_from_record(
                &record,
                &self.thread_id().unwrap_or_default(),
                &t3_core::sidebar::time::format_iso(crate::sidebar::controller::now_ms()),
            ) {
                self.seed_terminals(target, vec![terminal]);
            } else {
                held.push(record);
            }
        }
        self.seed_records(target, held);
        self.seed_reviews(target, plan.reviews);
        for imported in plan.attachments {
            self.stage_imported_attachment(target, imported, cx);
        }
    }

    pub(super) fn with_context_copy(
        &self,
        shell: gpui::Stateful<gpui::Div>,
        id: &str,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let id = id.to_owned();
        shell.on_action(
            cx.listener(move |this, _: &gpui_component::input::Copy, _, cx| {
                if let Some((text, fragment)) = super::imports::copy_chip(
                    &id,
                    &this.all_context_records(),
                    &this.environment_id(cx),
                ) {
                    write(text, fragment, cx);
                    cx.stop_propagation();
                }
            }),
        )
    }

    pub(super) fn copy_context_selection(&self, cx: &mut Context<Self>) -> bool {
        let input = self.input.read(cx);
        let range = input.selected_range();
        let text = input.value();
        let Some(text) = text.get(range) else {
            return false;
        };
        let all = self.all_context_records();
        let Some(fragment) = records::copy_fragment(
            text,
            &all,
            &self.environment_id(cx),
            self.thread_id().as_deref(),
            None,
        ) else {
            return false;
        };
        write(text.to_owned(), fragment, cx);
        true
    }
    pub(crate) fn paste_context(
        &mut self,
        text: &str,
        fragment: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self.snapshot.target.clone() else {
            return false;
        };
        if self.answering.is_some()
            || self.active_question().is_some()
            || self.editing_queued().is_some()
        {
            return false;
        }
        let Some(fragment) = records::decode_fragment(fragment) else {
            return false;
        };
        let environment = self.environment_id(cx);
        let mut incoming = records::clipboard_records(
            text,
            fragment["records"].as_array().expect("decoded records"),
        );
        if records::field(&fragment["source"], "environmentId") != environment {
            incoming.retain(|r| !matches!(records::field(r, "kind"), "image" | "file"));
        }
        let Some(plan) = super::imports::prepare(
            text,
            incoming,
            &self.all_context_records(),
            &environment,
            super::imports::ImportMode::Copy,
            crate::app_state::uuid_v4,
        ) else {
            return false;
        };
        let text = plan.text.clone();
        self.apply_import(&target, plan, cx);
        self.input
            .update(cx, |input, cx| input.insert(text, window, cx));
        self.draft_changed(cx);
        cx.notify();
        true
    }
}
pub(crate) fn write(text: String, fragment: String, cx: &mut App) {
    #[cfg(feature = "snapshot")]
    if crate::webview::is_offscreen() {
        cx.set_global(super::prompt_links::SnapshotClipboard(Some(
            serde_json::json!({"text":text,"fragment":fragment}).to_string(),
        )));
        return;
    }
    let html = records::clipboard_html(&text, &fragment);
    cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(text, fragment));
    #[cfg(target_os = "macos")]
    platform::write_html(&html);
    #[cfg(not(target_os = "macos"))]
    let _ = html;
}

pub(super) fn read_html_fragment() -> Option<String> {
    if crate::webview::is_offscreen() {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        platform::read_html().and_then(|html| records::fragment_from_html(&html))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeHTML};
    use objc2_foundation::{NSArray, NSString};
    pub(super) fn write_html(html: &str) {
        let board = NSPasteboard::generalPasteboard();
        let kind = unsafe { NSPasteboardTypeHTML };
        // No owner is installed: these bytes are supplied synchronously.
        unsafe {
            board.addTypes_owner(&NSArray::from_slice(&[kind]), None);
        }
        board.setString_forType(&NSString::from_str(html), kind);
    }
    pub(super) fn read_html() -> Option<String> {
        NSPasteboard::generalPasteboard()
            .stringForType(unsafe { NSPasteboardTypeHTML })
            .map(|html| html.to_string())
    }
}
