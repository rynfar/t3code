//! `DiffsSettingsPanel` / `DiffColorsSettingsRow` (`SettingsPanels.tsx`), `BrowserSettingsPanel`
//! (`IntegrationsSettings.tsx`, with `BrowserImportWizard.tsx`) and `SnapShotSettings.tsx`.
//! Server-streamed preview defaults (profiles, viewport, zoom, appearance, link target, search
//! engine, agent access) and macOS native SnapShots capture are live; Electron desktop cookie
//! import, floating preview, and WebView recordings remain Electron-only N/A.

use std::collections::HashMap;

use gpui::{
    App, AppContext, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, Styled, Subscription, Window, div, prelude::*,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use t3_core::client_settings::{
    BrowserLinkTarget, BrowserSearchEngine, ClientSettingChange, DiffColorScheme, valid_search_template,
};
use t3_core::diff::Layout;
use t3_core::preview::viewport::{
    AppearancePreference, BROWSER_PROFILE_MAX_COUNT, BROWSER_PROFILE_NAME_MAX_LENGTH,
    BrowserProfile, BrowserProfileKind, DEFAULT_BROWSER_PROFILE_ID, RESPONSIVE_SEED_SIZE,
    VIEWPORT_PRESETS, ViewportSetting, find_viewport_preset, is_built_in_browser_profile_id,
    next_unique_profile_name, preset_setting, rotate_viewport, validate_viewport_dimensions,
    viewport_select_value, zoom_label,
};
use t3_core::preview::{DEFAULT_ZOOM, ZOOM_LEVELS, method as preview_method};
use t3_core::settings::ServerSettingChange;
use t3_core::settings::snapshot::{
    SnapShotBackendState, SnapShotMode, SnapShotModifier, SnapShotShortcut, SnapShotSound,
    SnapShotSoundSelection, same_snap_shot_shortcut, snap_shot_description,
    snap_shot_setup_button_label, snap_shot_setup_complete, snap_shot_shortcut_status,
    snap_shot_status,
};

use super::controls::{
    Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, muted_text, r, section,
    unavailable_group,
};
use super::{Block, SettingsView, store};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::snapshot_capture::setup_dialog::{
    render_shortcut_keycaps, shortcut_conflict_message, shortcut_from_keystroke,
};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

const MAC: bool = cfg!(target_os = "macos");
const BROWSER_ELECTRON_ONLY: &str = "Only available in the desktop app.";
const ADVANCED: &[&str] = &[
    "browser-default-viewport",
    "browser-default-zoom",
    "browser-default-appearance",
];

#[derive(Default)]
struct SnapShotPageState {
    candidate: Option<SnapShotShortcut>,
    recording: bool,
}

pub(super) struct BrowserState {
    pub(super) custom_url: Entity<InputState>,
    viewport_width: Entity<InputState>,
    viewport_height: Entity<InputState>,
    profile_inputs: HashMap<String, (Entity<InputState>, Subscription)>,
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
        Self {
            custom_url,
            viewport_width,
            viewport_height,
            profile_inputs: HashMap::new(),
            advanced: false,
            snapshot: SnapShotPageState::default(),
            _subscriptions: vec![url_sub, width_sub, height_sub, snap_key_sub],
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

fn clear_server_profile(
    state: Entity<AppState>,
    profile_id: String,
    profile_name: String,
    remove_after: bool,
    cx: &mut App,
) {
    let Some(task) = state.read(cx).request(
        preview_method::CLEAR_PROFILE,
        serde_json::json!({ "profileId": profile_id }),
        Ok,
    ) else {
        let title = if remove_after {
            crate::i18n::t(cx, "Connect to an environment before removing this profile.").to_owned()
        } else {
            crate::i18n::t_fmt(cx, "Could not clear {name}''s data", &[("name", profile_name.as_str().into())])
        };
        state.update(cx, |state, cx| {
            state.show_toast(Toast::new(ToastKind::Error, title), cx);
        });
        return;
    };
    cx.spawn(async move |cx| {
        let result = task.await.unwrap_or_else(|_| Err("Disconnected.".to_owned()));
        cx.update(|cx| match result {
            Ok(_) => {
                if remove_after {
                    let mut profiles = store::client(cx).browser_user_profiles();
                    profiles.retain(|profile| profile.id != profile_id);
                    let was_default = store::client(cx).browser_default_profile_id() == profile_id;
                    store::update_client(&state, ClientSettingChange::BrowserProfiles(profiles), cx);
                    if was_default {
                        store::update_client(
                            &state,
                            ClientSettingChange::BrowserDefaultProfileId(DEFAULT_BROWSER_PROFILE_ID.to_owned()),
                            cx,
                        );
                    }
                    report_browser_profiles(cx);
                } else {
                    let title = crate::i18n::t_fmt(
                        cx,
                        "Cleared {name}''s cookies and cache",
                        &[("name", profile_name.as_str().into())],
                    );
                    state.update(cx, |state, cx| {
                        state.show_toast(Toast::new(ToastKind::Success, title), cx);
                    });
                }
            }
            Err(_) => {
                let title = if remove_after {
                    crate::i18n::t(cx, "Profile data could not be deleted. Try again.").to_owned()
                } else {
                    crate::i18n::t_fmt(cx, "Could not clear {name}''s data", &[("name", profile_name.as_str().into())])
                };
                state.update(cx, |state, cx| {
                    state.show_toast(Toast::new(ToastKind::Error, title), cx);
                });
            }
        });
    })
    .detach();
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
    value: &'static str,
    target: bool,
    cx: &App,
) -> gpui::AnyElement {
    Row::new(id, title)
        .description(description)
        .highlighted(target)
        .control(muted_text(crate::i18n::t(cx, value), cx))
        .into_any_element()
}

impl SettingsView {
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
        let has_server_browser = self
            .state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .is_some_and(|cfg| cfg.environment.capabilities.server_browser == Some(true));
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
                let profile_id_default = profile.id.clone();
                let state_default = state.clone();
                let profile_id_clear = profile.id.clone();
                let profile_name_clear = profile.name.clone();
                let state_clear = state.clone();
                let profile_id_remove = profile.id.clone();
                let profile_name_remove = profile.name.clone();
                let state_remove = state.clone();

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
                        .child(Input::new(input).small())
                        .into_any_element()
                } else {
                    div()
                        .truncate()
                        .text_sm()
                        .text_color(hsla(p.foreground))
                        .child(profile.name.clone())
                        .into_any_element()
                };

                let mut clear_btn = Button::new(
                    SharedString::from(format!("browser-profile-clear-{}", profile.id)),
                    ButtonVariant::GhostMuted,
                    ButtonSize::Xs,
                )
                .label("Clear cookies and cache")
                .disabled(!has_server_browser)
                .on_click(move |_, _, cx| {
                    clear_server_profile(
                        state_clear.clone(),
                        profile_id_clear.clone(),
                        profile_name_clear.clone(),
                        false,
                        cx,
                    );
                });
                if !has_server_browser {
                    clear_btn = clear_btn.tooltip("Connect to an environment to clear profile data");
                }

                let mut actions = h_flex().flex_none().items_center().gap(r(4.));
                if !is_default {
                    actions = actions.child(
                        Button::new(
                            SharedString::from(format!("browser-profile-default-{}", profile.id)),
                            ButtonVariant::GhostMuted,
                            ButtonSize::Xs,
                        )
                        .label("Set as default")
                        .disabled(is_project_scope)
                        .on_click(move |_, _, cx| {
                            store::update_client(
                                &state_default,
                                ClientSettingChange::BrowserDefaultProfileId(profile_id_default.clone()),
                                cx,
                            );
                            report_browser_profiles(cx);
                        }),
                    );
                }
                actions = actions.child(clear_btn);
                if !built_in {
                    let mut remove_btn = Button::new(
                        SharedString::from(format!("browser-profile-remove-{}", profile.id)),
                        ButtonVariant::DestructiveOutline,
                        ButtonSize::Xs,
                    )
                    .icon(Icon::Trash2)
                    .tooltip(if has_server_browser {
                        "Remove profile and data"
                    } else {
                        "Connect to an environment to remove this profile and its data."
                    })
                    .disabled(!has_server_browser)
                    .on_click(move |_, _, cx| {
                        clear_server_profile(
                            state_remove.clone(),
                            profile_id_remove.clone(),
                            profile_name_remove.clone(),
                            true,
                            cx,
                        );
                    });
                    if has_server_browser {
                        remove_btn = remove_btn.label("Remove profile");
                    }
                    actions = actions.child(remove_btn);
                }

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
                    .child(actions)
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
                        .child(Input::new(&self.browser.custom_url).small()),
                )
                .into_any_element(),
            );
        }
        browsing.push(
            unavailable_group(
                BROWSER_ELECTRON_ONLY,
                vec![
                    unavailable(
                        "browser-auto-show-floating-preview",
                        "Auto-show floating preview",
                        "Show the floating preview when an agent opens a browser or device unless the agent says otherwise.",
                        "Unavailable",
                        self.is_target("browser-auto-show-floating-preview"),
                        cx,
                    ),
                    unavailable(
                        "browser-cookie-import",
                        "Import from",
                        "Importing browser cookies from installed desktop browsers requires the Electron desktop app.",
                        "Unavailable",
                        false,
                        cx,
                    ),
                ],
                cx,
            )
            .into_any_element(),
        );
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
        let recordings = section("Recordings", vec![unavailable_group(BROWSER_ELECTRON_ONLY, vec![
            unavailable("browser-recording-frame-rate", "Browser recording frame rate", "Maximum recording rate. 30 fps saves CPU and storage; 60 fps is smoother.", "Unavailable", self.is_target("browser-recording-frame-rate"), cx),
            unavailable("browser-recording-key-presses", "Show key presses in recordings", "Show pressed keys and shortcuts in new recordings. Password fields are excluded.", "Unavailable", self.is_target("browser-recording-key-presses"), cx),
            unavailable("browser-recording-mouse-presses", "Show mouse presses in recordings", "Highlight mouse presses and held buttons in new recordings.", "Unavailable", self.is_target("browser-recording-mouse-presses"), cx),
        ], cx).into_any_element()], cx);
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
        if enabled {
            enable_controls = enable_controls.child(
                Button::new("snap-shot-setup-btn", ButtonVariant::Outline, ButtonSize::Xs)
                    .label(setup_btn_label)
                    .on_click(|_, _, cx| {
                        if let Some(dialog) = crate::snapshot_capture::active_setup_dialog(cx) {
                            dialog.update(cx, |d, cx| d.open(None, cx));
                        }
                    }),
            );
        }
        enable_controls = enable_controls.child(
            Switch::new("snap-shot-enabled-switch", enabled).on_change(move |checked, _, cx| {
                if !checked {
                    store::update_client(
                        &switch_state,
                        ClientSettingChange::SnapShotEnabled(false),
                        cx,
                    );
                } else if !setup_complete
                    && let Some(dialog) = crate::snapshot_capture::active_setup_dialog(cx)
                {
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
            let sound_state = state.clone();
            let sound_select = Select::new(
                "snap-shot-sound-select",
                SnapShotSoundSelection::ALL
                    .iter()
                    .map(|opt| SelectOption::new(opt.as_str(), opt.label()))
                    .collect(),
            )
            .selected(sound_selection.as_str())
            .width(176.)
            .show_check()
            .on_change(move |next, _, cx| {
                if let Some(selection) = SnapShotSoundSelection::parse(next.as_ref()) {
                    store::update_client(
                        &sound_state,
                        ClientSettingChange::SnapShotSoundSelection(selection),
                        cx,
                    );
                }
            });
            let preview_sound = match sound_selection {
                SnapShotSoundSelection::Off => None,
                SnapShotSoundSelection::Sound(sound) => Some(sound),
            };
            let sound_controls = h_flex()
                .items_center()
                .gap(r(6.))
                .child(sound_select)
                .child(
                    Button::new("snap-shot-sound-preview", ButtonVariant::Outline, ButtonSize::IconSm)
                        .icon(Icon::Play)
                        .tooltip(match preview_sound {
                            Some(SnapShotSound::SoftPop) => "Play Whoosh",
                            Some(SnapShotSound::CameraShutter) => "Play Click",
                            None => "Sound is off",
                        })
                        .disabled(preview_sound.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(sound) = preview_sound {
                                crate::snapshot_capture::platform(cx).play_sound(sound);
                            }
                        }),
                );
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
                    .control(sound_controls)
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
    use gpui::{Context, IntoElement, ParentElement, Render, Window};
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
}
