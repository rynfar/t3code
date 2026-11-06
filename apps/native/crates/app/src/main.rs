mod app_state;
mod announcements;
mod components;
mod chat_header;
mod composer;
mod connect;
mod context_menu;
mod context_chip;
mod device;
mod dialog;
mod guest_share;
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
mod chatgpt_setup;
mod palette;
mod preview;
mod project_files;
mod project_icon;
mod prompt_links;
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
mod theme_editor;
mod thread_details;
mod thread_view;
mod toast;
mod github_star_prompt;
mod timestamp;
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

    let (urls, mut incoming) = futures::channel::mpsc::unbounded::<String>();
    for url in command_line_urls() {
        let _ = urls.unbounded_send(url);
    }
    let application = gpui_platform::application().with_assets(icons::Assets);
    application.on_open_urls(move |links| {
        for link in links { let _ = urls.unbounded_send(link); }
    });
    application.run(move |cx: &mut App| {
        let state = boot(handle.clone(), cx);
        let receiver_state = state.clone();
        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while let Some(url) = incoming.next().await {
                receiver_state.update(cx, |state, cx| state.open_prompt_url(url, cx));
            }
        })
        .detach();
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
    theme_editor::init(state.clone(), cx);
    usage::init(state.clone(), cx);
    new_thread::init(state.clone(), cx);
    right_panel::init(state.clone(), cx);
    preview::init(state.clone(), cx);
    device::init(state.clone(), cx);
    pull_requests::init(state.clone(), cx);
    terminal::init(cx);
    files::init(cx);
    selectable_text::init(cx);
    thread_view::init(cx);
    onboarding::init(cx);
    chatgpt_setup::init(cx);
    updates::init(cx);
    state
}

fn root_view(state: &Entity<app_state::AppState>, window: &mut Window, cx: &mut App) -> Entity<Root> {
    theme::follow_system_appearance(window, cx);
    let view = cx.new(|cx| workspace::Workspace::new(state.clone(), window, cx));
    cx.new(|cx| Root::new(view, window, cx))
}

fn command_line_urls() -> Vec<String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    args.iter()
        .enumerate()
        .filter(|(i, arg)| (*i > 0 && args[*i - 1] == "--open-url") || arg.starts_with("t3code-native://") || arg.starts_with("https://app.t3.codes/"))
        .map(|(_, arg)| arg.clone())
        .collect()
}
