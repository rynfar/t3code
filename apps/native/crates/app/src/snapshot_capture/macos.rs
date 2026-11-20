//! macOS native SnapShots capture backend:
//! - Screen Recording (`CGPreflightScreenCaptureAccess`, `CGRequestScreenCaptureAccess`)
//! - Accessibility (`AXIsProcessTrusted`, `AXIsProcessTrustedWithOptions`, bounded AX text walk)
//! - System Settings privacy deep links (`Privacy_ScreenCapture`, `Privacy_Accessibility`)
//! - Frontmost non-self window lookup (`CGWindowListCopyWindowInfo` + `NSWorkspace`)
//! - Window capture via `/usr/sbin/screencapture -l <windowId> -o -x -t png <tempPath>`
//! - Global capture shortcut monitor (`NSEvent` global + local monitor for modifier pairs & chords)
//! - Shutter sound playback (`NSSound` with embedded Web MP3 assets)
//!
//! All OS side effects are strictly guarded behind `#[cfg(all(target_os = "macos", not(test)))]`
//! AND `!super::is_os_side_effects_disabled()`.

use t3_core::keybindings::KeybindingShortcut;

/// Maps macOS virtual key codes (`Carbon/Events.h` `kVK_*`) to normalized key names matching
/// `t3_core::keybindings::KeybindingShortcut::key`.
pub(crate) fn mac_keycode_to_key(code: u16) -> Option<&'static str> {
    match code {
        0x24 | 0x4C => Some("enter"),
        0x30 => Some("tab"),
        0x31 => Some(" "),
        0x33 => Some("backspace"),
        0x35 => Some("escape"),
        0x75 => Some("delete"),
        0x73 => Some("home"),
        0x77 => Some("end"),
        0x74 => Some("pageup"),
        0x79 => Some("pagedown"),
        0x7B => Some("arrowleft"),
        0x7C => Some("arrowright"),
        0x7D => Some("arrowdown"),
        0x7E => Some("arrowup"),
        0x7A => Some("f1"),
        0x78 => Some("f2"),
        0x63 => Some("f3"),
        0x76 => Some("f4"),
        0x60 => Some("f5"),
        0x61 => Some("f6"),
        0x62 => Some("f7"),
        0x64 => Some("f8"),
        0x65 => Some("f9"),
        0x6D => Some("f10"),
        0x67 => Some("f11"),
        0x6F => Some("f12"),
        0x69 => Some("f13"),
        0x6B => Some("f14"),
        0x71 => Some("f15"),
        0x6A => Some("f16"),
        0x40 => Some("f17"),
        0x4F => Some("f18"),
        0x50 => Some("f19"),
        0x5A => Some("f20"),
        _ => None,
    }
}

/// Pure matcher for macOS `NSEvent` keyDown modifier flags & key string against a `KeybindingShortcut`.
pub(crate) fn matches_mac_key_chord(
    chord: &KeybindingShortcut,
    modifier_flags: u64,
    key_code: u16,
    chars_ignoring_modifiers: Option<&str>,
) -> bool {
    const SHIFT: u64 = 1 << 17;
    const CONTROL: u64 = 1 << 18;
    const OPTION: u64 = 1 << 19;
    const COMMAND: u64 = 1 << 20;

    let has_shift = (modifier_flags & SHIFT) != 0;
    let has_ctrl = (modifier_flags & CONTROL) != 0;
    let has_alt = (modifier_flags & OPTION) != 0;
    let has_meta = (modifier_flags & COMMAND) != 0;

    let want_meta = chord.meta_key || chord.mod_key;
    let want_ctrl = chord.ctrl_key;
    if has_meta != want_meta
        || has_ctrl != want_ctrl
        || has_shift != chord.shift_key
        || has_alt != chord.alt_key
    {
        return false;
    }

    if let Some(named) = mac_keycode_to_key(key_code) {
        return named == chord.key;
    }
    let Some(chars) = chars_ignoring_modifiers else {
        return false;
    };
    chars.to_lowercase() == chord.key
}

#[cfg(all(target_os = "macos", not(test)))]
mod native {
    use std::cell::OnceCell;
    use std::collections::HashSet;
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::path::PathBuf;
    use std::ptr::{NonNull, null, null_mut};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{AnyThread, MainThreadMarker, msg_send};
    use objc2_app_kit::{NSSound, NSWorkspace};
    use objc2_foundation::{NSData, NSString, NSURL};
    use t3_core::settings::snapshot::{
        MAC_ACCESSIBILITY_SETTINGS_URL, MAC_SCREEN_CAPTURE_SETTINGS_URL, MacSnapShotPermissions,
        SNAP_SHOT_ACCESSIBLE_TEXT_MAX_CHARS, SnapShotShortcut, SnapShotSound,
        snap_shot_shortcut_registration_failure_message,
    };

    use super::matches_mac_key_chord;
    use crate::app_state::uuid_v4;
    use crate::snapshot_capture::{
        CapturedWindow, ShortcutListenerHandle, SnapShotPlatform, is_os_side_effects_disabled,
        prepare_captured_png,
    };

    const MAC_SCREEN_CAPTURE_PATH: &str = "/usr/sbin/screencapture";
    const SHORTCUT_COOLDOWN: Duration = Duration::from_millis(200);
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_CF_NUMBER_SINT32_TYPE: isize = 3;
    const K_CF_NUMBER_SINT64_TYPE: isize = 4;
    const K_CF_NUMBER_FLOAT64_TYPE: isize = 6;
    const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
    const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;
    const K_NS_EVENT_MASK_KEY_DOWN: u64 = 1 << 10;
    const K_NS_EVENT_MASK_FLAGS_CHANGED: u64 = 1 << 12;
    const MAX_AX_NODES: usize = 500;
    const MAX_AX_DEPTH: usize = 16;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
        fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> *const c_void;
        fn CGEventSourceFlagsState(state_id: i32) -> u64;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
        fn AXUIElementCopyAttributeValue(
            element: *const c_void,
            attribute: *const c_void,
            value: *mut *const c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
        fn CFRetain(cf: *const c_void) -> *const c_void;
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, idx: isize) -> *const c_void;
        fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
        fn CFNumberGetValue(
            number: *const c_void,
            the_type: isize,
            value_ptr: *mut c_void,
        ) -> bool;
        fn CFStringGetCString(
            the_string: *const c_void,
            buffer: *mut c_char,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;
        fn CFStringGetLength(the_string: *const c_void) -> isize;
        fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFGetTypeID(cf: *const c_void) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFDictionaryCreate(
            alloc: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        static kCFBooleanTrue: *const c_void;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
    }

    struct CfRef(*const c_void);

    impl CfRef {
        fn from_owned(ptr: *const c_void) -> Option<Self> {
            (!ptr.is_null()).then_some(Self(ptr))
        }

        fn from_borrowed(ptr: *const c_void) -> Option<Self> {
            if ptr.is_null() {
                return None;
            }
            // SAFETY: non-null CoreFoundation reference retained for RAII balance.
            unsafe { Some(Self(CFRetain(ptr))) }
        }

        fn as_ptr(&self) -> *const c_void {
            self.0
        }
    }

    impl Drop for CfRef {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` was created or retained by `CfRef`.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn cf_string(text: &str) -> Option<CfRef> {
        let cstr = CString::new(text).ok()?;
        // SAFETY: valid NUL-terminated UTF-8 string.
        let ptr = unsafe { CFStringCreateWithCString(null(), cstr.as_ptr(), K_CF_STRING_ENCODING_UTF8) };
        CfRef::from_owned(ptr)
    }

    fn cf_string_to_rust(ptr: *const c_void) -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        // SAFETY: verify CFString type id before reading contents.
        unsafe {
            if CFGetTypeID(ptr) != CFStringGetTypeID() {
                return None;
            }
            let len = CFStringGetLength(ptr);
            let max_size = CFStringGetMaximumSizeForEncoding(len, K_CF_STRING_ENCODING_UTF8) + 1;
            if max_size <= 0 {
                return None;
            }
            let mut buf = vec![0u8; max_size as usize];
            if CFStringGetCString(
                ptr,
                buf.as_mut_ptr().cast::<c_char>(),
                max_size,
                K_CF_STRING_ENCODING_UTF8,
            ) == 0
            {
                return None;
            }
            let cstr = CStr::from_ptr(buf.as_ptr().cast::<c_char>());
            Some(cstr.to_string_lossy().into_owned())
        }
    }

    fn dict_i64(dict: *const c_void, key: &str) -> Option<i64> {
        let k = cf_string(key)?;
        // SAFETY: `dict` is a valid CFDictionaryRef.
        let val = unsafe { CFDictionaryGetValue(dict, k.as_ptr()) };
        if val.is_null() {
            return None;
        }
        let mut out64: i64 = 0;
        // SAFETY: reading CFNumber into i64 buffer.
        if unsafe { CFNumberGetValue(val, K_CF_NUMBER_SINT64_TYPE, (&raw mut out64).cast()) } {
            return Some(out64);
        }
        let mut out32: i32 = 0;
        // SAFETY: reading CFNumber into i32 buffer.
        if unsafe { CFNumberGetValue(val, K_CF_NUMBER_SINT32_TYPE, (&raw mut out32).cast()) } {
            return Some(i64::from(out32));
        }
        None
    }

    fn dict_f64(dict: *const c_void, key: &str) -> Option<f64> {
        let k = cf_string(key)?;
        // SAFETY: `dict` is a valid CFDictionaryRef.
        let val = unsafe { CFDictionaryGetValue(dict, k.as_ptr()) };
        if val.is_null() {
            return None;
        }
        let mut out: f64 = 0.0;
        // SAFETY: reading CFNumber into f64 buffer.
        if unsafe { CFNumberGetValue(val, K_CF_NUMBER_FLOAT64_TYPE, (&raw mut out).cast()) } {
            Some(out)
        } else {
            None
        }
    }

    fn dict_string(dict: *const c_void, key: &str) -> Option<String> {
        let k = cf_string(key)?;
        // SAFETY: `dict` is a valid CFDictionaryRef.
        let val = unsafe { CFDictionaryGetValue(dict, k.as_ptr()) };
        cf_string_to_rust(val)
    }

    fn dict_bounds_non_tiny(dict: *const c_void) -> bool {
        let Some(k) = cf_string("kCGWindowBounds") else {
            return true;
        };
        // SAFETY: `dict` is a valid CFDictionaryRef.
        let bounds = unsafe { CFDictionaryGetValue(dict, k.as_ptr()) };
        if bounds.is_null() {
            return true;
        }
        let w = dict_f64(bounds, "Width").unwrap_or(0.0);
        let h = dict_f64(bounds, "Height").unwrap_or(0.0);
        w >= 10.0 && h >= 10.0
    }

    struct TempFileGuard(PathBuf);

    impl Drop for TempFileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    struct WindowCandidate {
        window_id: u32,
        owner_pid: i32,
        owner_name: String,
        window_title: String,
    }

    fn frontmost_app_pid() -> Option<i32> {
        // SAFETY: `NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier`.
        unsafe {
            let ws = NSWorkspace::sharedWorkspace();
            let app: Option<Retained<AnyObject>> = msg_send![&*ws, frontmostApplication];
            let app = app?;
            let pid: i32 = msg_send![&*app, processIdentifier];
            Some(pid)
        }
    }

    fn bundle_identifier_for_pid(pid: i32) -> Option<String> {
        // SAFETY: `NSRunningApplication.runningApplicationWithProcessIdentifier:`.
        unsafe {
            let cls = AnyClass::get(c"NSRunningApplication")?;
            let app: Option<Retained<AnyObject>> =
                msg_send![cls, runningApplicationWithProcessIdentifier: pid];
            let app = app?;
            let bundle_id: Option<Retained<NSString>> = msg_send![&*app, bundleIdentifier];
            bundle_id.map(|s| s.to_string()).filter(|s| !s.is_empty())
        }
    }

    fn find_target_window(exclude_self: bool) -> Option<WindowCandidate> {
        let self_pid = std::process::id() as i32;
        let front_pid = frontmost_app_pid().filter(|pid| !exclude_self || *pid != self_pid);
        // SAFETY: CoreGraphics returns an owned CFArrayRef of CFDictionaryRef entries in z-order.
        let list = CfRef::from_owned(unsafe {
            CGWindowListCopyWindowInfo(
                K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS,
                0,
            )
        })?;
        // SAFETY: `list` is a valid CFArrayRef.
        let count = unsafe { CFArrayGetCount(list.as_ptr()) };
        let mut fallback: Option<WindowCandidate> = None;

        for idx in 0..count {
            // SAFETY: `idx` is within `[0, count)`.
            let dict = unsafe { CFArrayGetValueAtIndex(list.as_ptr(), idx) };
            if dict.is_null() {
                continue;
            }
            if dict_i64(dict, "kCGWindowLayer").unwrap_or(0) != 0 {
                continue;
            }
            if dict_f64(dict, "kCGWindowAlpha").unwrap_or(1.0) <= 0.0 {
                continue;
            }
            if !dict_bounds_non_tiny(dict) {
                continue;
            }
            let Some(window_id) = dict_i64(dict, "kCGWindowNumber").and_then(|id| u32::try_from(id).ok()) else {
                continue;
            };
            let Some(owner_pid) = dict_i64(dict, "kCGWindowOwnerPID").and_then(|p| i32::try_from(p).ok()) else {
                continue;
            };
            if exclude_self && owner_pid == self_pid {
                continue;
            }
            let owner_name = dict_string(dict, "kCGWindowOwnerName").unwrap_or_default();
            let window_title = dict_string(dict, "kCGWindowName").unwrap_or_default();
            if owner_name.is_empty() && window_title.is_empty() {
                continue;
            }
            let candidate = WindowCandidate { window_id, owner_pid, owner_name, window_title };
            if front_pid == Some(owner_pid) {
                return Some(candidate);
            }
            if fallback.is_none() {
                fallback = Some(candidate);
            }
        }
        fallback
    }

    fn ax_copy_attr(element: *const c_void, name: &str) -> Option<CfRef> {
        let attr = cf_string(name)?;
        let mut value: *const c_void = null();
        // SAFETY: `element` is a valid AXUIElementRef and `attr` is a CFStringRef.
        let err = unsafe { AXUIElementCopyAttributeValue(element, attr.as_ptr(), &raw mut value) };
        if err == 0 { CfRef::from_owned(value) } else { None }
    }

    fn extract_accessibility_text(pid: i32) -> Option<(String, bool)> {
        // SAFETY: `AXIsProcessTrusted` has no side effects.
        if !unsafe { AXIsProcessTrusted() } {
            return None;
        }
        // SAFETY: creates an owned AXUIElementRef for `pid`.
        let app = CfRef::from_owned(unsafe { AXUIElementCreateApplication(pid) }.cast_const())?;
        let root = ax_copy_attr(app.as_ptr(), "AXFocusedWindow")
            .or_else(|| ax_copy_attr(app.as_ptr(), "AXMainWindow"))
            .or_else(|| {
                let windows = ax_copy_attr(app.as_ptr(), "AXWindows")?;
                // SAFETY: verify CFArray before indexing.
                unsafe {
                    if CFGetTypeID(windows.as_ptr()) == CFArrayGetTypeID()
                        && CFArrayGetCount(windows.as_ptr()) > 0
                    {
                        CfRef::from_borrowed(CFArrayGetValueAtIndex(windows.as_ptr(), 0))
                    } else {
                        None
                    }
                }
            })?;

        let mut stack: Vec<(CfRef, usize)> = vec![(root, 0)];
        let mut seen = HashSet::new();
        let mut out = String::new();
        let mut visited = 0usize;
        let mut truncated = false;

        while let Some((node, depth)) = stack.pop() {
            if visited >= MAX_AX_NODES {
                truncated = true;
                break;
            }
            visited += 1;

            for attr in ["AXTitle", "AXValue", "AXDescription"] {
                if let Some(val) = ax_copy_attr(node.as_ptr(), attr)
                    && let Some(raw) = cf_string_to_rust(val.as_ptr())
                {
                    let cleaned = raw.replace('\0', "");
                    let trimmed = cleaned.trim();
                    if !trimmed.is_empty() && seen.insert(trimmed.to_owned()) {
                        let sep = if out.is_empty() { "" } else { "\n" };
                        let current_chars = out.chars().count();
                        let remaining = SNAP_SHOT_ACCESSIBLE_TEXT_MAX_CHARS
                            .saturating_sub(current_chars + sep.len());
                        if remaining == 0 {
                            truncated = true;
                            break;
                        }
                        out.push_str(sep);
                        if trimmed.chars().count() > remaining {
                            out.extend(trimmed.chars().take(remaining));
                            truncated = true;
                            break;
                        }
                        out.push_str(trimmed);
                    }
                }
            }
            if truncated {
                break;
            }
            if depth >= MAX_AX_DEPTH {
                continue;
            }
            if let Some(children) = ax_copy_attr(node.as_ptr(), "AXChildren") {
                // SAFETY: verify CFArray before iterating.
                unsafe {
                    if CFGetTypeID(children.as_ptr()) == CFArrayGetTypeID() {
                        let count = CFArrayGetCount(children.as_ptr());
                        for idx in (0..count).rev() {
                            if let Some(child) =
                                CfRef::from_borrowed(CFArrayGetValueAtIndex(children.as_ptr(), idx))
                            {
                                stack.push((child, depth + 1));
                            }
                        }
                    }
                }
            }
        }

        (!out.is_empty()).then_some((out, truncated))
    }

    fn open_url(url_str: &str) -> bool {
        if is_os_side_effects_disabled() {
            return false;
        }
        let Some(url) = NSURL::URLWithString(&NSString::from_str(url_str)) else {
            return false;
        };
        NSWorkspace::sharedWorkspace().openURL(&url)
    }

    struct SnapShotSounds {
        whoosh: Option<Retained<NSSound>>,
        click: Option<Retained<NSSound>>,
    }

    impl SnapShotSounds {
        fn load() -> Self {
            let load = |bytes: &[u8]| {
                NSSound::initWithData(NSSound::alloc(), &NSData::with_bytes(bytes))
            };
            Self {
                whoosh: load(include_bytes!("assets/snap-shot-whoosh.mp3")),
                click: load(include_bytes!("assets/snap-shot-click.mp3")),
            }
        }
    }

    thread_local! {
        static SNAP_SHOT_SOUNDS: OnceCell<SnapShotSounds> = const { OnceCell::new() };
    }

    struct MonitorToken(*mut AnyObject);
    // SAFETY: monitor tokens are only removed via `NSEvent::removeMonitor:` during teardown.
    unsafe impl Send for MonitorToken {}

    struct SendKeepAlive<T>(T);
    // SAFETY: Objective-C blocks passed to `NSEvent` monitor APIs are copied by AppKit and
    // only dropped during main-thread teardown after `removeMonitor:`.
    unsafe impl<T> Send for SendKeepAlive<T> {}

    struct MacShortcutListenerHandle {
        tokens: Vec<MonitorToken>,
        _keep_alive: Vec<Box<dyn std::any::Any + Send>>,
        stopped: bool,
    }

    impl ShortcutListenerHandle for MacShortcutListenerHandle {
        fn stop(&mut self) {
            if self.stopped {
                return;
            }
            self.stopped = true;
            if MainThreadMarker::new().is_some()
                && let Some(ns_event) = AnyClass::get(c"NSEvent")
            {
                for token in self.tokens.drain(..) {
                    if !token.0.is_null() {
                        // SAFETY: `token.0` was returned by `addGlobal/LocalMonitorForEventsMatchingMask:handler:`.
                        unsafe {
                            let _: () = msg_send![ns_event, removeMonitor: token.0];
                            let retained = Retained::from_raw(token.0);
                            drop(retained);
                        }
                    }
                }
            } else {
                self.tokens.clear();
            }
            self._keep_alive.clear();
        }
    }

    impl Drop for MacShortcutListenerHandle {
        fn drop(&mut self) {
            self.stop();
        }
    }

    pub struct MacOsSnapShotPlatform;

    impl MacOsSnapShotPlatform {
        fn capture_window_inner(
            &self,
            exclude_self: bool,
            include_accessibility: bool,
        ) -> Result<CapturedWindow, String> {
            if is_os_side_effects_disabled() {
                return Err("SnapShots capture is disabled in test/snapshot mode.".to_owned());
            }
            // SAFETY: pure preflight check without prompting.
            if !unsafe { CGPreflightScreenCaptureAccess() } {
                return Err(
                    "Allow Screen Recording in System Settings, then restart T3 Code.".to_owned(),
                );
            }
            let target = find_target_window(exclude_self)
                .or_else(|| if exclude_self { None } else { find_target_window(false) })
                .ok_or_else(|| "No capturable window was found.".to_owned())?;

            let capture_id = format!("snap-{}", uuid_v4());
            let temp_path = std::env::temp_dir().join(format!("t3-snapshot-{capture_id}.png"));
            let _guard = TempFileGuard(temp_path.clone());

            let output = std::process::Command::new(MAC_SCREEN_CAPTURE_PATH)
                .args(["-l", &target.window_id.to_string(), "-o", "-x", "-t", "png"])
                .arg(&temp_path)
                .output()
                .map_err(|err| format!("Could not capture window: {err}"))?;
            if !output.status.success() {
                return Err("macOS could not capture the active window.".to_owned());
            }

            let raw_png = std::fs::read(&temp_path)
                .map_err(|_| "macOS returned an invalid snapshot.".to_owned())?;
            let (png_bytes, width, height) = prepare_captured_png(raw_png)?;

            let app_identifier = bundle_identifier_for_pid(target.owner_pid);
            let (accessible_text, accessible_text_truncated) = if include_accessibility {
                extract_accessibility_text(target.owner_pid)
                    .map(|(text, trunc)| (Some(text), trunc))
                    .unwrap_or((None, false))
            } else {
                (None, false)
            };

            let app_name = if !target.owner_name.trim().is_empty() {
                target.owner_name
            } else {
                "Window".to_owned()
            };
            let captured_at_iso = chrono::Utc::now().to_rfc3339();

            Ok(CapturedWindow {
                capture_id,
                png_bytes,
                width,
                height,
                app_name,
                window_title: target.window_title,
                app_identifier,
                accessible_text,
                accessible_text_truncated,
                captured_at_iso,
            })
        }
    }

    impl SnapShotPlatform for MacOsSnapShotPlatform {
        fn is_supported(&self) -> bool {
            !is_os_side_effects_disabled()
        }

        fn mac_permissions(&self) -> Option<MacSnapShotPermissions> {
            if is_os_side_effects_disabled() {
                return None;
            }
            // SAFETY: non-prompting macOS TCC status checks.
            let screen_recording = unsafe { CGPreflightScreenCaptureAccess() };
            let accessibility = unsafe { AXIsProcessTrusted() };
            Some(MacSnapShotPermissions { screen_recording, accessibility })
        }

        fn request_screen_recording_permission(&self) -> bool {
            if is_os_side_effects_disabled() {
                return false;
            }
            // SAFETY: standard CoreGraphics Screen Recording permission request.
            unsafe { CGRequestScreenCaptureAccess() }
        }

        fn request_accessibility_permission(&self) -> bool {
            if is_os_side_effects_disabled() {
                return false;
            }
            let Some(prompt_key) = cf_string("AXTrustedCheckOptionPrompt") else {
                return false;
            };
            // SAFETY: builds `{ kAXTrustedCheckOptionPrompt: kCFBooleanTrue }` CFDictionary.
            unsafe {
                let keys = [prompt_key.as_ptr()];
                let values = [kCFBooleanTrue];
                let opts = CfRef::from_owned(CFDictionaryCreate(
                    null(),
                    keys.as_ptr(),
                    values.as_ptr(),
                    1,
                    &raw const kCFTypeDictionaryKeyCallBacks,
                    &raw const kCFTypeDictionaryValueCallBacks,
                ));
                let Some(opts) = opts else {
                    return false;
                };
                AXIsProcessTrustedWithOptions(opts.as_ptr())
            }
        }

        fn open_screen_recording_settings(&self) -> bool {
            open_url(MAC_SCREEN_CAPTURE_SETTINGS_URL)
        }

        fn open_accessibility_settings(&self) -> bool {
            open_url(MAC_ACCESSIBILITY_SETTINGS_URL)
        }

        fn capture_frontmost_window(
            &self,
            include_accessibility: bool,
        ) -> Result<CapturedWindow, String> {
            self.capture_window_inner(true, include_accessibility)
        }

        fn capture_test_window(
            &self,
            include_accessibility: bool,
        ) -> Result<CapturedWindow, String> {
            self.capture_window_inner(false, include_accessibility)
        }

        fn play_sound(&self, sound: SnapShotSound) {
            if is_os_side_effects_disabled() || MainThreadMarker::new().is_none() {
                return;
            }
            SNAP_SHOT_SOUNDS.with(|cell| {
                let sounds = cell.get_or_init(SnapShotSounds::load);
                let target = match sound {
                    SnapShotSound::SoftPop => &sounds.whoosh,
                    SnapShotSound::CameraShutter => &sounds.click,
                };
                if let Some(ns_sound) = target {
                    ns_sound.stop();
                    ns_sound.play();
                }
            });
        }

        fn register_global_shortcut(
            &self,
            shortcut: &SnapShotShortcut,
            on_trigger: Arc<dyn Fn() + Send + Sync>,
        ) -> Result<Box<dyn ShortcutListenerHandle>, String> {
            if is_os_side_effects_disabled() {
                return Err("SnapShots shortcuts are disabled in test/snapshot mode.".to_owned());
            }
            if MainThreadMarker::new().is_none() {
                return Err(snap_shot_shortcut_registration_failure_message(shortcut, true));
            }
            let Some(ns_event) = AnyClass::get(c"NSEvent") else {
                return Err(snap_shot_shortcut_registration_failure_message(shortcut, true));
            };

            let last_fired = Arc::new(Mutex::new(None::<Instant>));
            let fire_with_cooldown = {
                let last_fired = last_fired.clone();
                let on_trigger = on_trigger.clone();
                Arc::new(move || {
                    let mut guard = last_fired.lock().unwrap_or_else(|e| e.into_inner());
                    let now = Instant::now();
                    if guard.is_some_and(|prev| now.duration_since(prev) < SHORTCUT_COOLDOWN) {
                        return;
                    }
                    *guard = Some(now);
                    drop(guard);
                    on_trigger();
                })
            };

            let mut tokens = Vec::new();
            let mut keep_alive: Vec<Box<dyn std::any::Any + Send>> = Vec::new();

            if let Some(modifier) = shortcut.modifier_pair() {
                let (left_mask, right_mask) = modifier.mac_device_masks();
                let both = left_mask | right_mask;
                let active = Arc::new(AtomicBool::new(false));
                let check_flags = {
                    let active = active.clone();
                    let fire = fire_with_cooldown.clone();
                    Arc::new(move |event_flags: u64| {
                        // SAFETY: `CGEventSourceFlagsState(0)` reads live combined hardware flags.
                        let live_flags = unsafe { CGEventSourceFlagsState(0) };
                        let flags = live_flags | event_flags;
                        let pressed = (flags & both) == both;
                        let was_pressed = active.swap(pressed, Ordering::SeqCst);
                        if pressed && !was_pressed {
                            fire();
                        }
                    })
                };

                let check_global = check_flags.clone();
                let global_block = RcBlock::new(move |event: NonNull<AnyObject>| {
                    // SAFETY: `NSEvent.modifierFlags` returns `NSUInteger`.
                    let flags: usize = unsafe { msg_send![event.as_ptr(), modifierFlags] };
                    check_global(flags as u64);
                });
                // SAFETY: registering AppKit global FlagsChanged monitor on the main thread.
                let global_token: Option<Retained<AnyObject>> = unsafe {
                    msg_send![
                        ns_event,
                        addGlobalMonitorForEventsMatchingMask: K_NS_EVENT_MASK_FLAGS_CHANGED,
                        handler: &*global_block
                    ]
                };
                if let Some(token) = global_token {
                    tokens.push(MonitorToken(Retained::into_raw(token)));
                }
                keep_alive.push(Box::new(SendKeepAlive(global_block)));

                let check_local = check_flags;
                let local_block = RcBlock::new(move |event: NonNull<AnyObject>| -> *mut AnyObject {
                    // SAFETY: `NSEvent.modifierFlags` returns `NSUInteger`.
                    let flags: usize = unsafe { msg_send![event.as_ptr(), modifierFlags] };
                    check_local(flags as u64);
                    event.as_ptr()
                });
                // SAFETY: registering AppKit local FlagsChanged monitor on the main thread.
                let local_token: Option<Retained<AnyObject>> = unsafe {
                    msg_send![
                        ns_event,
                        addLocalMonitorForEventsMatchingMask: K_NS_EVENT_MASK_FLAGS_CHANGED,
                        handler: &*local_block
                    ]
                };
                if let Some(token) = local_token {
                    tokens.push(MonitorToken(Retained::into_raw(token)));
                }
                keep_alive.push(Box::new(SendKeepAlive(local_block)));
            } else if let SnapShotShortcut::KeyChord(chord) = shortcut {
                let chord_global = chord.clone();
                let fire_global = fire_with_cooldown.clone();
                let global_block = RcBlock::new(move |event: NonNull<AnyObject>| {
                    // SAFETY: reading `NSEvent` keyDown properties.
                    unsafe {
                        let flags: usize = msg_send![event.as_ptr(), modifierFlags];
                        let key_code: u16 = msg_send![event.as_ptr(), keyCode];
                        let chars: Option<Retained<NSString>> =
                            msg_send![event.as_ptr(), charactersIgnoringModifiers];
                        let chars_str = chars.map(|s| s.to_string());
                        if matches_mac_key_chord(
                            &chord_global,
                            flags as u64,
                            key_code,
                            chars_str.as_deref(),
                        ) {
                            fire_global();
                        }
                    }
                });
                // SAFETY: registering AppKit global KeyDown monitor on the main thread.
                let global_token: Option<Retained<AnyObject>> = unsafe {
                    msg_send![
                        ns_event,
                        addGlobalMonitorForEventsMatchingMask: K_NS_EVENT_MASK_KEY_DOWN,
                        handler: &*global_block
                    ]
                };
                if let Some(token) = global_token {
                    tokens.push(MonitorToken(Retained::into_raw(token)));
                }
                keep_alive.push(Box::new(SendKeepAlive(global_block)));

                let chord_local = chord.clone();
                let fire_local = fire_with_cooldown;
                let local_block = RcBlock::new(move |event: NonNull<AnyObject>| -> *mut AnyObject {
                    // SAFETY: reading `NSEvent` keyDown properties.
                    let matched = unsafe {
                        let flags: usize = msg_send![event.as_ptr(), modifierFlags];
                        let key_code: u16 = msg_send![event.as_ptr(), keyCode];
                        let chars: Option<Retained<NSString>> =
                            msg_send![event.as_ptr(), charactersIgnoringModifiers];
                        let chars_str = chars.map(|s| s.to_string());
                        matches_mac_key_chord(
                            &chord_local,
                            flags as u64,
                            key_code,
                            chars_str.as_deref(),
                        )
                    };
                    if matched {
                        fire_local();
                        null_mut()
                    } else {
                        event.as_ptr()
                    }
                });
                // SAFETY: registering AppKit local KeyDown monitor on the main thread.
                let local_token: Option<Retained<AnyObject>> = unsafe {
                    msg_send![
                        ns_event,
                        addLocalMonitorForEventsMatchingMask: K_NS_EVENT_MASK_KEY_DOWN,
                        handler: &*local_block
                    ]
                };
                if let Some(token) = local_token {
                    tokens.push(MonitorToken(Retained::into_raw(token)));
                }
                keep_alive.push(Box::new(SendKeepAlive(local_block)));
            }

            if tokens.is_empty() {
                return Err(snap_shot_shortcut_registration_failure_message(shortcut, true));
            }

            Ok(Box::new(MacShortcutListenerHandle {
                tokens,
                _keep_alive: keep_alive,
                stopped: false,
            }))
        }
    }
}

#[cfg(all(target_os = "macos", not(test)))]
pub use native::MacOsSnapShotPlatform;

#[cfg(test)]
mod tests {
    use super::*;
    use t3_core::keybindings::parse_keybinding_shortcut;

    #[test]
    fn matches_macos_key_chord_with_modifiers_and_special_keys() {
        const SHIFT: u64 = 1 << 17;
        const COMMAND: u64 = 1 << 20;

        let mod_shift_2 = parse_keybinding_shortcut("mod+shift+2").unwrap();
        assert!(matches_mac_key_chord(&mod_shift_2, COMMAND | SHIFT, 0x13, Some("2")));
        assert!(!matches_mac_key_chord(&mod_shift_2, COMMAND, 0x13, Some("2")));

        let mod_f12 = parse_keybinding_shortcut("mod+f12").unwrap();
        assert!(matches_mac_key_chord(&mod_f12, COMMAND, 0x6F, None));
        assert!(!matches_mac_key_chord(&mod_f12, COMMAND, 0x7A, None));
    }
}
