//! What the agent waits on, attached above the composer: the approval banner with its decisions
//! (`TimelinePendingApprovalRow.tsx`, `ComposerPendingApprovalActions.tsx`) and the question
//! banner with its options (`ComposerPendingUserInputPanel.tsx`), plus the answering flow from
//! `usePendingThreadRequests.ts`: the prompt edits the active question's own answer while the
//! thread's draft waits aside.

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::commands::{self, ApprovalDecision};
use t3_core::requests::{self, PendingApproval, PendingUserInput, Question};

use super::controls::{Button, ButtonSize, ButtonVariant, Float, MenuItem, checkbox, floating, kbd, menu_surface, radio};
use super::{AUTO_ADVANCE, Composer, Target};
use crate::app_state::uuid_v4;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// `ComposerBanner` surface: the composer's surface (opaque, the no-blur fallback) inset 22px per
/// side and tucked 17px under the card. Warnings tint it and its outline.
fn banner_surface(warning: bool, content: impl IntoElement, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let outline = if warning {
        hsla(p.warning).opacity(0.28)
    } else if dark {
        gpui::white().opacity(0.05)
    } else {
        hsla(p.foreground).opacity(0.08)
    };
    div()
        .mx(px(22.))
        .mb(px(-17.))
        .rounded_t(px(16.))
        .border_1()
        .border_color(outline)
        .bg(hsla(if dark { p.surface_raised } else { p.card }))
        .child(
            div()
                .rounded_t(px(15.))
                .pt(px(4.))
                .pb(px(21.))
                .px(px(4.))
                .when(warning, |this| this.bg(hsla(p.warning).opacity(0.08)))
                .text_xs()
                .line_height(px(16.))
                .child(content),
        )
}

impl Composer {
    pub(super) fn active_input(&self) -> Option<&PendingUserInput> {
        self.snapshot.pending.user_inputs.first()
    }

    fn question_index(&self, input: &PendingUserInput) -> usize {
        let index = self.answers.get(&input.request_id).map(|answers| answers.index).unwrap_or(0);
        index.min(input.questions.len().saturating_sub(1))
    }

    pub(super) fn active_question(&self) -> Option<(String, Question)> {
        let input = self.active_input()?;
        Some((input.request_id.clone(), input.questions.get(self.question_index(input))?.clone()))
    }

    pub(super) fn progress(&self) -> Option<requests::Progress> {
        let input = self.active_input()?;
        let answers = self.answers.get(&input.request_id)?;
        Some(requests::progress(&input.questions, &answers.drafts, answers.index))
    }

    /// Enters or leaves answering when the first pending question changes.
    pub(super) fn sync_answering(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_input().map(|input| input.request_id.clone());
        if active == self.answering {
            return;
        }
        if self.answering.is_none() {
            self.stash = Some(self.prompt(cx));
        }
        self.answering = active.clone();
        self.auto_advance = None;
        match active {
            Some(request_id) => {
                let questions = self.active_input().map(|input| input.questions.clone()).unwrap_or_default();
                let thread_id = self.thread_id().unwrap_or_default();
                let answers = self.answers.entry(request_id).or_insert_with(|| super::Answers { thread_id, ..Default::default() });
                requests::seed_drafts(&questions, &mut answers.drafts);
                self.show_active_answer(window, cx);
            }
            None => {
                let text = self.stash.take().unwrap_or_default();
                self.set_prompt(&text, window, cx);
            }
        }
    }

    fn show_active_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.progress().map(|progress| progress.custom_answer).unwrap_or_default();
        self.set_prompt(&text, window, cx);
        if self.active_question().is_some_and(|(_, question)| !question.allows_custom_answer()) {
            window.focus(&self.question_focus, cx);
        }
    }

    /// `onAdvanceActivePendingUserInput`: next question, or submit on the last.
    pub(super) fn advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.active_input().cloned() else { return };
        if !input.can_respond() || self.responding.contains(&input.request_id) {
            return;
        }
        let Some(answers) = self.answers.get_mut(&input.request_id) else { return };
        let progress = requests::progress(&input.questions, &answers.drafts, answers.index);
        if progress.is_last_question {
            if let Some(built) = requests::build_answers(&input.questions, &answers.drafts) {
                self.submit_answers(input.request_id, built, cx);
            }
            return;
        }
        if progress.can_advance {
            answers.index = progress.question_index + 1;
            self.show_active_answer(window, cx);
            cx.notify();
        }
    }

    pub(super) fn previous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(answers) = self.answering.as_ref().and_then(|request_id| self.answers.get_mut(request_id)) else { return };
        answers.index = answers.index.saturating_sub(1);
        self.show_active_answer(window, cx);
        cx.notify();
    }

    /// Picks an option. Single-select questions move on once the pick settles, as the web does.
    pub(super) fn toggle_option(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some((request_id, question)) = self.active_question() else { return };
        if self.responding.contains(&request_id) {
            return;
        }
        let Some(answers) = self.answers.get_mut(&request_id) else { return };
        let draft = answers.drafts.get(&question.id);
        let displaced = draft.and_then(|draft| draft.custom.clone()).unwrap_or_default();
        let next = requests::toggle_option(&question, draft, &value);
        answers.drafts.insert(question.id.clone(), next);
        if let Some(stash) = self.stash.as_mut() {
            *stash = requests::carry_displaced_answer(stash, &displaced);
        }
        self.set_prompt("", window, cx);
        if !question.multi_select {
            self.auto_advance = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(AUTO_ADVANCE).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.auto_advance = None;
                    this.advance(window, cx);
                });
            }));
        }
        cx.notify();
    }

    fn submit_answers(&mut self, request_id: String, answers: serde_json::Map<String, serde_json::Value>, cx: &mut Context<Self>) {
        let Some(Target::Thread(thread_id)) = self.snapshot.target.clone() else { return };
        if !self.responding.insert(request_id.clone()) {
            return;
        }
        let command = commands::respond_to_user_input(&uuid_v4(), &thread_id, &request_id, answers);
        self.run_request(request_id, command, "Failed to submit user input.", cx);
    }

    fn dismiss(&mut self, request_id: String, cx: &mut Context<Self>) {
        let Some(Target::Thread(thread_id)) = self.snapshot.target.clone() else { return };
        if !self.responding.insert(request_id.clone()) {
            return;
        }
        let command = commands::dismiss_user_input(&uuid_v4(), &thread_id, &request_id);
        self.run_request(request_id, command, "Failed to dismiss the question.", cx);
    }

    /// The digit shortcuts (1–9) pick options while the prompt is not focused.
    pub(super) fn pick_by_digit(&mut self, digit: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((_, question)) = self.active_question() else { return false };
        let Some(option) = question.options.get(digit.wrapping_sub(1)) else { return false };
        self.toggle_option(option.answer_value().to_owned(), window, cx);
        true
    }

    pub(super) fn render_approval(&self, approval: &PendingApproval, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let (label, detail_label) = approval.presentation();
        let kind_icon = match approval.request_kind.as_str() {
            "command" => Icon::Terminal,
            "file-change" => Icon::SquarePen,
            "file-read" => Icon::Eye,
            "mcp-elicitation" => Icon::Hammer,
            _ => Icon::Wrench,
        };
        let responding = self.responding.contains(&approval.request_id);
        let (primary, more) = approval.split_options();
        let more: Vec<(ApprovalDecision, String, bool)> =
            more.into_iter().map(|option| (option.decision, option.label.clone(), option.warning.is_some())).collect();
        let actions = h_flex().flex_wrap().gap(px(8.)).children(primary.into_iter().map(|option| {
            let (request_id, decision) = (approval.request_id.clone(), option.decision);
            let variant = if decision == ApprovalDecision::Accept { ButtonVariant::Default } else { ButtonVariant::Outline };
            let mut button = Button::new(SharedString::from(format!("approval-{}", decision.as_str())), variant, ButtonSize::Xs)
                .label(option.label.clone())
                .disabled(responding)
                .on_click(cx.listener(move |this, _, _, cx| this.respond_to_approval(request_id.clone(), decision, cx)));
            if option.warning.is_some() {
                button = button.icon(Icon::TriangleAlert);
            }
            if let Some(warning) = &option.warning {
                button = button.tooltip(warning.clone());
            }
            button
        }));
        let actions = actions.when(!more.is_empty(), |this| {
            this.child(
                div().relative().child(
                    Button::new("approval-more", ButtonVariant::Outline, ButtonSize::IconXs)
                        .icon(Icon::Ellipsis)
                        .tooltip("More approval options")
                        .disabled(responding)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if Self::guard_reopen(&mut this.menu_dismissed) {
                                return;
                            }
                            this.approval_menu_open = !this.approval_menu_open;
                            cx.notify();
                        })),
                )
                .when(self.approval_menu_open, |this| {
                    let request_id = approval.request_id.clone();
                    this.child(floating(
                        Float::AboveEnd,
                        menu_surface(cx)
                                    .occlude()
                                    .mb(px(4.))
                                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                        this.approval_menu_open = false;
                                        this.menu_dismissed = Some(std::time::Instant::now());
                                        cx.notify();
                                    }))
                                    .children(more.iter().map(|(decision, label, warning)| {
                                        let (request_id, decision) = (request_id.clone(), *decision);
                                        let item = MenuItem::new(SharedString::from(format!("more-{}", decision.as_str())), label.clone())
                                            .on_click(cx.listener(move |this, _, _, cx| this.respond_to_approval(request_id.clone(), decision, cx)));
                                        if *warning { item.icon(Icon::TriangleAlert, hsla(p.warning)) } else { item }
                                    })),
                    ))
                }),
            )
        });
        let content = v_flex()
            .child(
                h_flex()
                    .h(px(28.))
                    .px(px(4.))
                    .gap(px(4.))
                    .child(div().w(px(24.)).flex().justify_center().child(kind_icon.render(px(16.), hsla(p.icon_muted))))
                    .child(div().flex_1().min_w_0().truncate().text_color(hsla(p.muted_foreground)).child(label))
                    .when(count > 1, |this| this.child(div().px(px(6.)).text_color(hsla(p.muted_foreground)).child(format!("1/{count}")))),
            )
            .child(
                v_flex()
                    .pl(px(32.))
                    .pr(px(12.))
                    .pb(px(6.))
                    .gap(px(4.))
                    .child(
                        div()
                            .id("approval-detail")
                            .max_h(px(160.))
                            .overflow_y_scroll()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .line_height(px(20.))
                            .text_color(hsla(p.foreground))
                            .child(SharedString::from(approval.detail.clone().unwrap_or_else(|| detail_label.to_owned()))),
                    )
                    .children(approval.app_name.clone().map(|name| div().text_color(hsla(p.muted_foreground)).child(name)))
                    .children(approval.reason.clone().map(|reason| div().text_color(hsla(p.muted_foreground)).child(reason)))
                    .child(match approval.live {
                        true => h_flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(
                                h_flex()
                                    .gap(px(4.))
                                    .text_color(hsla(p.warning_foreground))
                                    .child(Icon::Shield.render(px(12.), hsla(p.warning_foreground)))
                                    .child("Waiting for approval"),
                            )
                            .child(actions)
                            .children(approval.full_access_override.as_deref().map(|kind| {
                                let explanation = requests::full_access_override_explanation(kind);
                                div()
                                    .id("full-access-override")
                                    .text_color(hsla(p.muted_foreground))
                                    .hover(|this| this.underline())
                                    .child(requests::FULL_ACCESS_OVERRIDE_QUESTION)
                                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(explanation).build(window, cx))
                            }))
                            .into_any_element(),
                        false => div()
                            .text_color(hsla(p.warning_foreground))
                            .child("Provider process is gone. Interrupt or restart the run to respond.")
                            .into_any_element(),
                    }),
            );
        banner_surface(true, content, cx).into_any_element()
    }

    pub(super) fn render_question(&self, input: &PendingUserInput, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let index = self.question_index(input);
        let Some(question) = input.questions.get(index) else { return div().into_any_element() };
        let responding = self.responding.contains(&input.request_id) || !input.can_respond();
        let drafts = self.answers.get(&input.request_id).map(|answers| &answers.drafts);
        let draft = drafts.and_then(|drafts| drafts.get(&question.id));
        let request_id = input.request_id.clone();
        let content = v_flex()
            .child(
                h_flex()
                    .h(px(28.))
                    .pl(px(16.))
                    .pr(px(4.))
                    .gap(px(4.))
                    .child(div().flex_1().min_w_0().truncate().text_color(hsla(p.muted_foreground)).child(question.header.clone()))
                    .when(input.questions.len() > 1, |this| {
                        this.child(div().px(px(6.)).text_color(hsla(p.muted_foreground)).child(format!("{}/{}", index + 1, input.questions.len())))
                    })
                    .when(input.dismissible, |this| {
                        this.child(
                            Button::new("question-dismiss", ButtonVariant::Outline, ButtonSize::IconXs)
                                .icon(Icon::X)
                                .tooltip("Dismiss question without answering")
                                .disabled(responding)
                                .on_click(cx.listener(move |this, _, _, cx| this.dismiss(request_id.clone(), cx))),
                        )
                    }),
            )
            .child(
                v_flex()
                    .id("question-body")
                    .max_h(px(384.))
                    .overflow_y_scroll()
                    .pl(px(22.))
                    .pr(px(16.))
                    .pb(px(4.))
                    .child(div().pt(px(8.)).text_sm().line_height(px(22.75)).text_color(hsla(p.foreground).opacity(0.85)).child(question.question.clone()))
                    .child(v_flex().mt(px(6.)).ml(px(-6.)).children(question.options.iter().enumerate().map(|(position, option)| {
                        let value = option.answer_value().to_owned();
                        let selected = requests::is_option_selected(question, draft, &value);
                        h_flex()
                            .id(("question-option", position))
                            .items_start()
                            .gap(px(10.))
                            .py(px(6.))
                            .pl(px(6.))
                            .rounded(px(6.))
                            .text_color(if selected { hsla(p.foreground) } else { hsla(p.foreground).opacity(0.7) })
                            .when(responding, |this| this.opacity(0.5))
                            .when(!responding, |this| {
                                this.cursor_pointer()
                                    .when(!selected, |this| this.hover(|this| this.bg(hsla(p.muted).opacity(0.3))))
                                    .on_click(cx.listener(move |this, _, window, cx| this.toggle_option(value.clone(), window, cx)))
                            })
                            .child(div().h(px(20.)).flex().items_center().child(if question.multi_select {
                                checkbox(selected, cx)
                            } else {
                                radio(selected, cx)
                            }))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(div().text_sm().line_height(px(20.)).font_weight(FontWeight::MEDIUM).child(option.label.clone()))
                                    .when(!option.description.is_empty() && option.description != option.label, |this| {
                                        this.child(div().text_size(px(11.)).text_color(hsla(p.secondary_label)).child(option.description.clone()))
                                    }),
                            )
                            .when(position < 9, |this| {
                                this.child(div().h(px(20.)).w(px(24.)).flex().items_center().justify_center().child(kbd((position + 1).to_string(), cx)))
                            })
                    }))),
            );
        banner_surface(false, content, cx)
            .track_focus(&self.question_focus)
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, window, cx| window.focus(&this.question_focus, cx)))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                if keystroke.modifiers.modified() {
                    return;
                }
                if let Ok(digit) = keystroke.key.parse::<usize>()
                    && (1..=9).contains(&digit)
                    && this.pick_by_digit(digit, window, cx)
                {
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
    }
}
