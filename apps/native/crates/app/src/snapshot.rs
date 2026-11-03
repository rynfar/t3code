//! Offscreen screenshots of the real app: `cargo run -p t3-native --features snapshot -- --snapshot
//! out.png [options]`. Boots the same views as the app against the server saved in
//! `T3_NATIVE_HOME`, renders with GPUI's Metal test renderer (no window on screen, so it works
//! while the screen is locked or recording is denied), and writes one PNG.
//!
//! It never writes to the server unless told to (`--attach`, or a `--click` on a control that
//! writes): it only subscribes and reads. Local UI state goes to a throwaway copy of the home, so
//! the caller's saved panels and last thread stay as they were. It runs on the real platform, so a
//! `--keys` copy reaches the system clipboard.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, Entity, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, TouchPhase, VisualTestAppContext,
    point, px, size,
};
use t3_core::client_settings::AppearanceMode;
use t3_core::settings::search::{NAV_GROUPS, Section};
use t3_core::usage::{UsageMetric, UsagePeriod};

use crate::app_state::AppState;

const USAGE: &str = "usage: t3-native --snapshot <out.png> [--thread <id>] [--right-panel <surface>] \
[--settings <section>[:<project id>[:add-action]]] [--palette] [--new-thread] [--attach <file>]... [--prompt <text>] \
[--pull-requests] [--pull-request <number> [--pull-request-tab code]] \
[--connect] [--cloud-fixture <json>] [--usage <cost|tokens|limits>] [--usage-period <day|week|month|quarter>] [--usage-model] [--context-meter] [--welcome [connect|agents]] \
[--worktree-setup <snapshot.json>] \
[--click|--double-click|--triple-click|--shift-click|--right-click|--hover <x>,<y>]... [--drag <x1>,<y1>,<x2>,<y2>]... \
[--scroll <x>,<y>,<dy>]... [--keys <keystrokes>]... [--html-fixture <page.html>] [--width 1440] [--height 900] [--dark|--light] [--font-size <px>] [--wait 6]";

/// Input replayed in command-line order once everything else is open, at window points.
enum Gesture {
    /// Left presses: `--click`, `--double-click`, `--triple-click`, `--shift-click`.
    Click { at: Point<Pixels>, count: usize, shift: bool },
    RightClick(Point<Pixels>),
    /// The pointer rests here, so hover states show.
    Hover(Point<Pixels>),
    /// Press, move in steps, release.
    Drag(Point<Pixels>, Point<Pixels>),
    /// One wheel step of `dy` pixels with the pointer left there; positive scrolls toward the top.
    Scroll(Point<Pixels>, f32),
    /// Space-separated keystrokes, as `cmd-a escape`.
    Keys(String),
}

fn gestures(args: &[String]) -> Vec<Gesture> {
    args.windows(2)
        .filter_map(|pair| {
            let numbers: Option<Vec<f32>> = pair[1].split(',').map(|number| number.trim().parse().ok()).collect();
            let at = |n: &[f32]| point(px(n[0]), px(n[1]));
            let click = |n: &[f32], count, shift| Gesture::Click { at: at(n), count, shift };
            Some(match (pair[0].as_str(), numbers.as_deref()) {
                ("--click", Some(n @ [_, _])) => click(n, 1, false),
                ("--double-click", Some(n @ [_, _])) => click(n, 2, false),
                ("--triple-click", Some(n @ [_, _])) => click(n, 3, false),
                ("--shift-click", Some(n @ [_, _])) => click(n, 1, true),
                ("--right-click", Some(n @ [_, _])) => Gesture::RightClick(at(n)),
                ("--hover", Some(n @ [_, _])) => Gesture::Hover(at(n)),
                ("--drag", Some(n @ [_, _, _, _])) => Gesture::Drag(at(n), at(&n[2..])),
                ("--scroll", Some(n @ [_, _, dy])) => Gesture::Scroll(at(n), *dy),
                ("--keys", _) => Gesture::Keys(pair[1].clone()),
                _ => return None,
            })
        })
        .collect()
}

fn replay(cx: &mut VisualTestAppContext, window: AnyWindowHandle, gesture: &Gesture) {
    let none = Modifiers::default();
    match gesture {
        Gesture::Click { at, count, shift } => {
            let modifiers = Modifiers { shift: *shift, ..Modifiers::default() };
            cx.simulate_mouse_move(window, *at, None, modifiers);
            for click_count in 1..=*count {
                cx.simulate_event(window, MouseDownEvent { button: MouseButton::Left, position: *at, modifiers, click_count, first_mouse: false });
                cx.simulate_event(window, MouseUpEvent { button: MouseButton::Left, position: *at, modifiers, click_count });
            }
        }
        Gesture::RightClick(at) => {
            cx.simulate_mouse_move(window, *at, None, none);
            cx.simulate_mouse_down(window, *at, MouseButton::Right, none);
            cx.simulate_mouse_up(window, *at, MouseButton::Right, none);
        }
        Gesture::Drag(from, to) => {
            cx.simulate_mouse_move(window, *from, None, none);
            cx.simulate_mouse_down(window, *from, MouseButton::Left, none);
            for step in 1..=8 {
                let t = step as f32 / 8.;
                cx.simulate_mouse_move(window, point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t), MouseButton::Left, none);
            }
            cx.simulate_mouse_up(window, *to, MouseButton::Left, none);
        }
        Gesture::Hover(at) => cx.simulate_mouse_move(window, *at, None, none),
        Gesture::Scroll(at, dy) => {
            cx.simulate_mouse_move(window, *at, None, none);
            pump(cx, 0.5, |_| false);
            let delta = ScrollDelta::Pixels(point(px(0.), px(*dy)));
            cx.simulate_event(window, ScrollWheelEvent { position: *at, delta, modifiers: none, touch_phase: TouchPhase::Moved });
        }
        Gesture::Keys(keys) => cx.simulate_keystrokes(window, keys),
    }
}

pub struct Options {
    out: PathBuf,
    thread: Option<String>,
    right_panel: Option<String>,
    settings: Option<String>,
    connect_screen: bool,
    cloud_fixture: Option<String>,
    palette: bool,
    /// Opens the new-thread draft, as `chat.new` does.
    new_thread: bool,
    /// Files dropped on the composer. They upload, so point this at a scratch server.
    attach: Vec<PathBuf>,
    /// Typed into the composer last, so a trigger such as `@src` or `/` opens its menu.
    prompt: Option<String>,
    pull_requests: bool,
    pull_request: Option<u64>,
    pull_request_code: bool,
    usage: Option<String>,
    usage_period: Option<String>,
    usage_model: bool,
    context_meter: bool,
    /// Opens the welcome wizard at a step whatever the first-run gate decides.
    welcome: Option<crate::onboarding::Step>,
    /// A `WorktreeSetupSnapshot` file shown for `--thread` instead of the server's.
    worktree_setup: Option<PathBuf>,
    gestures: Vec<Gesture>,
    width: f32,
    height: f32,
    appearance: Option<AppearanceMode>,
    /// The interface font size, as Settings → Appearance sets it.
    font_size: Option<f32>,
    wait: f32,
}

impl Options {
    /// `Some` when the command line asks for a snapshot.
    pub fn from_args() -> Option<Options> {
        let args: Vec<String> = std::env::args().collect();
        let flag = |name: &str| args.iter().position(|arg| arg == name).and_then(|index| args.get(index + 1)).cloned();
        let has = |name: &str| args.iter().any(|arg| arg == name);
        let number = |name: &str, default: f32| flag(name).and_then(|value| value.parse().ok()).unwrap_or(default);
        let out = flag("--snapshot")?;
        crate::webview::set_offscreen();
        if let Some(page) = flag("--html-fixture") {
            crate::transcript::html_render::fixture::set(PathBuf::from(page));
        }
        Some(Options {
            out: PathBuf::from(out),
            thread: flag("--thread"),
            right_panel: flag("--right-panel"),
            settings: flag("--settings"),
            connect_screen: has("--connect"),
            cloud_fixture: flag("--cloud-fixture"),
            palette: has("--palette"),
            new_thread: has("--new-thread"),
            attach: args.windows(2).filter(|pair| pair[0] == "--attach").map(|pair| PathBuf::from(&pair[1])).collect(),
            prompt: flag("--prompt"),
            pull_requests: has("--pull-requests"),
            pull_request: flag("--pull-request").and_then(|value| value.trim_start_matches('#').parse().ok()),
            pull_request_code: flag("--pull-request-tab").is_some_and(|tab| tab == "code"),
            usage: flag("--usage"),
            usage_period: flag("--usage-period"),
            usage_model: has("--usage-model"),
            context_meter: has("--context-meter"),
            welcome: has("--welcome").then(|| match flag("--welcome").as_deref() {
                Some("agents") => crate::onboarding::Step::Agents,
                _ => crate::onboarding::Step::Connection,
            }),
            worktree_setup: flag("--worktree-setup").map(PathBuf::from),
            gestures: gestures(&args),
            width: number("--width", 1440.),
            height: number("--height", 900.),
            appearance: if has("--dark") {
                Some(AppearanceMode::Dark)
            } else if has("--light") {
                Some(AppearanceMode::Light)
            } else {
                None
            },
            font_size: flag("--font-size").and_then(|value| value.parse().ok()),
            wait: number("--wait", 6.),
        })
    }
}

fn section(name: &str) -> Option<Section> {
    let name = name.to_lowercase();
    NAV_GROUPS
        .iter()
        .flat_map(|(_, sections)| sections.iter().copied())
        .chain([Section::Diagnostics])
        .find(|section| format!("{section:?}").to_lowercase() == name || section.label().to_lowercase().starts_with(&name))
}

/// Runs foreground and background work and lets network replies land, for `seconds` or until
/// `done` holds.
fn pump(cx: &mut VisualTestAppContext, seconds: f32, done: impl Fn(&mut VisualTestAppContext) -> bool) {
    let until = Instant::now() + Duration::from_secs_f32(seconds);
    while Instant::now() < until {
        // The test platform has no display link; deliver `on_next_frame` work as the app would.
        cx.update(|cx| {
            for window in cx.windows() {
                let _ = window.update(cx, |_, window, cx| window.simulate_next_frame(cx));
            }
        });
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        cx.advance_clock(Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A private copy of the saved environments, so nothing the snapshot persists reaches the
/// caller's home. A home with none renders the connect screen.
fn isolated_home() -> Result<PathBuf, String> {
    let source = std::env::var_os("T3_NATIVE_HOME")
        .map(PathBuf::from)
        .ok_or("set T3_NATIVE_HOME to the native home whose saved environments to use")?;
    let home = std::env::temp_dir().join(format!("t3-native-snapshot-{}", std::process::id()));
    std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    for name in ["environments.json", "connection.json", "composer-drafts.json"] {
        if source.join(name).exists() {
            std::fs::copy(source.join(name), home.join(name)).map_err(|error| format!("could not copy {name}: {error}"))?;
        }
    }
    Ok(home)
}

pub fn run(options: Options, runtime: tokio::runtime::Handle) {
    let result = isolated_home().and_then(|home| {
        t3_client::store::use_home(home.clone());
        let result = capture(&options, runtime);
        let _ = std::fs::remove_dir_all(home);
        result
    });
    match result {
        Ok(()) => println!("{}", options.out.display()),
        Err(error) => {
            eprintln!("snapshot failed: {error}\n{USAGE}");
            std::process::exit(1);
        }
    }
}

fn capture(options: &Options, runtime: tokio::runtime::Handle) -> Result<(), String> {
    let mut cx = VisualTestAppContext::with_asset_source(gpui_platform::current_platform(false), Arc::new(crate::icons::Assets));
    let state: Entity<AppState> = cx.update(|cx| crate::boot(runtime, cx));
    let window: AnyWindowHandle = cx
        .open_offscreen_window(size(px(options.width), px(options.height)), |window, cx| crate::root_view(&state, window, cx))
        .map_err(|error| error.to_string())?
        .into();
    let synchronized = |state: &Entity<AppState>| {
        let state = state.clone();
        move |cx: &mut VisualTestAppContext| state.read_with(cx, |state, _| state.shell.synchronized)
    };
    pump(&mut cx, options.wait, synchronized(&state));
    if options.context_meter {
        state.update(&mut cx, |state, _| state.client_settings.context_window_meter = true);
    }
    if let Some(thread) = &options.thread {
        state.update(&mut cx, |state, cx| state.select_thread(thread, cx));
        let loaded = state.clone();
        pump(&mut cx, options.wait, move |cx| loaded.read_with(cx, |state, _| state.thread.projection.is_some()));
    }
    if let Some(path) = &options.worktree_setup {
        let text = std::fs::read_to_string(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        let snapshot = t3_core::worktree_setup::decode_event(value).map_err(|error| error.to_string())?.ok_or("the worktree setup file holds null")?;
        let store = cx.update(|cx| crate::worktree_setup::store(&state, cx));
        store.update(&mut cx, |store, cx| store.show_fixture(snapshot, cx));
    }
    if options.appearance.is_some() || options.font_size.is_some() {
        cx.update(|cx| {
            let current = crate::theme::preferences(cx).clone();
            let mode = options.appearance.unwrap_or(current.mode);
            let interface_font_size = options.font_size.unwrap_or(current.interface_font_size);
            crate::theme::set_preferences(crate::theme::Preferences { mode, interface_font_size, ..current }, cx);
        });
    }
    if let Some(surface) = &options.right_panel {
        let opened = state.update(&mut cx, |state, cx| crate::right_panel::open_surface(state, surface, cx));
        if !opened {
            return Err(format!("unknown right panel surface {surface:?}: use a launcher entry such as \"diff\""));
        }
    }
    if let Some(name) = &options.settings {
        // `projects:<project id>` opens a project's page; `:add-action` opens its action editor.
        let (name, project) = name.split_once(':').map_or((name.as_str(), None), |(name, project)| (name, Some(project)));
        // A settings search result's id lands on its row, scrolled into view.
        let row = section(name).is_none().then(|| name.to_owned());
        if let Some(id) = &row
            && !t3_core::settings::search::items(t3_core::settings::search::Features { secrets: true }).iter().any(|item| &item.id == id)
        {
            return Err(format!("unknown settings section or row {name:?}"));
        }
        let section = section(name).unwrap_or(Section::General);
        match project {
            _ if row.is_some() => {
                let id = row.unwrap_or_default();
                cx.update(|cx| crate::settings::open_setting(&id, cx));
            }
            Some(project) => {
                let (project, add_action) = project.strip_suffix(":add-action").map_or((project, false), |project| (project, true));
                cx.update(|cx| crate::settings::open_project(project, cx));
                if add_action {
                    pump(&mut cx, 1., |_| false);
                    cx.update(crate::settings::open_add_action);
                }
            }
            None => cx.update(|cx| crate::settings::open(section, cx)),
        }
    }
    if options.pull_requests || options.pull_request.is_some() {
        if let Some(number) = options.pull_request {
            cx.update(|cx| cx.set_global(crate::pull_requests::SnapshotSelection { number, code: options.pull_request_code }));
        }
        state.update(&mut cx, |state, cx| state.set_pull_requests_open(true, cx));
        pump(&mut cx, options.wait, |_| false);
    }
    if options.palette {
        // Actions bubble from the focused element; an offscreen window starts with none.
        cx.update_window(window, |_, window, cx| {
            window.focus_next(cx);
            window.dispatch_action(Box::new(crate::keymap::ToggleCommandPalette), cx);
        })
        .map_err(|error| error.to_string())?;
    }
    if let Some(step) = options.welcome {
        cx.update(|cx| crate::onboarding::open(step, cx));
        pump(&mut cx, options.wait.min(3.), |_| false);
    }
    if options.new_thread {
        cx.update(|cx| cx.dispatch_action(&crate::new_thread::NewThread));
        pump(&mut cx, 1., |_| false);
    }
    if !options.attach.is_empty() || options.prompt.is_some() {
        // Actions bubble from the focused element; an offscreen window starts with none.
        cx.update_window(window, |_, window, cx| window.focus_next(cx)).map_err(|error| error.to_string())?;
        cx.dispatch_action(window, crate::composer::FocusPrompt);
    }
    if !options.attach.is_empty() {
        // Dropped inside the composer's card: at the bottom of a thread, mid-page on a draft.
        let draft = state.read_with(&cx, |state, _| state.selected_thread.is_none() && state.draft_project.is_some());
        let y = if draft { options.height * 0.55 } else { options.height - 40. };
        let position = gpui::point(px((crate::workspace::SIDEBAR_WIDTH + options.width) / 2.), px(y));
        let paths = gpui::ExternalPaths(options.attach.clone().into());
        cx.simulate_event(window, gpui::FileDropEvent::Entered { position, paths });
        cx.simulate_event(window, gpui::FileDropEvent::Submit { position });
        pump(&mut cx, options.wait.min(3.), |_| false);
    }
    if let Some(prompt) = &options.prompt {
        cx.simulate_input(window, prompt);
    }
    if let Some(metric) = &options.usage {
        let metric = UsageMetric::ALL.into_iter().find(|candidate| candidate.label().eq_ignore_ascii_case(metric)).ok_or_else(|| format!("unknown usage metric {metric:?}"))?;
        let period = match options.usage_period.as_deref() {
            None => None,
            Some(name) => Some(
                UsagePeriod::ALL
                    .into_iter()
                    .find(|period| format!("{period:?}").eq_ignore_ascii_case(name))
                    .ok_or_else(|| format!("unknown usage period {name:?}"))?,
            ),
        };
        cx.update(|cx| crate::usage::open_read_only(metric, period, options.usage_model, cx));
        pump(&mut cx, options.wait.max(30.), |cx| cx.update(crate::usage::is_settled));
    }
    if options.connect_screen || options.cloud_fixture.is_some() {
        state.update(&mut cx, |state, cx| state.open_switcher(cx));
    }
    if let Some(path) = &options.cloud_fixture {
        #[derive(serde::Deserialize)]
        struct Fixture { identity: String, environments: Vec<t3_core::relay::Discovered> }
        let fixture: Fixture = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        state.update(&mut cx, |state, cx| {
            state.cloud.busy = false;
            state.cloud.identity = Some(fixture.identity);
            state.cloud.discovery = t3_core::relay::Discovery { listed: true, environments: fixture.environments, ..Default::default() };
            cx.emit(crate::app_state::AppEvent::Phase);
        });
    }
    for gesture in &options.gestures {
        pump(&mut cx, options.wait.min(3.), |_| false);
        replay(&mut cx, window, gesture);
    }
    pump(&mut cx, options.wait.min(3.), |_| false);
    cx.update_window(window, |_, window, _| window.refresh()).map_err(|error| error.to_string())?;
    pump(&mut cx, 0.5, |_| false);
    let image = cx.capture_screenshot(window).map_err(|error| error.to_string())?;
    if let Some(parent) = options.out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    image.save(&options.out).map_err(|error| error.to_string())

}
