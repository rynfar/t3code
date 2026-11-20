//! macOS VideoToolbox hardware H.264 decoder wrapped in safe RAII handles.
//!
//! Uses `VideoToolbox.framework`, `CoreMedia.framework`, `CoreVideo.framework`, and
//! `CoreFoundation.framework` directly via FFI (zero external crates or binaries).
//! On non-macOS targets, exposes a clean stub returning an unsupported error.

use std::sync::Arc;

use t3_core::device::stream::{AvccConfig, parse_avcc_config};

/// One decoded video frame in tight RGBA byte order (`width * height * 4` bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedRgba {
    pub width: u32,
    pub height: u32,
    pub bytes: Arc<[u8]>,
}

/// Safe RAII H.264 decoder backed by macOS VideoToolbox.
#[cfg(target_os = "macos")]
pub struct H264Decoder {
    session: *mut std::ffi::c_void,
    format_desc: *mut std::ffi::c_void,
    /// Stable heap-allocated callback target; lives until AFTER `VTDecompressionSessionInvalidate`
    /// and `CFRelease` run in `Drop`.
    sink: Box<macos::DecodeSink>,
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
    nal_length_size: u8,
}

#[cfg(target_os = "macos")]
// SAFETY: `VTDecompressionSessionRef` and `CMFormatDescriptionRef` are exclusively owned by this
// decoder and only accessed through `&mut self`; moving ownership between threads is sound.
unsafe impl Send for H264Decoder {}

#[cfg(target_os = "macos")]
impl H264Decoder {
    pub fn new() -> Self {
        Self {
            session: std::ptr::null_mut(),
            format_desc: std::ptr::null_mut(),
            sink: Box::new(macos::DecodeSink::default()),
            sps: Vec::new(),
            pps: Vec::new(),
            nal_length_size: 4,
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.session.is_null() && !self.format_desc.is_null()
    }

    /// Configures (or rebuilds on parameter-set change) the VideoToolbox session from a raw
    /// `avcC` (`AVCDecoderConfigurationRecord`) description payload.
    pub fn configure_avcc(&mut self, avcc: &[u8]) -> Result<(), String> {
        let config = parse_avcc_config(avcc).ok_or_else(|| "Invalid avcC H.264 description.".to_owned())?;
        self.configure(&config)
    }

    /// Configures (or rebuilds if SPS/PPS or NAL length size changed) the VideoToolbox session
    /// from parsed H.264 parameter sets.
    pub fn configure(&mut self, config: &AvccConfig) -> Result<(), String> {
        if config.sps.is_empty() || config.pps.is_empty() {
            return Err("H.264 configuration requires both SPS and PPS.".to_owned());
        }
        if self.is_configured()
            && self.nal_length_size == config.nal_length_size
            && self.sps == config.sps
            && self.pps == config.pps
        {
            return Ok(());
        }
        self.teardown();
        let (format_desc, session) = macos::create_session(&config.sps, &config.pps, config.nal_length_size, &mut self.sink)?;
        self.format_desc = format_desc;
        self.session = session;
        self.sps = config.sps.clone();
        self.pps = config.pps.clone();
        self.nal_length_size = config.nal_length_size;
        Ok(())
    }

    /// Decodes one length-prefixed AVCC access unit synchronously into tight RGBA pixels.
    pub fn decode_avcc(&mut self, avcc_access_unit: &[u8]) -> Result<Option<DecodedRgba>, String> {
        if avcc_access_unit.is_empty() {
            return Ok(None);
        }
        if !self.is_configured() {
            return Err("H.264 decoder is not configured.".to_owned());
        }
        macos::decode_frame(self.session, self.format_desc, avcc_access_unit, &mut self.sink)
    }

    /// Tears down the active decompression session and format description cleanly.
    pub fn teardown(&mut self) {
        macos::destroy_session(&mut self.session, &mut self.format_desc);
        self.sps.clear();
        self.pps.clear();
        self.sink.frame = None;
        self.sink.error = None;
    }
}

#[cfg(target_os = "macos")]
impl Default for H264Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "macos")]
impl Drop for H264Decoder {
    fn drop(&mut self) {
        self.teardown();
    }
}

#[cfg(not(target_os = "macos"))]
#[derive(Default)]
pub struct H264Decoder;

#[cfg(not(target_os = "macos"))]
impl H264Decoder {
    pub fn new() -> Self {
        Self
    }

    pub fn is_configured(&self) -> bool {
        false
    }

    pub fn configure_avcc(&mut self, _avcc: &[u8]) -> Result<(), String> {
        Err("H.264 hardware decoding is unavailable on this platform.".to_owned())
    }

    pub fn configure(&mut self, _config: &AvccConfig) -> Result<(), String> {
        Err("H.264 hardware decoding is unavailable on this platform.".to_owned())
    }

    pub fn decode_avcc(&mut self, _avcc_access_unit: &[u8]) -> Result<Option<DecodedRgba>, String> {
        Err("H.264 hardware decoding is unavailable on this platform.".to_owned())
    }

    pub fn teardown(&mut self) {}
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::marker::{PhantomData, PhantomPinned};
    use std::ptr::{null, null_mut};
    use std::sync::Arc;

    use super::DecodedRgba;

    const K_CF_NUMBER_SINT32_TYPE: isize = 3;
    const K_CV_PIXEL_FORMAT_TYPE_32_BGRA: i32 = u32::from_be_bytes(*b"BGRA") as i32;
    const K_CV_PIXEL_BUFFER_LOCK_READ_ONLY: u64 = 1;
    const K_CM_BLOCK_BUFFER_ASSURE_MEMORY_NOW_FLAG: u32 = 1;

    #[repr(C)]
    struct OpaqueStatic {
        _data: [u8; 0],
        _marker: PhantomData<(*mut u8, PhantomPinned)>,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CMTime {
        value: i64,
        timescale: i32,
        flags: u32,
        epoch: i64,
    }

    type VTDecompressionOutputCallback = unsafe extern "C" fn(
        decompression_output_ref_con: *mut c_void,
        source_frame_ref_con: *mut c_void,
        status: i32,
        info_flags: u32,
        image_buffer: *mut c_void,
        presentation_time_stamp: CMTime,
        presentation_duration: CMTime,
    );

    #[repr(C)]
    struct VTDecompressionOutputCallbackRecord {
        decompression_output_callback: Option<VTDecompressionOutputCallback>,
        decompression_output_ref_con: *mut c_void,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
        fn CFNumberCreate(allocator: *const c_void, the_type: isize, value_ptr: *const c_void) -> *const c_void;
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        static kCFTypeDictionaryKeyCallBacks: OpaqueStatic;
        static kCFTypeDictionaryValueCallBacks: OpaqueStatic;
    }

    #[link(name = "CoreMedia", kind = "framework")]
    unsafe extern "C" {
        fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
            allocator: *const c_void,
            parameter_set_count: usize,
            parameter_set_pointers: *const *const u8,
            parameter_set_sizes: *const usize,
            nal_unit_header_length: i32,
            format_description_out: *mut *mut c_void,
        ) -> i32;
        fn CMBlockBufferCreateWithMemoryBlock(
            structure_allocator: *const c_void,
            memory_block: *mut c_void,
            block_length: usize,
            block_allocator: *const c_void,
            custom_block_source: *const c_void,
            offset_to_data: usize,
            data_length: usize,
            flags: u32,
            block_buffer_out: *mut *mut c_void,
        ) -> i32;
        fn CMBlockBufferReplaceDataBytes(
            source_bytes: *const c_void,
            destination_buffer: *mut c_void,
            offset_into_destination: usize,
            data_length: usize,
        ) -> i32;
        fn CMSampleBufferCreateReady(
            allocator: *const c_void,
            data_buffer: *mut c_void,
            format_description: *mut c_void,
            num_samples: isize,
            num_sample_timing_entries: isize,
            sample_timing_array: *const c_void,
            num_sample_size_entries: isize,
            sample_size_array: *const usize,
            sample_buffer_out: *mut *mut c_void,
        ) -> i32;
    }

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        static kCVPixelBufferPixelFormatTypeKey: *const c_void;
        fn CVPixelBufferLockBaseAddress(pixel_buffer: *mut c_void, lock_flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(pixel_buffer: *mut c_void, unlock_flags: u64) -> i32;
        fn CVPixelBufferGetWidth(pixel_buffer: *mut c_void) -> usize;
        fn CVPixelBufferGetHeight(pixel_buffer: *mut c_void) -> usize;
        fn CVPixelBufferGetBytesPerRow(pixel_buffer: *mut c_void) -> usize;
        fn CVPixelBufferGetBaseAddress(pixel_buffer: *mut c_void) -> *const u8;
    }

    #[link(name = "VideoToolbox", kind = "framework")]
    unsafe extern "C" {
        fn VTDecompressionSessionCreate(
            allocator: *const c_void,
            video_format_description: *mut c_void,
            video_decoder_specification: *const c_void,
            destination_image_buffer_attributes: *const c_void,
            output_callback: *const VTDecompressionOutputCallbackRecord,
            decompression_session_out: *mut *mut c_void,
        ) -> i32;
        fn VTDecompressionSessionInvalidate(session: *mut c_void);
        fn VTDecompressionSessionDecodeFrame(
            session: *mut c_void,
            sample_buffer: *mut c_void,
            decode_flags: u32,
            source_frame_ref_con: *mut c_void,
            info_flags_out: *mut u32,
        ) -> i32;
    }

    /// RAII guard for owned CoreFoundation references.
    struct CfGuard(*const c_void);

    impl CfGuard {
        fn into_raw(self) -> *mut c_void {
            let ptr = self.0 as *mut c_void;
            std::mem::forget(self);
            ptr
        }
    }

    impl Drop for CfGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` is an owned non-null CFTypeRef.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    /// RAII guard that unlocks a locked `CVPixelBuffer` on drop.
    struct PixelBufferLock(*mut c_void);

    impl Drop for PixelBufferLock {
        fn drop(&mut self) {
            // SAFETY: Constructed only after a successful `CVPixelBufferLockBaseAddress` with the
            // matching read-only lock flag.
            unsafe {
                CVPixelBufferUnlockBaseAddress(self.0, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY);
            }
        }
    }

    #[derive(Default)]
    pub(super) struct DecodeSink {
        pub frame: Option<DecodedRgba>,
        pub error: Option<i32>,
    }

    unsafe extern "C" fn output_callback(
        decompression_output_ref_con: *mut c_void,
        _source_frame_ref_con: *mut c_void,
        status: i32,
        _info_flags: u32,
        image_buffer: *mut c_void,
        _presentation_time_stamp: CMTime,
        _presentation_duration: CMTime,
    ) {
        if decompression_output_ref_con.is_null() {
            return;
        }
        // SAFETY: `decompression_output_ref_con` points to the decoder's boxed `DecodeSink`, which
        // stays alive until after `VTDecompressionSessionInvalidate`, and decoding is synchronous.
        let sink = unsafe { &mut *(decompression_output_ref_con as *mut DecodeSink) };
        if status != 0 {
            sink.error = Some(status);
            return;
        }
        if image_buffer.is_null() {
            return;
        }
        // SAFETY: `image_buffer` is a valid `CVPixelBufferRef` for the duration of this callback.
        let lock_status = unsafe { CVPixelBufferLockBaseAddress(image_buffer, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY) };
        if lock_status != 0 {
            sink.error = Some(lock_status);
            return;
        }
        let _lock = PixelBufferLock(image_buffer);
        // SAFETY: Base address is locked read-only; dimensions and stride come from CoreVideo.
        let (width, height, bytes_per_row, base) = unsafe {
            (
                CVPixelBufferGetWidth(image_buffer),
                CVPixelBufferGetHeight(image_buffer),
                CVPixelBufferGetBytesPerRow(image_buffer),
                CVPixelBufferGetBaseAddress(image_buffer),
            )
        };
        if base.is_null() || width == 0 || height == 0 || bytes_per_row < width.saturating_mul(4) {
            return;
        }
        let Some(total_src) = bytes_per_row.checked_mul(height) else { return };
        let Some(total_dst) = width.checked_mul(height).and_then(|n| n.checked_mul(4)) else { return };
        // SAFETY: Locked CVPixelBuffer exposes `bytes_per_row * height` readable bytes from `base`.
        let src = unsafe { std::slice::from_raw_parts(base, total_src) };
        let mut rgba = vec![0u8; total_dst];
        let row_bytes = width * 4;
        for y in 0..height {
            let src_row = &src[y * bytes_per_row..y * bytes_per_row + row_bytes];
            let dst_row = &mut rgba[y * row_bytes..(y + 1) * row_bytes];
            for (src_px, dst_px) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
                // Convert kCVPixelFormatType_32BGRA -> tight RGBA.
                dst_px[0] = src_px[2];
                dst_px[1] = src_px[1];
                dst_px[2] = src_px[0];
                dst_px[3] = 0xff;
            }
        }
        if let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) {
            sink.frame = Some(DecodedRgba { width: w, height: h, bytes: Arc::from(rgba) });
        }
    }

    pub(super) fn create_session(
        sps: &[Vec<u8>],
        pps: &[Vec<u8>],
        nal_length_size: u8,
        sink: &mut Box<DecodeSink>,
    ) -> Result<(*mut c_void, *mut c_void), String> {
        let mut ptrs: Vec<*const u8> = Vec::with_capacity(sps.len() + pps.len());
        let mut sizes: Vec<usize> = Vec::with_capacity(sps.len() + pps.len());
        for set in sps.iter().chain(pps.iter()) {
            ptrs.push(set.as_ptr());
            sizes.push(set.len());
        }
        let mut format_desc: *mut c_void = null_mut();
        // SAFETY: `ptrs` and `sizes` have equal length >= 2 and point to valid parameter set bytes.
        let status = unsafe {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                null(),
                ptrs.len(),
                ptrs.as_ptr(),
                sizes.as_ptr(),
                i32::from(nal_length_size),
                &mut format_desc,
            )
        };
        if status != 0 || format_desc.is_null() {
            return Err(format!("CMVideoFormatDescriptionCreateFromH264ParameterSets failed ({status})."));
        }
        let format_guard = CfGuard(format_desc);

        let pixel_format = K_CV_PIXEL_FORMAT_TYPE_32_BGRA;
        // SAFETY: `CFNumberCreate` reads a valid `i32` for `kCFNumberSInt32Type`.
        let format_num = unsafe { CFNumberCreate(null(), K_CF_NUMBER_SINT32_TYPE, (&raw const pixel_format).cast()) };
        if format_num.is_null() {
            return Err("CFNumberCreate failed for BGRA pixel format.".to_owned());
        }
        let format_num_guard = CfGuard(format_num);

        // SAFETY: `kCVPixelBufferPixelFormatTypeKey` and CF dictionary callbacks are framework globals.
        let attrs = unsafe {
            let keys = [kCVPixelBufferPixelFormatTypeKey];
            let values = [format_num];
            CFDictionaryCreate(
                null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
                (&raw const kCFTypeDictionaryValueCallBacks).cast(),
            )
        };
        if attrs.is_null() {
            return Err("CFDictionaryCreate failed for destination image buffer attributes.".to_owned());
        }
        let attrs_guard = CfGuard(attrs);

        let callback = VTDecompressionOutputCallbackRecord {
            decompression_output_callback: Some(output_callback),
            decompression_output_ref_con: (&raw mut **sink).cast(),
        };
        let mut session: *mut c_void = null_mut();
        // SAFETY: All CF handles and callback record are valid; `session` receives an owned ref.
        let status = unsafe { VTDecompressionSessionCreate(null(), format_desc, null(), attrs, &callback, &mut session) };
        drop(attrs_guard);
        drop(format_num_guard);
        if status != 0 || session.is_null() {
            return Err(format!("VTDecompressionSessionCreate failed ({status})."));
        }
        Ok((format_guard.into_raw(), session))
    }

    pub(super) fn decode_frame(
        session: *mut c_void,
        format_desc: *mut c_void,
        avcc_access_unit: &[u8],
        sink: &mut Box<DecodeSink>,
    ) -> Result<Option<DecodedRgba>, String> {
        sink.frame = None;
        sink.error = None;
        let len = avcc_access_unit.len();
        let mut block_buf: *mut c_void = null_mut();
        // SAFETY: CoreMedia allocates and owns the block buffer memory (`memory_block = null_mut()`).
        let status = unsafe {
            CMBlockBufferCreateWithMemoryBlock(
                null(),
                null_mut(),
                len,
                null(),
                null(),
                0,
                len,
                K_CM_BLOCK_BUFFER_ASSURE_MEMORY_NOW_FLAG,
                &mut block_buf,
            )
        };
        if status != 0 || block_buf.is_null() {
            return Err(format!("CMBlockBufferCreateWithMemoryBlock failed ({status})."));
        }
        let block_guard = CfGuard(block_buf);

        // SAFETY: Copies `len` bytes from `avcc_access_unit` into the freshly allocated `block_buf`.
        let status = unsafe { CMBlockBufferReplaceDataBytes(avcc_access_unit.as_ptr().cast(), block_buf, 0, len) };
        if status != 0 {
            return Err(format!("CMBlockBufferReplaceDataBytes failed ({status})."));
        }

        let mut sample_buf: *mut c_void = null_mut();
        let sample_size = len;
        // SAFETY: `block_buf` and `format_desc` are valid owned CoreMedia objects.
        let status = unsafe { CMSampleBufferCreateReady(null(), block_buf, format_desc, 1, 0, null(), 1, &sample_size, &mut sample_buf) };
        if status != 0 || sample_buf.is_null() {
            return Err(format!("CMSampleBufferCreateReady failed ({status})."));
        }
        let sample_guard = CfGuard(sample_buf);

        let mut info_flags: u32 = 0;
        // SAFETY: `decode_flags = 0` runs synchronously, invoking `output_callback` before return.
        let status = unsafe { VTDecompressionSessionDecodeFrame(session, sample_buf, 0, null_mut(), &mut info_flags) };
        drop(sample_guard);
        drop(block_guard);
        if status != 0 {
            return Err(format!("VTDecompressionSessionDecodeFrame failed ({status})."));
        }
        if let Some(err) = sink.error.take() {
            return Err(format!("VideoToolbox output callback failed ({err})."));
        }
        Ok(sink.frame.take())
    }

    pub(super) fn destroy_session(session: &mut *mut c_void, format_desc: &mut *mut c_void) {
        if !session.is_null() {
            // SAFETY: Valid owned `VTDecompressionSessionRef`; invalidate before release.
            unsafe {
                VTDecompressionSessionInvalidate(*session);
                CFRelease(*session);
            }
            *session = null_mut();
        }
        if !format_desc.is_null() {
            // SAFETY: Valid owned `CMFormatDescriptionRef`.
            unsafe { CFRelease(*format_desc) };
            *format_desc = null_mut();
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use t3_core::device::stream::{annex_b_to_avcc, build_avcc_config};

    /// Minimal 32x16 solid-red H.264 Baseline Annex-B bitstream (SPS + PPS + IDR).
    const RED_32X16_ANNEX_B: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x0a, 0xd9, 0x0b, 0xb0, 0x11, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x32, 0x0f, 0x12, 0x26, 0x48,
        0x00, 0x00, 0x00, 0x01, 0x68, 0xcb, 0x83, 0xcb, 0x20,
        0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x0a, 0xf1, 0x18, 0xa0, 0x00, 0x24, 0x03, 0x1c, 0x00, 0x04, 0x3a, 0xe3, 0x80, 0x00, 0x97, 0xc4, 0x9e,
    ];

    /// Minimal 64x32 solid-blue H.264 Baseline Annex-B bitstream (SPS + PPS + IDR) for session rebuild testing.
    const BLUE_64X32_ANNEX_B: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x0a, 0xd9, 0x04, 0x5b, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0x20, 0xf1, 0x22, 0x64, 0x80,
        0x00, 0x00, 0x00, 0x01, 0x68, 0xcb, 0x83, 0xcb, 0x20,
        0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x0a, 0xf1, 0x18, 0xa0, 0x00, 0x27, 0xa7, 0x1c, 0x00, 0x04, 0xbe, 0x23, 0x80, 0x00, 0x82, 0xec, 0x9c, 0x9c, 0x9d, 0x75, 0xd7, 0x5e,
    ];

    #[test]
    fn videotoolbox_decodes_avcc_idr_and_rebuilds_cleanly_on_sps_change() {
        let mut decoder = H264Decoder::new();
        assert!(!decoder.is_configured());

        // 1. Decode 32x16 red frame via avcC description.
        let red_au = annex_b_to_avcc(RED_32X16_ANNEX_B);
        assert!(red_au.is_key);
        let red_avcc = build_avcc_config(&red_au.sps[0], &red_au.pps[0]).unwrap();
        decoder.configure_avcc(&red_avcc).expect("configure 32x16");
        assert!(decoder.is_configured());
        let frame = decoder.decode_avcc(&red_au.avcc_data).expect("decode 32x16").expect("produces RGBA frame");
        assert_eq!((frame.width, frame.height), (32, 16));
        assert_eq!(frame.bytes.len(), 32 * 16 * 4);
        // Center pixel must be strongly red in RGBA order (verifies BGRA->RGBA swap).
        let center = (8 * 32 + 16) * 4;
        assert!(frame.bytes[center] > 200, "R channel should be high, got {:?}", &frame.bytes[center..center + 4]);
        assert!(frame.bytes[center + 1] < 50, "G channel should be low");
        assert!(frame.bytes[center + 2] < 50, "B channel should be low");
        assert_eq!(frame.bytes[center + 3], 255, "A channel should be opaque");

        // 2. Rebuild session cleanly when SPS/PPS change to 64x32 blue frame.
        let blue_au = annex_b_to_avcc(BLUE_64X32_ANNEX_B);
        let blue_avcc = build_avcc_config(&blue_au.sps[0], &blue_au.pps[0]).unwrap();
        decoder.configure_avcc(&blue_avcc).expect("reconfigure 64x32");
        let frame2 = decoder.decode_avcc(&blue_au.avcc_data).expect("decode 64x32").expect("produces RGBA frame");
        assert_eq!((frame2.width, frame2.height), (64, 32));
        assert_eq!(frame2.bytes.len(), 64 * 32 * 4);
        let center2 = (16 * 64 + 32) * 4;
        assert!(frame2.bytes[center2] < 50, "R channel should be low");
        assert!(frame2.bytes[center2 + 2] > 200, "B channel should be high, got {:?}", &frame2.bytes[center2..center2 + 4]);
        assert_eq!(frame2.bytes[center2 + 3], 255);

        // 3. Teardown & drop are clean and idempotent.
        decoder.teardown();
        assert!(!decoder.is_configured());
        drop(decoder);
    }
}
