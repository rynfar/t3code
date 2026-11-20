//! Native SnapShots screen capture: platform abstraction, macOS backend, capture coordinator,
//! and setup wizard dialog (`apps/web/src/components/desktop/SnapShotCoordinator.tsx`,
//! `apps/web/src/components/settings/SnapShotSetupDialog.tsx`,
//! `apps/desktop/src/snapShot/DesktopSnapShot.ts`, `MacSnapShot.ts`).

pub mod coordinator;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod setup_dialog;

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(test, feature = "snapshot"))]
use std::sync::Mutex;

use gpui::{App, Entity, Global};
use t3_core::settings::snapshot::{
    MAX_CAPTURE_HEIGHT, MAX_CAPTURE_WIDTH, MacSnapShotPermissions, SNAP_SHOT_MAX_IMAGE_BYTES,
    SnapShotShortcut, SnapShotSound, fit_capture_dimensions, has_valid_png_signature,
    next_resize_dimensions_for_byte_limit,
};

pub use coordinator::SnapShotCoordinator;
pub use setup_dialog::SnapShotSetupDialog;

static SNAPSHOT_MODE: AtomicBool = AtomicBool::new(false);

/// Marks the process as running in headless/snapshot mode so OS capture hooks, permission
/// prompts, `screencapture` execution, and audio playback never fire.
#[cfg_attr(not(feature = "snapshot"), allow(dead_code))]
pub fn set_snapshot_mode(enabled: bool) {
    SNAPSHOT_MODE.store(enabled, Ordering::SeqCst);
}

/// Returns `true` whenever OS side effects must be suppressed (`cargo test` or `--snapshot`).
pub fn is_os_side_effects_disabled() -> bool {
    cfg!(test)
        || SNAPSHOT_MODE.load(Ordering::Relaxed)
        || std::env::args().any(|arg| arg == "--snapshot")
}

/// Result of capturing a window screenshot (and optional Accessibility text summary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedWindow {
    pub capture_id: String,
    pub png_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub app_name: String,
    pub window_title: String,
    pub app_identifier: Option<String>,
    pub accessible_text: Option<String>,
    pub accessible_text_truncated: bool,
    pub captured_at_iso: String,
}

/// Active global capture shortcut registration; dropping tears down the OS hook cleanly.
pub trait ShortcutListenerHandle: Send {
    fn stop(&mut self);
}

/// Platform boundary for SnapShots permissions, capture, global shortcut, and sound.
pub trait SnapShotPlatform: Send + Sync {
    fn is_supported(&self) -> bool;
    fn mac_permissions(&self) -> Option<MacSnapShotPermissions>;
    fn request_screen_recording_permission(&self) -> bool;
    fn request_accessibility_permission(&self) -> bool;
    fn open_screen_recording_settings(&self) -> bool;
    fn open_accessibility_settings(&self) -> bool;
    fn capture_frontmost_window(&self, include_accessibility: bool)
    -> Result<CapturedWindow, String>;
    fn capture_test_window(&self, include_accessibility: bool) -> Result<CapturedWindow, String>;
    fn play_sound(&self, sound: SnapShotSound);
    fn register_global_shortcut(
        &self,
        shortcut: &SnapShotShortcut,
        on_trigger: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn ShortcutListenerHandle>, String>;
}

#[derive(Clone)]
struct InstalledSnapShotPlatform(Arc<dyn SnapShotPlatform>);

impl Global for InstalledSnapShotPlatform {}

#[cfg(any(test, feature = "snapshot"))]
#[allow(dead_code)]
pub fn install_platform(platform: Arc<dyn SnapShotPlatform>, cx: &mut App) {
    cx.set_global(InstalledSnapShotPlatform(platform));
}

pub fn platform(cx: &App) -> Arc<dyn SnapShotPlatform> {
    if let Some(installed) = cx.try_global::<InstalledSnapShotPlatform>() {
        return installed.0.clone();
    }
    default_platform()
}

pub fn default_platform() -> Arc<dyn SnapShotPlatform> {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        if !is_os_side_effects_disabled() {
            return Arc::new(macos::MacOsSnapShotPlatform);
        }
    }
    Arc::new(StubSnapShotPlatform)
}

/// Shared runtime handles installed by `Workspace` so Settings UI and overlays can coordinate
/// without modifying `settings/mod.rs`.
#[derive(Clone)]
struct ActiveHandles {
    coordinator: Entity<SnapShotCoordinator>,
    setup_dialog: Entity<SnapShotSetupDialog>,
}

impl Global for ActiveHandles {}

pub fn register_workspace_handles(
    coordinator: Entity<SnapShotCoordinator>,
    setup_dialog: Entity<SnapShotSetupDialog>,
    cx: &mut App,
) {
    cx.set_global(ActiveHandles { coordinator, setup_dialog });
}

pub fn active_coordinator(cx: &App) -> Option<Entity<SnapShotCoordinator>> {
    cx.try_global::<ActiveHandles>().map(|h| h.coordinator.clone())
}

pub fn active_setup_dialog(cx: &App) -> Option<Entity<SnapShotSetupDialog>> {
    cx.try_global::<ActiveHandles>().map(|h| h.setup_dialog.clone())
}

/// Validates PNG header and resizes the captured PNG so dimensions fit within
/// `MAX_CAPTURE_WIDTH x MAX_CAPTURE_HEIGHT` and byte size stays within
/// `SNAP_SHOT_MAX_IMAGE_BYTES` (`MacSnapShot.ts`, `SnapShotCoordinator.tsx`).
pub fn prepare_captured_png(raw_png: Vec<u8>) -> Result<(Vec<u8>, u32, u32), String> {
    if !has_valid_png_signature(&raw_png) {
        return Err("macOS returned an invalid snapshot.".to_owned());
    }
    let decoded = ::image::load_from_memory(&raw_png)
        .map_err(|_| "macOS returned an invalid snapshot.".to_owned())?;
    let (orig_w, orig_h) = (decoded.width(), decoded.height());
    if orig_w == 0 || orig_h == 0 {
        return Err("macOS returned an invalid snapshot.".to_owned());
    }

    let (fit_w, fit_h) =
        fit_capture_dimensions(orig_w, orig_h, MAX_CAPTURE_WIDTH, MAX_CAPTURE_HEIGHT);
    let mut current_img = if fit_w != orig_w || fit_h != orig_h {
        decoded.resize_exact(fit_w, fit_h, ::image::imageops::FilterType::Lanczos3)
    } else {
        decoded
    };
    let mut current_png = if fit_w != orig_w || fit_h != orig_h {
        encode_png(&current_img)?
    } else {
        raw_png
    };

    for _ in 0..8 {
        if (current_png.len() as u64) <= SNAP_SHOT_MAX_IMAGE_BYTES {
            return Ok((current_png, current_img.width(), current_img.height()));
        }
        let Some((next_w, next_h)) = next_resize_dimensions_for_byte_limit(
            current_img.width(),
            current_img.height(),
            current_png.len() as u64,
            SNAP_SHOT_MAX_IMAGE_BYTES,
        ) else {
            break;
        };
        current_img =
            current_img.resize_exact(next_w, next_h, ::image::imageops::FilterType::Lanczos3);
        current_png = encode_png(&current_img)?;
    }

    if (current_png.len() as u64) <= SNAP_SHOT_MAX_IMAGE_BYTES {
        Ok((current_png, current_img.width(), current_img.height()))
    } else {
        Err("The captured window is too large to attach.".to_owned())
    }
}

fn encode_png(image: &::image::DynamicImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut out), ::image::ImageFormat::Png)
        .map_err(|_| "Could not encode captured snapshot.".to_owned())?;
    Ok(out)
}

/// Clean unsupported fallback used on non-macOS targets and in headless/test runs when no mock
/// platform is installed.
#[derive(Debug, Default)]
pub struct StubSnapShotPlatform;

impl SnapShotPlatform for StubSnapShotPlatform {
    fn is_supported(&self) -> bool {
        false
    }

    fn mac_permissions(&self) -> Option<MacSnapShotPermissions> {
        None
    }

    fn request_screen_recording_permission(&self) -> bool {
        false
    }

    fn request_accessibility_permission(&self) -> bool {
        false
    }

    fn open_screen_recording_settings(&self) -> bool {
        false
    }

    fn open_accessibility_settings(&self) -> bool {
        false
    }

    fn capture_frontmost_window(
        &self,
        _include_accessibility: bool,
    ) -> Result<CapturedWindow, String> {
        Err("SnapShots capture is not supported on this platform.".to_owned())
    }

    fn capture_test_window(&self, _include_accessibility: bool) -> Result<CapturedWindow, String> {
        Err("SnapShots capture is not supported on this platform.".to_owned())
    }

    fn play_sound(&self, _sound: SnapShotSound) {}

    fn register_global_shortcut(
        &self,
        _shortcut: &SnapShotShortcut,
        _on_trigger: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn ShortcutListenerHandle>, String> {
        Err("SnapShots capture is not supported on this platform.".to_owned())
    }
}

/// Deterministic mock platform for unit and GPUI tests (zero OS dialogs, monitors, or audio).
#[cfg(any(test, feature = "snapshot"))]
#[cfg_attr(not(test), allow(dead_code))]
pub struct MockSnapShotPlatform {
    pub supported: AtomicBool,
    pub permissions: Mutex<Option<MacSnapShotPermissions>>,
    pub next_capture: Mutex<Result<CapturedWindow, String>>,
    pub register_error: Mutex<Option<String>>,
    pub played_sounds: Mutex<Vec<SnapShotSound>>,
    pub opened_screen_settings: Mutex<usize>,
    pub opened_accessibility_settings: Mutex<usize>,
    pub registered_shortcuts: Mutex<Vec<SnapShotShortcut>>,
    pub active_listener_stopped: Arc<AtomicBool>,
    pub trigger_callback: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[cfg(any(test, feature = "snapshot"))]
struct MockShortcutHandle {
    stopped: Arc<AtomicBool>,
}

#[cfg(any(test, feature = "snapshot"))]
impl ShortcutListenerHandle for MockShortcutHandle {
    fn stop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

#[cfg(any(test, feature = "snapshot"))]
impl Drop for MockShortcutHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(any(test, feature = "snapshot"))]
#[allow(dead_code)]
impl MockSnapShotPlatform {
    pub fn sample_png() -> Vec<u8> {
        let img = ::image::DynamicImage::new_rgba8(16, 12);
        encode_png(&img).expect("encode 16x12 PNG")
    }

    pub fn macos_ready() -> Self {
        let png_bytes = Self::sample_png();
        Self {
            supported: AtomicBool::new(true),
            permissions: Mutex::new(Some(MacSnapShotPermissions {
                screen_recording: true,
                accessibility: true,
            })),
            next_capture: Mutex::new(Ok(CapturedWindow {
                capture_id: "snap-test-1".to_owned(),
                png_bytes,
                width: 16,
                height: 12,
                app_name: "Safari".to_owned(),
                window_title: "T3 Code Docs".to_owned(),
                app_identifier: Some("com.apple.Safari".to_owned()),
                accessible_text: Some("Welcome to T3 Code".to_owned()),
                accessible_text_truncated: false,
                captured_at_iso: "2026-11-20T00:00:00Z".to_owned(),
            })),
            register_error: Mutex::new(None),
            played_sounds: Mutex::new(Vec::new()),
            opened_screen_settings: Mutex::new(0),
            opened_accessibility_settings: Mutex::new(0),
            registered_shortcuts: Mutex::new(Vec::new()),
            active_listener_stopped: Arc::new(AtomicBool::new(false)),
            trigger_callback: Mutex::new(None),
        }
    }

    pub fn set_permissions(&self, permissions: Option<MacSnapShotPermissions>) {
        *self.permissions.lock().expect("permissions lock") = permissions;
    }

    pub fn set_next_capture(&self, next: Result<CapturedWindow, String>) {
        *self.next_capture.lock().expect("capture lock") = next;
    }

    pub fn trigger_shortcut(&self) {
        if let Some(cb) = self.trigger_callback.lock().expect("cb lock").clone() {
            cb();
        }
    }
}

#[cfg(any(test, feature = "snapshot"))]
impl SnapShotPlatform for MockSnapShotPlatform {
    fn is_supported(&self) -> bool {
        self.supported.load(Ordering::SeqCst)
    }

    fn mac_permissions(&self) -> Option<MacSnapShotPermissions> {
        *self.permissions.lock().expect("permissions lock")
    }

    fn request_screen_recording_permission(&self) -> bool {
        self.permissions
            .lock()
            .expect("permissions lock")
            .is_some_and(|p| p.screen_recording)
    }

    fn request_accessibility_permission(&self) -> bool {
        self.permissions
            .lock()
            .expect("permissions lock")
            .is_some_and(|p| p.accessibility)
    }

    fn open_screen_recording_settings(&self) -> bool {
        *self.opened_screen_settings.lock().expect("screen settings lock") += 1;
        true
    }

    fn open_accessibility_settings(&self) -> bool {
        *self
            .opened_accessibility_settings
            .lock()
            .expect("ax settings lock") += 1;
        true
    }

    fn capture_frontmost_window(
        &self,
        include_accessibility: bool,
    ) -> Result<CapturedWindow, String> {
        let mut capture = self.next_capture.lock().expect("capture lock").clone()?;
        if !include_accessibility {
            capture.accessible_text = None;
            capture.accessible_text_truncated = false;
        }
        Ok(capture)
    }

    fn capture_test_window(&self, include_accessibility: bool) -> Result<CapturedWindow, String> {
        self.capture_frontmost_window(include_accessibility)
    }

    fn play_sound(&self, sound: SnapShotSound) {
        self.played_sounds.lock().expect("sounds lock").push(sound);
    }

    fn register_global_shortcut(
        &self,
        shortcut: &SnapShotShortcut,
        on_trigger: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn ShortcutListenerHandle>, String> {
        if let Some(err) = self.register_error.lock().expect("err lock").clone() {
            return Err(err);
        }
        self.active_listener_stopped.store(false, Ordering::SeqCst);
        self.registered_shortcuts.lock().expect("shortcuts lock").push(shortcut.clone());
        *self.trigger_callback.lock().expect("cb lock") = Some(on_trigger);
        Ok(Box::new(MockShortcutHandle { stopped: self.active_listener_stopped.clone() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_side_effects_are_always_disabled_in_tests() {
        assert!(is_os_side_effects_disabled());
        let platform = default_platform();
        assert!(!platform.is_supported());
        assert!(platform.mac_permissions().is_none());
    }

    #[test]
    fn prepare_captured_png_validates_signature_and_resizes_large_images() {
        assert!(prepare_captured_png(b"not a png".to_vec()).is_err());

        let small = MockSnapShotPlatform::sample_png();
        let (out, w, h) = prepare_captured_png(small).expect("valid 16x12 PNG");
        assert_eq!((w, h), (16, 12));
        assert!(has_valid_png_signature(&out));

        // Oversized image downscales to fit MAX_CAPTURE_WIDTH x MAX_CAPTURE_HEIGHT.
        let large_img = ::image::DynamicImage::new_rgba8(3000, 1500);
        let large_png = encode_png(&large_img).expect("encode large PNG");
        let (resized, rw, rh) = prepare_captured_png(large_png).expect("resized PNG");
        assert_eq!((rw, rh), (2560, 1280));
        assert!((resized.len() as u64) <= SNAP_SHOT_MAX_IMAGE_BYTES);
    }
}
