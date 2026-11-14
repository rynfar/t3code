//! What the agent waits on, attached above the composer: the question banner with its options
//! (`ComposerPendingUserInputPanel.tsx`), plus the answering flow from `usePendingThreadRequests.ts`:
//! the prompt edits the active question's own answer while the thread's draft waits aside.

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, Styled, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::commands;
use t3_core::requests::{self, PendingUserInput, Question};

use super::controls::{Button, ButtonSize, ButtonVariant, Float, MenuItem, checkbox, floating, kbd, menu_surface, radio};
use super::{AUTO_ADVANCE, Composer, Target};
use crate::app_state::uuid_v4;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// `ComposerBanner` surface: the composer's surface (opaque, the no-blur fallback) inset 22px per
/// side and tucked 17px under the card, or detached (`rounded(14px)`) under the Draft Hero.
pub(crate) fn banner_surface_card(warning: bool, detached: bool, content: impl IntoElement, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let outline = if warning {
        hsla(p.warning).opacity(0.28)
    } else if dark {
        gpui::white().opacity(0.05)
    } else {
        hsla(p.foreground).opacity(0.08)
    };
    let card = div()
        .border_1()
        .border_color(outline)
        .bg(hsla(if dark { p.surface_raised } else { p.card }));
    if detached {
        return card.rounded(px(14.)).child(
            div()
                .rounded(px(13.))
                .py(px(6.))
                .px(px(6.))
                .when(warning, |this| this.bg(hsla(p.warning).opacity(0.08)))
                .text_xs()
                .line_height(px(16.))
                .child(content),
        );
    }
    card.mx(px(22.)).mb(px(-17.)).rounded_t(px(16.)).child(
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

pub(crate) fn banner_surface(warning: bool, content: impl IntoElement, cx: &App) -> gpui::Div {
    banner_surface_card(warning, false, content, cx)
}

impl Composer {
    pub(super) fn active_input(&self) -> Option<&PendingUserInput> {
        self.snapshot.pending.user_inputs.first()
    }

    fn question_index(&self, input: &PendingUserInput) -> usize {
        let index = self.answers.get(&self.answer_key(&input.request_id)).map(|answers| answers.index).unwrap_or(0);
        index.min(input.questions.len().saturating_sub(1))
    }

    pub(super) fn active_question(&self) -> Option<(String, Question)> {
        let input = self.active_input()?;
        Some((input.request_id.clone(), input.questions.get(self.question_index(input))?.clone()))
    }

    pub(super) fn progress(&self, cx: &gpui::App) -> Option<requests::Progress> {
        let input = self.active_input()?;
        let answers = self.answers.get(&self.answer_key(&input.request_id))?;
        Some(requests::progress(&input.questions, &self.question_answer_drafts(input, cx), answers.index))
    }

    /// Enters or leaves answering when the first pending question changes.
    pub(super) fn sync_answering(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_input().map(|input| self.answer_key(&input.request_id));
        if active == self.answering {
            return;
        }
        self.close_citation_comment("outside-press", window, cx);
        if self.answering.is_none() {
            self.stash = Some(self.prompt(cx));
        }
        self.answering = active.clone();
        self.question_collapsed = None;
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

    /// Every question change lands here, so it also drops a pending auto-advance.
    fn show_active_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_pending_send();
        self.auto_advance = None;
        let text = self.progress(cx).map(|progress| progress.custom_answer).unwrap_or_default();
        self.set_prompt(&text, window, cx);
        if self.active_question().is_some_and(|(_, question)| !question.allows_custom_answer()) {
            window.focus(&self.question_focus, cx);
        }
    }

    /// `onAdvanceActivePendingUserInput`: next question, or submit on the last.
    pub(super) fn advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) {
            return;
        }
        let Some(input) = self.active_input().cloned() else { return };
        if !self.can_answer(cx) || !input.can_respond() || self.question_responding(&input.request_id, cx) {
            return;
        }
        let drafts = self.question_answer_drafts(&input, cx);
        let key = self.answer_key(&input.request_id);
        let Some(answers) = self.answers.get_mut(&key) else { return };
        let progress = requests::progress(&input.questions, &drafts, answers.index);
        if progress.is_last_question {
            if let Some(built) = requests::build_answers(&input.questions, &drafts) {
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
        if self.is_rewinding(cx) || !self.can_answer(cx) || self.active_input().is_some_and(|input| self.question_responding(&input.request_id, cx)) { return; }
        let Some(answers) = self.answering.as_ref().and_then(|request_id| self.answers.get_mut(request_id)) else { return };
        answers.index = answers.index.saturating_sub(1);
        self.show_active_answer(window, cx);
        cx.notify();
    }

    /// Picks an option. Single-select questions move on once the pick settles, as the web does.
    pub(super) fn toggle_option(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) || !self.can_answer(cx) {
            return;
        }
        let Some((request_id, question)) = self.active_question() else { return };
        if self.question_responding(&request_id, cx) {
            return;
        }
        let key = self.answer_key(&request_id);
        let Some(answers) = self.answers.get_mut(&key) else { return };
        let draft = answers.drafts.get(&question.id);
        let displaced = draft.and_then(|draft| draft.custom.clone()).unwrap_or_default();
        let next = requests::toggle_option(&question, draft, &value);
        answers.drafts.insert(question.id.clone(), next);
        if let Some(stash) = self.stash.as_mut() {
            let next_stash = requests::carry_displaced_answer(stash, &displaced);
            let carried = !displaced.trim().is_empty() && next_stash != *stash;
            *stash = next_stash;
            if carried {
                self.state.update(cx, |state, cx| {
                    state.show_toast(
                        crate::toast::Toast::new(crate::toast::ToastKind::Info, "Your typed text was set aside")
                            .description("It will be back in the composer once the question is answered."),
                        cx,
                    );
                });
            }
        }
        self.set_prompt("", window, cx);
        if !question.multi_select {
            let picked = (self.answer_key(&request_id), question.id);
            self.auto_advance = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(AUTO_ADVANCE).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.auto_advance = None;
                    let active = this.active_question().map(|(request_id, question)| (this.answer_key(&request_id), question.id));
                    if active.as_ref() == Some(&picked) {
                        this.advance(window, cx);
                    }
                });
            }));
        }
        cx.notify();
    }

    fn submit_answers(&mut self, request_id: String, answers: serde_json::Map<String, serde_json::Value>, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) {
            return;
        }
        let Some(Target::Thread(thread_id)) = self.snapshot.target.clone() else { return };
        let Some(input) = self.active_input().filter(|input| input.request_id == request_id && input.can_respond()).cloned() else { return };
        if !self.can_answer(cx) || self.question_responding(&request_id, cx) { return; }
        let Some(attachments) = self.question_response_attachments(&input, cx) else { return };
        let key = self.answer_key(&request_id);
        self.responding.insert(key.clone());
        let command = commands::respond_to_user_input_with_attachments(&uuid_v4(), &thread_id, &request_id, answers, attachments);
        self.run_question_request(key, command, "Failed to submit user input.", cx);
    }

    fn dismiss(&mut self, request_id: String, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) {
            return;
        }
        let Some(Target::Thread(thread_id)) = self.snapshot.target.clone() else { return };
        if !self.can_answer(cx) || self.question_responding(&request_id, cx) || !self.active_input().is_some_and(|input| input.request_id == request_id && input.can_respond() && input.dismissible) { return; }
        let key = self.answer_key(&request_id);
        self.responding.insert(key.clone());
        let command = commands::dismiss_user_input(&uuid_v4(), &thread_id, &request_id);
        self.run_question_request(key, command, "Failed to dismiss the question.", cx);
    }

    fn is_question_collapsed(&self, input: &PendingUserInput) -> bool {
        let index = self.question_index(input);
        input.questions.get(index).is_some_and(|question| self.question_collapsed.as_deref() == Some(question.id.as_str()))
    }

    /// The digit shortcuts (1–9) pick options while the prompt is not focused and the question is expanded.
    pub(super) fn pick_by_digit(&mut self, digit: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.is_rewinding(cx) || !self.can_answer(cx) {
            return false;
        }
        if self.active_input().is_some_and(|input| self.is_question_collapsed(input)) {
            return false;
        }
        let Some((_, question)) = self.active_question() else { return false };
        let Some(option) = question.options.get(digit.wrapping_sub(1)) else { return false };
        self.toggle_option(option.answer_value().to_owned(), window, cx);
        true
    }

    pub(super) fn render_question(&mut self, input: &PendingUserInput, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let index = self.question_index(input);
        let Some(question) = input.questions.get(index).cloned() else { return div().into_any_element() };
        let collapsed = self.question_collapsed.as_deref() == Some(question.id.as_str());
        let responding = self.is_rewinding(cx) || !self.can_answer(cx) || self.question_responding(&input.request_id, cx) || !input.can_respond();
        let drafts = self.answers.get(&self.answer_key(&input.request_id)).map(|answers| &answers.drafts);
        let draft = drafts.and_then(|drafts| drafts.get(&question.id)).cloned();
        let request_id = input.request_id.clone();
        let question_id = question.id.clone();
        let prompt_markdown = (!collapsed).then(|| {
            let text: Arc<str> = Arc::from(question.question.as_str());
            let key = format!("composer-question:{}:{}", self.answer_key(&input.request_id), question.id);
            let action_ctx = crate::transcript::markdown::links::MarkdownActionContext {
                state: self.state.clone(),
                owner: cx.entity().entity_id(),
                cwd: self.menu_cwd(cx),
                thread_id: self.thread_id(),
                thread_view: None,
            };
            self.question_markdown.state_in_context(&key, &text, Some(action_ctx), cx)
        });
        let collapsed_preview = question.question.replace('\n', " ");
        let content = v_flex()
            .child(
                h_flex()
                    .h(px(28.))
                    .pl(px(16.))
                    .pr(px(4.))
                    .gap(px(4.))
                    .child(
                        h_flex()
                            .id("question-toggle")
                            .debug_selector(|| "question-toggle".to_owned())
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .items_center()
                            .gap(px(6.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .hover(|this| this.bg(hsla(p.foreground).opacity(0.05)))
                            .tooltip(move |window, cx| {
                                let label = if collapsed { "Show the question and its options" } else { "Hide the question and its options" };
                                gpui_component::tooltip::Tooltip::new(label).build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.question_collapsed = if collapsed { None } else { Some(question_id.clone()) };
                                cx.notify();
                            }))
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .items_center()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(hsla(p.muted_foreground))
                                            .when(collapsed, |this| this.max_w(gpui::relative(0.5)))
                                            .child(question.header.clone()),
                                    )
                                    .when(collapsed, |this| {
                                        this.child(div().flex_none().text_color(hsla(p.muted_foreground).opacity(0.4)).child("·"))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_color(hsla(p.secondary_label))
                                                    .child(collapsed_preview),
                                            )
                                    }),
                            )
                            .when(input.questions.len() > 1, |this| {
                                this.child(div().flex_none().px(px(6.)).text_color(hsla(p.muted_foreground)).child(format!("{}/{}", index + 1, input.questions.len())))
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .px(px(4.))
                                    .flex()
                                    .items_center()
                                    .child((if collapsed { Icon::ChevronRight } else { Icon::ChevronDown }).render(px(14.), hsla(p.muted_foreground))),
                            ),
                    )
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
            .when_some(prompt_markdown, |this, state| {
                this.child(
                    v_flex()
                        .id("question-body")
                        .debug_selector(|| "question-body".to_owned())
                        .max_h(px(384.))
                        .overflow_y_scroll()
                        .pl(px(22.))
                        .pr(px(16.))
                        .pb(px(4.))
                        .child(div().pt(px(8.)).child(crate::transcript::markdown::prose(&state, cx)))
                        .child(v_flex().mt(px(6.)).ml(px(-6.)).children(question.options.iter().enumerate().map(|(position, option)| {
                            let value = option.answer_value().to_owned();
                            let selected = requests::is_option_selected(&question, draft.as_ref(), &value);
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
                )
            });
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

    pub(super) fn render_plan_follow_up_banner(
        &self,
        plan: &t3_core::timeline::plan::ActiveProposedPlan,
        cx: &App,
    ) -> AnyElement {
        let p = palette(cx);
        let content = h_flex()
            .px(px(12.))
            .py(px(8.))
            .gap(px(8.))
            .items_center()
            .child(Icon::ListChecks.render(px(14.), hsla(p.primary)))
            .child(div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Plan ready"))
            .children(plan.title.clone().map(|title| {
                div().flex_1().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(title)
            }));
        banner_surface(false, content, cx).into_any_element()
    }

    pub(super) fn render_plan_primary(&self, has_text: bool, disabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        if has_text {
            return super::controls::Pill::new("composer-refine-plan", "Refine")
                .disabled(disabled)
                .on_click(cx.listener(|this, _, window, cx| this.submit_plan_follow_up(window, cx)))
                .into_any_element();
        }
        let open = self.plan_menu_open;
        div()
            .relative()
            .child(
                h_flex()
                    .h(px(28.))
                    .rounded_full()
                    .bg(hsla(p.message_action))
                    .text_color(hsla(p.message_action_foreground))
                    .when(disabled, |this| this.opacity(0.64))
                    .child(
                        h_flex()
                            .id("composer-implement-plan")
                            .h_full()
                            .pl(px(14.))
                            .pr(px(10.))
                            .rounded_l_full()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Implement")
                            .when(!disabled, |this| {
                                this.cursor_pointer()
                                    .hover(|this| this.bg(hsla(p.message_action_hover)))
                                    .on_click(cx.listener(|this, _, window, cx| this.submit_plan_follow_up(window, cx)))
                            }),
                    )
                    .child(div().w(px(1.)).h(px(16.)).bg(hsla(p.message_action_foreground).opacity(0.24)))
                    .child(
                        h_flex()
                            .id("composer-implement-plan-menu")
                            .h_full()
                            .px(px(8.))
                            .rounded_r_full()
                            .items_center()
                            .justify_center()
                            .child(Icon::ChevronDown.render(px(14.), hsla(p.message_action_foreground)))
                            .when(!disabled, |this| {
                                this.cursor_pointer()
                                    .hover(|this| this.bg(hsla(p.message_action_hover)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if Self::guard_reopen(&mut this.menu_dismissed) {
                                            return;
                                        }
                                        this.plan_menu_open = !this.plan_menu_open;
                                        cx.notify();
                                    }))
                            }),
                    ),
            )
            .when(open, |this| {
                this.child(floating(
                    Float::AboveEnd,
                    menu_surface(cx)
                        .occlude()
                        .mb(px(4.))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.plan_menu_open = false;
                            this.menu_dismissed = Some(std::time::Instant::now());
                            cx.notify();
                        }))
                        .child(
                            MenuItem::new("composer-implement-new-thread", "Implement in a new thread")
                                .icon(Icon::GitBranchPlus, hsla(p.muted_foreground))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.plan_menu_open = false;
                                    this.submit_plan_in_new_thread(window, cx);
                                })),
                        ),
                ))
            })
            .into_any_element()
    }
}
