//! The composer's layout (design-system §7.9, `ChatComposer.tsx`, `ComposerControlsBar.tsx`,
//! `ComposerFooterControls.tsx`, `ComposerPrimaryActions.tsx`): attached banners, the glass card
//! with the prompt, and the footer with the model picker, permissions, mode and send controls.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, Entity, ExternalPaths, Focusable, Hsla, IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled, Window,
    div, prelude::*, px,
};
use gpui_component::input::{Copy, Editor, Enter, Escape, IndentInline, MoveDown, MoveUp, Paste};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::commands::ComposerDispatch;
use t3_core::composer::{self as rules, runtime_mode_description, runtime_mode_label};
use t3_core::files::media_preview::DraggedFileMention;
use t3_core::requests;
use t3_core::sidebar::tag_filter::ThreadKey;
use t3_core::traits;

use super::controls::{
    Button, ButtonSize, ButtonVariant, ComposerControl, EffortBars, Float, Pill, RoundButton, RoundKind, floating, kbd, menu_surface,
    resolve_effort_glyph,
};
use super::model_picker::permissions_section;
use super::{Composer, MenuKey, OpenPermissions, OutdentList, PasteAsText, Resolved, StashPrompt, StopPhase, Target, ToggleModelPicker, runtime_mode_icon};
use crate::i18n::t;
use crate::icons::Icon;
use crate::provider_icon::{BadgeSize, ProviderIcon};
use crate::theme::{
    SlotState, ThemeSlot, ThemeSlotExt, ThemeSlotHoverExt, ThemeSlotInteractiveExt,
    apply_slot_override, hsla, palette, slot_has_state_background, slot_style,
    theme_slot_background_layer, theme_slot_pseudo_before, theme_slot_state_background_layer,
};

/// `DEFAULT_COMPOSER_PLACEHOLDER` (`web/src/composerPlaceholder.ts`).
pub const DEFAULT_PLACEHOLDER: &str = "Ask anything. @ files, $ skills, / commands";

/// A click that closed a popup must not reopen it through the trigger it landed on.
const REOPEN_GUARD: Duration = Duration::from_millis(300);

type Card = gpui::Stateful<gpui::Div>;
type ThreadDropReader<T> = for<'a> fn(&'a T) -> Option<(&'a str, Option<&'a ThreadKey>)>;
type InstallThreadDrop = dyn Fn(Card, Entity<Composer>, Hsla, Hsla) -> Card;
struct ThreadDrop(std::rc::Rc<InstallThreadDrop>);
impl gpui::Global for ThreadDrop {}

pub(crate) fn register_thread_drop<T: 'static>(cx: &mut App, read: ThreadDropReader<T>) {
    if cx.has_global::<ThreadDrop>() {
        return;
    }
    cx.set_global(ThreadDrop(std::rc::Rc::new(move |card, composer, fill, ring| {
        card.drag_over::<T>(move |style, drag, _, _| {
            if read(drag).is_some() {
                style.bg(fill).border_color(ring)
            } else {
                style
            }
        })
        .can_drop(move |drag, _, _| drag.downcast_ref::<T>().is_none_or(|drag| read(drag).is_some()))
        .on_drop(move |drag: &T, window, cx| {
            if let Some((id, key)) = read(drag) {
                composer.update(cx, |composer, cx| composer.drop_thread_reference(id, key, window, cx));
            }
        })
    })));
}

impl Composer {
    /// `ChatComposer` placeholders, most specific first.
    fn placeholder(&self, cx: &App) -> String {
        if let Some(approval) = self.snapshot.pending.approvals.first() {
            return if approval.supports_feedback {
                t(cx, "Send feedback to decline this request").to_owned()
            } else {
                t(cx, "Send a message to decline and redirect the agent").to_owned()
            };
        }
        if let Some((_, question)) = self.active_question() {
            return if question.allows_custom_answer() {
                t(cx, "Type your own answer, or leave this blank to use the selected option").to_owned()
            } else {
                t(cx, "Choose an option above").to_owned()
            };
        }
        if self.offered_prompt_suggestion(cx).is_some() {
            return String::new();
        }
        t(cx, DEFAULT_PLACEHOLDER).to_owned()
    }

    fn input_disabled(&self, cx: &App) -> bool {
        self.is_rewinding(cx)
            || self.active_input().is_some_and(|input| {
                !self.can_answer(cx) || !input.can_respond() || self.question_responding(&input.request_id, cx)
            })
            || self.active_question().is_some_and(|(_, question)| !question.allows_custom_answer())
    }

    fn can_drop_file_mention(&self, cx: &App) -> bool {
        !self.is_rewinding(cx)
            && !self.input_disabled(cx)
            && !self.voice.read(cx).freezes_editor()
            && self.snapshot.target.is_some()
            && self.answering.is_none()
    }

    fn drop_thread_reference(&mut self, id: &str, key: Option<&ThreadKey>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_drop_file_mention(cx) || self.editing_queued().is_some() {
            return;
        }
        let target = self.snapshot.target.clone().unwrap();
        let environment = self.environment_id(cx);
        let state = self.state.read(cx);
        let context = match key {
            Some(key) if key.environment_id != environment => {
                let thread_id = &key.thread_id;
                let title = state.fleet.environments.iter().find(|entry| {
                    entry.summary.owner.saved_id == key.environment_id
                        || entry.summary.owner.canonical_id.as_deref() == Some(key.environment_id.as_str())
                }).and_then(|entry| entry.summary.threads.iter().find(|thread| thread.thread.id == *thread_id))
                    .map(|thread| thread.thread.title.as_str())
                    .unwrap_or_else(|| t(cx, "Thread"));
                t3_core::composer::menu::ThreadContext::new(&key.environment_id, thread_id, title)
            }
            _ => {
                let Some(thread) = state.thread_shell(id) else { return };
                t3_core::composer::menu::ThreadContext::new(&environment, id, &thread.title)
            }
        };
        let prompt = self.prompt(cx);
        let range = self.input.read(cx).selected_range();
        let before = prompt.get(..range.start).and_then(|text| text.chars().next_back());
        let separator = if before.is_some_and(|ch| !ch.is_whitespace()) { " " } else { "" };
        self.replace_prompt_range(range.start, range.end, &format!("{separator}{} ", context.reference()), window, cx);
        self.remember_thread_context(target, context);
        self.draft_changed(cx);
        self.focus(window, cx);
    }

    /// Drop target for one or more files dragged from the Files explorer (`composerMentionDragHandlers`).
    pub(super) fn on_drop_file_mention(
        &mut self,
        dragged: &DraggedFileMention,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_drop_file_mention(cx) {
            return;
        }
        let mut inserted = false;
        for path in &dragged.paths {
            let trimmed = path.trim().trim_end_matches('/');
            if trimmed.is_empty() {
                continue;
            }
            self.insert_mention(trimmed, window, cx);
            inserted = true;
        }
        if inserted {
            cx.notify();
        }
    }

    /// Placeholder and disabled/readonly state follow what the agent waits on; written only on change.
    pub(super) fn sync_input_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = self.placeholder(cx);
        let disabled = self.input_disabled(cx);
        let readonly = self.voice.read(cx).freezes_editor();
        if self.input_state.as_ref() == Some(&(placeholder.clone(), disabled, readonly)) {
            return;
        }
        self.input_state = Some((placeholder.clone(), disabled, readonly));
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            input.set_disabled(disabled, cx);
            input.set_readonly(readonly, cx);
        });
    }

    pub(super) fn guard_reopen(dismissed: &mut Option<Instant>) -> bool {
        dismissed.take().is_some_and(|at| at.elapsed() < REOPEN_GUARD)
    }

    pub(super) fn render_model_trigger(&self, resolved: &Resolved, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let multi = !resolved.multi_models.is_empty();
        let suffix = traits::effort_suffix(&resolved.driver, &resolved.descriptors);
        let surface = hsla(if cx.theme().is_dark() { p.surface_raised } else { p.card });
        let mark = if multi {
            let remaining = resolved.multi_models.len().saturating_sub(3);
            Some(h_flex().flex_none()
                .children(resolved.multi_model_marks.iter().enumerate().map(|(index, mark)| {
                    div().flex_none().size(px(18.)).rounded_full().bg(surface)
                        .items_center().justify_center().flex()
                        .when(index > 0, |this| this.ml(px(-4.)))
                        .child(ProviderIcon::new(mark.clone()).size(px(16.)))
                }))
                .when(remaining > 0, |this| this.child(div().ml(px(-4.)).size(px(18.))
                    .flex_none().rounded_full().bg(surface).text_size(px(9.))
                    .flex().items_center().justify_center().child(format!("+{remaining}"))))
                .into_any_element())
        } else {
            resolved.mark.clone().map(|mark| ProviderIcon::new(mark).badge(BadgeSize::Compact, surface).into_any_element())
        };
        let label = if let Some(multi_label) = &resolved.multi_model_label {
            multi_label.clone()
        } else if resolved.model_label.is_empty() {
            t(cx, "Choose model").to_owned()
        } else {
            resolved.model_label.clone()
        };
        let suffix_color = if self.picker_open { hsla(p.foreground) } else { hsla(p.muted_foreground) };
        let effort_display = crate::settings::store::try_client(cx)
            .map_or(t3_core::client_settings::EffortDisplay::Label, t3_core::client_settings::ClientSettingsDoc::effort_display);
        let effort_glyph = traits::layout(&resolved.driver, &resolved.descriptors)
            .effort
            .and_then(|descriptor| match descriptor {
                t3_core::server_config::OptionDescriptor::Select(select) => {
                    let current = traits::select_value(select);
                    resolve_effort_glyph(select, current.as_deref())
                }
                t3_core::server_config::OptionDescriptor::Boolean(_) => None,
            });
        let (effort_word, effort_bars) = match (&effort_glyph, effort_display) {
            (Some(glyph), t3_core::client_settings::EffortDisplay::Bars) if glyph.special.is_none() => {
                (None, Some((glyph.count, glyph.filled)))
            }
            (Some(glyph), _) => (Some(glyph.special.clone().unwrap_or_else(|| glyph.label.clone())), None),
            (None, _) => (suffix.word.clone(), None),
        };
        let suffix = h_flex()
            .gap(px(6.))
            .text_color(suffix_color)
            .when(!multi, |this| {
                this.children(effort_word.map(|word| {
                    div()
                        .debug_selector(|| "composer-effort-word".to_owned())
                        .max_w(px(96.))
                        .truncate()
                        .child(word)
                }))
                .children(effort_bars.map(|(count, filled)| EffortBars::new(count, filled, suffix_color)))
                .when(suffix.fast || suffix.ultrafast, |this| this.child(Icon::ZapFill.render(px(12.), suffix_color)))
                .when(suffix.ultracode, |this| this.child(Icon::Workflow.render(px(12.), suffix_color)))
                .children(suffix.context_window.map(|window| div().max_w(px(96.)).truncate().text_xs().child(window)))
            });
        let tooltip_label = resolved.multi_model_tooltip.as_deref().unwrap_or(&label);
        let tooltip = [Some(tooltip_label.to_owned()), Some("⌘⇧M".to_owned())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        div()
            .relative()
            .min_w(px(52.))
            .child(
                ComposerControl::new("model-picker")
                    .when_some(mark, |control, mark| control.leading(mark))
                    .label(label)
                    .max_label_width(224.)
                    .suffix(suffix)
                    .pressed(self.picker_open)
                    .disabled(self.sending || self.is_rewinding(cx))
                    .tooltip(tooltip)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if Self::guard_reopen(&mut this.picker_dismissed) {
                            return;
                        }
                        this.toggle_picker(window, cx);
                    })),
            )
            .when(self.picker_open, |this| {
                this.child(floating(
                    Float::AboveStart,
                    div()
                        .mb(px(4.))
                        .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                            this.picker_open = false;
                            this.picker_dismissed = Some(Instant::now());
                            this.focus(window, cx);
                            cx.notify();
                        }))
                        .child(self.picker.clone()),
                ))
            })
            .into_any_element()
    }

    fn render_permissions_chip(&self, resolved: &Resolved, cx: &mut Context<Self>) -> AnyElement {
        let mode = resolved.runtime_mode;
        let modes = resolved.runtime_modes.clone();
        let rewinding = self.is_rewinding(cx);
        div()
            .relative()
            .child(
                ComposerControl::new("permissions")
                    .icon(runtime_mode_icon(mode))
                    .label(t(cx, runtime_mode_label(mode)))
                    .emphasized(true)
                    .pressed(self.permissions_open)
                    .disabled(rewinding)
                    .tooltip(format!("{}: {}. {}", t(cx, "Permissions"), t(cx, runtime_mode_label(mode)), t(cx, runtime_mode_description(mode))))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.is_rewinding(cx) || Self::guard_reopen(&mut this.menu_dismissed) {
                            return;
                        }
                        this.permissions_open = !this.permissions_open;
                        cx.notify();
                    })),
            )
            .when(self.permissions_open && !rewinding, |this| {
                this.child(floating(
                    Float::AboveStart,
                    menu_surface(cx)
                        .occlude()
                        .mb(px(4.))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.permissions_open = false;
                            this.menu_dismissed = Some(Instant::now());
                            cx.notify();
                        }))
                        .child(permissions_section(mode, &modes, cx, |mode, this: &mut Self, cx: &mut Context<Self>| {
                            this.set_runtime_mode(mode, cx)
                        })),
                ))
            })
            .into_any_element()
    }

    /// Renders the branch/worktree/environment strip below the composer for an active
    /// thread when `persistComposerContextStrip` is enabled (`BranchToolbar.logic.ts`
    /// `shouldShowComposerContextStrip` + `ChatView.tsx:1791-1799`).
    fn render_active_thread_context_strip(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.fixed.is_some() {
            return None;
        }
        let Target::Thread(thread_id) = self.snapshot.target.as_ref()? else {
            return None;
        };
        let persist = crate::settings::store::try_client(cx)
            .is_some_and(t3_core::client_settings::ClientSettingsDoc::persist_composer_context_strip);
        let state = self.state.read(cx);
        let thread = state.thread_shell(thread_id)?;
        let project = state.shell.project(&thread.project_id)?;
        let is_scratch = state.is_scratch_project(&thread.project_id);
        let git = crate::thread_details::details(cx).map(|details| details.read(cx).git.clone());
        let git = git.filter(|git| git.read(cx).target.as_ref().is_some_and(|target| target.thread_id == *thread_id));
        let git_status = git.as_ref().and_then(|git| git.read(cx).status.clone());
        let is_git_repo = !is_scratch
            && git_status
                .as_ref()
                .map_or(thread.branch.is_some() || thread.worktree_path.is_some(), |s| s.local.is_repo);
        let host = self.host_options(cx);
        let primary_id = state.local_saved_environment().map(|env| env.id.as_str()).or_else(|| {
            let mut local = state.environments.environments.iter().filter(|env| env.local);
            let first = local.next()?;
            local.next().is_none().then_some(first.id.as_str())
        });
        let active_is_primary = host.active.as_ref().is_some_and(|active| {
            primary_id == Some(active.saved_id.as_str())
                || state.environments.environments.iter().any(|e| e.id == active.saved_id && e.local)
        });
        let show_env_indicator = host.can_pick || (host.active.is_some() && !active_is_primary);
        if !t3_core::client_settings::should_show_composer_context_strip(
            false,
            persist,
            true,
            is_git_repo,
            show_env_indicator,
        ) {
            return None;
        }
        let p = palette(cx);
        let env_chip = show_env_indicator.then_some(host.active.as_ref()).flatten().map(|active| {
            let icon = crate::new_thread::environment_machine_icon(state, &active.saved_id);
            ComposerControl::new("composer-active-environment")
                .icon(icon)
                .label(active.label.clone())
                .max_label_width(160.)
                .tooltip(active.label.clone())
        });
        let workspace_path = thread
            .worktree_path
            .as_deref()
            .unwrap_or(&project.workspace_root)
            .to_owned();
        let workspace_chip = is_git_repo.then(|| {
            let label = match thread.worktree_path.as_deref() {
                Some(path) => path
                    .split(['/', '\\'])
                    .rfind(|part| !part.is_empty())
                    .unwrap_or("Worktree")
                    .to_owned(),
                None => "Local checkout".to_owned(),
            };
            let icon = if thread.worktree_path.is_some() { Icon::FolderGit } else { Icon::Folder };
            let copy_path = workspace_path.clone();
            ComposerControl::new("composer-active-workspace")
                .icon(icon)
                .label(label)
                .max_label_width(200.)
                .tooltip(format!("{workspace_path} · Click to copy path"))
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_path.clone()));
                })
        });
        let branch_label = git_status
            .as_ref()
            .and_then(|status| status.local.ref_name.clone())
            .or_else(|| thread.branch.clone());
        let branch_chip = is_git_repo
            .then_some(branch_label)
            .flatten()
            .filter(|b| !b.is_empty())
            .map(|branch| {
                let control = ComposerControl::new("composer-active-branch")
                    .icon(Icon::GitBranch)
                    .label(crate::transcript::middle_truncate(&branch, 28))
                    .max_label_width(224.)
                    .tooltip(branch);
                let Some(git) = git.filter(|_| !self.is_rewinding(cx)) else {
                    return control.into_any_element();
                };
                let key = SharedString::from(format!("composer-branch-picker:{thread_id}"));
                let picker = window.use_keyed_state(key, cx, |window, cx| {
                    cx.new(|cx| crate::git::BranchPicker::new(git, window, cx).above_trigger())
                });
                let picker = picker.read(cx).clone();
                let on_click = picker.clone();
                div()
                    .relative()
                    .min_w_0()
                    .child(control
                        .suffix(Icon::ChevronDown.render(px(12.), hsla(p.muted_foreground)))
                        .on_click(move |_, window, cx| on_click.update(cx, |picker, cx| picker.toggle(window, cx))))
                    .child(picker)
                    .into_any_element()
            });
        Some(
            h_flex()
                .min_w_0()
                .gap(px(4.))
                .text_color(hsla(p.muted_foreground))
                .children(env_chip)
                .children(workspace_chip)
                .children(branch_chip)
                .into_any_element(),
        )
    }

    fn render_primary(&self, resolved: &Resolved, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let running = self.snapshot.running;
        let can_interrupt = self.snapshot.can_interrupt;
        let can_operate = self.snapshot.can_operate_thread;
        let rewinding = self.is_rewinding(cx);
        let stop_pending = self.stop_phase == StopPhase::Pending;
        let stop_retained = matches!(self.stop_phase, StopPhase::Pending | StopPhase::TimedOut);
        let stop_button = || {
            div()
                .flex_none()
                .debug_selector(|| "stop".to_owned())
                .child(
                    RoundButton::new("stop", RoundKind::Stop, t(cx, if stop_pending { "Stopping…" } else { "Interrupt" }))
                        .disabled(rewinding || !can_operate || stop_pending)
                        .on_click(cx.listener(|this, _, _, cx| this.request_stop(cx))),
                )
        };
        let voice_blocks = self.voice.read(cx).blocks_submission();
        let voice_preparing = self.voice.read(cx).preparing();
        let voice_disabled = self.voice_is_disabled(cx);
        let speech_pill = crate::voice::render_speech_recording_pill(
            "composer",
            &self.voice,
            crate::voice::VoiceUiSize::Regular,
            voice_disabled,
            cx,
        );
        let speech_button = crate::voice::render_speech_button(
            "composer",
            &self.voice,
            crate::voice::VoiceUiSize::Regular,
            voice_disabled,
            cx,
        );
        let with_voice = |primary: AnyElement| -> AnyElement {
            h_flex()
                .items_center()
                .gap(px(8.))
                .child(speech_pill)
                .child(speech_button)
                .child(primary)
                .into_any_element()
        };
        if let (Some(input), Some(progress)) = (self.active_input(), self.progress(cx)) {
            let responding = rewinding || !self.can_answer(cx) || self.question_responding(&input.request_id, cx) || !input.can_respond();
            let enabled = if progress.is_last_question { progress.is_complete } else { progress.can_advance };
            return with_voice(
                h_flex()
                    .gap(px(8.))
                    .when(progress.question_index > 0, |this| {
                        this.child(
                            Button::new("question-previous", ButtonVariant::Outline, ButtonSize::Sm)
                                .label(t(cx, "Previous"))
                                .disabled(responding)
                                .on_click(cx.listener(|this, _, window, cx| this.previous(window, cx))),
                        )
                    })
                    .child(
                        Pill::new("question-submit", t(cx, requests::primary_action_label(&progress, responding)))
                            .disabled(!can_operate || responding || voice_preparing || (!voice_blocks && !enabled))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.stop_phase != StopPhase::Landed {
                                    if this.voice.read(cx).blocks_submission() {
                                        if !this.voice.read(cx).preparing() {
                                            this.voice_pending_intent = Some(rules::SubmitIntent::Foreground);
                                            this.voice.update(cx, |voice, cx| {
                                                voice.submit_after_dictation(cx);
                                            });
                                        }
                                    } else {
                                        this.advance(window, cx);
                                    }
                                }
                            })),
                    )
                    .when(can_interrupt || stop_retained, |this| this.child(stop_button()))
                    .into_any_element(),
            );
        }
        let preparing = self
            .attachment_target()
            .is_some_and(|target| self.attachments.preparations.count(&super::drafts::key(&target)) > 0);
        let current_prompt = self.prompt(cx);
        let queued_edit = self.editing_queued();
        let editing_queued = queued_edit.is_some();
        let has_text = if let Some(editing) = queued_edit {
            !current_prompt.trim().is_empty() || editing.has_attachments()
        } else {
            self.has_text
                || !self.staged().is_empty()
                || preparing
                || self.current_reply_to().is_some()
                || self.has_context_records(&current_prompt)
        };
        if (can_interrupt || stop_retained) && !has_text && !editing_queued && !voice_blocks {
            return with_voice(stop_button().into_any_element());
        }
        let stage_backdrop = crate::sidebar::active_stage_backdrop_variant(self.state.read(cx), cx);
        let runtime_mode_denial = self.runtime_mode_ceiling_denial(resolved, cx);
        let show_resume = self.snapshot.can_resume && !has_text && !editing_queued && !voice_blocks;
        if show_resume {
            let resume_tooltip: SharedString = runtime_mode_denial
                .clone()
                .map(Into::into)
                .unwrap_or_else(|| t(cx, "Resume thread").into());
            let resume_btn = RoundButton::new("send", RoundKind::Resume, resume_tooltip)
                .disabled(self.sending || !can_operate || rewinding || runtime_mode_denial.is_some())
                .busy(self.sending)
                .stage_backdrop(stage_backdrop)
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.stop_phase != StopPhase::Landed {
                        this.resume(window, cx);
                    }
                }));
            let elem = if stop_retained {
                h_flex().gap(px(8.)).child(resume_btn).child(stop_button()).into_any_element()
            } else {
                resume_btn.into_any_element()
            };
            return with_voice(elem);
        }
        let fresh_armed = self.is_fresh_session_armed(cx);
        let compact_tokens = (!editing_queued && !fresh_armed)
            .then(|| self.compact_before_send_tokens(resolved, cx))
            .flatten()
            .map(|tokens| t3_core::usage::context_window::format_context_tokens(Some(tokens)));
        let keep_full_history = self.is_keep_full_history(cx);
        let compacts_before_send = compact_tokens.is_some() && !keep_full_history;
        let attachment_block = if editing_queued {
            None
        } else if self.snapshot.pending.approvals.is_empty() {
            t3_core::composer::attachments::send_block_reason(&self.staged_attachments())
        } else {
            t3_core::composer::attachments::approval_redirect_block_reason(&self.staged_attachments())
        };
        let settings = self.state.read(cx).client_settings;
        let follow_up = if self.snapshot.pending.approvals.is_empty() { settings.follow_up_behavior } else { rules::FollowUpBehavior::Steer };
        let can_steer = !self.snapshot.compaction_blocks_steer && !fresh_armed;
        let dispatch = rules::dispatch_mode(running, false, follow_up, can_steer);
        let alternate = rules::dispatch_mode(running, true, follow_up, can_steer);
        let (kind, label) = match (editing_queued, compacts_before_send, running, dispatch) {
            (true, _, _, _) => (RoundKind::UpdateQueued, "Update queued message"),
            (false, true, _, _) => (RoundKind::Send, "Compact and send"),
            (false, false, true, ComposerDispatch::Queue) => (RoundKind::Queue, "Queue message"),
            (false, false, true, _) => (RoundKind::Steer, "Steer message"),
            _ => (RoundKind::Send, "Submit message"),
        };
        let blocked = self.snapshot.setup_blocks_send || rewinding || !can_operate;
        let disabled = self.sending
            || voice_preparing
            || (!has_text && !voice_blocks)
            || (resolved.model.is_none() && resolved.multi_models.is_empty())
            || attachment_block.is_some()
            || runtime_mode_denial.is_some()
            || blocked;
        let tooltip: SharedString = if resolved.model.is_none() && resolved.multi_models.is_empty() {
            t(cx, "No provider available").into()
        } else if rewinding {
            t(cx, "Rewinding conversation").into()
        } else if self.snapshot.setup_blocks_send {
            t(cx, t3_core::worktree_setup::SEND_BLOCKED).into()
        } else if let Some(reason) = attachment_block {
            t(cx, &reason).to_owned().into()
        } else if let Some(reason) = runtime_mode_denial {
            reason.into()
        } else if self.sending {
            t(cx, if editing_queued { "Updating queued message" } else { "Submitting message" }).into()
        } else if let Some(tokens) = compact_tokens.as_deref().filter(|_| compacts_before_send) {
            format!("Summarize {tokens} tokens of history, then send").into()
        } else if running && can_steer && !editing_queued {
            let action = |mode| if mode == ComposerDispatch::Queue { "queue" } else { "steer" };
            let alt_key = crate::keymap::label_in(
                "composer.sendAlternate",
                &t3_core::keybindings::ShortcutMatchContext::default().with("composerFocus", true).with("turnRunning", true),
                cx,
            );
            let via = alt_key.map_or_else(|| "⌘-click".to_owned(), |shortcut| format!("{shortcut} or ⌘-click"));
            format!("Click to {}, {via} to {}", action(dispatch), action(alternate)).into()
        } else {
            t(cx, label).into()
        };
        let submit_btn = div().flex_none().debug_selector(|| "send".to_owned()).child(
            RoundButton::new("send", kind, tooltip)
                .disabled(disabled)
                .busy(self.sending || self.pending_send.is_pending())
                .stage_backdrop(stage_backdrop)
                .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                    if this.stop_phase == StopPhase::Landed {
                        return;
                    }
                    let modifier = event.modifiers().platform || event.modifiers().control;
                    let draft = matches!(this.snapshot.target, Some(Target::Draft(_)));
                    let key = rules::EnterKey { modifier, ..rules::EnterKey::default() };
                    let intent = rules::submit_intent(key, this.snapshot.running, draft, rules::SendShortcut::Enter, "");
                    if let Some(intent) = intent {
                        if this.voice.read(cx).blocks_submission() {
                            if !this.voice.read(cx).preparing() {
                                this.voice_pending_intent = Some(intent);
                                this.voice.update(cx, |voice, cx| {
                                    voice.submit_after_dictation(cx);
                                });
                            }
                        } else {
                            this.submit(intent, window, cx);
                        }
                    }
                })),
        );
        let compact_toggle = compact_tokens.map(|tokens| {
            let toggle_tooltip = if keep_full_history {
                format!("Next send keeps all {tokens} tokens. Click to compact first")
            } else {
                format!("Next send compacts {tokens} tokens first. Click to keep full history")
            };
            div().flex_none().debug_selector(|| "compact-before-send".to_owned()).child(
                ComposerControl::new("compact-before-send")
                    .icon(Icon::Minimize2)
                    .label(t(cx, if keep_full_history { "Keep" } else { "Compact" }))
                    .suffix(div().text_color(hsla(p.muted_foreground)).child(SharedString::from(tokens)))
                    .active(!keep_full_history)
                    .disabled(!can_operate)
                    .tooltip(toggle_tooltip)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_keep_full_history(cx))),
            )
        });
        if compact_toggle.is_none() && !stop_retained {
            return with_voice(submit_btn.into_any_element());
        }
        h_flex()
            .gap(px(8.))
            .children(compact_toggle)
            .child(with_voice(submit_btn.into_any_element()))
            .when(stop_retained, |this| this.child(stop_button()))
            .into_any_element()
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.question_markdown.begin_frame();
        self.sync_input_state(window, cx);
        self.sync_rich_decorations(cx);
        let resolved = self.resolve(cx);
        let rewinding = self.is_rewinding(cx);
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let error = self.error.clone().or_else(|| self.snapshot.last_error.clone());
        let question = self.active_input().cloned();
        let surface = hsla(p.composer_surface);
        let outline = hsla(p.composer_outline);
        let show_permissions = resolved.runtime_mode != resolved.default_runtime_mode;
        let show_attach = self.shows_attach_button(cx);
        let (drop_fill, drop_ring) = (hsla(p.accent).opacity(0.45), hsla(p.primary).opacity(0.7));
        let input_disabled = self.input_disabled(cx);
        let voice_frozen = self.voice.read(cx).freezes_editor();
        let can_drop_mention = self.can_drop_file_mention(cx);
        let thread_drop = cx.try_global::<ThreadDrop>().filter(|_| can_drop_mention && self.editing_queued().is_none()).map(|drop| drop.0.clone());
        let gutter = if self.fixed.is_some() { 12. } else { 48. };
        let host_slot = slot_style(ThemeSlot::ComposerHost, cx);
        let host_focused = self.input.read(cx).focus_handle(cx).contains_focused(window, cx);
        let host_state = if host_focused { SlotState::FocusWithin } else { SlotState::Base };
        let host_bg_layer =
            theme_slot_state_background_layer(ThemeSlot::ComposerHost, host_state, px(16.), cx);
        let host_before_bar = theme_slot_pseudo_before(ThemeSlot::ComposerHost, host_state, cx);
        let host_overrides_bg = host_bg_layer.is_some()
            || slot_has_state_background(ThemeSlot::ComposerHost, host_state, cx);
        let host_overrides_border = host_slot.as_ref().is_some_and(|s| {
            s.base.border_color.is_some()
                || !s.base.border_widths.is_empty()
                || s.base.border_gradient.is_some()
                || (host_focused
                    && (s.focus_within.border_color.is_some()
                        || !s.focus_within.border_widths.is_empty()
                        || s.focus_within.border_gradient.is_some()))
        });
        let tiptap_bg_layer = theme_slot_background_layer(ThemeSlot::ComposerTiptap, cx);
        let tiptap_hover = slot_style(ThemeSlot::ComposerTiptap, cx)
            .filter(|s| !s.hover.is_empty())
            .map(|s| s.hover);
        let prompt_editor = Editor::new(&self.input)
            .appearance(false)
            .disabled(input_disabled)
            .readonly(voice_frozen)
            .h(px(self.composer_editor_height(cx)))
            .font_family(crate::theme::font_family(t3_core::client_settings::typography::FontRole::Prompt, cx))
            .text_size(px(crate::theme::font_size(t3_core::client_settings::typography::FontRole::Prompt, cx)))
            .line_height(px(crate::theme::font_size(t3_core::client_settings::typography::FontRole::Prompt, cx) * 1.625))
            .theme_slot(ThemeSlot::ComposerTiptap, cx);
        let prompt_editor_el: AnyElement = if tiptap_bg_layer.is_some() || tiptap_hover.is_some() {
            let mut wrap = div().relative().w_full().min_w_0();
            if let Some(bg_layer) = tiptap_bg_layer {
                wrap = wrap.overflow_hidden().child(bg_layer);
            }
            if let Some(hover) = tiptap_hover {
                wrap = wrap.hover(|mut style| {
                    apply_slot_override(&mut style, &hover, cx);
                    style
                });
            }
            wrap.child(prompt_editor).into_any_element()
        } else {
            prompt_editor.into_any_element()
        };
        let suggestion = self.offered_prompt_suggestion(cx)
            .filter(|_| self.snapshot.pending.approvals.is_empty() && self.active_question().is_none())
            .map(|text| SharedString::from(text.to_owned()));
        let prompt_editor_el = div().relative().child(prompt_editor_el).children(suggestion.map(|text| {
            h_flex()
                .absolute().top_0().left_0().right_0().min_w_0().gap(px(6.))
                .font_family(crate::theme::font_family(t3_core::client_settings::typography::FontRole::Prompt, cx))
                .text_size(px(crate::theme::font_size(t3_core::client_settings::typography::FontRole::Prompt, cx)))
                .text_color(hsla(p.placeholder))
                .child(div().min_w_0().truncate().child(text))
                .child(kbd(t(cx, "Tab"), cx))
        }));
        div().w_full().flex().justify_center().px(px(gutter)).pb(px(if self.fixed.is_some() { 12. } else { 16. })).pt(px(8.)).child(
            v_flex()
                .key_context("Composer")
                .on_modifiers_changed(cx.listener(|_, event: &gpui::ModifiersChangedEvent, _, cx| {
                    crate::voice::handle_modifiers_changed(&event.modifiers, cx);
                }))
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "enter" {
                        this.enter_repeat = event.is_held;
                        if !event.is_held
                            && !event.keystroke.modifiers.shift
                            && !event.keystroke.modifiers.alt
                            && this.voice.read(cx).blocks_submission()
                            && !this.voice.read(cx).preparing()
                        {
                            let modifier = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;
                            let draft = matches!(this.snapshot.target, Some(Target::Draft(_)));
                            let settings = this.state.read(cx).client_settings;
                            let prompt = this.prompt(cx);
                            if let Some(intent) = rules::submit_intent(
                                rules::EnterKey { modifier, repeat: false, composing: false },
                                this.snapshot.running,
                                draft,
                                settings.send_shortcut,
                                &prompt,
                            ) {
                                this.voice_pending_intent = Some(intent);
                                this.voice.update(cx, |voice, cx| {
                                    voice.submit_after_dictation(cx);
                                });
                                cx.stop_propagation();
                            }
                        }
                    }
                }))
                .on_action(cx.listener(|this, _: &OutdentList, window, cx| this.outdent_list(window, cx)))
                // Each composer takes its own shortcuts, so a side chat's never reach the open thread's.
                .on_action(cx.listener(|this, _: &ToggleModelPicker, window, cx| this.toggle_picker(window, cx)))
                .on_action(cx.listener(|this, _: &super::OpenWorkspace, window, cx| this.open_workspace(window, cx)))
                .on_action(cx.listener(|this, _: &super::OpenBranch, window, cx| this.open_base_branch_picker(window, cx)))
                .on_action(cx.listener(|this, _: &super::OpenHost, window, cx| this.open_host(window, cx)))
                .on_action(cx.listener(|this, _: &super::CycleHost, window, cx| this.cycle_host(window, cx)))
                .on_action(cx.listener(|this, _: &super::UsePreviousWorktree, window, cx| this.use_previous_worktree(window, cx)))
                .on_action(cx.listener(|this, _: &super::IncreaseEffort, window, cx| this.step_reasoning_effort(1, window, cx)))
                .on_action(cx.listener(|this, _: &super::DecreaseEffort, window, cx| this.step_reasoning_effort(-1, window, cx)))
                .on_action(cx.listener(|this, _: &super::SendAlternate, window, cx| this.send_alternate(window, cx)))
                .on_action(cx.listener(|this, _: &super::SteerQueuedMessage, window, cx| {
                    if !this.steer_queued_message(window, cx) {
                        cx.propagate();
                    }
                }))
                .on_action(cx.listener(|this, _: &super::EditQueuedMessage, window, cx| {
                    if !this.edit_queued_message(window, cx) {
                        cx.propagate();
                    }
                }))
                .on_action(cx.listener(|this, _: &OpenPermissions, window, cx| this.open_permissions(window, cx)))
                // The open menu takes these keys before the prompt does (`composerCommandKey`).
                .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                    if this.workspace_key(MenuKey::Up, window, cx) || this.stash_key(MenuKey::Up, window, cx) || this.menu_key(MenuKey::Up, window, cx) || this.history_key(true, window, cx) {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                    if this.workspace_key(MenuKey::Down, window, cx) || this.stash_key(MenuKey::Down, window, cx)
                        || this.menu_key(MenuKey::Down, window, cx)
                        || this.history_key(false, window, cx)
                    {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                    if this.workspace_key(MenuKey::Accept, window, cx) {cx.stop_propagation(); return;}
                    if !action.secondary
                        && !action.shift
                        && (this.stash_key(MenuKey::Accept, window, cx) || this.menu_key(MenuKey::Accept, window, cx))
                    {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                    if this.menu_key(MenuKey::Accept, window, cx)
                        || this.accept_prompt_suggestion(window, cx)
                        || this.indent_list_or_code(window, cx)
                    {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                    if this.workspace_key(MenuKey::Dismiss, window, cx)
                        || this.stash_key(MenuKey::Dismiss, window, cx)
                        || this.menu_key(MenuKey::Dismiss, window, cx)
                        || this.dismiss_prompt_suggestion(cx)
                    {
                        cx.stop_propagation();
                    }
                }))
                .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    let mod_key = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;
                    if mod_key
                        && !event.keystroke.modifiers.alt
                        && !event.keystroke.modifiers.shift
                        && event.keystroke.key == "backspace"
                        && this.stash_key(MenuKey::Delete, window, cx)
                    {
                        cx.stop_propagation();
                    }
                }))
                // Formatting shortcuts run in bubble phase so app keybindings (like Mod+B sidebar
                // toggle) always take precedence first (#29445, #34493).
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if !this.rich_text_enabled(cx) {
                        return;
                    }
                    let mod_key = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;
                    if !mod_key || event.keystroke.modifiers.alt || !this.input.read(cx).focus_handle(cx).is_focused(window) {
                        return;
                    }
                    let format = match (event.keystroke.key.as_str(), event.keystroke.modifiers.shift) {
                        ("b", false) => Some(t3_core::composer::rich_text::InlineFormat::Bold),
                        ("i", false) => Some(t3_core::composer::rich_text::InlineFormat::Italic),
                        ("e", false) => Some(t3_core::composer::rich_text::InlineFormat::Code),
                        ("s", true) => Some(t3_core::composer::rich_text::InlineFormat::Strike),
                        _ => None,
                    };
                    if let Some(format) = format {
                        this.toggle_inline_format(format, window, cx);
                        cx.stop_propagation();
                    }
                }))
                .on_action(cx.listener(|this, _: &StashPrompt, window, cx| this.stash_prompt(window, cx)))
                .capture_action(cx.listener(|this, _: &Copy, _, cx| {
                    if this.copy_context_selection(cx) { cx.stop_propagation(); }
                }))
                .capture_action(cx.listener(|this, _: &Paste, window, cx| {
                    if this.input.read(cx).focus_handle(cx).is_focused(window) && this.paste(window, cx) {
                        cx.stop_propagation();
                    }
                }))
                .on_action(cx.listener(|this, _: &PasteAsText, window, cx| this.paste_as_text(window, cx)))
                .on_action(cx.listener(|this, _: &Escape, _, cx| {
                    if this.voice.read(cx).state().is_busy() {
                        this.voice.update(cx, |voice, cx| voice.cancel(cx));
                        cx.stop_propagation();
                        return;
                    }
                    if this.close_context_details(cx) {
                        return;
                    }
                    if this.workspace.open || this.permissions_open {
                        this.workspace.open = false;
                        this.permissions_open = false;
                        cx.notify();
                    } else {
                        cx.propagate();
                    }
                }))
                .relative()
                .w_full()
                .when_some(crate::theme::chat_max_width(cx), |this, width| this.max_w(width))
                .theme_slot_hover(ThemeSlot::ChatComposerForm, cx)
                .child(crate::webview::occluder("composer-overlay-cover", crate::webview::Layer::TranscriptChrome))
                .children(self.render_queue(cx))
                .children(error.map(|error| {
                    div()
                        .id("composer-stream-error")
                        .debug_selector(|| "composer-stream-error".to_owned())
                        .relative()
                        .block_mouse_except_scroll()
                        .px(px(8.))
                        .py(px(4.))
                        .mb(px(4.))
                        .rounded(px(8.))
                        .text_xs()
                        .text_color(hsla(p.error_foreground))
                        .child(crate::glass::page_backdrop(8., cx))
                        .child(div().relative().child(SharedString::from(error)))
                }))
                .when(!matches!(self.snapshot.target, Some(Target::Draft(_))) && self.fixed.is_none(), |this| this.child(self.limit_recovery.clone()))
                .when(!matches!(self.snapshot.target, Some(Target::Draft(_))), |this| this.children(self.render_branch_mismatch_banner(cx)))
                .when(!matches!(self.snapshot.target, Some(Target::Draft(_))), |this| this.children(self.render_usage_limits_banner(false, cx)))
                .children(self.render_fresh_session_banner(cx))
                .when_some(question, |this, input| this.child(self.render_question(&input, cx)))
                .when(self.active_input().is_none(), |this| {
                    this.children(self.render_reply_banner(cx))
                })
                .children(self.render_stash_badge(cx))
                .children(self.render_stash_menu(cx))
                .when(self.voice.read(cx).preview().is_some(), |this| {
                    this.child(
                        div()
                            .id("composer-voice-preview-wrap")
                            .block_mouse_except_scroll()
                            .pb(px(8.))
                            .child(crate::voice::render_voice_preview("composer", &self.voice, cx)),
                    )
                })
                .child(
                    div().relative().children(self.render_menu(cx)).child(
                    v_flex()
                        .id("composer-card")
                        .block_mouse_except_scroll()
                        .when(rewinding, |this| this.opacity(0.65))
                        .rounded(px(16.))
                        .border_1()
                        .border_color(if host_overrides_bg { outline } else { outline.opacity(0.) })
                        .relative()
                        .overflow_hidden()
                        .when(!dark, |this| {
                            this.shadow(vec![gpui::BoxShadow {
                                color: gpui::black().opacity(0.05),
                                offset: gpui::point(px(0.), px(1.)),
                                blur_radius: px(2.),
                                spread_radius: px(0.),
                                inset: false,
                            }])
                        })
                        .theme_slot_interactive(ThemeSlot::ComposerHost, cx)
                        .when(host_focused, |this| {
                            this.theme_slot_state(ThemeSlot::ComposerHost, SlotState::FocusWithin, cx)
                        })
                        .when(!host_overrides_bg, |this| {
                            let border = if host_overrides_border { outline.opacity(0.) } else { outline };
                            let highlight = (!host_overrides_border && dark).then(|| hsla(p.composer_highlight));
                            this.child(crate::glass::backdrop_with_highlight(16., surface, border, highlight, cx))
                        })
                        .children(host_bg_layer)
                        .children(host_before_bar)
                        // Files dragged in from Finder (`isDragOverComposer`) or file-tree mentions (`composerMentionDragHandlers`).
                        .when(!rewinding, |this| {
                            this.drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_fill).border_color(drop_ring))
                                .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.on_drop_paths(paths, window, cx)))
                                .when(can_drop_mention, |this| {
                                    this.drag_over::<DraggedFileMention>(move |style, _, _, _| {
                                        style.bg(drop_fill).border_color(drop_ring)
                                    })
                                    .on_drop(cx.listener(|this, dragged: &DraggedFileMention, window, cx| {
                                        this.on_drop_file_mention(dragged, window, cx);
                                    }))
                                })
                        })
                        .when_some(thread_drop, |card, install| install(card, cx.entity(), drop_fill, drop_ring))
                        .child(
                            div()
                                .px(px(16.))
                                .pt(px(16.))
                                .pb(px(8.))
                                .children(if self.editing_queued().is_some() {
                                    self.render_queued_edit_attachments(cx)
                                } else {
                                    self.render_attachments(cx)
                                })
                                .children(self.render_context_chips(cx))
                                .children(self.render_citation_chips(cx))
                                .child(prompt_editor_el),
                        )
                        .when(self.answering.is_none(), |this| {
                            this.when_some(self.prompt_length_error.clone(), |this, message| {
                                this.child(
                                    div()
                                        .id("prompt-length-validation")
                                        .debug_selector(|| "prompt-length-validation".to_owned())
                                        .px(px(16.))
                                        .pb(px(8.))
                                        .text_xs()
                                        .text_color(hsla(p.destructive))
                                        .child(message),
                                )
                            })
                        })
                        .child(
                            h_flex()
                                .px(px(8.))
                                .pb(px(8.))
                                .gap(px(2.))
                                .when(show_attach, |this| {
                                    this.child(
                                        div().mr(px(8.)).child(
                                            Button::new("attach-files", ButtonVariant::Ghost, ButtonSize::IconSm)
                                                .icon(Icon::Paperclip)
                                                .disabled(rewinding)
                                                .tooltip(t(cx, "Attach files"))
                                                .on_click(cx.listener(|this, _, window, cx| this.pick_files(window, cx))),
                                        ),
                                    )
                                })
                                .child(self.render_model_trigger(&resolved, cx))
                                .when(show_permissions, |this| this.child(self.render_permissions_chip(&resolved, cx)))
                                .child(div().flex_1().min_w(px(8.)))
                                .child(self.render_primary(&resolved, cx)),
                        ),
                    ),
                )
                .children(
                    if matches!(self.snapshot.target, Some(Target::Draft(_))) {
                        self.render_workspace(cx)
                    } else {
                        self.render_active_thread_context_strip(window, cx)
                    }
                    .map(|control| {
                            h_flex()
                                .id("composer-context-strip")
                                .debug_selector(|| "composer-context-strip".to_owned())
                                .relative()
                                .block_mouse_except_scroll()
                                .w_full()
                                .pt(px(8.))
                                .px(px(8.))
                                .rounded_b(px(10.))
                                .child(crate::glass::page_backdrop(10., cx))
                                .child(div().relative().min_w_0().w_full().child(control))
                        }),
                )
                .when(matches!(self.snapshot.target, Some(Target::Draft(_))), |this| {
                    this.children(self.render_usage_limits_banner(true, cx).map(render_draft_usage_limits_overlay))
                })
                .child(self.viewer.clone()),
        )
    }
}

pub(super) fn render_draft_usage_limits_overlay(banner: AnyElement) -> AnyElement {
    div()
        .id("draft-usage-limits-overlay")
        .debug_selector(|| "draft-usage-limits-overlay".to_owned())
        .absolute()
        .left_0()
        .right_0()
        .top_full()
        .pt(px(16.))
        .child(banner)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;

    #[gpui::test]
    fn composer_drop_file_mention_inserts_single_and_multi_preserving_draft_and_cursor(
        cx: &mut gpui::TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
        });
        let (composer, vcx) = cx.add_window_view(|window, cx| Composer::new(state.clone(), window, cx));

        // 1. Without a target, drop is ignored and draft stays empty.
        vcx.update(|window, cx| {
            composer.update(cx, |c, cx| {
                assert!(!c.can_drop_file_mention(cx));
                let payload = DraggedFileMention::from_drag("src/main.rs", &[]).expect("payload");
                c.on_drop_file_mention(&payload, window, cx);
                assert_eq!(c.prompt(cx), "");
            });
        });

        // 2. With an active thread target, dropping onto empty composer inserts `[main.rs](src/main.rs) `.
        vcx.update(|window, cx| {
            composer.update(cx, |c, cx| {
                c.snapshot.target = Some(Target::Thread("t-drop".into()));
                assert!(c.can_drop_file_mention(cx));
                let payload = DraggedFileMention::from_drag("src/main.rs", &[]).expect("payload");
                c.on_drop_file_mention(&payload, window, cx);
                assert_eq!(c.prompt(cx), "[main.rs](src/main.rs) ");
                assert_eq!(c.input.read(cx).cursor(), c.prompt(cx).len());
            });
            window.blur();
            window.draw(cx).clear();
        });

        // 3. Dropping multiple files onto existing draft text preserves text with single space boundary.
        vcx.update(|window, cx| {
            composer.update(cx, |c, cx| {
                c.input.update(cx, |input, cx| input.set_value("Check these", window, cx));
                let selection = vec!["src/a.rs".to_owned(), "src/dir with space/b.rs".to_owned()];
                let payload = DraggedFileMention::from_drag("src/a.rs", &selection).expect("multi payload");
                c.on_drop_file_mention(&payload, window, cx);
                let expected = "Check these [a.rs](src/a.rs) [b.rs](src/dir%20with%20space/b.rs) ";
                assert_eq!(c.prompt(cx), expected);
                assert_eq!(c.input.read(cx).cursor(), expected.len());
            });
            window.blur();
            window.draw(cx).clear();
        });

        // 4. Dropping a payload with trailing slash trims cleanly and avoids double spaces after trailing whitespace.
        vcx.update(|window, cx| {
            composer.update(cx, |c, cx| {
                c.input.update(cx, |input, cx| input.set_value("Inspect ", window, cx));
                let payload = DraggedFileMention {
                    paths: vec!["crates/core/".into(), "   ".into()],
                    mention: "[core](crates/core)".into(),
                    label: "core".into(),
                };
                c.on_drop_file_mention(&payload, window, cx);
                assert_eq!(c.prompt(cx), "Inspect [core](crates/core) ");
            });
            window.blur();
            window.draw(cx).clear();
        });

        #[cfg(feature = "snapshot")]
        state.update(vcx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }

    #[gpui::test]
    fn composer_renders_theme_css_slots_and_background_image_cleanly(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
        });
        let (_composer, vcx) = cx.add_window_view(|window, cx| Composer::new(state.clone(), window, cx));

        let colors = t3_core::themes::standard(true);
        let slot_css = r#"
            [data-chat-composer-form] { padding: 4px; }
            [data-chat-composer-form]:hover { opacity: 0.99; }
            [data-slot="composer-host"] { background: #141926; border: 2px solid #ff44aa; border-radius: 20px; }
            [data-slot="composer-host"]:hover { border-color: #66ccff; }
            [data-slot="composer-host"]:active { opacity: 0.96; }
            .composer-tiptap { font-size: 16px; line-height: 1.5; color: #f8fafc; padding: 2px; }
            .composer-tiptap:hover { opacity: 1; }
        "#;
        vcx.update(|window, cx| {
            crate::theme::preview_with_css(Some((&colors, Some(slot_css), true)), cx);
            assert!(slot_style(ThemeSlot::ChatComposerForm, cx).is_some());
            assert!(slot_style(ThemeSlot::ComposerHost, cx).is_some());
            assert!(slot_style(ThemeSlot::ComposerTiptap, cx).is_some());
            window.blur();
            window.draw(cx).clear();
            window.draw(cx).clear();
        });

        let bg_image = std::sync::Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::from_pixel(2, 2, image::Rgba([0x44, 0x22, 0x88, 0xff])),
        )]));
        let bg_css = r#"
            [data-slot="composer-host"] { background: var(--theme-background-image) center / cover no-repeat; border-radius: 18px; }
        "#;
        vcx.update(|window, cx| {
            crate::theme::set_background_image(Some(bg_image), cx);
            crate::theme::preview_with_css(Some((&colors, Some(bg_css), true)), cx);
            assert!(theme_slot_background_layer(ThemeSlot::ComposerHost, cx).is_some());
            window.blur();
            window.draw(cx).clear();
            window.draw(cx).clear();
        });

        vcx.update(|window, cx| {
            crate::theme::preview(None, cx);
            crate::theme::set_background_image(None, cx);
            window.blur();
            window.draw(cx).clear();
        });

        #[cfg(feature = "snapshot")]
        state.update(vcx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }
}
