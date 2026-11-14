//! New threads (ledger CO-NEWTHREAD, TV-DRAFT): the `chat.new` action, the chat header's
//! new-thread controls, and the draft hero above the composer. Ports `resolveThreadActionProjectRef`
//! and `startNewThreadFromContext` (`web/src/lib/chatThreadActions.ts`), `resolveDefaultProjectRef`
//! (`web/src/hooks/useHandleNewThread.ts`), the project breadcrumb in `ChatHeader.tsx`, and
//! `DraftHeroProjectHeading.tsx`, `DraftHeroHeadline.tsx`, and `TriageDraftHero.tsx` (the project
//! picker, "or start without a project", the scratch draft's "What should we work on?", and the
//! triage hero's "What went wrong?" + `npx t3 triage` copy prompt). The draft's first send
//! launches the thread (see `composer`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Point, Render,
    ScrollHandle, SharedString, Styled, Subscription, TextRun, Window, actions, anchored, canvas, deferred, div, point, prelude::*, px,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use t3_core::auth::ORCHESTRATION_OPERATE_SCOPE;
use t3_core::sidebar::ENVIRONMENT_ID;
use t3_core::sidebar::grouping::{
    EnvironmentOwner, GroupConsumer, ProjectEnvironmentBadge, ScopedProjectGroup, build_project_groups, filter_project_scope_items, order_while_open,
    project_groups_span_environments, resolve_project_environment_badge,
};

use crate::app_state::{AppEvent, AppState};
use crate::composer::Composer;
use crate::icons::Icon;
use crate::project_icon::ProjectVisual;
use crate::theme::{hsla, palette};

actions!(t3, [NewThread, NewThreadWithoutProject]);

pub(crate) struct ProjectPickerEntry {
    pub group: t3_core::sidebar::grouping::ScopedProjectGroup,
    pub target: t3_core::sidebar::grouping::ScopedProjectMember,
    pub is_preferred: bool,
}

pub(crate) fn picker_entries(
    groups: Vec<t3_core::sidebar::grouping::ScopedProjectGroup>,
    preferred: Option<&t3_core::sidebar::grouping::ProjectRef>,
) -> Vec<ProjectPickerEntry> {
    let mut entries = groups.into_iter().filter(|group| !group.archived).filter_map(|group| {
        let target = t3_core::sidebar::grouping::pick_project_target(&group, preferred)?;
        let is_preferred = preferred.is_some_and(|reference| group.member_refs.contains(reference));
        Some(ProjectPickerEntry { group, target, is_preferred })
    }).collect::<Vec<_>>();
    entries.sort_by_key(|entry| !entry.group.pinned);
    if let Some(index) = entries.iter().position(|entry| entry.is_preferred && !entry.group.pinned) {
        let entry = entries.remove(index);
        let pinned = entries.iter().take_while(|entry| entry.group.pinned).count();
        entries.insert(pinned, entry);
    }
    entries
}

pub(crate) fn draft_picker_entries(entries: Vec<ProjectPickerEntry>) -> impl Iterator<Item = ProjectPickerEntry> {
    entries.into_iter().filter(|entry| !entry.target.project.is_scratch)
}

pub fn init(state: Entity<AppState>, cx: &mut App) {
    let scratch = state.clone();
    cx.on_action(move |_: &NewThread, cx| state.update(cx, start_new_thread));
    cx.on_action(move |_: &NewThreadWithoutProject, cx| crate::home::start_without_project(&scratch, cx));
}

/// `resolveThreadActionProjectRef`: the open thread's project, else the draft's, else the first
/// project that is not archived.
pub fn default_project(state: &AppState) -> Option<String> {
    let thread_project = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id)).map(|t| t.project_id.clone());
    thread_project.or_else(|| state.draft_project.clone()).or_else(|| {
        let snapshot = state.shell.snapshot.as_ref()?;
        snapshot.projects.iter().find(|project| project.archived_at.is_none()).map(|project| project.id.clone())
    })
}

pub fn start_new_thread(state: &mut AppState, cx: &mut Context<AppState>) {
    if let Some(project_id) = default_project(state) {
        state.start_draft(&project_id, cx);
    }
}

/// The header's New thread button: a ghost 28px toggle with the square-pen icon.
pub fn new_thread_button(cx: &App) -> impl IntoElement {
    let p = palette(cx);
    div()
        .id("new-thread")
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .hover(|this| this.bg(hsla(p.accent)))
        .child(Icon::SquarePen.render(px(16.), hsla(p.foreground).opacity(0.8)))
        .tooltip(|window, cx| Tooltip::new("New thread (⌘N)").build(window, cx))
        .on_click(|_, window, cx| window.dispatch_action(Box::new(NewThread), cx))
}

const ADD_PROJECT_VALUE: &str = "add-project";
const HERO_LARGE_BREAKPOINT: f32 = 1024.;
const HERO_SMALL_BREAKPOINT: f32 = 640.;
const HEADING_MAX_WIDTH: f32 = 1024.;

#[derive(Clone)]
enum PinTarget {
    Scoped(ScopedProjectGroup),
    Local(Vec<String>),
}

#[derive(Clone)]
struct HeroPickerEntry {
    key: String,
    project_id: String,
    label: String,
    pinned: bool,
    owner: Option<EnvironmentOwner>,
    pin_target: Option<PinTarget>,
    env_badge: Option<ProjectEnvironmentBadge>,
    visual: ProjectVisual,
}

#[derive(Clone)]
enum PickerRow {
    Project(Box<HeroPickerEntry>),
    AddProject,
}

impl PickerRow {
    fn value(&self) -> &str {
        match self {
            PickerRow::Project(entry) => &entry.key,
            PickerRow::AddProject => ADD_PROJECT_VALUE,
        }
    }

    fn label(&self) -> &str {
        match self {
            PickerRow::Project(entry) => &entry.label,
            PickerRow::AddProject => "Add project",
        }
    }
}

fn environment_machine_icon(state: &AppState, saved_id: &str) -> Icon {
    let config = state
        .scoped_environment(saved_id)
        .and_then(|entry| entry.config.config.as_ref())
        .or_else(|| (state.environments.active.as_deref() == Some(saved_id)).then_some(state.server_config.config.as_ref()).flatten());
    let machine = config
        .and_then(|cfg| cfg.settings.environment_icon.as_deref().or_else(|| cfg.environment.platform.as_ref().map(|platform| platform.machine.as_str())))
        .unwrap_or("server");
    match machine {
        "laptop" => Icon::Laptop,
        "desktop" => Icon::Monitor,
        "cloud" => Icon::Cloud,
        "linux" => Icon::Linux,
        "mac-mini" => Icon::MacMini,
        "mac-studio" => Icon::MacStudio,
        _ => Icon::Server,
    }
}

fn primary_environment_id(state: &AppState) -> Option<&str> {
    state.local_saved_environment().map(|environment| environment.id.as_str()).or_else(|| {
        let mut local = state.environments.environments.iter().filter(|environment| environment.local);
        let first = local.next()?;
        local.next().is_none().then_some(first.id.as_str())
    })
}

fn can_pin_scoped_group(state: &AppState, group: &ScopedProjectGroup) -> bool {
    !group.registrations.is_empty()
        && group.registrations.iter().all(|project| {
            state.scoped_ready(&project.owner, ORCHESTRATION_OPERATE_SCOPE)
                && state
                    .scoped_environment(&project.owner.saved_id)
                    .and_then(|entry| entry.config.config.as_ref())
                    .is_some_and(|config| config.environment.capabilities.project_organization == Some(true))
        })
}

fn pin_tooltip_label(pinned: bool, label: &str, cx: &App) -> String {
    let action = if pinned { "Unpin" } else { "Pin" };
    match crate::keymap::label("thread.pin", cx) {
        Some(shortcut) => format!("{action} {label} ({shortcut})"),
        None => format!("{action} {label}"),
    }
}

fn inline_heading_width(project_title: &str, window: &mut Window, cx: &App) -> f32 {
    let text = format!("What should we build in {project_title}?");
    let font = gpui::font(cx.theme().font_family.clone());
    let run = TextRun { len: text.len(), font, color: gpui::black(), background_color: None, underline: None, strikethrough: None };
    let shaped = window.text_system().shape_line(SharedString::from(text), px(30.), &[run], None);
    f32::from(shaped.width) + 16.
}

const TRIAGE_PROMPT_FILE_NAME: &str = "triage-playbook.md";
const TRIAGE_CLI_COMMAND: &str = "npx t3 triage";

fn is_triage_prompt(prompt: &str) -> bool {
    prompt.contains(&format!("[{TRIAGE_PROMPT_FILE_NAME}]("))
}

/// The draft's page: "What should we build in <project>?" over the composer, centered.
pub struct DraftHero {
    state: Entity<AppState>,
    composer: Entity<Composer>,
    projects_open: bool,
    popup_origin: Point<Pixels>,
    trigger_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    heading_width: Rc<Cell<f32>>,
    dismissed: Option<Instant>,
    triage_copied_until: Option<Instant>,
    search: Option<Entity<InputState>>,
    search_sub: Option<Subscription>,
    order: RefCell<Vec<String>>,
    highlighted: Option<String>,
    scroll: ScrollHandle,
    popup_focus: FocusHandle,
    return_focus: Option<FocusHandle>,
    _subscription: Subscription,
}

impl DraftHero {
    pub fn new(state: Entity<AppState>, composer: Entity<Composer>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Selection | AppEvent::Shell) {
                this.projects_open = false;
                this.order.borrow_mut().clear();
                this.highlighted = None;
                cx.notify();
            }
        });
        DraftHero {
            state,
            composer,
            projects_open: false,
            popup_origin: point(px(0.), px(0.)),
            trigger_bounds: Rc::new(Cell::new(None)),
            heading_width: Rc::new(Cell::new(0.)),
            dismissed: None,
            triage_copied_until: None,
            search: None,
            search_sub: None,
            order: RefCell::new(Vec::new()),
            highlighted: None,
            scroll: ScrollHandle::new(),
            popup_focus: cx.focus_handle(),
            return_focus: None,
            _subscription: subscription,
        }
    }

    fn ensure_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(search) = &self.search {
            return search.clone();
        }
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects..."));
        let subscription = cx.subscribe_in(&search, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                this.sync_highlight(cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } if this.projects_open => {
                this.activate_highlighted(window, cx);
            }
            _ => {}
        });
        self.search = Some(search.clone());
        self.search_sub = Some(subscription);
        search
    }

    fn entries(&self, cx: &App) -> (Vec<HeroPickerEntry>, bool) {
        let app = self.state.read(cx);
        let current = app.draft_project.clone();
        if !app.fleet.environments.is_empty() {
            let preferred = current
                .as_ref()
                .and_then(|id| Some(t3_core::sidebar::grouping::ProjectRef { environment_id: app.environments.active.clone()?, project_id: id.clone() }));
            let built: Vec<_> = draft_picker_entries(picker_entries(app.project_groups_for(GroupConsumer::DraftPicker, cx), preferred.as_ref())).collect();
            let groups: Vec<_> = built.iter().map(|entry| entry.group.clone()).collect();
            let span = project_groups_span_environments(&groups);
            let primary = primary_environment_id(app);
            let rows = built
                .into_iter()
                .map(|entry| {
                    let badge = span.then(|| resolve_project_environment_badge(&entry.group, primary)).flatten();
                    let visual = ProjectVisual::for_scoped_project(app, &entry.target.project.reference());
                    let pin_target = can_pin_scoped_group(app, &entry.group).then(|| PinTarget::Scoped(entry.group.clone()));
                    HeroPickerEntry {
                        key: entry.group.key,
                        project_id: entry.target.project.project.id,
                        label: entry.group.label,
                        pinned: entry.group.pinned,
                        owner: Some(entry.target.project.owner),
                        pin_target,
                        env_badge: badge,
                        visual,
                    }
                })
                .collect();
            return (rows, span);
        }
        let Some(snapshot) = app.shell.snapshot.as_ref() else {
            return (Vec::new(), false);
        };
        let mut groups: Vec<_> = build_project_groups(ENVIRONMENT_ID, &snapshot.projects, &app.sidebar.settings.grouping)
            .into_iter()
            .filter(|group| !group.archived && !app.is_scratch_project(&group.representative_id))
            .collect();
        groups.sort_by_key(|group| !group.pinned);
        if let Some(current_id) = current.as_deref()
            && let Some(index) = groups.iter().position(|group| !group.pinned && group.member_project_ids.iter().any(|id| id == current_id))
        {
            let entry = groups.remove(index);
            let pinned = groups.iter().take_while(|group| group.pinned).count();
            groups.insert(pinned, entry);
        }
        let can_pin = app.can_pin_projects();
        let rows = groups
            .into_iter()
            .map(|group| HeroPickerEntry {
                key: group.key,
                visual: ProjectVisual::for_project(app, &group.representative_id),
                project_id: group.representative_id,
                label: group.label,
                pinned: group.pinned,
                owner: None,
                pin_target: can_pin.then_some(PinTarget::Local(group.member_project_ids)),
                env_badge: None,
            })
            .collect();
        (rows, false)
    }

    fn visible_rows(&self, cx: &App) -> (Vec<PickerRow>, bool) {
        let (entries, span) = self.entries(cx);
        let ordered = order_while_open(&mut self.order.borrow_mut(), entries, |entry| entry.key.as_str());
        let items: Vec<PickerRow> = ordered.into_iter().map(|entry| PickerRow::Project(Box::new(entry))).chain(std::iter::once(PickerRow::AddProject)).collect();
        let query = self.search.as_ref().map(|search| search.read(cx).value().to_string()).unwrap_or_default();
        let filtered = if query.trim().is_empty() {
            items
        } else {
            let project_items: Vec<_> = items.into_iter().filter(|item| !matches!(item, PickerRow::AddProject)).collect();
            filter_project_scope_items(project_items, &query, PickerRow::value, PickerRow::label)
        };
        (filtered, span)
    }

    fn sync_highlight(&mut self, cx: &App) {
        if !self.projects_open {
            self.highlighted = None;
            return;
        }
        let (rows, _) = self.visible_rows(cx);
        if self.highlighted.as_deref().is_some_and(|cur| rows.iter().any(|row| row.value() == cur)) {
            return;
        }
        self.highlighted = rows.first().map(|row| row.value().to_owned());
        self.scroll.scroll_to_item(0);
    }

    fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let (rows, _) = self.visible_rows(cx);
        if rows.is_empty() {
            self.highlighted = None;
            cx.notify();
            return;
        }
        let next = match self.highlighted.as_deref().and_then(|cur| rows.iter().position(|row| row.value() == cur)) {
            Some(idx) => (idx as isize + step).rem_euclid(rows.len() as isize) as usize,
            None if step >= 0 => 0,
            None => rows.len() - 1,
        };
        self.highlighted = Some(rows[next].value().to_owned());
        self.scroll.scroll_to_item(next);
        cx.notify();
    }

    fn open_picker(&mut self, click_pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if self.dismissed.take().is_some_and(|at| at.elapsed() < Duration::from_millis(300)) {
            return;
        }
        if self.projects_open {
            self.close_picker(Some(window), false, cx);
            return;
        }
        self.order.borrow_mut().clear();
        self.return_focus = window.focused(cx);
        self.popup_origin = match self.trigger_bounds.get() {
            Some(bounds) => point((bounds.left() + bounds.size.width * 0.5 - px(144.)).max(px(8.)), bounds.bottom() + px(4.)),
            None => click_pos,
        };
        self.projects_open = true;
        let search = self.ensure_search(window, cx);
        search.update(cx, |input, cx| {
            if !input.value().is_empty() {
                input.set_value("", window, cx);
            }
            input.focus(window, cx);
        });
        self.sync_highlight(cx);
        cx.notify();
    }

    fn close_picker(&mut self, window: Option<&mut Window>, restore_focus: bool, cx: &mut Context<Self>) {
        self.projects_open = false;
        self.order.borrow_mut().clear();
        self.highlighted = None;
        let return_focus = self.return_focus.take();
        if let Some(window) = window {
            if let Some(search) = &self.search
                && !search.read(cx).value().is_empty()
            {
                search.update(cx, |input, cx| input.set_value("", window, cx));
            }
            if restore_focus && let Some(focus) = return_focus {
                window.focus(&focus, cx);
            }
        }
        cx.notify();
    }

    fn select_entry(&mut self, entry: HeroPickerEntry, window: &mut Window, cx: &mut Context<Self>) {
        self.close_picker(Some(window), false, cx);
        let (current, active_env) = {
            let app = self.state.read(cx);
            (app.draft_project.clone(), app.environments.active.clone())
        };
        let same_target = current.as_deref() == Some(entry.project_id.as_str())
            && entry.owner.as_ref().is_none_or(|owner| active_env.as_deref() == Some(&owner.saved_id));
        if !same_target {
            if let Some(from) = current {
                let same_env = entry.owner.as_ref().is_none_or(|owner| active_env.as_deref() == Some(&owner.saved_id));
                self.composer.update(cx, |composer, cx| {
                    composer.retarget_draft(&from, &entry.project_id, same_env, window, cx);
                });
            }
            self.state.update(cx, |state, cx| match &entry.owner {
                Some(owner) => state.navigate_scoped(owner.clone(), crate::app_state::ScopedRouteAction::Draft(entry.project_id), cx),
                None => state.start_draft(&entry.project_id, cx),
            });
        }
        self.composer.update(cx, |composer, cx| composer.focus(window, cx));
    }

    fn activate_highlighted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (rows, _) = self.visible_rows(cx);
        let chosen = self
            .highlighted
            .as_deref()
            .and_then(|cur| rows.iter().find(|row| row.value() == cur))
            .or_else(|| rows.first())
            .cloned();
        let Some(chosen) = chosen else { return };
        match chosen {
            PickerRow::AddProject => {
                self.close_picker(Some(window), false, cx);
                window.dispatch_action(Box::new(crate::palette::OpenAddProject), cx);
            }
            PickerRow::Project(entry) => self.select_entry(*entry, window, cx),
        }
    }

    fn toggle_pin(&mut self, pin_target: PinTarget, next_pinned: bool, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| match pin_target {
            PinTarget::Scoped(group) => state.pin_scoped_project_group(&group, next_pinned, cx),
            PinTarget::Local(ids) => {
                for id in &ids {
                    state.set_project_pinned(id, next_pinned, cx);
                }
            }
        });
        cx.notify();
    }

    /// `startScratchThread`: ensures the scratch project, then retargets or opens the draft.
    fn start_without_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (scratch_ready, from_project, active_env, rpc) = {
            let app = self.state.read(cx);
            (
                app.scratch_root().is_some(),
                app.draft_project.clone(),
                app.environments.active.clone(),
                app.rpc(t3_core::home::PROJECTS_ENSURE_SCRATCH, serde_json::json!({})),
            )
        };
        if !scratch_ready {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = rpc.await;
            let _ = this.update_in(cx, |hero, window, cx| {
                match result.as_ref().ok().and_then(t3_core::home::ensure_scratch_project_id) {
                    Some(scratch_id) => {
                        let app = hero.state.read(cx);
                        let same_draft = app.selected_thread.is_none()
                            && app.draft_project == from_project
                            && app.environments.active == active_env;
                        if !same_draft {
                            return;
                        }
                        if let Some(from) = from_project {
                            hero.composer.update(cx, |composer, cx| {
                                composer.retarget_draft(&from, &scratch_id, true, window, cx);
                            });
                        }
                        hero.state.update(cx, |state, cx| state.start_draft(&scratch_id, cx));
                        hero.composer.update(cx, |composer, cx| composer.focus(window, cx));
                    }
                    None => {
                        let description = result.err().map_or_else(|| "An error occurred.".to_owned(), |error| error.to_string());
                        hero.state.update(cx, |state, cx| {
                            state.show_toast(
                                crate::toast::Toast::new(crate::toast::ToastKind::Error, "Could not start without a project").description(description),
                                cx,
                            );
                        });
                    }
                }
            });
        })
        .detach();
    }

    fn toggle_highlighted_pin(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(target) = self.highlighted.as_deref() else { return false };
        let (entries, _) = self.entries(cx);
        let Some(entry) = entries.into_iter().find(|entry| entry.key == target) else { return false };
        let Some(pin_target) = entry.pin_target else { return false };
        self.toggle_pin(pin_target, !entry.pinned, cx);
        true
    }

    fn copy_triage_command(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(TRIAGE_CLI_COMMAND.to_owned()));
        let until = Instant::now() + Duration::from_millis(1500);
        self.triage_copied_until = Some(until);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |hero, cx| {
                if hero.triage_copied_until == Some(until) {
                    hero.triage_copied_until = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn render_triage_hero(&self, heading_size: Pixels, heading_lh: Pixels, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let app = self.state.read(cx);
        let machine_label = app
            .active_environment()
            .map(|env| env.label.clone())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| "This machine".to_owned());
        let server_version = app
            .server_config
            .config
            .as_ref()
            .map(|config| config.environment.server_version.clone())
            .filter(|version| !version.is_empty());
        let copied = self.triage_copied_until.is_some_and(|until| Instant::now() < until);
        let mono = crate::theme::font_family(t3_core::client_settings::typography::FontRole::Code, cx);
        let copy_row = if copied {
            div().child("Copied").into_any_element()
        } else {
            h_flex()
                .items_baseline()
                .gap(px(4.))
                .child("or run")
                .child(div().font_family(mono).text_xs().child(TRIAGE_CLI_COMMAND))
                .into_any_element()
        };

        v_flex()
            .w_full()
            .max_w(px(HEADING_MAX_WIDTH))
            .mx_auto()
            .px(px(48.))
            .items_center()
            .child(
                h_flex()
                    .w_full()
                    .justify_center()
                    .text_size(heading_size)
                    .line_height(heading_lh)
                    .text_color(hsla(p.foreground))
                    .child("What went wrong?"),
            )
            .child(
                h_flex()
                    .mt(px(8.))
                    .h(px(24.))
                    .w_full()
                    .justify_center()
                    .items_center()
                    .gap(px(6.))
                    .text_sm()
                    .child(
                        h_flex()
                            .id("triage-machine-label")
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .tooltip(|window, cx| {
                                Tooltip::new("The agent reads T3 Code's logs and source here, and asks before it applies a fix or posts anything.")
                                    .build(window, cx)
                            })
                            .child(machine_label)
                            .when_some(server_version, |this, version| {
                                this.child(
                                    div()
                                        .ml(px(6.))
                                        .font_weight(FontWeight::NORMAL)
                                        .text_color(hsla(p.muted_foreground))
                                        .child(format!("v{version}")),
                                )
                            }),
                    )
                    .child(div().text_color(hsla(p.muted_foreground)).child("·"))
                    .child(
                        div()
                            .id("triage-copy-command")
                            .cursor_pointer()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.muted_foreground))
                            .hover(|this| this.underline().text_color(hsla(p.foreground)))
                            .tooltip(|window, cx| Tooltip::new("Copy command, for a terminal").build(window, cx))
                            .child(copy_row)
                            .on_click(cx.listener(|this, _, _, cx| this.copy_triage_command(cx))),
                    ),
            )
            .into_any_element()
    }

    /// The project picker trigger (`InlineButton tone="picker"`), or dotted "Add a project" trigger
    /// when there are no non-scratch projects (`canChooseProject == false`).
    fn picker_trigger(&self, label: SharedString, tooltip: Option<String>, can_choose: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        if !can_choose {
            return div()
                .id("draft-project-add")
                .min_w_0()
                .max_w_full()
                .cursor_pointer()
                .border_b_1()
                .border_color(hsla(p.muted_foreground).opacity(0.35))
                .text_color(hsla(p.muted_foreground).opacity(0.6))
                .hover(|this| this.border_color(hsla(p.muted_foreground).opacity(0.6)).text_color(hsla(p.muted_foreground).opacity(0.8)))
                .tooltip(|window, cx| Tooltip::new("Add a project").build(window, cx))
                .child(div().min_w_0().truncate().child(label))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::palette::OpenAddProject), cx))
                .into_any_element();
        }
        let bounds_cell = self.trigger_bounds.clone();
        h_flex()
            .id("draft-project")
            .relative()
            .min_w_0()
            .max_w_full()
            .cursor_pointer()
            .font_weight(FontWeight::MEDIUM)
            .text_color(hsla(p.foreground))
            .when(self.projects_open, |this| this.underline())
            .hover(|this| this.underline())
            .when_some(tooltip, |this, title| this.tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx)))
            .child(div().min_w_0().truncate().child(label))
            .child(canvas(move |bounds, _, _| bounds_cell.set(Some(bounds)), |_, _, _, _| {}).absolute().inset_0())
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                this.open_picker(event.position(), window, cx);
            }))
            .into_any_element()
    }

    fn render_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.projects_open {
            return None;
        }
        let search = self.search.as_ref()?.clone();
        let p = palette(cx);
        let (rows, span_environments) = self.visible_rows(cx);
        let (current, active_env) = {
            let app = self.state.read(cx);
            (app.draft_project.clone(), app.environments.active.clone())
        };

        let mut list_rows = Vec::with_capacity(rows.len());
        for row in rows {
            let highlighted = self.highlighted.as_deref() == Some(row.value());
            match row {
                PickerRow::AddProject => {
                    list_rows.push(
                        div()
                            .id("draft-project-row-add")
                            .debug_selector(|| "draft-project-row-add".to_owned())
                            .min_h(px(30.))
                            .px(px(8.))
                            .py(px(3.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .when(highlighted, |s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .hover(|s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .child(Icon::FolderPlus.render(px(16.), hsla(p.muted_foreground)))
                            .child(div().min_w_0().truncate().child("Add project"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_picker(Some(window), false, cx);
                                window.dispatch_action(Box::new(crate::palette::OpenAddProject), cx);
                            }))
                            .into_any_element(),
                    );
                }
                PickerRow::Project(entry) => {
                    let row_key = entry.key.clone();
                    let row_id = SharedString::from(format!("draft-project-row-{row_key}"));
                    let row_group = SharedString::from(format!("draft-project-group-{row_key}"));
                    let pin_btn_group = SharedString::from(format!("draft-project-pin-btn-{row_key}"));
                    let selected = current.as_deref() == Some(entry.project_id.as_str())
                        && entry.owner.as_ref().is_none_or(|owner| active_env.as_deref() == Some(&owner.saved_id));
                    let visual = entry.visual.render(crate::project_icon::size(16., cx), hsla(p.icon_muted), cx);
                    let title_for_tooltip = entry.label.clone();
                    let pin_el = entry.pin_target.clone().map(|pin_target| {
                        let is_pinned = entry.pinned;
                        let pin_label = entry.label.clone();
                        div()
                            .id(SharedString::from(format!("draft-project-pin-{row_key}")))
                            .group(pin_btn_group.clone())
                            .relative()
                            .size(px(24.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .when(!is_pinned, |s| {
                                s.invisible().when(highlighted, |s| s.visible()).group_hover(row_group.clone(), |s| s.visible())
                            })
                            .hover(|s| s.bg(hsla(p.accent)))
                            .tooltip(move |window, cx| Tooltip::new(pin_tooltip_label(is_pinned, &pin_label, cx)).build(window, cx))
                            .child(
                                Icon::Pin
                                    .render(px(14.), hsla(p.muted_foreground))
                                    .when(is_pinned, |icon| icon.group_hover(pin_btn_group.clone(), |s| s.invisible())),
                            )
                            .when(is_pinned, |btn| {
                                btn.child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .invisible()
                                        .group_hover(pin_btn_group, |s| s.visible())
                                        .child(Icon::PinOff.render(px(14.), hsla(p.foreground))),
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_pin(pin_target.clone(), !is_pinned, cx);
                            }))
                            .into_any_element()
                    });
                    let badge_el = (span_environments && entry.env_badge.is_some()).then(|| {
                        let badge = entry.env_badge.clone().expect("checked");
                        let icon = environment_machine_icon(self.state.read(cx), &badge.environment_id);
                        let desc = badge.description;
                        div()
                            .id(SharedString::from(format!("draft-project-env-{row_key}")))
                            .flex_none()
                            .flex()
                            .items_center()
                            .tooltip(move |window, cx| Tooltip::new(desc.clone()).build(window, cx))
                            .child(icon.render(px(14.), hsla(p.muted_foreground)))
                            .into_any_element()
                    });
                    let select = (*entry).clone();
                    list_rows.push(
                        div()
                            .id(row_id)
                            .group(row_group)
                            .min_h(px(30.))
                            .px(px(8.))
                            .py(px(3.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .when(highlighted, |s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .hover(|s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .child(visual)
                            .child(
                                div()
                                    .id(SharedString::from(format!("draft-project-title-{row_key}")))
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .when(selected, |s| s.font_weight(FontWeight::MEDIUM))
                                    .tooltip(move |window, cx| Tooltip::new(title_for_tooltip.clone()).build(window, cx))
                                    .child(entry.label.clone()),
                            )
                            .children(pin_el)
                            .children(badge_el)
                            .on_click(cx.listener(move |this, _, window, cx| this.select_entry(select.clone(), window, cx)))
                            .into_any_element(),
                    );
                }
            }
        }

        let body = if list_rows.is_empty() {
            div()
                .py(px(18.))
                .px(px(8.))
                .flex()
                .justify_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child("No matching projects.")
                .into_any_element()
        } else {
            div()
                .id("draft-project-list")
                .max_h(px(320.))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .p(px(4.))
                .flex()
                .flex_col()
                .gap(px(1.))
                .children(list_rows)
                .into_any_element()
        };

        let search_down = search.clone();
        let search_up = search.clone();
        let search_esc = search.clone();
        let search_key = search.clone();
        let popup = div()
            .id("draft-project-popup")
            .track_focus(&self.popup_focus)
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .capture_action(cx.listener(move |this, _: &MoveDown, window, cx| {
                if !crate::sidebar::is_composing(&search_down, window, cx) {
                    this.move_highlight(1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(move |this, _: &MoveUp, window, cx| {
                if !crate::sidebar::is_composing(&search_up, window, cx) {
                    this.move_highlight(-1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(move |this, _: &Escape, window, cx| {
                if !crate::sidebar::is_composing(&search_esc, window, cx) {
                    this.close_picker(Some(window), true, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &crate::keymap::PinThread, _, cx| {
                if this.projects_open && this.toggle_highlighted_pin(cx) {
                    cx.stop_propagation();
                }
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if crate::sidebar::is_composing(&search_key, window, cx) {
                    return;
                }
                let key = &event.keystroke;
                let search_focused = search_key.read(cx).focus_handle(cx).is_focused(window);
                if !search_focused && !key.modifiers.control && !key.modifiers.alt && !key.modifiers.platform && !key.modifiers.function {
                    match key.key.as_str() {
                        "down" => {
                            window.prevent_default();
                            cx.stop_propagation();
                            this.move_highlight(1, cx);
                        }
                        "up" => {
                            window.prevent_default();
                            cx.stop_propagation();
                            this.move_highlight(-1, cx);
                        }
                        "escape" => {
                            window.prevent_default();
                            cx.stop_propagation();
                            this.close_picker(Some(window), true, cx);
                        }
                        "enter" => {
                            window.prevent_default();
                            cx.stop_propagation();
                            this.activate_highlighted(window, cx);
                        }
                        _ => {}
                    }
                }
            }))
            .relative()
            .w(px(288.))
            .max_h(px(380.))
            .rounded(px(8.))
            .overflow_hidden()
            .flex()
            .flex_col()
            .text_sm()
            .text_color(hsla(p.popover_foreground))
            .shadow(crate::composer::controls::floating_shadow(cx.theme().is_dark()))
            .child(crate::glass::backdrop(8., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
            .child(
                div()
                    .id("draft-project-search")
                    .h(px(36.))
                    .px(px(10.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .child(Icon::Search.sized(px(15.)).text_color(hsla(p.muted_foreground).opacity(0.6)))
                    .child(div().flex_1().min_w_0().child(Input::new(&search).appearance(false).bordered(false).xsmall().px_0())),
            )
            .child(body);

        Some(
            deferred(
                div()
                    .id("draft-project-backdrop")
                    .absolute()
                    .inset_0()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.dismissed = Some(Instant::now());
                            this.close_picker(Some(window), false, cx);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _, window, cx| {
                            this.dismissed = Some(Instant::now());
                            this.close_picker(Some(window), false, cx);
                        }),
                    )
                    .child(anchored().position(self.popup_origin).snap_to_window_with_margin(px(8.)).child(popup)),
            )
            .with_priority(130)
            .into_any_element(),
        )
    }
}

impl Render for DraftHero {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let (entries, _) = self.entries(cx);
        let can_choose = !entries.is_empty();
        let app = self.state.read(cx);
        let project = app.draft_project.as_deref().and_then(|id| app.shell.project(id)).map(|project| (project.id.clone(), project.title.clone()));
        let scratch_draft = project.as_ref().is_some_and(|(id, _)| app.is_scratch_project(id));
        let has_resolved = project.is_some() && !scratch_draft;
        let scratch_available = app.scratch_root().is_some();
        let shortcut = crate::keymap::label("chat.newWithoutProject", cx);
        let viewport_width = f32::from(window.viewport_size().width);
        let large_heading = viewport_width >= HERO_SMALL_BREAKPOINT;
        let (heading_size, heading_lh) = if large_heading { (px(30.), px(36.)) } else { (px(24.), px(32.)) };
        let heading_width = self.heading_width.clone();
        let view_id = cx.entity_id();
        let triage_hero = scratch_draft && is_triage_prompt(&self.composer.read(cx).prompt_text(cx));

        if triage_hero {
            let hero_block = self.render_triage_hero(heading_size, heading_lh, cx);
            return v_flex()
                .size_full()
                .justify_center()
                .child(hero_block)
                .child(div().h(px(16.)))
                .child(self.composer.clone())
                .children(self.render_popup(cx));
        }

        let headline = match &project {
            Some((_, title)) if !scratch_draft => {
                let measured_container = self.heading_width.get();
                let inline_width = inline_heading_width(title, window, cx);
                let fits_inline = viewport_width >= HERO_LARGE_BREAKPOINT && measured_container > 0. && inline_width <= measured_container;
                let trigger = self.picker_trigger(title.clone().into(), Some(title.clone()), can_choose, cx);
                div()
                    .relative()
                    .w_full()
                    .text_color(hsla(p.foreground))
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                let next = f32::from(bounds.size.width);
                                if (heading_width.replace(next) - next).abs() > 0.5 {
                                    cx.notify(view_id);
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .child(if fits_inline {
                        h_flex()
                            .w_full()
                            .justify_center()
                            .items_baseline()
                            .gap(px(8.))
                            .text_size(px(30.))
                            .line_height(px(36.))
                            .child("What should we build in")
                            .child(h_flex().min_w_0().max_w_full().items_baseline().child(trigger).child("?"))
                            .into_any_element()
                    } else {
                        v_flex()
                            .w_full()
                            .items_center()
                            .gap(px(8.))
                            .child(div().text_size(heading_size).line_height(heading_lh).child("What should we build?"))
                            .child(div().max_w_full().text_size(px(16.)).line_height(px(24.)).child(trigger))
                            .into_any_element()
                    })
                    .into_any_element()
            }
            Some(_) => h_flex()
                .w_full()
                .justify_center()
                .text_size(heading_size)
                .line_height(heading_lh)
                .text_color(hsla(p.foreground))
                .child("What should we work on?")
                .into_any_element(),
            None => {
                let trigger = self.picker_trigger(
                    if can_choose { "Choose a project" } else { "Add a project" }.into(),
                    None,
                    can_choose,
                    cx,
                );
                h_flex()
                    .w_full()
                    .justify_center()
                    .flex_wrap()
                    .gap(px(8.))
                    .text_size(heading_size)
                    .line_height(heading_lh)
                    .text_color(hsla(p.foreground))
                    .child(trigger)
                    .child("to start")
                    .into_any_element()
            }
        };

        // The line under the headline: the scratch draft's picker, or the way to start without one.
        let below = if !scratch_available {
            None
        } else if scratch_draft {
            Some(self.picker_trigger("No project".into(), None, can_choose, cx))
        } else if has_resolved || can_choose {
            Some(
                div()
                    .id("draft-without-project")
                    .cursor_pointer()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.muted_foreground))
                    .hover(|this| this.underline().text_color(hsla(p.foreground)))
                    .child("or start without a project")
                    .when_some(shortcut, |this, shortcut| this.tooltip(move |window, cx| Tooltip::new(shortcut.clone()).build(window, cx)))
                    .on_click(cx.listener(|this, _, window, cx| this.start_without_project(window, cx)))
                    .into_any_element(),
            )
        } else {
            None
        };

        v_flex()
            .size_full()
            .justify_center()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(HEADING_MAX_WIDTH))
                    .mx_auto()
                    .px(px(48.))
                    .items_center()
                    .child(headline)
                    .children(below.map(|below| h_flex().mt(px(8.)).h(px(24.)).w_full().justify_center().text_sm().child(below))),
            )
            .child(div().h(px(16.)))
            .child(self.composer.clone())
            .children(self.render_popup(cx))
    }
}

impl AppState {
    pub(crate) fn start_branch_draft(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(owner) = crate::context_menu::ThreadMenuOwner::capture(self, thread_id) else { return };
        let Some(thread) = self.thread_shell(thread_id) else { return };
        let project_id = thread.project_id.clone();
        let Some(project) = self.shell.project(&project_id) else { return };
        let choice = t3_core::workspace::DraftWorkspace {
            destination: Some((self.server_config.config.as_ref().map(|config| config.environment.environment_id.clone()).unwrap_or_default(), project.created_at.clone())),
            branch: thread.branch.clone(),
            environment_id: self.environments.active.clone().unwrap_or_default(),
            project_root: project.workspace_root.clone(),
            mode: if thread.worktree_path.is_some() { t3_core::server_config::ThreadEnvMode::Worktree } else { t3_core::server_config::ThreadEnvMode::Local },
            previous: thread.worktree_path.as_ref().map(|path| t3_core::workspace::PreviousWorktreeSeed { worktree_path:path.clone(), branch:thread.branch.clone() }),
        };
        self.start_draft(&project_id, cx);
        self.branch_draft = Some((owner, choice));
        cx.emit(AppEvent::BranchDraft);
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::{WindowHandle, size};

    struct HeroHarness {
        hero: Entity<DraftHero>,
        composer: Entity<crate::composer::Composer>,
    }

    impl Render for HeroHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn open_hero_harness(cx: &mut gpui::TestAppContext) -> (Entity<AppState>, t3_client::supervisor::ClientFixture, tokio::runtime::Runtime, WindowHandle<HeroHarness>) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (state, fixture) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            installed
        });
        let window = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let hero = cx.new(|cx| DraftHero::new(state.clone(), composer.clone(), cx));
            HeroHarness { hero, composer }
        });
        (state, fixture, runtime, window)
    }

    #[gpui::test]
    fn draft_hero_searchable_picker_filters_pins_and_retargets_non_empty_draft(cx: &mut gpui::TestAppContext) {
        let (state, fixture, runtime, window) = open_hero_harness(cx);
        let (hero, composer) = window.read_with(cx, |harness, _| (harness.hero.clone(), harness.composer.clone())).unwrap();
        let _ = window.update(cx, |_, window, cx| {
            let projects = state.read(cx).shell.snapshot.as_ref().unwrap().projects.clone();
            assert!(projects.len() >= 2, "fixture provides multiple pickable projects");
            let first = projects[0].id.clone();
            state.update(cx, |state, cx| state.start_draft(&first, cx));
            // Verify picker opens, filters with query hiding Add project, and handles empty results.
            hero.update(cx, |hero, cx| {
                hero.open_picker(point(px(200.), px(200.)), window, cx);
                assert!(hero.projects_open);
                let (rows, _) = hero.visible_rows(cx);
                assert!(matches!(rows.last(), Some(PickerRow::AddProject)), "empty query includes Add project row");
                let search = hero.search.clone().expect("search initialized");
                search.update(cx, |input, cx| input.set_value("zzzzzz-missing", window, cx));
                let (missing_rows, _) = hero.visible_rows(cx);
                assert!(missing_rows.is_empty(), "non-matching query leaves zero rows and hides Add project");
                search.update(cx, |input, cx| input.set_value("", window, cx));
                hero.close_picker(Some(window), false, cx);
                assert!(!hero.projects_open);
            });
        });
        cx.run_until_parked();

        // Now test empty draft does not clobber destination, while non-empty draft retargets.
        let (entries, p1, p2) = window
            .update(cx, |_, _, cx| {
                let (entries, _) = hero.read(cx).entries(cx);
                assert!(entries.len() >= 2, "grouped entries contain at least two distinct project groups");
                let p1 = entries[0].project_id.clone();
                let p2 = entries[1].project_id.clone();
                state.update(cx, |state, cx| state.start_draft(&p2, cx));
                (entries, p1, p2)
            })
            .unwrap();
        cx.run_until_parked();
        let _ = window.update(cx, |_, window, cx| {
            composer.update(cx, |composer, cx| composer.set_test_prompt("Keep destination", window, cx));
            state.update(cx, |state, cx| state.start_draft(&p1, cx));
        });
        cx.run_until_parked();
        let _ = window.update(cx, |_, window, cx| {
            // p1 is empty -> selecting p2 via hero must preserve p2's draft
            let entry_p2 = entries[1].clone();
            hero.update(cx, |h, cx| h.select_entry(entry_p2, window, cx));
        });
        cx.run_until_parked();
        let _ = window.update(cx, |_, window, cx| {
            assert_eq!(state.read(cx).draft_project.as_deref(), Some(p2.as_str()));
            assert_eq!(composer.read(cx).test_prompt(cx), "Keep destination");

            // Now type in p2 and switch back to p1 -> non-empty draft retargets to p1
            composer.update(cx, |composer, cx| composer.set_test_prompt("Retargeted prompt", window, cx));
            let entry_p1 = entries[0].clone();
            hero.update(cx, |h, cx| h.select_entry(entry_p1, window, cx));
        });
        cx.run_until_parked();
        window.read_with(cx, |_, cx| {
            assert_eq!(state.read(cx).draft_project.as_deref(), Some(p1.as_str()));
            assert_eq!(composer.read(cx).test_prompt(cx), "Retargeted prompt");
            assert!(
                composer.read(cx).sidebar_drafts().get(&t3_core::composer::drafts::new_thread_key(&p2)).is_none(),
                "source draft is removed after retargeting"
            );
        }).unwrap();

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn draft_hero_heading_and_empty_projects_follow_width_and_pickability(cx: &mut gpui::TestAppContext) {
        let (state, fixture, runtime, window) = open_hero_harness(cx);
        let hero = window.read_with(cx, |harness, _| harness.hero.clone()).unwrap();
        let _ = window.update(cx, |_, window, cx| {
            let short = inline_heading_width("Alpha", window, cx);
            let long = inline_heading_width(&"SuperLongProjectName".repeat(6), window, cx);
            assert!(short < 600., "short project fits inline inside desktop container: {short}");
            assert!(long > 1024., "very long project overflows max-w-5xl container: {long}");

            // Clear all projects -> entries() is empty (canChooseProject == false)
            state.update(cx, |state, cx| {
                state.fleet = std::sync::Arc::new(Default::default());
                state.sidebar.grouping_projects.clear();
                if let Some(snapshot) = state.shell.snapshot.as_mut() {
                    snapshot.projects.clear();
                }
                state.clear_selection(cx);
            });
            let (entries, _) = hero.read(cx).entries(cx);
            assert!(entries.is_empty());
            let (rows, _) = hero.read(cx).visible_rows(cx);
            assert_eq!(rows.len(), 1);
            assert!(matches!(rows[0], PickerRow::AddProject));

            // Triage prompt detection + copy state.
            assert!(is_triage_prompt("See [triage-playbook.md](file:///tmp/triage-playbook.md)"));
            assert!(!is_triage_prompt("Regular prompt"));
            hero.update(cx, |hero, cx| {
                hero.copy_triage_command(cx);
                assert!(hero.triage_copied_until.is_some());
            });
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some(TRIAGE_CLI_COMMAND.to_owned())
            );
        });

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }
}
