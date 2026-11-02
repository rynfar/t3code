//! Offscreen screenshots of the real app: `cargo run -p t3-native --features snapshot -- --snapshot
//! out.png [options]`. Boots the same views as the app against the server saved in
//! `T3_NATIVE_HOME`, renders with GPUI's Metal test renderer (no window on screen, so it works
//! while the screen is locked or recording is denied), and writes one PNG.
//!
//! It never writes to the server: it only subscribes and reads. Local UI state goes to a
//! throwaway copy of the home, so the caller's saved panels and last thread stay as they were.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AnyWindowHandle, Entity, VisualTestAppContext, px, size};
use t3_core::client_settings::AppearanceMode;
use t3_core::settings::search::{NAV_GROUPS, Section};

use crate::app_state::AppState;

const USAGE: &str = "usage: t3-native --snapshot <out.png> [--thread <id>] [--right-panel <surface>] \
[--settings <section>] [--palette] [--width 1440] [--height 900] [--dark|--light] [--wait 6]";

pub struct Options {
    out: PathBuf,
    thread: Option<String>,
    right_panel: Option<String>,
    settings: Option<String>,
    palette: bool,
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
        let section = section(name).ok_or_else(|| format!("unknown settings section {name:?}"))?;
        cx.update(|cx| crate::settings::open(section, cx));
    }
    if options.palette {
        // Actions bubble from the focused element; an offscreen window starts with none.
        cx.update_window(window, |_, window, cx| {
            window.focus_next(cx);
            window.dispatch_action(Box::new(crate::keymap::ToggleCommandPalette), cx);
        })
        .map_err(|error| error.to_string())?;
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
