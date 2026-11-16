use std::future::Future;
use std::pin::Pin;
#[cfg(any(test, feature = "snapshot"))]
use std::sync::Mutex;
#[cfg(any(test, feature = "snapshot"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui::{App, Global};
use t3_core::voice_input::{AudioDeviceInfo, MICROPHONE_PERMISSION_ERROR};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicPermissionStatus {
    Granted,
    Denied,
    Restricted,
    NotDetermined,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionOutcome {
    Granted,
    Denied { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureStartError {
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Unsupported(String),
    PermissionDenied(String),
    Failed(String),
}

pub struct AudioCaptureCallbacks {
    pub on_level: Arc<dyn Fn(f32) + Send + Sync>,
    pub on_pcm: Arc<dyn Fn(Vec<f32>) + Send + Sync>,
    pub on_limit: Arc<dyn Fn() + Send + Sync>,
    pub on_error: Arc<dyn Fn(String) + Send + Sync>,
}

pub trait AudioCaptureSession: Send {
    fn stop(&mut self) -> Result<(), String>;
}

pub trait AudioPlatform: Send + Sync {
    fn list_input_devices(&self) -> Vec<AudioDeviceInfo>;
    fn permission_status(&self) -> MicPermissionStatus;
    fn request_permission(&self) -> Pin<Box<dyn Future<Output = PermissionOutcome> + Send>>;
    fn start_capture(
        &self,
        device_id: &str,
        meter_only: bool,
        callbacks: AudioCaptureCallbacks,
    ) -> Result<Box<dyn AudioCaptureSession>, CaptureStartError>;
    fn open_microphone_settings(&self) -> bool;
}

#[derive(Clone)]
struct InstalledAudioPlatform(Arc<dyn AudioPlatform>);

impl Global for InstalledAudioPlatform {}

#[cfg(any(test, feature = "snapshot"))]
pub fn install_audio_platform(platform: Arc<dyn AudioPlatform>, cx: &mut App) {
    cx.set_global(InstalledAudioPlatform(platform));
}

pub fn audio_platform(cx: &App) -> Arc<dyn AudioPlatform> {
    if let Some(installed) = cx.try_global::<InstalledAudioPlatform>() {
        return installed.0.clone();
    }
    default_audio_platform()
}

pub fn default_audio_platform() -> Arc<dyn AudioPlatform> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(super::macos::MacOsAudioPlatform)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(StubAudioPlatform::default())
    }
}

/// Deterministic stub audio platform used on non-macOS targets and headless builds.
#[cfg_attr(target_os = "macos", allow(dead_code))]
#[derive(Debug, Default)]
pub struct StubAudioPlatform {
    pub devices: Vec<AudioDeviceInfo>,
}

impl AudioPlatform for StubAudioPlatform {
    fn list_input_devices(&self) -> Vec<AudioDeviceInfo> {
        self.devices.clone()
    }

    fn permission_status(&self) -> MicPermissionStatus {
        MicPermissionStatus::Denied
    }

    fn request_permission(&self) -> Pin<Box<dyn Future<Output = PermissionOutcome> + Send>> {
        Box::pin(async {
            PermissionOutcome::Denied {
                message: MICROPHONE_PERMISSION_ERROR.to_owned(),
            }
        })
    }

    fn start_capture(
        &self,
        _device_id: &str,
        _meter_only: bool,
        _callbacks: AudioCaptureCallbacks,
    ) -> Result<Box<dyn AudioCaptureSession>, CaptureStartError> {
        Err(CaptureStartError::Unsupported(
            "Voice capture is not supported on this platform.".to_owned(),
        ))
    }

    fn open_microphone_settings(&self) -> bool {
        false
    }
}

/// Deterministic mock audio platform for unit, GPUI, and `--snapshot` tests.
#[cfg(any(test, feature = "snapshot"))]
#[cfg_attr(not(test), allow(dead_code))]
pub struct MockAudioPlatform {
    devices: Mutex<Vec<AudioDeviceInfo>>,
    permission: Mutex<MicPermissionStatus>,
    active: Mutex<Option<Arc<MockSessionState>>>,
}

#[cfg(any(test, feature = "snapshot"))]
#[cfg_attr(not(test), allow(dead_code))]
pub struct MockSessionState {
    callbacks: AudioCaptureCallbacks,
    stopped: AtomicBool,
}

#[cfg(any(test, feature = "snapshot"))]
struct MockCaptureSession {
    state: Arc<MockSessionState>,
}

#[cfg(any(test, feature = "snapshot"))]
impl AudioCaptureSession for MockCaptureSession {
    fn stop(&mut self) -> Result<(), String> {
        self.state.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(any(test, feature = "snapshot"))]
impl Default for MockAudioPlatform {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "snapshot"))]
#[cfg_attr(not(test), allow(dead_code))]
impl MockAudioPlatform {
    pub fn new() -> Self {
        Self {
            devices: Mutex::new(vec![
                AudioDeviceInfo { device_id: "default".into(), label: "System Default".into() },
                AudioDeviceInfo { device_id: "mock-mic-1".into(), label: "Studio Mic".into() },
            ]),
            permission: Mutex::new(MicPermissionStatus::Granted),
            active: Mutex::new(None),
        }
    }

    pub fn set_permission(&self, status: MicPermissionStatus) {
        *self.permission.lock().expect("permission lock") = status;
    }

    pub fn emit_level(&self, level: f32) {
        if let Some(state) = self.active.lock().expect("active lock").as_ref() {
            (state.callbacks.on_level)(level);
        }
    }

    pub fn emit_pcm(&self, samples: Vec<f32>) {
        if let Some(state) = self.active.lock().expect("active lock").as_ref() {
            (state.callbacks.on_pcm)(samples);
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.active
            .lock()
            .expect("active lock")
            .as_ref()
            .is_none_or(|state| state.stopped.load(Ordering::SeqCst))
    }
}

#[cfg(any(test, feature = "snapshot"))]
impl AudioPlatform for MockAudioPlatform {
    fn list_input_devices(&self) -> Vec<AudioDeviceInfo> {
        self.devices.lock().expect("devices lock").clone()
    }

    fn permission_status(&self) -> MicPermissionStatus {
        *self.permission.lock().expect("permission lock")
    }

    fn request_permission(&self) -> Pin<Box<dyn Future<Output = PermissionOutcome> + Send>> {
        let current = self.permission_status();
        if matches!(current, MicPermissionStatus::Granted) {
            Box::pin(async { PermissionOutcome::Granted })
        } else {
            Box::pin(async {
                PermissionOutcome::Denied {
                    message: MICROPHONE_PERMISSION_ERROR.to_owned(),
                }
            })
        }
    }

    fn start_capture(
        &self,
        _device_id: &str,
        _meter_only: bool,
        callbacks: AudioCaptureCallbacks,
    ) -> Result<Box<dyn AudioCaptureSession>, CaptureStartError> {
        if !matches!(self.permission_status(), MicPermissionStatus::Granted) {
            return Err(CaptureStartError::PermissionDenied(
                MICROPHONE_PERMISSION_ERROR.to_owned(),
            ));
        }
        let state = Arc::new(MockSessionState {
            callbacks,
            stopped: AtomicBool::new(false),
        });
        *self.active.lock().expect("active lock") = Some(state.clone());
        Ok(Box::new(MockCaptureSession { state }))
    }

    fn open_microphone_settings(&self) -> bool {
        true
    }
}
