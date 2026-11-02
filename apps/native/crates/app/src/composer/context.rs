//! Context chips and terminal excerpts in the prompt. Ports the chip shell and its details
//! popover (`ContextChip.tsx`, `contextChipParts.tsx`), the composer's chip per context kind
//! (`composerContextPresentation.tsx`, `chat/TerminalContextInlineChip.tsx`,
//! `ThreadContextChip.tsx`), and `ChatComposer.addTerminalContext`. The web's editor draws each
//! `t3-context://` link as an inline chip; this prompt is plain text, so the links it holds show
//! as chips in a row above it, in prompt order, each with an X that removes its link. Rules live
//! in `t3_core::composer::context`.

use std::collections::HashMap;
use std::time::Instant;

use gpui::{AnyElement, App, Context, Hsla, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, h_flex, v_flex};
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
    /// The chip whose details popover is open, by context id.
    open: Option<String>,
    dismissed: Option<Instant>,
    /// The links the prompt held at the last edit, so other edits do not repaint the row.
    linked: Vec<String>,
}

/// One chip in the row, resolved against what the composer holds.
enum Chip {
    Terminal(TerminalContext),
    Thread(ThreadContext),
    /// A link whose context this composer does not have (`UnresolvedContextChip`).
    Unresolved { context_id: String, label: String },
}

impl Chip {
    fn context_id(&self) -> String {
        match self {
            Chip::Terminal(context) => context.context_id(),
            Chip::Thread(thread) => thread.context_id.clone(),
            Chip::Unresolved { context_id, .. } => context_id.clone(),
        }
    }
}

/// How a chip's ink and icon read (`contextChipVariants` `kind` and `state`).
#[derive(Clone, Copy)]
enum Ink {
    Accent(u32),
    Invalid,
    /// `unresolved`: the surrounding text color.
    Current,
}

impl Composer {
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
        let linked: Vec<String> = context_references(text).into_iter().map(|reference| reference.context_id.to_owned()).collect();
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
            chips.push(match (terminal, thread) {
                (Some(terminal), _) => Chip::Terminal(terminal.clone()),
                (_, Some(thread)) => Chip::Thread(thread.clone()),
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
                .flex_wrap()
                .gap_x(px(12.))
                .gap_y(px(4.))
                .mb(px(8.))
                .children(chips.into_iter().map(|chip| self.render_chip(chip, cx)))
                .into_any_element(),
        )
    }

    fn render_chip(&self, chip: Chip, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let context_id = chip.context_id();
        match chip {
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
                .tooltip(text_tooltip("Remove".to_owned()))
                .on_click(cx.listener(move |this, _, window, cx| this.remove_context(&remove_id, window, cx)))
                .child(Icon::X.render(px(CHIP_TEXT * 0.85), color)),
        )
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

fn text_tooltip(text: String) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView {
    let text = SharedString::from(text);
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

/// `ContextChipShell`: icon and label in the kind's ink at the prompt's size, medium weight; a
/// chip that opens something underlines its label on hover.
fn chip_shell(context_id: &str, icon: Icon, label: String, ink: Ink, clickable: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
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
        .child(div().flex_none().child(icon.render(px(CHIP_TEXT), color)))
        .child(
            div()
                .min_w_0()
                .truncate()
                .when(clickable, |this| this.group_hover(group, |style| style.underline()))
                .child(SharedString::from(label)),
        )
}
