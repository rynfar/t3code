//! PDF viewer layout, progressive Find, position retention, and lightweight fallback parser.
//! Ports `apps/web/src/components/files/pdfLayout.ts` and `PdfViewer.tsx` (including the
//! progressive first-hit jump of `pingdotgg/t3code#33956`).

use std::collections::VecDeque;

/// Gap between rendered pages in CSS pixels (`PDF_PAGE_GAP`).
pub const PDF_PAGE_GAP: f32 = 16.0;
pub const PDF_MIN_ZOOM: f32 = 0.25;
pub const PDF_MAX_ZOOM: f32 = 3.0;
pub const PDF_ZOOM_STEP: f32 = 0.25;
pub const PDF_MAX_PAGES: usize = 2_000;
pub const PDF_MAX_FIND_TEXT: usize = 4 * 1024 * 1024;
pub const PDF_MAX_BYTES: usize = 50 * 1024 * 1024;
pub const PDF_MAX_POSITIONS: usize = 50;

/// Link destination inside a PDF page (`PdfViewer.tsx::PdfLink`, `pingdotgg/t3code#40507`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfLinkTarget {
    External(String),
    Page(usize),
}

/// Clickable link annotation rectangle in `[0..=1]` top-left normalized page coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfPageLink {
    pub box_norm: [f32; 4],
    pub target: PdfLinkTarget,
}

/// Only external `http(s)://` URLs are clickable in the PDF overlay (`pingdotgg/t3code#40507`).
pub fn is_safe_pdf_external_url(url: &str) -> bool {
    let trimmed = url.trim();
    trimmed
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || trimmed
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}

/// Converts a PDF page-space rectangle `[x1, y1, x2, y2]` relative to `crop_box`
/// `[origin_x, origin_y, width, height]` and page `rotation` into top-left normalized
/// `[left, top, width, height]` coordinates in `0.0..=1.0`.
pub fn pdf_rect_to_normalized_box(rect: [f32; 4], crop_box: [f32; 4], rotation: i32) -> Option<[f32; 4]> {
    let [bx, by, bw, bh] = crop_box;
    if !bw.is_finite() || !bh.is_finite() || bw <= 0.0 || bh <= 0.0 {
        return None;
    }
    let rot = rotation.rem_euclid(360);
    let convert = |x: f32, y: f32| -> (f32, f32) {
        let nx = (x - bx) / bw;
        let ny = (y - by) / bh;
        match rot {
            90 => (ny, nx),
            180 => (1.0 - nx, ny),
            270 => (1.0 - ny, 1.0 - nx),
            _ => (nx, 1.0 - ny),
        }
    };
    let (vx1, vy1) = convert(rect[0], rect[1]);
    let (vx2, vy2) = convert(rect[2], rect[3]);
    let left = vx1.min(vx2).clamp(0.0, 1.0);
    let top = vy1.min(vy2).clamp(0.0, 1.0);
    let right = vx1.max(vx2).clamp(0.0, 1.0);
    let bottom = vy1.max(vy2).clamp(0.0, 1.0);
    let width = right - left;
    let height = bottom - top;
    (width > 0.0 && height > 0.0).then_some([left, top, width, height])
}

/// Saved viewport position for one PDF document (`PdfViewState` + measured ratios).
#[derive(Debug, Clone, PartialEq)]
pub struct PdfViewState {
    pub page: usize,
    pub offset: f32,
    pub zoom: Option<f32>,
    pub left: f32,
}

impl Default for PdfViewState {
    fn default() -> Self {
        Self { page: 0, offset: 0.0, zoom: None, left: 0.0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SavedPdfPosition {
    pub view: PdfViewState,
    pub width: f32,
    pub ratios: Vec<f32>,
}

/// Bounded LRU position store across tab switches (`MAX_POSITIONS = 50`).
#[derive(Debug, Default)]
pub struct PdfPositionStore {
    entries: VecDeque<(String, SavedPdfPosition)>,
}

impl PdfPositionStore {
    pub fn get(&self, key: &str) -> Option<&SavedPdfPosition> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn save(&mut self, key: String, position: SavedPdfPosition) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &key) {
            self.entries.remove(pos);
        }
        self.entries.push_back((key, position));
        while self.entries.len() > PDF_MAX_POSITIONS {
            self.entries.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// `pdfPageWidth`: `(scale, rendered_width)` given viewport width, natural page width, and
/// optional explicit zoom (`None` = fit to width).
pub fn pdf_page_width(viewport_width: f32, page_width: f32, zoom: Option<f32>) -> (f32, f32) {
    let safe_page = if page_width.is_finite() && page_width > 0.0 { page_width } else { 612.0 };
    let fit_width = (viewport_width - 2.0 * PDF_PAGE_GAP).max(1.0);
    match zoom {
        Some(z) => {
            let clamped = z.clamp(PDF_MIN_ZOOM, PDF_MAX_ZOOM);
            (clamped, safe_page * clamped)
        }
        None => (fit_width / safe_page, fit_width),
    }
}

/// `stepPdfZoom`: increments or decrements scale by `PDF_ZOOM_STEP`, clamped to min/max.
pub fn step_pdf_zoom(scale: f32, direction: i8) -> f32 {
    if direction > 0 {
        (scale + PDF_ZOOM_STEP).min(PDF_MAX_ZOOM)
    } else {
        (scale - PDF_ZOOM_STEP).max(PDF_MIN_ZOOM)
    }
}

/// `pdfPageOffsets`: top offset of each page plus the end of the last one; `ratios` are `height / width`.
pub fn pdf_page_offsets(ratios: &[f32], width: f32) -> Vec<f32> {
    let mut result = Vec::with_capacity(ratios.len() + 1);
    result.push(PDF_PAGE_GAP);
    for &ratio in ratios {
        let last = *result.last().unwrap_or(&PDF_PAGE_GAP);
        let safe_ratio = if ratio.is_finite() && ratio > 0.0 { ratio } else { 792.0 / 612.0 };
        result.push(last + width * safe_ratio + PDF_PAGE_GAP);
    }
    result
}

/// `currentPdfPage`: 0-based index of the topmost page visible past `scroll_top`.
pub fn current_pdf_page(offsets: &[f32], page_count: usize, scroll_top: f32) -> usize {
    for index in 0..page_count {
        if offsets.get(index + 1).is_some_and(|&bottom| bottom > scroll_top + PDF_PAGE_GAP + 1.0) {
            return index;
        }
    }
    0
}

/// `visiblePdfPages`: pages within one viewport above and two below the visible area stay mounted.
pub fn visible_pdf_pages(ratios: &[f32], offsets: &[f32], scroll_top: f32, viewport_height: f32) -> Vec<(usize, f32)> {
    ratios
        .iter()
        .enumerate()
        .filter_map(|(index, &ratio)| {
            let top = *offsets.get(index)?;
            let bottom = *offsets.get(index + 1)?;
            (bottom >= scroll_top - viewport_height && top <= scroll_top + viewport_height * 3.0)
                .then_some((index, ratio))
        })
        .collect()
}

/// `pdfPageScrollTop`: vertical scroll coordinate that lands `index` at the top of the viewport.
pub fn pdf_page_scroll_top(offsets: &[f32], index: usize) -> f32 {
    let top = offsets.get(index).copied().unwrap_or(PDF_PAGE_GAP);
    (top - PDF_PAGE_GAP).max(0.0)
}

/// `pdfRestoredScrollTop`: restores a saved position even if the reloaded PDF has fewer or shorter pages.
pub fn pdf_restored_scroll_top(
    ratios: &[f32],
    width: f32,
    view: &PdfViewState,
    saved_width: Option<f32>,
) -> f32 {
    if ratios.is_empty() {
        return 0.0;
    }
    let page = view.page.min(ratios.len() - 1);
    let offsets = pdf_page_offsets(ratios, width);
    let prev_width = saved_width.filter(|w| w.is_finite() && *w > 0.0).unwrap_or(width.max(1.0));
    let scaled_offset = (view.offset * width / prev_width).min((width * ratios[page] - 1.0).max(0.0));
    (offsets[page] - PDF_PAGE_GAP + scaled_offset).max(0.0)
}

/// Folds typeset curly quotes to straight ASCII quotes (`foldQuotes`), as pdf.js Find does.
pub fn fold_quotes(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\u{2018}'..='\u{201b}' => '\'',
            '\u{201c}'..='\u{201f}' => '"',
            other => other,
        })
        .collect()
}

/// Normalizes text for PDF Find: lowercase, fold curly quotes, drop hyphenation across a line
/// break between Unicode letters (`(\p{L})-\n(?=\p{L})`), and collapse whitespace runs to one space.
fn normalize_pdf_find_text(runs: &[&str]) -> (String, Vec<usize>) {
    let mut chars: Vec<(char, usize)> = Vec::new();
    for (run_idx, run) in runs.iter().enumerate() {
        for ch in fold_quotes(&run.to_lowercase()).chars() {
            chars.push((ch, run_idx));
        }
    }
    let mut text = String::new();
    let mut last = '\0';
    let mut owner: Vec<usize> = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let (ch, run) = chars[index];
        if ch.is_whitespace() {
            if !text.ends_with(' ') {
                text.push(' ');
                last = ' ';
                owner.push(run);
            }
            index += 1;
        } else if ch == '-'
            && chars.get(index + 1).is_some_and(|(next, _)| *next == '\n')
            && last.is_alphabetic()
            && chars.get(index + 2).is_some_and(|(after, _)| after.is_alphabetic())
        {
            index += 2;
        } else {
            text.push(ch);
            last = ch;
            for _ in 0..ch.len_utf8() {
                owner.push(run);
            }
            index += 1;
        }
    }
    (text, owner)
}

fn normalize_needle(query: &str) -> String {
    let folded = fold_quotes(&query.to_lowercase());
    let mut out = String::with_capacity(folded.len());
    let mut in_ws = false;
    for ch in folded.chars() {
        if ch.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(ch);
            in_ws = false;
        }
    }
    out
}

/// `pdfFindIncludes`: whether a page's extracted text matches `query`.
pub fn pdf_find_includes(text: &str, query: &str) -> bool {
    let needle = normalize_needle(query);
    if needle.trim().is_empty() {
        return false;
    }
    let (normalized, _) = normalize_pdf_find_text(&[text]);
    normalized.contains(&needle)
}

/// `pdfFindMatchRuns`: returns a boolean per text run indicating whether it overlaps any match.
pub fn pdf_find_match_runs(runs: &[&str], query: &str) -> Vec<bool> {
    let mut hits = vec![false; runs.len()];
    let needle = normalize_needle(query);
    if needle.trim().is_empty() {
        return hits;
    }
    let (text, owner) = normalize_pdf_find_text(runs);
    let mut search_from = 0;
    while let Some(offset) = text[search_from..].find(&needle) {
        let start = search_from + offset;
        let end = start + needle.len();
        for &run_idx in &owner[start..end] {
            if let Some(slot) = hits.get_mut(run_idx) {
                *slot = true;
            }
        }
        search_from = end;
    }
    hits
}

/// Progressive PDF Find state machine (`pingdotgg/t3code#33956`):
/// scans pages incrementally, jumps to the first hit while the remaining pages are still being
/// scanned, and does NOT override manual page navigation when the scan finishes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PdfFindState {
    pub query: String,
    pub finding: bool,
    pub matches: Vec<usize>,
    pub active_match: usize,
    pub error: Option<String>,
    pub scanned_text_bytes: usize,
    jumped_to_first: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfFindStep {
    Continue,
    /// First match found mid-scan: jump to this page immediately while keeping `finding = true`.
    JumpToFirstMatch(usize),
    /// Text budget exceeded (`PDF_MAX_FIND_TEXT`).
    ExceededLimit(String),
}

impl PdfFindState {
    pub fn start(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.matches.clear();
        self.active_match = 0;
        self.error = None;
        self.scanned_text_bytes = 0;
        self.jumped_to_first = false;
        self.finding = !self.query.trim().is_empty();
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Feeds the next scanned page's text. Returns `JumpToFirstMatch(page)` only on the first hit
    /// so later manual page navigation is never clobbered when scanning continues or finishes.
    pub fn push_page_text(&mut self, page_index: usize, text: &str) -> PdfFindStep {
        if !self.finding {
            return PdfFindStep::Continue;
        }
        self.scanned_text_bytes = self.scanned_text_bytes.saturating_add(text.len());
        if self.scanned_text_bytes > PDF_MAX_FIND_TEXT {
            let msg = "This PDF has too much text to search. Save it to search in another app.".to_owned();
            self.finding = false;
            self.error = Some(msg.clone());
            return PdfFindStep::ExceededLimit(msg);
        }
        if !pdf_find_includes(text, &self.query) {
            return PdfFindStep::Continue;
        }
        self.matches.push(page_index);
        if !self.jumped_to_first {
            self.jumped_to_first = true;
            self.active_match = 0;
            return PdfFindStep::JumpToFirstMatch(page_index);
        }
        PdfFindStep::Continue
    }

    /// Marks the scan complete without issuing another jump.
    pub fn finish(&mut self) {
        self.finding = false;
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        self.finding = false;
        self.error = Some(message.into());
    }

    /// Advances to the next matching page and returns its 0-based page index.
    pub fn next_match(&mut self) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        self.active_match = (self.active_match + 1) % self.matches.len();
        self.matches.get(self.active_match).copied()
    }

    /// Moves to the previous matching page and returns its 0-based page index.
    pub fn prev_match(&mut self) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        self.active_match = if self.active_match == 0 {
            self.matches.len() - 1
        } else {
            self.active_match - 1
        };
        self.matches.get(self.active_match).copied()
    }

    /// Status label matching `PdfViewer.tsx` (`Searching…`, error text, or `1/3` / `0/0`).
    pub fn status_label(&self) -> String {
        if self.finding {
            "Searching…".to_owned()
        } else if let Some(error) = &self.error {
            error.clone()
        } else if self.matches.is_empty() {
            "0/0".to_owned()
        } else {
            format!("{}/{}", self.active_match + 1, self.matches.len())
        }
    }
}

/// Lightweight pure-Rust PDF metadata & uncompressed text-stream extractor used on non-macOS
/// targets and in unit tests.
#[derive(Debug, Clone, PartialEq)]
pub struct BasicPdfPage {
    pub width: f32,
    pub height: f32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BasicPdfDocument {
    pub pages: Vec<BasicPdfPage>,
}

pub fn parse_basic_pdf(bytes: &[u8]) -> Result<BasicPdfDocument, String> {
    if bytes.len() > PDF_MAX_BYTES {
        return Err("Preview limited to files under 50 MB.".to_owned());
    }
    if !bytes.starts_with(b"%PDF-") {
        return Err("Unable to open PDF.".to_owned());
    }
    let text = String::from_utf8_lossy(bytes);
    let mut pages = Vec::new();
    let mut search = 0usize;
    while let Some(pos) = text[search..].find("/Type /Page") {
        let abs = search + pos;
        let after = abs + "/Type /Page".len();
        search = after;
        // Skip `/Type /Pages` (the parent tree node).
        if text[after..].starts_with('s') {
            continue;
        }
        let dict_end = text[abs..].find(">>").map_or(text.len(), |i| abs + i);
        let dict = &text[abs..dict_end];
        let (mut w, mut h) = parse_mediabox(dict).unwrap_or((612.0, 792.0));
        let rotate = parse_rotate(dict).unwrap_or(0);
        if rotate % 180 != 0 {
            std::mem::swap(&mut w, &mut h);
        }
        let page_text = extract_page_contents(&text, dict).unwrap_or_default();
        pages.push(BasicPdfPage { width: w.max(1.0), height: h.max(1.0), text: page_text });
        if pages.len() > PDF_MAX_PAGES {
            return Err("Preview limited to 2,000 pages. Save this PDF to read it in full.".to_owned());
        }
    }
    if pages.is_empty() {
        // Minimal valid PDF with no uncompressed `/Type /Page` literal (e.g., object streams).
        pages.push(BasicPdfPage { width: 612.0, height: 792.0, text: String::new() });
    }
    Ok(BasicPdfDocument { pages })
}

fn parse_mediabox(dict: &str) -> Option<(f32, f32)> {
    let idx = dict.find("/MediaBox")?;
    let rest = &dict[idx + "/MediaBox".len()..];
    let open = rest.find('[')?;
    let close = rest[open + 1..].find(']')?;
    let nums: Vec<f32> = rest[open + 1..open + 1 + close]
        .split_whitespace()
        .filter_map(|token| token.parse::<f32>().ok())
        .collect();
    if nums.len() >= 4 {
        let w = (nums[2] - nums[0]).abs();
        let h = (nums[3] - nums[1]).abs();
        if w > 0.0 && h > 0.0 {
            return Some((w, h));
        }
    }
    None
}

fn parse_rotate(dict: &str) -> Option<i32> {
    let idx = dict.find("/Rotate")?;
    let rest = dict[idx + "/Rotate".len()..].trim_start();
    let token = rest.split_whitespace().next()?;
    token.parse::<i32>().ok()
}

fn extract_page_contents(doc: &str, dict: &str) -> Option<String> {
    let idx = dict.find("/Contents")?;
    let rest = dict[idx + "/Contents".len()..].trim_start();
    let obj_num = rest.split_whitespace().next()?.parse::<u32>().ok()?;
    let marker = format!("{obj_num} 0 obj");
    let obj_start = doc.find(&marker)?;
    let obj_body = &doc[obj_start..];
    let stream_start = obj_body.find("stream")? + "stream".len();
    let after_stream = obj_body[stream_start..].trim_start_matches(['\r', '\n']);
    let stream_end = after_stream.find("endstream")?;
    let stream = &after_stream[..stream_end];
    Some(extract_pdf_literal_strings(stream))
}

fn extract_pdf_literal_strings(stream: &str) -> String {
    let mut out = String::new();
    let mut chars = stream.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '(' {
            continue;
        }
        let mut depth = 1usize;
        let mut run = String::new();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(esc) = chars.next() {
                    match esc {
                        'n' => run.push('\n'),
                        'r' => run.push('\r'),
                        't' => run.push('\t'),
                        other => run.push(other),
                    }
                }
            } else if c == '(' {
                depth += 1;
                run.push(c);
            } else if c == ')' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                run.push(c);
            } else {
                run.push(c);
            }
        }
        if !run.is_empty() {
            out.push_str(&run);
        }
    }
    out
}
