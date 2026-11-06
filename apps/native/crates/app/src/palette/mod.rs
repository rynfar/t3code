//! The command palette (`apps/web/src/components/CommandPalette.tsx`, design-system §5 and
//! §6.8): 576 wide, radius 16, 10vh from the top. Command mode lists actions, recent threads,
//! and on a query, threads and projects ranked by `t3_core::palette`; go-to-file and content
//! search are modes of the same overlay (`overlayModeForCommand`), and Escape goes back to it.

mod content;
mod files;
mod projects;

use std::collections::HashMap;

use gpui::{
    App, BoxShadow, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, MouseButton, ParentElement, Render, ScrollHandle, SharedString, Styled,
    Subscription, Window, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use t3_core::palette::{
    CommandPaletteGroup, CommandPaletteItem, CommandPaletteItemKind, CommandPaletteProject, CommandPaletteRow, CommandPaletteSearch, CommandPaletteSearchState, CommandPaletteSearchAction, CommandPaletteView, PaletteThread, build_command_palette_rows,
    create_command_palette_search_state, reduce_command_palette_search_state,
    build_project_action_items, build_root_groups, build_thread_action_items, filter_command_palette_groups, recent_thread_items,
};
use t3_core::sidebar::time::{compact_relative_label, format_iso, parse_iso};

use crate::app_state::AppState;
use crate::keymap::{
    self, CopyThreadReference, JumpToThread1, JumpToThread2, JumpToThread3, JumpToThread4, JumpToThread5, JumpToThread6, JumpToThread7, JumpToThread8,
    JumpToThread9, PinThread,
};
use crate::sidebar::is_composing;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

const ENVIRONMENT: &str = t3_core::sidebar::ENVIRONMENT_ID;

gpui::actions!(palette, [OpenAddProject]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Command,
    Files,
    Content,
    /// Add project: browse the host's folders.
    Browse,
    NewProject,
}

impl Mode {
    fn placeholder(self, project: Option<&str>) -> String {
        match (self, project) {
            (Mode::Command, _) => "Search commands, projects, and threads...".to_owned(),
            (Mode::Files, _) => "Search files…".to_owned(),
            (Mode::Content, Some(project)) => format!("Search in {project}"),
            (Mode::Content, None) => "Search project contents…".to_owned(),
            (Mode::Browse, _) => "Enter project path (e.g. ~/projects/my-app)".to_owned(),
            (Mode::NewProject, _) => "Project name".to_owned(),
        }
    }
}

pub struct CommandPalette {
    state: Entity<AppState>,
    composer: Entity<crate::composer::Composer>,
    input: Entity<InputState>,
    open: bool,
    mode: Mode,
    files: files::FileSearch,
    content: content::ContentSearch,
    browse: projects::ProjectBrowse,
    search: CommandPaletteSearchState,
    root: Vec<CommandPaletteGroup>,
    threads: Vec<CommandPaletteItem>,
    projects: Vec<CommandPaletteItem>,
    scoped_project_actions: HashMap<String, (t3_core::sidebar::grouping::ScopedProjectGroup, t3_core::sidebar::grouping::ProjectRef, bool)>,
    scoped_thread_actions: HashMap<String, t3_core::sidebar::tag_filter::ThreadKey>,
    settings: Vec<CommandPaletteItem>,
    groups: Vec<CommandPaletteGroup>,
    highlighted: Option<String>,
    focus_return: Option<FocusHandle>,
    scroll: ScrollHandle,
    _subscription: Subscription,
}

impl Focusable for CommandPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

/// The palette's own commands: what this client can run today, with web titles.
fn action_items(state: &AppState) -> Vec<CommandPaletteItem> {
    let action = |value: &str, title: &str, terms: &[&str], command: Option<&str>| CommandPaletteItem {
        value: value.to_owned(),
        title: title.to_owned(),
        search_terms: std::iter::once(title).chain(terms.iter().copied()).map(str::to_owned).collect(),
        shortcut_command: command.map(str::to_owned),
        ..Default::default()
    };
    let mut items = vec![
        action("action:add-project", "Add project", &["folder", "open", "import"], None),
        action("action:new-project", "New project", &["create", "folder"], None),
        action("action:toggle-sidebar", "Toggle sidebar", &["hide", "show", "panel"], Some("sidebar.toggle")),
    ];
    if let Some(thread) = state.selected_thread.as_deref().and_then(|id| state.sidebar.model.thread(id)) {
        let pin = if thread.pinned { "Unpin thread" } else { "Pin thread" };
        let settle = if thread.is_settled() { "Un-settle thread" } else { "Settle thread" };
        items.push(action("action:pin", pin, &["pin"], Some("thread.pin")));
        items.push(action("action:settle", settle, &["archive", "done"], Some("thread.settle")));
        items.push(action("action:copy-thread", "Copy thread reference", &["link", "copy"], Some("thread.copyReference")));
        items.push(action("action:link-pull-request", "Link pull request to thread", &["link", "pull request", "pr", "attach", "stack"], None));
        let linked = state.thread_shell(&thread.id).is_some_and(|shell| !t3_core::pull_requests::link::visible_links(&shell.pull_requests).is_empty());
        items.push(CommandPaletteItem { disabled: !linked, ..action("action:open-thread-pull-requests", "Show linked pull requests", &["pull requests", "linked", "stack", "prs"], None) });
    }
    if state.server_config.config.is_some() {
        items.push(action("action:pull-requests", "Open pull requests", &["pull requests", "prs", "pr", "github", "review", "merge", "branch"], None));
    }
    if let Some(project_id) = crate::new_thread::default_project(state) {
        let name = state.sidebar.model.project_names.get(&project_id).cloned().or_else(|| state.shell.project(&project_id).map(|project| project.title.clone()));
        let terms = ["project", "settings", "name", "icon", "scripts", "model", "workspace", "grouping", "checkout", "remove", "t3.json"];
        items.push(CommandPaletteItem { description: name, ..action("action:project-settings", "Project settings", &terms, None) });
    }
    items.push(action("action:previous-thread", "Previous thread", &["navigate"], Some("thread.previous")));
    items.push(action("action:next-thread", "Next thread", &["navigate"], Some("thread.next")));
    items.push(action("action:theme-editor", "Toggle theme editor", &["theme", "appearance", "colors", "palette", "customize"], Some("themeEditor.toggle")));
    items.push(action("action:settings", "Open settings", &["settings", "preferences", "configuration", "keybindings"], None));
    items.push(action("action:usage", "Open usage", &["usage", "use", "tokens", "cost", "spend", "limits", "stats", "analytics"], Some("usage.open")));
    items.push(action("action:switch-environment", "Switch environment...", &["environment", "server", "connect", "pair", "machine", "disconnect"], None));
    items
}

fn scoped_project_items(entries: &[crate::new_thread::ProjectPickerEntry]) -> (
    Vec<CommandPaletteItem>,
    HashMap<String, (t3_core::sidebar::grouping::ScopedProjectGroup, t3_core::sidebar::grouping::ProjectRef, bool)>,
) {
    let mut items = Vec::new();
    let mut actions = HashMap::new();
    for entry in entries {
        let group = &entry.group;
        let target = &entry.target;
        let target_ref = target.project.reference();
        let description = format!("{} · {}", target.project.environment_label, target.project.project.workspace_root);
        for (prefix, title, draft) in [("scoped-project", group.label.clone(), false), ("scoped-new-thread", format!("New thread in {}", group.label), true)] {
            let value = format!("{prefix}:{}", group.key);
            let mut search_terms = vec![group.label.clone(), target.project.project.workspace_root.clone(), target.project.environment_label.clone()];
            for member in &group.members {
                search_terms.extend([member.project.project.title.clone(), member.project.project.workspace_root.clone()]);
                if !draft { search_terms.push(member.project.environment_label.clone()); }
            }
            items.push(CommandPaletteItem { value:value.clone(),title,description:Some(description.clone()),search_terms,..Default::default() });
            actions.insert(value, (group.clone(),target_ref.clone(),draft));
        }
    }
    (items, actions)
}

fn scoped_project_thread_items(
    items: &[CommandPaletteItem],
    actions: &HashMap<String, (t3_core::sidebar::grouping::ScopedProjectGroup, t3_core::sidebar::grouping::ProjectRef, bool)>,
    preferred: Option<&t3_core::sidebar::grouping::ProjectRef>,
    primary: Option<&str>,
    actual_context: Option<&str>,
    eligible: &std::collections::HashSet<String>,
) -> Vec<CommandPaletteItem> {
    let drafts = items.iter().filter_map(|item| {
        let (group, reference, draft) = actions.get(&item.value)?;
        let target = group.registrations.iter().find(|project| project.reference() == *reference)?;
        (*draft).then_some((item, group, target))
    }).collect::<Vec<_>>();
    let available = || drafts.iter().filter(|(_, _, target)| target.is_scratch && eligible.contains(&target.owner.saved_id));
    let scratch = available().find(|(_, _, target)| primary == Some(target.owner.saved_id.as_str()))
        .or_else(|| available().find(|(_, _, target)| actual_context == Some(target.owner.saved_id.as_str())))
        .or_else(|| {
            let mut offering = available();
            let first = offering.next()?;
            offering.next().is_none().then_some(first)
        });
    let mut projects = drafts.iter().filter(|(_, _, target)| !target.is_scratch)
        .map(|(item, group, _)| CommandPaletteItem { title:group.label.clone(), ..(*item).clone() }).collect::<Vec<_>>();
    if let Some((item, _, _)) = scratch {
        let first = drafts.iter().find(|(_, _, target)| target.is_scratch).unwrap();
        let preferred_scratch = preferred.is_some_and(|reference| first.1.member_refs.contains(reference));
        let index = if first.1.pinned || preferred_scratch {
            drafts.iter().take_while(|(candidate, _, _)| candidate.value != first.0.value).filter(|(_, _, target)| !target.is_scratch).count()
        } else {
            (drafts.iter().filter(|(_, group, target)| !target.is_scratch && group.pinned).count() + 1).min(projects.len())
        };
        projects.insert(index, CommandPaletteItem { title:"No project".into(), search_terms:vec!["no project".into(),"without project".into(),"none".into()], ..(*item).clone() });
    }
    projects
}

fn palette_threads(
    model: &t3_core::sidebar::SidebarModel,
    history: &t3_core::sidebar::visits::OpenHistory,
    environment: Option<&t3_core::sidebar::visits::OpenEnvironment>,
) -> Vec<PaletteThread> {
    model
        .threads
        .iter()
        .map(|thread| PaletteThread {
            environment_id: ENVIRONMENT.to_owned(),
            id: thread.id.clone(),
            project_id: thread.project_id.clone(),
            title: thread.title.clone(),
            branch: thread.branch.clone(),
            created_at: thread.created_at.map(format_iso).unwrap_or_default(),
            updated_at: thread.updated_at.map(format_iso).unwrap_or_default(),
            latest_user_message_at: thread.latest_user_message_at.map(format_iso),
            latest_user_authored_message_at: thread
                .latest_user_authored_message_at
                .map(|at| at.map(format_iso)),
            opens: thread.opens.clone(),
            local_opened_at: environment
                .and_then(|environment| history.local_opened_at(environment, &thread.id)),
            ..Default::default()
        })
        .collect()
}

impl CommandPalette {
    pub fn new(state: Entity<AppState>, composer: Entity<crate::composer::Composer>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search commands, projects, and threads..."));
        let subscription = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                let query = input.read(cx).value().to_string();
                match this.mode {
                    Mode::Command => {
                        this.search = reduce_command_palette_search_state(std::mem::take(&mut this.search), CommandPaletteSearchAction::Query(query));
                        this.refilter(cx);
                    }
                    Mode::Files => this.files.search(query, &this.state.clone(), cx),
                    Mode::Content => this.content.search(query, &this.state.clone(), cx),
                    Mode::Browse => this.browse.set_query(query, &this.state.clone(), cx),
                    Mode::NewProject => this.search = reduce_command_palette_search_state(std::mem::take(&mut this.search), CommandPaletteSearchAction::Query(query)),
                }
                cx.notify();
            }
            InputEvent::PressEnter { secondary, .. } => match this.mode {
                Mode::Command => this.activate_highlighted(window, cx),
                Mode::Files => this.open_file(*secondary, window, cx),
                Mode::Content => this.open_match(*secondary, window, cx),
                Mode::Browse => this.browse_pick(*secondary, window, cx),
                Mode::NewProject => {
                    let name = this.search.query.clone();
                    this.close(window, cx);
                    this.state.update(cx, |state, cx| state.create_new_project(&name, cx));
                }
            },
            _ => {}
        });
        CommandPalette {
            state,
            composer,
            input,
            open: false,
            mode: Mode::Command,
            files: Default::default(),
            content: Default::default(),
            browse: Default::default(),
            search: create_command_palette_search_state(""),
            root: Vec::new(),
            threads: Vec::new(),
            projects: Vec::new(),
            scoped_project_actions: HashMap::new(),
            scoped_thread_actions: HashMap::new(),
            settings: crate::settings::palette_items(cx),
            groups: Vec::new(),
            highlighted: None,
            focus_return: None,
            scroll: ScrollHandle::new(),
            _subscription: subscription,
        }
    }

    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_mode(Mode::Command, window, cx);
    }

    /// `ToggleMode`: the same mode again closes the palette; another one switches to it.
    pub fn toggle_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.mode == mode {
            return self.close(window, cx);
        }
        if !self.open {
            self.focus_return = window.focused(cx);
        }
        self.open = true;
        self.enter_mode(mode, window, cx);
    }

    fn enter_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        let target = self.state.read(cx).active_project_target();
        let placeholder = mode.placeholder(target.as_ref().map(|target| target.project_name.as_str()));
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder(placeholder, window, cx);
            input.focus(window, cx);
        });
        match mode {
            Mode::Command => {
                self.rebuild(cx);
                self.search = create_command_palette_search_state("");
                self.refilter(cx);
            }
            Mode::Files => self.files.start(target, &self.state.clone(), cx),
            Mode::Content => self.content.start(target),
            Mode::Browse => {
                self.browse.reset();
                self.input.update(cx, |input, cx| input.set_value("~/", window, cx));
                self.browse.set_query("~/".to_owned(), &self.state.clone(), cx);
            }
            Mode::NewProject => self.search = create_command_palette_search_state(""),
        }
        cx.notify();
    }

    fn browse_pick(&mut self, add: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.browse.pick(add) {
            Some(projects::BrowsePick::Add(path)) => {
                self.close(window, cx);
                self.state.update(cx, |state, cx| state.add_project(&path, cx));
            }
            Some(projects::BrowsePick::Enter(query)) => self.set_browse_query(query, window, cx),
            None => {}
        }
    }

    fn set_browse_query(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_value(query.clone(), window, cx));
        self.browse.set_query(query, &self.state.clone(), cx);
        cx.notify();
    }

    /// Escape: a file or content search goes back to the command list; the list closes.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Command if !self.search.views.is_empty() => {
                self.search = reduce_command_palette_search_state(std::mem::take(&mut self.search), CommandPaletteSearchAction::Back);
                self.input.update(cx, |input, cx| input.set_value(self.search.query.clone(), window, cx));
                self.refilter(cx);
            }
            Mode::Command => self.close(window, cx),
            _ => self.enter_mode(Mode::Command, window, cx),
        }
    }

    /// Enter opens the file in the right panel (`openFile`); without an open thread, in the
    /// preferred editor (or mentions it when the server has none). Cmd+Enter mentions it.
    fn open_target(&mut self, path: String, line: Option<u64>, mention: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.state.read(cx).active_project_target().map(|target| target.cwd) else { return };
        self.close(window, cx);
        self.state.update(cx, |state, cx| {
            if !mention && state.file_workspace().is_some() {
                state.open_file(&path, line, cx);
            } else if mention || !state.open_in_editor(&cwd, &path, line, cx) {
                state.request_composer_mention(crate::project_files::ComposerMention { path, line }, cx);
            }
        });
    }

    fn open_file(&mut self, mention: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.files.highlighted_path() {
            self.open_target(path, None, mention, window, cx);
        }
    }

    fn open_match(&mut self, mention: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, line)) = self.content.selected_match() {
            self.open_target(path, Some(line), mention, window, cx);
        }
    }

    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Command => self.move_highlight(step, cx),
            Mode::Files => self.files.move_highlight(step),
            Mode::Content => self.content.move_selection(step),
            Mode::Browse => self.browse.move_highlight(step),
            Mode::NewProject => {}
        }
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        if let Some(focus) = self.focus_return.take() {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let environment = self.state.update(cx, |state, _| state.visit_environment());
        let state = self.state.read(cx);
        let model = state.sidebar.model.clone();
        let titles: HashMap<String, String> = model.project_names.iter().map(|(id, name)| (format!("{ENVIRONMENT}:{id}"), name.clone())).collect();
        let threads = palette_threads(&model, &state.open_history, environment.as_ref());
        let active = state.selected_thread.as_deref().map(|id| (ENVIRONMENT, id));
        let items = build_thread_action_items(&threads, active, &titles, |_| None);
        let active_value = active.map(|(env, id)| format!("thread:{env}:{id}"));
        let recent = recent_thread_items(&items, active_value.as_deref());
        let projects: Vec<CommandPaletteProject> = state
            .shell
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .projects
                    .iter()
                    .filter(|project| project.archived_at.is_none())
                    .map(|project| CommandPaletteProject {
                        environment_id: ENVIRONMENT.to_owned(),
                        id: project.id.clone(),
                        title: project.title.clone(),
                        display_name: model.project_names.get(&project.id).cloned().unwrap_or_else(|| project.title.clone()),
                        workspace_root: project.workspace_root.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.projects = build_project_action_items(&projects, "project", |_| Vec::new(), None);
        self.scoped_project_actions.clear();
        self.scoped_thread_actions.clear();
        let preferred = crate::new_thread::default_project(state).and_then(|project_id| Some(t3_core::sidebar::grouping::ProjectRef { environment_id:state.environments.active.clone()?,project_id }));
        if !state.fleet.environments.is_empty() {
            let groups = state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Palette, cx);
            self.projects.clear();
            let entries = crate::new_thread::picker_entries(groups, preferred.as_ref());
            (self.projects, self.scoped_project_actions) = scoped_project_items(&entries);
        }
        let primary = state.local_saved_environment().map(|environment| environment.id.as_str()).or_else(|| {
            let mut local = state.environments.environments.iter().filter(|environment| environment.local);
            let first = local.next()?;
            local.next().is_none().then_some(first.id.as_str())
        });
        let actual_context = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id)).map(|_| ())
            .or_else(|| state.draft_project.as_deref().and_then(|id| state.shell.project(id)).map(|_| ()))
            .and(state.environments.active.as_deref());
        let eligible = state.fleet.environments.iter().filter(|entry| state.scoped_ready(&entry.summary.owner, t3_core::auth::ORCHESTRATION_READ_SCOPE))
            .map(|entry| entry.summary.owner.saved_id.clone()).collect();
        let project_thread_items = if state.fleet.environments.is_empty() {
            build_project_action_items(&projects, "new-thread-in", |_| Vec::new(), None)
        } else {
            scoped_project_thread_items(&self.projects, &self.scoped_project_actions, preferred.as_ref(), primary, actual_context, &eligible)
        };
        let mut actions = action_items(state);
        if !project_thread_items.is_empty() {
            actions.push(CommandPaletteItem {
                kind: CommandPaletteItemKind::Submenu(CommandPaletteView { groups: vec![CommandPaletteGroup { value:"projects".into(), label:"Projects".into(), items:project_thread_items }], initial_query:None }),
                value:"action:new-thread-in".into(), title:"New thread in...".into(), search_terms:vec!["new thread project pick choose select".into()], ..Default::default()
            });
        }
        for (context, value, title) in [(false,"action:copy-prompt-link","Copy prompt link"),(true,"action:copy-prompt-link-context","Copy prompt link with repo and model")] {
            if self.composer.read(cx).prompt_link(context,cx).is_some() {
                actions.push(CommandPaletteItem { value: value.into(), title: title.into(), search_terms: vec!["copy prompt link share repo model".into()], ..Default::default() });
            }
        }
        self.root = build_root_groups(actions, recent);
        self.threads = items;
        if !state.fleet.environments.is_empty() {
            self.threads.clear();
            for row in &state.sidebar.fleet.threads {
                let value = format!("scoped-thread:{}", self.scoped_thread_actions.len());
                self.scoped_thread_actions.insert(value.clone(), row.key.clone());
                self.threads.push(CommandPaletteItem { value, title:row.thread.title.clone(),
                    description:state.sidebar.fleet.project_names.get(&row.project).cloned(),
                    search_terms:vec![row.thread.title.clone(),row.thread.branch.clone().unwrap_or_default(),state.scoped_environment(&row.owner.saved_id).and_then(|entry| entry.config.config.as_ref()).map(|config| config.environment.label.clone()).unwrap_or_default()],
                    ..Default::default() });
            }
        }
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.groups = filter_command_palette_groups(&CommandPaletteSearch {
            active_groups: self.search.views.last().map_or(&self.root, |view| &view.view.groups),
            query: &self.search.query,
            is_in_submenu: !self.search.views.is_empty(),
            all_options: None,
            project_search_items: &self.projects,
            settings_search_items: &self.settings,
            thread_search_items: &self.threads,
            now: now_ms(),
        });
        let rows = build_command_palette_rows(&self.groups);
        self.highlighted = rows.item_values.first().map(|value| (*value).to_owned());
        cx.notify();
    }

    fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let rows = build_command_palette_rows(&self.groups);
        let count = rows.item_values.len() as isize;
        if count == 0 {
            return;
        }
        let current = self.highlighted.as_deref().and_then(|value| rows.item_values.iter().position(|v| *v == value)).map_or(-1, |p| p as isize);
        let next = if current < 0 { if step > 0 { 0 } else { count - 1 } } else { (current + step).rem_euclid(count) };
        self.highlighted = rows.item_values.get(next as usize).map(|value| (*value).to_owned());
        if let Some(row) = rows.row_index_by_item_index.get(next as usize) {
            self.scroll.scroll_to_item(*row);
        }
        cx.notify();
    }

    /// A jump key runs the row that carries its shortcut; otherwise the palette swallows it.
    fn jump(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(command) = t3_core::keybindings::thread_jump_command_for_index(index) else { return };
        let value = self.groups.iter().flat_map(|group| &group.items).find(|item| item.shortcut_command.as_deref() == Some(command)).map(|item| item.value.clone());
        if let Some(value) = value {
            self.activate(&value, window, cx);
        }
    }

    /// The pin key toggles the highlighted thread row's pin; anything else pins the open thread.
    fn pin_highlighted(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.highlighted.as_deref().and_then(|value| value.strip_prefix("thread:")).map(|rest| rest.split_once(':').map_or(rest, |(_, id)| id).to_owned()) else {
            return cx.propagate();
        };
        self.state.update(cx, |state, cx| match state.sidebar.model.thread(&thread_id).map(|thread| thread.pinned) {
            Some(true) => state.request_unpin_thread(&thread_id, cx),
            Some(false) => state.pin_thread(&thread_id, None, cx),
            None => {}
        });
    }

    fn activate_highlighted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(value) = self.highlighted.clone() {
            self.activate(&value, window, cx);
        }
    }

    fn activate(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.groups.iter().flat_map(|group| &group.items).find_map(|item| {
            if item.value != value { return None; }
            match &item.kind { CommandPaletteItemKind::Submenu(view) => Some(view.clone()), CommandPaletteItemKind::Action => None }
        }) {
            self.search = reduce_command_palette_search_state(std::mem::take(&mut self.search), CommandPaletteSearchAction::Push(view));
            self.input.update(cx, |input, cx| input.set_value(self.search.query.clone(), window, cx));
            self.refilter(cx);
            return;
        }
        match value {
            "action:add-project" => return self.enter_mode(Mode::Browse, window, cx),
            "action:new-project" => return self.enter_mode(Mode::NewProject, window, cx),
            _ => {}
        }
        let project_action = self.scoped_project_actions.get(value).cloned();
        let thread_action = self.scoped_thread_actions.get(value).cloned();
        self.close(window, cx);
        if let Some((group, target, draft)) = project_action {
            self.state.update(cx, |state, cx| {
                if !draft {
                    let threads: Vec<_> = state.fleet.environments.iter().flat_map(|entry| entry.summary.threads.iter().cloned()).collect();
                    if let Some(key) = t3_core::sidebar::grouping::latest_group_thread(&group, &threads, crate::settings::store::client(cx).thread_sort_order()) {
                        if let Some(row) = state.scoped_thread(&key) { state.navigate_scoped(row.owner.clone(), crate::app_state::ScopedRouteAction::Thread(key.thread_id), cx); }
                        return;
                    }
                }
                if let Some(project) = state.scoped_project(&target) { state.navigate_scoped(project.owner.clone(), crate::app_state::ScopedRouteAction::Draft(target.project_id), cx); }
            });
            return;
        }
        if let Some(key) = thread_action {
            self.state.update(cx, |state, cx| { if let Some(row) = state.scoped_thread(&key) { state.navigate_scoped(row.owner.clone(), crate::app_state::ScopedRouteAction::Thread(key.thread_id), cx); } });
            return;
        }
        if let Some(command) = value.strip_prefix("action:") {
            let action: Box<dyn gpui::Action> = match command {
                "copy-prompt-link" => Box::new(crate::composer::prompt_links::CopyPromptLink),
                "copy-prompt-link-context" => Box::new(crate::composer::prompt_links::CopyPromptLinkWithContext),
                "toggle-sidebar" => keymap::action_for("sidebar.toggle"),
                "pin" => keymap::action_for("thread.pin"),
                "settle" => keymap::action_for("thread.settle"),
                "copy-thread" => keymap::action_for("thread.copyReference"),
                "previous-thread" => keymap::action_for("thread.previous"),
                "next-thread" => keymap::action_for("thread.next"),
                "link-pull-request" => Box::new(crate::pull_requests::LinkPullRequest),
                "open-thread-pull-requests" => Box::new(crate::pull_requests::ShowLinkedPullRequests),
                "pull-requests" => Box::new(crate::pull_requests::OpenPullRequests),
                "switch-environment" => Box::new(crate::menus::SwitchEnvironment),
                "usage" => keymap::action_for("usage.open"),
                "theme-editor" => keymap::action_for("themeEditor.toggle"),
                "settings" => {
                    crate::settings::open(t3_core::settings::search::Section::General, cx);
                    return;
                }
                "project-settings" => {
                    if let Some(project_id) = crate::new_thread::default_project(self.state.read(cx)) {
                        crate::settings::open_project(&project_id, cx);
                    }
                    return;
                }
                _ => return,
            };
            window.dispatch_action(action, cx);
        } else if let Some(id) = value.strip_prefix("setting:") {
            crate::settings::open_setting(id, cx);
        } else if let Some(rest) = value.strip_prefix("thread:") {
            let thread_id = rest.split_once(':').map_or(rest, |(_, id)| id).to_owned();
            self.state.update(cx, |state, cx| state.select_thread(&thread_id, cx));
        } else if let Some(rest) = value.strip_prefix("new-thread-in:") {
            let project_id = rest.split_once(':').map_or(rest, |(_, id)| id).to_owned();
            self.state.update(cx, |state, cx| state.start_draft(&project_id, cx));
        } else if let Some(rest) = value.strip_prefix("project:") {
            let project_id = rest.split_once(':').map_or(rest, |(_, id)| id).to_owned();
            // No new-thread flow yet: a project opens its latest thread.
            let latest = self.state.read(cx).sidebar.model.threads.iter().filter(|t| t.project_id == project_id).max_by_key(|t| t.latest_user_message_at.or(t.updated_at)).map(|t| t.id.clone());
            if let Some(thread_id) = latest {
                self.state.update(cx, |state, cx| state.select_thread(&thread_id, cx));
            }
        }
    }

    fn render_row(&self, row: &CommandPaletteRow, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        match row {
            CommandPaletteRow::Label { label, first, .. } => div()
                .px(px(8.))
                .pt(px(if *first { 4. } else { 12. }))
                .pb(px(4.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.muted_foreground))
                .child(label.to_string())
                .into_any_element(),
            CommandPaletteRow::Item { item, .. } => {
                let highlighted = self.highlighted.as_deref() == Some(item.value.as_str());
                let value = item.value.clone();
                let hover_value = item.value.clone();
                let trailing = item
                    .shortcut_command
                    .as_deref()
                    .and_then(|command| keymap::label(command, cx))
                    .or_else(|| item.timestamp.as_deref().and_then(parse_iso).map(|at| compact_relative_label(at, now_ms())));
                div()
                    .id(SharedString::from(format!("palette-{}", item.value)))
                    .min_h(px(28.))
                    .px(px(8.))
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(4.))
                    .text_sm()
                    .cursor_pointer()
                    .when(highlighted, |this| this.bg(hsla(p.foreground).opacity(0.09)))
                    .child(div().flex_none().max_w(px(320.)).truncate().text_color(hsla(p.foreground)).child(item.title.clone()))
                    .children(item.description.clone().map(|text| div().flex_1().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(text)))
                    .child(div().flex_1())
                    .children(trailing.map(|text| div().flex_none().text_xs().text_color(hsla(p.muted_foreground)).child(text)))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.highlighted.as_deref() != Some(hover_value.as_str()) {
                            this.highlighted = Some(hover_value.clone());
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(&value, window, cx)))
                    .into_any_element()
            }
        }
    }
}

impl CommandPalette {
    /// Match case, whole word and regex: a segmented group in the input row.
    fn render_content_options(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let options = self.content.options;
        let toggle = |id: &'static str, label: &'static str, on: bool, underline: bool, pick: fn(&mut t3_core::project_search::ContentOptions)| {
            div()
                .id(id)
                .h(px(24.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .font_family("SF Mono")
                .text_xs()
                .cursor_pointer()
                .text_color(hsla(if on { p.foreground } else { p.muted_foreground }))
                .when(on, |this| this.bg(hsla(p.input).opacity(0.72)))
                .when(underline, |this| this.underline())
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.content.toggle_option(pick, &this.state.clone(), cx)))
        };
        div()
            .flex()
            .flex_none()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.muted).opacity(0.3))
            .child(toggle("match-case", "Aa", options.case_sensitive, false, |o| o.case_sensitive = !o.case_sensitive))
            .child(toggle("whole-word", "ab", options.whole_word, true, |o| o.whole_word = !o.whole_word))
            .child(toggle("use-regex", ".*", options.use_regex, false, |o| o.use_regex = !o.use_regex))
    }

    /// The footer hints (`CommandFooter`): navigate, the Enter action, and Escape.
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let enter = match self.mode {
            Mode::Command => "Open".to_owned(),
            Mode::Browse => "Enter folder / Add".to_owned(),
            Mode::NewProject => "Create project".to_owned(),
            _ if self.state.read(cx).file_workspace().is_some() => "Open file".to_owned(),
            _ => self.state.read(cx).preferred_editor_label().map_or_else(|| "Mention".to_owned(), |editor| format!("Open in {editor}")),
        };
        let hint = |key: &'static str, label: String| div().flex().items_center().gap(px(6.)).child(div().px(px(4.)).h(px(20.)).flex().items_center().rounded(px(4.)).bg(hsla(p.foreground).opacity(0.07)).child(key)).child(label);
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .py(px(10.))
            .border_t_1()
            .border_color(hsla(p.border))
            .text_xs()
            .text_color(hsla(p.muted_foreground))
            .child(hint("↑↓", "Navigate".to_owned()))
            .child(hint("↵", enter))
            .when(matches!(self.mode, Mode::Files | Mode::Content), |this| this.child(hint("⌘↵", "Mention in composer".to_owned())))
            .when(self.mode == Mode::Browse, |this| this.child(hint("⌘↵", "Add folder".to_owned())).child(hint("Tab", "Complete".to_owned())))
            .child(div().flex_1())
            .child(hint("Esc", if self.mode == Mode::Command && self.search.views.is_empty() { "Close".to_owned() } else { "Back".to_owned() }))
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let p = palette(cx);
        let groups = std::mem::take(&mut self.groups);
        let rows = build_command_palette_rows(&groups);
        let rendered: Vec<_> = rows.rows.iter().map(|row| self.render_row(row, cx)).collect();
        let empty = rows.item_values.is_empty();
        drop(rows);
        self.groups = groups;
        let top = window.viewport_size().height * 0.1;
        deferred(
            div()
                .id("palette-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .justify_center()
                .pt(top)
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close(window, cx)))
                .child(
                    div()
                        .id("command-palette")
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                            if !is_composing(&this.input, window, cx) {
                                this.move_selection(1, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                            if !is_composing(&this.input, window, cx) {
                                this.move_selection(-1, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .capture_action(cx.listener(|this, _: &gpui_component::input::IndentInline, window, cx| {
                            if this.mode == Mode::Browse && !is_composing(&this.input, window, cx) {
                                if let Some(query) = this.browse.complete() {
                                    this.set_browse_query(query, window, cx);
                                }
                                cx.stop_propagation();
                            }
                        }))
                        .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                            if !is_composing(&this.input, window, cx) {
                                this.escape(window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        // Inside the palette, jump keys and the pin key act on its rows (`CommandPalette.tsx`).
                        .on_action(cx.listener(|this, _: &JumpToThread1, window, cx| this.jump(0, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread2, window, cx| this.jump(1, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread3, window, cx| this.jump(2, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread4, window, cx| this.jump(3, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread5, window, cx| this.jump(4, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread6, window, cx| this.jump(5, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread7, window, cx| this.jump(6, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread8, window, cx| this.jump(7, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread9, window, cx| this.jump(8, window, cx)))
                        .on_action(cx.listener(|this, _: &PinThread, _, cx| this.pin_highlighted(cx)))
                        .on_action(cx.listener(|this, _: &CopyThreadReference, window, cx| {
                            this.close(window, cx);
                            cx.propagate();
                        }))
                        .w(px(576.))
                        .map(|this| match self.mode {
                            Mode::Command => this.max_h(px(420.)),
                            Mode::Files => this.max_h((window.viewport_size().height * 0.76).min(px(544.))),
                            Mode::Content => this.h((window.viewport_size().height * 0.76).min(px(560.))),
                            Mode::Browse | Mode::NewProject => this.max_h(px(420.)),
                        })
                        .flex()
                        .flex_col()
                        .relative()
                        .rounded(px(16.))
                        .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                        .shadow(vec![BoxShadow { color: gpui_hsla(0., 0., 0., 0.65), offset: point(px(0.), px(24.)), blur_radius: px(64.), spread_radius: px(-24.), inset: false }])
                        .child(
                            div()
                                .px(px(8.))
                                .py(px(6.))
                                .border_b_1()
                                .border_color(hsla(p.border))
                                .child(
                                    div()
                                        .h(px(34.))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .child(div().flex_1().min_w_0().child(Input::new(&self.input).appearance(false).bordered(false)))
                                        .when(self.mode == Mode::Content && self.content.target.is_some(), |this| this.child(self.render_content_options(cx))),
                                ),
                        )
                        .child(match self.mode {
                            Mode::Command => div()
                                .id("palette-results")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll)
                                .p(px(8.))
                                .when(empty, |this| this.child(div().py(px(24.)).flex().justify_center().text_sm().text_color(hsla(p.muted_foreground)).child("No results")))
                                .children(rendered)
                                .into_any_element(),
                            Mode::Files => self.files.render(cx),
                            Mode::Content => self.content.render(cx),
                            Mode::Browse => self.browse.render(cx),
                            Mode::NewProject => div()
                                .py(px(24.))
                                .px(px(24.))
                                .text_sm()
                                .text_color(hsla(p.muted_foreground))
                                .child("Name the project; the server creates its folder and a first commit.")
                                .into_any_element(),
                        })
                        .child(self.render_footer(cx)),
                ),
        )
        .with_priority(60)
        .into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod scoped_project_tests {
    use super::*;
    use t3_core::sidebar::grouping::{EnvironmentOwner, GroupConsumer, GroupInput, GroupingSettings, ProjectPreferences, ProjectRef, ProjectSortOrder, ScopedProject, ScopedProjectGroup, build_scoped_project_groups};

    fn groups() -> Vec<ScopedProjectGroup> {
        let projects = [("p", "Pinned", "/p", "p"), ("a", "Alpha", "/alpha", "repo"), ("b", "Beta", "/b", "b"), ("remote", "Secondary", "/uniquebeta", "repo")].map(|(saved, title, root, repo)| ScopedProject {
            owner: EnvironmentOwner { saved_id: saved.into(), canonical_id: Some(saved.into()), generation: 1, connection_generation: 1, shell_generation: 1 },
            project: serde_json::from_value(serde_json::json!({"id":"same","title":title,"workspaceRoot":root,"repositoryIdentity":{"canonicalKey":repo},"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z","pinnedAt":if saved == "p" { Some("2026-01-01T00:00:00Z") } else { None }})).unwrap(),
            is_scratch: false, environment_label: if saved == "remote" { "UniqueRemoteHost".into() } else { saved.into() },
        });
        build_scoped_project_groups(&GroupInput { projects: &projects, threads: &[], settings: &GroupingSettings::default(), preferences: &ProjectPreferences::default(), preferred_environment: Some("a"), consumer: GroupConsumer::Palette, sort: ProjectSortOrder::Manual })
    }

    fn scratch_groups() -> Vec<ScopedProjectGroup> {
        let mut projects = groups().into_iter().flat_map(|group| group.registrations).collect::<Vec<_>>();
        for saved in ["scratch-a", "scratch-b"] {
            let mut scratch = projects.iter().find(|project| project.owner.saved_id == "a").unwrap().clone();
            scratch.owner.saved_id = saved.into(); scratch.owner.canonical_id = Some(saved.into());
            scratch.project.title = saved.into(); scratch.project.workspace_root = format!("/{saved}");
            scratch.project.repository_identity = None; scratch.is_scratch = true;
            projects.push(scratch);
        }
        build_scoped_project_groups(&GroupInput { projects:&projects,threads:&[],settings:&GroupingSettings::default(),preferences:&ProjectPreferences::default(),preferred_environment:Some("scratch-b"),consumer:GroupConsumer::Palette,sort:ProjectSortOrder::Manual })
    }

    #[test]
    fn scoped_project_picker_promotes_only_exact_unpinned_context() {
        let groups = groups();
        let original = groups.iter().map(|group| group.key.clone()).collect::<Vec<_>>();
        let pinned = groups.iter().find(|group| group.pinned).unwrap().key.clone();
        let alpha = groups.iter().find(|group| group.member_refs.contains(&ProjectRef { environment_id:"a".into(), project_id:"same".into() })).unwrap().key.clone();
        let beta = groups.iter().find(|group| group.member_refs.contains(&ProjectRef { environment_id:"b".into(), project_id:"same".into() })).unwrap().key.clone();
        for (saved, order) in [("b", vec![pinned.clone(), beta, alpha]), ("p", original.clone()), ("missing", original.clone())] {
            let preferred = ProjectRef { environment_id: saved.into(), project_id:"same".into() };
            let entries = crate::new_thread::picker_entries(groups.clone(), Some(&preferred));
            assert_eq!(entries.iter().map(|entry| entry.group.key.clone()).collect::<Vec<_>>(), order, "context {saved}");
            if let Some(entry) = entries.iter().find(|entry| entry.is_preferred) { assert_eq!(entry.target.project.reference(), preferred); }
        }
        assert_eq!(groups.iter().map(|group| group.key.clone()).collect::<Vec<_>>(), original);
    }

    #[test]
    fn scoped_project_palette_search_finds_nonrepresentative_checkout() {
        let preferred = ProjectRef { environment_id:"a".into(), project_id:"same".into() };
        let entries = crate::new_thread::picker_entries(groups(), Some(&preferred));
        let (items, actions) = scoped_project_items(&entries);
        for (query, draft) in [("/uniquebeta", false), ("UniqueRemoteHost", false), ("Secondary", false), ("/uniquebeta", true)] {
            let category = items.iter().filter(|item| actions[&item.value].2 == draft).cloned().collect::<Vec<_>>();
            let matches = filter_command_palette_groups(&CommandPaletteSearch { active_groups:&[],query,is_in_submenu:false,all_options:None,project_search_items:&category,settings_search_items:&[],thread_search_items:&[],now:0 });
            let matched = matches.iter().flat_map(|group| &group.items).collect::<Vec<_>>();
            assert_eq!(matched.len(), 1, "query {query}, draft {draft}");
            assert_eq!(actions[&matched[0].value].1, preferred);
        }
    }

    #[test]
    fn scoped_project_draft_picker_excludes_actual_scratch_targets() {
        let preferred = ProjectRef { environment_id:"scratch-b".into(),project_id:"same".into() };
        let entries = crate::new_thread::picker_entries(scratch_groups(), Some(&preferred));
        let drafts = crate::new_thread::draft_picker_entries(entries).collect::<Vec<_>>();
        assert!(drafts.iter().all(|entry| !entry.target.project.is_scratch));
        assert_eq!(drafts.len(), 3);
    }

    #[test]
    fn scoped_project_palette_offers_one_existing_scratch_target_in_context_order() {
        for (saved, primary, context, ready, expected, index) in [
            ("scratch-b", Some("scratch-a"), Some("scratch-b"), vec!["scratch-b"], Some("scratch-b"), 1),
            ("scratch-b", None, None, vec!["scratch-a", "scratch-b"], None, 1),
            ("scratch-b", Some("scratch-a"), Some("scratch-b"), vec!["scratch-a", "scratch-b"], Some("scratch-a"), 1),
            ("scratch-b", None, Some("scratch-b"), vec!["scratch-a", "scratch-b"], Some("scratch-b"), 1),
            ("b", None, Some("b"), vec!["scratch-a", "scratch-b"], None, 2),
            ("b", Some("scratch-a"), Some("b"), vec!["scratch-a", "scratch-b"], Some("scratch-a"), 2),
            ("scratch-b", Some("missing"), Some("scratch-b"), vec!["scratch-a", "scratch-b"], Some("scratch-b"), 1),
        ] {
            let preferred = ProjectRef { environment_id:saved.into(),project_id:"same".into() };
            let entries = crate::new_thread::picker_entries(scratch_groups(), Some(&preferred));
            let (items, actions) = scoped_project_items(&entries);
            let root_scratch = items.iter().filter(|item| !actions[&item.value].2 && actions[&item.value].0.registrations.iter().any(|project| project.is_scratch)).count();
            assert_eq!(root_scratch, 2, "root project search retains physical scratch entries");
            let eligible = ready.into_iter().map(str::to_owned).collect();
            let submenu = scoped_project_thread_items(&items, &actions, Some(&preferred), primary, context, &eligible);
            let scratch = submenu.iter().filter(|item| actions[&item.value].0.registrations.iter().all(|project| project.is_scratch)).collect::<Vec<_>>();
            assert_eq!(scratch.len(), usize::from(expected.is_some()));
            if let Some(expected) = expected {
                assert_eq!(scratch[0].title, "No project");
                assert_eq!(actions[&scratch[0].value].1.environment_id, expected);
                let position = submenu.iter().position(|item| item.value == scratch[0].value).unwrap();
                assert_eq!(position, index, "placement follows first ordered scratch, not selected destination");
            }
        }
        let entries = crate::new_thread::picker_entries(scratch_groups().into_iter().filter(|group| !group.registrations.iter().any(|project| project.owner.saved_id == "scratch-b")).collect(), None);
        let (items, actions) = scoped_project_items(&entries);
        let submenu = scoped_project_thread_items(&items, &actions, None, None, None, &["scratch-a".to_owned()].into());
        let scratch = submenu.iter().find(|item| item.title == "No project").unwrap();
        assert_eq!(actions[&scratch.value].1.environment_id, "scratch-a", "sole existing offering does not need a primary or context");
    }

    #[gpui::test]
    fn scoped_project_palette_empty_query_exposes_new_thread_group_browse(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (state, fixture) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            installed
        });
        let (palette, cx) = cx.add_window_view(|window, cx| {
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            CommandPalette::new(state.clone(), composer, window, cx)
        });
        cx.update(|window, cx| palette.update(cx, |palette, cx| {
            palette.toggle_mode(Mode::Command, window, cx);
            let value = palette.root.iter().flat_map(|group| &group.items).find(|item| item.value == "action:new-thread-in").map(|item| item.value.clone()).expect("new-thread project browse is reachable without a search query");
            palette.activate(&value, window, cx);
            assert!(palette.open, "entering the group list keeps the palette open");
            assert!(!build_command_palette_rows(&palette.groups).item_values.is_empty());
            palette.escape(window, cx);
            assert!(palette.open, "Escape from the group list returns to the root");
            assert!(palette.search.views.is_empty());
            palette.search = reduce_command_palette_search_state(std::mem::take(&mut palette.search), CommandPaletteSearchAction::Query("new thread in".into()));
            palette.input.update(cx, |input, cx| input.set_value("new thread in", window, cx));
            palette.refilter(cx);
            palette.activate(&value, window, cx);
            assert_eq!(palette.search.query, "");
            assert_eq!(palette.search.views.len(), 1);
            palette.escape(window, cx);
            assert_eq!(palette.search.query, "new thread in");
            assert!(palette.search.views.is_empty());
            assert!(palette.open);
            assert!(palette.input.read(cx).focus_handle(cx).is_focused(window));
            window.blur();
        }));
        state.update(cx, |state, _| state.stop_pump());
        cx.cx.update(|cx| { cx.shutdown(); cx.clear_globals(); });
        drop(fixture); drop(runtime); cx.run_until_parked();
    }
}

#[cfg(test)]
mod visit_recency_tests {
    use super::*;
    use t3_core::sidebar::visits::OpenHistory;

    #[test]
    fn palette_ranking_observes_deduped_local_opens_and_identity_replacement() {
        let mut model = t3_core::sidebar::SidebarModel::default();
        for id in ["a", "b"] {
            let shell: t3_core::shell::ThreadShell = serde_json::from_value(serde_json::json!({
                "id":id,"projectId":"p","title":id,"providerInstanceId":"replay",
                "modelSelection":{"instanceId":"replay","model":"replay"},"runtimeMode":"full-access","interactionMode":"default",
                "lineage":{"rootThreadId":id},"status":"idle","hasActionableProposedPlan":false,
                "itemCount":0,"visibleItemCount":0,"createdAt":"1970-01-01T00:00:00Z","updatedAt":"1970-01-01T00:00:00Z"
            })).unwrap();
            model
                .threads
                .push(t3_core::sidebar::thread::present_thread(&shell, None));
        }
        let mut history = OpenHistory::default();
        let environment = history.environment(Some("saved"), None).unwrap();
        assert!(history.claim(&environment, "a", 1_000));
        assert!(history.claim(&environment, "b", 2_000));
        let rank = |history: &OpenHistory,
                    environment: &t3_core::sidebar::visits::OpenEnvironment| {
            let items = build_thread_action_items(
                &palette_threads(&model, history, Some(environment)),
                None,
                &HashMap::new(),
                |_| None,
            );
            recent_thread_items(&items, None)
                .into_iter()
                .map(|item| item.value)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            rank(&history, &environment),
            ["thread:local:b", "thread:local:a"]
        );
        assert!(!history.claim(&environment, "a", 3_000));
        assert_eq!(
            rank(&history, &environment),
            ["thread:local:a", "thread:local:b"]
        );
        let bound = history
            .environment(Some("saved"), Some("canonical"))
            .unwrap();
        assert_eq!(rank(&history, &bound), ["thread:local:a", "thread:local:b"]);
        let other = history.environment(Some("other"), Some("other")).unwrap();
        assert!(
            palette_threads(&model, &history, Some(&other))
                .iter()
                .all(|thread| thread.local_opened_at.is_none())
        );
        let replacement = history
            .environment(Some("saved"), Some("replacement"))
            .unwrap();
        assert!(
            palette_threads(&model, &history, Some(&replacement))
                .iter()
                .all(|thread| thread.local_opened_at.is_none())
        );
    }
}
