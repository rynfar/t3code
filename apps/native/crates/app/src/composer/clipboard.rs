//! Clipboard payload transport for `composerInlineTokenPaste.ts`; rules live in core.
use super::{Composer, Target};
use gpui::{App, ClipboardItem, Context, Window};
use serde_json::Value;
use t3_core::composer::{context, records};

impl Composer {
    pub(super) fn records_for(&self, target: &Target) -> &[Value] {
        self.context
            .records
            .get(target)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
    pub(super) fn seed_records(&mut self, target: &Target, records: Vec<Value>) {
        context::restore_unsent(
            self.context.records.entry(target.clone()).or_default(),
            records,
        );
    }
    pub(super) fn take_records(&mut self, target: &Target, text: &str) -> Vec<Value> {
        self.context
            .records
            .get_mut(target)
            .map(|r| context::take_for_send(r, text))
            .unwrap_or_default()
    }
    fn all_context_records(&self) -> Vec<Value> {
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
            .chain(self.records_for(target).iter().cloned())
            .chain(
                self.staged_for(target)
                    .iter()
                    .filter_map(|s| s.attachment.context_record()),
            )
            .collect()
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
        let mut held = self.all_context_records();
        let Some(text) = records::import_fragment(
            text,
            fragment,
            &mut held,
            &self.environment_id(cx),
            crate::app_state::uuid_v4,
        ) else {
            return false;
        };
        let existing: Vec<String> = self
            .mentions(&target)
            .iter()
            .map(|r| r.context_id.clone())
            .chain(self.terminals_for(&target).iter().map(|r| r.context_id()))
            .chain(self.reviews_for(&target).iter().map(|r| r.context_id()))
            .collect();
        let mut terminals = Vec::new();
        let mut attachments = Vec::new();
        held.retain(|r| {
            if let Some(terminal) = records::terminal_from_record(
                r,
                &self.thread_id().unwrap_or_default(),
                &t3_core::sidebar::time::format_iso(crate::sidebar::controller::now_ms()),
            ) {
                terminals.push(terminal);
                return false;
            }
            if let Some(attachment) = records::attachment_from_record(r) {
                attachments.push(attachment);
                return false;
            }

            true
        });
        self.seed_terminals(&target, terminals);
        let present: Vec<String> = self
            .staged_for(&target)
            .iter()
            .map(|s| s.attachment.context_id())
            .collect();
        attachments.retain(|a| !present.contains(&a.context_id()));
        self.seed_attachments(&target, attachments);
        held.retain(|r| !existing.contains(&records::field(r, "contextId").to_owned()));
        self.seed_records(&target, held);
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
