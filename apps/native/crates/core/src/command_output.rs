//! One expanded command row's output: `applyCommandOutputFrame` from
//! `packages/client-runtime/src/state/commandOutput.ts`, the `OrchestrationV2CommandOutputFrame`
//! wire type, and what `apps/web/src/components/chat/CommandOutputPanel.tsx` shows for each state.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::terminal_output::{TERMINAL_OUTPUT_TAIL_CHARS, TerminalOutputState};

pub const SUBSCRIBE_COMMAND_OUTPUT: &str = "orchestration.subscribeCommandOutput";

pub const LOADING: &str = "Loading output…";
pub const NO_OUTPUT: &str = "No output.";
pub const TRUNCATED: &str = "Earlier output not shown";

pub fn subscribe_request(thread_id: &str, item_id: &str) -> Value {
    json!({ "threadId": thread_id, "itemId": item_id })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameKind {
    Replace,
    Append,
}

/// `OrchestrationV2CommandOutputFrame`: `replace` carries the whole bounded tail (the first
/// frame, a viewer that fell behind, or the final output), `append` raw terminal text, and
/// `running: false` ends the stream.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CommandOutputFrame {
    pub kind: FrameKind,
    pub text: String,
    /// Earlier output was dropped to keep the tail bounded.
    pub truncated: bool,
    pub running: bool,
}

/// `CommandOutputView`: one command row's output as a client holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutputView {
    pub output: TerminalOutputState,
    /// False once the command settled and `output` is final.
    pub running: bool,
}

impl Default for CommandOutputView {
    /// `EMPTY_COMMAND_OUTPUT`.
    fn default() -> Self {
        CommandOutputView { output: TerminalOutputState::default(), running: true }
    }
}

impl CommandOutputView {
    /// `applyCommandOutputFrame`, in place. An `append` is normalized with the same rules the
    /// server uses, so escapes and redraws split across frames fold as they would whole.
    pub fn apply(&mut self, frame: &CommandOutputFrame) {
        if frame.kind == FrameKind::Replace {
            self.output = TerminalOutputState::default();
        }
        self.output.truncated |= frame.truncated;
        self.output.append(&frame.text, TERMINAL_OUTPUT_TAIL_CHARS);
        self.running = frame.running;
    }
}

/// What a row draws from: the normalized text, still holding its SGR escapes.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutput {
    pub text: Arc<str>,
    pub truncated: bool,
    pub running: bool,
}

impl From<&CommandOutputView> for CommandOutput {
    fn from(view: &CommandOutputView) -> Self {
        CommandOutput { text: Arc::from(view.output.text.as_str()), truncated: view.output.truncated, running: view.running }
    }
}

/// A subscription's state: the last output it folded, and the failure that ended it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommandOutputStatus {
    /// `None` until the first frame.
    pub output: Option<CommandOutput>,
    pub error: Option<String>,
}

/// `CommandOutputPanel`'s branches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Panel<'a> {
    Output(&'a CommandOutput),
    /// `Couldn't load output: …`.
    Failed(&'a str),
    Loading,
    /// A running command that has printed nothing yet draws nothing.
    Waiting,
    Empty,
}

impl CommandOutputStatus {
    pub fn panel(&self) -> Panel<'_> {
        match (&self.output, self.error.as_deref()) {
            (Some(output), _) if !output.text.is_empty() => Panel::Output(output),
            (_, Some(error)) => Panel::Failed(error),
            (None, None) => Panel::Loading,
            (Some(output), None) if output.running => Panel::Waiting,
            (Some(_), None) => Panel::Empty,
        }
    }
}

/// `formatEnvironmentQueryError`.
pub fn error_message(message: &str) -> String {
    if message.trim().is_empty() { "The environment request failed.".to_owned() } else { message.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(kind: FrameKind, text: &str, truncated: bool, running: bool) -> CommandOutputFrame {
        CommandOutputFrame { kind, text: text.to_owned(), truncated, running }
    }

    #[test]
    fn decodes_the_wire_frame() {
        let frame: CommandOutputFrame = serde_json::from_value(json!({ "kind": "append", "text": "a", "truncated": false, "running": true })).unwrap();
        assert_eq!(frame, CommandOutputFrame { kind: FrameKind::Append, text: "a".into(), truncated: false, running: true });
    }

    /// The web's only case: an escape and a redraw split across appends.
    #[test]
    fn continues_an_escape_and_a_redraw_split_across_appends() {
        let mut view = CommandOutputView::default();
        view.apply(&frame(FrameKind::Replace, "install\n12%\u{1b}[3", false, true));
        assert_eq!(view.output.text, "install\n12%");
        view.apply(&frame(FrameKind::Append, "2m\r100%\u{1b}[0m\n", false, true));
        // The colour opened before the redraw survives it.
        assert_eq!(view.output.text, "install\n\u{1b}[0;32m100%\u{1b}[0m\n");
        assert!(view.running);
    }

    #[test]
    fn a_replace_after_appends_starts_over() {
        let mut view = CommandOutputView::default();
        view.apply(&frame(FrameKind::Replace, "one\n", false, true));
        view.apply(&frame(FrameKind::Append, "two\n\u{1b}[1A", false, true));
        view.apply(&frame(FrameKind::Append, "TWO", false, true));
        assert_eq!(view.output.text, "one\nTWO\n");
        // The final output replaces everything folded so far, cursor and pending escape included.
        view.apply(&frame(FrameKind::Append, "\u{1b}[3", false, true));
        view.apply(&frame(FrameKind::Replace, "done\n", false, false));
        assert_eq!(view.output.text, "done\n");
        assert_eq!(view.output.pending(), "");
        assert!(!view.running);
        // A later append folds onto the replaced screen, not the old one.
        view.apply(&frame(FrameKind::Append, "x", false, false));
        assert_eq!(view.output.text, "done\nx");
    }

    #[test]
    fn truncated_sticks_until_a_replace_clears_it() {
        let mut view = CommandOutputView::default();
        view.apply(&frame(FrameKind::Replace, "tail\n", true, true));
        assert!(view.output.truncated);
        view.apply(&frame(FrameKind::Append, "more\n", false, true));
        assert!(view.output.truncated, "an append keeps earlier truncation");
        view.apply(&frame(FrameKind::Replace, "final\n", false, false));
        assert!(!view.output.truncated, "a replace carries its own flag");
    }

    #[test]
    fn appends_past_the_tail_truncate_on_the_client() {
        let mut view = CommandOutputView::default();
        view.apply(&frame(FrameKind::Replace, "", false, true));
        let line = format!("{}\n", "x".repeat(1023));
        for _ in 0..80 {
            view.apply(&frame(FrameKind::Append, &line, false, true));
        }
        assert!(view.output.truncated);
        assert!(view.output.text.encode_utf16().count() <= TERMINAL_OUTPUT_TAIL_CHARS);
        assert!(view.output.text.starts_with('x'), "the cut lands on a line start");
    }

    #[test]
    fn panel_follows_the_web_branches() {
        let output = |text: &str, running: bool| Some(CommandOutput { text: Arc::from(text), truncated: false, running });
        assert_eq!(CommandOutputStatus::default().panel(), Panel::Loading);
        let waiting = CommandOutputStatus { output: output("", true), error: None };
        assert_eq!(waiting.panel(), Panel::Waiting);
        let empty = CommandOutputStatus { output: output("", false), error: None };
        assert_eq!(empty.panel(), Panel::Empty);
        let failed = CommandOutputStatus { output: None, error: Some("No such command.".into()) };
        assert_eq!(failed.panel(), Panel::Failed("No such command."));
        // Output already shown stays when the stream later fails.
        let kept = CommandOutputStatus { output: output("hi", true), error: Some("lost".into()) };
        assert!(matches!(kept.panel(), Panel::Output(output) if &*output.text == "hi"));
        assert_eq!(error_message("  "), "The environment request failed.");
    }
}
