//! The `@`, `/` and `$` menus: `ComposerCommandMenu.tsx` (the attached list), the searches of
//! `useComposerMenuItems.ts` (`projects.searchEntries`, threads, provider skills and commands with
//! their per-cwd snapshot from `server.refreshProviders`), `onSelectComposerItem` and
//! `nudgeComposerMenuHighlight` from `ChatComposer.tsx`, and the menu keys of
//! `composerCommandKey.ts`. Rows, ranking and inserted text come from `t3_core::composer::menu`.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{AnyElement, App, Context, Focusable, FontWeight, IntoElement, ParentElement, ScrollHandle, SharedString, Styled, Task, Window, div, prelude::*, px, relative};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::composer::menu::{
    self as rules, MenuAction, MenuItem, PathEntry, PathKind, SkillSource, SlashCommand, SlashMenuOptions, ThreadContext, Trigger, TriggerKind,
    TriggerState,
};
use t3_core::palette::PaletteThread;
use t3_core::server_config::{InteractionMode, WorkspaceSnapshot};
use t3_core::sidebar::time::format_iso;

use super::{Composer, Target};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

/// `(instance id, cwd)`.
type SnapshotKey = (String, String);

/// A failed scan waits this long before the next try (`WORKSPACE_SNAPSHOT_RETRY_COOLDOWN_MS`).
const SNAPSHOT_RETRY_MS: i64 = 10_000;
/// `max-h-72`.
const LIST_MAX_HEIGHT: f32 = 288.;
/// How far the attached menu tucks under the card (`--chat-composer-attachment-overlap`).
const OVERLAP: f32 = 17.;

/// A key the open menu takes from the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuKey {
    Up,
    Down,
    Accept,
    Dismiss,
}

#[derive(Default)]
pub(super) struct Menu {
    triggers: TriggerState,
    /// The row the user moved to and the search it was listed for.
    highlighted: Option<(String, String)>,
    items: Vec<MenuItem>,
    loading: bool,
    /// The latest `projects.searchEntries` reply: cwd, query, entries.
    paths: Option<(String, String, Vec<PathEntry>)>,
    path_search: Option<(String, String, Task<()>)>,
    snapshots: HashMap<SnapshotKey, WorkspaceSnapshot>,
    /// When each snapshot was last asked for; one scan per TTL.
    requested: HashMap<SnapshotKey, i64>,
    scanning: HashSet<SnapshotKey>,
    /// The caret and length the menu last read, so cursor blinks cost a comparison.
    caret: Option<(usize, usize)>,
    scroll: ScrollHandle,
    /// Threads mentioned per target. A send carries those the prompt still links.
    threads: HashMap<Target, Vec<ThreadContext>>,
}

/// The sidebar's threads in the shape thread search ranks.
fn palette_threads(state: &AppState) -> Vec<PaletteThread> {
    state
        .sidebar
        .model
        .threads
        .iter()
        .filter(|thread| !thread.deleted)
        .map(|thread| PaletteThread {
            environment_id: t3_core::sidebar::ENVIRONMENT_ID.to_owned(),
            id: thread.id.clone(),
            project_id: thread.project_id.clone(),
            title: thread.title.clone(),
            branch: thread.branch.clone(),
            archived_at: thread.archived.then(|| thread.updated_at.map(format_iso).unwrap_or_default()),
            hidden: !thread.listed,
            created_at: thread.created_at.map(format_iso).unwrap_or_default(),
            updated_at: thread.updated_at.map(format_iso).unwrap_or_default(),
            latest_user_message_at: thread.latest_user_message_at.map(format_iso),
            latest_user_authored_message_at: thread.latest_user_authored_message_at.map(|at| at.map(format_iso)),
            opens: thread.opens.clone(),
            ..Default::default()
        })
        .collect()
}

impl Composer {
    pub(super) fn menu_open(&self) -> bool {
        self.menu.triggers.trigger().is_some()
    }

    /// The directory searches and skills resolve against: the thread's worktree, else its project.
    fn menu_cwd(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let worktree = self.thread_id().and_then(|id| state.shell.thread(&id).and_then(|thread| thread.worktree_path.clone()));
        worktree.or_else(|| Some(state.shell.project(self.snapshot.project_id.as_deref()?)?.workspace_root.clone()))
    }

    fn environment_id(&self, cx: &App) -> String {
        let config = self.state.read(cx).server_config.config.as_ref();
        config.map(|config| config.environment.environment_id.clone()).unwrap_or_else(|| t3_core::sidebar::ENVIRONMENT_ID.to_owned())
    }

    /// Re-reads the token under the caret after an edit (`force`) or a caret move.
    pub(super) fn sync_menu(&mut self, force: bool, cx: &mut Context<Self>) {
        let caret = {
            let input = self.input.read(cx);
            (input.cursor(), input.text().len())
        };
        if !force && self.menu.caret == Some(caret) {
            return;
        }
        self.menu.caret = Some(caret);
        // A question's custom answer is literal text: no menus (`isLiteralPendingAnswer`).
        let candidate = match self.answering.is_some() || self.snapshot.target.is_none() {
            true => None,
            false => rules::detect_trigger(&self.prompt(cx), caret.0).filter(|trigger| trigger.kind != TriggerKind::PullRequest),
        };
        if self.menu.triggers.set(candidate) {
            self.refresh_menu(cx);
            cx.notify();
        }
    }

    /// Rebuilds the rows for the active trigger, starting the searches it needs.
    pub(super) fn refresh_menu(&mut self, cx: &mut Context<Self>) {
        let Some(trigger) = self.menu.triggers.trigger().cloned() else {
            self.menu.items.clear();
            self.menu.loading = false;
            self.menu.path_search = None;
            return;
        };
        let cwd = self.menu_cwd(cx);
        let (items, loading) = match trigger.kind {
            TriggerKind::Path => self.path_menu(&trigger, cwd, cx),
            TriggerKind::SlashCommand | TriggerKind::Skill => self.skill_menu(&trigger, cwd, cx),
            TriggerKind::PullRequest => (Vec::new(), false),
        };
        self.menu.items = items;
        self.menu.loading = loading;
    }

    /// Threads first, then the workspace entries of the latest search for this cwd. While a
    /// newer query loads, the previous rows stay, but picking one does nothing.
    fn path_menu(&mut self, trigger: &Trigger, cwd: Option<String>, cx: &mut Context<Self>) -> (Vec<MenuItem>, bool) {
        let query = trigger.query.trim().to_owned();
        let mut items = if query.is_empty() {
            Vec::new()
        } else {
            let environment = self.environment_id(cx);
            let threads = palette_threads(self.state.read(cx));
            rules::thread_items(&threads, &environment, self.thread_id().as_deref(), &trigger.query, now_ms())
        };
        let Some(cwd) = cwd.filter(|_| !query.is_empty()) else {
            self.menu.path_search = None;
            return (items, false);
        };
        let mut current = false;
        if let Some((searched_cwd, searched, entries)) = &self.menu.paths
            && *searched_cwd == cwd
        {
            current = *searched == query;
            items.extend(rules::path_items(entries));
        }
        if !current && !matches!(&self.menu.path_search, Some((c, q, _)) if *c == cwd && *q == query) {
            self.search_paths(cwd, query, cx);
        }
        (items, !current)
    }

    fn search_paths(&mut self, cwd: String, query: String, cx: &mut Context<Self>) {
        let (searched_cwd, searched) = (cwd.clone(), query.clone());
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(rules::PATH_SEARCH_DEBOUNCE_MS)).await;
            let request = this.update(cx, |this, cx| {
                let payload = rules::search_entries_request(&searched_cwd, &searched);
                this.state.read(cx).request(rules::SEARCH_ENTRIES, payload, |reply| Ok(rules::decode_entries(&reply)))
            });
            let Ok(Some(request)) = request else { return };
            let entries = request.await.ok().and_then(Result::ok).unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if matches!(&this.menu.path_search, Some((c, q, _)) if *c == searched_cwd && *q == searched) {
                    this.menu.path_search = None;
                }
                this.menu.paths = Some((searched_cwd, searched, entries));
                this.refresh_menu(cx);
                cx.notify();
            });
        });
        self.menu.path_search = Some((cwd, query, task));
    }

    /// The `/` and `$` rows from the selected provider, with its snapshot for this cwd.
    fn skill_menu(&mut self, trigger: &Trigger, cwd: Option<String>, cx: &mut Context<Self>) -> (Vec<MenuItem>, bool) {
        let resolved = self.resolve(cx);
        let state = self.state.read(cx);
        let show_skills = state.client_settings.show_skills_in_slash_menu;
        let provider = resolved.model.as_ref().and_then(|model| state.server_config.config.as_ref()?.provider(&model.instance_id)).cloned();
        let Some(provider) = provider else { return (Vec::new(), false) };
        let key = cwd.as_ref().map(|cwd| (provider.instance_id.clone(), cwd.clone()));
        let fetched = key.as_ref().and_then(|key| self.menu.snapshots.get(key));
        let snapshot = rules::workspace_snapshot(&provider, fetched, cwd.as_deref());
        let (skills, commands) = rules::skills_and_commands(&provider, snapshot);
        let now = now_ms();
        let current = rules::snapshot_is_current(snapshot, now);
        let complete = snapshot.is_some_and(|snapshot| snapshot.slash_commands_pending != Some(true));
        let items = match trigger.kind {
            TriggerKind::SlashCommand => {
                let options = SlashMenuOptions {
                    plan_mode: resolved.plan_enabled,
                    side_chat: self.fixed.is_none() && state.side_chats_offered(),
                    side_chat_disabled: self.side_chat_disabled_reason(cx),
                    compact: self.compact_available(trigger, cx),
                    show_skills,
                };
                rules::slash_items(trigger, &provider.driver, &skills, &commands, options)
            }
            _ => rules::skill_items(trigger, &provider.driver, &skills),
        };
        let Some(key) = key else { return (items, false) };
        if !current {
            self.refresh_snapshot(key.clone(), now, cx);
        }
        (items, !complete && self.menu.scanning.contains(&key))
    }

    /// `useProviderWorkspaceSnapshotRefresh`: one scan per TTL, a short cooldown after a failure.
    fn refresh_snapshot(&mut self, key: SnapshotKey, now: i64, cx: &mut Context<Self>) {
        if self.menu.scanning.contains(&key)
            || self.menu.requested.get(&key).is_some_and(|at| now - at < rules::WORKSPACE_SNAPSHOT_TTL_MS)
        {
            return;
        }
        self.menu.requested.insert(key.clone(), now);
        self.menu.scanning.insert(key.clone());
        let request = self.state.read(cx).rpc(rules::REFRESH_PROVIDERS, rules::refresh_providers_request(&key.0, &key.1));
        cx.spawn(async move |this, cx| {
            let reply = request.await;
            let _ = this.update(cx, |this, cx| {
                this.menu.scanning.remove(&key);
                match reply.ok().and_then(|reply| rules::refreshed_snapshot(&reply, &key.0, &key.1)) {
                    Some(snapshot) => {
                        this.menu.snapshots.insert(key, snapshot);
                    }
                    None => {
                        this.menu.requested.insert(key, now_ms() - rules::WORKSPACE_SNAPSHOT_TTL_MS + SNAPSHOT_RETRY_MS);
                    }
                }
                if this.menu_open() {
                    this.refresh_menu(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// `compactSlashCommandAvailable`: `/compact` alone in the prompt of an idle server thread
    /// that has something to compact.
    fn compact_available(&self, trigger: &Trigger, cx: &App) -> bool {
        let Some(Target::Thread(thread_id)) = &self.snapshot.target else { return false };
        let snapshot = &self.snapshot;
        if snapshot.running
            || !snapshot.loaded
            || !snapshot.pending.approvals.is_empty()
            || !snapshot.pending.user_inputs.is_empty()
            || !self.staged().is_empty()
        {
            return false;
        }
        let text = self.prompt(cx);
        let blank = |range: Option<&str>| range.is_some_and(|text| text.trim().is_empty());
        if !blank(text.get(..trigger.range_start)) || !blank(text.get(trigger.range_end..)) {
            return false;
        }
        let state = self.state.read(cx);
        state.thread_state(thread_id).projection.as_ref().filter(|projection| projection.thread.id == *thread_id).is_some_and(rules::has_compactable_conversation)
    }

    fn active_menu_item(&self) -> Option<&MenuItem> {
        let key = self.menu.triggers.trigger().map(rules::search_key);
        let (id, highlight_key) = match &self.menu.highlighted {
            Some((id, highlight_key)) => (Some(id.as_str()), Some(highlight_key.as_str())),
            None => (None, None),
        };
        rules::active_item(&self.menu.items, id, highlight_key, key.as_deref())
    }

    fn highlight(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(key) = self.menu.triggers.trigger().map(rules::search_key) else { return };
        if self.menu.highlighted.as_ref().is_some_and(|(current, k)| *current == id && *k == key) {
            return;
        }
        self.menu.highlighted = Some((id, key));
        cx.notify();
    }

    /// The menu's share of `composerCommandKey`. Returns whether the key was taken.
    pub(super) fn menu_key(&mut self, key: MenuKey, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // Only the prompt's own keys: the model picker's search field has its own Up and Down.
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        if !self.menu_open()
            || !focused
            || self.input.update(cx, |input, cx| gpui::EntityInputHandler::marked_text_range(input, window, cx).is_some())
        {
            return false;
        }
        match key {
            MenuKey::Up | MenuKey::Down => {
                let current = self.active_menu_item().map(|item| item.id.clone());
                let Some(next) = rules::nudge(&self.menu.items, current.as_deref(), key == MenuKey::Down) else { return false };
                if let Some(index) = self.menu.items.iter().position(|item| item.id == next) {
                    self.menu.scroll.scroll_to_item(index);
                }
                self.highlight(next, cx);
                true
            }
            MenuKey::Accept => {
                let Some(item) = self.active_menu_item().cloned() else { return false };
                self.select_menu_item(item, window, cx);
                true
            }
            MenuKey::Dismiss => {
                let trigger = self.current_trigger(cx);
                self.menu.triggers.dismiss(trigger);
                self.refresh_menu(cx);
                cx.notify();
                true
            }
        }
    }

    /// The trigger under the caret now, unless Escape dismissed it.
    fn current_trigger(&self, cx: &App) -> Option<Trigger> {
        let cursor = self.input.read(cx).cursor();
        self.menu.triggers.resolve(rules::detect_trigger(&self.prompt(cx), cursor))
    }

    /// Replaces `start..end` the way typing would, so undo and IME state stay intact.
    fn replace_prompt_range(&mut self, start: usize, end: usize, replacement: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_selected_range(start..end, cx);
            input.replace(replacement.to_owned(), window, cx);
        });
    }

    /// Replaces the trigger token, swallowing one space after it when the text ends in one.
    fn replace_trigger(&mut self, text: &str, trigger: &Trigger, replacement: &str, window: &mut Window, cx: &mut Context<Self>) {
        let end = rules::extend_range_for_trailing_space(text, trigger.range_end, replacement);
        self.replace_prompt_range(trigger.range_start, end, replacement, window, cx);
    }

    /// `onSelectComposerItem`.
    pub(super) fn select_menu_item(&mut self, item: MenuItem, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.prompt(cx);
        let Some(trigger) = self.current_trigger(cx) else { return };
        match item.action {
            MenuAction::Path { path, .. } => {
                let cwd = self.menu_cwd(cx);
                let current = matches!(&self.menu.paths, Some((c, q, _)) if Some(c) == cwd.as_ref() && *q == trigger.query.trim());
                if trigger.kind != TriggerKind::Path || !current {
                    return;
                }
                self.replace_trigger(&text, &trigger, &rules::file_mention_text(&path), window, cx);
            }
            MenuAction::Slash(SlashCommand::Model) => {
                self.replace_prompt_range(trigger.range_start, trigger.range_end, "", window, cx);
                if !self.picker_open {
                    self.toggle_picker(window, cx);
                }
            }
            MenuAction::Slash(command @ (SlashCommand::Plan | SlashCommand::Default)) => {
                if !self.resolve(cx).plan_enabled {
                    return;
                }
                let mode = if command == SlashCommand::Plan { InteractionMode::Plan } else { InteractionMode::Default };
                if let Some(choices) = self.choices_mut() {
                    choices.interaction_mode = Some(mode);
                }
                self.replace_prompt_range(trigger.range_start, trigger.range_end, "", window, cx);
                self.draft_changed(cx);
            }
            MenuAction::Slash(command @ (SlashCommand::Side | SlashCommand::Btw)) => {
                if self.side_chat_disabled_reason(cx).is_some() {
                    return;
                }
                self.replace_prompt_range(trigger.range_start, trigger.range_end, &format!("/{} ", command.name()), window, cx);
            }
            MenuAction::ProviderCommand { name } => self.replace_trigger(&text, &trigger, &format!("/{name} "), window, cx),
            MenuAction::Skill { name, .. } => self.replace_trigger(&text, &trigger, &format!("${name} "), window, cx),
            MenuAction::Thread { thread_id, title } => {
                if trigger.kind != TriggerKind::Path {
                    return;
                }
                let context = ThreadContext::new(&self.environment_id(cx), &thread_id, &title);
                self.replace_trigger(&text, &trigger, &format!("{} ", context.reference()), window, cx);
                if let Some(target) = self.snapshot.target.clone() {
                    let threads = self.menu.threads.entry(target).or_default();
                    threads.retain(|existing| existing.context_id != context.context_id);
                    threads.push(context);
                }
                self.draft_changed(cx);
            }
        }
        self.menu.highlighted = None;
        // A click on a row leaves the caret where typing continues; `/model` hands focus to the picker.
        if !self.picker_open {
            self.focus(window, cx);
        }
        cx.notify();
    }

    /// Inserts a file mention at the end of the prompt, after a space when the prompt does not
    /// end in one (`insertComposerTextAtEnd` with `ensureLeadingBoundary`). For the file tree and
    /// go-to-file.
    pub fn insert_mention(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.snapshot.target.is_none() || self.answering.is_some() {
            return;
        }
        let text = self.prompt(cx);
        let boundary = if text.is_empty() || text.ends_with(char::is_whitespace) { "" } else { " " };
        let end = text.len();
        self.replace_prompt_range(end, end, &format!("{boundary}{}", rules::file_mention_text(path)), window, cx);
        self.focus(window, cx);
    }

    /// The `context` a send to the current target carries: the threads its text still links,
    /// then its attachments.
    pub(super) fn message_context(&self, text: &str, attachments: &[t3_core::composer::attachments::Attachment]) -> Option<serde_json::Value> {
        let threads = self.snapshot.target.as_ref().and_then(|target| self.menu.threads.get(target)).map(Vec::as_slice).unwrap_or_default();
        t3_core::composer::attachments::message_context(text, threads, attachments)
    }

    pub(super) fn mentions(&self, target: &Target) -> &[ThreadContext] {
        self.menu.threads.get(target).map(Vec::as_slice).unwrap_or_default()
    }

    pub(super) fn seed_mentions(&mut self, target: &Target, threads: Vec<ThreadContext>) {
        if !threads.is_empty() {
            self.menu.threads.insert(target.clone(), threads);
        }
    }

    /// Forgets a target's mentioned threads once its message went out.
    pub(super) fn clear_mentions(&mut self, target: &Target) {
        self.menu.threads.remove(target);
    }

    pub(super) fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let trigger = self.menu.triggers.trigger()?;
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let outline = if dark { gpui::white().opacity(0.05) } else { hsla(p.foreground).opacity(0.08) };
        let body = if self.menu.items.is_empty() {
            div()
                .px(px(20.))
                .pt(px(14.))
                .pb(px(28.))
                .text_xs()
                .text_color(hsla(p.secondary_label))
                .child(rules::empty_text(trigger.kind, self.menu.loading))
                .into_any_element()
        } else {
            let active = self.active_menu_item().map(|item| item.id.clone());
            div()
                .id("composer-menu-list")
                .max_h(px(LIST_MAX_HEIGHT))
                .overflow_y_scroll()
                .track_scroll(&self.menu.scroll)
                .p(px(8.))
                .children(self.menu.items.iter().enumerate().map(|(index, item)| {
                    self.render_menu_row(index, item, trigger.kind, active.as_deref() == Some(item.id.as_str()), cx)
                }))
                .into_any_element()
        };
        let surface = v_flex()
            .id("composer-menu")
            .occlude()
            .mb(px(-OVERLAP))
            .rounded_t(px(16.))
            .border_1()
            .border_color(outline)
            .bg(hsla(if dark { p.surface_raised } else { p.card }))
            .pb(px(OVERLAP))
            .child(body);
        Some(div().absolute().left(px(22.)).right(px(22.)).bottom_full().child(surface).into_any_element())
    }

    fn render_menu_row(&self, index: usize, item: &MenuItem, kind: TriggerKind, active: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let muted = hsla(p.secondary_label);
        let icon = match &item.action {
            MenuAction::Path { kind: PathKind::Directory, .. } => Some(Icon::Folder.render(px(16.), hsla(p.muted_foreground))),
            MenuAction::Path { .. } => Some(Icon::File.render(px(16.), hsla(p.muted_foreground))),
            MenuAction::Thread { .. } => Some(Icon::MessagesSquare.render(px(16.), muted)),
            _ => None,
        };
        let label: AnyElement = match (&item.action, kind) {
            (MenuAction::Skill { .. }, TriggerKind::SlashCommand) => h_flex()
                .child(div().text_color(muted).child("/skill:"))
                .child(SharedString::from(item.label.trim_start_matches("/skill:").to_owned()))
                .into_any_element(),
            _ => SharedString::from(item.label.clone()).into_any_element(),
        };
        let badge = match &item.action {
            MenuAction::Skill { source, .. } => Some(skill_badge(*source, kind == TriggerKind::Skill, cx)),
            _ => None,
        };
        let id = item.id.clone();
        let picked = item.clone();
        h_flex()
            .id(("composer-menu-item", index))
            .min_h(px(28.))
            .px(px(8.))
            .py(px(6.))
            .gap(px(8.))
            .rounded(px(8.))
            .text_sm()
            .cursor_pointer()
            .when(active, |this| this.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
            .on_mouse_move(cx.listener(move |this, _, _, cx| this.highlight(id.clone(), cx)))
            .on_click(cx.listener(move |this, _, window, cx| this.select_menu_item(picked.clone(), window, cx)))
            .children(icon)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.))
                    .child(div().flex_shrink_0().max_w(relative(0.45)).min_w_0().truncate().text_xs().font_weight(FontWeight::MEDIUM).child(label))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(SharedString::from(item.description.clone())))
                    .children(badge),
            )
            .into_any_element()
    }
}

/// `SkillSourceBadge`: the secondary badge naming where a skill comes from.
fn skill_badge(source: SkillSource, suffix: bool, cx: &App) -> AnyElement {
    let p = palette(cx);
    let icon = match source {
        SkillSource::App => Icon::Blocks,
        SkillSource::Repo | SkillSource::Project => Icon::Folder,
        SkillSource::Personal => Icon::UserRound,
        SkillSource::System => Icon::Settings,
        SkillSource::Other => Icon::Package,
    };
    let color = hsla(p.secondary_foreground);
    h_flex()
        .flex_shrink_0()
        .h(px(18.))
        .min_w(px(18.))
        .px(px(3.))
        .gap(px(4.))
        .rounded(px(4.))
        .bg(hsla(p.secondary))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(icon.render(px(12.), color.opacity(0.8)))
        .child(if suffix { format!("{} Skill", source.label()) } else { source.label().to_owned() })
        .into_any_element()
}
