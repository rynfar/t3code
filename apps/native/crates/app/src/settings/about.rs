//! About: ports `AboutSettingsPanel`, `AboutVersionTitle`, `MobileAppSettingsRow`, and
//! `OpenSourceLicensesPanel` (`SettingsPanels.tsx`, `AboutVersionTitle.tsx`,
//! `MobileAppSettingsRow.tsx`, `OpenSourceLicenses.tsx`).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AppContext, Bounds, ClipboardItem, Context, Entity, Focusable, FontWeight,
    IntoElement, ParentElement, SharedString, Styled, Subscription, Task, Window, canvas, div,
    fill, point, prelude::*, px, size,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use t3_core::managed_policy;
use t3_core::onboarding::{CliCommandStatus, INSTALL_T3_COMMAND_UNIX, profile_contains_line};
use t3_core::settings::{
    ThirdPartyLicenseManifest, decode_third_party_license_manifest, filter_third_party_license_entries,
    format_license_bundles, third_party_license_entry_key,
};
use t3_core::updates::{
    UpdateTrack, channel_switch_label, managed_updates_message, managed_updates_off_denial,
    nightly_denied_by,
};

use super::controls::{
    Button, ButtonSize, ButtonVariant, InlineButton, InlineTone, Row, Select, SelectOption, group,
    icon, r, section, section_title,
};
use super::{Block, SettingsView};
use crate::i18n::t;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};
use crate::updates::{ManualCheckStatus, UpdateTrackSwitchDialog};

const PRIVACY_POLICY_URL: &str = "https://t3.codes/privacy-policy";
const APP_STORE_URL: &str = "https://apps.apple.com/us/app/t3-code-remote-claude-more/id6787819824";
const PLAY_STORE_URL: &str = "https://play.google.com/store/apps/details?id=com.t3tools.t3code";
const LICENSE_MANIFEST_MAX_BYTES: usize = 16 * 1024 * 1024;
const QR_SIZE: f32 = 128.;

fn bundle_resource(relative: &str) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let macos = executable.parent()?;
    (macos.file_name()? == "MacOS").then(|| macos.join("../Resources").join(relative))
}

fn bundled_license_manifest() -> Option<Result<ThirdPartyLicenseManifest, String>> {
    use std::io::Read;
    let path = bundle_resource("third-party-licenses.json")?;
    let file = std::fs::File::open(path).ok()?;
    let result = (|| {
        let mut bytes = Vec::new();
        file.take(LICENSE_MANIFEST_MAX_BYTES as u64 + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() > LICENSE_MANIFEST_MAX_BYTES { return None; }
        serde_json::from_slice(&bytes).ok()
    })();
    Some(result.ok_or_else(|| "The bundled license manifest could not load.".to_owned())
        .and_then(|value| decode_third_party_license_manifest(&value)))
}

fn cli_source() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::var_os("T3_NATIVE_T3_BIN").map(PathBuf::from).or_else(|| bundle_resource("bin/t3"))?;
        let path = path.canonicalize().ok()?;
        let meta = path.metadata().ok()?;
        (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(path)
    }
    #[cfg(not(unix))]
    None
}

fn owns_cli_link(destination: &Path, source: &Path) -> bool {
    std::fs::read_link(destination).is_ok_and(|target| target == source)
}

#[cfg(unix)]
fn set_cli_link(destination: &Path, source: &Path, install: bool) -> std::io::Result<()> {
    if install {
        if let Some(parent) = destination.parent() { std::fs::create_dir_all(parent)?; }
        std::os::unix::fs::symlink(source, destination)
    } else if owns_cli_link(destination, source) {
        std::fs::remove_file(destination)
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "The t3 command now belongs to another installation."))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MobileStore {
    Ios,
    Android,
}

impl MobileStore {
    const ALL: [MobileStore; 2] = [MobileStore::Ios, MobileStore::Android];

    fn id(self) -> &'static str {
        match self {
            MobileStore::Ios => "ios",
            MobileStore::Android => "android",
        }
    }

    fn icon(self) -> Icon {
        match self {
            MobileStore::Ios => Icon::Apple,
            MobileStore::Android => Icon::Android,
        }
    }

    fn label(self) -> &'static str {
        match self {
            MobileStore::Ios => "iPhone and iPad",
            MobileStore::Android => "Android",
        }
    }

    fn store(self) -> &'static str {
        match self {
            MobileStore::Ios => "App Store",
            MobileStore::Android => "Google Play",
        }
    }

    fn url(self) -> &'static str {
        match self {
            MobileStore::Ios => APP_STORE_URL,
            MobileStore::Android => PLAY_STORE_URL,
        }
    }
}

struct QrMatrix {
    width: usize,
    dark: Vec<bool>,
}

fn qr_matrix(text: &str) -> Option<QrMatrix> {
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    Some(QrMatrix {
        width: code.width(),
        dark: code.to_colors().into_iter().map(|color| color == qrcode::Color::Dark).collect(),
    })
}

fn qr_element(matrix: Rc<QrMatrix>) -> AnyElement {
    let element = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let width = matrix.width;
            let unit = f32::from(bounds.size.width) / (width + 2) as f32;
            window.paint_quad(fill(bounds, gpui::white()));
            for y in 0..width {
                let row = &matrix.dark[y * width..(y + 1) * width];
                let mut x = 0;
                while x < width {
                    if !row[x] {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    while x < width && row[x] {
                        x += 1;
                    }
                    let origin = point(
                        bounds.origin.x + px(unit * (start + 1) as f32),
                        bounds.origin.y + px(unit * (y + 1) as f32),
                    );
                    window.paint_quad(
                        fill(Bounds::new(origin, size(px(unit * (x - start) as f32), px(unit))), gpui::black()),
                    );
                }
            }
        },
    )
    .size(r(QR_SIZE));
    div().flex_none().rounded(r(6.)).overflow_hidden().child(element).into_any_element()
}

#[derive(Default)]
enum LicensesStatus {
    #[default]
    Idle,
    Loading,
    Loaded(ThirdPartyLicenseManifest),
    Failed(String),
}

const MOBILE_POPOVER_REOPEN_GUARD: Duration = Duration::from_millis(250);

pub(crate) struct AboutState {
    copied_version: bool,
    copied_cli_install: bool,
    mobile_popover: Option<MobileStore>,
    mobile_popover_dismissed: Option<(MobileStore, Instant)>,
    qr_cache: RefCell<Option<(MobileStore, Rc<QrMatrix>)>>,
    track_switch_dialog: Option<Entity<UpdateTrackSwitchDialog>>,
    cli_status: Option<CliCommandStatus>,
    cli_source: Option<PathBuf>,
    cli_link_owned: bool,
    cli_checked: bool,
    cli_path_added: bool,
    cli_path_saving: bool,
    cli_path_task: Option<Task<()>>,
    pub(super) licenses_open: bool,
    licenses_status: LicensesStatus,
    pub(super) licenses_search: Entity<InputState>,
    licenses_search_open: bool,
    licenses_expanded: Option<String>,
    licenses_load_id: u64,
    licenses_connection: Option<u64>,
    _subscription: Subscription,
}

impl AboutState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let licenses_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search licenses"));
        let subscription = cx.subscribe_in(&licenses_search, window, |this, input, event: &InputEvent, _, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::Blur if input.read(cx).value().is_empty() => {
                this.about.licenses_search_open = false;
                cx.notify();
            }
            _ => {}
        });
        AboutState {
            copied_version: false,
            copied_cli_install: false,
            mobile_popover: None,
            mobile_popover_dismissed: None,
            qr_cache: RefCell::new(None),
            track_switch_dialog: None,
            cli_status: None,
            cli_source: None,
            cli_link_owned: false,
            cli_checked: false,
            cli_path_added: false,
            cli_path_saving: false,
            cli_path_task: None,
            licenses_open: false,
            licenses_status: LicensesStatus::Idle,
            licenses_search,
            licenses_search_open: false,
            licenses_expanded: None,
            licenses_load_id: 0,
            licenses_connection: None,
            _subscription: subscription,
        }
    }

    pub(super) fn leave(&mut self) {
        self.mobile_popover = None;
        self.mobile_popover_dismissed = None;
        self.track_switch_dialog = None;
        self.cli_path_task = None;
        self.cli_status = None;
        self.cli_checked = false;
        self.cli_path_saving = false;
        self.licenses_open = false;
        self.licenses_search_open = false;
        self.licenses_expanded = None;
        if matches!(self.licenses_status, LicensesStatus::Failed(_)) {
            self.licenses_status = LicensesStatus::Idle;
        }
    }

    fn cached_qr(&self, store: MobileStore) -> Option<Rc<QrMatrix>> {
        let mut cache = self.qr_cache.borrow_mut();
        if let Some((cached, matrix)) = cache.as_ref()
            && *cached == store
        {
            return Some(matrix.clone());
        }
        let matrix = Rc::new(qr_matrix(store.url())?);
        *cache = Some((store, matrix.clone()));
        Some(matrix)
    }
}

fn is_http_url(value: &str) -> bool {
    let trimmed = value.trim();
    let Some(rest) = trimmed.strip_prefix("https://").or_else(|| trimmed.strip_prefix("http://")) else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty() && !trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn telemetry_off_message(policy: Option<&serde_json::Value>) -> Option<String> {
    let policy = policy?;
    (policy["limits"]["telemetry"] == false)
        .then(|| format!("{} turned off anonymous usage data.", managed_policy::organization(policy)))
}

impl SettingsView {
    pub(super) fn dismiss_about_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(dialog) = self.about.track_switch_dialog.take() {
            dialog.update(cx, |dialog, cx| dialog.close_without_callback(window, cx));
            self.focus.focus(window, cx);
            cx.notify();
            return true;
        }
        if self.about.mobile_popover.take().is_some() {
            cx.notify();
            return true;
        }
        if self.about.licenses_open {
            if self.about.licenses_search_open
                && (self.about.licenses_search.focus_handle(cx).is_focused(window)
                    || !self.about.licenses_search.read(cx).value().is_empty())
            {
                self.about.licenses_search.update(cx, |input, cx| input.set_value("", window, cx));
                self.about.licenses_search_open = false;
                self.focus.focus(window, cx);
                cx.notify();
                return true;
            }
            self.about.leave();
            self.focus.focus(window, cx);
            cx.notify();
            return true;
        }
        false
    }

    fn open_track_switch_dialog(&mut self, channel: UpdateTrack, window: &mut Window, cx: &mut Context<Self>) {
        self.about.mobile_popover = None;
        let weak = cx.entity().downgrade();
        let state = self.state.clone();
        let dialog = cx.new(|cx| {
            UpdateTrackSwitchDialog::new(
                state,
                channel,
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.about.track_switch_dialog = None;
                        cx.notify();
                    });
                },
                window,
                cx,
            )
        });
        self.about.track_switch_dialog = Some(dialog);
        cx.notify();
    }

    fn ensure_cli_status(&mut self, cx: &mut Context<Self>) {
        if self.about.cli_checked || self.about.cli_status.is_some() {
            return;
        }
        self.about.cli_checked = true;
        let load = cx.background_executor().spawn(async {
            let status = t3_client::local::cli_command_status();
            let source = cli_source();
            let owned = status.as_ref().zip(source.as_ref()).is_some_and(|(status, source)| {
                owns_cli_link(&Path::new(&status.path_dir).join("t3"), source)
            });
            (status, source, owned)
        });
        self.about.cli_path_task = Some(cx.spawn(async move |this, cx| {
            let (status, source, owned) = load.await;
            let _ = this.update(cx, |this, cx| {
                this.about.cli_status = status;
                this.about.cli_source = source;
                this.about.cli_link_owned = owned;
                this.about.cli_path_task = None;
                cx.notify();
            });
        }));
    }

    #[cfg(unix)]
    fn change_cli_installation(&mut self, install: bool, cx: &mut Context<Self>) {
        if self.about.cli_path_saving { return; }
        let Some(status) = self.about.cli_status.as_ref() else { return };
        let Some(source) = self.about.cli_source.clone() else { return };
        let destination = Path::new(&status.path_dir).join("t3");
        self.about.cli_path_saving = true;
        let action = cx.background_executor().spawn(async move {
            let result = set_cli_link(&destination, &source, install);
            let status = t3_client::local::cli_command_status();
            let owned = owns_cli_link(&destination, &source);
            (result, status, owned)
        });
        self.about.cli_path_task = Some(cx.spawn(async move |this, cx| {
            let (result, status, owned) = action.await;
            let _ = this.update(cx, |this, cx| {
                this.about.cli_status = status;
                this.about.cli_link_owned = owned;
                this.about.cli_path_saving = false;
                this.about.cli_path_task = None;
                if let Err(error) = result {
                    let title = t(cx, if install { "Could not install t3" } else { "Could not remove t3" });
                    this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, title).description(error.to_string()), cx));
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn copy_cli_install_command(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(INSTALL_T3_COMMAND_UNIX.to_owned()));
        self.about.copied_cli_install = true;
        self.state.update(cx, |state, cx| {
            state.show_toast(
                Toast::new(ToastKind::Success, "Copied install command")
                    .description(format!("Run `{INSTALL_T3_COMMAND_UNIX}` in your terminal to install `t3`.")),
                cx,
            );
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |this, cx| {
                this.about.copied_cli_install = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Reuses the onboarding shell-profile PATH append rules (`profile_contains_line` +
    /// `t3_client::local::cli_command_status()`) without duplicating detection logic.
    fn add_cli_to_path(&mut self, profile: String, export_line: String, cx: &mut Context<Self>) {
        if self.about.cli_path_saving {
            return;
        }
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    Toast::new(ToastKind::Error, "Could not add t3 to PATH")
                        .description("Could not locate your home directory.".to_owned()),
                    cx,
                );
            });
            return;
        };
        self.about.cli_path_saving = true;
        let path = profile
            .strip_prefix("~/")
            .map_or_else(|| std::path::PathBuf::from(&profile), |rest| home.join(rest));
        let task = cx.spawn(async move |this, cx| {
            let result: Result<Option<CliCommandStatus>, String> = cx
                .background_executor()
                .spawn(async move {
                    let existing = match std::fs::read_to_string(&path) {
                        Ok(content) => content,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
                    };
                    if !profile_contains_line(&existing, &export_line) {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent)
                                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
                        }
                        let prefix = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
                        let next = format!("{existing}{prefix}{export_line}\n");
                        std::fs::write(&path, next)
                            .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
                    }
                    Ok(t3_client::local::cli_command_status())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.about.cli_path_saving = false;
                this.about.cli_path_task = None;
                match result {
                    Ok(status) => {
                        this.about.cli_path_added = true;
                        if status.is_some() {
                            this.about.cli_status = status;
                        }
                    }
                    Err(error) => {
                        this.state.update(cx, |state, cx| {
                            state.show_toast(
                                Toast::new(ToastKind::Error, "Could not add t3 to PATH").description(error),
                                cx,
                            );
                        });
                    }
                }
                cx.notify();
            });
        });
        self.about.cli_path_task = Some(task);
        cx.notify();
    }

    fn copy_app_version(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(env!("CARGO_PKG_VERSION").to_owned()));
        self.about.copied_version = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |this, cx| {
                this.about.copied_version = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_licenses(&mut self, cx: &mut Context<Self>) {
        self.about.mobile_popover = None;
        self.about.licenses_open = true;
        self.about.licenses_expanded = None;
        let connection = self.state.read(cx).connection_id();
        if !matches!(self.about.licenses_status, LicensesStatus::Loaded(_))
            || self.about.licenses_connection != connection
        {
            self.load_licenses(cx);
        }
        cx.notify();
    }

    fn load_licenses(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let connection = state.connection_id();
        self.about.licenses_connection = connection;
        self.about.licenses_load_id += 1;
        let load_id = self.about.licenses_load_id;
        let http_base = state.active_environment().map(|env| env.http_base.clone());
        self.about.licenses_status = LicensesStatus::Loading;
        let runtime = state.runtime().clone();
        let bundled = cx.background_executor().spawn(async { bundled_license_manifest() });
        cx.spawn(async move |this, cx| {
            let result = match bundled.await {
                Some(result) => result,
                None => if let Some(http_base) = http_base {
                    runtime.spawn(async move {
                    let bytes = t3_client::asset::fetch_asset(&http_base, "/third-party-licenses.json", LICENSE_MANIFEST_MAX_BYTES)
                        .await.map_err(|_| "The license manifest could not load.".to_owned())?;
                    let value = serde_json::from_slice(&bytes).map_err(|_| "The license manifest could not load.".to_owned())?;
                    decode_third_party_license_manifest(&value)
                    }).await.unwrap_or_else(|_| Err("The license manifest could not load.".to_owned()))
                } else {
                    Err("Connect an environment to load open-source notices.".to_owned())
                },
            };
            let _ = this.update(cx, |this, cx| {
                if this.about.licenses_load_id != load_id || this.about.licenses_connection != connection {
                    return;
                }
                this.about.licenses_status = match result {
                    Ok(manifest) => LicensesStatus::Loaded(manifest),
                    Err(message) => LicensesStatus::Failed(message),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn about(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        if self.about.licenses_open {
            return self.open_source_licenses_page(cx);
        }
        self.ensure_cli_status(cx);
        let p = palette(cx);
        let policy = self
            .state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .and_then(|config| config.managed_policy.clone());

        let mut app_rows = Vec::new();
        if let Some(policy) = policy.as_ref() {
            let org = managed_policy::organization(policy).to_owned();
            let support_url = policy
                .get("supportUrl")
                .and_then(|v| v.as_str())
                .filter(|url| is_http_url(url))
                .map(str::to_owned);
            let mut row = Row::new("about-organization", "Organization")
                .description(format!("This environment is managed by {org}. Some settings and features follow its policy."));
            if let Some(url) = support_url {
                row = row.control(
                    div().debug_selector(|| "about-support-link".to_owned()).child(
                        Button::new("about-org-help", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Get help")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
                );
            }
            app_rows.push(row.into_any_element());
        }

        let version = env!("CARGO_PKG_VERSION");
        let copied = self.about.copied_version;
        let updates_denial = managed_updates_off_denial(policy.as_ref());
        let nightly_denied = nightly_denied_by(policy.as_ref());
        let selected_track = crate::updates::saved_update_track(cx);
        let update_run_active = crate::updates::has_active_update_run(cx);
        let updates_available = crate::updates::has_available_updates(self.state.read(cx), cx);
        let check_status = crate::updates::manual_check_status(cx);
        let version_description = if updates_available {
            "Update available.".to_owned()
        } else if let Some(denial) = updates_denial.clone() {
            denial
        } else {
            "Current version of the application.".to_owned()
        };
        let check_label = if updates_available {
            "Update"
        } else {
            match check_status {
                ManualCheckStatus::Checking => "Checking\u{2026}",
                ManualCheckStatus::UpToDate => "Up to Date",
                ManualCheckStatus::UpdateAvailable => "Update",
                ManualCheckStatus::Idle => "Check for updates",
            }
        };
        let check_disabled = updates_denial.is_some() || check_status == ManualCheckStatus::Checking;
        let check_state = self.state.clone();
        app_rows.push(
            Row::new("app-version", "Version")
                .description(version_description)
                .highlighted(self.is_target("app-version"))
                .unavailable(updates_denial.clone())
                .control(
                    h_flex()
                        .gap(r(8.))
                        .child(
                            div()
                                .debug_selector(|| "about-version-copy".to_owned())
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(
                                    InlineButton::new("copy-app-version", version, InlineTone::Muted)
                                        .tooltip(if copied { "Copied" } else { "Copy version" })
                                        .on_click(cx.listener(|this, _, _, cx| this.copy_app_version(cx))),
                                ),
                        )
                        .child(
                            div().debug_selector(|| "about-check-updates".to_owned()).child(
                                Button::new("about-check-updates-button", ButtonVariant::Outline, ButtonSize::Sm)
                                    .label(check_label)
                                    .disabled(check_disabled)
                                    .on_click(move |_, window, cx| {
                                        crate::updates::check_for_updates(&check_state, window, cx);
                                    }),
                            ),
                        ),
                )
                .into_any_element(),
        );

        let unswitched = crate::updates::unswitched_machine_count(self.state.read(cx), selected_track);
        let track_options = vec![
            SelectOption::new(UpdateTrack::Latest.as_str(), channel_switch_label(UpdateTrack::Latest)),
            SelectOption::new(UpdateTrack::Nightly.as_str(), channel_switch_label(UpdateTrack::Nightly))
                .disabled_reason(
                    nightly_denied
                        .as_ref()
                        .filter(|_| selected_track != UpdateTrack::Nightly)
                        .map(|org| format!("{} Nightly builds are not available.", managed_updates_message(org))),
                ),
        ];
        let track_disabled =
            self.about.track_switch_dialog.is_some() || update_run_active || updates_denial.is_some();
        let track_weak = cx.entity().downgrade();
        let mut track_row = Row::new("update-track", "Update track")
            .highlighted(self.is_target("update-track"))
            .unavailable(updates_denial);
        if let Some(org) = nightly_denied {
            let policy_msg = managed_updates_message(&org);
            track_row = track_row.description(if selected_track == UpdateTrack::Nightly {
                format!("{policy_msg} Nightly builds are not available; switch to Stable to keep getting updates.")
            } else {
                format!("{policy_msg} Nightly builds are not available.")
            });
        } else if unswitched > 0 {
            let other_label = channel_switch_label(selected_track.other());
            let count_copy = if unswitched == 1 {
                format!("1 machine still runs {other_label}.")
            } else {
                format!("{unswitched} machines still run {other_label}.")
            };
            let review_weak = cx.entity().downgrade();
            track_row = track_row.description_content(
                h_flex()
                    .flex_wrap()
                    .gap_x(r(6.))
                    .child(count_copy)
                    .child(
                        div().debug_selector(|| "about-track-review".to_owned()).child(
                            InlineButton::new("about-track-review-btn", "Review", InlineTone::Muted)
                                .on_click(move |_, window, cx| {
                                    if !update_run_active {
                                        let _ = review_weak.update(cx, |this, cx| {
                                            this.open_track_switch_dialog(selected_track, window, cx);
                                        });
                                    }
                                }),
                        ),
                    ),
            );
        } else {
            track_row = track_row.description(
                "Switches this app and every connected machine whose database the target release supports.",
            );
        }
        app_rows.push(
            track_row
                .control(
                    div().debug_selector(|| "about-update-track-select".to_owned()).child(
                        Select::new("update-track-select", track_options)
                            .selected(selected_track.as_str())
                            .width(160.)
                            .disabled(track_disabled)
                            .on_change(move |value, window, cx| {
                                if let Some(next) = UpdateTrack::parse(&value)
                                    && next != selected_track
                                {
                                    let _ = track_weak.update(cx, |this, cx| {
                                        this.open_track_switch_dialog(next, window, cx);
                                    });
                                }
                            }),
                    ),
                )
                .into_any_element(),
        );

        if let Some(status) = self.about.cli_status.clone() {
            let installed = status.installed_path.is_some();
            let cli_description = if let Some(shadowed) = &status.shadowed_by {
                format!("Another t3 at {shadowed} runs first in a new terminal. Remove it to use T3 Code's.")
            } else if let Some(installed_path) = &status.installed_path {
                if status.on_path {
                    format!("Installed at {installed_path}. Open a new terminal to use it.")
                } else if self.about.cli_path_added {
                    match &status.profile_file {
                        Some(file) => format!("Installed at {installed_path} and added to PATH in {file}. Open a new terminal to use it."),
                        None => format!("Installed at {installed_path} and added to PATH. Open a new terminal to use it."),
                    }
                } else if status.profile_file.is_some() {
                    format!("Installed at {installed_path}, which is not on your PATH yet.")
                } else {
                    format!("Installed at {installed_path}, which is not on your PATH yet. Add its folder to your PATH to run `t3`.")
                }
            } else {
                "Run T3 Code's CLI as `t3` from any terminal.".to_owned()
            };

            let mut cli_controls = h_flex().gap(r(8.));
            if installed
                && !status.on_path
                && !self.about.cli_path_added
                && let (Some(profile), Some(export_line)) = (status.profile_file.clone(), status.path_line.clone())
            {
                let saving = self.about.cli_path_saving;
                cli_controls = cli_controls.child(
                    div().debug_selector(|| "about-cli-add-path".to_owned()).child(
                        Button::new("about-cli-add-path-btn", ButtonVariant::Outline, ButtonSize::Sm)
                            .label(if saving { "Adding\u{2026}" } else { "Add to PATH" })
                            .disabled(saving)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_cli_to_path(profile.clone(), export_line.clone(), cx);
                            })),
                    ),
                );
            }
            #[cfg(unix)]
            if self.about.cli_source.is_some() && (!installed || self.about.cli_link_owned) {
                cli_controls = cli_controls.child(
                    Button::new("about-cli-link-action", ButtonVariant::Outline, ButtonSize::Sm)
                        .label(t(cx, if self.about.cli_link_owned { "Remove" } else { "Install" }))
                        .disabled(self.about.cli_path_saving || (!self.about.cli_link_owned && status.shadowed_by.is_some()))
                        .on_click(cx.listener(|this, _, _, cx| this.change_cli_installation(!this.about.cli_link_owned, cx))),
                );
            }
            if !installed && self.about.cli_source.is_none() {
                let copied_cli = self.about.copied_cli_install;
                cli_controls = cli_controls.child(
                    div().debug_selector(|| "about-cli-install".to_owned()).child(
                        Button::new("about-cli-install-btn", ButtonVariant::Outline, ButtonSize::Sm)
                            .label(if copied_cli { "Copied command" } else { "Install 't3' command" })
                            .disabled(status.shadowed_by.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.copy_cli_install_command(cx))),
                    ),
                );
            }

            let mut cli_row = Row::new("cli-command", "t3 command")
                .description(cli_description)
                .highlighted(self.is_target("cli-command"));
            if !installed || self.about.cli_link_owned || (!status.on_path && !self.about.cli_path_added && status.profile_file.is_some()) {
                cli_row = cli_row.control(cli_controls);
            }
            app_rows.push(cli_row.into_any_element());
        }

        let open_store = self.about.mobile_popover;
        let store_buttons = h_flex().gap(r(8.)).children(MobileStore::ALL.into_iter().map(|store| {
            let is_open = open_store == Some(store);
            let popover = is_open.then(|| {
                let qr = self.about.cached_qr(store);
                let url = store.url();
                let store_name = store.store();
                gpui::deferred(
                    gpui::anchored()
                        .anchor(gpui::Anchor::TopRight)
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            crate::composer::controls::popup_surface(cx)
                                .occlude()
                                .mt(r(4.))
                                .p(r(16.))
                                .gap(r(12.))
                                .items_center()
                                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                    if let Some(prev) = this.about.mobile_popover.take() {
                                        this.about.mobile_popover_dismissed = Some((prev, Instant::now()));
                                        cx.notify();
                                    }
                                }))
                                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child(store_name))
                                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Scan with your phone camera."))
                                .children(qr.map(|matrix| div().rounded(r(8.)).bg(gpui::white()).p(r(6.)).child(qr_element(matrix))))
                                .child(
                                    Button::new(SharedString::from(format!("open-store-{}", store.id())), ButtonVariant::Outline, ButtonSize::Sm)
                                        .label(format!("Open {store_name}"))
                                        .on_click(move |_, _, cx| cx.open_url(url)),
                                ),
                        ),
                )
                .with_priority(2)
            });
            div()
                .relative()
                .debug_selector(move || format!("mobile-app-{}", store.id()))
                .child(
                    Button::new(SharedString::from(format!("mobile-app-{}", store.id())), ButtonVariant::Outline, ButtonSize::Sm)
                        .icon(store.icon())
                        .label(crate::i18n::t(cx, store.label()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let just_dismissed_same = this
                                .about
                                .mobile_popover_dismissed
                                .take()
                                .is_some_and(|(prev, at)| prev == store && at.elapsed() < MOBILE_POPOVER_REOPEN_GUARD);
                            this.about.mobile_popover = if just_dismissed_same || this.about.mobile_popover == Some(store) {
                                None
                            } else {
                                Some(store)
                            };
                            cx.notify();
                        })),
                )
                .children(popover.map(|popover| div().absolute().bottom_0().right_0().child(popover)))
        }));
        app_rows.push(
            Row::new("mobile-app", "Mobile app")
                .description("Connect to your server from your phone or tablet.")
                .highlighted(self.is_target("mobile-app"))
                .control(store_buttons)
                .into_any_element(),
        );
        // The Welcome page sets up this computer, so it targets the local/primary environment
        // (`SettingsPanels.tsx:2292-2293` `usePrimaryEnvironmentId`).
        let welcome_env = {
            let app = self.state.read(cx);
            app.local_saved_environment()
                .or_else(|| {
                    let mut local = app.environments.environments.iter().filter(|env| env.local);
                    let first = local.next()?;
                    local.next().is_none().then_some(first)
                })
                .map(|env| env.id.clone())
        };
        if let Some(env_id) = welcome_env {
            app_rows.push(
                Row::new("welcome-page", "Welcome page")
                    .description("See your agents, import projects, and set up your phone.")
                    .highlighted(self.is_target("welcome-page"))
                    .control(
                        div().debug_selector(|| "about-welcome-open".to_owned()).child(
                            Button::new("about-welcome-open", ButtonVariant::Outline, ButtonSize::Sm)
                                .label("Open")
                                .on_click(move |_, window, cx| {
                                    super::close(window, cx);
                                    crate::onboarding::resume(env_id.clone(), cx);
                                }),
                        ),
                    )
                    .into_any_element(),
            );
        }
        let app = section("App", app_rows, cx).children(self.about.track_switch_dialog.clone());

        let privacy_description = match telemetry_off_message(policy.as_ref()) {
            Some(off) => format!("How we handle your data. {off}"),
            None => "How we handle your data, including the anonymous usage data T3 Code collects.".to_owned(),
        };
        let legal = section(
            "Legal",
            vec![
                Row::new("privacy-policy", "Privacy policy")
                    .description(privacy_description)
                    .highlighted(self.is_target("privacy-policy"))
                    .control(
                        Button::new("view-privacy-policy", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("View policy")
                            .on_click(|_, _, cx| cx.open_url(PRIVACY_POLICY_URL)),
                    )
                    .into_any_element(),
                Row::new("open-source-licenses", crate::i18n::t(cx, "Open source licenses"))
                    .description("Notices for dependencies, assets, and optional tools used by T3 Code.")
                    .highlighted(self.is_target("open-source-licenses"))
                    .control(
                        Button::new("view-open-source-licenses", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("View licenses")
                            .on_click(cx.listener(|this, _, _, cx| this.open_licenses(cx))),
                    )
                    .into_any_element(),
            ],
            cx,
        );

        vec![
            Block::new(&["about", "app-version", "update-track", "cli-command", "mobile-app", "welcome-page"], app),
            Block::new(&["legal", "privacy-policy", "open-source-licenses"], legal),
        ]
    }

    fn open_source_licenses_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let query = self.about.licenses_search.read(cx).value().to_string();

        let back = h_flex().px(r(16.)).child(
            Button::new("licenses-back", ButtonVariant::GhostMuted, ButtonSize::Sm)
                .icon(Icon::ChevronLeft)
                .label("Back to About")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.about.leave();
                    cx.notify();
                })),
        );

        let (header_action, rows): (Option<AnyElement>, Vec<AnyElement>) = match &self.about.licenses_status {
            LicensesStatus::Loaded(manifest) => {
                let total = manifest.entries.len();
                let filtered = filter_third_party_license_entries(&manifest.entries, &query);
                let filtered_count = filtered.len();
                let count_label = if filtered_count == total {
                    format!("{total} notices")
                } else {
                    format!("{filtered_count} of {total}")
                };
                let action = if !self.about.licenses_search_open {
                    h_flex()
                        .gap(r(6.))
                        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(count_label))
                        .child(
                            Button::new("licenses-open-search", ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                                .icon(Icon::Search)
                                .tooltip("Search licenses")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.about.licenses_search_open = true;
                                    this.about.licenses_search.update(cx, |input, cx| input.focus(window, cx));
                                    cx.notify();
                                })),
                        )
                        .into_any_element()
                } else {
                    h_flex()
                        .gap(r(8.))
                        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(count_label))
                        .child(div().w(r(176.)).child(Input::new(&self.about.licenses_search).small()))
                        .into_any_element()
                };
                let content = if filtered.is_empty() {
                    vec![
                        div()
                            .px(r(16.))
                            .py(r(32.))
                            .text_center()
                            .text_sm()
                            .text_color(hsla(p.muted_foreground))
                            .child("No licenses match that search.")
                            .into_any_element(),
                    ]
                } else {
                    filtered
                        .into_iter()
                        .enumerate()
                        .map(|(index, entry)| {
                            let key = third_party_license_entry_key(entry);
                            let is_open = self.about.licenses_expanded.as_deref() == Some(key.as_str());
                            let toggle_key = key.clone();
                            let meta = format!("{} \u{00b7} {}", entry.license, format_license_bundles(&entry.bundles));
                            let source_url = entry.source_url.clone();
                            let notice_text = entry.notice_text.clone();
                            v_flex()
                                .child(
                                    h_flex()
                                        .min_h(r(36.))
                                        .px(r(12.))
                                        .gap(r(8.))
                                        .hover(|this| this.bg(hsla(p.muted).opacity(0.35)))
                                        .child(
                                            h_flex()
                                                .id(("license-entry", index))
                                                .flex_1()
                                                .min_w_0()
                                                .gap(r(10.))
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if this.about.licenses_expanded.as_deref() == Some(toggle_key.as_str()) {
                                                        this.about.licenses_expanded = None;
                                                    } else {
                                                        this.about.licenses_expanded = Some(toggle_key.clone());
                                                    }
                                                    cx.notify();
                                                }))
                                                .child(icon(
                                                    if is_open { Icon::ChevronDown } else { Icon::ChevronRight },
                                                    14.,
                                                    hsla(p.muted_foreground),
                                                ))
                                                .child(
                                                    h_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .gap(r(8.))
                                                        .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(entry.name.clone()))
                                                        .children(entry.version.clone().map(|version| {
                                                            div().flex_none().font_family(mono.clone()).text_xs().text_color(hsla(p.muted_foreground)).child(version)
                                                        })),
                                                )
                                                .child(div().max_w(r(260.)).truncate().text_xs().text_color(hsla(p.muted_foreground)).child(meta)),
                                        )
                                        .children(source_url.map(|url| {
                                            Button::new(("license-source", index), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                                                .icon(Icon::ExternalLink)
                                                .tooltip("Project source")
                                                .on_click(move |_, _, cx| cx.open_url(&url))
                                        })),
                                )
                                .when(is_open, |this| {
                                    this.child(
                                        div()
                                            .pl(r(40.))
                                            .pr(r(16.))
                                            .pt(r(4.))
                                            .pb(r(16.))
                                            .font_family(mono.clone())
                                            .text_xs()
                                            .line_height(r(20.))
                                            .text_color(hsla(p.foreground).opacity(0.8))
                                            .child(notice_text),
                                    )
                                })
                                .into_any_element()
                        })
                        .collect()
                };
                (Some(action), content)
            }
            LicensesStatus::Failed(message) => (
                None,
                vec![
                    v_flex()
                        .gap(r(12.))
                        .px(r(16.))
                        .py(r(20.))
                        .items_start()
                        .child(
                            v_flex()
                                .gap(r(4.))
                                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Open-source notices are unavailable"))
                                .child(div().text_xs().text_color(hsla(p.muted_foreground).opacity(0.8)).child(message.clone())),
                        )
                        .child(
                            Button::new("licenses-retry", ButtonVariant::Outline, ButtonSize::Xs)
                                .label("Try again")
                                .on_click(cx.listener(|this, _, _, cx| this.load_licenses(cx))),
                        )
                        .into_any_element(),
                ],
            ),
            LicensesStatus::Idle | LicensesStatus::Loading => (
                None,
                vec![
                    div()
                        .px(r(16.))
                        .py(r(20.))
                        .text_sm()
                        .text_color(hsla(p.muted_foreground))
                        .child("Loading open-source notices\u{2026}")
                        .into_any_element(),
                ],
            ),
        };

        let notices = v_flex()
            .gap(r(10.))
            .child(
                h_flex()
                    .justify_between()
                    .gap(r(16.))
                    .child(section_title("Third-party notices"))
                    .children(header_action.map(|action| div().pr(r(16.)).child(action))),
            )
            .child(group(rows, cx));

        vec![Block::new(&[], back), Block::new(&["open-source-licenses"], notices)]
    }
}

#[cfg(test)]
mod unit_tests {
    use super::is_http_url;

    #[test]
    fn http_url_filter_accepts_http_and_rejects_unsafe_schemes() {
        assert!(is_http_url("https://help.acme.example/support"));
        assert!(is_http_url("http://localhost:3000/help"));
        assert!(!is_http_url("javascript:alert(1)"));
        assert!(!is_http_url("file:///etc/passwd"));
        assert!(!is_http_url("https://"));
        assert!(!is_http_url("https://help.acme.example/with space"));
    }
}

#[cfg(all(test, unix))]
#[test]
fn cli_link_preserves_a_command_from_another_installation() {
    let directory = std::env::temp_dir().join(format!("t3-native-cli-link-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let source = directory.join("bundled-t3");
    let destination = directory.join("bin/t3");
    std::fs::write(&source, "cli").unwrap();
    set_cli_link(&destination, &source, true).unwrap();
    assert!(owns_cli_link(&destination, &source));
    set_cli_link(&destination, &source, false).unwrap();
    std::fs::write(&destination, "independent installation").unwrap();
    assert!(set_cli_link(&destination, &source, true).is_err());
    assert!(set_cli_link(&destination, &source, false).is_err());
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "independent installation");
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use gpui::{Render, Window, div, prelude::*};
    use serde_json::json;
    use t3_core::settings::search::Section;

    use super::*;

    struct SettingsFrame;
    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().children(crate::settings::overlay(window, cx))
        }
    }

    #[gpui::test]
    fn about_renders_managed_org_and_escape_dismisses_qr_before_licenses(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.executor().allow_parking();
        let (state, fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": { "environmentId": "fixture-env", "label": "Mac" },
                        "managedPolicy": {
                            "organization": "Acme Corp",
                            "supportUrl": "https://support.acme.example",
                            "limits": { "telemetry": false }
                        }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|_, cx| crate::settings::open(Section::About, cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let view = vcx.read(|cx| cx.global::<crate::settings::Host>().view.clone().expect("settings view open"));
        assert!(vcx.debug_bounds("about-version-copy").is_some());
        assert!(vcx.debug_bounds("about-support-link").is_some());

        // Clicking an open Mobile app QR trigger closes it instead of immediately reopening on mouse-up.
        let ios_center = vcx.debug_bounds("mobile-app-ios").expect("iOS button bounds").center();
        let android_center = vcx.debug_bounds("mobile-app-android").expect("Android button bounds").center();
        vcx.simulate_click(ios_center, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        view.read_with(&vcx, |view, _| assert_eq!(view.about.mobile_popover, Some(MobileStore::Ios)));
        vcx.simulate_click(ios_center, Default::default());
        vcx.run_until_parked();
        view.read_with(&vcx, |view, _| assert_eq!(view.about.mobile_popover, None));

        // Clicking the other store button while one is open switches directly to the other store.
        vcx.simulate_click(ios_center, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        vcx.simulate_click(android_center, Default::default());
        vcx.run_until_parked();
        view.read_with(&vcx, |view, _| assert_eq!(view.about.mobile_popover, Some(MobileStore::Android)));

        // Escape dismisses mobile QR popover first, then licenses subview, without closing Settings.
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.about.mobile_popover = Some(MobileStore::Ios);
                view.about.licenses_open = true;
                assert!(view.dismiss_about_overlay(window, cx));
                assert!(view.about.mobile_popover.is_none());
                assert!(view.about.licenses_open);
                assert!(view.dismiss_about_overlay(window, cx));
                assert!(!view.about.licenses_open);
                assert!(!view.dismiss_about_overlay(window, cx));
            });
        });

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(vcx);
        drop(fixture);
        runtime.shutdown_background();
    }

    #[gpui::test]
    fn about_welcome_page_row_reopens_welcome_and_hides_without_environment(cx: &mut gpui::TestAppContext) {
        struct AboutWithWelcome {
            wizard: Entity<crate::onboarding::WelcomeWizard>,
        }
        impl Render for AboutWithWelcome {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .relative()
                    .size_full()
                    .child(self.wizard.clone())
                    .children(crate::settings::overlay(window, cx))
            }
        }

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.executor().allow_parking();
        let (state, fixture) =
            cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::onboarding::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            // Mark onboarding already completed so Welcome stays closed until triggered from About.
            crate::settings::store::update_client(
                &state,
                t3_core::client_settings::ClientSettingChange::OnboardingCompletedAt("2026-11-19T00:00:00Z".into()),
                cx,
            );
        });
        // Ensure a local primary environment exists alongside a secondary remote environment.
        state.update(cx, |state, _| {
            if let Some(env) = state.environments.environments.first_mut() {
                env.local = true;
            }
            let mut remote = state.environments.environments[0].clone();
            remote.id = "remote-box".into();
            remote.local = false;
            remote.label = "Remote Box".into();
            state.environments.environments.push(remote);
        });
        let window = cx.add_window(|window, cx| {
            let wizard = cx.new(|cx| crate::onboarding::WelcomeWizard::new(state.clone(), window, cx));
            AboutWithWelcome { wizard }
        });
        let wizard = window.read_with(cx, |root, _| root.wizard.clone()).unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);

        // With a local primary environment, the Welcome page row renders and "Open" closes Settings and opens Welcome.
        vcx.update(|_, cx| crate::settings::open_setting("welcome-page", cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(!wizard.read_with(&vcx, |w, _| w.is_open()));
        let open_center = vcx.debug_bounds("about-welcome-open").expect("Welcome page Open button").center();
        vcx.simulate_click(open_center, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(
            vcx.read(|cx| cx.global::<crate::settings::Host>().view.is_none()),
            "opening Welcome page from About closes Settings"
        );
        assert!(
            wizard.read_with(&vcx, |w, _| w.is_open()),
            "clicking Open on About -> Welcome page reopens WelcomeWizard"
        );

        // Remote-only setup (active environment is non-local and no local environment exists):
        // Welcome page row must hide (`SettingsPanels.tsx:2292-2293`).
        state.update(&mut vcx, |state, cx| {
            for env in &mut state.environments.environments {
                env.local = false;
            }
            state.connect.local_server = None;
            cx.emit(crate::app_state::AppEvent::Phase);
        });
        vcx.update(|_, cx| crate::settings::open(Section::About, cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(
            vcx.debug_bounds("about-welcome-open").is_none(),
            "Welcome page row must hide on remote-only setups without a local primary environment"
        );

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(vcx);
        drop(fixture);
        runtime.shutdown_background();
    }

    #[gpui::test]
    fn about_renders_check_updates_update_track_review_and_cli_command_rows(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.executor().allow_parking();
        let (state, fixture) =
            cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            crate::updates::set_saved_update_track(UpdateTrack::Latest, cx);
        });

        // Install a connected fleet environment running Nightly so `unswitched > 0` for Stable track.
        let session = serde_json::from_value(json!({
            "authenticated": true,
            "permissions": ["environment:maintain", "orchestration:read"]
        }))
        .unwrap();
        let (client, _env_fx) = t3_client::supervisor::ClientFixture::handle(session);
        let mut cfg = t3_core::server_config::ServerConfigState::default();
        cfg.apply(
            serde_json::from_value(json!({
                "type": "snapshot",
                "config": {
                    "environment": {
                        "environmentId": "nightly-box",
                        "label": "Nightly Box",
                        "serverVersion": "0.2.0-nightly.20261119.1",
                        "capabilities": { "serverSelfUpdate": "boot-service" }
                    },
                    "providers": []
                }
            }))
            .unwrap(),
        );
        let mut shell = t3_core::shell::ShellState::default();
        shell.synchronized = true;
        let install = state.update(cx, |s, _| {
            s.connections.install_fixture(
                "nightly-box",
                client,
                shell,
                cfg,
                t3_client::supervisor::ConnectionStatus::Connected,
            )
        });
        let _events = runtime.block_on(install);
        // Pump fleet snapshot into AppState.
        state.update(cx, |s, cx| {
            s.fleet = s.connections.view().borrow().clone();
            cx.emit(crate::app_state::AppEvent::ServerConfig);
        });

        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|_, cx| crate::settings::open(Section::About, cx));
        vcx.run_until_parked();
        let view = vcx.read(|cx| cx.global::<crate::settings::Host>().view.clone().expect("settings view"));

        // Seed CLI status: installed off-PATH with shell profile -> shows Add to PATH button.
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.about.cli_status = Some(CliCommandStatus {
                    installed_path: Some("/tmp/.t3/bin/t3".into()),
                    on_path: false,
                    shadowed_by: None,
                    path_dir: "/tmp/.t3/bin".into(),
                    profile_file: Some("~/.zshrc".into()),
                    path_line: Some("export PATH=\"$PATH:/tmp/.t3/bin\"".into()),
                });
                cx.notify();
            });
        });
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });

        assert!(vcx.debug_bounds("about-version-copy").is_some());
        assert!(vcx.debug_bounds("about-check-updates").is_some());
        assert!(vcx.debug_bounds("about-update-track-select").is_some());
        assert!(vcx.debug_bounds("about-track-review").is_some(), "unswitched machine must show Review button");
        assert!(vcx.debug_bounds("about-cli-add-path").is_some(), "installed off-PATH CLI must show Add to PATH");

        // Clicking Review opens UpdateTrackSwitchDialog; dismiss_about_overlay closes it without closing Settings.
        let review_center = vcx.debug_bounds("about-track-review").unwrap().center();
        vcx.simulate_click(review_center, Default::default());
        vcx.run_until_parked();
        view.read_with(&vcx, |view, _| {
            assert!(view.about.track_switch_dialog.is_some());
        });
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert!(view.dismiss_about_overlay(window, cx));
                assert!(view.about.track_switch_dialog.is_none());
            });
        });

        // Uninstalled CLI status renders "Install 't3' command".
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.about.cli_status = Some(CliCommandStatus {
                    installed_path: None,
                    on_path: false,
                    shadowed_by: None,
                    path_dir: "/tmp/.t3/bin".into(),
                    profile_file: Some("~/.zshrc".into()),
                    path_line: None,
                });
                cx.notify();
            });
        });
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("about-cli-install").is_some());
        assert!(vcx.debug_bounds("about-cli-add-path").is_none());

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(vcx);
        drop(fixture);
        runtime.shutdown_background();
    }
}
