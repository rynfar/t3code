use std::ffi::{CStr, c_char, c_void};
use std::future::Future;
use std::pin::Pin;
use std::ptr::{NonNull, null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::msg_send;
use objc2::runtime::{AnyClass, Bool};
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSURL};
use t3_core::voice_input::{
    AudioDeviceInfo, MICROPHONE_PERMISSION_ERROR, PcmCaptureEvent, PcmCaptureProcessor,
};

use super::platform::{
    AudioCaptureCallbacks, AudioCaptureSession, AudioPlatform, CaptureStartError,
    MicPermissionStatus, PermissionOutcome,
};

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {}
#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {}
#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

type OSStatus = i32;
type AudioObjectID = u32;
type AudioObjectPropertySelector = u32;
type AudioObjectPropertyScope = u32;
type AudioObjectPropertyElement = u32;
type AudioQueueRef = *mut c_void;
type AudioQueuePropertyID = u32;
type CFStringRef = *const c_void;
type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;

const K_AUDIO_OBJECT_SYSTEM_OBJECT: AudioObjectID = 1;
const K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL: AudioObjectPropertyScope = fourcc(b"glob");
const K_AUDIO_OBJECT_PROPERTY_SCOPE_INPUT: AudioObjectPropertyScope = fourcc(b"inpt");
const K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: AudioObjectPropertyElement = 0;

const K_AUDIO_HARDWARE_PROPERTY_DEVICES: AudioObjectPropertySelector = fourcc(b"dev#");
const K_AUDIO_HARDWARE_PROPERTY_DEFAULT_INPUT_DEVICE: AudioObjectPropertySelector = fourcc(b"dIn ");
const K_AUDIO_OBJECT_PROPERTY_NAME: AudioObjectPropertySelector = fourcc(b"lnam");
const K_AUDIO_DEVICE_PROPERTY_DEVICE_UID: AudioObjectPropertySelector = fourcc(b"uid ");
const K_AUDIO_DEVICE_PROPERTY_STREAMS: AudioObjectPropertySelector = fourcc(b"stm#");

const K_AUDIO_FORMAT_LINEAR_PCM: u32 = fourcc(b"lpcm");
const K_AUDIO_FORMAT_FLAG_IS_FLOAT: u32 = 1 << 0;
const K_AUDIO_FORMAT_FLAG_IS_PACKED: u32 = 1 << 3;

const K_AUDIO_QUEUE_PROPERTY_IS_RUNNING: AudioQueuePropertyID = fourcc(b"aqrn");
const K_AUDIO_QUEUE_PROPERTY_CURRENT_DEVICE: AudioQueuePropertyID = fourcc(b"aqcd");

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const CAPTURE_SAMPLE_RATE: u32 = 48_000;
const BUFFER_FRAMES: u32 = 2048;
const BUFFER_COUNT: usize = 3;

const fn fourcc(bytes: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*bytes)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioObjectPropertyAddress {
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioStreamBasicDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
struct AudioQueueBuffer {
    audio_data_bytes_capacity: u32,
    audio_data: *mut c_void,
    audio_data_byte_size: u32,
    user_data: *mut c_void,
    packet_description_capacity: u32,
    packet_descriptions: *mut c_void,
    packet_description_count: u32,
}

type AudioQueueInputCallback = unsafe extern "C" fn(
    in_user_data: *mut c_void,
    in_aq: AudioQueueRef,
    in_buffer: *mut AudioQueueBuffer,
    in_start_time: *const c_void,
    in_number_packet_descriptions: u32,
    in_packet_descs: *const c_void,
);

type AudioQueuePropertyListenerProc = unsafe extern "C" fn(
    in_user_data: *mut c_void,
    in_aq: AudioQueueRef,
    in_id: AudioQueuePropertyID,
);

unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(
        in_object_id: AudioObjectID,
        in_address: *const AudioObjectPropertyAddress,
        in_qualifier_data_size: u32,
        in_qualifier_data: *const c_void,
        out_data_size: *mut u32,
    ) -> OSStatus;

    fn AudioObjectGetPropertyData(
        in_object_id: AudioObjectID,
        in_address: *const AudioObjectPropertyAddress,
        in_qualifier_data_size: u32,
        in_qualifier_data: *const c_void,
        io_data_size: *mut u32,
        out_data: *mut c_void,
    ) -> OSStatus;

    fn AudioQueueNewInput(
        in_format: *const AudioStreamBasicDescription,
        in_callback_proc: AudioQueueInputCallback,
        in_user_data: *mut c_void,
        in_callback_run_loop: *const c_void,
        in_callback_run_loop_mode: CFStringRef,
        in_flags: u32,
        out_aq: *mut AudioQueueRef,
    ) -> OSStatus;

    fn AudioQueueAllocateBuffer(
        in_aq: AudioQueueRef,
        in_buffer_byte_size: u32,
        out_buffer: *mut *mut AudioQueueBuffer,
    ) -> OSStatus;

    fn AudioQueueEnqueueBuffer(
        in_aq: AudioQueueRef,
        in_buffer: *mut AudioQueueBuffer,
        in_num_packet_descs: u32,
        in_packet_descs: *const c_void,
    ) -> OSStatus;

    fn AudioQueueStart(in_aq: AudioQueueRef, in_start_time: *const c_void) -> OSStatus;
    fn AudioQueueStop(in_aq: AudioQueueRef, in_immediate: u8) -> OSStatus;
    fn AudioQueueDispose(in_aq: AudioQueueRef, in_immediate: u8) -> OSStatus;

    fn AudioQueueGetProperty(
        in_aq: AudioQueueRef,
        in_id: AudioQueuePropertyID,
        out_data: *mut c_void,
        io_data_size: *mut u32,
    ) -> OSStatus;

    fn AudioQueueSetProperty(
        in_aq: AudioQueueRef,
        in_id: AudioQueuePropertyID,
        in_data: *const c_void,
        in_data_size: u32,
    ) -> OSStatus;

    fn AudioQueueAddPropertyListener(
        in_aq: AudioQueueRef,
        in_id: AudioQueuePropertyID,
        in_proc: AudioQueuePropertyListenerProc,
        in_user_data: *mut c_void,
    ) -> OSStatus;

    fn AudioQueueRemovePropertyListener(
        in_aq: AudioQueueRef,
        in_id: AudioQueuePropertyID,
        in_proc: AudioQueuePropertyListenerProc,
        in_user_data: *mut c_void,
    ) -> OSStatus;

    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;

    fn CFStringGetLength(the_string: CFStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
    fn CFRelease(cf: CFTypeRef);
}

#[derive(Debug, Default)]
pub struct MacOsAudioPlatform;

impl AudioPlatform for MacOsAudioPlatform {
    fn list_input_devices(&self) -> Vec<AudioDeviceInfo> {
        enumerate_macos_input_devices()
    }

    fn permission_status(&self) -> MicPermissionStatus {
        avfoundation_permission_status()
    }

    fn request_permission(&self) -> Pin<Box<dyn Future<Output = PermissionOutcome> + Send>> {
        Box::pin(request_avfoundation_permission())
    }

    fn start_capture(
        &self,
        device_id: &str,
        meter_only: bool,
        callbacks: AudioCaptureCallbacks,
    ) -> Result<Box<dyn AudioCaptureSession>, CaptureStartError> {
        match self.permission_status() {
            MicPermissionStatus::Denied | MicPermissionStatus::Restricted => {
                return Err(CaptureStartError::PermissionDenied(
                    MICROPHONE_PERMISSION_ERROR.to_owned(),
                ));
            }
            MicPermissionStatus::Granted | MicPermissionStatus::NotDetermined => {}
        }
        let session = MacOsAudioCaptureSession::start(device_id, meter_only, callbacks)?;
        Ok(Box::new(session))
    }

    fn open_microphone_settings(&self) -> bool {
        let url_str =
            NSString::from_str("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone");
        let Some(url) = NSURL::URLWithString(&url_str) else {
            return false;
        };
        NSWorkspace::sharedWorkspace().openURL(&url)
    }
}

fn avfoundation_permission_status() -> MicPermissionStatus {
    unsafe {
        let Some(cls) = AnyClass::get(c"AVCaptureDevice") else {
            return MicPermissionStatus::NotDetermined;
        };
        let media_type = NSString::from_str("soun");
        let status: isize = msg_send![cls, authorizationStatusForMediaType: &*media_type];
        match status {
            1 => MicPermissionStatus::Restricted,
            2 => MicPermissionStatus::Denied,
            3 => MicPermissionStatus::Granted,
            _ => MicPermissionStatus::NotDetermined,
        }
    }
}

async fn request_avfoundation_permission() -> PermissionOutcome {
    match avfoundation_permission_status() {
        MicPermissionStatus::Granted => return PermissionOutcome::Granted,
        MicPermissionStatus::Denied | MicPermissionStatus::Restricted => {
            return PermissionOutcome::Denied {
                message: MICROPHONE_PERMISSION_ERROR.to_owned(),
            };
        }
        MicPermissionStatus::NotDetermined => {}
    }

    let (tx, rx) = futures::channel::oneshot::channel();
    {
        let tx = Mutex::new(Some(tx));
        let block = RcBlock::new(move |granted: Bool| {
            if let Some(tx) = tx.lock().expect("permission tx lock").take() {
                let outcome = if granted.as_bool() {
                    PermissionOutcome::Granted
                } else {
                    PermissionOutcome::Denied {
                        message: MICROPHONE_PERMISSION_ERROR.to_owned(),
                    }
                };
                let _ = tx.send(outcome);
            }
        });

        unsafe {
            let Some(cls) = AnyClass::get(c"AVCaptureDevice") else {
                return PermissionOutcome::Denied {
                    message: MICROPHONE_PERMISSION_ERROR.to_owned(),
                };
            };
            let media_type = NSString::from_str("soun");
            let _: () =
                msg_send![cls, requestAccessForMediaType: &*media_type, completionHandler: &*block];
        }
    }

    rx.await.unwrap_or_else(|_| PermissionOutcome::Denied {
        message: MICROPHONE_PERMISSION_ERROR.to_owned(),
    })
}

fn enumerate_macos_input_devices() -> Vec<AudioDeviceInfo> {
    let mut devices = Vec::new();
    let default_id = default_input_device_id();
    let mut default_name: Option<String> = None;
    let mut physical = Vec::new();

    for object_id in all_audio_devices() {
        if !device_has_input_streams(object_id) {
            continue;
        }
        let Some(uid) = device_cfstring_property(object_id, K_AUDIO_DEVICE_PROPERTY_DEVICE_UID)
        else {
            continue;
        };
        let name = device_cfstring_property(object_id, K_AUDIO_OBJECT_PROPERTY_NAME)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| uid.clone());
        if default_id == Some(object_id) {
            default_name = Some(name.clone());
        }
        physical.push(AudioDeviceInfo { device_id: uid, label: name });
    }

    let default_label = match default_name {
        Some(name) => format!("System Default ({name})"),
        None => "System Default".to_owned(),
    };
    devices.push(AudioDeviceInfo {
        device_id: "default".to_owned(),
        label: default_label,
    });
    devices.extend(physical);
    devices
}

fn all_audio_devices() -> Vec<AudioObjectID> {
    let address = AudioObjectPropertyAddress {
        selector: K_AUDIO_HARDWARE_PROPERTY_DEVICES,
        scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut size: u32 = 0;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(K_AUDIO_OBJECT_SYSTEM_OBJECT, &address, 0, null(), &mut size)
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    let count = (size as usize) / std::mem::size_of::<AudioObjectID>();
    let mut ids = vec![0u32; count];
    let status = unsafe {
        AudioObjectGetPropertyData(
            K_AUDIO_OBJECT_SYSTEM_OBJECT,
            &address,
            0,
            null(),
            &mut size,
            ids.as_mut_ptr().cast::<c_void>(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids.truncate((size as usize) / std::mem::size_of::<AudioObjectID>());
    ids
}

fn default_input_device_id() -> Option<AudioObjectID> {
    let address = AudioObjectPropertyAddress {
        selector: K_AUDIO_HARDWARE_PROPERTY_DEFAULT_INPUT_DEVICE,
        scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut device_id: AudioObjectID = 0;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            K_AUDIO_OBJECT_SYSTEM_OBJECT,
            &address,
            0,
            null(),
            &mut size,
            (&raw mut device_id).cast::<c_void>(),
        )
    };
    (status == 0 && device_id != 0).then_some(device_id)
}

fn device_has_input_streams(object_id: AudioObjectID) -> bool {
    let address = AudioObjectPropertyAddress {
        selector: K_AUDIO_DEVICE_PROPERTY_STREAMS,
        scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_INPUT,
        element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut size: u32 = 0;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(object_id, &address, 0, null(), &mut size)
    };
    status == 0 && size > 0
}

fn device_cfstring_property(
    object_id: AudioObjectID,
    selector: AudioObjectPropertySelector,
) -> Option<String> {
    let address = AudioObjectPropertyAddress {
        selector,
        scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut cf_str: CFStringRef = null();
    let mut size = std::mem::size_of::<CFStringRef>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            &address,
            0,
            null(),
            &mut size,
            (&raw mut cf_str).cast::<c_void>(),
        )
    };
    if status != 0 || cf_str.is_null() {
        return None;
    }
    let result = cfstring_to_rust(cf_str);
    unsafe { CFRelease(cf_str) };
    result
}

fn cfstring_to_rust(cf_str: CFStringRef) -> Option<String> {
    let len = unsafe { CFStringGetLength(cf_str) };
    if len <= 0 {
        return Some(String::new());
    }
    let max_bytes = unsafe { CFStringGetMaximumSizeForEncoding(len, K_CF_STRING_ENCODING_UTF8) };
    if max_bytes <= 0 {
        return None;
    }
    let mut buf = vec![0u8; (max_bytes as usize) + 1];
    let ok = unsafe {
        CFStringGetCString(
            cf_str,
            buf.as_mut_ptr().cast::<c_char>(),
            buf.len() as isize,
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr().cast::<c_char>()) };
    cstr.to_str().ok().map(str::to_owned)
}

struct CaptureInner {
    processor: Mutex<PcmCaptureProcessor>,
    callbacks: AudioCaptureCallbacks,
    stopping: AtomicBool,
    error_reported: AtomicBool,
}

impl CaptureInner {
    fn dispatch_events(&self, events: Vec<PcmCaptureEvent>) {
        for event in events {
            match event {
                PcmCaptureEvent::Level(level) => (self.callbacks.on_level)(level),
                PcmCaptureEvent::PcmChunk(chunk) => (self.callbacks.on_pcm)(chunk),
                PcmCaptureEvent::Limit => {
                    if !self.stopping.swap(true, Ordering::SeqCst) {
                        (self.callbacks.on_limit)();
                    }
                }
                PcmCaptureEvent::Stopped => {}
            }
        }
    }
}

struct MacOsAudioCaptureSession {
    queue: NonNull<c_void>,
    inner: Arc<CaptureInner>,
    user_data: *mut c_void,
    stopped: bool,
}

unsafe impl Send for MacOsAudioCaptureSession {}

impl MacOsAudioCaptureSession {
    fn start(
        device_id: &str,
        meter_only: bool,
        callbacks: AudioCaptureCallbacks,
    ) -> Result<Self, CaptureStartError> {
        let mut processor = PcmCaptureProcessor::new(CAPTURE_SAMPLE_RATE, meter_only)
            .map_err(|err| CaptureStartError::Failed(err.to_string()))?;
        processor.start();
        let inner = Arc::new(CaptureInner {
            processor: Mutex::new(processor),
            callbacks,
            stopping: AtomicBool::new(false),
            error_reported: AtomicBool::new(false),
        });
        let user_data = Arc::into_raw(inner.clone()).cast_mut().cast::<c_void>();

        let format = AudioStreamBasicDescription {
            sample_rate: f64::from(CAPTURE_SAMPLE_RATE),
            format_id: K_AUDIO_FORMAT_LINEAR_PCM,
            format_flags: K_AUDIO_FORMAT_FLAG_IS_FLOAT | K_AUDIO_FORMAT_FLAG_IS_PACKED,
            bytes_per_packet: 4,
            frames_per_packet: 1,
            bytes_per_frame: 4,
            channels_per_frame: 1,
            bits_per_channel: 32,
            reserved: 0,
        };

        let mut queue: AudioQueueRef = null_mut();
        let status = unsafe {
            AudioQueueNewInput(
                &format,
                audio_queue_input_callback,
                user_data,
                null(),
                null(),
                0,
                &mut queue,
            )
        };
        let Some(queue_nn) = NonNull::new(queue) else {
            unsafe { drop(Arc::from_raw(user_data.cast::<CaptureInner>())) };
            return Err(CaptureStartError::Failed(format!(
                "Could not start voice recording (OSStatus {status})."
            )));
        };
        if status != 0 {
            unsafe {
                AudioQueueDispose(queue_nn.as_ptr(), 1);
                drop(Arc::from_raw(user_data.cast::<CaptureInner>()));
            }
            return Err(CaptureStartError::Failed(format!(
                "Could not start voice recording (OSStatus {status})."
            )));
        }

        if !device_id.is_empty() && device_id != "default"
            && let Ok(c_uid) = std::ffi::CString::new(device_id)
        {
            let cf_uid = unsafe {
                CFStringCreateWithCString(null(), c_uid.as_ptr(), K_CF_STRING_ENCODING_UTF8)
            };
            if !cf_uid.is_null() {
                let status = unsafe {
                    AudioQueueSetProperty(
                        queue_nn.as_ptr(),
                        K_AUDIO_QUEUE_PROPERTY_CURRENT_DEVICE,
                        (&raw const cf_uid).cast::<c_void>(),
                        std::mem::size_of::<CFStringRef>() as u32,
                    )
                };
                unsafe { CFRelease(cf_uid) };
                if status != 0 {
                    tracing::debug!(
                        device_id,
                        status,
                        "preferred input device unavailable; using system default"
                    );
                }
            }
        }

        let _ = unsafe {
            AudioQueueAddPropertyListener(
                queue_nn.as_ptr(),
                K_AUDIO_QUEUE_PROPERTY_IS_RUNNING,
                audio_queue_property_listener,
                user_data,
            )
        };

        let buffer_bytes = BUFFER_FRAMES * 4;
        for _ in 0..BUFFER_COUNT {
            let mut buffer: *mut AudioQueueBuffer = null_mut();
            let alloc_status = unsafe {
                AudioQueueAllocateBuffer(queue_nn.as_ptr(), buffer_bytes, &mut buffer)
            };
            if alloc_status != 0 || buffer.is_null() {
                unsafe {
                    let _ = AudioQueueRemovePropertyListener(
                        queue_nn.as_ptr(),
                        K_AUDIO_QUEUE_PROPERTY_IS_RUNNING,
                        audio_queue_property_listener,
                        user_data,
                    );
                    AudioQueueDispose(queue_nn.as_ptr(), 1);
                    drop(Arc::from_raw(user_data.cast::<CaptureInner>()));
                }
                return Err(CaptureStartError::Failed(format!(
                    "Could not start voice recording (OSStatus {alloc_status})."
                )));
            }
            let enqueue_status =
                unsafe { AudioQueueEnqueueBuffer(queue_nn.as_ptr(), buffer, 0, null()) };
            if enqueue_status != 0 {
                unsafe {
                    let _ = AudioQueueRemovePropertyListener(
                        queue_nn.as_ptr(),
                        K_AUDIO_QUEUE_PROPERTY_IS_RUNNING,
                        audio_queue_property_listener,
                        user_data,
                    );
                    AudioQueueDispose(queue_nn.as_ptr(), 1);
                    drop(Arc::from_raw(user_data.cast::<CaptureInner>()));
                }
                return Err(CaptureStartError::Failed(format!(
                    "Could not start voice recording (OSStatus {enqueue_status})."
                )));
            }
        }

        let start_status = unsafe { AudioQueueStart(queue_nn.as_ptr(), null()) };
        if start_status != 0 {
            unsafe {
                let _ = AudioQueueRemovePropertyListener(
                    queue_nn.as_ptr(),
                    K_AUDIO_QUEUE_PROPERTY_IS_RUNNING,
                    audio_queue_property_listener,
                    user_data,
                );
                AudioQueueDispose(queue_nn.as_ptr(), 1);
                drop(Arc::from_raw(user_data.cast::<CaptureInner>()));
            }
            return Err(CaptureStartError::Failed(format!(
                "Could not start voice recording (OSStatus {start_status})."
            )));
        }

        Ok(Self { queue: queue_nn, inner, user_data, stopped: false })
    }

    fn teardown(&mut self, flush: bool) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.inner.stopping.store(true, Ordering::SeqCst);

        unsafe {
            let _ = AudioQueueRemovePropertyListener(
                self.queue.as_ptr(),
                K_AUDIO_QUEUE_PROPERTY_IS_RUNNING,
                audio_queue_property_listener,
                self.user_data,
            );
            let _ = AudioQueueStop(self.queue.as_ptr(), 1);
            let _ = AudioQueueDispose(self.queue.as_ptr(), 1);
        }

        if flush {
            let events = self.inner.processor.lock().expect("pcm processor lock").stop();
            self.inner.dispatch_events(events);
        }

        // SAFETY: `AudioQueueDispose(..., 1)` destroys the queue synchronously and waits for any
        // active callback to finish before returning, so no callback can dereference `user_data`.
        unsafe { drop(Arc::from_raw(self.user_data.cast::<CaptureInner>())) };
    }
}

impl AudioCaptureSession for MacOsAudioCaptureSession {
    fn stop(&mut self) -> Result<(), String> {
        self.teardown(true);
        Ok(())
    }
}

impl Drop for MacOsAudioCaptureSession {
    fn drop(&mut self) {
        self.teardown(false);
    }
}

unsafe extern "C" fn audio_queue_input_callback(
    in_user_data: *mut c_void,
    in_aq: AudioQueueRef,
    in_buffer: *mut AudioQueueBuffer,
    _in_start_time: *const c_void,
    _in_number_packet_descriptions: u32,
    _in_packet_descs: *const c_void,
) {
    if in_user_data.is_null() || in_buffer.is_null() {
        return;
    }
    let inner = unsafe { &*in_user_data.cast::<CaptureInner>() };
    if inner.stopping.load(Ordering::SeqCst) {
        return;
    }

    let buffer = unsafe { &*in_buffer };
    let byte_len = buffer.audio_data_byte_size as usize;
    if !buffer.audio_data.is_null() && byte_len >= 4 {
        let frame_count = byte_len / std::mem::size_of::<f32>();
        let samples = unsafe {
            std::slice::from_raw_parts(buffer.audio_data.cast::<f32>(), frame_count)
        };
        let events = inner
            .processor
            .lock()
            .expect("pcm processor lock")
            .process(&[samples]);
        inner.dispatch_events(events);
    }

    if !inner.stopping.load(Ordering::SeqCst) {
        let _ = unsafe { AudioQueueEnqueueBuffer(in_aq, in_buffer, 0, null()) };
    }
}

unsafe extern "C" fn audio_queue_property_listener(
    in_user_data: *mut c_void,
    in_aq: AudioQueueRef,
    in_id: AudioQueuePropertyID,
) {
    if in_user_data.is_null() || in_id != K_AUDIO_QUEUE_PROPERTY_IS_RUNNING {
        return;
    }
    let inner = unsafe { &*in_user_data.cast::<CaptureInner>() };
    if inner.stopping.load(Ordering::SeqCst) {
        return;
    }
    let mut is_running: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioQueueGetProperty(
            in_aq,
            K_AUDIO_QUEUE_PROPERTY_IS_RUNNING,
            (&raw mut is_running).cast::<c_void>(),
            &mut size,
        )
    };
    if status == 0 && is_running == 0 && !inner.error_reported.swap(true, Ordering::SeqCst) {
        (inner.callbacks.on_error)("The microphone disconnected.".to_owned());
    }
}
