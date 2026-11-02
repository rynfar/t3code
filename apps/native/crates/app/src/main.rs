mod app_state;
mod composer;
mod connect;
mod context_menu;
mod dialog;
mod diff;
mod glass;
mod icons;
mod keymap;
mod menus;
mod new_thread;
mod palette;
mod right_panel;
mod sidebar;
mod snooze_dialog;
mod terminal;
mod theme;
mod thread_view;
mod toast;
mod transcript;
mod workspace;

use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, prelude::*, px, size};
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

    gpui_platform::application().with_assets(icons::Assets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        theme::init(cx);
        composer::init(cx);
        keymap::init(cx);
        let state = cx.new(|cx| app_state::AppState::new(handle.clone(), cx));
        menus::init(state.clone(), cx);
        new_thread::init(state.clone(), cx);
        right_panel::init(state.clone(), cx);
        terminal::init(cx);
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
            |window, cx| {
                theme::follow_system_appearance(window, cx);
                let view = cx.new(|cx| workspace::Workspace::new(state.clone(), window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("open window");
        cx.activate(true);
    });
}
