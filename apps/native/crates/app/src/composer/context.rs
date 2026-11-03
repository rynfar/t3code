//! Context chips and terminal excerpts in the prompt. Ports the chip shell and its details
//! popover (`ContextChip.tsx`, `contextChipParts.tsx`), the composer's chip per context kind
//! (`composerContextPresentation.tsx`, `chat/TerminalContextInlineChip.tsx`,
//! `ThreadContextChip.tsx`), and `ChatComposer.addTerminalContext`. The web's editor draws each
//! `t3-context://` link as an inline chip; this prompt is plain text, so the links it holds show
//! as chips in a row above it, in prompt order, each with an X that removes its link. Rules live
//! in `t3_core::composer::context`.

use std::collections::HashMap;
use std::time::Instant;

use gpui::{
    AnyElement, App, BoxShadow, Context, Hsla, IntoElement, KeyDownEvent, ParentElement, SharedString, Styled, Window, div, point,
    prelude::*, px,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::composer::review::ReviewComment;
use t3_core::composer::context::{self as rules, TerminalContext, TerminalSelection};
use t3_core::composer::menu::{ThreadContext, context_references, sanitize_context_label};

use super::controls::{Float, floating, floating_shadow};
use super::{Composer, Target};
use crate::app_state::uuid_v4;
use crate::icons::Icon;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

/// The prompt font's size: chips are as tall as the text they stand in for (`size-[1em]` icons).
const CHIP_TEXT: f32 = 14.;

#[derive(Default)]
pub(super) struct ContextChips {
    /// Terminal excerpts per target. A send carries those the prompt still links; unlinked ones
    /// stay so an undo that brings a link back brings its chip back.
    terminals: HashMap<Target, Vec<TerminalContext>>,
    pub(super) reviews: HashMap<Target, Vec<ReviewComment>>,
    /// The chip whose details popover is open, by context id.
    open: Option<String>,
    dismissed: Option<Instant>,
    /// The links the prompt held at the last edit, so other edits do not repaint the row.
    linked: Vec<String>,
}

/// One chip in the row, resolved against what the composer holds.
enum Chip {
    Terminal(TerminalContext),
    Review(ReviewComment),
    Thread(ThreadContext),
    /// A link whose context this composer does not have (`UnresolvedContextChip`).
    Unresolved { context_id: String, label: String },
}

impl Chip {
    fn context_id(&self) -> String {
        match self {
            Chip::Terminal(context) => context.context_id(),
            Chip::Review(context) => context.context_id(),
            Chip::Thread(thread) => thread.context_id.clone(),
            Chip::Unresolved { context_id, .. } => context_id.clone(),
        }
    }
}

/// How a chip's ink and icon read (`contextChipVariants` `kind` and `state`).
#[derive(Clone, Copy)]
pub(super) enum Ink {
    Accent(u32),
    Invalid,
    /// `unresolved`: the surrounding text color.
    Current,
}

impl Composer {
    pub(super) fn reviews_for(&self, target: &Target) -> &[ReviewComment] {
        self.context.reviews.get(target).map(Vec::as_slice).unwrap_or_default()
    }

    pub(super) fn current_reviews(&self) -> &[ReviewComment] {
        self.snapshot.target.as_ref().map(|target| self.reviews_for(target)).unwrap_or_default()
    }

    pub(super) fn seed_reviews(&mut self, target: &Target, comments: Vec<ReviewComment>) {
        rules::restore_unsent(self.context.reviews.entry(target.clone()).or_default(), comments);
    }

    pub(super) fn take_reviews(&mut self, target: &Target, prompt: &str) -> Vec<ReviewComment> {
        self.context.reviews.get_mut(target).map(|held| rules::take_for_send(held, prompt)).unwrap_or_default()
    }

    pub(super) fn add_review_comment(&mut self, comment: ReviewComment, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.snapshot.target.clone() else { return };
        let reference = comment.reference();
        self.seed_reviews(&target, vec![comment]);
        if let Some(editing) = self.queue.editing.as_mut() {
            editing.stash = rules::insert_reference(&editing.stash, editing.stash.len(), &reference).0;
        } else if let Some(stash) = self.stash.as_mut().filter(|_| self.answering.is_some()) {
            *stash = rules::insert_reference(stash, stash.len(), &reference).0;
        } else {
            let prompt = self.prompt(cx);
            let edit = rules::reference_edit(&prompt, self.input.read(cx).selected_range().end, &reference);
            self.input.update(cx, |input, cx| {
                input.set_selected_range(edit.start..edit.end, cx);
                input.replace(edit.text.clone(), window, cx);
                input.set_selected_range(edit.cursor()..edit.cursor(), cx);
            });
            self.focus(window, cx);
        }
        self.draft_changed(cx);
        cx.notify();
    }

    pub(super) fn remove_review_comment(&mut self, context_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editing) = self.queue.editing.as_mut() {
            editing.stash = rules::remove_reference(&editing.stash, context_id).0;
        } else if let Some(stash) = self.stash.as_mut().filter(|_| self.answering.is_some()) {
            *stash = rules::remove_reference(stash, context_id).0;
        } else {
            self.remove_context(context_id, window, cx);
        }
        self.draft_changed(cx);
        cx.notify();
    }

    pub(super) fn terminals_for(&self, target: &Target) -> &[TerminalContext] {
        self.context.terminals.get(target).map(Vec::as_slice).unwrap_or_default()
    }

    pub(super) fn current_terminals(&self) -> &[TerminalContext] {
        self.snapshot.target.as_ref().map(|target| self.terminals_for(target)).unwrap_or_default()
    }

    pub(super) fn seed_terminals(&mut self, target: &Target, terminals: Vec<TerminalContext>) {
        if terminals.is_empty() {
            return;
        }
        let held = self.context.terminals.entry(target.clone()).or_default();
        for terminal in terminals {
            if !held.iter().any(|existing| existing.id == terminal.id) {
                held.push(terminal);
            }
        }
    }

    pub(super) fn clear_terminals(&mut self, target: &Target) {
        self.context.terminals.remove(target);
    }

    /// The excerpts a send's `prompt` links; the rest go with the prompt's history.
    pub(super) fn take_terminals(&mut self, target: &Target, prompt: &str) -> Vec<TerminalContext> {
        self.context.terminals.get_mut(target).map(|held| rules::take_for_send(held, prompt)).unwrap_or_default()
    }

    pub(super) fn restore_terminals(&mut self, target: &Target, sent: Vec<TerminalContext>) {
        rules::restore_unsent(self.context.terminals.entry(target.clone()).or_default(), sent);
    }

    /// Leaving a target ends its prompt's undo history (`set_value` clears it), so the threads
    /// and excerpts its draft no longer links go.
    pub(super) fn reset_context_history(&mut self, target: &Target, prompt: &str) {
        self.reset_thread_history(target, prompt);
        if let Some(held) = self.context.reviews.get_mut(target) { rules::reset_history(held, prompt); }
        if let Some(held) = self.context.terminals.get_mut(target) {
            rules::reset_history(held, prompt);
        }
    }

    /// `addTerminalContext`: links the selection at the caret, unless the prompt already links
    /// the same lines, and moves focus to the prompt after the link.
    pub(super) fn add_terminal_context(&mut self, selection: TerminalSelection, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Target::Thread(thread_id)) = self.snapshot.target.clone() else { return };
        // A question's answer is literal text, and a queued edit carries text only.
        if self.answering.is_some() || self.active_question().is_some() || self.editing_queued().is_some() {
            return;
        }
        let Some(context) = TerminalContext::new(selection, &uuid_v4(), &thread_id, &t3_core::sidebar::time::format_iso(now_ms())) else {
            return;
        };
        let (prompt, cursor) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.selected_range().end)
        };
        let target = Target::Thread(thread_id);
        let Some(edit) = rules::add_terminal_context(&prompt, cursor, self.terminals_for(&target), &context) else { return };
        self.context.terminals.entry(target).or_default().push(context);
        let caret = edit.cursor();
        self.input.update(cx, |input, cx| {
            input.set_selected_range(edit.start..edit.end, cx);
            input.replace(edit.text, window, cx);
            input.set_selected_range(caret..caret, cx);
        });
        self.draft_changed(cx);
        self.focus(window, cx);
        cx.notify();
    }

    /// Closes an open details popover; whether one was open. Escape does this from the prompt
    /// and from anything in the chip row.
    pub(super) fn close_context_details(&mut self, cx: &mut Context<Self>) -> bool {
        let closed = self.context.open.take().is_some();
        if closed {
            cx.notify();
        }
        closed
    }

    /// Removes every link to `context_id` from the prompt, as one undoable edit.
    fn remove_context(&mut self, context_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.prompt(cx);
        let (next, caret) = rules::remove_reference(&prompt, context_id);
        if next == prompt {
            return;
        }
        if self.context.open.as_deref() == Some(context_id) {
            self.context.open = None;
        }
        let end = prompt.len();
        self.input.update(cx, |input, cx| {
            input.set_selected_range(0..end, cx);
            input.replace(next, window, cx);
            input.set_selected_range(caret..caret, cx);
        });
        self.focus(window, cx);
        cx.notify();
    }

    /// Repaints only when the prompt's links change; called on every edit.
    pub(super) fn sync_context_chips(&mut self, text: &str, cx: &mut Context<Self>) {
        let linked: Vec<String> = context_references(text).into_iter().map(|reference| reference.context_id.to_owned())
            .chain(t3_core::assistant_citations::collect(text).into_iter().map(|(range, _)| format!("{}:{}", range.start, &text[range.clone()]))).collect();
        if linked != self.context.linked {
            if self.context.open.as_ref().is_some_and(|open| !linked.contains(open)) {
                self.context.open = None;
            }
            self.context.linked = linked;
            cx.notify();
        }
    }

    /// The chips for the prompt's links, once each, in prompt order. Images and files show in
    /// the attachment strip.
    fn chips(&self, prompt: &str) -> Vec<Chip> {
        let Some(target) = self.snapshot.target.as_ref() else { return Vec::new() };
        let mut chips: Vec<Chip> = Vec::new();
        for reference in context_references(prompt) {
            if matches!(reference.kind, "image" | "file") || chips.iter().any(|chip| chip.context_id() == reference.context_id) {
                continue;
            }
            let terminal = (reference.kind == rules::TERMINAL_KIND)
                .then(|| self.terminals_for(target).iter().find(|context| context.context_id() == reference.context_id))
                .flatten();
            let thread = (reference.kind == "thread")
                .then(|| self.mentions(target).iter().find(|thread| thread.context_id == reference.context_id))
                .flatten();
            let review = self.reviews_for(target).iter().find(|comment| comment.context_id() == reference.context_id);
            chips.push(match (terminal, thread, review) {
                (Some(terminal), _, _) => Chip::Terminal(terminal.clone()),
                (_, Some(thread), _) => Chip::Thread(thread.clone()),
                (_, _, Some(comment)) => Chip::Review(comment.clone()),
                _ => Chip::Unresolved {
                    context_id: reference.context_id.to_owned(),
                    label: sanitize_context_label(reference.label, reference.kind),
                },
            });
        }
        chips
    }

    /// The row of chips above the prompt, or nothing when it links no context.
    pub(super) fn render_context_chips(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.answering.is_some() || self.editing_queued().is_some() {
            return None;
        }
        let chips = self.chips(&self.prompt(cx));
        if chips.is_empty() {
            return None;
        }
        Some(
            h_flex()
                .id("context-chips")
                .flex_wrap()
                .gap_x(px(12.))
                .gap_y(px(4.))
                .mb(px(8.))
                // Escape from a focused chip or its X; the prompt's Escape action covers the rest.
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" && this.close_context_details(cx) {
                        cx.stop_propagation();
                    }
                }))
                .children(chips.into_iter().map(|chip| self.render_chip(chip, cx)))
                .into_any_element(),
        )
    }

    fn render_chip(&self, chip: Chip, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let context_id = chip.context_id();
        match chip {
            Chip::Review(comment) => {
                let ink = Ink::Accent(p.context_chip_review_comment);
                let open = self.context.open.as_deref() == Some(context_id.as_str());
                let toggle_id = context_id.clone();
                let shell = chip_shell(&context_id, Icon::MessageCircle, comment.label(), ink, true, cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.context.open = if this.context.open.as_deref() == Some(&toggle_id) { None } else { Some(toggle_id.clone()) };
                        cx.notify();
                    }));
                div().relative().child(self.with_remove(shell, &context_id, ink, cx))
                    .when(open, |this| this.child(floating(Float::AboveStart, self.review_popover(&comment, cx))))
                    .into_any_element()
            }
            Chip::Terminal(context) if !context.is_expired() => {
                let open = self.context.open.as_deref() == Some(context_id.as_str());
                let toggle_id = context_id.clone();
                let shell = chip_shell(&context_id, Icon::Terminal, context.label(), Ink::Accent(p.context_chip_terminal), true, cx)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if Self::guard_reopen(&mut this.context.dismissed) {
                            return;
                        }
                        this.context.open = if this.context.open.as_deref() == Some(toggle_id.as_str()) { None } else { Some(toggle_id.clone()) };
                        cx.notify();
                    }));
                div()
                    .relative()
                    .child(self.with_remove(shell, &context_id, Ink::Accent(p.context_chip_terminal), cx))
                    .when(open, |this| this.child(floating(Float::AboveStart, self.terminal_popover(&context, cx))))
                    .into_any_element()
            }
            Chip::Terminal(context) => {
                let tooltip = rules::expired_tooltip(&context.label());
                let shell = chip_shell(&context_id, Icon::Terminal, context.label(), Ink::Invalid, false, cx).tooltip(text_tooltip(tooltip));
                self.with_remove(shell, &context_id, Ink::Invalid, cx).into_any_element()
            }
            Chip::Thread(thread) => {
                let state = self.state.read(cx);
                let shell_title = state.thread_shell(&thread.thread_id).map(|shell| shell.title.trim().to_owned()).filter(|title| !title.is_empty());
                let unreadable = thread.environment_id != self.environment_id(cx);
                let tooltip = if unreadable {
                    "On another machine, so this machine's agent can't read it. Remove it or switch back to send."
                } else if shell_title.is_some() {
                    "Open thread"
                } else {
                    "Thread no longer available"
                };
                let ink = if unreadable { Ink::Invalid } else { Ink::Accent(p.context_chip_thread) };
                let available = shell_title.is_some() && !unreadable;
                let thread_id = thread.thread_id.clone();
                let shell = chip_shell(&context_id, Icon::MessagesSquare, shell_title.unwrap_or(thread.label), ink, available, cx)
                    .tooltip(text_tooltip(tooltip.to_owned()))
                    .when(available, |this| {
                        this.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                            this.state.update(cx, |state, cx| state.select_thread(&thread_id, cx));
                        }))
                    });
                self.with_remove(shell, &context_id, ink, cx).into_any_element()
            }
            Chip::Unresolved { label, .. } => {
                let shell = chip_shell(&context_id, Icon::CircleDashed, label, Ink::Current, false, cx).tooltip(text_tooltip(rules::UNRESOLVED_TOOLTIP.to_owned()));
                self.with_remove(shell, &context_id, Ink::Current, cx).into_any_element()
            }
        }
    }

    /// The chip followed by its remove action (`ContextChipAction`): a 1em square whose X takes
    /// the link out of the prompt.
    fn with_remove(&self, shell: gpui::Stateful<gpui::Div>, context_id: &str, ink: Ink, cx: &mut Context<Self>) -> impl IntoElement {
        let color = ink_color(ink, cx);
        let ring_color = hsla(palette(cx).ring);
        let remove_id = context_id.to_owned();
        h_flex().items_center().child(shell).child(
            div()
                .id(SharedString::from(format!("context-chip-remove-{context_id}")))
                .ml(px(CHIP_TEXT * 0.17))
                .size(px(CHIP_TEXT))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(move |style| style.bg(color.opacity(0.17)))
                .tab_index(0)
                .focus_visible(move |style| style.shadow(ring(ring_color, 1.)))
                .tooltip(text_tooltip("Remove".to_owned()))
                .on_click(cx.listener(move |this, _, window, cx| this.remove_context(&remove_id, window, cx)))
                .child(Icon::X.render(px(CHIP_TEXT * 0.85), color)),
        )
    }

    fn review_popover(&self, comment: &ReviewComment, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        v_flex().id("review-comment-details").w(px(384.)).max_w_full().p(px(12.)).gap(px(8.))
            .rounded(px(8.)).border_1().border_color(hsla(p.border).opacity(0.7)).bg(hsla(p.popover))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| { this.context.open = None; cx.notify(); }))
            .child(div().text_size(px(12.)).child(comment.file_path.clone()))
            .child(div().text_size(px(11.)).text_color(hsla(p.secondary_label)).child(format!("{} · {}", comment.section_title, comment.range_label)))
            .child(div().text_size(px(14.)).child(SharedString::from(comment.text.clone())))
            .child(div().id("review-comment-excerpt").max_h(px(256.)).overflow_y_scroll().font_family(cx.theme().mono_font_family.clone()).text_size(px(11.)).child(SharedString::from(comment.diff.clone())))
            .into_any_element()
    }

    /// `TerminalContextInlineChip`'s popover: the terminal and its lines over the captured text.
    fn terminal_popover(&self, context: &TerminalContext, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let rule = hsla(p.border).opacity(0.7);
        let mono = cx.theme().mono_font_family.clone();
        v_flex()
            .id("terminal-context-popover")
            .occlude()
            .mb(px(4.))
            .w(px(384.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.dropdown_border))
            .bg(hsla(p.popover))
            .text_color(hsla(p.popover_foreground))
            .shadow(floating_shadow(cx.theme().is_dark()))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.context.open.take().is_some() {
                    this.context.dismissed = Some(Instant::now());
                    cx.notify();
                }
            }))
            .child(
                v_flex()
                    .overflow_hidden()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(rule)
                    .bg(hsla(p.background).opacity(0.8))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .px(px(12.))
                            .py(px(8.))
                            .border_b_1()
                            .border_color(rule)
                            .child(div().flex_none().child(Icon::Terminal.render(px(16.), hsla(p.success))))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(hsla(p.foreground))
                                    .child(SharedString::from(context.terminal_label.clone())),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(hsla(p.secondary_label))
                                    .child(SharedString::from(context.line_caption())),
                            ),
                    )
                    .child(
                        div()
                            .id("terminal-context-text")
                            .max_h(px(320.))
                            .overflow_scroll()
                            .bg(hsla(p.muted))
                            .p(px(12.))
                            .font_family(mono)
                            .text_size(px(12.))
                            .line_height(px(12. * 1.625))
                            .text_color(hsla(p.foreground))
                            .whitespace_nowrap()
                            .child(SharedString::from(context.text.clone())),
                    ),
            )
            .into_any_element()
    }
}

fn ink_color(ink: Ink, cx: &App) -> Hsla {
    let p = palette(cx);
    match ink {
        Ink::Accent(color) => hsla(color),
        Ink::Invalid => hsla(p.context_chip_invalid),
        Ink::Current => hsla(p.foreground),
    }
}

/// A focus ring drawn inside the control, so focusing it moves nothing.
fn ring(color: Hsla, width: f32) -> Vec<BoxShadow> {
    vec![BoxShadow { color, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(width), inset: true }]
}

fn text_tooltip(text: String) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView {
    let text = SharedString::from(text);
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

/// `ContextChipShell`: icon and label in the kind's ink at the prompt's size, medium weight. A
/// chip that opens something is a tab stop (Enter and Space click it) with a 2px ring in its ink,
/// and underlines its label on hover.
pub(super) fn chip_shell(context_id: &str, icon: Icon, label: String, ink: Ink, clickable: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let color = ink_color(ink, cx);
    let group = SharedString::from(format!("context-chip-{context_id}"));
    h_flex()
        .id(SharedString::from(format!("context-chip-{context_id}")))
        .group(group.clone())
        .min_w_0()
        .max_w(px(320.))
        .gap(px(CHIP_TEXT * 0.2))
        .rounded(px(4.))
        .text_size(px(CHIP_TEXT))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(color)
        .when(clickable, |this| this.tab_index(0).focus_visible(move |style| style.shadow(ring(color, 2.))))
        .child(div().flex_none().child(icon.render(px(CHIP_TEXT), color)))
        .child(
            div()
                .min_w_0()
                .truncate()
                .when(clickable, |this| this.group_hover(group, |style| style.underline()))
                .child(SharedString::from(label)),
        )
}
