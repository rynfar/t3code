mod app_state;
mod chat_header;
mod composer;
mod connect;
mod context_menu;
mod device;
mod dialog;
mod diff;
mod environments;
mod cloud;
mod files;
mod git;
mod glass;
mod home;
mod icons;
mod keymap;
mod media;
mod menus;
mod new_thread;
mod omitted_rows;
mod notifications;
mod onboarding;
mod palette;
mod preview;
mod project_files;
mod projects;
mod provider_icon;
mod pull_requests;
mod quit_hold;
mod right_panel;
mod selectable_text;
mod settings;
mod side_chat;
#[cfg(feature = "snapshot")]
mod snapshot;
mod sidebar;
mod lineage;
mod snooze_dialog;
mod ssh_prompt;
mod terminal;
mod theme;
mod thread_details;
mod thread_view;
mod toast;
mod transcript;
mod usage;
mod updates;
mod webview;
mod workspace;
mod worktree_setup;

use gpui::{App, Bounds, Entity, TitlebarOptions, Window, WindowBounds, WindowOptions, point, prelude::*, px, size};
use gpui_component::Root;

#[cfg(target_os = "macos")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("T3_NATIVE_LOG")
                .unwrap_or_else(|_| "warn,t3_native=info,t3_client=info".into()),
        )
        .init();

    // The network side runs on its own small runtime; the UI awaits its channels.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("t3-net")
        .enable_all()
        .build()
        .expect("tokio runtime");
    let handle = runtime.handle().clone();
    std::mem::forget(runtime);

    #[cfg(feature = "snapshot")]
    if let Some(options) = snapshot::Options::from_args() {
        snapshot::run(options, handle);
        return;
    }

    gpui_platform::application().with_assets(icons::Assets).run(move |cx: &mut App| {
        let state = boot(handle.clone(), cx);
        cx.on_system_notification_response({
            let state = state.downgrade();
            move |response, cx| {
                let _ = state.update(cx, |state, cx| state.open_notification(&response.tag, cx));
            }
        });
        let bounds = Bounds::centered(None, size(px(1280.), px(840.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("T3 Code".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(14.), px(14.))),
                }),
                window_min_size: Some(size(px(720.), px(480.))),
                ..Default::default()
            },
            |window, cx| root_view(&state, window, cx),
        )
        .expect("open window");
        cx.activate(true);
    });
}

/// The app's globals, actions and state; shared by the window and the offscreen snapshot.
fn boot(runtime: tokio::runtime::Handle, cx: &mut App) -> Entity<app_state::AppState> {
    gpui_component::init(cx);
    theme::init(cx);
    composer::init(cx);
    keymap::init(cx);
    let state = cx.new(|cx| app_state::AppState::new(runtime, cx));
    menus::init(state.clone(), cx);
    settings::init(state.clone(), cx);
    usage::init(state.clone(), cx);
    new_thread::init(state.clone(), cx);
    right_panel::init(state.clone(), cx);
    preview::init(state.clone(), cx);
    device::init(state.clone(), cx);
    pull_requests::init(state.clone(), cx);
    terminal::init(cx);
    files::init(cx);
    selectable_text::init(cx);
    onboarding::init(cx);
    updates::init(cx);
    state
}

fn root_view(state: &Entity<app_state::AppState>, window: &mut Window, cx: &mut App) -> Entity<Root> {
    theme::follow_system_appearance(window, cx);
    let view = cx.new(|cx| workspace::Workspace::new(state.clone(), window, cx));
    cx.new(|cx| Root::new(view, window, cx))
}
