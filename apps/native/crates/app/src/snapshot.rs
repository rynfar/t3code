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
[--open-url <url>] [--prompt-link-repository <owner/name>] [--prompt-link-fixture <json>] [--clipboard-output <path>] [--settings <section>[:<project id>[:add-action]]] [--palette] [--new-thread] [--attach <file>]... [--prompt <text>] \
[--pull-requests] [--pull-request <number> [--pull-request-tab code]] \
[--connect] [--cloud-fixture <json>] [--usage <cost|tokens|limits>] [--usage-period <day|week|month|quarter>] [--usage-model] [--context-meter] [--welcome [connect|agents]] [--welcome-terminal-fixture <json>] \
[--question-request-fixture <json>] [--question-staged-fixture <json>] [--question-command-output <json>] [--staged-attachments-fixture <json>] [--environment-attach-fixture] [--notification-fixture visible|hidden|background] [--project-grouping-fixture <json>] [--provider-refresh-pending] [--turn-items-fixture <json>] [--no-command-output-streaming] [--chatgpt-fixture <json>] [--provider-fixture <json>] [--resource-telemetry-fixture <json>] [--managed-policy-fixture <json>] [--announcement-fixture <json>] [--updates-fixture <json>] [--worktree-setup <snapshot.json>] [--draft-workspace-fixture <json>] [--composer-recovery-fixture] [--workspace-availability-fixture] [--composer-audit <json>] \
[--click|--press|--double-click|--triple-click|--shift-click|--right-click|--hover <x>,<y>]... [--drag <x1>,<y1>,<x2>,<y2>]... \
[--scroll <x>,<y>,<dy>]... [--pause <seconds>]... [--gesture-wait <seconds>] [--render-counts-output <path>] [--media-audit <json>] [--media-profile-prefix <path>] [--media-select-all] [--media-cache-budget <bytes>] [--motion-frames <directory>] [--focus <element-id>] [--keys <keystrokes> | --key-up <keystrokes>]... [--type <text>]... [--html-fixture <page.html>] [--markdown-file-fixture <json>] [--document-fixture <json>] [--width 1440] [--height 900] [--dark|--light] [--font-size <px>] [--resize-font <px>] [--wait 6]";

/// Input replayed in command-line order once everything else is open, at window points.
enum Gesture {
    /// Left presses: `--click`, `--double-click`, `--triple-click`, `--shift-click`.
    Click { at: Point<Pixels>, count: usize, shift: bool },
    RightClick(Point<Pixels>),
    Press(Point<Pixels>),
    /// The pointer rests here, so hover states show.
    Hover(Point<Pixels>),
    /// Press, move in steps, release.
    Drag(Point<Pixels>, Point<Pixels>),
    /// One wheel step of `dy` pixels with the pointer left there; positive scrolls toward the top.
    Scroll(Point<Pixels>, f32),
    /// Space-separated keystrokes, as `cmd-a escape`.
    Keys(String),
    KeyUp(String),
    Focus(String),
    Text(String),
    FontSize(f32),
    Pause(f32),
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
                ("--press", Some(n @ [_, _])) => Gesture::Press(at(n)),
                ("--hover", Some(n @ [_, _])) => Gesture::Hover(at(n)),
                ("--drag", Some(n @ [_, _, _, _])) => Gesture::Drag(at(n), at(&n[2..])),
                ("--scroll", Some(n @ [_, _, dy])) => Gesture::Scroll(at(n), *dy),
                ("--resize-font", Some([size])) => Gesture::FontSize(*size),
                ("--pause", Some([seconds])) if seconds.is_finite() && *seconds >= 0. => Gesture::Pause(*seconds),
                ("--keys", _) => Gesture::Keys(pair[1].clone()),
                ("--key-up", _) => Gesture::KeyUp(pair[1].clone()),
                ("--focus", _) => Gesture::Focus(pair[1].clone()),
                ("--type", _) => Gesture::Text(pair[1].clone()),
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
        Gesture::Press(at) => {
            cx.simulate_mouse_move(window, *at, None, none);
            cx.simulate_mouse_down(window, *at, MouseButton::Left, none);
        }
        Gesture::Hover(at) => cx.simulate_mouse_move(window, *at, None, none),
        Gesture::Scroll(at, dy) => {
            cx.simulate_mouse_move(window, *at, None, none);
            pump(cx, 0.5, |_| false);
            let delta = ScrollDelta::Pixels(point(px(0.), px(*dy)));
            cx.simulate_event(window, ScrollWheelEvent { position: *at, delta, modifiers: none, touch_phase: TouchPhase::Moved });
        }
        Gesture::Keys(keys) => cx.simulate_keystrokes(window, keys),
        Gesture::KeyUp(keys) => {
            for key in keys.split_whitespace() {cx.simulate_event(window,gpui::KeyUpEvent {keystroke:gpui::Keystroke::parse(key).expect("snapshot key")});}
        }
        Gesture::Focus(id) => {
            cx.update_window(window, |_, window, cx| {
                let focus = cx.global::<FocusTargets>().0.get(id.as_str()).expect("unknown snapshot focus target").clone();
                window.focus(&focus, cx);
            }).expect("snapshot window closed");
        }
        Gesture::Text(text) => cx.simulate_input(window, text),
        Gesture::Pause(seconds) => pump(cx, *seconds, |_| false),
        Gesture::FontSize(size) => cx.update(|cx| {
            let current = crate::theme::preferences(cx).clone();
            crate::theme::set_preferences(crate::theme::Preferences { interface_font_size: *size, ..current }, cx);
        }),
    }
}

pub(crate) struct ProviderRefreshPending;
impl gpui::Global for ProviderRefreshPending {}

#[derive(Default)]
struct FocusTargets(std::collections::HashMap<&'static str, gpui::FocusHandle>);
impl gpui::Global for FocusTargets {}

pub(crate) fn register_focus(id: &'static str, focus: gpui::FocusHandle, cx: &mut gpui::App) {
    if cx.try_global::<FocusTargets>().is_none() { cx.set_global(FocusTargets::default()); }
    cx.global_mut::<FocusTargets>().0.insert(id, focus);
}

#[derive(Default)]
struct RenderCounts(std::cell::RefCell<std::collections::HashMap<&'static str, usize>>);
impl gpui::Global for RenderCounts {}

pub(crate) fn record_render(name: &'static str, cx: &mut gpui::App) {
    if let Some(counts) = cx.try_global::<RenderCounts>() { *counts.0.borrow_mut().entry(name).or_default() += 1; }
}

#[derive(Default)]
struct MediaAudit {
    sources: std::cell::RefCell<std::collections::BTreeMap<String, std::collections::BTreeSet<String>>>,
    notifications: std::cell::RefCell<std::collections::BTreeMap<String, usize>>,
    pointers: std::cell::RefCell<std::collections::BTreeMap<String, String>>,
    views: std::cell::RefCell<std::collections::BTreeMap<String, gpui::WeakEntity<gpui_component::text::TextViewState>>>,
}
impl gpui::Global for MediaAudit {}
pub(crate) fn record_media_prose(state: &Entity<gpui_component::text::TextViewState>, text: &str, cx: &mut gpui::App) {
    if let Some(audit) = cx.try_global::<MediaAudit>() {
        audit.sources.borrow_mut().entry(state.entity_id().to_string()).or_default().insert(text.to_owned());
    }
}
fn media_selections(cx: &gpui::App) -> std::collections::BTreeMap<String, String> {
    cx.global::<MediaAudit>().views.borrow().iter().filter_map(|(id, view)| view.upgrade().map(|view| (id.clone(), view.read(cx).selected_text()))).collect()
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentFixture { text: String, thread_id: Option<String>, base_dir: Option<String>, after_text: Option<String> }
struct DocumentView {
    text: Arc<str>,
    markdown: crate::transcript::markdown::MarkdownStore,
    scope: crate::media::MediaScope,
    _viewer: Entity<crate::media::MediaViewer>,
}
impl DocumentView {
    fn new(fixture: &DocumentFixture, cx: &mut gpui::Context<Self>) -> Self {
        use gpui::AppContext;
        let viewer = cx.new(crate::media::MediaViewer::new);
        Self {
            text: fixture.text.clone().into(), markdown: crate::transcript::markdown::MarkdownStore::new(cx.entity_id()),
            scope: crate::media::MediaScope { owner: cx.entity_id(), thread_id: fixture.thread_id.clone(), workspace_root: fixture.base_dir.clone(), workspace_mutation_id: None, viewer: viewer.downgrade() },
            _viewer: viewer,
        }
    }
}
impl gpui::Render for DocumentView {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{ParentElement, Styled};
        crate::media::set_scope(self.scope.clone(), cx);
        self.markdown.begin_frame();
        let state = self.markdown.state("document-fixture", &self.text, cx);
        if !self.text.contains("![") { record_media_prose(&state.entity, &self.text, cx); }
        gpui::div().size_full().p(px(24.)).child(crate::transcript::markdown::prose(&state, cx))
    }
}

fn media_profile_checkpoint(prefix: Option<&str>, phase: u64) -> Result<(), String> {
    let Some(prefix) = prefix else { return Ok(()) };
    #[cfg(unix)]
    unsafe {
        if const { option_env!("T3_MEDIA_COVERAGE").is_none() } { return Err("media profiling requires an instrument-coverage build".into()); }
        unsafe extern "C" {
            #[link_name = "__llvm_profile_set_filename"] fn set_filename(name: *const std::ffi::c_char);
            #[link_name = "__llvm_profile_write_file"] fn write() -> std::ffi::c_int;
            #[link_name = "__llvm_profile_reset_counters"] fn reset();
        }
        if phase > 0 {
            let filename = std::ffi::CString::new(format!("{prefix}.{phase}.profraw")).map_err(|e| e.to_string())?;
            set_filename(filename.as_ptr());
            if write() != 0 { return Err("could not write media profile".into()); }
        }
        let exit = std::ffi::CString::new(format!("{prefix}.exit.profraw")).map_err(|e| e.to_string())?;
        set_filename(exit.as_ptr());
        reset();
        Ok(())
    }
    #[cfg(not(unix))]
    { let _ = (prefix, phase); Err("media profiling requires Unix".into()) }
}

pub struct Options {
    out: PathBuf,
    thread: Option<String>,
    right_panel: Option<String>,
    settings: Option<String>,
    connect_screen: bool,
    cloud_fixture: Option<String>,
    chatgpt_fixture: Option<PathBuf>,
    provider_fixture: Option<PathBuf>,
    turn_items_fixture: Option<PathBuf>,
    updates_fixture: Option<String>,
    managed_policy_fixture: Option<String>,
    announcement_fixture: Option<String>,
    notification_fixture: Option<String>,
    environment_attach_fixture: bool,
    palette: bool,
    /// Opens the new-thread draft, as `chat.new` does.
    new_thread: bool,
    draft_workspace_fixture: Option<PathBuf>,
    composer_audit: Option<PathBuf>,
    recovery_fixture: bool,
    workspace_availability_fixture: bool,
    /// Files dropped on the composer. They upload, so point this at a scratch server.
    attach: Vec<PathBuf>,
    /// Typed into the composer last, so a trigger such as `@src` or `/` opens its menu.
    prompt: Option<String>,
    open_url: Option<String>,
    prompt_link_repository: Option<String>,
    prompt_link_fixture: Option<PathBuf>,
    project_grouping_fixture: Option<PathBuf>,
    clipboard_output: Option<PathBuf>,
    pull_requests: bool,
    pull_request: Option<u64>,
    pull_request_code: bool,
    usage: Option<String>,
    usage_period: Option<String>,
    usage_model: bool,
    context_meter: bool,
    /// Opens the welcome wizard at a step whatever the first-run gate decides.
    welcome: Option<crate::onboarding::Step>,
    welcome_terminal_fixture: Option<PathBuf>,
    /// A `WorktreeSetupSnapshot` file shown for `--thread` instead of the server's.
    worktree_setup: Option<PathBuf>,
    resource_telemetry_fixture: Option<PathBuf>,
    gestures: Vec<Gesture>,
    gesture_wait: f32,
    motion_frames: Option<PathBuf>,
    render_counts_output: Option<PathBuf>,
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
        if let Some(page) = flag("--mcp-fixture") {
            crate::transcript::mcp_app::fixture::set(PathBuf::from(page));
        }
        Some(Options {
            out: PathBuf::from(out),
            thread: flag("--thread"),
            right_panel: flag("--right-panel"),
            settings: flag("--settings"),
            connect_screen: has("--connect"),
            cloud_fixture: flag("--cloud-fixture"),
            chatgpt_fixture: flag("--chatgpt-fixture").map(PathBuf::from),
            provider_fixture: flag("--provider-fixture").map(PathBuf::from),
            turn_items_fixture: flag("--turn-items-fixture").map(PathBuf::from),
            updates_fixture: flag("--updates-fixture"),
            managed_policy_fixture: flag("--managed-policy-fixture"),
            announcement_fixture: flag("--announcement-fixture"),
            notification_fixture: flag("--notification-fixture"),
            environment_attach_fixture: has("--environment-attach-fixture"),
            palette: has("--palette"),
            new_thread: has("--new-thread"),
            draft_workspace_fixture: flag("--draft-workspace-fixture").map(PathBuf::from),
            composer_audit: flag("--composer-audit").map(PathBuf::from),
            recovery_fixture: has("--composer-recovery-fixture"),
            workspace_availability_fixture: has("--workspace-availability-fixture"),
            attach: args.windows(2).filter(|pair| pair[0] == "--attach").map(|pair| PathBuf::from(&pair[1])).collect(),
            prompt: flag("--prompt"),
            open_url: flag("--open-url"),
            prompt_link_repository: flag("--prompt-link-repository"),
            prompt_link_fixture: flag("--prompt-link-fixture").map(PathBuf::from),
            project_grouping_fixture: flag("--project-grouping-fixture").map(PathBuf::from),
            clipboard_output: flag("--clipboard-output").map(PathBuf::from),
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
            welcome_terminal_fixture: flag("--welcome-terminal-fixture").map(PathBuf::from),
            worktree_setup: flag("--worktree-setup").map(PathBuf::from),
            resource_telemetry_fixture: flag("--resource-telemetry-fixture").map(PathBuf::from),
            gestures: gestures(&args),
            gesture_wait: number("--gesture-wait", 3.).max(0.),
            motion_frames: flag("--motion-frames").map(PathBuf::from),
            render_counts_output: flag("--render-counts-output").map(PathBuf::from),
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
    for name in ["environments.json", "connection.json", "composer-drafts.json", "settings.json"] {
        if source.join(name).exists() {
            std::fs::copy(source.join(name), home.join(name)).map_err(|error| format!("could not copy {name}: {error}"))?;
        }
    }
    Ok(home)
}

pub fn run(options: Options, runtime: tokio::runtime::Handle) {
    let result = isolated_home().and_then(|home| {
        if std::env::var_os("T3_NATIVE_THEME_IMPORT_WRITE_FAILURE").is_some() {
            let path=home.join("settings.json");
            let _=std::fs::remove_file(&path);
            std::fs::create_dir(&path).map_err(|e|e.to_string())?;
        }
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
    let media_audit = std::env::args().collect::<Vec<_>>().windows(2).find(|pair| pair[0] == "--media-audit").map(|pair| pair[1].clone());
    if media_audit.is_some() {
        gpui::profiler::set_frame_trace_enabled(true);
        cx.update(|cx| {
            cx.set_global(MediaAudit::default());
            cx.observe_new::<gpui_component::text::TextViewState>(|state, _, cx| {
                let id = cx.entity_id().to_string();
                cx.global::<MediaAudit>().views.borrow_mut().insert(id.clone(), cx.entity().downgrade());
                let pointer = format!("{:#x}", std::ptr::from_ref(state) as usize);
                cx.global::<MediaAudit>().pointers.borrow_mut().insert(pointer, id.clone());
                cx.observe_self(move |_, cx| {
                    *cx.global::<MediaAudit>().notifications.borrow_mut().entry(id.clone()).or_default() += 1;
                }).detach();
            }).detach();
        });
    }
    if let Some(path) = &options.chatgpt_fixture {
        let fixture: crate::chatgpt_setup::Fixture = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        cx.update(|cx| cx.set_global(fixture));
    }
    if options.render_counts_output.is_some() { cx.update(|cx| cx.set_global(RenderCounts::default())); }
    if let Some(path) = std::env::args().collect::<Vec<_>>().windows(2).find(|pair| pair[0] == "--staged-attachments-fixture").map(|pair| pair[1].clone()) {
        let fixture = serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        cx.update(|cx| cx.set_global(crate::composer::AttachmentFixture(fixture)));
    }
    let state: Entity<AppState> = cx.update(|cx| crate::boot(runtime, cx));
    let document_fixture = std::env::args().collect::<Vec<_>>().windows(2).find(|pair| pair[0] == "--document-fixture").map(|pair| {
        let bytes = std::fs::read(&pair[1]).map_err(|e| e.to_string())?;
        serde_json::from_slice::<DocumentFixture>(&bytes).map_err(|e| e.to_string())
    }).transpose()?;
    let mut document_view = None;
    let window: AnyWindowHandle = cx
        .open_offscreen_window(size(px(options.width), px(options.height)), |window, cx| {
            use gpui::AppContext;
            if let Some(fixture) = &document_fixture {
                crate::media::cache(&state, cx);
                let document = cx.new(|cx| DocumentView::new(fixture, cx));
                document_view = Some(document.clone());
                cx.new(|cx| gpui_component::Root::new(document, window, cx))
            } else { crate::root_view(&state, window, cx) }
        })
        .map_err(|error| error.to_string())?
        .into();
    let synchronized = |state: &Entity<AppState>| {
        let state = state.clone();
        move |cx: &mut VisualTestAppContext| state.read_with(cx, |state, _| state.shell.synchronized)
    };
    pump(&mut cx, options.wait, synchronized(&state));
    if options.chatgpt_fixture.is_some() {
        let configured = state.clone();
        pump(&mut cx, options.wait, move |cx| configured.read_with(cx, |state, _| state.server_config.config.is_some()));
        state.update(&mut cx, |state, cx| {
            let fixture = cx.global::<crate::chatgpt_setup::Fixture>().clone();
            if let Some(config) = state.server_config.config.as_mut() {
                config.providers.retain(|p| p.driver != "codex");
                let mut instances=config.settings.provider_instances.iter().filter(|(_,p)|p.driver!="codex").map(|(id,p)|(id.to_owned(),p.clone())).collect::<Vec<_>>();
                for provider in std::iter::once(&fixture.provider).chain(&fixture.other_accounts) {
                    instances.push((provider.instance_id.clone(),t3_core::server_config::ProviderInstanceConfig {driver:"codex".into(),display_name:provider.display_name.clone(),config:fixture.managed.then(||serde_json::json!({"setupMode":"managed"})),..Default::default()}));
                }
                config.settings.provider_instances=instances.into_iter().collect();
                config.providers.splice(0..0,std::iter::once(fixture.provider).chain(fixture.other_accounts));
            }
            cx.emit(crate::app_state::AppEvent::ServerConfig);
        });
    }

    if std::env::args().any(|arg| arg == "--provider-refresh-pending") {
        cx.update(|cx| cx.set_global(ProviderRefreshPending));
    }
    if let Some(path) = &options.provider_fixture {
        let fixture: t3_core::server_config::ServerConfig =
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let configured = state.clone();
        pump(&mut cx, options.wait, move |cx| configured.read_with(cx, |state, _| state.server_config.config.is_some()));
        state.update(&mut cx, |state, cx| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.providers.retain(|provider| !fixture.providers.iter().any(|replacement| replacement.instance_id == provider.instance_id));
                config.providers.extend(fixture.providers);
                if fixture.secret_references {
                    config.secret_references=true;
                    config.settings.secret_providers=fixture.settings.secret_providers;
                }
                config.settings.provider_instances = config.settings.provider_instances.iter()
                    .filter(|(id, _)| fixture.settings.provider_instances.get(id).is_none())
                    .chain(fixture.settings.provider_instances.iter())
                    .map(|(id, instance)| (id.to_owned(), instance.clone())).collect();
            }
            cx.emit(crate::app_state::AppEvent::ServerConfig);
        });
    }

    if let Some(path) = &options.managed_policy_fixture {
        let policy: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        state.update(&mut cx, |state, cx| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.managed_policy = Some(policy);
            }
            cx.emit(crate::app_state::AppEvent::ServerConfig);
        });
    }
    if let Some(path) = &options.announcement_fixture {
        let messages: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        state.update(&mut cx, |state, cx| {
            for message in messages {
                if let Some(text) = message["text"].as_str() {
                    let priority = if message["assertive"] == true {
                        t3_core::announcements::Priority::Assertive
                    } else {
                        t3_core::announcements::Priority::Polite
                    };
                    state.announce_priority(text.into(), None, priority, cx);
                }
            }
        });
        pump(&mut cx, 1., |_| false);
    }
    if options.context_meter {
        state.update(&mut cx, |state, _| state.client_settings.context_window_meter = true);
    }
    if let Some(repo) = &options.prompt_link_repository {
        state.update(&mut cx, |state, cx| {
            if let Some(project) = state.shell.snapshot.as_mut().and_then(|s| s.projects.first_mut()) {
                project.repository_identity = Some(serde_json::json!({"canonicalKey": format!("github.com/{repo}")}));
            }
            cx.emit(crate::app_state::AppEvent::Shell);
        });
    }
    if let Some(thread) = &options.thread {
        state.update(&mut cx, |state, cx| state.select_thread(thread, cx));
        let loaded = state.clone();
        pump(&mut cx, options.wait, move |cx| loaded.read_with(cx, |state, _| state.thread.projection.is_some()));
    }
    if std::env::args().any(|arg| arg == "--no-command-output-streaming") {
        state.update(&mut cx, |state, _| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.environment.capabilities.command_output_streaming = Some(false);
            }
        });
    }
    if let Some(path) = std::env::args().collect::<Vec<_>>().windows(2).find(|pair| pair[0] == "--markdown-file-fixture").map(|pair| pair[1].clone()) {
        let fixture: crate::files::MarkdownFixture = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let path = fixture.path.clone();
        cx.update(|cx| cx.set_global(fixture));
        state.update(&mut cx, |state, cx| state.open_file(&path, None, cx));
    }
    if let Some(path) = std::env::args().collect::<Vec<_>>().windows(2).find(|pair| pair[0] == "--question-request-fixture").map(|pair| pair[1].clone()) {
        let fixture: serde_json::Value = serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        state.update(&mut cx, |state, cx| {
            state.freeze_snapshot();
            if let Some(supports) = fixture["questionAttachments"].as_bool() && let Some(config) = state.server_config.config.as_mut() { config.environment.capabilities.question_attachments = Some(supports); }
            if let Some(projection) = state.thread.projection.as_mut() {
                projection.runtime_requests = serde_json::from_value(fixture["runtimeRequests"].clone()).expect("fixture requests");
                projection.turn_items = serde_json::from_value(fixture["turnItems"].clone()).expect("fixture items");
                cx.emit(crate::app_state::AppEvent::Thread);
            }
        });
    }
    if let Some(path) = std::env::args().collect::<Vec<_>>().windows(2).find(|pair| pair[0] == "--question-command-output").map(|pair| pair[1].clone()) {
        cx.update(|cx| cx.set_global(crate::composer::QuestionCommandSpy { output:PathBuf::from(path), count:Default::default() }));
    }
    if let Some(path) = std::env::args().collect::<Vec<_>>().windows(2).find(|pair| pair[0] == "--question-staged-fixture").map(|pair| pair[1].clone()) {
        let fixture: serde_json::Value = serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        cx.update(|cx| crate::composer::snapshot_composer(cx).update(cx, |composer, cx| composer.seed_question_fixture(&fixture, cx)));
    }
    if let Some(path) = &options.turn_items_fixture {
        let items: Vec<t3_core::thread::ProjectedTurnItem> = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        state.update(&mut cx, |state, cx| {
            // Freeze only this renderer's projection; fixture items never reach the server.
            state.freeze_snapshot();
            if let Some(projection) = state.thread.projection.as_mut() {
                projection.turn_items = items.iter().map(|row| row.item.clone()).collect();
                projection.visible_turn_items = items;
                cx.emit(crate::app_state::AppEvent::Thread);
            }
        });
    }
    if let Some(path) = &options.project_grouping_fixture {
        let configured = state.clone();
        pump(&mut cx, options.wait, move |cx| configured.read_with(cx, |state, _| state.server_config.config.is_some()));
        let projects: Vec<t3_core::shell::ProjectShell> = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        state.update(&mut cx, |state, cx| {
            // Freeze this renderer's project list; fixture checkouts never reach the server.
            state.stop_pump();
            if let Some(snapshot) = state.shell.snapshot.as_mut() { snapshot.projects = projects; }
            state.rebuild_sidebar(cx);
            cx.emit(crate::app_state::AppEvent::Shell);
        });
    }
    if let Some(path) = &options.prompt_link_fixture {
        let configured = state.clone();
        pump(&mut cx, options.wait, move |cx| configured.read_with(cx, |state, _| state.server_config.config.is_some()));
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Fixture {
            thread: String,
            hidden: bool,
            pull_request: Option<String>,
            message: Option<String>,
            context: Option<serde_json::Value>,
            environment_id: Option<String>,
        }
        let fixture: Fixture = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        // Mutates only this renderer's in-memory projection, never the server or saved settings.
        let link = fixture
            .pull_request
            .as_deref()
            .map(|pr| {
                let pr = t3_core::prompt_links::parse_pull_request(pr).ok_or("invalid fixture pull request")?;
                serde_json::from_value(serde_json::json!({
                    "host":"github.com", "repository":format!("{}/{}",pr.repository.owner,pr.repository.name),
                    "number":pr.number,"url":pr.url(),"source":"manual","linkedAt":"2026-01-01T00:00:00.000Z"
                }))
                .map_err(|e| e.to_string())
            })
            .transpose()?;
        state.update(&mut cx, |state, cx| {
            // Hold this snapshot fixture against later repository-enrichment frames.
            state.stop_pump();
            if let Some(id) = fixture.environment_id && let Some(config) = state.server_config.config.as_mut() { config.environment.environment_id=id; }
            if let Some(thread) = state
                .shell
                .snapshot
                .as_mut()
                .and_then(|s| s.threads.iter_mut().find(|t| t.id == fixture.thread))
            {
                if fixture.hidden {
                    thread.tags.push("prompt-link-fixture".into());
                }
                if let Some(link) = link {
                    thread.pull_requests = vec![link];
                }
            }
            if fixture.hidden
                && let Some(config) = state.server_config.config.as_mut()
            {
                config.settings.hide_tagged_threads = true;
                config.settings.hidden_thread_tags = vec!["prompt-link-fixture".into()];
            }
            if let Some(message) = fixture.message
                && let Some(projection) = state.thread.projection.as_mut().filter(|p| p.thread.id == fixture.thread)
                && let Some(item) = projection
                    .visible_turn_items
                    .iter_mut()
                    .rev()
                    .find(|i| matches!(i.item.kind, t3_core::turn_item::TurnItemKind::UserMessage { .. }))
                && let t3_core::turn_item::TurnItemKind::UserMessage { text, context, .. } = &mut Arc::make_mut(&mut item.item).kind
            {
                *text = message;
                *context = fixture.context;
            }
            state.rebuild_sidebar(cx);
            cx.emit(crate::app_state::AppEvent::Shell);
            cx.emit(crate::app_state::AppEvent::Thread);
        });
    }
    if let Some(path) = &options.worktree_setup {
        let text = std::fs::read_to_string(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        let snapshot = t3_core::worktree_setup::decode_event(value).map_err(|error| error.to_string())?.ok_or("the worktree setup file holds null")?;
        let store = cx.update(|cx| crate::worktree_setup::store(&state, cx));
        store.update(&mut cx, |store, cx| store.show_fixture(snapshot, cx));
    }
    if let Some(path) = std::env::var_os("T3_NATIVE_IMPORT_THEME_FIXTURE") {
        cx.update(|cx|crate::settings::import_theme_fixture(std::path::Path::new(&path), &state, cx))?;
    }
    if let Some(path) = std::env::var_os("T3_NATIVE_ENVIRONMENT_THEMES_FIXTURE") {
        let text=std::fs::read_to_string(path).map_err(|e|e.to_string())?;
        let values:Vec<serde_json::Value>=serde_json::from_str(&text).map_err(|e|e.to_string())?;
        let definitions=std::thread::spawn(move||t3_core::themes::environment(&values)).join().map_err(|_|"theme fixture worker failed")?;
        cx.update(|cx|crate::theme::set_environment_themes(&definitions,cx));
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
    if let Some(path) = &options.updates_fixture {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        cx.update_window(window, |_, window, cx| crate::updates::show_fixture(value, window, cx)).map_err(|e| e.to_string())?;
    }
    if let Some(path) = &options.resource_telemetry_fixture {
        let fixture: crate::settings::diagnostics::resource_monitor::Fixture = serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        cx.update(|cx| cx.set_global(fixture));
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
    if let Some(path)=std::env::var_os("T3_NATIVE_THEME_EDITOR_FIXTURE") {
        let value:serde_json::Value=serde_json::from_slice(&std::fs::read(path).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        let pending_exit=value.get("pendingExit").and_then(serde_json::Value::as_str).map(str::to_owned);
        cx.update_window(window,|_,window,cx|crate::theme_editor::fixture(value,window,cx)).map_err(|e|e.to_string())?;
        if let Some(exit)=pending_exit {
            // Settle save receipts before delivering the queued preview frame.
            cx.run_until_parked();
            cx.update(|cx|crate::theme_editor::verify_pending_exit_retired(&exit,cx))?;
        }
    }
    if let Some(path)=std::env::var_os("T3_NATIVE_THEME_IMPORT_FIXTURE") {
        pump(&mut cx,1.,|_|false);
        let value:serde_json::Value=serde_json::from_slice(&std::fs::read(path).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        cx.update_window(window,|_,window,cx|crate::settings::theme_import::fixture(value.clone(),window,cx)).map_err(|e|e.to_string())??;
        pump(&mut cx,options.wait,|_|false);
        if let Some(paths)=value["dropPaths"].as_array() {
            let paths=gpui::ExternalPaths(paths.iter().filter_map(|p|p.as_str().map(PathBuf::from)).collect());
            let position=point(px(value["dropPosition"][0].as_f64().unwrap_or(720.) as f32),px(value["dropPosition"][1].as_f64().unwrap_or(330.) as f32));
            cx.simulate_event(window,gpui::FileDropEvent::Entered {position,paths});
            cx.simulate_event(window,gpui::FileDropEvent::Submit {position});
            pump(&mut cx,options.wait,|_|false);
        }
    }
    if let Some(path)=std::env::var_os("T3_NATIVE_THEME_REIMPORT_FIXTURE") {
        pump(&mut cx,1.,|_|false);
        let json=std::fs::read_to_string(path).map_err(|e|e.to_string())?;
        cx.update_window(window,|_,window,cx|crate::settings::theme_import::fixture(serde_json::json!({"json":json,"submit":true}),window,cx)).map_err(|e|e.to_string())??;
        pump(&mut cx,options.wait,|_|false);
    }
    if options.pull_requests || options.pull_request.is_some() {
        if let Some(number) = options.pull_request {
            cx.update(|cx| cx.set_global(crate::pull_requests::SnapshotSelection { number, code: options.pull_request_code }));
        }
        state.update(&mut cx, |state, cx| state.set_pull_requests_open(true, cx));
        pump(&mut cx, options.wait, |_| false);
    }
    if let Some(step) = options.welcome {
        cx.update(|cx| crate::onboarding::open(step, cx));
        pump(&mut cx, options.wait.min(3.), |_| false);
    }
    if let Some(path) = &options.welcome_terminal_fixture {
        let fixture = serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        cx.update_window(window, |_, window, cx| crate::onboarding::terminal_fixture(fixture, window, cx)).map_err(|error| error.to_string())??;
        pump(&mut cx, options.wait.min(3.), |_| false);
    }
    if options.composer_audit.is_some() || options.workspace_availability_fixture {cx.update(|cx| cx.set_global(crate::composer::SnapshotAudit::default()));}
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
    if let Some(path) = &options.draft_workspace_fixture {
        let mut threads: Vec<t3_core::shell::ThreadShell> = serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        state.update(&mut cx, |state, cx| {
            state.freeze_snapshot();
            for thread in &mut threads {
                if thread.project_id == "$draft" {thread.project_id = state.draft_project.clone().unwrap_or_default();}
            }
            if let Some(shell) = state.shell.snapshot.as_mut() { shell.threads = threads; }
            cx.emit(crate::app_state::AppEvent::Shell);
        });
    }
    if options.workspace_availability_fixture {
        cx.update_window(window, |_, window, cx| crate::composer::snapshot_composer(cx).update(cx, |composer, cx| composer.verify_workspace_availability(window, cx))).map_err(|error| error.to_string())??;
    }
    if options.recovery_fixture {
        cx.update_window(window, |_, window, cx| crate::composer::snapshot_composer(cx).update(cx, |composer, cx| composer.verify_failed_launch_recovery(window, cx))).map_err(|error| error.to_string())??;
    }
    if let Some(prompt) = &options.prompt {
        cx.simulate_input(window, prompt);
    }
    if let Some(url) = &options.open_url {
        state.update(&mut cx, |state, cx| state.open_prompt_url(url.clone(), cx));
        pump(&mut cx, 1., |_| false);
    }
    if options.palette {
        // Actions bubble from the focused element; an offscreen window starts with none.
        cx.update_window(window, |_, window, cx| {
            window.focus_next(cx);
            window.dispatch_action(Box::new(crate::keymap::ToggleCommandPalette), cx);
        })
        .map_err(|error| error.to_string())?;
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
        struct Fixture { identity: String, environments: Vec<t3_core::relay::Environment> }
        let fixture: Fixture = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        state.update(&mut cx, |state, cx| {
            state.cloud.busy = false;
            state.cloud.identity = Some(fixture.identity);
            state.cloud.discovery = t3_core::relay::Discovery { listed: true, environments: fixture.environments, ..Default::default() };
            cx.emit(crate::app_state::AppEvent::Phase);
        });
    }
    if let (Some(document), Some(text)) = (&document_view, document_fixture.as_ref().and_then(|fixture| fixture.after_text.as_ref())) {
        document.update(&mut cx, |document, cx| { document.text = text.clone().into(); cx.notify(); });
    }
    if let Some(scenario) = std::env::args().collect::<Vec<_>>().windows(2).find(|pair| pair[0] == "--star-prompt-fixture").map(|pair| pair[1].clone()) {
        state.update(&mut cx, |state, cx| state.install_star_fixture(&scenario, cx));
    }
    if let Some(key) = std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|pair| pair[0] == "--star-keyboard-audit")
        .map(|pair| pair[1].clone())
    {
        use t3_core::github_star_prompt::Stage;
        pump(&mut cx, 0.1, |_| false);
        cx.update_window(window, |_, window, cx| {
            let focus = cx
                .global::<FocusTargets>()
                .0
                .get("star-activate")
                .expect("star action rendered")
                .clone();
            window.focus(&focus, cx);
        })
        .map_err(|e| e.to_string())?;
        pump(&mut cx, 0.1, |_| false);
        let (key, held) = key
            .strip_suffix("-held")
            .map_or((key.as_str(), false), |key| (key, true));
        let stroke = gpui::Keystroke::parse(key).map_err(|e| e.to_string())?;
        for is_held in
            std::iter::once(false).chain(std::iter::repeat_n(true, if held { 3 } else { 0 }))
        {
            cx.simulate_event(
                window,
                gpui::KeyDownEvent {
                    keystroke: stroke.clone(),
                    is_held,
                    prefer_character_input: false,
                },
            );
            pump(&mut cx, 0.05, |_| false);
        }
        cx.simulate_event(
            window,
            gpui::KeyUpEvent {
                keystroke: stroke.clone(),
            },
        );
        pump(&mut cx, 0.05, |_| false);
        let first = state.read_with(&cx, |state, _| state.star_fixture_audit());
        eprintln!(
            "star keyboard audit: {key}, held={held}, first gesture={first:?}; pre-RPC/no-URL spies only"
        );
        assert_eq!(
            first,
            (Stage::Fallback, 1, 0),
            "one physical key gesture must star once and leave fallback for a separate gesture"
        );
        cx.simulate_event(
            window,
            gpui::KeyUpEvent {
                keystroke: stroke.clone(),
            },
        );
        pump(&mut cx, 0.05, |_| false);
        assert_eq!(
            state.read_with(&cx, |state, _| state.star_fixture_audit()),
            first,
            "an extra release cannot activate fallback"
        );
        cx.simulate_event(
            window,
            gpui::KeyDownEvent {
                keystroke: stroke.clone(),
                is_held: false,
                prefer_character_input: false,
            },
        );
        cx.simulate_event(window, gpui::KeyUpEvent { keystroke: stroke });
        pump(&mut cx, 0.05, |_| false);
        let second = state.read_with(&cx, |state, _| state.star_fixture_audit());
        eprintln!("star keyboard audit: separate fallback gesture={second:?}; no URL opened");
        assert_eq!(second, (Stage::Suppressed, 1, 1));
    }
    for (index, gesture) in options.gestures.iter().enumerate() {
        pump(&mut cx, options.wait.min(options.gesture_wait), |_| false);
        if index + 1 == options.gestures.len() && options.render_counts_output.is_some() {
            cx.update(|cx| cx.global::<RenderCounts>().0.borrow_mut().clear());
        }
        replay(&mut cx, window, gesture);
        if index + 1 == options.gestures.len() && let Some(directory) = &options.motion_frames {
            std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
            for frame in 0..8 {
                pump(&mut cx, 0.034, |_| false);
                let image = cx.capture_screenshot(window).map_err(|error| error.to_string())?;
                image.save(directory.join(format!("{frame:02}.png"))).map_err(|error| error.to_string())?;
            }
        }
    }
    pump(&mut cx, options.wait.min(3.), |_| false);
    if let Some(path) = &options.composer_audit {
        let value = cx.update(|cx| {
            let submissions = cx.global::<crate::composer::SnapshotAudit>().submissions.get();
            crate::composer::snapshot_composer(cx).read(cx).snapshot_audit(submissions, cx)
        });
        std::fs::write(path, serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        if value["submissions"] != 0 {return Err("workspace popup dispatched a submission".into());}
    }
    if let Some(path) = &options.render_counts_output {
        let counts = cx.update(|cx| serde_json::to_vec_pretty(&*cx.global::<RenderCounts>().0.borrow())).map_err(|error| error.to_string())?;
        std::fs::write(path, counts).map_err(|error| error.to_string())?;
    }
    if options.environment_attach_fixture {
        state.update(&mut cx, |state, cx| {
            state.stop_pump();
            let client = state.client_handle().expect("fixture needs the read-only smoke connection");
            let mut shell = state.shell.clone();
            let snapshot = shell.snapshot.as_mut().expect("cached shell");
            let project = snapshot.projects.first().expect("smoke project").id.clone();
            snapshot.older_settled = Some(t3_core::shell::OlderSettledCut {
                before: "2026-01-01T00:00:00.000Z".into(), as_of_sequence: snapshot.snapshot_sequence,
                counts: vec![t3_core::shell::OlderSettledCount { project_id: project.clone(), count: 6 }],
            });
            let config = state.server_config.clone();
            state.clear_connection_state(cx);
            state.install_active_environment(client, shell, config, t3_client::supervisor::ConnectionStatus::Connected, cx);
            let older = state.omitted.older().expect("attaching an idle cached shell initializes older-settled counts");
            assert_eq!(older.counts.get(&project), Some(&6));
            assert!(state.omitted.pages.frontier.is_some(), "attaching initializes the paging frontier without another shell frame");
            eprintln!("cached attach fixture: older-settled count=6; frontier initialized; no additional shell frame");
        });
    }
    if let Some(visibility) = &options.notification_fixture {
        if !matches!(visibility.as_str(), "visible" | "hidden" | "background") { return Err("notification fixture must be visible, hidden or background".into()); }
        state.update(&mut cx, |state, cx| {
            state.stop_pump();
            // This exercises toast retirement without requesting OS delivery or playing audio.
            state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Success, "Thread completed")
                .description(if visibility == "background" { "Background environment thread" } else { "Notification fixture" })
                .open_environment_thread(t3_core::sidebar::tag_filter::ThreadKey::new(if visibility == "background" { "fixture-background" } else { state.environments.active.as_deref().unwrap_or("local") }, "snapshot-notification")), cx);
            if visibility == "hidden" {
                state.sidebar.hidden_thread_ids.insert("snapshot-notification".into());
                state.retire_hidden_notifications(cx);
            }
        });
    }
    if media_audit.is_some() && std::env::args().any(|arg| arg == "--media-select-all") {
        let view = cx.update(|cx| {
            let audit = cx.global::<MediaAudit>();
            let id = audit.sources.borrow().keys().next().cloned().expect("media prose state");
            audit.views.borrow().get(&id).and_then(gpui::WeakEntity::upgrade).expect("live media prose state")
        });
        view.update(&mut cx, |state, cx| state.select_all(cx));
    }
    cx.update_window(window, |_, window, _| window.refresh()).map_err(|error| error.to_string())?;
    pump(&mut cx, 0.5, |_| false);
    let mut media_idle = Vec::new();
    if media_audit.is_some() {
        let mut frames = gpui::FrameTimingCollector::new();
        let profile_prefix = std::env::args().collect::<Vec<_>>().windows(2).find(|pair| pair[0] == "--media-profile-prefix").map(|pair| pair[1].clone());
        media_profile_checkpoint(profile_prefix.as_deref(), 0)?;
        for phase in 1..=3 {
            let before: usize = cx.update(|cx| cx.global::<MediaAudit>().notifications.borrow().values().sum());
            pump(&mut cx, 0.5, |_| false);
            let notifications: usize = cx.update(|cx| cx.global::<MediaAudit>().notifications.borrow().values().sum::<usize>() - before);
            media_idle.push(serde_json::json!({"notifications":notifications,"windowDraws":frames.collect_unseen().len(),"selected":cx.update(|cx| media_selections(cx))}));
            media_profile_checkpoint(profile_prefix.as_deref(), phase)?;
        }
    }
    if let Some(path)=std::env::var_os("T3_NATIVE_THEME_EDITOR_FIXTURE") {
        let value:serde_json::Value=serde_json::from_slice(&std::fs::read(path).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        if let Some(exit)=value.get("pendingExit").and_then(serde_json::Value::as_str) {
            cx.update(|cx|crate::theme_editor::verify_pending_exit_fixture(exit,cx))?;
        }
    }
    if let Some(path) = media_audit {
        let values = cx.update(|cx| serde_json::to_vec_pretty(&serde_json::json!({"prose":&*cx.global::<MediaAudit>().sources.borrow(),"notifications":&*cx.global::<MediaAudit>().notifications.borrow(),"entities":&*cx.global::<MediaAudit>().pointers.borrow(),"idleSamples":media_idle,"selected":media_selections(cx),"cache":cx.try_global::<crate::media::Media>().map(|media|media.cache.read(cx).audit())}))).map_err(|e| e.to_string())?;
        std::fs::write(path, values).map_err(|e| e.to_string())?;
    }
    cx.update_window(window, |_, window, _| window.refresh()).map_err(|error| error.to_string())?;
    let image = cx.capture_screenshot(window).map_err(|error| error.to_string())?;
    if let Some(parent) = options.out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if let Some(path) = &options.clipboard_output {
        let link = cx.update(|cx| cx.try_global::<crate::composer::prompt_links::SnapshotClipboard>().and_then(|s| s.0.clone())).ok_or("no prompt link was copied")?;
        std::fs::write(path, link).map_err(|e| e.to_string())?;
    }
    cx.update(|cx| crate::transcript::html_render::fixture::capture(&options.out, cx))?;
    image.save(&options.out).map_err(|error| error.to_string())

}
