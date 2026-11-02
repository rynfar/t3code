//! Worktrees: `WorktreesSettingsPanel` in `web/src/components/settings/WorktreesSettings.tsx`
//! at environment scope, with `BranchNamingSettings.tsx`, `WorktreeSubmodulesRow`
//! (`ProjectDefaultsSettings.tsx`), and `WorktreesDirectoryRow` and `WorktreeCleanupSection`
//! (`StorageSettings.tsx`). Every row is server-backed; rows the server lacks a capability for
//! are left out as the web leaves them out.

use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{Sizable, h_flex, v_flex};
use t3_core::server_config::ServerConfig;
use t3_core::settings::worktrees::{
    BranchNaming, CleanupRule, DEFAULT_BRANCH_PREFIX, DEFAULT_RETENTION_DAYS, KeepWhen, Submodules, WorktreeChange, clamp_retention,
};

use super::controls::{Row, Select, SelectOption, Switch, group, icon, muted_text, r, section, section_title};
use super::{Block, SettingsView, store};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// The two retention fields, settled first so `RetentionControl`'s minimum differs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Retention {
    Inactive,
    Settled,
}

impl Retention {
    fn minimum(self) -> u32 {
        match self {
            Retention::Inactive => 1,
            Retention::Settled => 0,
        }
    }

    fn saved(self, config: &ServerConfig) -> Option<u32> {
        let rules = config.settings.worktrees.cleanup();
        match self {
            Retention::Inactive => rules.worktree_after_days,
            Retention::Settled => rules.worktree_settled_after_days,
        }
    }

    fn change(self, days: Option<u32>) -> WorktreeChange {
        WorktreeChange::Cleanup(match self {
            Retention::Inactive => CleanupRule::AfterDays(days),
            Retention::Settled => CleanupRule::SettledAfterDays(days),
        })
    }
}

pub(crate) struct WorktreesState {
    pub(crate) directory: Entity<InputState>,
    pub(crate) prefix: Entity<InputState>,
    pub(crate) instructions: Entity<TextareaState>,
    pub(crate) inactive_days: Entity<InputState>,
    pub(crate) settled_days: Entity<InputState>,
    creation_advanced: bool,
    cleanup_advanced: bool,
    _subscriptions: Vec<Subscription>,
}

impl WorktreesState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let directory = cx.new(|cx| InputState::new(window, cx).placeholder("Default"));
        let prefix = cx.new(|cx| InputState::new(window, cx).placeholder("No prefix"));
        let instructions = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(4, 8).placeholder("Use julius/ followed by the issue ID and a short description.")
        });
        let inactive_days = cx.new(|cx| InputState::new(window, cx));
        let settled_days = cx.new(|cx| InputState::new(window, cx));
        let commit_on = |event: &InputEvent| matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur);
        let subscriptions = vec![
            cx.subscribe_in(&directory, window, move |this, _, event: &InputEvent, _, cx| {
                if commit_on(event) {
                    this.commit_worktree_directory(cx);
                }
            }),
            cx.subscribe_in(&prefix, window, move |this, _, event: &InputEvent, _, cx| {
                if commit_on(event) {
                    this.commit_branch_prefix(cx);
                }
            }),
            cx.subscribe_in(&instructions, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Blur) {
                    this.commit_branch_instructions(cx);
                }
            }),
            cx.subscribe_in(&inactive_days, window, move |this, _, event: &InputEvent, window, cx| {
                if commit_on(event) {
                    this.commit_retention(Retention::Inactive, window, cx);
                }
            }),
            cx.subscribe_in(&settled_days, window, move |this, _, event: &InputEvent, window, cx| {
                if commit_on(event) {
                    this.commit_retention(Retention::Settled, window, cx);
                }
            }),
        ];
        WorktreesState {
            directory,
            prefix,
            instructions,
            inactive_days,
            settled_days,
            creation_advanced: false,
            cleanup_advanced: false,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        [&self.directory, &self.prefix, &self.inactive_days, &self.settled_days].into_iter()
    }

    fn retention_input(&self, field: Retention) -> &Entity<InputState> {
        match field {
            Retention::Inactive => &self.inactive_days,
            Retention::Settled => &self.settled_days,
        }
    }

    /// `AdvancedSettings` opens when a search jump targets a row inside it.
    pub(super) fn reveal(&mut self, target: &str) {
        if target == "worktree-submodules" {
            self.creation_advanced = true;
        }
        if matches!(target, "storage-worktrees" | "storage-worktree-keep-when") {
            self.cleanup_advanced = true;
        }
    }
}

/// `AdvancedSettings`: a disclosure at the end of a group whose rows fold under it.
pub(super) fn advanced(id: &'static str, open: bool, rows: Vec<AnyElement>, on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> AnyElement {
    let p = palette(cx);
    let divider = hsla(p.border).opacity(0.5);
    v_flex()
        .child(
            h_flex()
                .id(id)
                .min_h(r(40.))
                .px(r(16.))
                .gap(r(6.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.muted_foreground))
                .cursor_pointer()
                .hover(|this| this.text_color(hsla(p.foreground)))
                .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 14., hsla(p.muted_foreground)))
                .child("Advanced")
                .on_click(on_toggle),
        )
        .when(open, |this| this.children(rows.into_iter().map(move |row| div().border_t_1().border_color(divider).child(row))))
        .into_any_element()
}

impl SettingsView {
    fn save_worktree(&self, change: WorktreeChange, cx: &mut App) {
        store::update_server(&self.state, change.into_server(), cx);
    }

    fn commit_worktree_directory(&mut self, cx: &mut Context<Self>) {
        let value = self.worktrees.directory.read(cx).value().trim().to_owned();
        let saved = self.thread_settings(cx).map(|config| config.settings.worktrees.worktrees_directory.clone());
        if saved.is_some_and(|saved| saved != value) {
            self.save_worktree(WorktreeChange::Directory(value), cx);
        }
    }

    fn commit_branch_prefix(&mut self, cx: &mut Context<Self>) {
        let value = self.worktrees.prefix.read(cx).value().trim().to_owned();
        let saved = self.thread_settings(cx).map(|config| config.settings.worktrees.branch_name_prefix.clone());
        if saved.is_some_and(|saved| saved != value) {
            self.save_worktree(WorktreeChange::BranchPrefix(value), cx);
        }
    }

    fn commit_branch_instructions(&mut self, cx: &mut Context<Self>) {
        let value = self.worktrees.instructions.read(cx).value().trim().to_owned();
        let saved = self.thread_settings(cx).map(|config| config.settings.worktrees.branch_name_instructions.clone());
        if saved.is_some_and(|saved| saved != value) {
            self.save_worktree(WorktreeChange::BranchInstructions(value), cx);
        }
    }

    /// `RetentionControl`'s commit: a number saves clamped; anything else puts the saved value back.
    fn commit_retention(&mut self, field: Retention, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.worktrees.retention_input(field).clone();
        let Some(saved) = self.thread_settings(cx).and_then(|config| field.saved(config)) else { return };
        match input.read(cx).value().trim().parse::<f64>().ok().filter(|days| days.is_finite()) {
            Some(days) => {
                let days = clamp_retention(days, field.minimum());
                input.update(cx, |input, cx| input.set_value(days.to_string(), window, cx));
                if days != saved {
                    self.save_worktree(field.change(Some(days)), cx);
                }
            }
            None => input.update(cx, |input, cx| input.set_value(saved.to_string(), window, cx)),
        }
    }

    pub(super) fn commit_focused_worktree_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.worktrees.directory.focus_handle(cx).is_focused(window) {
            self.commit_worktree_directory(cx);
        }
        if self.worktrees.prefix.focus_handle(cx).is_focused(window) {
            self.commit_branch_prefix(cx);
        }
        if self.worktrees.instructions.focus_handle(cx).is_focused(window) {
            self.commit_branch_instructions(cx);
        }
        for field in [Retention::Inactive, Retention::Settled] {
            if self.worktrees.retention_input(field).focus_handle(cx).is_focused(window) {
                self.commit_retention(field, window, cx);
            }
        }
    }

    fn sync_textarea(input: &Entity<TextareaState>, value: &str, window: &mut Window, cx: &mut App) {
        if !input.focus_handle(cx).is_focused(window) && input.read(cx).value().as_ref() != value {
            let value = value.to_owned();
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
    }

    /// `RetentionControl`: a day count while on, "Off" while off, and a switch that turns it on at
    /// eight days.
    fn retention_control(&self, field: Retention, id: &'static str, value: Option<u32>, cx: &App) -> impl IntoElement {
        let p = palette(cx);
        let state = self.state.clone();
        h_flex()
            .gap(r(12.))
            .map(|this| match value {
                Some(_) => this.child(
                    h_flex()
                        .gap(r(6.))
                        .child(div().w(r(72.)).child(Input::new(self.worktrees.retention_input(field)).small()))
                        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("days")),
                ),
                None => this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Off")),
            })
            .child(Switch::new(id, value.is_some()).on_change(move |on, _, cx| {
                let days = on.then_some(DEFAULT_RETENTION_DAYS);
                store::update_server(&state, field.change(days).into_server(), cx);
            }))
    }

    fn worktree_switch(&self, id: &'static str, checked: bool, change: impl Fn(bool) -> WorktreeChange + 'static) -> Switch {
        let state = self.state.clone();
        Switch::new(id, checked).on_change(move |checked, _, cx| store::update_server(&state, change(checked).into_server(), cx))
    }

    fn worktree_reset(&self, change: WorktreeChange) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        let state = self.state.clone();
        move |_, _, cx| store::update_server(&state, change.clone().into_server(), cx)
    }

    pub(super) fn worktrees_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let Some(config) = self.thread_settings(cx).cloned() else {
            return vec![Block::new(&[], muted_text("Reconnect to change this environment's settings.", cx))];
        };
        let settings = &config.settings.worktrees;
        let capabilities = &config.environment.capabilities;
        let rules = settings.cleanup();
        Self::sync_input(&self.worktrees.directory, &settings.worktrees_directory, window, cx);
        Self::sync_input(&self.worktrees.prefix, &settings.branch_name_prefix, window, cx);
        Self::sync_textarea(&self.worktrees.instructions, &settings.branch_name_instructions, window, cx);
        for (field, days) in [(Retention::Inactive, rules.worktree_after_days), (Retention::Settled, rules.worktree_settled_after_days)] {
            if let Some(days) = days {
                Self::sync_input(self.worktrees.retention_input(field), &days.to_string(), window, cx);
            }
        }

        let mut creation = Vec::new();
        if capabilities.worktrees_directory == Some(true) {
            creation.push(
                Row::new("storage-worktrees-location", "Worktree location")
                    .description("Folder where new worktrees are created, on any drive, such as D:\\worktrees or ~/worktrees. Existing worktrees stay where they are. Leave empty to use the T3 home folder.")
                    .highlighted(self.is_target("storage-worktrees-location"))
                    .reset(!settings.worktrees_directory.is_empty(), self.worktree_reset(WorktreeChange::Directory(String::new())))
                    .control(div().w(r(288.)).child(Input::new(&self.worktrees.directory).small()))
                    .into_any_element(),
            );
        }
        let start_from_origin = config.settings.new_worktrees_start_from_origin.unwrap_or(true);
        creation.push(
            Row::new("start-from-origin", "Start from remote")
                .description("Uses the latest commit of the base branch\u{2019}s tracked remote branch, even if its name differs. Otherwise uses the same-named branch on origin. Uses your local branch if that remote branch is missing.")
                .highlighted(self.is_target("start-from-origin"))
                .reset(!start_from_origin, self.worktree_reset(WorktreeChange::StartFromOrigin(true)))
                .control(self.worktree_switch("start-from-origin-switch", start_from_origin, WorktreeChange::StartFromOrigin))
                .into_any_element(),
        );
        let submodules_state = self.state.clone();
        let submodules = Row::new("worktree-submodules", "Submodules")
            .description("How new worktrees populate git submodules. Projects and their t3.json can override it.")
            .highlighted(self.is_target("worktree-submodules"))
            .reset(settings.worktree_submodules.is_some(), self.worktree_reset(WorktreeChange::Submodules(None)))
            .control(
                Select::new("worktree-submodules-select", Submodules::ALL.iter().map(|mode| SelectOption::new(mode.as_str(), mode.label())).collect())
                    .selected(settings.effective_submodules().as_str())
                    .width(176.)
                    .on_change(move |value, _, cx| {
                        if let Some(mode) = Submodules::parse(&value) {
                            store::update_server(&submodules_state, WorktreeChange::Submodules(Some(mode)).into_server(), cx);
                        }
                    }),
            )
            .into_any_element();
        let toggle_creation = cx.listener(|this, _, _, cx| {
            this.worktrees.creation_advanced = !this.worktrees.creation_advanced;
            cx.notify();
        });
        creation.push(advanced("worktree-creation-advanced", self.worktrees.creation_advanced, vec![submodules], toggle_creation, cx));
        let creation = section("Creation", creation, cx);

        let mode = settings.branch_naming_mode;
        let naming_state = self.state.clone();
        let mut branches = vec![
            Row::new("worktree-branch-naming", "Worktree branch naming")
                .description("Choose how new worktree branches are named from your first message.")
                .when(mode == BranchNaming::Semantic, |row| {
                    row.status("The model chooses a prefix that describes the work, such as feat/add-search, fix/login-timeout, or refactor/auth.")
                })
                .highlighted(self.is_target("worktree-branch-naming"))
                .reset(mode != BranchNaming::default(), self.worktree_reset(WorktreeChange::BranchNaming(BranchNaming::default())))
                .control(
                    Select::new("worktree-branch-naming-select", BranchNaming::ALL.iter().map(|mode| SelectOption::new(mode.as_str(), mode.label())).collect())
                        .selected(mode.as_str())
                        .width(196.)
                        .on_change(move |value, _, cx| {
                            if let Some(mode) = BranchNaming::parse(&value) {
                                store::update_server(&naming_state, WorktreeChange::BranchNaming(mode).into_server(), cx);
                            }
                        }),
                )
                .into_any_element(),
        ];
        match mode {
            BranchNaming::Static => branches.push(
                Row::new("worktree-branch-prefix", "Branch prefix")
                    .description("For example, t3 or t3/ produces t3/add-search. Leave empty for no prefix.")
                    .reset(settings.branch_name_prefix != DEFAULT_BRANCH_PREFIX, self.worktree_reset(WorktreeChange::BranchPrefix(DEFAULT_BRANCH_PREFIX.to_owned())))
                    .control(div().w(r(176.)).child(Input::new(&self.worktrees.prefix).small()))
                    .into_any_element(),
            ),
            BranchNaming::Custom => branches.push(
                Row::new("worktree-branch-instructions", "Branch naming instructions")
                    .description("Appended to the naming prompt. The model returns the complete branch name; no prefix or suffix is added.")
                    .reset(!settings.branch_name_instructions.is_empty(), self.worktree_reset(WorktreeChange::BranchInstructions(String::new())))
                    .below(div().mt(r(12.)).pb(r(14.)).max_w(r(672.)).child(Textarea::new(&self.worktrees.instructions)))
                    .into_any_element(),
            ),
            BranchNaming::Semantic => {}
        }
        let branches = section("Branches", branches, cx);

        let cleanup = if capabilities.storage_cleanup == Some(true) {
            self.cleanup_section(&config, cx).into_any_element()
        } else {
            v_flex()
                .gap(r(10.))
                .child(section_title("Cleanup"))
                .child(div().px(r(16.)).child(muted_text("Update the selected environments to use storage cleanup, or choose a machine that supports it.", cx)))
                .into_any_element()
        };

        vec![
            Block::new(&["storage-worktrees-location", "start-from-origin", "worktree-submodules"], creation),
            Block::new(&["worktree-branch-naming"], branches),
            Block::new(&["storage-worktrees", "storage-worktree-keep-when"], cleanup),
        ]
    }

    fn cleanup_section(&mut self, config: &ServerConfig, cx: &mut Context<Self>) -> impl IntoElement {
        let rules = config.settings.worktrees.cleanup();
        let capabilities = &config.environment.capabilities;
        let rule = |rule: fn(bool) -> CleanupRule| move |on: bool| WorktreeChange::Cleanup(rule(on));
        let rows = vec![
            Row::new("worktree-on-delete", "Delete worktrees with deleted threads")
                .description("Remove unused worktrees when active or archived threads are deleted. Worktrees with local changes are kept.")
                .control(self.worktree_switch("worktree-on-delete-switch", rules.worktree_on_delete, rule(CleanupRule::OnDelete)))
                .into_any_element(),
            Row::new("worktree-on-merge", "Delete merged worktrees")
                .description("Remove worktrees whose pull request is merged and whose commits are included in the default branch.")
                .control(self.worktree_switch("worktree-on-merge-switch", rules.worktree_on_merge, rule(CleanupRule::OnMerge)))
                .into_any_element(),
            Row::new("worktree-after-days", "Delete inactive worktrees")
                .description("Remove worktrees after their threads have been inactive for this many days. Branches and thread history are kept.")
                .control(self.retention_control(Retention::Inactive, "worktree-after-days-switch", rules.worktree_after_days, cx))
                .into_any_element(),
        ];
        let mut advanced_rows = Vec::new();
        if capabilities.settled_worktree_cleanup == Some(true) {
            advanced_rows.push(
                Row::new("worktree-settled-after-days", "Delete settled worktrees")
                    .description("Remove worktrees this many days after their thread settles. Set 0 days to remove them at the next cleanup. Branches and thread history are kept.")
                    .control(self.retention_control(Retention::Settled, "worktree-settled-after-days-switch", rules.worktree_settled_after_days, cx))
                    .into_any_element(),
            );
        }
        advanced_rows.push(
            Row::new("worktree-unchanged", "Delete unchanged worktrees")
                .description("Remove worktrees with no commits beyond the default branch.")
                .control(self.worktree_switch("worktree-unchanged-switch", rules.worktree_unchanged, rule(CleanupRule::Unchanged)))
                .into_any_element(),
        );
        if capabilities.storage_cleanup_run == Some(true) {
            let keep_state = self.state.clone();
            advanced_rows.push(
                Row::new("storage-worktree-keep-when", "Keep worktrees with local changes")
                    .description(rules.worktree_keep_when.description())
                    .highlighted(self.is_target("storage-worktree-keep-when"))
                    .control(
                        Select::new("storage-worktree-keep-when-select", KeepWhen::ALL.iter().map(|mode| SelectOption::new(mode.as_str(), mode.label())).collect())
                            .selected(rules.worktree_keep_when.as_str())
                            .width(196.)
                            .on_change(move |value, _, cx| {
                                if let Some(mode) = KeepWhen::parse(&value) {
                                    store::update_server(&keep_state, WorktreeChange::Cleanup(CleanupRule::KeepWhen(mode)).into_server(), cx);
                                }
                            }),
                    )
                    .into_any_element(),
            );
        }
        let toggle = cx.listener(|this, _, _, cx| {
            this.worktrees.cleanup_advanced = !this.worktrees.cleanup_advanced;
            cx.notify();
        });
        let mut rows = rows;
        rows.push(advanced("worktree-cleanup-advanced", self.worktrees.cleanup_advanced, advanced_rows, toggle, cx));
        let highlighted = self.is_target("storage-worktrees");
        let p = palette(cx);
        v_flex()
            .id(SharedString::new_static("storage-worktrees"))
            .gap(r(10.))
            .rounded(r(12.))
            .when(highlighted, |this| this.bg(hsla(p.accent).opacity(0.3)))
            .child(section_title("Cleanup"))
            .child(group(rows, cx))
    }
}
