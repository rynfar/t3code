//! Offscreen screenshots of the real app: `cargo run -p t3-native --features snapshot -- --snapshot
//! out.png [options]`. Boots the same views as the app against the server saved in
//! `T3_NATIVE_HOME`, renders with GPUI's Metal test renderer (no window on screen, so it works
//! while the screen is locked or recording is denied), and writes one PNG.
//!
//! It never writes to the server unless told to (`--attach`, or a `--click` on a control that
//! writes): it only subscribes and reads. Local UI state goes to a throwaway copy of the home, so
//! the caller's saved panels and last thread stay as they were.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AnyWindowHandle, Entity, VisualTestAppContext, px, size};
use t3_core::client_settings::AppearanceMode;
use t3_core::settings::search::{NAV_GROUPS, Section};
use t3_core::usage::{UsageMetric, UsagePeriod};

use crate::app_state::AppState;

const USAGE: &str = "usage: t3-native --snapshot <out.png> [--thread <id>] [--right-panel <surface>] \
[--settings <section>[:<project id>[:add-action]]] [--palette] [--new-thread] [--attach <file>]... [--prompt <text>] \
[--pull-requests] [--pull-request <number> [--pull-request-tab code]] \
[--usage <cost|tokens|limits>] [--usage-period <day|week|month|quarter>] [--usage-model] [--context-meter] \
[--click <x>,<y>]... [--width 1440] [--height 900] [--dark|--light] [--wait 6]";

pub struct Options {
    out: PathBuf,
    thread: Option<String>,
    right_panel: Option<String>,
    settings: Option<String>,
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
    /// Left clicks at window points, in order, after everything else is open.
    clicks: Vec<(f32, f32)>,
    width: f32,
    height: f32,
    appearance: Option<AppearanceMode>,
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
        Some(Options {
            out: PathBuf::from(out),
            thread: flag("--thread"),
            right_panel: flag("--right-panel"),
            settings: flag("--settings"),
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
            clicks: args
                .windows(2)
                .filter(|pair| pair[0] == "--click")
                .filter_map(|pair| pair[1].split_once(',').and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?))))
                .collect(),
            width: number("--width", 1440.),
            height: number("--height", 900.),
            appearance: if has("--dark") {
                Some(AppearanceMode::Dark)
            } else if has("--light") {
                Some(AppearanceMode::Light)
            } else {
                None
            },
            wait: number("--wait", 6.),
        })
    }
}

fn section(name: &str) -> Option<Section> {
    let name = name.to_lowercase();
    NAV_GROUPS
        .iter()
        .flat_map(|(_, sections)| sections.iter().copied())
        .find(|section| format!("{section:?}").to_lowercase() == name || section.label().to_lowercase().starts_with(&name))
}

/// Runs foreground and background work and lets network replies land, for `seconds` or until
/// `done` holds.
fn pump(cx: &mut VisualTestAppContext, seconds: f32, done: impl Fn(&mut VisualTestAppContext) -> bool) {
    let until = Instant::now() + Duration::from_secs_f32(seconds);
    while Instant::now() < until {
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
    for name in ["environments.json", "connection.json"] {
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
    if let Some(mode) = options.appearance {
        cx.update(|cx| {
            let preferences = crate::theme::Preferences { mode, ..crate::theme::preferences(cx).clone() };
            crate::theme::set_preferences(preferences, cx);
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
        let section = section(name).ok_or_else(|| format!("unknown settings section {name:?}"))?;
        match project {
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
    for &(x, y) in &options.clicks {
        pump(&mut cx, options.wait.min(3.), |_| false);
        let position = gpui::point(px(x), px(y));
        cx.simulate_mouse_move(window, position, None, gpui::Modifiers::default());
        cx.simulate_click(window, position, gpui::Modifiers::default());
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
