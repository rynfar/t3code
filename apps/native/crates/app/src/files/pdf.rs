//! Native PDF file preview (`PdfViewer.tsx`): bounded asset download, macOS CoreGraphics +
//! PDFKit page rasterization and progressive Find (`pingdotgg/t3code#33956`), zoom / fit-width,
//! Prev/Next page navigation, saved position LRU, and cross-platform pure-Rust fallback.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Corners, Entity, IntoElement, ParentElement,
    RenderImage, ScrollHandle, SharedString, Styled, Subscription, Task, Window, canvas, div,
    point, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{Sizable, h_flex, v_flex};
use t3_core::files::pdf::{
    PDF_MAX_BYTES, PDF_MAX_PAGES, PDF_MAX_ZOOM, PDF_MIN_ZOOM, PDF_PAGE_GAP, PdfFindState,
    PdfFindStep, PdfLinkTarget, PdfPageLink, PdfPositionStore, PdfViewState, SavedPdfPosition,
    current_pdf_page, is_safe_pdf_external_url, parse_basic_pdf, pdf_page_offsets,
    pdf_page_scroll_top, pdf_page_width, pdf_rect_to_normalized_box, pdf_restored_scroll_top,
    step_pdf_zoom, visible_pdf_pages,
};
use t3_core::files::{absolute_path, workspace_asset_request};

use super::Root;
use super::preview::{AssetFetchOutcome, KnownAsset, Preview, load_workspace_asset};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

const PAGE_RASTER_CACHE_LIMIT: usize = 8;
const FIND_DEBOUNCE: Duration = Duration::from_millis(250);

#[cfg(target_os = "macos")]
mod native_pdf {
    use std::ffi::c_void;
    use std::sync::Arc;

    use gpui::RenderImage;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{msg_send, sel};
    use objc2_foundation::{NSData, NSRect, NSString, NSURL};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGAffineTransform {
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        tx: f64,
        ty: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDataProviderCreateWithData(
            info: *mut c_void,
            data: *const c_void,
            size: usize,
            release_data: Option<unsafe extern "C" fn(*mut c_void, *const c_void, usize)>,
        ) -> *mut c_void;
        fn CGDataProviderRelease(provider: *mut c_void);
        fn CGPDFDocumentCreateWithProvider(provider: *mut c_void) -> *mut c_void;
        fn CGPDFDocumentRelease(document: *mut c_void);
        fn CGPDFDocumentGetNumberOfPages(document: *mut c_void) -> usize;
        fn CGPDFDocumentGetPage(document: *mut c_void, page_number: usize) -> *mut c_void;
        fn CGPDFPageGetBoxRect(page: *mut c_void, box_type: i32) -> CGRect;
        fn CGPDFPageGetRotationAngle(page: *mut c_void) -> i32;
        fn CGPDFPageGetDrawingTransform(
            page: *mut c_void,
            box_type: i32,
            rect: CGRect,
            rotate: i32,
            preserve_aspect_ratio: bool,
        ) -> CGAffineTransform;
        fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
        fn CGColorSpaceRelease(space: *mut c_void);
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: *mut c_void,
            bitmap_info: u32,
        ) -> *mut c_void;
        fn CGContextRelease(ctx: *mut c_void);
        fn CGContextSetRGBFillColor(ctx: *mut c_void, r: f64, g: f64, b: f64, a: f64);
        fn CGContextFillRect(ctx: *mut c_void, rect: CGRect);
        fn CGContextConcatCTM(ctx: *mut c_void, transform: CGAffineTransform);
        fn CGContextDrawPDFPage(ctx: *mut c_void, page: *mut c_void);
    }

    #[link(name = "PDFKit", kind = "framework")]
    unsafe extern "C" {}

    const K_CGPDF_CROP_BOX: i32 = 1;
    // kCGBitmapByteOrder32Little (2 << 12) | kCGImageAlphaPremultipliedFirst (2) -> BGRA.
    const BITMAP_INFO_BGRA: u32 = (2 << 12) | 2;

    unsafe extern "C" fn release_arc_bytes(info: *mut c_void, _data: *const c_void, _size: usize) {
        if !info.is_null() {
            let _ = unsafe { Arc::from_raw(info as *const Vec<u8>) };
        }
    }

    struct CgPdfDoc {
        raw: *mut c_void,
    }

    impl Drop for CgPdfDoc {
        fn drop(&mut self) {
            if !self.raw.is_null() {
                unsafe { CGPDFDocumentRelease(self.raw) };
            }
        }
    }

    impl CgPdfDoc {
        fn open(bytes: &Arc<Vec<u8>>) -> Option<Self> {
            let ptr = bytes.as_ptr() as *const c_void;
            let len = bytes.len();
            let cloned = Arc::clone(bytes);
            let info = Arc::into_raw(cloned) as *mut c_void;
            let provider = unsafe { CGDataProviderCreateWithData(info, ptr, len, Some(release_arc_bytes)) };
            if provider.is_null() {
                let _ = unsafe { Arc::from_raw(info as *const Vec<u8>) };
                return None;
            }
            let raw = unsafe { CGPDFDocumentCreateWithProvider(provider) };
            unsafe { CGDataProviderRelease(provider) };
            if raw.is_null() {
                return None;
            }
            Some(Self { raw })
        }

        fn page_count(&self) -> usize {
            unsafe { CGPDFDocumentGetNumberOfPages(self.raw) }
        }

        fn page_dimensions(&self, page_index: usize) -> Option<(f32, f32)> {
            let page = unsafe { CGPDFDocumentGetPage(self.raw, page_index + 1) };
            if page.is_null() {
                return None;
            }
            let rect = unsafe { CGPDFPageGetBoxRect(page, K_CGPDF_CROP_BOX) };
            let rotate = unsafe { CGPDFPageGetRotationAngle(page) };
            let (mut w, mut h) = (rect.size.width.abs() as f32, rect.size.height.abs() as f32);
            if rotate % 180 != 0 {
                std::mem::swap(&mut w, &mut h);
            }
            if w > 0.0 && h > 0.0 { Some((w, h)) } else { Some((612.0, 792.0)) }
        }

        fn render_page(&self, page_index: usize, target_width: f32) -> Option<Arc<RenderImage>> {
            let page = unsafe { CGPDFDocumentGetPage(self.raw, page_index + 1) };
            if page.is_null() {
                return None;
            }
            let (natural_w, natural_h) = self.page_dimensions(page_index)?;
            let ratio = natural_h / natural_w;
            let css_w = target_width.max(32.0);
            let css_h = (css_w * ratio).max(32.0);
            // Cap backing bitmap to 8M pixels, matching PdfViewer.tsx.
            let max_scale = ((8.0 * 1024.0 * 1024.0) / (css_w * css_h)).sqrt().clamp(0.5, 2.0);
            let px_w = (css_w * max_scale).ceil().max(1.0) as usize;
            let px_h = (css_h * max_scale).ceil().max(1.0) as usize;
            let bytes_per_row = px_w * 4;
            let mut buffer = vec![0u8; bytes_per_row * px_h];
            let color_space = unsafe { CGColorSpaceCreateDeviceRGB() };
            if color_space.is_null() {
                return None;
            }
            let ctx = unsafe {
                CGBitmapContextCreate(
                    buffer.as_mut_ptr() as *mut c_void,
                    px_w,
                    px_h,
                    8,
                    bytes_per_row,
                    color_space,
                    BITMAP_INFO_BGRA,
                )
            };
            unsafe { CGColorSpaceRelease(color_space) };
            if ctx.is_null() {
                return None;
            }
            let bounds = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize { width: px_w as f64, height: px_h as f64 },
            };
            unsafe {
                CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);
                CGContextFillRect(ctx, bounds);
                let transform = CGPDFPageGetDrawingTransform(page, K_CGPDF_CROP_BOX, bounds, 0, true);
                CGContextConcatCTM(ctx, transform);
                CGContextDrawPDFPage(ctx, page);
                CGContextRelease(ctx);
            }
            let img = image::RgbaImage::from_raw(px_w as u32, px_h as u32, buffer)?;
            Some(Arc::new(RenderImage::new(vec![image::Frame::new(img)])))
        }
    }

    pub fn inspect_pdf(bytes: &Arc<Vec<u8>>) -> Result<(usize, f32, Vec<f32>), String> {
        let doc = CgPdfDoc::open(bytes).ok_or_else(|| "Unable to open PDF.".to_owned())?;
        let count = doc.page_count();
        if count == 0 {
            return Err("Unable to open PDF.".to_owned());
        }
        if count > t3_core::files::pdf::PDF_MAX_PAGES {
            return Err("Preview limited to 2,000 pages. Save this PDF to read it in full.".to_owned());
        }
        let (first_w, first_h) = doc.page_dimensions(0).unwrap_or((612.0, 792.0));
        let first_ratio = first_h / first_w;
        // Measure the first few pages upfront and fill the rest with page 1's ratio, matching PdfViewer.tsx.
        let mut ratios = vec![first_ratio; count];
        for (i, slot) in ratios.iter_mut().enumerate().take(count.min(16)) {
            if let Some((w, h)) = doc.page_dimensions(i) {
                *slot = h / w;
            }
        }
        Ok((count, first_w, ratios))
    }

    pub fn render_page_image(bytes: &Arc<Vec<u8>>, page_index: usize, width: f32) -> Option<Arc<RenderImage>> {
        let doc = CgPdfDoc::open(bytes)?;
        doc.render_page(page_index, width)
    }

    pub fn extract_page_links(
        bytes: &Arc<Vec<u8>>,
        page_index: usize,
    ) -> Vec<super::PdfPageLink> {
        objc2::rc::autoreleasepool(|_| {
            let Some(cls) = AnyClass::get(c"PDFDocument") else {
                return Vec::new();
            };
            let data = NSData::with_bytes(bytes);
            let doc: Option<Retained<AnyObject>> = unsafe {
                let alloc: *mut AnyObject = msg_send![cls, alloc];
                let init: *mut AnyObject = msg_send![alloc, initWithData: &*data];
                Retained::from_raw(init)
            };
            let Some(doc) = doc else {
                return Vec::new();
            };
            let page_count: usize = unsafe { msg_send![&*doc, pageCount] };
            if page_index >= page_count {
                return Vec::new();
            }
            let page: *mut AnyObject = unsafe { msg_send![&*doc, pageAtIndex: page_index] };
            if page.is_null() {
                return Vec::new();
            }
            // kPDFDisplayBoxCropBox = 1
            let crop_box: NSRect = unsafe { msg_send![page, boundsForBox: 1isize] };
            let rotation: isize = unsafe { msg_send![page, rotation] };
            let annots: *mut AnyObject = unsafe { msg_send![page, annotations] };
            if annots.is_null() {
                return Vec::new();
            }
            let count: usize = unsafe { msg_send![annots, count] };
            let crop = [
                crop_box.origin.x as f32,
                crop_box.origin.y as f32,
                crop_box.size.width as f32,
                crop_box.size.height as f32,
            ];
            let mut links = Vec::new();
            for i in 0..count {
                let annot: *mut AnyObject = unsafe { msg_send![annots, objectAtIndex: i] };
                if annot.is_null() {
                    continue;
                }
                let kind: *mut NSString = unsafe { msg_send![annot, type] };
                if kind.is_null() {
                    continue;
                }
                let kind_str = unsafe { &*kind }.to_string();
                if kind_str != "Link" && kind_str != "/Link" {
                    continue;
                }
                let bounds: NSRect = unsafe { msg_send![annot, bounds] };
                let Some(box_norm) = super::pdf_rect_to_normalized_box(
                    [
                        bounds.origin.x as f32,
                        bounds.origin.y as f32,
                        (bounds.origin.x + bounds.size.width) as f32,
                        (bounds.origin.y + bounds.size.height) as f32,
                    ],
                    crop,
                    rotation as i32,
                ) else {
                    continue;
                };
                let mut url_obj: *mut NSURL = unsafe { msg_send![annot, URL] };
                let action: *mut AnyObject = unsafe { msg_send![annot, action] };
                if url_obj.is_null() && !action.is_null() {
                    let responds_url: bool = unsafe { msg_send![action, respondsToSelector: sel!(URL)] };
                    if responds_url {
                        url_obj = unsafe { msg_send![action, URL] };
                    }
                }
                if !url_obj.is_null() {
                    let abs: *mut NSString = unsafe { msg_send![url_obj, absoluteString] };
                    if !abs.is_null() {
                        let url_str = unsafe { &*abs }.to_string();
                        if super::is_safe_pdf_external_url(&url_str) {
                            links.push(super::PdfPageLink {
                                box_norm,
                                target: super::PdfLinkTarget::External(url_str),
                            });
                            continue;
                        }
                    }
                }
                let mut dest: *mut AnyObject = unsafe { msg_send![annot, destination] };
                if dest.is_null() && !action.is_null() {
                    let responds_dest: bool = unsafe { msg_send![action, respondsToSelector: sel!(destination)] };
                    if responds_dest {
                        dest = unsafe { msg_send![action, destination] };
                    }
                }
                if !dest.is_null() {
                    let target_page: *mut AnyObject = unsafe { msg_send![dest, page] };
                    if !target_page.is_null() {
                        let target_idx: usize = unsafe { msg_send![&*doc, indexForPage: target_page] };
                        if target_idx < page_count {
                            links.push(super::PdfPageLink {
                                box_norm,
                                target: super::PdfLinkTarget::Page(target_idx),
                            });
                        }
                    }
                }
            }
            links
        })
    }

    /// Single-parse PDF scan handle reused across every page of a progressive Find pass.
    /// Wraps ObjC allocations (`NSData`, `PDFDocument`, `PDFPage`, `NSString`) in explicit
    /// autorelease pools so background executor threads never accumulate autoreleased objects.
    pub struct PdfScanSession {
        cg_doc: Option<CgPdfDoc>,
        pdfkit_doc: Option<Retained<AnyObject>>,
    }

    impl Drop for PdfScanSession {
        fn drop(&mut self) {
            objc2::rc::autoreleasepool(|_| {
                drop(self.pdfkit_doc.take());
            });
        }
    }

    impl PdfScanSession {
        pub fn open(bytes: &Arc<Vec<u8>>) -> Self {
            let cg_doc = CgPdfDoc::open(bytes);
            let pdfkit_doc = objc2::rc::autoreleasepool(|_| {
                let cls = AnyClass::get(c"PDFDocument")?;
                let data = NSData::with_bytes(bytes);
                unsafe {
                    let alloc: *mut AnyObject = msg_send![cls, alloc];
                    let init: *mut AnyObject = msg_send![alloc, initWithData: &*data];
                    Retained::from_raw(init)
                }
            });
            Self { cg_doc, pdfkit_doc }
        }

        pub fn page_ratio_and_text(&self, page_index: usize) -> (Option<f32>, Option<String>) {
            let ratio = self
                .cg_doc
                .as_ref()
                .and_then(|doc| doc.page_dimensions(page_index))
                .map(|(w, h)| h / w);
            let text = self.pdfkit_doc.as_ref().and_then(|doc| {
                objc2::rc::autoreleasepool(|_| {
                    let page: *mut AnyObject = unsafe { msg_send![&**doc, pageAtIndex: page_index] };
                    if page.is_null() {
                        return None;
                    }
                    let text: *mut NSString = unsafe { msg_send![page, string] };
                    if text.is_null() {
                        return Some(String::new());
                    }
                    let text_ref: &NSString = unsafe { &*text };
                    Some(text_ref.to_string())
                })
            });
            (ratio, text)
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod native_pdf {
    use std::sync::Arc;
    use gpui::RenderImage;

    pub fn inspect_pdf(bytes: &Arc<Vec<u8>>) -> Result<(usize, f32, Vec<f32>), String> {
        let parsed = t3_core::files::pdf::parse_basic_pdf(bytes)?;
        let first_w = parsed.pages.first().map_or(612.0, |p| p.width);
        let ratios = parsed.pages.iter().map(|p| p.height / p.width).collect();
        Ok((parsed.pages.len(), first_w, ratios))
    }

    pub fn render_page_image(_bytes: &Arc<Vec<u8>>, _page_index: usize, _width: f32) -> Option<Arc<RenderImage>> {
        None
    }

    pub fn extract_page_links(_bytes: &Arc<Vec<u8>>, _page_index: usize) -> Vec<super::PdfPageLink> {
        Vec::new()
    }

    pub struct PdfScanSession {
        parsed: Option<t3_core::files::pdf::BasicPdfDocument>,
    }

    impl PdfScanSession {
        pub fn open(bytes: &Arc<Vec<u8>>) -> Self {
            Self { parsed: t3_core::files::pdf::parse_basic_pdf(bytes).ok() }
        }

        pub fn page_ratio_and_text(&self, page_index: usize) -> (Option<f32>, Option<String>) {
            match self.parsed.as_ref().and_then(|doc| doc.pages.get(page_index)) {
                Some(page) => (Some(page.height / page.width), Some(page.text.clone())),
                None => (None, None),
            }
        }
    }
}

pub struct DecodedPdf {
    pub bytes: Arc<Vec<u8>>,
    pub page_count: usize,
    pub page_width: f32,
    pub ratios: Vec<f32>,
    /// Text fallback per page when rasterization is unavailable.
    pub fallback_text: Vec<String>,
}

pub(super) fn load(
    state: &AppState,
    root: &Root,
    path: &str,
    mutation: Option<&str>,
    known: KnownAsset,
) -> Option<tokio::task::JoinHandle<Result<AssetFetchOutcome, String>>> {
    let request = workspace_asset_request(
        &root.workspace.thread_id,
        &root.workspace.cwd,
        &absolute_path(path, &root.workspace.cwd),
    );
    load_workspace_asset(state, request, PDF_MAX_BYTES, known, mutation, "Unable to open PDF.")
}

pub fn decode(bytes: Vec<u8>) -> Result<DecodedPdf, String> {
    if bytes.len() > PDF_MAX_BYTES {
        return Err("Preview limited to files under 50 MB.".to_owned());
    }
    let bytes = Arc::new(bytes);
    let (page_count, page_width, ratios) = match native_pdf::inspect_pdf(&bytes) {
        Ok(info) => info,
        Err(native_err) => match parse_basic_pdf(&bytes) {
            Ok(parsed) => {
                let first_w = parsed.pages.first().map_or(612.0, |p| p.width);
                let ratios = parsed.pages.iter().map(|p| p.height / p.width).collect();
                (parsed.pages.len(), first_w, ratios)
            }
            Err(_) => return Err(native_err),
        },
    };
    if page_count > PDF_MAX_PAGES {
        return Err("Preview limited to 2,000 pages. Save this PDF to read it in full.".to_owned());
    }
    let fallback_text = parse_basic_pdf(&bytes)
        .map(|doc| doc.pages.into_iter().map(|p| p.text).collect())
        .unwrap_or_default();
    Ok(DecodedPdf { bytes, page_count, page_width, ratios, fallback_text })
}

struct CachedPage {
    width_bucket: u32,
    image: Arc<RenderImage>,
}

pub enum PdfState {
    Loading,
    Ready(Box<PdfReady>),
    Failed(String),
}

pub struct PdfReady {
    pub doc_key: String,
    pub bytes: Arc<Vec<u8>>,
    pub page_count: usize,
    pub page_width: f32,
    pub ratios: Vec<f32>,
    pub fallback_text: Vec<String>,
    pub zoom: Option<f32>,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub scroll: ScrollHandle,
    pub find: PdfFindState,
    pub find_input: Option<Entity<InputState>>,
    pub find_focus_pending: bool,
    pub find_generation: u64,
    _find_sub: Option<Subscription>,
    _find_task: Option<Task<()>>,
    rendered_pages: HashMap<usize, CachedPage>,
    render_lru: VecDeque<usize>,
    in_flight_renders: HashMap<(usize, u32), Task<()>>,
    page_links: HashMap<usize, Vec<PdfPageLink>>,
    restored: bool,
}

impl PdfReady {
    pub fn new(doc_key: String, decoded: DecodedPdf, saved: Option<&SavedPdfPosition>) -> Self {
        let ratios = match saved {
            Some(pos) if pos.ratios.len() == decoded.page_count => pos.ratios.clone(),
            _ => decoded.ratios,
        };
        let zoom = saved.and_then(|pos| pos.view.zoom);
        Self {
            doc_key,
            bytes: decoded.bytes,
            page_count: decoded.page_count,
            page_width: decoded.page_width,
            ratios,
            fallback_text: decoded.fallback_text,
            zoom,
            viewport_width: 640.0,
            viewport_height: 800.0,
            scroll: ScrollHandle::new(),
            find: PdfFindState::default(),
            find_input: None,
            find_focus_pending: false,
            find_generation: 0,
            _find_sub: None,
            _find_task: None,
            rendered_pages: HashMap::new(),
            render_lru: VecDeque::new(),
            in_flight_renders: HashMap::new(),
            page_links: HashMap::new(),
            restored: false,
        }
    }

    pub fn release(&mut self, cx: &mut App) {
        self.in_flight_renders.clear();
        self._find_task = None;
        for (_, cached) in self.rendered_pages.drain() {
            cx.drop_image(cached.image, None);
        }
        self.render_lru.clear();
    }

    fn store_rendered_page(&mut self, index: usize, width_bucket: u32, image: Arc<RenderImage>, cx: &mut App) {
        if let Some(old) = self.rendered_pages.insert(index, CachedPage { width_bucket, image }) {
            cx.drop_image(old.image, None);
        }
        if let Some(pos) = self.render_lru.iter().position(|&p| p == index) {
            self.render_lru.remove(pos);
        }
        self.render_lru.push_back(index);
        while self.render_lru.len() > PAGE_RASTER_CACHE_LIMIT {
            if let Some(evict) = self.render_lru.pop_front()
                && let Some(old) = self.rendered_pages.remove(&evict)
            {
                cx.drop_image(old.image, None);
            }
        }
    }

    pub fn scroll_top(&self) -> f32 {
        (-f32::from(self.scroll.offset().y)).max(0.0)
    }

    pub fn jump_to_page(&mut self, page_index: usize) {
        let (_, width) = pdf_page_width(self.viewport_width, self.page_width, self.zoom);
        let offsets = pdf_page_offsets(&self.ratios, width);
        let target_y = pdf_page_scroll_top(&offsets, page_index);
        let current_x = self.scroll.offset().x;
        self.scroll.set_offset(point(current_x, px(-target_y)));
    }
}

impl PdfState {
    pub fn from_decoded(doc_key: String, result: Result<DecodedPdf, String>, store: &PdfPositionStore) -> Self {
        match result {
            Ok(decoded) => Self::Ready(Box::new(PdfReady::new(doc_key.clone(), decoded, store.get(&doc_key)))),
            Err(error) => Self::Failed(error),
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    pub fn release(&mut self, cx: &mut App) {
        if let Self::Ready(ready) = self {
            ready.release(cx);
        }
    }

    pub fn save_position(&self, store: &mut PdfPositionStore) {
        let Self::Ready(ready) = self else { return };
        let (_, width) = pdf_page_width(ready.viewport_width, ready.page_width, ready.zoom);
        let offsets = pdf_page_offsets(&ready.ratios, width);
        let top = ready.scroll_top();
        let page = current_pdf_page(&offsets, ready.page_count, top);
        let page_top = (offsets.get(page).copied().unwrap_or(PDF_PAGE_GAP) - PDF_PAGE_GAP).max(0.0);
        let left = (-f32::from(ready.scroll.offset().x)).max(0.0);
        store.save(
            ready.doc_key.clone(),
            SavedPdfPosition {
                view: PdfViewState { page, offset: (top - page_top).max(0.0), zoom: ready.zoom, left },
                width,
                ratios: ready.ratios.clone(),
            },
        );
    }
}

impl Preview {
    pub(super) fn focus_pdf_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(ready) = self.pdf_ready_mut() else {
            return false;
        };
        if let Some(input) = ready.find_input.clone() {
            input.update(cx, |state, cx| state.focus(window, cx));
        } else {
            ready.find_focus_pending = true;
            cx.notify();
        }
        true
    }

    pub(super) fn start_pdf_find(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(ready) = self.pdf_ready_mut() else { return };
        ready.find_generation += 1;
        let generation = ready.find_generation;
        if query.trim().is_empty() {
            ready.find.clear();
            ready._find_task = None;
            cx.notify();
            return;
        }
        ready.find.start(query.clone());
        let bytes = Arc::clone(&ready.bytes);
        let page_count = ready.page_count;
        let fallback_text = ready.fallback_text.clone();
        cx.notify();

        // Stream page-by-page text from background executor so the UI jumps to the first hit
        // immediately while scanning remaining pages (`pingdotgg/t3code#33956`).
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(usize, Option<f32>, String)>();
        let executor = cx.background_executor().clone();
        ready._find_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIND_DEBOUNCE).await;
            let scanner = executor.spawn(async move {
                let session = native_pdf::PdfScanSession::open(&bytes);
                for index in 0..page_count {
                    if tx.is_closed() {
                        break;
                    }
                    let (ratio, native_text) = session.page_ratio_and_text(index);
                    let text = native_text
                        .or_else(|| fallback_text.get(index).cloned())
                        .unwrap_or_default();
                    if tx.send((index, ratio, text)).is_err() {
                        break;
                    }
                }
            });
            while let Some((index, ratio, text)) = rx.recv().await {
                let keep_going = this.update(cx, |this, cx| {
                    let Some(ready) = this.pdf_ready_mut() else { return false };
                    if ready.find_generation != generation {
                        return false;
                    }
                    if let Some(r) = ratio
                        && let Some(slot) = ready.ratios.get_mut(index)
                    {
                        *slot = r;
                    }
                    match ready.find.push_page_text(index, &text) {
                        PdfFindStep::JumpToFirstMatch(page) => {
                            ready.jump_to_page(page);
                            cx.notify();
                            true
                        }
                        PdfFindStep::ExceededLimit(_) => {
                            cx.notify();
                            false
                        }
                        PdfFindStep::Continue => true,
                    }
                });
                if !matches!(keep_going, Ok(true)) {
                    drop(rx);
                    drop(scanner);
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if let Some(ready) = this.pdf_ready_mut()
                    && ready.find_generation == generation
                {
                    ready.find.finish();
                    cx.notify();
                }
            });
            scanner.await;
        }));
    }

    fn ensure_pdf_page_rendered(&mut self, index: usize, width: f32, cx: &mut Context<Self>) {
        let width_bucket = (width.round() as u32).max(32);
        let Some(ready) = self.pdf_ready_mut() else { return };
        let image_cached = ready.rendered_pages.get(&index).is_some_and(|c| c.width_bucket == width_bucket);
        let need_links = !ready.page_links.contains_key(&index);
        if image_cached && !need_links {
            return;
        }
        if ready.in_flight_renders.contains_key(&(index, width_bucket)) {
            return;
        }
        let bytes = Arc::clone(&ready.bytes);
        let doc_key = ready.doc_key.clone();
        let task = cx.spawn(async move |this, cx| {
            let (rendered, links) = cx
                .background_executor()
                .spawn(async move {
                    let rendered = if image_cached {
                        None
                    } else {
                        native_pdf::render_page_image(&bytes, index, width)
                    };
                    let links = if need_links {
                        Some(native_pdf::extract_page_links(&bytes, index))
                    } else {
                        None
                    };
                    (rendered, links)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(ready) = this.pdf_ready_mut()
                    && ready.doc_key == doc_key
                {
                    ready.in_flight_renders.remove(&(index, width_bucket));
                    if let Some(links) = links {
                        ready.page_links.insert(index, links);
                    }
                    if let Some(image) = rendered {
                        ready.store_rendered_page(index, width_bucket, image, cx);
                    }
                    cx.notify();
                }
            });
        });
        ready.in_flight_renders.insert((index, width_bucket), task);
    }

    pub(super) fn render_pdf(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let is_loading = matches!(self.pdf_state(), Some(PdfState::Loading));
        if is_loading {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child("Loading PDF…")
                .into_any_element();
        }
        if let Some(PdfState::Failed(err)) = self.pdf_state() {
            let message = err.clone();
            return v_flex()
                .id("file-pdf-error")
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .px(px(24.))
                .text_center()
                .child(div().text_sm().font_weight(gpui::FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("No preview for this file"))
                .child(div().max_w(px(360.)).text_xs().text_color(hsla(p.muted_foreground)).child(message))
                .child(
                    Button::new("pdf-retry")
                        .variant(Variant::Secondary)
                        .size(Size::Xs)
                        .label("Retry")
                        .on_click(cx.listener(|this, _, _, cx| this.refetch(cx))),
                )
                .into_any_element();
        }

        // Ensure find input exists and restore saved view once.
        let saved_position = self.pdf_ready().and_then(|r| self.pdf_positions.get(&r.doc_key).cloned());
        if let Some(ready) = self.pdf_ready_mut() {
            if ready.find_input.is_none() {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in PDF").clean_on_escape());
                let sub = cx.subscribe(&input, |this: &mut Preview, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        let query = input.read(cx).value().to_string();
                        this.start_pdf_find(query, cx);
                    }
                });
                ready.find_input = Some(input);
                ready._find_sub = Some(sub);
            }
            if std::mem::take(&mut ready.find_focus_pending)
                && let Some(input) = ready.find_input.clone()
            {
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            if !ready.restored {
                ready.restored = true;
                if let Some(saved) = saved_position {
                    let (_, width) = pdf_page_width(ready.viewport_width, ready.page_width, ready.zoom);
                    let top = pdf_restored_scroll_top(&ready.ratios, width, &saved.view, Some(saved.width));
                    ready.scroll.set_offset(point(px(-saved.view.left), px(-top)));
                }
            }
        }

        let Some(ready) = self.pdf_ready() else { return div().into_any_element() };
        let (scale, width) = pdf_page_width(ready.viewport_width, ready.page_width, ready.zoom);
        let offsets = pdf_page_offsets(&ready.ratios, width);
        let scroll_top = ready.scroll_top();
        let page_count = ready.page_count;
        let current = current_pdf_page(&offsets, page_count, scroll_top);
        let visible = visible_pdf_pages(&ready.ratios, &offsets, scroll_top, ready.viewport_height);
        let zoom_is_fit = ready.zoom.is_none();
        let find_input = ready.find_input.clone();
        let has_query = !ready.find.query.trim().is_empty();
        let find_status = ready.find.status_label();
        let has_matches = !ready.find.matches.is_empty();
        let total_height = offsets.last().copied().unwrap_or(PDF_PAGE_GAP);
        let scroll = ready.scroll.clone();

        // Schedule rasterization for visible pages off-thread.
        for &(index, _) in &visible {
            self.ensure_pdf_page_rendered(index, width, cx);
        }

        let Some(ready) = self.pdf_ready() else { return div().into_any_element() };
        let page_label = if page_count == 1 {
            format!("{} / 1 page", current + 1)
        } else {
            format!("{} / {page_count} pages", current + 1)
        };
        let zoom_label = if zoom_is_fit { "Fit width".to_owned() } else { format!("{}%", (scale * 100.0).round() as i32) };

        let page_elements: Vec<AnyElement> = visible
            .iter()
            .map(|&(index, ratio)| {
                let page_h = (width * ratio).max(32.0);
                let top = offsets.get(index).copied().unwrap_or(PDF_PAGE_GAP);
                let left = ((ready.viewport_width - width) / 2.0).max(PDF_PAGE_GAP);
                let image = ready.rendered_pages.get(&index).map(|c| c.image.clone());
                let is_match = ready.find.matches.contains(&index);
                let fallback = ready.fallback_text.get(index).cloned().unwrap_or_default();
                let body: AnyElement = if let Some(image) = image {
                    canvas(
                        |_, _, _| {},
                        move |bounds: Bounds<gpui::Pixels>, (), window, _| {
                            let _ = window.paint_image(bounds, Corners::default(), image.clone(), 0, false);
                        },
                    )
                    .size_full()
                    .into_any_element()
                } else if !fallback.is_empty() {
                    div()
                        .size_full()
                        .p(px(16.))
                        .overflow_hidden()
                        .text_xs()
                        .text_color(gpui::black())
                        .child(SharedString::from(fallback))
                        .into_any_element()
                } else {
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(format!("Page {}", index + 1))
                        .into_any_element()
                };
                let links: Vec<AnyElement> = ready
                    .page_links
                    .get(&index)
                    .map(|page_links| {
                        page_links
                            .iter()
                            .enumerate()
                            .map(|(link_idx, link)| {
                                let [nx, ny, nw, nh] = link.box_norm;
                                let target = link.target.clone();
                                let tip: SharedString = match &target {
                                    PdfLinkTarget::External(url) => url.clone().into(),
                                    PdfLinkTarget::Page(p) => format!("Page {}", p + 1).into(),
                                };
                                div()
                                    .id(("pdf-page-link", index * 4096 + link_idx))
                                    .absolute()
                                    .left(px(nx * width))
                                    .top(px(ny * page_h))
                                    .w(px((nw * width).max(4.0)))
                                    .h(px((nh * page_h).max(4.0)))
                                    .rounded(px(2.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(hsla(p.primary).opacity(0.12)))
                                    .tooltip(move |window, cx| crate::right_panel::tooltip_view(tip.clone(), window, cx))
                                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        match &target {
                                            PdfLinkTarget::External(url) => {
                                                let thread_id = this.target.as_ref().map(|t| t.root.workspace.thread_id.clone());
                                                crate::transcript::markdown::links::open_browser_link(
                                                    url,
                                                    event.modifiers(),
                                                    Some(&this.state),
                                                    thread_id,
                                                    cx,
                                                );
                                            }
                                            PdfLinkTarget::Page(target_page) => {
                                                if let Some(ready) = this.pdf_ready_mut() {
                                                    ready.jump_to_page(*target_page);
                                                    cx.notify();
                                                }
                                            }
                                        }
                                    }))
                                    .into_any_element()
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                div()
                    .id(("pdf-page", index))
                    .absolute()
                    .top(px(top))
                    .left(px(left))
                    .w(px(width))
                    .h(px(page_h))
                    .bg(gpui::white())
                    .shadow_sm()
                    .when(is_match, |this| this.border_2().border_color(hsla(p.primary)))
                    .child(body)
                    .when(!links.is_empty(), |this| {
                        this.child(div().absolute().inset_0().children(links))
                    })
                    .into_any_element()
            })
            .collect();

        let entity = cx.entity().downgrade();
        let viewport_observer = canvas(
            move |bounds, _, cx| {
                let w = f32::from(bounds.size.width);
                let h = f32::from(bounds.size.height);
                if w > 0.0 && h > 0.0
                    && let Some(preview) = entity.upgrade()
                {
                    preview.update(cx, |this, cx| {
                        if let Some(ready) = this.pdf_ready_mut()
                            && ((ready.viewport_width - w).abs() > 1.0 || (ready.viewport_height - h).abs() > 1.0)
                        {
                            ready.viewport_width = w;
                            ready.viewport_height = h;
                            cx.notify();
                        }
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        v_flex()
            .id("file-pdf-viewer")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .child(
                h_flex()
                    .flex_none()
                    .min_h(px(36.))
                    .px(px(8.))
                    .py(px(4.))
                    .gap(px(4.))
                    .items_center()
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .bg(hsla(p.background))
                    .text_xs()
                    .child(div().mr_auto().px(px(4.)).text_color(hsla(p.foreground)).child(SharedString::from(page_label)))
                    .child(
                        Button::new("pdf-prev-page")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::ChevronUp)
                            .tooltip("Previous page")
                            .disabled(current == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(ready) = this.pdf_ready_mut() {
                                    ready.jump_to_page(current.saturating_sub(1));
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("pdf-next-page")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::ChevronDown)
                            .tooltip("Next page")
                            .disabled(current + 1 >= page_count)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(ready) = this.pdf_ready_mut() {
                                    ready.jump_to_page((current + 1).min(page_count.saturating_sub(1)));
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("pdf-zoom-out")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::Minus)
                            .tooltip("Zoom out")
                            .disabled(scale <= PDF_MIN_ZOOM)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(ready) = this.pdf_ready_mut() {
                                    ready.zoom = Some(step_pdf_zoom(scale, -1));
                                    cx.notify();
                                }
                            })),
                    )
                    .child(div().min_w(px(56.)).text_center().text_color(hsla(p.muted_foreground)).child(SharedString::from(zoom_label)))
                    .child(
                        Button::new("pdf-zoom-in")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::Plus)
                            .tooltip("Zoom in")
                            .disabled(scale >= PDF_MAX_ZOOM)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(ready) = this.pdf_ready_mut() {
                                    ready.zoom = Some(step_pdf_zoom(scale, 1));
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("pdf-fit-width")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::Maximize2)
                            .tooltip("Fit width")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(ready) = this.pdf_ready_mut() {
                                    ready.zoom = None;
                                    let y = ready.scroll.offset().y;
                                    ready.scroll.set_offset(point(px(0.), y));
                                    cx.notify();
                                }
                            })),
                    )
                    .when_some(find_input, |this, input| {
                        this.child(div().w(px(120.)).child(Input::new(&input).small()))
                    })
                    .when(has_query, |this| {
                        this.child(div().text_color(hsla(p.muted_foreground)).child(SharedString::from(find_status)))
                            .child(
                                Button::new("pdf-next-match")
                                    .variant(Variant::Ghost)
                                    .size(Size::IconXs)
                                    .icon(Icon::ChevronDown)
                                    .tooltip("Next matching page")
                                    .disabled(!has_matches)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(ready) = this.pdf_ready_mut()
                                            && let Some(page) = ready.find.next_match()
                                        {
                                            ready.jump_to_page(page);
                                            cx.notify();
                                        }
                                    })),
                            )
                    }),
            )
            .child(
                div()
                    .id("pdf-viewport")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .bg(hsla(p.muted).opacity(0.4))
                    .child(
                        div()
                            .id("pdf-scroll")
                            .size_full()
                            .overflow_scroll()
                            .track_scroll(&scroll)
                            .child(
                                div()
                                    .relative()
                                    .w(px(width + PDF_PAGE_GAP * 2.0))
                                    .min_w_full()
                                    .h(px(total_height.max(100.0)))
                                    .children(page_elements),
                            ),
                    )
                    .vertical_scrollbar(&scroll)
                    .child(viewport_observer),
            )
            .into_any_element()
    }
}
