//! The Device panel: simulators and emulators the server reaches through its device hub. This
//! controller follows the environment's device state as `state/device.ts` does (`useDeviceState`,
//! `deviceHostProbeAtom`), runs `device.open` and `device.close` for a thread as
//! `components/device/DevicePanel.tsx` does (its per-thread `operation`), and gates the launcher's
//! "Device" entry as `useRightPanelActions.ts` `addDeviceSurface` does. The tab is `DevicePane`,
//! a device's live screen is `DeviceScreen`, and the setup wizard is `DeviceSetupDialog`.

mod pane;
mod screen;
mod setup;

use std::collections::HashMap;
use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};
use serde_json::{Value, json};
use t3_client::session::RpcError;
use t3_core::device::{Device, Opened, SETTINGS_WRITE_SCOPE, ServiceState, close_payload, method, open_payload};
use t3_core::right_panel::{DeviceTarget, RightPanels, Surface};

pub use pane::DevicePane;
pub use setup::DeviceSetupDialog;

use crate::app_state::{AppEvent, AppState};

const AUTHORIZATION_ERROR: &str = "EnvironmentAuthorizationError";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevicesEvent {
    Changed,
}

/// Aborts a network task with its owner.
struct Abort(tokio::task::JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A thread's device open or power-off in flight, or how the last one failed.
#[derive(Debug, Clone, Default)]
pub struct Operation {
    pub pending: Option<Device>,
    pub error: Option<String>,
    id: u64,
}

pub struct Devices {
    state: Entity<AppState>,
    connection: Option<u64>,
    /// `None` until the first state arrives.
    service: Option<ServiceState>,
    /// The subscription failed before a first state, so it never settles on its own.
    load_error: Option<String>,
    /// `None` until the session state answers; unknown counts as granted.
    can_configure: Option<bool>,
    operations: HashMap<String, Operation>,
    next_operation: u64,
    /// The thread the launcher asked to set devices up for.
    setup_requested: Option<String>,
    feed: Option<(Abort, Task<()>)>,
    grants: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<DevicesEvent> for Devices {}

struct DevicesGlobal(Entity<Devices>);

impl Global for DevicesGlobal {}

pub fn init(state: Entity<AppState>, cx: &mut App) {
    let devices = cx.new(|cx| Devices::new(state, cx));
    cx.set_global(DevicesGlobal(devices));
}

pub fn devices(cx: &App) -> Option<Entity<Devices>> {
    cx.try_global::<DevicesGlobal>().map(|global| global.0.clone())
}

/// The launcher's "Device" entry (`addDeviceSurface`): setup first while devices are off or not
/// set up, else the device list in a tab.
pub fn add_device_surface(state: &mut AppState, cx: &mut Context<AppState>) {
    let Some(thread_id) = state.selected_thread.clone() else { return };
    let needs_setup = devices(cx).and_then(|devices| devices.read(cx).service.as_ref().map(ServiceState::needs_setup)) == Some(true);
    if needs_setup {
        cx.defer(move |cx| {
            if let Some(devices) = devices(cx) {
                devices.update(cx, |devices, cx| devices.request_setup(thread_id, cx));
            }
        });
        return;
    }
    state.mutate_panels(cx, |panels, thread| panels.panels.open(thread, Surface::device(None)));
}

/// Applies a panel change to `thread_id`, which may no longer be the one showing.
fn edit_panels(state: &mut AppState, thread_id: &str, cx: &mut Context<AppState>, change: impl FnOnce(&mut RightPanels, &str) -> bool) {
    if state.selected_thread.as_deref() == Some(thread_id) {
        state.mutate_panels(cx, |panels, thread| change(&mut panels.panels, thread));
    } else if change(&mut state.panels.panels, thread_id) {
        state.save_panels(cx);
    }
}

/// `formatEnvironmentQueryError`.
fn error_text(error: &RpcError) -> String {
    match error {
        RpcError::Failure(failure) => failure.message.clone(),
        RpcError::Closed => "Disconnected.".to_owned(),
    }
}

impl Devices {
    fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if let AppEvent::Phase = event {
                this.sync_connection(cx);
            }
        });
        let mut devices = Devices {
            state,
            connection: None,
            service: None,
            load_error: None,
            can_configure: None,
            operations: HashMap::new(),
            next_operation: 0,
            setup_requested: None,
            feed: None,
            grants: None,
            _subscription: subscription,
        };
        devices.sync_connection(cx);
        devices
    }

    pub fn service(&self) -> Option<&ServiceState> {
        self.service.as_ref()
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// `useEnvironmentScope(environmentId, AuthSettingsWriteScope)`.
    pub fn can_configure(&self) -> bool {
        self.can_configure != Some(false)
    }

    pub fn operation(&self, thread_id: &str) -> Option<&Operation> {
        self.operations.get(thread_id)
    }

    pub fn setup_requested(&self) -> Option<&str> {
        self.setup_requested.as_deref()
    }

    /// A new connection starts over: its devices and grants are its own.
    fn sync_connection(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let current = state.connection_id();
        if current == self.connection {
            return;
        }
        self.connection = current;
        self.service = None;
        self.load_error = None;
        self.can_configure = None;
        self.operations.clear();
        self.feed = None;
        self.grants = None;
        if let Some(client) = state.client_handle() {
            let mut grants = client.session_state();
            self.can_configure = grants.borrow_and_update().as_ref().map(|state| state.grants(SETTINGS_WRITE_SCOPE));
            self.grants = Some(cx.spawn(async move |this, cx| {
                while grants.changed().await.is_ok() {
                    let granted = grants.borrow_and_update().as_ref().map(|state| state.grants(SETTINGS_WRITE_SCOPE));
                    let updated = this.update(cx, |this, cx| {
                        if this.can_configure != granted {
                            this.can_configure = granted;
                            cx.emit(DevicesEvent::Changed);
                        }
                    });
                    if updated.is_err() {
                        return;
                    }
                }
            }));
            self.subscribe(cx);
        }
        cx.emit(DevicesEvent::Changed);
    }

    /// `subscribeDeviceState`, resubscribing after a drop.
    fn subscribe(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<ServiceState, String>>();
        let driver = state.runtime().spawn(async move {
            let mut attempt = 0u32;
            loop {
                let failure = match client.stream(method::SUBSCRIBE_STATE, json!({})).await {
                    Ok(mut stream) => {
                        let mut failure = None;
                        while let Some(item) = stream.receiver.recv().await {
                            match item.map_err(|error| (error_text(&error), matches!(&error, RpcError::Failure(failure) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR)))) {
                                Ok(value) => {
                                    attempt = 0;
                                    match ServiceState::decode(value) {
                                        Ok(state) => {
                                            if tx.send(Ok(state)).is_err() {
                                                return;
                                            }
                                        }
                                        Err(error) => tracing::warn!(%error, "undecodable device state"),
                                    }
                                }
                                Err((message, authorization)) => {
                                    if authorization {
                                        let _ = tx.send(Err(message));
                                        return;
                                    }
                                    failure = Some(message);
                                    break;
                                }
                            }
                        }
                        failure
                    }
                    Err(error) => {
                        let authorization = matches!(&error, RpcError::Failure(failure) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR));
                        if authorization {
                            let _ = tx.send(Err(error_text(&error)));
                            return;
                        }
                        Some(error_text(&error))
                    }
                };
                if let Some(message) = failure
                    && tx.send(Err(message)).is_err()
                {
                    return;
                }
                if tx.is_closed() {
                    return;
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_millis((250u64 << attempt.min(5)).min(5_000))).await;
            }
        });
        let pump = cx.spawn(async move |this, cx| {
            while let Some(first) = rx.recv().await {
                let mut latest = first;
                while let Ok(next) = rx.try_recv() {
                    latest = next;
                }
                if this.update(cx, |this, cx| this.apply(latest, cx)).is_err() {
                    return;
                }
            }
        });
        self.feed = Some((Abort(driver), pump));
    }

    fn apply(&mut self, next: Result<ServiceState, String>, cx: &mut Context<Self>) {
        match next {
            Ok(service) => {
                self.load_error = None;
                self.set_service(service, cx);
            }
            // A state already shown stays; only a first load reports its failure.
            Err(error) if self.service.is_none() => {
                self.load_error = Some(error);
                cx.emit(DevicesEvent::Changed);
            }
            Err(_) => {}
        }
    }

    fn set_service(&mut self, service: ServiceState, cx: &mut Context<Self>) {
        if self.service.as_ref().is_some_and(|current| current.revision > service.revision) || self.service.as_ref() == Some(&service) {
            return;
        }
        self.service = Some(service);
        cx.emit(DevicesEvent::Changed);
    }

    /// The loading view's Retry: subscribe again.
    pub fn retry_load(&mut self, cx: &mut Context<Self>) {
        self.load_error = None;
        self.feed = None;
        self.subscribe(cx);
        cx.emit(DevicesEvent::Changed);
    }

    /// `device.list`: probes the hosts ("Refresh devices", the probe while the panel shows, and
    /// setup's "Check again"); `retryHostId` retries one failed host. Never installs or starts
    /// helpers. Its answer is the new state.
    pub fn list(&mut self, input: Value, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        let Some(task) = self.state.read(cx).request(method::LIST, input, ServiceState::decode) else { return Task::ready(Err("Disconnected.".to_owned())) };
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Disconnected.".to_owned()));
            this.update(cx, |this, cx| result.map(|service| this.set_service(service, cx))).unwrap_or(Ok(()))
        })
    }

    /// `device.configure` from setup. Its answer is the new state.
    pub fn configure(&mut self, input: Value, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        let Some(task) = self.state.read(cx).request(method::CONFIGURE, input, ServiceState::decode) else { return Task::ready(Err("Disconnected.".to_owned())) };
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Disconnected.".to_owned()));
            this.update(cx, |this, cx| result.map(|service| this.set_service(service, cx))).unwrap_or(Ok(()))
        })
    }

    fn start_operation(&mut self, thread_id: &str, pending: Option<Device>, cx: &mut Context<Self>) -> u64 {
        self.next_operation += 1;
        let id = self.next_operation;
        self.operations.insert(thread_id.to_owned(), Operation { pending, error: None, id });
        cx.emit(DevicesEvent::Changed);
        id
    }

    /// Settles an operation unless a newer one on the same thread replaced it.
    fn settle(&mut self, thread_id: &str, id: u64, error: Option<String>, cx: &mut Context<Self>) {
        if let Some(operation) = self.operations.get_mut(thread_id).filter(|operation| operation.id == id) {
            operation.pending = None;
            operation.error = error;
            cx.emit(DevicesEvent::Changed);
        }
    }

    pub fn dismiss_error(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.operations.remove(thread_id).is_some() {
            cx.emit(DevicesEvent::Changed);
        }
    }

    /// `selectDevice`: opens (booting if needed) a device for the thread, then its tab there.
    pub fn open_device(&mut self, thread_id: &str, device: Device, cx: &mut Context<Self>) {
        let id = self.start_operation(thread_id, Some(device.clone()), cx);
        let thread_id = thread_id.to_owned();
        let Some(task) = self.state.read(cx).request(method::OPEN, open_payload(&thread_id, &device), Opened::decode) else {
            return self.settle(&thread_id, id, Some("Disconnected.".to_owned()), cx);
        };
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Disconnected.".to_owned()));
            let _ = this.update(cx, |this, cx| match result {
                Ok(opened) => {
                    let target = DeviceTarget { host_id: opened.host_id, device_id: opened.device_id, platform: device.platform, name: device.name.clone() };
                    this.state.update(cx, |state, cx| edit_panels(state, &thread_id, cx, |panels, thread| panels.open_device(thread, target)));
                    this.settle(&thread_id, id, None, cx);
                }
                Err(error) => this.settle(&thread_id, id, Some(error), cx),
            });
        })
        .detach();
    }

    /// `closeActive`: closes the tab, powering the device off first when asked.
    pub fn close_device(&mut self, thread_id: &str, surface_id: &str, session: Option<(String, String)>, power_off: bool, cx: &mut Context<Self>) {
        let (thread_id, surface_id) = (thread_id.to_owned(), surface_id.to_owned());
        if !power_off {
            self.state.update(cx, |state, cx| edit_panels(state, &thread_id, cx, |panels, thread| panels.close_surface(thread, &surface_id)));
            return;
        }
        let Some((host_id, device_id)) = session else { return };
        let id = self.start_operation(&thread_id, None, cx);
        let task = self.state.read(cx).rpc(method::CLOSE, close_payload(&thread_id, &host_id, &device_id, true));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => {
                    this.state.update(cx, |state, cx| edit_panels(state, &thread_id, cx, |panels, thread| panels.close_surface(thread, &surface_id)));
                    this.settle(&thread_id, id, None, cx);
                }
                Err(error) => this.settle(&thread_id, id, Some(error_text(&error)), cx),
            });
        })
        .detach();
    }

    /// The launcher asked for setup on a thread whose devices are off or not set up.
    fn request_setup(&mut self, thread_id: String, cx: &mut Context<Self>) {
        self.setup_requested = Some(thread_id);
        cx.emit(DevicesEvent::Changed);
    }

    fn clear_setup_request(&mut self, cx: &mut Context<Self>) {
        if self.setup_requested.take().is_some() {
            cx.emit(DevicesEvent::Changed);
        }
    }
}
