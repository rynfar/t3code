//! `DiffsSettingsPanel` / `DiffColorsSettingsRow` (`SettingsPanels.tsx`), `BrowserSettingsPanel`
//! (`IntegrationsSettings.tsx`, with `BrowserImportWizard.tsx`) and `SnapShotSettings.tsx`.
//! Electron capture, profiles/import, recording and window controls have no native backend.

use gpui::{
    App, AppContext, Context, Entity, Focusable, IntoElement, ParentElement, Styled, Subscription,
    Window, div,
};
use gpui_component::Sizable;
use gpui_component::input::{Input, InputEvent, InputState};
use t3_core::client_settings::{
    BrowserLinkTarget, BrowserSearchEngine, ClientSettingChange, DiffColorScheme, valid_search_template,
};
use t3_core::diff::Layout;
use t3_core::settings::ServerSettingChange;

use super::controls::{
    Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, muted_text, r, section,
    unavailable_group,
};
use super::{Block, SettingsView, store};
use crate::app_state::AppState;
use crate::icons::Icon;

const BROWSER_UNAVAILABLE: &str = "Browser profiles and cookie import, floating previews, viewport, zoom, appearance, and recordings require the Electron desktop app. They aren't available in the native client yet.";
const PR_UNAVAILABLE: &str = "Proactive panels aren't available in the native client yet.";
const SNAPSHOTS_UNAVAILABLE: &str = "SnapShots capture requires the Electron desktop app. The native client doesn't have window capture or global capture shortcuts yet.";
const ADVANCED: &[&str] = &[
    "browser-default-viewport",
    "browser-default-zoom",
    "browser-default-appearance",
];

pub(super) struct BrowserState {
    custom_url: Entity<InputState>,
    advanced: bool,
    _subscription: Subscription,
}

impl BrowserState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let custom_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com/search?q=%s"));
        let subscription =
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
        Self {
            custom_url,
            advanced: false,
            _subscription: subscription,
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
    value: &'static str,
    target: bool,
    cx: &App,
) -> gpui::AnyElement {
    Row::new(id, title)
        .description(description)
        .highlighted(target)
        .control(muted_text(value, cx))
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
                .control(select(&state, "diff-colors-select", DiffColorScheme::ALL, doc.diff_color_scheme(), DiffColorScheme::as_str, DiffColorScheme::label, ClientSettingChange::DiffColorScheme).label(if doc.diff_color_scheme() == DiffColorScheme::RedGreen { "Red & green" } else { "Blue & orange" }).popup_width(208.)).into_any_element(),
        ], cx);
        let pr_links_state = state.clone();
        let prs = section("Pull requests", vec![
            unavailable_group(PR_UNAVAILABLE, vec![
                unavailable("proactive-panels", "Open pull requests and changes panels proactively", "Open linked pull requests first. Otherwise, open Changes for edits to at least 3 files or 50 lines.", "Unavailable", self.is_target("proactive-panels"), cx),
            ], cx).into_any_element(),
            Row::new("pull-request-links-open-in-app", "Open pull requests in T3 Code")
                .description("Clicking a pull request link in an HTML page opens it in T3 Code. Turn off to open pull request links like any other link, on your git host.")
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
        let config = self.state.read(cx).server_config.config.as_ref();
        let enabled = config
            .and_then(|config| config.settings.enable_agent_browser_access)
            .unwrap_or(true);
        let available = config.is_some() && self.may("settings:write", cx);
        let state = self.state.clone();
        let reset_state = state.clone();
        let access = section(
            "Agent access",
            vec![
                Row::new("agent-browser-access", "Agent browser access")
                    .description(
                        "Allow agents to use the shared browser. Projects can override it.",
                    )
                    .highlighted(self.is_target("agent-browser-access"))
                    .unavailable((!available).then_some(
                        "Reconnect with settings write access to change agent browser access.",
                    ))
                    .reset(!enabled, move |_, _, cx| {
                        store::update_server(
                            &reset_state,
                            ServerSettingChange::AgentBrowserAccess(true),
                            cx,
                        )
                    })
                    .control(
                        Switch::new("agent-browser-access-switch", enabled)
                            .disabled(!available)
                            .on_change(move |on, _, cx| {
                                store::update_server(
                                    &state,
                                    ServerSettingChange::AgentBrowserAccess(on),
                                    cx,
                                )
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
        let mut browsing = vec![unavailable_group(BROWSER_UNAVAILABLE, vec![
            unavailable("browser-profiles", "Browser profiles", "Profiles separate cookies and logins. Incognito data is cleared when the app closes.", "Unavailable", self.is_target("browser") || self.is_target("browser-profiles"), cx),
            unavailable("browser-auto-show-floating-preview", "Auto-show floating preview", "Show the floating preview when an agent opens a browser or device unless the agent says otherwise.", "Unavailable", self.is_target("browser-auto-show-floating-preview"), cx),
        ], cx).into_any_element(),
            Row::new("browser-link-target", "Open links in")
                .description("Where links in HTML pages open. Hold ⌘ or Ctrl while clicking a link to open it in your default browser either way.")
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
                .control(select(&state, "browser-search-engine-select", BrowserSearchEngine::ALL, doc.browser_search_engine(), BrowserSearchEngine::as_str, BrowserSearchEngine::label, ClientSettingChange::BrowserSearchEngine)).into_any_element()];
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
            browsing.push(unavailable_group(BROWSER_UNAVAILABLE, vec![
                unavailable("browser-default-viewport", "Default browser viewport", "Tab size for you and agents. Fill fits the panel; other sizes show the device toolbar.", "Unavailable", self.is_target("browser-default-viewport"), cx),
                unavailable("browser-default-zoom", "Default browser zoom", "Page zoom applied to new browser tabs.", "Unavailable", self.is_target("browser-default-zoom"), cx),
                unavailable("browser-default-appearance", "Default browser appearance", "The color scheme pages are told to prefer. System follows your OS setting.", "Unavailable", self.is_target("browser-default-appearance"), cx),
            ], cx).into_any_element());
        }
        let recordings = section("Recordings", vec![unavailable_group(BROWSER_UNAVAILABLE, vec![
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

    pub(super) fn snapshots_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        vec![Block::new(
            &["snap-shot", "snap-shot-enabled"],
            section(
                "SnapShots",
                vec![
                    unavailable_group(
                        SNAPSHOTS_UNAVAILABLE,
                        vec![
                            Row::new("snap-shot-enabled", "SnapShots")
                                .description(
                                    "Capture a window and attach it to your current draft.",
                                )
                                .highlighted(self.is_target("snap-shot-enabled"))
                                .control(
                                    Switch::new("snap-shot-enabled-switch", false).disabled(true),
                                )
                                .into_any_element(),
                        ],
                        cx,
                    )
                    .into_any_element(),
                ],
                cx,
            ),
        )]
    }
}
