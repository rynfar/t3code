//! `DiffsSettingsPanel` / `DiffColorsSettingsRow` (`SettingsPanels.tsx`), `BrowserSettingsPanel`
//! (`IntegrationsSettings.tsx`, with `BrowserImportWizard.tsx`) and `SnapShotSettings.tsx`.
//! Server-streamed browser preferences and macOS SnapShot capture.

use std::collections::HashMap;

use gpui::{
    App, AppContext, Context, ElementId, Entity, Focusable, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use t3_core::client_settings::{
    BrowserLinkTarget, BrowserSearchEngine, ClientSettingChange, ClientSettingsDoc, DiffColorScheme, valid_search_template,
};
use t3_core::diff::Layout;
use t3_core::preview::viewport::{
    AppearancePreference, BROWSER_PROFILE_MAX_COUNT, BROWSER_PROFILE_NAME_MAX_LENGTH,
    BrowserProfile, BrowserProfileKind, DEFAULT_BROWSER_PROFILE_ID, RESPONSIVE_SEED_SIZE,
    VIEWPORT_PRESETS, ViewportSetting, find_viewport_preset, is_built_in_browser_profile_id,
    next_unique_profile_name, preset_setting, report_profiles_payload, rotate_viewport, validate_viewport_dimensions,
    viewport_select_value, zoom_label,
};
use t3_core::preview::{DEFAULT_ZOOM, ZOOM_LEVELS, method as preview_method};
use t3_core::settings::ServerSettingChange;
use t3_core::sidebar::grouping::EnvironmentOwner;
use t3_core::settings::snapshot::{
    SnapShotBackendState, SnapShotMode, SnapShotModifier, SnapShotShortcut, SnapShotSound,
    SnapShotSoundSelection, same_snap_shot_shortcut, snap_shot_description,
    snap_shot_setup_button_label, snap_shot_setup_complete, snap_shot_shortcut_status,
    snap_shot_status,
};

use super::controls::{
    Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, muted_text, r, section, unavailable_group,
};
use super::{Block, SettingsView, store};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::snapshot_capture::setup_dialog::{
    render_shortcut_keycaps, shortcut_conflict_message, shortcut_from_keystroke,
};
use crate::theme::{ThemeSlot, ThemeSlotExt, hsla, palette, theme_slot_clipped_background_layer};
use crate::toast::{Toast, ToastKind};

const MAC: bool = cfg!(target_os = "macos");
const BROWSER_COOKIE_IMPORT_UNAVAILABLE: &str =
    "Importing browser cookies from installed desktop browsers requires the Electron desktop app.";
const RECORDING_OVERLAYS_UNAVAILABLE: &str =
    "Input overlays are available in Electron browser recordings only.";
const CLEAR_PROFILE_UNAVAILABLE: &str =
    "Connect to an environment to clear profile data";
const AUTO_SHOW_FLOATING_PREVIEW: &str = "browserAutoShowFloatingPreview";
const RECORDING_FRAME_RATE: &str = "browserRecordingFrameRate";
const DEFAULT_RECORDING_FRAME_RATE: u64 = 30;
const ADVANCED: &[&str] = &[
    "browser-default-viewport",
    "browser-default-zoom",
    "browser-default-appearance",
];

#[derive(Default)]
struct SnapShotPageState {
    candidate: Option<SnapShotShortcut>,
    recording: bool,
    sound_menu: bool,
}

pub(super) struct BrowserState {
    pub(super) custom_url: Entity<InputState>,
    viewport_width: Entity<InputState>,
    viewport_height: Entity<InputState>,
    profile_inputs: HashMap<String, (Entity<InputState>, Subscription)>,
    profile_menu: Option<String>,
    clearing_profile: bool,
    advanced: bool,
    snapshot: SnapShotPageState,
    _subscriptions: Vec<Subscription>,
}

impl BrowserState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let custom_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com/search?q=%s"));
        let viewport_width = cx.new(|cx| InputState::new(window, cx));
        let viewport_height = cx.new(|cx| InputState::new(window, cx));
        let url_sub =
            cx.subscribe_in(&custom_url, window, |view, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let next = view.browser.custom_url.read(cx).value().to_string();
                    store::update_client(
                        &view.state,
                        ClientSettingChange::BrowserCustomSearchUrl(next),
                        cx,
                    );
                }
            });
        let width_sub =
            cx.subscribe_in(&viewport_width, window, |view, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    view.commit_browser_viewport_dimensions(window, cx);
                }
            });
        let height_sub =
            cx.subscribe_in(&viewport_height, window, |view, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    view.commit_browser_viewport_dimensions(window, cx);
                }
            });
        let view = cx.entity().downgrade();
        let snap_key_sub = cx.intercept_keystrokes(move |event, _, cx| {
            let _ = view.update(cx, |view, cx| {
                if !view.browser.snapshot.recording {
                    return;
                }
                let setup_open = crate::snapshot_capture::active_setup_dialog(cx)
                    .is_some_and(|dialog| dialog.read(cx).is_open());
                if view.section != t3_core::settings::search::Section::SnapShot || setup_open {
                    view.stop_snapshot_recording(cx);
                    return;
                }
                if event.keystroke.key == "tab" {
                    return;
                }
                cx.stop_propagation();
                if event.keystroke.key == "escape" {
                    view.stop_snapshot_recording(cx);
                    return;
                }
                if let Some(shortcut) = shortcut_from_keystroke(&event.keystroke, cx) {
                    view.browser.snapshot.candidate = Some(shortcut);
                    view.stop_snapshot_recording(cx);
                }
            });
        });
        let self_sub = cx.observe_self(|view, cx| {
            if view.section != t3_core::settings::search::Section::Browser {
                view.browser.profile_menu = None;
            }
            if view.section != t3_core::settings::search::Section::SnapShot {
                view.browser.snapshot.sound_menu = false;
            }
            if view.browser.snapshot.recording
                && view.section != t3_core::settings::search::Section::SnapShot
            {
                view.stop_snapshot_recording(cx);
            }
        });
        let view = cx.entity().downgrade();
        let dismiss_menu = cx.intercept_keystrokes(move |event, _, cx| {
            if event.keystroke.key != "escape" || crate::dialog::is_open(cx) {
                return;
            }
            let _ = view.update(cx, |view, cx| {
                if view.browser.profile_menu.take().is_some()
                    || std::mem::take(&mut view.browser.snapshot.sound_menu)
                {
                    cx.stop_propagation();
                    cx.notify();
                }
            });
        });
        let release_sub = cx.on_release(|view: &mut SettingsView, cx| {
            if view.browser.snapshot.recording {
                view.browser.snapshot.recording = false;
                if let Some(coordinator) = crate::snapshot_capture::active_coordinator(cx) {
                    coordinator.update(cx, |c, cx| c.set_shortcut_suppressed(false, cx));
                }
            }
        });
        let mut subscriptions = vec![url_sub, width_sub, height_sub, snap_key_sub, self_sub, dismiss_menu, release_sub];
        if let Some(coordinator) = crate::snapshot_capture::active_coordinator(cx) {
            subscriptions.push(cx.observe(&coordinator, |view, _, cx| {
                if view.section == t3_core::settings::search::Section::SnapShot {
                    cx.notify();
                }
            }));
        }
        if let Some(dialog) = crate::snapshot_capture::active_setup_dialog(cx) {
            subscriptions.push(cx.observe(&dialog, |view, dialog, cx| {
                if dialog.read(cx).is_open() && view.browser.snapshot.recording {
                    view.stop_snapshot_recording(cx);
                }
                if view.section == t3_core::settings::search::Section::SnapShot {
                    cx.notify();
                }
            }));
        }
        Self {
            custom_url,
            viewport_width,
            viewport_height,
            profile_inputs: HashMap::new(),
            profile_menu: None,
            clearing_profile: false,
            advanced: false,
            snapshot: SnapShotPageState::default(),
            _subscriptions: subscriptions,
        }
    }
}

fn report_browser_profiles(cx: &mut App) {
    if let Some(previews) = crate::preview::previews(cx) {
        previews.update(cx, |previews, cx| previews.report_profiles_if_needed(cx));
    }
}

fn viewport_trigger_label(viewport: &ViewportSetting, cx: &App) -> SharedString {
    match viewport_select_value(viewport) {
        "fill" => crate::i18n::t(cx, "Fill panel").to_owned().into(),
        "responsive" => crate::i18n::t(cx, "Responsive").to_owned().into(),
        preset_id => find_viewport_preset(preset_id)
            .map(|preset| SharedString::from(preset.label))
            .unwrap_or_else(|| crate::i18n::t(cx, "Responsive").to_owned().into()),
    }
}

fn zoom_value_key(level: f64) -> String {
    format!("{level}")
}

fn profile_clear_targets(state: &AppState) -> Option<Vec<EnvironmentOwner>> {
    use t3_client::supervisor::ConnectionStatus;

    let mut owners = Vec::new();
    for entry in &state.fleet.environments {
        if !entry.config.config.as_ref().is_some_and(|config| {
            config.environment.capabilities.server_browser == Some(true)
        }) {
            continue;
        }
        if entry.status != ConnectionStatus::Connected
            || !entry.summary.synchronized
            || !entry.grants.as_ref().is_some_and(|grants| grants.grants("preview:operate"))
        {
            return None;
        }
        owners.push(entry.summary.owner.clone());
    }
    (!owners.is_empty()).then_some(owners)
}

fn client_preference(key: &str, value: impl Into<serde_json::Value>) -> ClientSettingChange {
    ClientSettingChange::ThemePatch([(key.to_owned(), value.into())].into_iter().collect())
}

pub(crate) fn browser_auto_show_floating_preview(doc: &ClientSettingsDoc) -> bool {
    doc.entries()
        .get(AUTO_SHOW_FLOATING_PREVIEW)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

// Device events already produce a docked surface; the device controller can float it in place.
#[allow(dead_code)]
pub(crate) fn maybe_float_agent_device(
    devices: &mut crate::device::Devices,
    opened: &t3_core::device::AgentOpenedSession,
    cx: &mut Context<crate::device::Devices>,
) {
    if !store::try_client(cx).is_some_and(browser_auto_show_floating_preview) {
        return;
    }
    let Some(device) = devices.service()
        .and_then(|service| service.device(&opened.host_id, &opened.device_id)).cloned()
    else { return };
    let surface = t3_core::right_panel::Surface::device(Some(t3_core::right_panel::DeviceTarget {
        host_id: opened.host_id.clone(),
        device_id: opened.device_id.clone(),
        platform: opened.platform,
        name: opened.name.clone(),
    }));
    devices.float_device(&opened.thread_id, &surface.id, device, cx);
}

fn recording_frame_rate(doc: &ClientSettingsDoc) -> u64 {
    match doc.entries().get(RECORDING_FRAME_RATE).and_then(serde_json::Value::as_u64) {
        Some(60) => 60,
        _ => DEFAULT_RECORDING_FRAME_RATE,
    }
}

// The preview reporter uses the same payload on settings changes and reconnects.
#[allow(dead_code)]
pub(crate) fn browser_profile_report_payload(doc: &ClientSettingsDoc) -> serde_json::Value {
    let mut payload = report_profiles_payload(
        &doc.browser_user_profiles(),
        &doc.browser_default_profile_id(),
    );
    payload["recordingFrameRate"] = recording_frame_rate(doc).into();
    payload
}

#[derive(Clone, Copy)]
enum ProfileDataAction {
    Clear,
    Remove,
}

impl ProfileDataAction {
    fn confirmation(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Clear => (
                "Clear “{name}”’s cookies and cache?",
                "You are signed out of its sites. Server browser tabs open in this profile close now.",
                "Clear cookies and cache",
            ),
            Self::Remove => (
                "Remove “{name}”?",
                "Its cookies and logins are deleted. Server browser tabs open in this profile close now.",
                "Remove profile",
            ),
        }
    }
}

fn reset(
    state: &Entity<AppState>,
    change: ClientSettingChange,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let state = state.clone();
    move |_, _, cx| store::update_client(&state, change.clone(), cx)
}

fn select<T: Copy + 'static>(
    state: &Entity<AppState>,
    id: &'static str,
    all: &'static [T],
    selected: T,
    value: fn(T) -> &'static str,
    label: fn(T) -> &'static str,
    change: fn(T) -> ClientSettingChange,
) -> Select {
    let state = state.clone();
    Select::new(
        id,
        all.iter()
            .map(|item| SelectOption::new(value(*item), label(*item)))
            .collect(),
    )
    .selected(value(selected))
    .width(160.)
    .on_change(move |next, _, cx| {
        if let Some(item) = all.iter().find(|item| value(**item) == next.as_ref()) {
            store::update_client(&state, change(*item), cx);
        }
    })
}

fn unavailable(
    id: &'static str,
    title: &'static str,
    description: &'static str,
    reason: &'static str,
    target: bool,
    cx: &App,
) -> gpui::AnyElement {
    Row::new(id, title)
        .description(description)
        .highlighted(target)
        .unavailable(Some(reason))
        .control(muted_text(crate::i18n::t(cx, "Unavailable"), cx))
        .into_any_element()
}

impl SettingsView {
    fn confirm_profile_action(
        &mut self,
        profile: BrowserProfile,
        action: ProfileDataAction,
        cx: &mut Context<Self>,
    ) {
        self.browser.profile_menu = None;
        cx.notify();
        if self.browser.clearing_profile || crate::dialog::is_open(cx) {
            return;
        }
        let (title, description, label) = action.confirmation();
        let name = if is_built_in_browser_profile_id(&profile.id) {
            crate::i18n::t(cx, &profile.name)
        } else {
            &profile.name
        };
        let title = crate::i18n::t_fmt(cx, title, &[("name", name.into())]);
        let question = format!("{title}\n{description}");
        let view = cx.entity().downgrade();
        crate::dialog::ask_with_options(
            &question,
            label,
            crate::dialog::ConfirmVariant::Destructive,
            crate::dialog::InitialFocus::Cancel,
            move |_, cx| {
                cx.defer(move |cx| {
                    let _ = view.update(cx, |view, cx| view.apply_profile_action(profile, action, cx));
                });
            },
            None,
            cx,
        );
    }

    fn apply_profile_action(
        &mut self,
        profile: BrowserProfile,
        action: ProfileDataAction,
        cx: &mut Context<Self>,
    ) {
        if self.browser.clearing_profile {
            return;
        }
        let app = self.state.read(cx);
        let Some(owners) = profile_clear_targets(app) else {
            let message = crate::i18n::t(cx, CLEAR_PROFILE_UNAVAILABLE).to_owned();
            self.state.update(cx, |app, cx| {
                app.show_toast(Toast::new(ToastKind::Error, message), cx);
            });
            return;
        };
        let lookups: Vec<_> = owners
            .iter()
            .map(|owner| app.connections.destination(owner, "preview:operate"))
            .collect();
        let fleet = app.connections.view();
        let state = self.state.clone();
        let profile_id = profile.id.clone();
        let task = app.runtime().spawn(async move {
            let destinations = futures::future::try_join_all(lookups).await.map_err(|_| ())?;
            if !destinations.iter().all(|target| target.is_current(&fleet.borrow())) {
                return Err(());
            }
            let requests = destinations.into_iter().map(|target| {
                let payload = serde_json::json!({ "profileId": profile_id });
                async move {
                    target.client.request(preview_method::CLEAR_PROFILE, payload).await
                }
            });
            if futures::future::join_all(requests).await.iter().all(Result::is_ok) {
                Ok(())
            } else {
                Err(())
            }
        });
        self.browser.clearing_profile = true;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = task.await;
            cx.update(|cx| {
                let _ = view.update(cx, |view, cx| {
                    view.browser.clearing_profile = false;
                    cx.notify();
                });
                let name = if is_built_in_browser_profile_id(&profile.id) {
                    crate::i18n::t(cx, &profile.name)
                } else {
                    &profile.name
                };
                let (kind, title) = match (result, action) {
                    (Ok(Ok(())), ProfileDataAction::Remove) => {
                        let doc = store::client(cx);
                        let mut profiles = doc.browser_user_profiles();
                        profiles.retain(|item| item.id != profile.id);
                        let mut changes = doc.clone().apply_change(&ClientSettingChange::BrowserProfiles(profiles));
                        if doc.browser_default_profile_id() == profile.id {
                            changes.insert("browserDefaultProfileId".to_owned(), DEFAULT_BROWSER_PROFILE_ID.into());
                        }
                        store::update_client(&state, ClientSettingChange::ThemePatch(changes), cx);
                        report_browser_profiles(cx);
                        return;
                    }
                    (Ok(Ok(())), ProfileDataAction::Clear) => (ToastKind::Success, crate::i18n::t_fmt(
                        cx, "Cleared {name}''s cookies and cache", &[("name", name.into())],
                    )),
                    (_, ProfileDataAction::Remove) => (ToastKind::Error, crate::i18n::t(
                        cx, "Profile data could not be deleted. Try again.",
                    ).to_owned()),
                    (_, ProfileDataAction::Clear) => (ToastKind::Error, crate::i18n::t_fmt(
                        cx, "Could not clear {name}''s data", &[("name", name.into())],
                    )),
                };
                state.update(cx, |app, cx| app.show_toast(Toast::new(kind, title), cx));
            });
        }).detach();
    }

    fn browser_profile_menu(
        &self,
        profile: &BrowserProfile,
        is_default: bool,
        default_read_only: bool,
        clear_available: bool,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let id = ElementId::Name(profile.id.clone().into());
        let open = self.browser.profile_menu.as_deref() == Some(profile.id.as_str());
        let toggle_id = profile.id.clone();
        let trigger = Button::new((id.clone(), "browser-profile-actions"), ButtonVariant::GhostMuted, ButtonSize::IconXs)
            .icon(Icon::Ellipsis)
            .tooltip(crate::i18n::t_fmt(cx, "{name} options", &[("name", profile.name.as_str().into())]))
            .disabled(self.browser.clearing_profile)
            .on_click(cx.listener(move |view, _, _, cx| {
                view.browser.profile_menu = (!open).then(|| toggle_id.clone());
                cx.notify();
            }));
        let popup = open.then(|| {
            let p = palette(cx);
            let make_default = profile.id.clone();
            let clear = profile.clone();
            let remove = profile.clone();
            let item = |key, label| {
                Button::new((id.clone(), key), ButtonVariant::GhostMuted, ButtonSize::Xs)
                    .label(label)
                    .justify_between()
            };
            let data_action = |button: Button| {
                button.disabled(!clear_available).when(!clear_available, |button| {
                    button.tooltip(CLEAR_PROFILE_UNAVAILABLE)
                })
            };
            div().absolute().bottom_0().right_0().child(
                gpui::deferred(
                    gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
                        v_flex()
                            .id((id.clone(), "browser-profile-menu"))
                            .relative()
                            .occlude()
                            .mt(r(4.))
                            .w(r(208.))
                            .p(r(4.))
                            .rounded(r(8.))
                            .border_1()
                            .border_color(hsla(p.dropdown_border))
                            .bg(hsla(p.popover))
                            .theme_slot(ThemeSlot::MenuPopup, cx)
                            .children(theme_slot_clipped_background_layer(ThemeSlot::MenuPopup, r(8.), cx))
                            .on_mouse_down_out(cx.listener(|view, _, _, cx| {
                                view.browser.profile_menu = None;
                                cx.notify();
                            }))
                            .child(item("default", "Set as default")
                                .disabled(is_default || default_read_only)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.browser.profile_menu = None;
                                    store::update_client(&view.state, ClientSettingChange::BrowserDefaultProfileId(make_default.clone()), cx);
                                    report_browser_profiles(cx);
                                    cx.notify();
                                })))
                            .child(data_action(item("clear", "Clear cookies and cache"))
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.confirm_profile_action(clear.clone(), ProfileDataAction::Clear, cx);
                                })))
                            .when(!is_built_in_browser_profile_id(&profile.id), |menu| {
                                menu.child(data_action(Button::new((id.clone(), "remove"), ButtonVariant::GhostDestructive, ButtonSize::Xs)
                                    .label("Remove profile and data")
                                    .justify_between())
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.confirm_profile_action(remove.clone(), ProfileDataAction::Remove, cx);
                                    })))
                            }),
                    )
                ).with_priority(2),
            )
        });
        div().relative().flex_none().child(trigger).children(popup).into_any_element()
    }

    pub(super) fn commit_browser_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .browser
            .custom_url
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            let next = self.browser.custom_url.read(cx).value().to_string();
            store::update_client(
                &self.state,
                ClientSettingChange::BrowserCustomSearchUrl(next),
                cx,
            );
        }
        if self.browser.viewport_width.read(cx).focus_handle(cx).is_focused(window)
            || self.browser.viewport_height.read(cx).focus_handle(cx).is_focused(window)
        {
            self.commit_browser_viewport_dimensions(window, cx);
        }
        let focused_profile_id = self.browser.profile_inputs.iter().find_map(|(id, (input, _))| {
            input.read(cx).focus_handle(cx).is_focused(window).then(|| id.clone())
        });
        if let Some(profile_id) = focused_profile_id {
            self.commit_browser_profile_rename(&profile_id, window, cx);
        }
    }

    fn commit_browser_viewport_dimensions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = store::client(cx).browser_default_viewport();
        let Some((cur_w, cur_h)) = viewport.dimensions() else {
            return;
        };
        let parsed = self
            .browser
            .viewport_width
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
            .ok()
            .zip(
                self.browser
                    .viewport_height
                    .read(cx)
                    .value()
                    .trim()
                    .parse::<u32>()
                    .ok(),
            )
            .filter(|(w, h)| validate_viewport_dimensions(*w, *h));
        let Some((width, height)) = parsed else {
            self.browser
                .viewport_width
                .update(cx, |input, cx| input.set_value(cur_w.to_string(), window, cx));
            self.browser
                .viewport_height
                .update(cx, |input, cx| input.set_value(cur_h.to_string(), window, cx));
            return;
        };
        if (width, height) != (cur_w, cur_h) {
            store::update_client(
                &self.state,
                ClientSettingChange::BrowserDefaultViewport(ViewportSetting::Freeform { width, height }),
                cx,
            );
        }
    }

    fn commit_browser_profile_rename(
        &mut self,
        profile_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((input, _)) = self.browser.profile_inputs.get(profile_id) else {
            return;
        };
        let input = input.clone();
        let raw = input.read(cx).value().to_string();
        let next: String = raw.trim().chars().take(BROWSER_PROFILE_NAME_MAX_LENGTH).collect();
        let mut profiles = store::client(cx).browser_user_profiles();
        let Some(profile) = profiles.iter_mut().find(|profile| profile.id == profile_id) else {
            return;
        };
        if next.is_empty() {
            let previous = profile.name.clone();
            input.update(cx, |input, cx| input.set_value(previous, window, cx));
            return;
        }
        if profile.name != next {
            profile.name = next.clone();
            store::update_client(&self.state, ClientSettingChange::BrowserProfiles(profiles), cx);
            report_browser_profiles(cx);
        }
        if input.read(cx).value().as_ref() != next {
            input.update(cx, |input, cx| input.set_value(next, window, cx));
        }
    }

    pub(super) fn diffs_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let doc = store::client(cx).clone();
        let state = self.state.clone();
        let whitespace_state = state.clone();
        let folds_state = state.clone();
        let diffs = section("Diffs", vec![
            Row::new("diff-layout", "Diff layout")
                .description("Show diffs stacked or side by side. The toggle in the diff toolbar changes this too.")
                .highlighted(self.is_target("diff-layout"))
                .reset(doc.diff_layout() != Layout::Stacked, reset(&state, ClientSettingChange::DiffLayout(Layout::Stacked)))
                .control(select(&state, "diff-layout-select", &[Layout::Stacked, Layout::Split], doc.diff_layout(), |layout| match layout { Layout::Stacked => "stacked", Layout::Split => "split" }, |layout| match layout { Layout::Stacked => "Stacked", Layout::Split => "Split" }, ClientSettingChange::DiffLayout)).into_any_element(),
            Row::new("hide-whitespace-changes", "Hide whitespace changes")
                .description("Set whether the diff panel ignores whitespace-only edits by default.")
                .highlighted(self.is_target("hide-whitespace-changes"))
                .reset(!doc.diff_ignore_whitespace(), reset(&state, ClientSettingChange::DiffIgnoreWhitespace(true)))
                .control(Switch::new("diff-whitespace-switch", doc.diff_ignore_whitespace()).on_change(move |on, _, cx| store::update_client(&whitespace_state, ClientSettingChange::DiffIgnoreWhitespace(on), cx))).into_any_element(),
            Row::new("default-diff-file-state", "Default diff file state")
                .description("Start with files expanded or collapsed when opening diffs or a pull request's Code tab.")
                .highlighted(self.is_target("default-diff-file-state"))
                .reset(!doc.diff_files_collapsed(), reset(&state, ClientSettingChange::DiffFilesCollapsed(true)))
                .control(Select::new("diff-files-select", vec![SelectOption::new("expanded", "Expanded"), SelectOption::new("collapsed", "Collapsed")])
                    .width(160.).selected(if doc.diff_files_collapsed() { "collapsed" } else { "expanded" })
                    .on_change(move |value, _, cx| store::update_client(&folds_state, ClientSettingChange::DiffFilesCollapsed(value == "collapsed"), cx))).into_any_element(),
            Row::new("diff-color-scheme", "Diff colors")
                .description("Choose colors for additions and deletions, including change counts.")
                .highlighted(self.is_target("diff-color-scheme"))
                .reset(doc.diff_color_scheme() != DiffColorScheme::default(), reset(&state, ClientSettingChange::DiffColorScheme(DiffColorScheme::default())))
                .control({
                    let active_scheme = doc.diff_color_scheme();
                    select(&state, "diff-colors-select", DiffColorScheme::ALL, active_scheme, DiffColorScheme::as_str, DiffColorScheme::label, ClientSettingChange::DiffColorScheme)
                        .render_icon(move |value, cx| {
                            let p = palette(cx);
                            let dark = cx.theme().mode.is_dark();
                            let (first, second) = diff_swatch_colors(value, active_scheme, dark, &p);
                            Some(
                                h_flex()
                                    .gap(r(4.))
                                    .child(div().size(r(8.)).rounded_full().bg(hsla(first)))
                                    .child(div().size(r(8.)).rounded_full().bg(hsla(second)))
                                    .into_any_element(),
                            )
                        })
                        .label(if active_scheme == DiffColorScheme::RedGreen { "Red & green" } else { "Blue & orange" })
                        .popup_width(208.)
                }).into_any_element(),
        ], cx);
        let proactive_state = state.clone();
        let pr_links_state = state.clone();
        let prs = section("Pull requests", vec![
            Row::new("proactive-panels", "Open pull requests and changes panels proactively")
                .description("Open linked pull requests first. Otherwise, open Changes for edits to at least 3 files or 50 lines.")
                .highlighted(self.is_target("proactive-panels"))
                .reset(doc.proactive_panels_enabled(), reset(&state, ClientSettingChange::ProactivePanelsEnabled(false)))
                .control(Switch::new("proactive-panels-switch", doc.proactive_panels_enabled()).on_change(move |on, _, cx| store::update_client(&proactive_state, ClientSettingChange::ProactivePanelsEnabled(on), cx)))
                .into_any_element(),
            Row::new("pull-request-links-open-in-app", "Open pull requests in T3 Code")
                .description("Clicking a pull request link opens it in T3 Code. Turn off to open pull request links like any other link, on your git host.")
                .highlighted(self.is_target("pull-request-links-open-in-app"))
                .reset(!doc.pull_request_links_open_in_app(), reset(&state, ClientSettingChange::PullRequestLinksOpenInApp(true)))
                .control(Switch::new("pull-request-links-open-in-app-switch", doc.pull_request_links_open_in_app()).on_change(move |on, _, cx| store::update_client(&pr_links_state, ClientSettingChange::PullRequestLinksOpenInApp(on), cx))).into_any_element(),
        ], cx);
        vec![
            Block::new(
                &[
                    "diffs",
                    "diff-layout",
                    "hide-whitespace-changes",
                    "default-diff-file-state",
                    "diff-color-scheme",
                ],
                diffs,
            ),
            Block::new(
                &[
                    "pull-requests",
                    "proactive-panels",
                    "pull-request-links-open-in-app",
                ],
                prs,
            ),
        ]
    }

    pub(super) fn browser_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Block> {
        let config = self.scoped_representative_config(cx);
        let all_configs = self.scoped_all_configs(cx);
        let enabled = config
            .as_ref()
            .and_then(|config| config.settings.enable_agent_browser_access)
            .unwrap_or(true);
        let available = config.is_some() && self.may("settings:write", cx);
        let agent_browser_locked = t3_core::managed_policy::setting_lock_reason_for_any(
            if all_configs.is_empty() {
                vec![config.as_ref().and_then(|c| c.managed_policy.as_ref())]
            } else {
                all_configs.iter().map(|c| c.managed_policy.as_ref()).collect()
            },
            &["enableAgentBrowserAccess"],
        );
        let agent_browser_unavailable = agent_browser_locked.or_else(|| {
            (!available).then(|| {
                "Reconnect with settings write access to change agent browser access.".to_owned()
            })
        });
        let is_project_scope = self.is_project_scope(cx);
        let switch_view = cx.entity().downgrade();
        let access = section(
            "Agent access",
            vec![
                self.apply_scoped_reset(
                    Row::new("agent-browser-access", "Agent browser access")
                        .description(if is_project_scope {
                            "Allow agents in this project to use the shared browser. Applies when the agent session next starts."
                        } else {
                            "Allow agents to use the shared browser. Projects can override it."
                        })
                        .highlighted(self.is_target("agent-browser-access"))
                        .unavailable(agent_browser_unavailable.clone()),
                    &["enableAgentBrowserAccess"],
                    !enabled,
                    ServerSettingChange::AgentBrowserAccess(true),
                    cx,
                )
                .control(
                    Switch::new("agent-browser-access-switch", enabled)
                        .disabled(agent_browser_unavailable.is_some())
                        .on_change(move |on, _, cx| {
                            let _ = switch_view.update(cx, |this, cx| {
                                this.update_scoped_server(ServerSettingChange::AgentBrowserAccess(on), cx);
                            });
                        }),
                )
                .into_any_element(),
            ],
            cx,
        );
        let doc = store::client(cx).clone();
        let state = self.state.clone();
        let reset_state = state.clone();
        let reset_input = self.browser.custom_url.clone();
        let weak = cx.entity().downgrade();
        let cookie_import_denial = t3_core::managed_policy::capability_denial(
            self.state
                .read(cx)
                .server_config
                .config
                .as_ref()
                .and_then(|c| c.managed_policy.as_ref()),
            t3_core::managed_policy::Capability::BrowserCookieImport,
        );
        let clear_available = profile_clear_targets(self.state.read(cx)).is_some();
        let user_profiles = doc.browser_user_profiles();
        let resolved_profiles = doc.browser_profiles();
        let listed_profiles: Vec<BrowserProfile> = resolved_profiles
            .iter()
            .filter(|profile| profile.kind != BrowserProfileKind::Incognito)
            .cloned()
            .collect();
        let default_profile_id = doc.browser_default_profile_id();
        let at_profile_limit = user_profiles.len() >= BROWSER_PROFILE_MAX_COUNT;

        // Sync inline rename inputs with current custom profiles.
        self.browser
            .profile_inputs
            .retain(|id, _| user_profiles.iter().any(|profile| profile.id == *id));
        for profile in &user_profiles {
            if let Some((input, _)) = self.browser.profile_inputs.get(&profile.id) {
                if !input.read(cx).focus_handle(cx).is_focused(window)
                    && input.read(cx).value().as_ref() != profile.name
                {
                    let name = profile.name.clone();
                    input.update(cx, |input, cx| input.set_value(name, window, cx));
                }
            } else {
                let id_for_sub = profile.id.clone();
                let initial_name = profile.name.clone();
                let input = cx.new(|cx| InputState::new(window, cx).default_value(initial_name));
                let sub = cx.subscribe_in(&input, window, move |view, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        view.commit_browser_profile_rename(&id_for_sub, window, cx);
                    }
                });
                self.browser.profile_inputs.insert(profile.id.clone(), (input, sub));
            }
        }

        let p = palette(cx);
        let divider = hsla(p.border).opacity(0.6);
        let profile_rows: Vec<gpui::AnyElement> = listed_profiles
            .iter()
            .enumerate()
            .map(|(index, profile)| {
                let built_in = is_built_in_browser_profile_id(&profile.id);
                let is_default = profile.id == default_profile_id;
                let name_slot: gpui::AnyElement = if built_in {
                    div()
                        .truncate()
                        .text_sm()
                        .text_color(hsla(p.foreground))
                        .child(crate::i18n::t(cx, &profile.name).to_owned())
                        .into_any_element()
                } else if let Some((input, _)) = self.browser.profile_inputs.get(&profile.id) {
                    div()
                        .w(r(200.))
                        .child(Input::new(input).small().theme_slot(ThemeSlot::Input, cx))
                        .into_any_element()
                } else {
                    div()
                        .truncate()
                        .text_sm()
                        .text_color(hsla(p.foreground))
                        .child(profile.name.clone())
                        .into_any_element()
                };

                h_flex()
                    .id(SharedString::from(format!("browser-profile-row-{}", profile.id)))
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(r(12.))
                    .px(r(12.))
                    .py(r(8.))
                    .when(index > 0, |this| this.border_t_1().border_color(divider))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(r(8.))
                            .child(name_slot)
                            .when(is_default, |this| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .px(r(6.))
                                        .py(r(1.))
                                        .rounded(r(999.))
                                        .border_1()
                                        .border_color(hsla(p.border))
                                        .bg(hsla(p.muted).opacity(0.6))
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(hsla(p.foreground))
                                        .child(crate::i18n::t(cx, "Default").to_owned()),
                                )
                            }),
                    )
                    .child(self.browser_profile_menu(profile, is_default, is_project_scope, clear_available, cx))
                    .into_any_element()
            })
            .collect();

        let add_profile_state = state.clone();
        let mut add_profile_btn = Button::new(
            "browser-add-profile",
            ButtonVariant::Outline,
            ButtonSize::Sm,
        )
        .icon(Icon::Plus)
        .label("Add profile")
        .disabled(at_profile_limit)
        .on_click(move |_, _, cx| {
            let current_user = store::client(cx).browser_user_profiles();
            if current_user.len() >= BROWSER_PROFILE_MAX_COUNT {
                return;
            }
            let resolved = store::client(cx).browser_profiles();
            let base = crate::i18n::t(cx, "New profile").to_owned();
            let name = next_unique_profile_name(&resolved, &base);
            let mut next = current_user;
            next.push(BrowserProfile {
                id: format!("profile-{}", crate::app_state::uuid_v4()),
                name,
                kind: BrowserProfileKind::Persistent,
            });
            store::update_client(&add_profile_state, ClientSettingChange::BrowserProfiles(next), cx);
            report_browser_profiles(cx);
        });
        if at_profile_limit {
            add_profile_btn = add_profile_btn.tooltip("You\u{2019}ve reached the profile limit");
        }

        let reset_default_profile_state = state.clone();
        let profiles_row = Row::new("browser-profiles", "Browser profiles")
            .description(
                "Profiles separate cookies and logins. Incognito data is cleared when the app closes.",
            )
            .highlighted(
                self.is_target("browser")
                    || self.is_target("browser-profiles")
                    || self.is_target("browser-default-profile"),
            )
            .reset(
                !is_project_scope && default_profile_id != DEFAULT_BROWSER_PROFILE_ID,
                move |_, _, cx| {
                    store::update_client(
                        &reset_default_profile_state,
                        ClientSettingChange::BrowserDefaultProfileId(
                            DEFAULT_BROWSER_PROFILE_ID.to_owned(),
                        ),
                        cx,
                    );
                    report_browser_profiles(cx);
                },
            )
            .control(add_profile_btn)
            .below(
                v_flex()
                    .mt(r(8.))
                    .mb(r(8.))
                    .overflow_hidden()
                    .rounded(r(8.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .children(profile_rows),
            )
            .into_any_element();

        let mut browsing = vec![profiles_row];
        if let Some(denial) = cookie_import_denial {
            browsing.push(
                div()
                    .px(r(16.))
                    .py(r(10.))
                    .debug_selector(|| "managed-browser-cookie-import-notice".to_owned())
                    .child(crate::components::managed::notice(denial, cx))
                    .into_any_element(),
            );
        }
        browsing.extend([
            Row::new("browser-link-target", "Open links in")
                .description("Where links in the chat and terminal open. Hold \u{2318} or Ctrl while clicking a link to open it in your default browser either way.")
                .highlighted(self.is_target("browser-link-target"))
                .reset(doc.browser_link_target() != BrowserLinkTarget::default(), reset(&state, ClientSettingChange::BrowserLinkTarget(BrowserLinkTarget::default())))
                .control(select(&state, "browser-link-target-select", BrowserLinkTarget::ALL, doc.browser_link_target(), BrowserLinkTarget::as_str, BrowserLinkTarget::label, ClientSettingChange::BrowserLinkTarget)).into_any_element(),
            Row::new("browser-search-engine", "Search engine")
                .description("What the address bar searches when you type something that isn't a URL.")
                .highlighted(self.is_target("browser-search-engine"))
                .reset(doc.browser_search_engine() != BrowserSearchEngine::default() || !doc.browser_custom_search_url().is_empty(), move |_, window, cx| {
                    store::update_client(&reset_state, ClientSettingChange::BrowserSearchEngine(BrowserSearchEngine::default()), cx);
                    store::update_client(&reset_state, ClientSettingChange::BrowserCustomSearchUrl(String::new()), cx);
                    reset_input.update(cx, |input, cx| input.set_value("", window, cx));
                })
                .control(select(&state, "browser-search-engine-select", BrowserSearchEngine::ALL, doc.browser_search_engine(), BrowserSearchEngine::as_str, BrowserSearchEngine::label, ClientSettingChange::BrowserSearchEngine)).into_any_element(),
        ]);
        if doc.browser_search_engine() == BrowserSearchEngine::Custom {
            if !self
                .browser
                .custom_url
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
            {
                let saved = doc.browser_custom_search_url();
                if self.browser.custom_url.read(cx).value().as_ref() != saved {
                    self.browser
                        .custom_url
                        .update(cx, |input, cx| input.set_value(saved, window, cx));
                }
            }
            let mut row = Row::new("browser-custom-search-url", "Custom search URL")
                .description("Use %s where the search terms go.");
            if !valid_search_template(doc.browser_custom_search_url()) {
                row = row.status(
                    "Enter an http or https URL containing %s. Searches use DuckDuckGo until then.",
                );
            }
            browsing.push(
                row.control(
                    div()
                        .w(r(256.))
                        .child(Input::new(&self.browser.custom_url).small().theme_slot(ThemeSlot::Input, cx)),
                )
                .into_any_element(),
            );
        }
        let auto_show = browser_auto_show_floating_preview(&doc);
        let auto_show_state = state.clone();
        browsing.push(
            Row::new("browser-auto-show-floating-preview", "Auto-show floating preview")
                .description("Show the floating preview when an agent opens a browser or device unless the agent says otherwise.")
                .highlighted(self.is_target("browser-auto-show-floating-preview"))
                .reset(!auto_show, reset(&state, client_preference(AUTO_SHOW_FLOATING_PREVIEW, true)))
                .control(Switch::new("browser-auto-show-floating-preview-switch", auto_show)
                    .on_change(move |enabled, _, cx| {
                        store::update_client(&auto_show_state, client_preference(AUTO_SHOW_FLOATING_PREVIEW, enabled), cx);
                    }))
                .into_any_element(),
        );
        browsing.push(unavailable(
            "browser-cookie-import", "Import from",
            "Import cookies from an installed browser into a browser profile.",
            BROWSER_COOKIE_IMPORT_UNAVAILABLE, self.is_target("browser-cookie-import"), cx,
        ));
        let advanced = self.browser.advanced || ADVANCED.iter().any(|id| self.is_target(id));
        browsing.push(
            div()
                .px(r(16.))
                .py(r(8.))
                .child(
                    Button::new(
                        "browser-advanced",
                        ButtonVariant::GhostMuted,
                        ButtonSize::Sm,
                    )
                    .icon(if advanced {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    })
                    .label("Advanced")
                    .on_click(move |_, _, cx| {
                        let _ = weak.update(cx, |view, cx| {
                            view.browser.advanced = !advanced;
                            view.target = None;
                            cx.notify();
                        });
                    }),
                )
                .into_any_element(),
        );
        if advanced {
            let viewport = doc.browser_default_viewport();
            let sized = viewport.dimensions();
            if let Some((w, h)) = sized {
                if !self.browser.viewport_width.read(cx).focus_handle(cx).is_focused(window) {
                    let w_str = w.to_string();
                    if self.browser.viewport_width.read(cx).value().as_ref() != w_str {
                        self.browser
                            .viewport_width
                            .update(cx, |input, cx| input.set_value(w_str, window, cx));
                    }
                }
                if !self.browser.viewport_height.read(cx).focus_handle(cx).is_focused(window) {
                    let h_str = h.to_string();
                    if self.browser.viewport_height.read(cx).value().as_ref() != h_str {
                        self.browser
                            .viewport_height
                            .update(cx, |input, cx| input.set_value(h_str, window, cx));
                    }
                }
            }
            let mut viewport_options = vec![
                SelectOption::new("fill", "Fill panel"),
                SelectOption::new("responsive", "Responsive"),
            ];
            viewport_options.extend(VIEWPORT_PRESETS.iter().map(|preset| {
                SelectOption::new(preset.id, preset.label)
                    .group("Standard")
                    .description(preset.detail)
            }));
            let vp_state = state.clone();
            let vp_sized = sized;
            let viewport_select = Select::new("browser-default-viewport-select", viewport_options)
                .selected(viewport_select_value(&viewport).to_owned())
                .label(viewport_trigger_label(&viewport, cx))
                .width(176.)
                .popup_width(248.)
                .show_check()
                .on_change(move |value, _, cx| {
                    let next = match value.as_ref() {
                        "fill" => ViewportSetting::Fill,
                        "responsive" => {
                            let (width, height) = vp_sized.unwrap_or(RESPONSIVE_SEED_SIZE);
                            ViewportSetting::Freeform { width, height }
                        }
                        preset_id => match preset_setting(preset_id) {
                            Some(preset) => preset,
                            None => return,
                        },
                    };
                    store::update_client(&vp_state, ClientSettingChange::BrowserDefaultViewport(next), cx);
                });
            let mut viewport_control = h_flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap(r(8.))
                .child(viewport_select);
            if let Some((w, h)) = sized {
                let rotate_state = state.clone();
                let rotate_source = viewport.clone();
                viewport_control = viewport_control.child(
                    h_flex()
                        .items_center()
                        .gap(r(4.))
                        .child(div().w(r(68.)).child(Input::new(&self.browser.viewport_width).small()))
                        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("×"))
                        .child(div().w(r(68.)).child(Input::new(&self.browser.viewport_height).small()))
                        .child(
                            Button::new(
                                "browser-default-viewport-rotate",
                                ButtonVariant::GhostMuted,
                                ButtonSize::IconSm,
                            )
                            .icon(Icon::RotateCcw)
                            .tooltip(if h >= w {
                                "Rotate to landscape"
                            } else {
                                "Rotate to portrait"
                            })
                            .on_click(move |_, _, cx| {
                                let (rotated, _) = rotate_viewport(&rotate_source, None);
                                store::update_client(
                                    &rotate_state,
                                    ClientSettingChange::BrowserDefaultViewport(rotated),
                                    cx,
                                );
                            }),
                        ),
                );
            }

            let zoom = doc.browser_default_zoom_factor();
            let zoom_state = state.clone();
            let zoom_options: Vec<SelectOption> = ZOOM_LEVELS
                .iter()
                .map(|level| SelectOption::new(zoom_value_key(*level), zoom_label(*level)))
                .collect();
            let zoom_select = Select::new("browser-default-zoom-select", zoom_options)
                .selected(zoom_value_key(zoom))
                .label(zoom_label(zoom))
                .width(160.)
                .show_check()
                .on_change(move |value, _, cx| {
                    if let Some(&level) = ZOOM_LEVELS.iter().find(|level| zoom_value_key(**level) == value.as_ref()) {
                        store::update_client(&zoom_state, ClientSettingChange::BrowserDefaultZoomFactor(level), cx);
                    }
                });

            let appearance = doc.browser_default_appearance();
            let appearance_state = state.clone();
            let appearance_select = Select::new(
                "browser-default-appearance-select",
                AppearancePreference::ALL
                    .iter()
                    .map(|pref| SelectOption::new(pref.as_str(), pref.label()))
                    .collect(),
            )
            .selected(appearance.as_str())
            .width(160.)
            .show_check()
            .on_change(move |value, _, cx| {
                if let Some(pref) = AppearancePreference::parse(value.as_ref()) {
                    store::update_client(&appearance_state, ClientSettingChange::BrowserDefaultAppearance(pref), cx);
                }
            });

            browsing.extend([
                Row::new("browser-default-viewport", "Default browser viewport")
                    .description("Tab size for you and agents. Fill fits the panel; other sizes show the device toolbar.")
                    .highlighted(self.is_target("browser-default-viewport"))
                    .reset(!viewport.is_fill(), reset(&state, ClientSettingChange::BrowserDefaultViewport(ViewportSetting::Fill)))
                    .reset_tooltip("Reset default browser viewport to default")
                    .control(viewport_control)
                    .into_any_element(),
                Row::new("browser-default-zoom", "Default browser zoom")
                    .description("Page zoom applied to new browser tabs.")
                    .highlighted(self.is_target("browser-default-zoom"))
                    .reset(
                        (zoom - DEFAULT_ZOOM).abs() > 1e-9,
                        reset(&state, ClientSettingChange::BrowserDefaultZoomFactor(DEFAULT_ZOOM)),
                    )
                    .reset_tooltip("Reset default browser zoom to default")
                    .control(zoom_select)
                    .into_any_element(),
                Row::new("browser-default-appearance", "Default browser appearance")
                    .description("The color scheme pages are told to prefer. System follows your OS setting.")
                    .highlighted(self.is_target("browser-default-appearance"))
                    .reset(
                        appearance != AppearancePreference::System,
                        reset(&state, ClientSettingChange::BrowserDefaultAppearance(AppearancePreference::System)),
                    )
                    .reset_tooltip("Reset default browser appearance to default")
                    .control(appearance_select)
                    .into_any_element(),
            ]);
        }
        let frame_rate = recording_frame_rate(&doc);
        let frame_rate_state = state.clone();
        let reset_frame_rate_state = state.clone();
        let recordings = section("Recordings", vec![
            Row::new("browser-recording-frame-rate", "Browser recording frame rate")
                .description("Maximum recording rate. 30 fps saves CPU and storage; 60 fps is smoother.")
                .highlighted(self.is_target("browser-recording-frame-rate"))
                .reset(frame_rate != DEFAULT_RECORDING_FRAME_RATE, move |_, _, cx| {
                    store::update_client(&reset_frame_rate_state, client_preference(RECORDING_FRAME_RATE, DEFAULT_RECORDING_FRAME_RATE), cx);
                    report_browser_profiles(cx);
                })
                .control(Select::new(
                    "browser-recording-frame-rate-select",
                    vec![SelectOption::new("30", "30 fps"), SelectOption::new("60", "60 fps")],
                ).selected(frame_rate.to_string()).show_check().on_change(move |value, _, cx| {
                    let rate = if value.as_ref() == "60" { 60 } else { DEFAULT_RECORDING_FRAME_RATE };
                    store::update_client(&frame_rate_state, client_preference(RECORDING_FRAME_RATE, rate), cx);
                    report_browser_profiles(cx);
                }))
                .into_any_element(),
            unavailable("browser-recording-key-presses", "Show key presses in recordings", "Show pressed keys and shortcuts in new recordings. Password fields are excluded.", RECORDING_OVERLAYS_UNAVAILABLE, self.is_target("browser-recording-key-presses"), cx),
            unavailable("browser-recording-mouse-presses", "Show mouse presses in recordings", "Highlight mouse presses and held buttons in new recordings.", RECORDING_OVERLAYS_UNAVAILABLE, self.is_target("browser-recording-mouse-presses"), cx),
        ], cx);
        vec![
            Block::new(&["browser-agent-access", "agent-browser-access"], access),
            Block::new(
                &[
                    "browser",
                    "browser-profiles",
                    "browser-default-profile",
                    "browser-link-target",
                    "browser-search-engine",
                    "browser-auto-show-floating-preview",
                    "browser-default-viewport",
                    "browser-default-zoom",
                    "browser-default-appearance",
                ],
                section("Browsing", browsing, cx),
            ),
            Block::new(
                &[
                    "browser-recordings",
                    "browser-recording-frame-rate",
                    "browser-recording-key-presses",
                    "browser-recording-mouse-presses",
                ],
                recordings,
            ),
        ]
    }

    fn start_snapshot_recording(&mut self, cx: &mut Context<Self>) {
        self.browser.snapshot.recording = true;
        if let Some(coordinator) = crate::snapshot_capture::active_coordinator(cx) {
            coordinator.update(cx, |c, cx| c.set_shortcut_suppressed(true, cx));
        }
        cx.notify();
    }

    fn stop_snapshot_recording(&mut self, cx: &mut Context<Self>) {
        if !self.browser.snapshot.recording {
            return;
        }
        self.browser.snapshot.recording = false;
        if let Some(coordinator) = crate::snapshot_capture::active_coordinator(cx) {
            coordinator.update(cx, |c, cx| c.set_shortcut_suppressed(false, cx));
        }
        cx.notify();
    }

    fn snapshot_backend_state(&self, cx: &App) -> SnapShotBackendState {
        if let Some(coordinator) = crate::snapshot_capture::active_coordinator(cx) {
            return coordinator.read(cx).backend_state().clone();
        }
        let doc = store::client(cx);
        let plat = crate::snapshot_capture::platform(cx);
        if !plat.is_supported() {
            return SnapShotBackendState::unsupported(doc.snap_shot_shortcut());
        }
        if let Some(perms) = plat.mac_permissions() {
            SnapShotBackendState::macos(
                doc.snap_shot_shortcut(),
                perms,
                doc.snap_shot_include_accessibility(),
                doc.snap_shot_enabled() && perms.screen_recording,
                false,
            )
        } else {
            SnapShotBackendState::unsupported(doc.snap_shot_shortcut())
        }
    }

    fn snapshot_sound_menu(&self, selection: SnapShotSoundSelection, cx: &Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let open = self.browser.snapshot.sound_menu;
        let trigger = Button::new("snap-shot-sound-select", ButtonVariant::Outline, ButtonSize::Sm)
            .label(selection.label()).trailing(Icon::ChevronDown)
            .on_click(cx.listener(move |view, _, _, cx| {
                view.browser.snapshot.sound_menu = !open;
                cx.notify();
            }));
        let popup = open.then(|| {
            let items = SnapShotSoundSelection::ALL.iter().copied().map(|option| {
                let id = ElementId::Name(option.as_str().into());
                let pick = Button::new((id.clone(), "snapshot-sound"), ButtonVariant::GhostMuted, ButtonSize::Xs)
                    .label(option.label()).justify_between()
                    .when(option == selection, |button| button.trailing(Icon::Check))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.browser.snapshot.sound_menu = false;
                        store::update_client(&view.state, ClientSettingChange::SnapShotSoundSelection(option), cx);
                        cx.notify();
                    }));
                h_flex().gap(r(4.)).child(div().flex_1().min_w_0().child(pick))
                    .children(match option {
                        SnapShotSoundSelection::Off => None,
                        SnapShotSoundSelection::Sound(sound) => Some(
                            Button::new((id, "snapshot-sound-play"), ButtonVariant::GhostMuted, ButtonSize::IconXs)
                                .icon(Icon::Play)
                                .tooltip(match sound {
                                    SnapShotSound::SoftPop => "Play Whoosh",
                                    SnapShotSound::CameraShutter => "Play Click",
                                })
                                .on_click(move |_, _, cx| crate::snapshot_capture::platform(cx).play_sound(sound)),
                        ),
                    })
            });
            div().absolute().bottom_0().right_0().child(
                gpui::deferred(gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
                    v_flex().id("snap-shot-sound-menu").relative().occlude()
                        .mt(r(4.)).w(r(196.)).p(r(4.)).rounded(r(8.)).border_1()
                        .border_color(hsla(p.dropdown_border)).bg(hsla(p.popover))
                        .theme_slot(ThemeSlot::MenuPopup, cx)
                        .children(theme_slot_clipped_background_layer(ThemeSlot::MenuPopup, r(8.), cx))
                        .on_mouse_down_out(cx.listener(|view, _, _, cx| {
                            view.browser.snapshot.sound_menu = false;
                            cx.notify();
                        }))
                        .children(items),
                )).with_priority(2)
            )
        });
        div().relative().child(trigger).children(popup).into_any_element()
    }

    pub(super) fn snapshots_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let doc = store::client(cx).clone();
        let state = self.state.clone();
        let backend = self.snapshot_backend_state(cx);
        let capture_available = backend.mode != SnapShotMode::Unavailable;
        let enabled = doc.snap_shot_enabled();

        if !capture_available {
            return vec![Block::new(
                &[
                    "snap-shot",
                    "snap-shot-enabled",
                    "snap-shot-accessibility",
                    "snap-shot-shortcut",
                    "snap-shot-sound",
                    "snap-shot-flash",
                    "snap-shot-animations",
                ],
                section(
                    "SnapShots",
                    vec![
                        unavailable_group(
                            "Not supported on this platform.",
                            vec![
                                Row::new("snap-shot-enabled", "SnapShots")
                                    .description(snap_shot_description(Some(&backend)))
                                    .highlighted(self.is_target("snap-shot-enabled"))
                                    .control(
                                        Switch::new("snap-shot-enabled-switch", false)
                                            .disabled(true),
                                    )
                                    .into_any_element(),
                            ],
                            cx,
                        )
                        .into_any_element(),
                    ],
                    cx,
                ),
            )];
        }

        let include_ax = doc.snap_shot_include_accessibility();
        let setup_complete = snap_shot_setup_complete(Some(&backend), include_ax);
        let status_text = snap_shot_status(Some(&backend), enabled);
        let switch_state = state.clone();
        let setup_btn_label = snap_shot_setup_button_label(Some(&backend));

        let mut enable_controls = h_flex().items_center().gap(r(8.));
        let weak_setup_btn = cx.entity().downgrade();
        let weak_switch_setup = cx.entity().downgrade();
        if enabled {
            enable_controls = enable_controls.child(
                Button::new("snap-shot-setup-btn", ButtonVariant::Outline, ButtonSize::Xs)
                    .label(setup_btn_label)
                    .on_click(move |_, _, cx| {
                        let _ = weak_setup_btn.update(cx, |view, cx| {
                            view.stop_snapshot_recording(cx);
                        });
                        if let Some(dialog) = crate::snapshot_capture::active_setup_dialog(cx) {
                            dialog.update(cx, |d, cx| d.open(None, cx));
                        }
                    }),
            );
        }
        enable_controls = enable_controls.child(
            Switch::new("snap-shot-enabled-switch", enabled).on_change(move |checked, _, cx| {
                if !checked {
                    let _ = weak_switch_setup.update(cx, |view, cx| {
                        view.stop_snapshot_recording(cx);
                    });
                    store::update_client(
                        &switch_state,
                        ClientSettingChange::SnapShotEnabled(false),
                        cx,
                    );
                } else if !setup_complete
                    && let Some(dialog) = crate::snapshot_capture::active_setup_dialog(cx)
                {
                    let _ = weak_switch_setup.update(cx, |view, cx| {
                        view.stop_snapshot_recording(cx);
                    });
                    dialog.update(cx, |d, cx| d.open(None, cx));
                } else {
                    store::update_client(
                        &switch_state,
                        ClientSettingChange::SnapShotEnabled(true),
                        cx,
                    );
                }
            }),
        );

        let mut rows: Vec<gpui::AnyElement> = vec![
            Row::new("snap-shot-enabled", "SnapShots")
                .description(snap_shot_description(Some(&backend)))
                .status(status_text)
                .highlighted(self.is_target("snap-shot-enabled"))
                .control(enable_controls)
                .into_any_element(),
        ];

        if enabled {
            let ax_state = state.clone();
            rows.push(
                Row::new("snap-shot-accessibility", "Include app text")
                    .description("Include text and controls when the app makes them available.")
                    .highlighted(self.is_target("snap-shot-accessibility"))
                    .reset(
                        !include_ax,
                        reset(&state, ClientSettingChange::SnapShotIncludeAccessibility(true)),
                    )
                    .control(
                        Switch::new("snap-shot-accessibility-switch", include_ax).on_change(
                            move |on, _, cx| {
                                store::update_client(
                                    &ax_state,
                                    ClientSettingChange::SnapShotIncludeAccessibility(on),
                                    cx,
                                );
                                if let Some(coordinator) =
                                    crate::snapshot_capture::active_coordinator(cx)
                                {
                                    coordinator.update(cx, |c, cx| {
                                        c.refresh_state(cx);
                                    });
                                }
                            },
                        ),
                    )
                    .into_any_element(),
            );

            let saved_shortcut = doc.snap_shot_shortcut();
            let candidate = self
                .browser
                .snapshot
                .candidate
                .clone()
                .unwrap_or_else(|| saved_shortcut.clone());
            let shortcut_changed = !same_snap_shot_shortcut(&candidate, &saved_shortcut, MAC);
            let recording = self.browser.snapshot.recording;
            let conflict = shortcut_changed
                .then(|| shortcut_conflict_message(&candidate, &self.state, cx))
                .flatten();
            let can_save_shortcut = shortcut_changed && conflict.is_none();

            let shortcut_status = if recording {
                Some("Press your shortcut. Esc cancels.".to_owned())
            } else if let Some(conflict_msg) = conflict {
                Some(conflict_msg)
            } else if shortcut_changed {
                Some("Ready to save.".to_owned())
            } else {
                snap_shot_shortcut_status(Some(&backend))
            };

            let weak_record = cx.entity().downgrade();
            let weak_save = cx.entity().downgrade();
            let weak_cancel = cx.entity().downgrade();
            let weak_reset = cx.entity().downgrade();
            let reset_shortcut_state = state.clone();
            let save_shortcut_state = state.clone();
            let save_candidate = candidate.clone();
            let p = palette(cx);

            let recorder_trigger = div()
                .id("snap-shot-shortcut-recorder")
                .px(r(8.))
                .py(r(4.))
                .rounded(r(6.))
                .border_1()
                .border_color(if recording {
                    hsla(p.ring)
                } else {
                    hsla(p.input)
                })
                .bg(if recording {
                    hsla(p.secondary)
                } else {
                    hsla(p.card)
                })
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    let _ = weak_record.update(cx, |view, cx| {
                        if view.browser.snapshot.recording {
                            view.stop_snapshot_recording(cx);
                        } else {
                            view.start_snapshot_recording(cx);
                        }
                    });
                })
                .child(if recording {
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(crate::i18n::t(cx, "Press shortcut\u{2026}").to_owned())
                        .into_any_element()
                } else {
                    render_shortcut_keycaps(&candidate, cx)
                });

            let mut shortcut_controls = h_flex().items_center().gap(r(6.)).child(recorder_trigger);
            if shortcut_changed {
                shortcut_controls = shortcut_controls
                    .child(
                        Button::new("snap-shot-shortcut-save", ButtonVariant::Default, ButtonSize::Xs)
                            .label("Save")
                            .disabled(!can_save_shortcut)
                            .on_click(move |_, _, cx| {
                                store::update_client(
                                    &save_shortcut_state,
                                    ClientSettingChange::SnapShotShortcut(save_candidate.clone()),
                                    cx,
                                );
                                let _ = weak_save.update(cx, |view, cx| {
                                    view.browser.snapshot.candidate = None;
                                    view.stop_snapshot_recording(cx);
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("snap-shot-shortcut-cancel", ButtonVariant::Ghost, ButtonSize::Xs)
                            .label("Cancel")
                            .on_click(move |_, _, cx| {
                                let _ = weak_cancel.update(cx, |view, cx| {
                                    view.browser.snapshot.candidate = None;
                                    view.stop_snapshot_recording(cx);
                                    cx.notify();
                                });
                            }),
                    );
            } else {
                shortcut_controls = shortcut_controls.child(
                    Button::new("snap-shot-test-capture", ButtonVariant::Outline, ButtonSize::Xs)
                        .label("Test capture")
                        .on_click(|_, _, cx| {
                            if let Some(coordinator) =
                                crate::snapshot_capture::active_coordinator(cx)
                            {
                                coordinator.update(cx, |c, cx| c.capture_now(false, cx));
                            }
                        }),
                );
            }

            let mut shortcut_row = Row::new("snap-shot-shortcut", "Capture shortcut")
                .description("Capture the window you're using without switching apps.")
                .highlighted(self.is_target("snap-shot-shortcut"))
                .reset(
                    !same_snap_shot_shortcut(&saved_shortcut, &SnapShotShortcut::BothShiftKeys, MAC),
                    move |_, _, cx| {
                        store::update_client(
                            &reset_shortcut_state,
                            ClientSettingChange::SnapShotShortcut(SnapShotShortcut::BothShiftKeys),
                            cx,
                        );
                        let _ = weak_reset.update(cx, |view, cx| {
                            view.browser.snapshot.candidate = None;
                            view.stop_snapshot_recording(cx);
                            cx.notify();
                        });
                    },
                )
                .control(shortcut_controls);
            if let Some(status) = shortcut_status {
                shortcut_row = shortcut_row.status(status);
            }
            if recording {
                let presets = h_flex().flex_wrap().gap(r(6.)).pt(r(6.)).children(
                    SnapShotModifier::ALL.iter().copied().map(|modifier| {
                        let preset = SnapShotShortcut::from_modifier_pair(modifier);
                        let weak_preset = cx.entity().downgrade();
                        Button::new(
                            SharedString::from(format!("snap-shot-preset-{}", modifier.as_str())),
                            ButtonVariant::Outline,
                            ButtonSize::Xs,
                        )
                        .label(modifier.pair_label(MAC))
                        .on_click(move |_, _, cx| {
                            let _ = weak_preset.update(cx, |view, cx| {
                                view.browser.snapshot.candidate = Some(preset.clone());
                                view.stop_snapshot_recording(cx);
                                cx.notify();
                            });
                        })
                    }),
                );
                shortcut_row = shortcut_row.below(presets);
            }
            rows.push(shortcut_row.into_any_element());

            let sound_selection = doc.snap_shot_sound_selection();
            rows.push(
                Row::new("snap-shot-sound", "Capture sound")
                    .description("Choose the sound played when capture starts.")
                    .highlighted(self.is_target("snap-shot-sound"))
                    .reset(
                        sound_selection != SnapShotSoundSelection::Sound(SnapShotSound::SoftPop),
                        reset(
                            &state,
                            ClientSettingChange::SnapShotSoundSelection(
                                SnapShotSoundSelection::Sound(SnapShotSound::SoftPop),
                            ),
                        ),
                    )
                    .control(self.snapshot_sound_menu(sound_selection, cx))
                    .into_any_element(),
            );

            let flash_state = state.clone();
            rows.push(
                Row::new("snap-shot-flash", "Capture flash")
                    .description("Show a gentle cue on the captured window.")
                    .highlighted(self.is_target("snap-shot-flash"))
                    .reset(
                        !doc.snap_shot_flash(),
                        reset(&state, ClientSettingChange::SnapShotFlash(true)),
                    )
                    .control(
                        Switch::new("snap-shot-flash-switch", doc.snap_shot_flash()).on_change(
                            move |on, _, cx| {
                                store::update_client(
                                    &flash_state,
                                    ClientSettingChange::SnapShotFlash(on),
                                    cx,
                                );
                            },
                        ),
                    )
                    .into_any_element(),
            );

            let anim_state = state.clone();
            rows.push(
                Row::new("snap-shot-animations", "Capture animations")
                    .description("Animate captured windows into your draft.")
                    .highlighted(self.is_target("snap-shot-animations"))
                    .reset(
                        !doc.snap_shot_animations(),
                        reset(&state, ClientSettingChange::SnapShotAnimations(true)),
                    )
                    .control(
                        Switch::new("snap-shot-animations-switch", doc.snap_shot_animations())
                            .on_change(move |on, _, cx| {
                                store::update_client(
                                    &anim_state,
                                    ClientSettingChange::SnapShotAnimations(on),
                                    cx,
                                );
                            }),
                    )
                    .into_any_element(),
            );
        }

        vec![Block::new(
            &[
                "snap-shot",
                "snap-shot-enabled",
                "snap-shot-accessibility",
                "snap-shot-shortcut",
                "snap-shot-sound",
                "snap-shot-flash",
                "snap-shot-animations",
            ],
            section("SnapShots", rows, cx),
        )]
    }
}

/// Resolves each diff color scheme option's own swatch pair `(first, second)` without letting
/// the currently active scheme overwrite the inactive option's dots in the dropdown popup.
fn diff_swatch_colors(option_value: &str, active_scheme: DiffColorScheme, dark: bool, p: &crate::theme::Palette) -> (u32, u32) {
    if option_value == "blue-orange" {
        (p.blue_orange_diff_addition, p.blue_orange_diff_deletion)
    } else if active_scheme == DiffColorScheme::RedGreen {
        (p.diff_deletion, p.diff_addition)
    } else {
        let base = crate::theme::BUILT_IN_THEMES
            .iter()
            .find_map(|theme| if dark { theme.dark } else { theme.light })
            .unwrap_or(*p);
        (base.diff_deletion, base.diff_addition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_swatch_colors_distinguish_both_options_regardless_of_active_scheme() {
        let base = crate::theme::BUILT_IN_THEMES[0].dark.expect("default dark palette");
        let mut blue_active = base;
        blue_active.diff_addition = base.blue_orange_diff_addition;
        blue_active.diff_deletion = base.blue_orange_diff_deletion;

        let rg_when_rg = diff_swatch_colors("red-green", DiffColorScheme::RedGreen, true, &base);
        let bo_when_rg = diff_swatch_colors("blue-orange", DiffColorScheme::RedGreen, true, &base);
        assert_eq!(rg_when_rg, (base.diff_deletion, base.diff_addition));
        assert_eq!(bo_when_rg, (base.blue_orange_diff_addition, base.blue_orange_diff_deletion));
        assert_ne!(rg_when_rg, bo_when_rg);

        let rg_when_bo = diff_swatch_colors("red-green", DiffColorScheme::BlueOrange, true, &blue_active);
        let bo_when_bo = diff_swatch_colors("blue-orange", DiffColorScheme::BlueOrange, true, &blue_active);
        assert_eq!(rg_when_bo, (base.diff_deletion, base.diff_addition));
        assert_eq!(bo_when_bo, (base.blue_orange_diff_addition, base.blue_orange_diff_deletion));
        assert_ne!(rg_when_bo, bo_when_bo);
    }

    #[test]
    fn preview_preferences_keep_contract_defaults_and_report_saved_frame_rate() {
        let mut settings = ClientSettingsDoc::default();
        assert!(browser_auto_show_floating_preview(&settings));
        assert_eq!(browser_profile_report_payload(&settings)["recordingFrameRate"], 30);
        settings.apply_change(&client_preference(AUTO_SHOW_FLOATING_PREVIEW, false));
        settings.apply_change(&client_preference(RECORDING_FRAME_RATE, 60_u64));
        assert!(!browser_auto_show_floating_preview(&settings));
        assert_eq!(browser_profile_report_payload(&settings)["recordingFrameRate"], 60);
        settings.apply_change(&client_preference(RECORDING_FRAME_RATE, 24_u64));
        assert_eq!(browser_profile_report_payload(&settings)["recordingFrameRate"], 30);
    }

    #[test]
    fn zoom_value_keys_round_trip_every_zoom_level() {
        for &level in &ZOOM_LEVELS {
            let key = zoom_value_key(level);
            let matched = ZOOM_LEVELS.iter().copied().find(|candidate| zoom_value_key(*candidate) == key);
            assert_eq!(matched, Some(level));
        }
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod gpui_tests {
    use gpui::{AppContext, Context, IntoElement, ParentElement, Render, Window};
    use serde_json::json;
    use t3_core::client_settings::ClientSettingChange;
    use t3_core::preview::viewport::{
        AppearancePreference, BrowserProfile, BrowserProfileKind, ViewportSetting, preset_setting,
    };
    use t3_core::settings::search::Section;

    struct SettingsFrame;

    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            gpui::div().children(crate::settings::overlay(window, cx))
        }
    }

    #[gpui::test]
    fn browser_settings_page_renders_live_profiles_viewport_zoom_and_appearance_controls(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.update(|cx| {
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            crate::preview::init(state.clone(), cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": {
                            "environmentId": "fixture-env",
                            "label": "Studio Mac",
                            "platform": { "os": "darwin", "arch": "arm64" },
                            "capabilities": { "serverBrowser": true }
                        },
                        "settings": { "enableAgentBrowserAccess": true }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
            let preset = preset_setting("iphone-12-pro").unwrap();
            let mut scratch = t3_core::client_settings::ClientSettingsDoc::default();
            let mut patch = serde_json::Map::new();
            for change in [
                ClientSettingChange::BrowserProfiles(vec![BrowserProfile {
                    id: "work".into(),
                    name: "Work".into(),
                    kind: BrowserProfileKind::Persistent,
                }]),
                ClientSettingChange::BrowserDefaultProfileId("work".into()),
                ClientSettingChange::BrowserDefaultViewport(preset),
                ClientSettingChange::BrowserDefaultZoomFactor(1.25),
                ClientSettingChange::BrowserDefaultAppearance(AppearancePreference::Dark),
            ] {
                patch.extend(scratch.apply_change(&change));
            }
            crate::settings::store::install_committed_client(&state, patch, cx);
        });

        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|_, cx| crate::settings::open_at(Section::Browser, Some("browser-default-viewport".to_owned()), cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });

        vcx.read(|cx| {
            let doc = crate::settings::store::client(cx);
            assert_eq!(doc.browser_default_viewport(), preset_setting("iphone-12-pro").unwrap());
            assert_eq!(doc.browser_default_zoom_factor(), 1.25);
            assert_eq!(doc.browser_default_appearance(), AppearancePreference::Dark);
            assert_eq!(doc.browser_default_profile_id(), "work");
            assert_eq!(doc.browser_user_profiles().len(), 1);
            let previews = crate::preview::previews(cx).expect("previews initialized");
            let defaults = previews.read(cx).browser_defaults(cx);
            assert_eq!(defaults.viewport, ViewportSetting::Preset { width: 390, height: 844, preset_id: "iphone-12-pro".into() });
            assert_eq!(defaults.zoom_factor, 1.25);
            assert_eq!(defaults.appearance, AppearancePreference::Dark);
            assert_eq!(defaults.profile_id, "work");
        });

        state.update(&mut vcx, |state, _| state.stop_pump());
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn snapshots_settings_page_renders_live_controls_and_unsupported_fallback(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::sync::Arc;
        use t3_core::settings::snapshot::{SnapShotSound, SnapShotSoundSelection};

        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mock = Arc::new(crate::snapshot_capture::MockSnapShotPlatform::macos_ready());
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::snapshot_capture::install_platform(mock.clone(), cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.update(|cx| {
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            let mut scratch = t3_core::client_settings::ClientSettingsDoc::default();
            let mut patch = serde_json::Map::new();
            for change in [
                ClientSettingChange::SnapShotEnabled(true),
                ClientSettingChange::SnapShotSoundSelection(SnapShotSoundSelection::Sound(
                    SnapShotSound::CameraShutter,
                )),
                ClientSettingChange::SnapShotFlash(false),
                ClientSettingChange::SnapShotAnimations(false),
                ClientSettingChange::SnapShotIncludeAccessibility(false),
            ] {
                patch.extend(scratch.apply_change(&change));
            }
            crate::settings::store::install_committed_client(&state, patch, cx);
        });

        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|_, cx| {
            crate::settings::open_at(Section::SnapShot, Some("snap-shot-enabled".to_owned()), cx)
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });

        vcx.read(|cx| {
            let doc = crate::settings::store::client(cx);
            assert!(doc.snap_shot_enabled());
            assert_eq!(
                doc.snap_shot_sound_selection(),
                SnapShotSoundSelection::Sound(SnapShotSound::CameraShutter)
            );
            assert!(!doc.snap_shot_flash());
            assert!(!doc.snap_shot_animations());
            assert!(!doc.snap_shot_include_accessibility());
        });

        // Verify unsupported fallback renders cleanly too.
        mock.supported.store(false, std::sync::atomic::Ordering::SeqCst);
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });

        state.update(&mut vcx, |state, _| state.stop_pump());
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn snapshot_recorder_clears_suppression_on_section_switch_setup_open_and_settings_close(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mock = Arc::new(crate::snapshot_capture::MockSnapShotPlatform::macos_ready());
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::snapshot_capture::install_platform(mock.clone(), cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            let mut scratch = t3_core::client_settings::ClientSettingsDoc::default();
            let patch = scratch.apply_change(&ClientSettingChange::SnapShotEnabled(true));
            crate::settings::store::install_committed_client(&state, patch, cx);
        });

        let state_for_win = state.clone();
        let window = cx.add_window(|window, cx| {
            let composer = cx.new(|cx| crate::composer::Composer::new(state_for_win.clone(), window, cx));
            let coordinator = cx.new(|cx| {
                crate::snapshot_capture::SnapShotCoordinator::new(
                    state_for_win.clone(),
                    composer,
                    window,
                    cx,
                )
            });
            let setup_dialog = cx.new(|cx| {
                crate::snapshot_capture::SnapShotSetupDialog::new(
                    state_for_win.clone(),
                    coordinator.clone(),
                    window,
                    cx,
                )
            });
            crate::snapshot_capture::register_workspace_handles(coordinator, setup_dialog, cx);
            SettingsFrame
        });
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|_, cx| crate::settings::open_at(Section::SnapShot, None, cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });

        assert!(!mock.active_listener_stopped.load(Ordering::SeqCst));

        // 1. Start recording on SnapShot section -> global shortcut suppressed.
        let settings_view = vcx.read(|cx| cx.global::<super::super::Host>().view.clone().expect("settings view"));
        vcx.update(|_, cx| {
            settings_view.update(cx, |view, cx| view.start_snapshot_recording(cx));
        });
        assert!(mock.active_listener_stopped.load(Ordering::SeqCst));

        // 2. Switching to another section stops recording and restores the global shortcut.
        vcx.update(|_, cx| {
            settings_view.update(cx, |view, cx| view.navigate(Section::Diffs, None, cx));
        });
        vcx.run_until_parked();
        assert!(!settings_view.read_with(&vcx, |view, _| view.browser.snapshot.recording));
        assert!(!mock.active_listener_stopped.load(Ordering::SeqCst));

        // 3. Opening SnapShotSetupDialog while recording stops Settings recording.
        vcx.update(|_, cx| {
            settings_view.update(cx, |view, cx| {
                view.navigate(Section::SnapShot, None, cx);
                view.start_snapshot_recording(cx);
            });
        });
        assert!(mock.active_listener_stopped.load(Ordering::SeqCst));
        vcx.update(|_, cx| {
            let dialog = crate::snapshot_capture::active_setup_dialog(cx).expect("setup dialog");
            dialog.update(cx, |d, cx| d.open(None, cx));
        });
        vcx.run_until_parked();
        assert!(!settings_view.read_with(&vcx, |view, _| view.browser.snapshot.recording));
        assert!(!mock.active_listener_stopped.load(Ordering::SeqCst));

        // 4. Closing Settings while recording triggers `on_release` and restores the global shortcut.
        vcx.update(|_, cx| {
            let dialog = crate::snapshot_capture::active_setup_dialog(cx).expect("setup dialog");
            dialog.update(cx, |d, cx| d.close(false, cx));
            settings_view.update(cx, |view, cx| view.start_snapshot_recording(cx));
        });
        assert!(mock.active_listener_stopped.load(Ordering::SeqCst));
        drop(settings_view);
        vcx.update(|window, cx| {
            super::super::finish_close(window, cx);
            window.refresh();
            window.draw(cx).clear();
        });
        vcx.run_until_parked();
        assert!(!mock.active_listener_stopped.load(Ordering::SeqCst));

        state.update(&mut vcx, |state, _| state.stop_pump());
        vcx.run_until_parked();
    }
}
