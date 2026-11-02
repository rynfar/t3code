//! The Device panel's data: simulators and emulators a server reaches through its device hub.
//! Wire types port `packages/contracts/src/device.ts` (`DeviceServiceState` with its `easHosts`
//! and `simlockHosts` merged into `hosts`, `DeviceSummary`, `DeviceHostSummary`, `DeviceSession`,
//! and the open, close, configure and list inputs). The selectors port
//! `apps/web/src/components/device/DeviceSetup.tsx` (`deviceHubReadiness`,
//! `isAndroidDiscoveryUncertain`, `platformSetupStatus`, `deviceHostDiagnostics`,
//! `DeviceHubSetupStatus`, `AgentDeviceSetupStatus`), `DevicePanel.tsx` (`groupDevices`, the
//! discovery warnings, the loading and empty messages, the device rows), `DeviceHostUpdates.tsx`,
//! and the hub URLs of `client-runtime/src/state/deviceHubAccess.ts` and `device/hubAccess.ts`.
//! The stream protocol is in `stream`.

pub mod stream;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

pub mod method {
    pub const SUBSCRIBE_STATE: &str = "subscribeDeviceState";
    pub const LIST: &str = "device.list";
    pub const CONFIGURE: &str = "device.configure";
    pub const OPEN: &str = "device.open";
    pub const CLOSE: &str = "device.close";
}

/// `AuthSettingsWriteScope`: Device setup's switches and Done need it.
pub const SETTINGS_WRITE_SCOPE: &str = "settings:write";

/// `EMPTY_DEVICE_STATE.hubBasePath`, which the web's hub access always uses.
pub const HUB_BASE_PATH: &str = "/api/device-hub";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Ios,
    Android,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Ios => "ios",
            Platform::Android => "android",
        }
    }

    /// `platformName` in `DeviceSetup.tsx`.
    pub fn name(self) -> &'static str {
        match self {
            Platform::Ios => "iOS",
            Platform::Android => "Android",
        }
    }

    /// `platformLabel` in `DevicePanel.tsx`.
    pub fn group_label(self) -> &'static str {
        match self {
            Platform::Ios => "iOS Simulators",
            Platform::Android => "Android Emulators",
        }
    }
}

/// `DeviceHostStatus`. A status a newer server adds decodes as `Unknown` and counts as none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostStatus {
    Disabled,
    Idle,
    Installing,
    Starting,
    Ready,
    Failed,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostKind {
    Local,
    Ssh,
    Eas,
    Simlock,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PlatformAvailability {
    pub platform: Platform,
    pub available: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `DeviceHostSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub id: String,
    pub label: String,
    pub kind: HostKind,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub platforms: Vec<PlatformAvailability>,
    #[serde(default)]
    pub agent_device_installed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatusEntry {
    pub status: HostStatus,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub android_discovery_incomplete: bool,
}

/// `DeviceSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub host_id: String,
    pub id: String,
    pub platform: Platform,
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub booted: bool,
}

impl Device {
    /// `deviceKey`.
    pub fn key(&self) -> (&str, &str) {
        (&self.host_id, &self.id)
    }
}

/// `DeviceSession`: a device open for a thread.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub thread_id: String,
    pub host_id: String,
    pub device_id: String,
    pub platform: Platform,
}

/// A `bootingDevices` entry: a device starting for a thread.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootingDevice {
    pub thread_id: String,
    pub name: String,
}

/// `DeviceServiceState`, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceState {
    pub hosts: Vec<Host>,
    pub host_status: HostStatus,
    pub host_status_detail: Option<String>,
    pub host_statuses: BTreeMap<String, HostStatusEntry>,
    pub devices: Vec<Device>,
    pub sessions: Vec<Session>,
    pub booting_devices: Vec<BootingDevice>,
    pub onboarding_completed: bool,
    pub agent_access_enabled: bool,
    pub supports_host_retry: bool,
    pub revision: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
    host_status: HostStatus,
    #[serde(default)]
    host_status_detail: Option<String>,
    #[serde(deserialize_with = "lenient_map")]
    host_statuses: BTreeMap<String, HostStatusEntry>,
    #[serde(deserialize_with = "lenient_vec")]
    hosts: Vec<Host>,
    #[serde(default, deserialize_with = "lenient_vec")]
    eas_hosts: Vec<Host>,
    #[serde(default, deserialize_with = "lenient_vec")]
    simlock_hosts: Vec<Host>,
    #[serde(deserialize_with = "lenient_vec")]
    devices: Vec<Device>,
    #[serde(deserialize_with = "lenient_vec")]
    sessions: Vec<Session>,
    #[serde(default, deserialize_with = "lenient_vec")]
    booting_devices: Vec<BootingDevice>,
    onboarding_completed: bool,
    agent_access_enabled: bool,
    #[serde(default)]
    supports_host_retry: bool,
    revision: i64,
}

/// One bad entry is dropped; the rest of the list survives.
fn lenient_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(deserializer: D) -> Result<Vec<T>, D::Error> {
    let values = Option::<Vec<Value>>::deserialize(deserializer)?.unwrap_or_default();
    Ok(values.into_iter().filter_map(|value| serde_json::from_value(value).ok()).collect())
}

fn lenient_map<'de, D: Deserializer<'de>, T: DeserializeOwned>(deserializer: D) -> Result<BTreeMap<String, T>, D::Error> {
    let values = Option::<BTreeMap<String, Value>>::deserialize(deserializer)?.unwrap_or_default();
    Ok(values.into_iter().filter_map(|(key, value)| serde_json::from_value(value).ok().map(|value| (key, value))).collect())
}

impl ServiceState {
    /// `EMPTY_DEVICE_STATE`: what a client reads before the first state, the hub off.
    pub fn empty() -> ServiceState {
        ServiceState {
            hosts: Vec::new(),
            host_status: HostStatus::Disabled,
            host_status_detail: None,
            host_statuses: BTreeMap::new(),
            devices: Vec::new(),
            sessions: Vec::new(),
            booting_devices: Vec::new(),
            onboarding_completed: false,
            agent_access_enabled: false,
            supports_host_retry: false,
            revision: 0,
        }
    }

    pub fn decode(value: Value) -> Result<ServiceState, String> {
        let wire: Wire = serde_json::from_value(value).map_err(|error| format!("Invalid device state: {error}"))?;
        let mut hosts = wire.hosts;
        hosts.extend(wire.eas_hosts);
        hosts.extend(wire.simlock_hosts);
        Ok(ServiceState {
            hosts,
            host_status: wire.host_status,
            host_status_detail: wire.host_status_detail,
            host_statuses: wire.host_statuses,
            devices: wire.devices,
            sessions: wire.sessions,
            booting_devices: wire.booting_devices,
            onboarding_completed: wire.onboarding_completed,
            agent_access_enabled: wire.agent_access_enabled,
            supports_host_retry: wire.supports_host_retry,
            revision: wire.revision,
        })
    }

    pub fn host(&self, host_id: &str) -> Option<&Host> {
        self.hosts.iter().find(|host| host.id == host_id)
    }

    pub fn device(&self, host_id: &str, device_id: &str) -> Option<&Device> {
        self.devices.iter().find(|device| device.host_id == host_id && device.id == device_id)
    }

    /// The panel shows Device setup instead: `!onboardingCompleted || hostStatus === "disabled"`.
    pub fn needs_setup(&self) -> bool {
        !self.onboarding_completed || self.host_status == HostStatus::Disabled
    }

    pub fn hub_enabled(&self) -> bool {
        self.host_status != HostStatus::Disabled
    }

    /// `deviceHubReadiness`: `hostStatus` tracks only this machine, so remote hosts count too.
    pub fn hub_readiness(&self) -> Readiness {
        let statuses = || self.host_statuses.values().map(|entry| entry.status);
        let ready = self.host_status == HostStatus::Ready || statuses().any(|status| status == HostStatus::Ready);
        let busy = !ready && std::iter::once(self.host_status).chain(statuses()).any(|status| matches!(status, HostStatus::Installing | HostStatus::Starting));
        Readiness { ready, busy }
    }

    /// `isAndroidDiscoveryUncertain`.
    pub fn android_discovery_uncertain(&self) -> bool {
        self.host_statuses.values().any(|entry| entry.status == HostStatus::Ready && entry.android_discovery_incomplete)
    }

    /// `platformSetupStatus`: whether a platform can be used, in the setup wizard's words.
    pub fn platform_setup_status(&self, platform: Platform) -> PlatformStatus {
        let has_devices = self.devices.iter().any(|device| device.platform == platform);
        if platform == Platform::Android && !has_devices && self.android_discovery_uncertain() {
            return PlatformStatus::not_ready("The Android device list may be incomplete. Check the device host and try again.");
        }
        let availabilities: Vec<(&Host, &PlatformAvailability)> =
            self.hosts.iter().flat_map(|host| host.platforms.iter().filter(|entry| entry.platform == platform).map(move |entry| (host, entry))).collect();
        let supporting: Vec<&Host> = availabilities.iter().filter(|(_, entry)| entry.available).map(|(host, _)| *host).collect();
        if supporting.is_empty() {
            let reason = availabilities.first().and_then(|(_, entry)| entry.reason.clone());
            return PlatformStatus::not_ready(reason.unwrap_or_else(|| format!("{} support was not detected.", platform.name())));
        }
        let ready_host = supporting.iter().any(|host| {
            self.host_statuses.get(&host.id).is_some_and(|entry| entry.status == HostStatus::Ready) || (host.kind == HostKind::Local && self.host_status == HostStatus::Ready)
        });
        if ready_host && !has_devices {
            return PlatformStatus::not_ready(match platform {
                Platform::Ios => "Xcode is installed, but no iOS Simulator is available. Install a runtime in Xcode Settings → Components.",
                Platform::Android => "The Android SDK is installed, but no virtual device exists. Create one in Android Studio → Device Manager.",
            });
        }
        let remote = supporting.iter().all(|host| host.kind != HostKind::Local).then(|| supporting[0]);
        let location = remote.map(|host| format!(" on {}", host.label)).unwrap_or_default();
        let message = match (remote, platform) {
            (Some(host), _) if host.kind == HostKind::Eas => {
                let kind = if platform == Platform::Ios { "iOS Simulators" } else { "Android emulators" };
                format!("{} starts {kind} in the cloud with EAS Simulator.", host.label)
            }
            (_, Platform::Ios) => format!("Xcode and iOS Simulator are available{location}."),
            (_, Platform::Android) => format!("The Android SDK and Emulator are available{location}."),
        };
        PlatformStatus { ready: true, message }
    }

    /// `deviceHostDiagnostics` (setup): ready hosts that still report a discovery warning.
    pub fn setup_diagnostics(&self) -> Vec<Diagnostic> {
        self.hosts
            .iter()
            .filter_map(|host| {
                let entry = self.host_statuses.get(&host.id)?;
                let detail = entry.detail.clone().filter(|_| entry.status == HostStatus::Ready)?;
                Some(Diagnostic { host_id: host.id.clone(), label: host.label.clone(), detail })
            })
            .collect()
    }

    /// The panel's discovery warnings: a ready host's detail, or this machine's.
    pub fn panel_diagnostics(&self) -> Vec<Diagnostic> {
        self.hosts
            .iter()
            .filter_map(|host| {
                let entry = self.host_statuses.get(&host.id);
                let detail = entry.and_then(|entry| entry.detail.clone()).or_else(|| (host.kind == HostKind::Local).then(|| self.host_status_detail.clone()).flatten())?;
                let status = entry.map_or(self.host_status, |entry| entry.status);
                (status == HostStatus::Ready).then(|| Diagnostic { host_id: host.id.clone(), label: host.label.clone(), detail })
            })
            .collect()
    }

    /// `DeviceHostUpdates`: hosts installing, starting or failed, while the hub is on.
    pub fn host_updates(&self) -> Vec<HostUpdate> {
        if !self.hub_enabled() {
            return Vec::new();
        }
        self.hosts
            .iter()
            .filter_map(|host| {
                let entry = self.host_statuses.get(&host.id)?;
                let failed = entry.status == HostStatus::Failed;
                let fallback = match entry.status {
                    HostStatus::Failed => "Device support could not start.",
                    HostStatus::Installing => "Installing device tools…",
                    HostStatus::Starting => "Starting device tools…",
                    _ => return None,
                };
                Some(HostUpdate {
                    host_id: host.id.clone(),
                    label: host.label.clone(),
                    detail: entry.detail.clone().unwrap_or_else(|| fallback.to_owned()),
                    failed,
                    retry: failed && self.supports_host_retry,
                })
            })
            .collect()
    }

    /// `groupDevices`: iOS then Android, running devices first, then by name.
    pub fn grouped(&self) -> Vec<(Platform, Vec<&Device>)> {
        [Platform::Ios, Platform::Android]
            .into_iter()
            .filter_map(|platform| {
                let mut devices: Vec<&Device> = self.devices.iter().filter(|device| device.platform == platform).collect();
                devices.sort_by(|a, b| b.booted.cmp(&a.booted).then_with(|| compare_names(&a.name, &b.name)));
                (!devices.is_empty()).then_some((platform, devices))
            })
            .collect()
    }

    /// The session behind a device tab: the thread's session on the tab's device.
    pub fn thread_session(&self, thread_id: &str, host_id: &str, device_id: &str) -> Option<&Session> {
        self.sessions.iter().find(|session| session.thread_id == thread_id && session.host_id == host_id && session.device_id == device_id)
    }

    /// "Starting …" names the devices booting for this thread.
    pub fn booting_names(&self, thread_id: &str) -> Vec<&str> {
        self.booting_devices.iter().filter(|device| device.thread_id == thread_id).map(|device| device.name.as_str()).collect()
    }

    /// The panel's Android hint: the hub is ready, discovery is certain, and no host found or
    /// ruled out an Android device.
    pub fn show_android_hint(&self) -> bool {
        self.hub_readiness().ready
            && !self.android_discovery_uncertain()
            && !self.devices.iter().any(|device| device.platform == Platform::Android)
            && !self.hosts.iter().any(|host| host.platforms.iter().any(|entry| !entry.available && entry.platform == Platform::Android))
    }

    /// Setup checks simulator support on its first step when this machine supports neither.
    pub fn local_platforms_unavailable(&self) -> bool {
        self.hosts.iter().any(|host| host.kind == HostKind::Local && !host.platforms.iter().any(|entry| entry.available))
    }

    /// The panel's empty state with no device to list.
    pub fn empty_message(&self) -> String {
        if self.host_status == HostStatus::Failed {
            self.host_status_detail.clone().unwrap_or_else(|| "The device hub failed to start.".to_owned())
        } else {
            "No simulators or emulators were found on this environment.".to_owned()
        }
    }

    /// `DeviceHubSetupStatus` (not compact): `None` while it shows nothing.
    pub fn hub_setup_status(&self, pending: bool) -> Option<SetupStatus> {
        if pending {
            let text = match self.host_status {
                HostStatus::Installing => "Installing device hub…",
                HostStatus::Starting => "Starting device hub…",
                _ => "Updating device hub…",
            };
            return Some(SetupStatus { pending: true, text });
        }
        self.hub_readiness().ready.then_some(SetupStatus { pending: false, text: "Device hub is ready." })
    }

    /// `AgentDeviceSetupStatus` (not compact).
    pub fn agent_setup_status(&self, pending: bool) -> Option<SetupStatus> {
        if pending {
            let text = match self.host_status {
                HostStatus::Installing => "Installing agent tools…",
                HostStatus::Starting => "Starting agent tools…",
                _ => "Updating agent access…",
            };
            return Some(SetupStatus { pending: true, text });
        }
        (self.agent_access_enabled && self.hub_readiness().ready && self.hosts.iter().any(|host| host.agent_device_installed))
            .then_some(SetupStatus { pending: false, text: "Agent tools are ready." })
    }

    /// A device row's second line: host, version, and whether it runs.
    pub fn device_detail(&self, device: &Device) -> String {
        let host = self.host(&device.host_id).map_or("", |host| host.label.as_str());
        format!("{host} · {} · {}", device.version, if device.booted { "Running" } else { "Stopped" })
    }

    /// The loading view's description for a device: its host and version.
    pub fn device_description(&self, device: &Device) -> String {
        let host = self.host(&device.host_id).map_or("Device host", |host| host.label.as_str());
        format!("{host} · {}", device.version)
    }

    /// The loading view's message while no device shows yet.
    pub fn loading_message(&self, pending: Option<&Device>) -> String {
        match pending {
            Some(device) if device.booted => "Opening device…".to_owned(),
            Some(_) => "Starting device…".to_owned(),
            None if self.host_status == HostStatus::Installing => self.host_status_detail.clone().unwrap_or_else(|| "Installing device support…".to_owned()),
            None => "Finding devices…".to_owned(),
        }
    }
}

/// `a.name.localeCompare(b.name)` for device names: case-insensitive first, then exact.
fn compare_names(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub ready: bool,
    pub busy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformStatus {
    pub ready: bool,
    pub message: String,
}

impl PlatformStatus {
    fn not_ready(message: impl Into<String>) -> PlatformStatus {
        PlatformStatus { ready: false, message: message.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub host_id: String,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostUpdate {
    pub host_id: String,
    pub label: String,
    pub detail: String,
    pub failed: bool,
    /// The server can retry the host (`supportsHostRetry`).
    pub retry: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupStatus {
    /// A spinner instead of the check.
    pub pending: bool,
    pub text: &'static str,
}

pub const HUB_DESCRIPTION: &str = "Enable this environment to open simulators and emulators, whether they run here or on a remote device host.";
pub const AGENT_DESCRIPTION: &str =
    "Allow new agent sessions in this environment to start and control local and remote devices, with required tools set up automatically.";
pub const SETUP_STEPS: [&str; 3] = ["Device hub", "Simulators", "Agent access"];
pub const ANDROID_HINT: &str = "No Android virtual devices found. Create one in Android Studio's Device Manager, then refresh.";
pub const HOST_FAILED_HINT: &str = "Check the host connection and network access, then retry. Your device settings are saved.";

/// `device.open` for a thread: boots the device unless it runs.
pub fn open_payload(thread_id: &str, device: &Device) -> Value {
    json!({ "threadId": thread_id, "hostId": device.host_id, "deviceId": device.id, "platform": device.platform })
}

/// `device.close` for one device of a thread, powering it off when `shutdown`.
pub fn close_payload(thread_id: &str, host_id: &str, device_id: &str, shutdown: bool) -> Value {
    json!({ "threadId": thread_id, "hostId": host_id, "deviceId": device_id, "shutdown": shutdown })
}

/// `device.configure` from the hub switch: turning the hub off turns agent access off with it.
pub fn configure_hub_payload(enabled: bool) -> Value {
    if enabled { json!({ "enabled": true }) } else { json!({ "enabled": false, "agentAccessEnabled": false }) }
}

/// `device.list` retrying one failed host.
pub fn retry_host_payload(host_id: &str) -> Value {
    json!({ "retryHostId": host_id })
}

/// What `device.open` answered: the session it opened, which may name a different host.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Opened {
    pub host_id: String,
    pub device_id: String,
}

impl Opened {
    pub fn decode(value: Value) -> Result<Opened, String> {
        serde_json::from_value(value).map_err(|error| format!("Invalid device session: {error}"))
    }
}

/// Where the server's device hub answers, with the media ticket and host every hub request
/// carries (`resolveDeviceHubAccess` with `withDeviceHubQuery`, plus the panel's `hostId`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubAccess {
    http_base: String,
    ticket: String,
    host_id: String,
}

impl HubAccess {
    /// `environmentEndpointUrl(httpBaseUrl, hubBasePath)`: the hub path replaces the base's.
    pub fn new(server_http_base: &str, ticket: &str, host_id: &str) -> Option<HubAccess> {
        let mut url = url::Url::parse(server_http_base).ok()?;
        url.set_path(HUB_BASE_PATH);
        url.set_query(None);
        url.set_fragment(None);
        Some(HubAccess { http_base: url.to_string(), ticket: ticket.to_owned(), host_id: host_id.to_owned() })
    }

    /// serve-sim (iOS and Simlock hosts) or serve-emu (Android).
    fn vendor(serve_sim: bool) -> &'static str {
        if serve_sim { "/vendor/serve-sim" } else { "/vendor/serve-emu" }
    }

    fn with_query(&self, base: String, path_and_query: &str) -> String {
        let separator = if path_and_query.contains('?') { '&' } else { '?' };
        let query = url::form_urlencoded::Serializer::new(String::new()).append_pair("wsTicket", &self.ticket).append_pair("hostId", &self.host_id).finish();
        format!("{base}{path_and_query}{separator}{query}")
    }

    /// A hub HTTP URL; `path_and_query` is already encoded.
    pub fn http_url(&self, serve_sim: bool, path_and_query: &str) -> String {
        self.with_query(format!("{}{}", self.http_base, Self::vendor(serve_sim)), path_and_query)
    }

    /// The same hub path over WebSocket.
    pub fn ws_url(&self, serve_sim: bool, path_and_query: &str) -> String {
        let base = match self.http_base.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some((_, rest)) => format!("ws://{rest}"),
            None => self.http_base.clone(),
        };
        self.with_query(format!("{base}{}", Self::vendor(serve_sim)), path_and_query)
    }
}

/// `encodeURIComponent`, for device ids in tab ids and hub paths.
pub fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
