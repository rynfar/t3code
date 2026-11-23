//! Office & rich document preview parser and spreadsheet resize model.
//! Ports `apps/web/src/components/files/OfficeViewer.tsx`, `OpenXmlOfficeViewer.tsx`,
//! `officePreviewProtocol.ts`, `office/sheet-sizing.ts`, `office/sheet-viewport.ts`,
//! `office/sheet-cells.ts` (`columnName`), and the row/column resize rules of
//! `pingdotgg/t3code#35548`.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use zip::ZipArchive;

/// `OFFICE_PREVIEW_MAX_BYTES`: 50 MB hard ceiling matching `officePreviewProtocol.ts`.
pub const OFFICE_PREVIEW_MAX_BYTES: usize = 50 * 1024 * 1024;
/// Bounded rows/columns rendered per sheet in the native preview.
pub const OFFICE_PREVIEW_MAX_ROWS: usize = 250_000;
pub const OFFICE_PREVIEW_MAX_COLUMNS: usize = 1024;
/// Max uncompressed XML bytes read from any single archive entry (decompression-bomb guard).
const MAX_XML_ENTRY_BYTES: usize = 16 * 1024 * 1024;
const MAX_PARAGRAPHS: usize = 10_000;
const MAX_SLIDES: usize = 500;
const MAX_CELLS_PER_SHEET: usize = 250_000;

/// Excel's 255-character column width and 409.5-point row height in CSS pixels (`sheet-sizing.ts`).
pub const MAXIMUM_COL_WIDTH: f32 = 255.0 * 7.0 + 5.0;
pub const MAXIMUM_ROW_HEIGHT: f32 = (409.5 * 4.0) / 3.0;
pub const MINIMUM_TRACK_SIZE: f32 = 16.0;
pub const DEFAULT_COL_WIDTH: f32 = 100.0;
pub const DEFAULT_ROW_HEIGHT: f32 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficeKind {
    Docx,
    Xlsx,
    Pptx,
    Odt,
    Ods,
    Odp,
    Rtf,
    Epub,
}

impl OfficeKind {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "docx" => Some(Self::Docx),
            "xlsx" => Some(Self::Xlsx),
            "pptx" => Some(Self::Pptx),
            "odt" => Some(Self::Odt),
            "ods" => Some(Self::Ods),
            "odp" => Some(Self::Odp),
            "rtf" => Some(Self::Rtf),
            "epub" => Some(Self::Epub),
            _ => None,
        }
    }

    pub fn is_spreadsheet(self) -> bool {
        matches!(self, Self::Xlsx | Self::Ods)
    }

    pub fn is_presentation(self) -> bool {
        matches!(self, Self::Pptx | Self::Odp)
    }
}

/// Next sheet tab index for keyboard navigation (`ArrowRight` / `ArrowLeft` / `Home` / `End`)
/// on a workbook tablist (`pingdotgg/t3code#41008`).
pub fn next_sheet_tab_index(current: usize, count: usize, key: &str) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let current = current.min(count - 1);
    match key {
        "right" => Some((current + 1) % count),
        "left" => Some((current + count - 1) % count),
        "home" => Some(0),
        "end" => Some(count - 1),
        _ => None,
    }
}

/// `columnName` (`office/sheet-cells.ts`): 0 -> `"A"`, 25 -> `"Z"`, 26 -> `"AA"`.
pub fn column_name(mut col: usize) -> String {
    let mut name = String::new();
    loop {
        let rem = col % 26;
        name.push((b'A' + rem as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    name.chars().rev().collect()
}

/// Parses an A1-style cell reference (e.g. `"BC12"`) into 0-based `(row, col)`.
pub fn parse_cell_ref(reference: &str) -> Option<(usize, usize)> {
    let bytes = reference.as_bytes();
    let mut i = 0;
    let mut col: usize = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        let digit = (bytes[i].to_ascii_uppercase() - b'A') as usize + 1;
        col = col.checked_mul(26)?.checked_add(digit)?;
        i += 1;
    }
    if i == 0 || i >= bytes.len() {
        return None;
    }
    let row = reference[i..].parse::<usize>().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeAxis {
    Row,
    Column,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OfficeSheet {
    pub name: String,
    pub rows: Vec<Vec<String>>,
    pub column_count: usize,
    pub col_widths: Vec<f32>,
    pub row_heights: Vec<f32>,
    pub frozen_rows: usize,
    pub frozen_cols: usize,
    pub show_gridlines: bool,
    pub truncated: bool,
}

impl OfficeSheet {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn col_width(&self, col: usize) -> f32 {
        self.col_widths.get(col).copied().unwrap_or(DEFAULT_COL_WIDTH)
    }

    pub fn row_height(&self, row: usize) -> f32 {
        self.row_heights.get(row).copied().unwrap_or(DEFAULT_ROW_HEIGHT)
    }

    /// Total width across all columns plus the row header gutter.
    pub fn total_width(&self, gutter: f32) -> f32 {
        gutter + (0..self.column_count).map(|c| self.col_width(c)).sum::<f32>()
    }

    /// Preview-only track resize (`pingdotgg/t3code#35548`, `spreadsheet.worker.ts::resize`):
    /// validates index & finite delta and clamps to `[MINIMUM_TRACK_SIZE, MAXIMUM_*]`.
    /// Never modifies the source file.
    pub fn resize_track(&mut self, axis: ResizeAxis, index: usize, delta: f32) -> Result<f32, String> {
        if !delta.is_finite() {
            return Err("Invalid track resize".to_owned());
        }
        match axis {
            ResizeAxis::Column => {
                if index >= self.column_count.max(1) || index >= OFFICE_PREVIEW_MAX_COLUMNS {
                    return Err("Invalid track resize".to_owned());
                }
                if self.col_widths.len() <= index {
                    self.col_widths.resize(index + 1, DEFAULT_COL_WIDTH);
                }
                let next = (self.col_widths[index] + delta).clamp(MINIMUM_TRACK_SIZE, MAXIMUM_COL_WIDTH);
                self.col_widths[index] = next;
                Ok(next)
            }
            ResizeAxis::Row => {
                if index >= self.rows.len().max(1) || index >= OFFICE_PREVIEW_MAX_ROWS {
                    return Err("Invalid track resize".to_owned());
                }
                if self.row_heights.len() <= index {
                    self.row_heights.resize(index + 1, DEFAULT_ROW_HEIGHT);
                }
                let next = (self.row_heights[index] + delta).clamp(MINIMUM_TRACK_SIZE, MAXIMUM_ROW_HEIGHT);
                self.row_heights[index] = next;
                Ok(next)
            }
        }
    }

    /// Sets a column width directly (clamped to `[MINIMUM_TRACK_SIZE, MAXIMUM_COL_WIDTH]`).
    pub fn set_col_width(&mut self, col: usize, width: f32) -> Result<f32, String> {
        let current = self.col_width(col);
        self.resize_track(ResizeAxis::Column, col, width - current)
    }

    /// Sets a row height directly (clamped to `[MINIMUM_TRACK_SIZE, MAXIMUM_ROW_HEIGHT]`).
    pub fn set_row_height(&mut self, row: usize, height: f32) -> Result<f32, String> {
        let current = self.row_height(row);
        self.resize_track(ResizeAxis::Row, row, height - current)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OfficeWorkbook {
    pub sheets: Vec<OfficeSheet>,
    pub truncated: bool,
}

impl OfficeWorkbook {
    pub fn details_label(&self) -> String {
        match self.sheets.len() {
            1 => "1 sheet".to_owned(),
            n => format!("{n} sheets"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentBlock {
    Heading { level: u8, text: String },
    Paragraph(String),
    Table(Vec<Vec<String>>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentChapter {
    pub title: Option<String>,
    pub blocks: Vec<DocumentBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficeDocument {
    pub chapters: Vec<DocumentChapter>,
    pub truncated: bool,
}

impl OfficeDocument {
    pub fn details_label(&self) -> String {
        if self.chapters.len() > 1 {
            format!("{} chapters", self.chapters.len())
        } else {
            let blocks: usize = self.chapters.iter().map(|c| c.blocks.len()).sum();
            match blocks {
                1 => "1 block".to_owned(),
                n => format!("{n} blocks"),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficeSlide {
    pub title: Option<String>,
    pub paragraphs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficePresentation {
    pub slides: Vec<OfficeSlide>,
    pub truncated: bool,
}

impl OfficePresentation {
    pub fn details_label(&self) -> String {
        match self.slides.len() {
            1 => "1 slide".to_owned(),
            n => format!("{n} slides"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OfficePreview {
    Workbook(OfficeWorkbook),
    Document(OfficeDocument),
    Presentation(OfficePresentation),
}

impl OfficePreview {
    pub fn truncated(&self) -> bool {
        match self {
            Self::Workbook(wb) => wb.truncated,
            Self::Document(doc) => doc.truncated,
            Self::Presentation(pres) => pres.truncated,
        }
    }

    pub fn details_label(&self) -> String {
        match self {
            Self::Workbook(wb) => wb.details_label(),
            Self::Document(doc) => format!("{} · Fit to width", doc.details_label()),
            Self::Presentation(pres) => format!("{} · Fit to width", pres.details_label()),
        }
    }
}

pub fn parse_office_preview(kind: OfficeKind, bytes: &[u8]) -> Result<OfficePreview, String> {
    if bytes.len() > OFFICE_PREVIEW_MAX_BYTES {
        return Err("Preview limited to files under 50 MB.".to_owned());
    }
    match kind {
        OfficeKind::Xlsx => parse_xlsx(bytes).map(OfficePreview::Workbook),
        OfficeKind::Ods => parse_ods(bytes).map(OfficePreview::Workbook),
        OfficeKind::Docx => parse_docx(bytes).map(OfficePreview::Document),
        OfficeKind::Odt => parse_odt(bytes).map(OfficePreview::Document),
        OfficeKind::Rtf => parse_rtf(bytes).map(OfficePreview::Document),
        OfficeKind::Epub => parse_epub(bytes).map(OfficePreview::Document),
        OfficeKind::Pptx => parse_pptx(bytes).map(OfficePreview::Presentation),
        OfficeKind::Odp => parse_odp(bytes).map(OfficePreview::Presentation),
    }
}

fn read_zip_text(archive: &mut ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Option<String>, String> {
    let mut entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(_) => return Err("Could not read this Office document.".to_owned()),
    };
    let mut buf = Vec::new();
    let mut limited = entry.by_ref().take((MAX_XML_ENTRY_BYTES + 1) as u64);
    limited.read_to_end(&mut buf).map_err(|_| "Could not read this Office document.".to_owned())?;
    if buf.len() > MAX_XML_ENTRY_BYTES {
        buf.truncate(MAX_XML_ENTRY_BYTES);
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn decode_xml_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';') {
            let entity = &after[..semi];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16).ok().and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => entity[1..].parse::<u32>().ok().and_then(char::from_u32),
                _ => None,
            };
            if let Some(ch) = decoded {
                out.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn xml_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let patterns = [format!(" {name}=\""), format!(" {name}='"), format!("\t{name}=\"")];
    for pat in patterns {
        if let Some(idx) = tag.find(&pat) {
            let quote = pat.as_bytes()[pat.len() - 1] as char;
            let rest = &tag[idx + pat.len()..];
            if let Some(end) = rest.find(quote) {
                return Some(&rest[..end]);
            }
        }
    }
    None
}

fn collect_xml_tag_text(xml: &str, tag_name: &str) -> String {
    let open_prefix = format!("<{tag_name}");
    let close_tag = format!("</{tag_name}>");
    let mut out = String::new();
    let mut cursor = 0;
    while let Some(pos) = xml[cursor..].find(&open_prefix) {
        let start = cursor + pos;
        let after_name = start + open_prefix.len();
        let next_ch = xml[after_name..].chars().next();
        if !matches!(next_ch, Some('>' | ' ' | '\t' | '\n' | '\r' | '/')) {
            cursor = after_name;
            continue;
        }
        let Some(gt_rel) = xml[after_name..].find('>') else { break };
        let gt = after_name + gt_rel;
        if xml[..gt].ends_with('/') {
            cursor = gt + 1;
            continue;
        }
        let Some(end_rel) = xml[gt + 1..].find(&close_tag) else { break };
        let content = &xml[gt + 1..gt + 1 + end_rel];
        out.push_str(&decode_xml_entities(content));
        cursor = gt + 1 + end_rel + close_tag.len();
    }
    out
}

fn parse_xlsx(bytes: &[u8]) -> Result<OfficeWorkbook, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "Could not open this spreadsheet archive.".to_owned())?;
    let shared_strings = read_zip_text(&mut archive, "xl/sharedStrings.xml")?
        .map(|xml| parse_shared_strings(&xml))
        .unwrap_or_default();
    let rels = read_zip_text(&mut archive, "xl/_rels/workbook.xml.rels")?
        .map(|xml| parse_rels(&xml))
        .unwrap_or_default();
    let sheet_specs = read_zip_text(&mut archive, "xl/workbook.xml")?
        .map(|xml| parse_workbook_sheets(&xml, &rels))
        .unwrap_or_default();

    let mut sheets = Vec::new();
    let mut truncated = false;
    let specs: Vec<(String, String)> = if sheet_specs.is_empty() {
        let mut names: Vec<String> = (0..archive.len())
            .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_owned()))
            .filter(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
            .collect();
        names.sort();
        names
            .into_iter()
            .enumerate()
            .map(|(i, path)| (format!("Sheet {}", i + 1), path))
            .collect()
    } else {
        sheet_specs
    };

    for (name, path) in specs {
        let normalized = path.trim_start_matches('/');
        let full_path = if normalized.starts_with("xl/") {
            normalized.to_owned()
        } else {
            format!("xl/{normalized}")
        };
        let Some(xml) = read_zip_text(&mut archive, &full_path)? else { continue };
        let sheet = parse_xlsx_sheet(name, &xml, &shared_strings);
        truncated |= sheet.truncated;
        sheets.push(sheet);
    }

    if sheets.is_empty() {
        return Err("This workbook has no visible sheets.".to_owned());
    }
    Ok(OfficeWorkbook { sheets, truncated })
}

fn parse_shared_strings(xml: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut cursor = 0;
    while let Some(pos) = xml[cursor..].find("<si") {
        let start = cursor + pos;
        let Some(gt_rel) = xml[start..].find('>') else { break };
        let body_start = start + gt_rel + 1;
        let Some(end_rel) = xml[body_start..].find("</si>") else { break };
        let si_body = &xml[body_start..body_start + end_rel];
        strings.push(collect_xml_tag_text(si_body, "t"));
        cursor = body_start + end_rel + "</si>".len();
    }
    strings
}

fn parse_rels(xml: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut cursor = 0;
    while let Some(pos) = xml[cursor..].find("<Relationship ") {
        let start = cursor + pos;
        let Some(end_rel) = xml[start..].find('>') else { break };
        let tag = &xml[start..start + end_rel + 1];
        if let (Some(id), Some(target)) = (xml_attr(tag, "Id"), xml_attr(tag, "Target")) {
            map.insert(id.to_owned(), target.to_owned());
        }
        cursor = start + end_rel + 1;
    }
    map
}

fn parse_workbook_sheets(xml: &str, rels: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(pos) = xml[cursor..].find("<sheet ") {
        let start = cursor + pos;
        let Some(end_rel) = xml[start..].find('>') else { break };
        let tag = &xml[start..start + end_rel + 1];
        cursor = start + end_rel + 1;
        if matches!(xml_attr(tag, "state"), Some("hidden" | "veryHidden")) {
            continue;
        }
        let name = xml_attr(tag, "name").map(decode_xml_entities).unwrap_or_else(|| format!("Sheet {}", out.len() + 1));
        let rid = xml_attr(tag, "r:id").or_else(|| xml_attr(tag, "id"));
        if let Some(target) = rid.and_then(|id| rels.get(id)) {
            out.push((name, target.clone()));
        }
    }
    out
}

fn parse_xlsx_sheet(name: String, xml: &str, shared_strings: &[String]) -> OfficeSheet {
    let mut show_gridlines = true;
    let mut frozen_rows = 0usize;
    let mut frozen_cols = 0usize;

    if let Some(sv_pos) = xml.find("<sheetView ")
        && let Some(gt) = xml[sv_pos..].find('>')
    {
        let tag = &xml[sv_pos..sv_pos + gt + 1];
        if xml_attr(tag, "showGridLines") == Some("0") {
            show_gridlines = false;
        }
    }
    if let Some(pane_pos) = xml.find("<pane ")
        && let Some(gt) = xml[pane_pos..].find('>')
    {
        let tag = &xml[pane_pos..pane_pos + gt + 1];
        if matches!(xml_attr(tag, "state"), Some("frozen" | "frozenSplit")) {
            frozen_rows = xml_attr(tag, "ySplit").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0).max(0.0) as usize;
            frozen_cols = xml_attr(tag, "xSplit").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0).max(0.0) as usize;
        }
    }

    // Parse `<cols><col min="1" max="3" width="18"/>...</cols>`.
    let mut col_width_overrides: HashMap<usize, f32> = HashMap::new();
    let mut col_cursor = 0;
    while let Some(pos) = xml[col_cursor..].find("<col ") {
        let start = col_cursor + pos;
        let Some(gt) = xml[start..].find('>') else { break };
        let tag = &xml[start..start + gt + 1];
        col_cursor = start + gt + 1;
        if let (Some(min), Some(max), Some(width)) = (
            xml_attr(tag, "min").and_then(|v| v.parse::<usize>().ok()),
            xml_attr(tag, "max").and_then(|v| v.parse::<usize>().ok()),
            xml_attr(tag, "width").and_then(|v| v.parse::<f32>().ok()),
        ) && width.is_finite()
        {
            let px = (width * 7.0 + 5.0).clamp(MINIMUM_TRACK_SIZE, MAXIMUM_COL_WIDTH);
            let lo = min.saturating_sub(1);
            let hi = max.saturating_sub(1).min(OFFICE_PREVIEW_MAX_COLUMNS - 1);
            for col in lo..=hi {
                col_width_overrides.insert(col, px);
            }
        }
    }

    type SparseRow = (usize, Option<f32>, Vec<(usize, String)>);
    let mut sparse_rows: Vec<SparseRow> = Vec::new();
    let mut max_row = 0usize;
    let mut max_col = 0usize;
    let mut total_cells = 0usize;
    let mut truncated = false;

    let mut row_cursor = 0;
    let mut implicit_row = 0usize;
    while let Some(pos) = xml[row_cursor..].find("<row") {
        let start = row_cursor + pos;
        let after_name = start + 4;
        if !matches!(xml[after_name..].chars().next(), Some('>' | ' ' | '\t' | '\n' | '\r' | '/')) {
            row_cursor = after_name;
            continue;
        }
        let Some(gt_rel) = xml[start..].find('>') else { break };
        let tag = &xml[start..start + gt_rel + 1];
        let self_closing = tag.ends_with("/>");
        let row_idx = xml_attr(tag, "r")
            .and_then(|v| v.parse::<usize>().ok())
            .and_then(|r| r.checked_sub(1))
            .unwrap_or(implicit_row);
        implicit_row = row_idx + 1;
        let row_ht = xml_attr(tag, "ht")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|h| h.is_finite() && *h > 0.0)
            .map(|pt| (pt * 4.0 / 3.0).clamp(MINIMUM_TRACK_SIZE, MAXIMUM_ROW_HEIGHT));

        if self_closing {
            row_cursor = start + gt_rel + 1;
            continue;
        }
        let body_start = start + gt_rel + 1;
        let Some(end_rel) = xml[body_start..].find("</row>") else { break };
        let row_body = &xml[body_start..body_start + end_rel];
        row_cursor = body_start + end_rel + "</row>".len();

        if row_idx >= OFFICE_PREVIEW_MAX_ROWS {
            truncated = true;
            break;
        }

        let mut cells = Vec::new();
        let mut cell_cursor = 0;
        let mut implicit_col = 0usize;
        while let Some(c_pos) = row_body[cell_cursor..].find("<c") {
            let c_start = cell_cursor + c_pos;
            let c_after = c_start + 2;
            if !matches!(row_body[c_after..].chars().next(), Some('>' | ' ' | '\t' | '\n' | '\r' | '/')) {
                cell_cursor = c_after;
                continue;
            }
            let Some(c_gt) = row_body[c_start..].find('>') else { break };
            let c_tag = &row_body[c_start..c_start + c_gt + 1];
            let col_idx = xml_attr(c_tag, "r")
                .and_then(parse_cell_ref)
                .map(|(_, c)| c)
                .unwrap_or(implicit_col);
            implicit_col = col_idx + 1;
            let cell_type = xml_attr(c_tag, "t").unwrap_or("");

            if c_tag.ends_with("/>") {
                cell_cursor = c_start + c_gt + 1;
                continue;
            }
            let c_body_start = c_start + c_gt + 1;
            let Some(c_end) = row_body[c_body_start..].find("</c>") else { break };
            let c_body = &row_body[c_body_start..c_body_start + c_end];
            cell_cursor = c_body_start + c_end + "</c>".len();

            if col_idx >= OFFICE_PREVIEW_MAX_COLUMNS {
                truncated = true;
                continue;
            }
            let value = match cell_type {
                "s" => {
                    let raw = collect_xml_tag_text(c_body, "v");
                    raw.trim()
                        .parse::<usize>()
                        .ok()
                        .and_then(|idx| shared_strings.get(idx))
                        .cloned()
                        .unwrap_or_default()
                }
                "inlineStr" => collect_xml_tag_text(c_body, "t"),
                "b" => {
                    let raw = collect_xml_tag_text(c_body, "v");
                    if raw.trim() == "1" { "TRUE".to_owned() } else { "FALSE".to_owned() }
                }
                _ => collect_xml_tag_text(c_body, "v"),
            };
            if !value.is_empty() {
                max_row = max_row.max(row_idx + 1);
                max_col = max_col.max(col_idx + 1);
                cells.push((col_idx, value));
                total_cells += 1;
                if total_cells >= MAX_CELLS_PER_SHEET {
                    truncated = true;
                    break;
                }
            }
        }
        if !cells.is_empty() || row_ht.is_some() {
            max_row = max_row.max(row_idx + 1);
            sparse_rows.push((row_idx, row_ht, cells));
        }
        if total_cells >= MAX_CELLS_PER_SHEET {
            break;
        }
    }

    let row_count = max_row.max(1);
    let col_count = max_col.max(1);
    let mut rows = vec![vec![String::new(); col_count]; row_count];
    let mut row_heights = vec![DEFAULT_ROW_HEIGHT; row_count];
    for (r, ht, cells) in sparse_rows {
        if r < row_count {
            if let Some(h) = ht {
                row_heights[r] = h;
            }
            for (c, val) in cells {
                if c < col_count {
                    rows[r][c] = val;
                }
            }
        }
    }
    let col_widths = (0..col_count)
        .map(|c| col_width_overrides.get(&c).copied().unwrap_or(DEFAULT_COL_WIDTH))
        .collect();

    OfficeSheet {
        name,
        rows,
        column_count: col_count,
        col_widths,
        row_heights,
        frozen_rows,
        frozen_cols,
        show_gridlines,
        truncated,
    }
}

fn parse_ods(bytes: &[u8]) -> Result<OfficeWorkbook, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "Could not open this spreadsheet archive.".to_owned())?;
    let Some(xml) = read_zip_text(&mut archive, "content.xml")? else {
        return Err("This spreadsheet has no content.xml.".to_owned());
    };
    let mut sheets = Vec::new();
    let mut truncated = false;
    let mut cursor = 0;
    while let Some(pos) = xml[cursor..].find("<table:table ") {
        let start = cursor + pos;
        let Some(gt) = xml[start..].find('>') else { break };
        let tag = &xml[start..start + gt + 1];
        let name = xml_attr(tag, "table:name")
            .map(decode_xml_entities)
            .unwrap_or_else(|| format!("Sheet {}", sheets.len() + 1));
        let body_start = start + gt + 1;
        let Some(end) = xml[body_start..].find("</table:table>") else { break };
        let body = &xml[body_start..body_start + end];
        cursor = body_start + end + "</table:table>".len();

        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut max_col = 1usize;
        let mut sheet_truncated = false;
        let mut r_cur = 0;
        while let Some(r_pos) = body[r_cur..].find("<table:table-row") {
            let r_start = r_cur + r_pos;
            let Some(r_gt) = body[r_start..].find('>') else { break };
            let r_tag = &body[r_start..r_start + r_gt + 1];
            let repeat_rows = xml_attr(r_tag, "table:number-rows-repeated")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1)
                .min(256);
            if r_tag.ends_with("/>") {
                r_cur = r_start + r_gt + 1;
                continue;
            }
            let rb_start = r_start + r_gt + 1;
            let Some(r_end) = body[rb_start..].find("</table:table-row>") else { break };
            let r_body = &body[rb_start..rb_start + r_end];
            r_cur = rb_start + r_end + "</table:table-row>".len();

            let mut row_cells: Vec<String> = Vec::new();
            let mut c_cur = 0;
            while let Some(c_pos) = r_body[c_cur..].find("<table:table-cell") {
                let c_start = c_cur + c_pos;
                let Some(c_gt) = r_body[c_start..].find('>') else { break };
                let c_tag = &r_body[c_start..c_start + c_gt + 1];
                let repeat_cols = xml_attr(c_tag, "table:number-columns-repeated")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1)
                    .min(OFFICE_PREVIEW_MAX_COLUMNS.saturating_sub(row_cells.len()));
                let val = if c_tag.ends_with("/>") {
                    c_cur = c_start + c_gt + 1;
                    String::new()
                } else {
                    let cb_start = c_start + c_gt + 1;
                    let Some(c_end) = r_body[cb_start..].find("</table:table-cell>") else { break };
                    let c_body = &r_body[cb_start..cb_start + c_end];
                    c_cur = cb_start + c_end + "</table:table-cell>".len();
                    collect_xml_tag_text(c_body, "text:p")
                };
                if val.is_empty() && repeat_cols > 32 {
                    break;
                }
                for _ in 0..repeat_cols {
                    if row_cells.len() >= OFFICE_PREVIEW_MAX_COLUMNS {
                        sheet_truncated = true;
                        break;
                    }
                    row_cells.push(val.clone());
                }
            }
            while row_cells.last().is_some_and(String::is_empty) {
                row_cells.pop();
            }
            if row_cells.is_empty() && repeat_rows > 16 {
                continue;
            }
            max_col = max_col.max(row_cells.len());
            for _ in 0..repeat_rows {
                if rows.len() >= OFFICE_PREVIEW_MAX_ROWS {
                    sheet_truncated = true;
                    break;
                }
                rows.push(row_cells.clone());
            }
        }
        if rows.is_empty() {
            rows.push(vec![String::new(); max_col]);
        } else {
            for row in &mut rows {
                row.resize(max_col, String::new());
            }
        }
        let row_heights = vec![DEFAULT_ROW_HEIGHT; rows.len()];
        let col_widths = vec![DEFAULT_COL_WIDTH; max_col];
        truncated |= sheet_truncated;
        sheets.push(OfficeSheet {
            name,
            rows,
            column_count: max_col,
            col_widths,
            row_heights,
            frozen_rows: 0,
            frozen_cols: 0,
            show_gridlines: true,
            truncated: sheet_truncated,
        });
    }
    if sheets.is_empty() {
        return Err("This workbook has no visible sheets.".to_owned());
    }
    Ok(OfficeWorkbook { sheets, truncated })
}

fn parse_docx(bytes: &[u8]) -> Result<OfficeDocument, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "Could not open this Word document.".to_owned())?;
    let Some(xml) = read_zip_text(&mut archive, "word/document.xml")? else {
        return Err("This Word document has no readable body.".to_owned());
    };
    let mut blocks = Vec::new();
    let mut truncated = false;
    let mut cursor = 0;

    // Walk top-level paragraphs and tables in document order.
    while cursor < xml.len() {
        let next_p = xml[cursor..].find("<w:p");
        let next_tbl = xml[cursor..].find("<w:tbl");
        match (next_p, next_tbl) {
            (None, None) => break,
            (Some(p), Some(t)) if t < p => {
                let start = cursor + t;
                let Some(end) = xml[start..].find("</w:tbl>") else { break };
                let tbl_xml = &xml[start..start + end];
                let table = parse_docx_table(tbl_xml);
                if !table.is_empty() {
                    blocks.push(DocumentBlock::Table(table));
                }
                cursor = start + end + "</w:tbl>".len();
            }
            (None, Some(t)) => {
                let start = cursor + t;
                let Some(end) = xml[start..].find("</w:tbl>") else { break };
                let tbl_xml = &xml[start..start + end];
                let table = parse_docx_table(tbl_xml);
                if !table.is_empty() {
                    blocks.push(DocumentBlock::Table(table));
                }
                cursor = start + end + "</w:tbl>".len();
            }
            (Some(p), _) => {
                let start = cursor + p;
                let after = start + 4;
                if !matches!(xml[after..].chars().next(), Some('>' | ' ' | '\t' | '\n' | '\r' | '/')) {
                    cursor = after;
                    continue;
                }
                let Some(gt) = xml[start..].find('>') else { break };
                if xml[start..start + gt + 1].ends_with("/>") {
                    cursor = start + gt + 1;
                    continue;
                }
                let body_start = start + gt + 1;
                let Some(end) = xml[body_start..].find("</w:p>") else { break };
                let p_xml = &xml[body_start..body_start + end];
                cursor = body_start + end + "</w:p>".len();

                let text = collect_xml_tag_text(p_xml, "w:t").trim().to_owned();
                if text.is_empty() {
                    continue;
                }
                let heading = extract_docx_heading_level(p_xml);
                match heading {
                    Some(level) => blocks.push(DocumentBlock::Heading { level, text }),
                    None => blocks.push(DocumentBlock::Paragraph(text)),
                }
            }
        }
        if blocks.len() >= MAX_PARAGRAPHS {
            truncated = true;
            break;
        }
    }

    Ok(OfficeDocument {
        chapters: vec![DocumentChapter { title: None, blocks }],
        truncated,
    })
}

fn extract_docx_heading_level(p_xml: &str) -> Option<u8> {
    let idx = p_xml.find("<w:pStyle")?;
    let gt = p_xml[idx..].find('>')?;
    let tag = &p_xml[idx..idx + gt + 1];
    let style = xml_attr(tag, "w:val")?;
    if style.eq_ignore_ascii_case("Title") {
        return Some(1);
    }
    let lower = style.to_ascii_lowercase();
    lower
        .strip_prefix("heading")
        .and_then(|rest| rest.trim().parse::<u8>().ok())
        .map(|lvl| lvl.clamp(1, 6))
}

fn parse_docx_table(tbl_xml: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut r_cur = 0;
    while let Some(r_pos) = tbl_xml[r_cur..].find("<w:tr") {
        let r_start = r_cur + r_pos;
        let Some(r_end) = tbl_xml[r_start..].find("</w:tr>") else { break };
        let r_xml = &tbl_xml[r_start..r_start + r_end];
        r_cur = r_start + r_end + "</w:tr>".len();
        let mut cells = Vec::new();
        let mut c_cur = 0;
        while let Some(c_pos) = r_xml[c_cur..].find("<w:tc") {
            let c_start = c_cur + c_pos;
            let Some(c_end) = r_xml[c_start..].find("</w:tc>") else { break };
            let c_xml = &r_xml[c_start..c_start + c_end];
            c_cur = c_start + c_end + "</w:tc>".len();
            cells.push(collect_xml_tag_text(c_xml, "w:t").trim().to_owned());
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    rows
}

const ODF_SKIPPED_ELEMENTS: &[&str] = &[
    "office:annotation",
    "presentation:notes",
    "text:tracked-changes",
    "office:scripts",
    "draw:object",
    "draw:object-ole",
    "text:table-of-content-source",
    "text:illustration-index-source",
    "text:table-index-source",
    "text:object-index-source",
    "text:user-index-source",
    "text:alphabetical-index-source",
    "text:bibliography-source",
];

fn find_xml_tag_end(xml: &str, start: usize) -> Option<usize> {
    let bytes = xml.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i),
            None => {}
        }
        i += 1;
    }
    None
}

fn xml_tag_name(raw: &str) -> &str {
    let trimmed = raw.trim_start_matches('/').trim_end_matches('/').trim_start();
    trimmed
        .split([' ', '\t', '\r', '\n', '/'])
        .next()
        .unwrap_or("")
}

/// Removes ODF skipped subtrees (index/TOC/bibliography sources, scripts, embedded objects,
/// annotations, tracked changes) while leaving visible bodies intact (`pingdotgg/t3code#40509`).
fn strip_odf_skipped_elements(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut skip_stack: Vec<&'static str> = Vec::new();
    let mut cursor = 0;
    while cursor < xml.len() {
        let Some(rel) = xml[cursor..].find('<') else {
            if skip_stack.is_empty() {
                out.push_str(&xml[cursor..]);
            }
            break;
        };
        let lt = cursor + rel;
        if skip_stack.is_empty() {
            out.push_str(&xml[cursor..lt]);
        }
        if xml[lt..].starts_with("<!--") {
            let end = xml[lt + 4..].find("-->").map_or(xml.len(), |p| lt + 4 + p + 3);
            if skip_stack.is_empty() {
                out.push_str(&xml[lt..end]);
            }
            cursor = end;
            continue;
        }
        if xml[lt..].starts_with("<![CDATA[") {
            let end = xml[lt + 9..].find("]]>").map_or(xml.len(), |p| lt + 9 + p + 3);
            if skip_stack.is_empty() {
                out.push_str(&xml[lt..end]);
            }
            cursor = end;
            continue;
        }
        let Some(gt) = find_xml_tag_end(xml, lt + 1) else {
            if skip_stack.is_empty() {
                out.push_str(&xml[lt..]);
            }
            break;
        };
        let inner = &xml[lt + 1..gt];
        cursor = gt + 1;
        if let Some(closing) = inner.strip_prefix('/') {
            let name = xml_tag_name(closing);
            if skip_stack.last().copied() == Some(name) {
                skip_stack.pop();
            } else if skip_stack.is_empty() {
                out.push_str(&xml[lt..cursor]);
            }
            continue;
        }
        let self_closing = inner.trim_end().ends_with('/');
        let name = xml_tag_name(inner);
        if let Some(&matched) = ODF_SKIPPED_ELEMENTS.iter().find(|&&tag| tag == name) {
            if !self_closing {
                skip_stack.push(matched);
            }
            continue;
        }
        if skip_stack.is_empty() {
            out.push_str(&xml[lt..cursor]);
        }
    }
    out
}

fn find_next_odf_text_block(xml: &str, mut cursor: usize) -> Option<(bool, usize)> {
    while cursor < xml.len() {
        let next_h = xml[cursor..].find("<text:h").map(|rel| (true, cursor + rel));
        let next_p = xml[cursor..].find("<text:p").map(|rel| (false, cursor + rel));
        let (is_heading, start) = match (next_h, next_p) {
            (None, None) => return None,
            (Some(h), Some(p)) if h.1 < p.1 => h,
            (Some(h), None) => h,
            (_, Some(p)) => p,
        };
        let after = start + "<text:p".len();
        if matches!(xml[after..].chars().next(), Some('>' | ' ' | '\t' | '\n' | '\r' | '/')) {
            return Some((is_heading, start));
        }
        cursor = after;
    }
    None
}

fn odf_inline_text(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut cursor = 0;
    while cursor < xml.len() {
        let Some(rel) = xml[cursor..].find('<') else {
            out.push_str(&decode_xml_entities(&xml[cursor..]));
            break;
        };
        let lt = cursor + rel;
        if lt > cursor {
            out.push_str(&decode_xml_entities(&xml[cursor..lt]));
        }
        let Some(gt) = find_xml_tag_end(xml, lt + 1) else {
            break;
        };
        let tag = &xml[lt..=gt];
        let name = xml_tag_name(&xml[lt + 1..gt]);
        match name {
            "text:s" => {
                let count = xml_attr(tag, "text:c")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 100);
                for _ in 0..count {
                    out.push(' ');
                }
            }
            "text:tab" => out.push('\t'),
            "text:line-break" => out.push('\n'),
            _ => {}
        }
        cursor = gt + 1;
    }
    out
}

fn parse_odt(bytes: &[u8]) -> Result<OfficeDocument, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "Could not open this OpenDocument file.".to_owned())?;
    let Some(raw_xml) = read_zip_text(&mut archive, "content.xml")? else {
        return Err("This document has no content.xml.".to_owned());
    };
    let xml = strip_odf_skipped_elements(&raw_xml);
    let mut blocks = Vec::new();
    let mut truncated = false;
    let mut cursor = 0;
    while let Some((is_heading, start)) = find_next_odf_text_block(&xml, cursor) {
        let close = if is_heading { "</text:h>" } else { "</text:p>" };
        let Some(gt) = find_xml_tag_end(&xml, start + 1) else { break };
        let tag = &xml[start..=gt];
        if tag.ends_with("/>") {
            cursor = gt + 1;
            continue;
        }
        let body_start = gt + 1;
        let Some(end) = xml[body_start..].find(close) else { break };
        let inner = &xml[body_start..body_start + end];
        cursor = body_start + end + close.len();
        let text = odf_inline_text(inner).trim().to_owned();
        if text.is_empty() {
            continue;
        }
        if is_heading {
            let level = xml_attr(tag, "text:outline-level")
                .and_then(|v| v.parse::<u8>().ok())
                .unwrap_or(1)
                .clamp(1, 6);
            blocks.push(DocumentBlock::Heading { level, text });
        } else {
            blocks.push(DocumentBlock::Paragraph(text));
        }
        if blocks.len() >= MAX_PARAGRAPHS {
            truncated = true;
            break;
        }
    }
    Ok(OfficeDocument {
        chapters: vec![DocumentChapter { title: None, blocks }],
        truncated,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RtfEncoding {
    Windows1252,
    MacRoman,
    Utf8,
}

/// WHATWG / IANA `windows-1252` upper table (`0x80..=0x9F`); `0xA0..=0xFF` map directly.
const WIN1252_HIGH: [char; 32] = [
    '€', '\u{FFFD}', '‚', 'ƒ', '„', '…', '†', '‡',
    'ˆ', '‰', 'Š', '‹', 'Œ', '\u{FFFD}', 'Ž', '\u{FFFD}',
    '\u{FFFD}', '‘', '’', '“', '”', '•', '–', '—',
    '˜', '™', 'š', '›', 'œ', '\u{FFFD}', 'ž', 'Ÿ',
];

/// WHATWG / IANA `macintosh` (Mac Roman) upper table (`0x80..=0xFF`, `pingdotgg/t3code#39846`).
const MAC_ROMAN_HIGH: [char; 128] = [
    'Ä', 'Å', 'Ç', 'É', 'Ñ', 'Ö', 'Ü', 'á',
    'à', 'â', 'ä', 'ã', 'å', 'ç', 'é', 'è',
    'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ñ', 'ó',
    'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü',
    '†', '°', '¢', '£', '§', '•', '¶', 'ß',
    '®', '©', '™', '´', '¨', '≠', 'Æ', 'Ø',
    '∞', '±', '≤', '≥', '¥', 'µ', '∂', '∑',
    '∏', 'π', '∫', 'ª', 'º', 'Ω', 'æ', 'ø',
    '¿', '¡', '¬', '√', 'ƒ', '≈', '∆', '«',
    '»', '…', '\u{00A0}', 'À', 'Ã', 'Õ', 'Œ', 'œ',
    '–', '—', '“', '”', '‘', '’', '÷', '◊',
    'ÿ', 'Ÿ', '⁄', '€', '‹', '›', 'ﬁ', 'ﬂ',
    '‡', '·', '‚', '„', '‰', 'Â', 'Ê', 'Á',
    'Ë', 'È', 'Í', 'Î', 'Ï', 'Ì', 'Ó', 'Ô',
    '\u{F8FF}', 'Ò', 'Ú', 'Û', 'Ù', 'ı', 'ˆ', '˜',
    '¯', '˘', '˙', '˚', '¸', '˝', '˛', 'ˇ',
];

fn decode_rtf_bytes(bytes: &[u8], encoding: RtfEncoding) -> String {
    if encoding == RtfEncoding::Utf8 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    bytes
        .iter()
        .map(|&b| match encoding {
            RtfEncoding::Windows1252 => match b {
                0x80..=0x9F => WIN1252_HIGH[(b - 0x80) as usize],
                other => other as char,
            },
            RtfEncoding::MacRoman => match b {
                0x80..=0xFF => MAC_ROMAN_HIGH[(b - 0x80) as usize],
                other => other as char,
            },
            RtfEncoding::Utf8 => unreachable!(),
        })
        .collect()
}

#[derive(Clone, Copy)]
struct RtfState {
    skip: bool,
    deleted: bool,
    hidden_text: bool,
    uc: usize,
    font: i32,
    font_table: bool,
    encoding: RtfEncoding,
}

impl RtfState {
    fn is_hidden(self) -> bool {
        self.skip || self.deleted || self.hidden_text
    }
}

fn rtf_special_char(word: &str) -> Option<&'static str> {
    Some(match word {
        "tab" => "\t",
        "endash" => "–",
        "emdash" => "—",
        "bullet" => "•",
        "lquote" => "‘",
        "rquote" => "’",
        "ldblquote" => "“",
        "rdblquote" => "”",
        "enspace" => "\u{2002}",
        "emspace" => "\u{2003}",
        "qmspace" => "\u{2005}",
        "zwbo" => "\u{200B}",
        "zwnj" => "\u{200C}",
        "zwj" => "\u{200D}",
        "ltrmark" => "\u{200E}",
        "rtlmark" => "\u{200F}",
        _ => return None,
    })
}

fn is_rtf_skipped_destination(word: &str) -> bool {
    matches!(
        word,
        "colortbl"
            | "stylesheet"
            | "info"
            | "pict"
            | "object"
            | "fldinst"
            | "header"
            | "headerl"
            | "headerr"
            | "headerf"
            | "footer"
            | "footerl"
            | "footerr"
            | "footerf"
            | "filetbl"
            | "datastore"
            | "themedata"
    )
}

fn parse_rtf(bytes: &[u8]) -> Result<OfficeDocument, String> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("This RTF document is too large to preview.".to_owned());
    }
    let trimmed_start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    if !bytes[trimmed_start..].starts_with(b"{\\rtf") {
        return Err("Not a valid RTF document.".to_owned());
    }

    let mut state = RtfState {
        skip: false,
        deleted: false,
        hidden_text: false,
        uc: 1,
        font: 0,
        font_table: false,
        encoding: RtfEncoding::Windows1252,
    };
    let mut fonts: HashMap<i32, RtfEncoding> = HashMap::new();
    let mut default_font = 0i32;
    let mut stack: Vec<RtfState> = Vec::new();
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut encoded: Vec<u8> = Vec::new();
    let mut fallback = 0usize;
    let mut pending_high_surrogate: Option<u16> = None;
    let mut truncated = false;

    let flush_encoded = |encoded: &mut Vec<u8>, state: RtfState, fonts: &HashMap<i32, RtfEncoding>, current: &mut String| {
        if encoded.is_empty() {
            return;
        }
        let enc = fonts.get(&state.font).copied().unwrap_or(state.encoding);
        current.push_str(&decode_rtf_bytes(encoded, enc));
        encoded.clear();
    };
    let push_paragraph = |blocks: &mut Vec<DocumentBlock>, current: &mut String, truncated: &mut bool| {
        let trimmed = current.trim().to_owned();
        if !trimmed.is_empty() {
            if blocks.len() >= MAX_PARAGRAPHS {
                *truncated = true;
            } else {
                blocks.push(DocumentBlock::Paragraph(trimmed));
            }
        }
        current.clear();
    };

    let mut i = trimmed_start;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        match b {
            b'{' => {
                flush_encoded(&mut encoded, state, &fonts, &mut current);
                fallback = 0;
                pending_high_surrogate = None;
                if stack.len() > 100 {
                    return Err("This RTF document is too deeply nested.".to_owned());
                }
                stack.push(state);
            }
            b'}' => {
                flush_encoded(&mut encoded, state, &fonts, &mut current);
                fallback = 0;
                pending_high_surrogate = None;
                let Some(parent) = stack.pop() else {
                    return Err("Not a valid RTF document.".to_owned());
                };
                state = parent;
            }
            b'\\' => {
                let Some(&next) = bytes.get(i) else {
                    break;
                };
                if next == b'\'' {
                    let Some(hex) = bytes.get(i + 1..i + 3).and_then(|s| std::str::from_utf8(s).ok()) else {
                        return Err("Not a valid RTF document.".to_owned());
                    };
                    let Ok(byte) = u8::from_str_radix(hex, 16) else {
                        return Err("Not a valid RTF document.".to_owned());
                    };
                    i += 3;
                    if fallback > 0 {
                        fallback -= 1;
                    } else if !state.is_hidden() {
                        pending_high_surrogate = None;
                        encoded.push(byte);
                    }
                    continue;
                }
                flush_encoded(&mut encoded, state, &fonts, &mut current);
                if matches!(next, b'\\' | b'{' | b'}') {
                    i += 1;
                    if fallback > 0 {
                        fallback -= 1;
                    } else if !state.is_hidden() {
                        pending_high_surrogate = None;
                        current.push(next as char);
                    }
                    continue;
                }
                if next == b'*' {
                    state.skip = true;
                    i += 1;
                    continue;
                }
                if next.is_ascii_alphabetic() {
                    let word_start = i;
                    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    let word = std::str::from_utf8(&bytes[word_start..i]).unwrap_or("");
                    let arg = if i < bytes.len() && (bytes[i] == b'-' || bytes[i].is_ascii_digit()) {
                        let arg_start = i;
                        if bytes[i] == b'-' {
                            i += 1;
                        }
                        while i < bytes.len() && bytes[i].is_ascii_digit() {
                            i += 1;
                        }
                        std::str::from_utf8(&bytes[arg_start..i])
                            .ok()
                            .and_then(|s| s.parse::<i32>().ok())
                            .unwrap_or(1)
                    } else {
                        1
                    };
                    if i < bytes.len() && bytes[i] == b' ' {
                        i += 1;
                    }
                    if word == "bin" {
                        if arg < 0 || (arg as usize) > bytes.len().saturating_sub(i) {
                            return Err("Not a valid RTF document.".to_owned());
                        }
                        i += arg as usize;
                    }
                    if fallback > 0 {
                        fallback -= 1;
                        continue;
                    }
                    match word {
                        "fonttbl" => {
                            state.font_table = true;
                            state.skip = true;
                        }
                        "deff" => {
                            default_font = arg;
                            state.font = arg;
                        }
                        "f" => state.font = arg,
                        "fcharset" if state.font_table => {
                            let enc = match arg {
                                0 | 1 => Some(state.encoding),
                                77 => Some(RtfEncoding::MacRoman),
                                _ => None,
                            };
                            if let Some(enc) = enc {
                                fonts.insert(state.font, enc);
                            }
                        }
                        w if is_rtf_skipped_destination(w) => state.skip = true,
                        "mac" => state.encoding = RtfEncoding::MacRoman,
                        "ansicpg" => {
                            state.encoding = match arg {
                                10000 => RtfEncoding::MacRoman,
                                65001 => RtfEncoding::Utf8,
                                _ => RtfEncoding::Windows1252,
                            };
                        }
                        "deleted" => state.deleted = arg != 0,
                        "v" => state.hidden_text = arg != 0,
                        "plain" => {
                            state.deleted = false;
                            state.hidden_text = false;
                            state.font = default_font;
                        }
                        "uc" => state.uc = arg.clamp(0, 10) as usize,
                        "u" => {
                            let code = if arg < 0 {
                                (arg + 65536) as u32
                            } else {
                                arg as u32
                            };
                            if code == 0x2028 {
                                pending_high_surrogate = None;
                                if !state.is_hidden() {
                                    push_paragraph(&mut blocks, &mut current, &mut truncated);
                                }
                            } else if (0xD800..=0xDBFF).contains(&code) {
                                pending_high_surrogate = Some(code as u16);
                            } else if (0xDC00..=0xDFFF).contains(&code)
                                && let Some(hi) = pending_high_surrogate.take()
                            {
                                let cp = 0x10000 + (((hi as u32) - 0xD800) << 10) + (code - 0xDC00);
                                if !state.is_hidden()
                                    && let Some(ch) = char::from_u32(cp)
                                {
                                    current.push(ch);
                                }
                            } else {
                                pending_high_surrogate = None;
                                if !state.is_hidden()
                                    && let Some(ch) = char::from_u32(code)
                                {
                                    current.push(ch);
                                }
                            }
                            fallback = state.uc;
                        }
                        "page" | "par" | "line" if !state.is_hidden() => {
                            pending_high_surrogate = None;
                            push_paragraph(&mut blocks, &mut current, &mut truncated);
                        }
                        "row" if !state.skip => {
                            pending_high_surrogate = None;
                            push_paragraph(&mut blocks, &mut current, &mut truncated);
                        }
                        "cell" if !state.skip => {
                            pending_high_surrogate = None;
                            current.push('\t');
                        }
                        "sect" | "column" if !state.is_hidden() => {
                            pending_high_surrogate = None;
                            if !current.trim().is_empty() {
                                push_paragraph(&mut blocks, &mut current, &mut truncated);
                            }
                        }
                        other => {
                            if let Some(ch) = rtf_special_char(other)
                                && !state.is_hidden()
                            {
                                pending_high_surrogate = None;
                                current.push_str(ch);
                            }
                        }
                    }
                } else {
                    i += 1;
                    let symbol = match next {
                        b'~' => Some('\u{00A0}'),
                        b'_' => Some('\u{2011}'),
                        b'-' => Some('\u{00AD}'),
                        _ => None,
                    };
                    if fallback > 0 {
                        fallback -= 1;
                    } else if let Some(ch) = symbol {
                        if !state.is_hidden() {
                            pending_high_surrogate = None;
                            current.push(ch);
                        }
                    } else if matches!(next, b'\n' | b'\r') && !state.is_hidden() {
                        if next == b'\r' && bytes.get(i) == Some(&b'\n') {
                            i += 1;
                        }
                        pending_high_surrogate = None;
                        push_paragraph(&mut blocks, &mut current, &mut truncated);
                    }
                }
            }
            b'\r' | b'\n' => {}
            other => {
                if fallback > 0 {
                    fallback -= 1;
                } else if !state.is_hidden() {
                    pending_high_surrogate = None;
                    encoded.push(other);
                }
            }
        }
        if truncated {
            break;
        }
    }
    if !truncated && !stack.is_empty() {
        return Err("Not a valid RTF document.".to_owned());
    }
    flush_encoded(&mut encoded, state, &fonts, &mut current);
    push_paragraph(&mut blocks, &mut current, &mut truncated);
    Ok(OfficeDocument {
        chapters: vec![DocumentChapter { title: None, blocks }],
        truncated,
    })
}

fn parse_epub(bytes: &[u8]) -> Result<OfficeDocument, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "Could not open this EPUB archive.".to_owned())?;
    let mut html_entries: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_owned()))
        .filter(|n| {
            let lower = n.to_ascii_lowercase();
            (lower.ends_with(".xhtml") || lower.ends_with(".html") || lower.ends_with(".htm"))
                && !lower.contains("nav.")
                && !lower.contains("toc.")
        })
        .collect();
    html_entries.sort();

    let mut chapters = Vec::new();
    let mut truncated = false;
    for (idx, path) in html_entries.into_iter().enumerate() {
        if chapters.len() >= 100 {
            truncated = true;
            break;
        }
        let Some(html) = read_zip_text(&mut archive, &path)? else { continue };
        let title = {
            let t = collect_xml_tag_text(&html, "title").trim().to_owned();
            if t.is_empty() { Some(format!("Chapter {}", idx + 1)) } else { Some(t) }
        };
        let body = html
            .find("<body")
            .and_then(|s| html[s..].find('>').map(|gt| &html[s + gt + 1..]))
            .unwrap_or(&html);
        let stripped = strip_xml_tags(body);
        let blocks: Vec<DocumentBlock> = stripped
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(500)
            .map(|l| DocumentBlock::Paragraph(l.to_owned()))
            .collect();
        if !blocks.is_empty() {
            chapters.push(DocumentChapter { title, blocks });
        }
    }
    if chapters.is_empty() {
        return Err("This EPUB has no readable chapters.".to_owned());
    }
    Ok(OfficeDocument { chapters, truncated })
}

fn parse_pptx(bytes: &[u8]) -> Result<OfficePresentation, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "Could not open this presentation archive.".to_owned())?;
    let mut slide_paths: Vec<(usize, String)> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_owned()))
        .filter_map(|name| {
            let rest = name.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?;
            let num = rest.parse::<usize>().ok()?;
            Some((num, name))
        })
        .collect();
    slide_paths.sort_by_key(|(num, _)| *num);

    let mut slides = Vec::new();
    let mut truncated = false;
    for (_, path) in slide_paths {
        if slides.len() >= MAX_SLIDES {
            truncated = true;
            break;
        }
        let Some(xml) = read_zip_text(&mut archive, &path)? else { continue };
        let mut paragraphs = Vec::new();
        let mut cursor = 0;
        while let Some(pos) = xml[cursor..].find("<a:p") {
            let start = cursor + pos;
            let after = start + 4;
            if !matches!(xml[after..].chars().next(), Some('>' | ' ' | '\t' | '\n' | '\r' | '/')) {
                cursor = after;
                continue;
            }
            let Some(gt) = xml[start..].find('>') else { break };
            if xml[start..start + gt + 1].ends_with("/>") {
                cursor = start + gt + 1;
                continue;
            }
            let body_start = start + gt + 1;
            let Some(end) = xml[body_start..].find("</a:p>") else { break };
            let p_xml = &xml[body_start..body_start + end];
            cursor = body_start + end + "</a:p>".len();
            let line = collect_xml_tag_text(p_xml, "a:t").trim().to_owned();
            if !line.is_empty() {
                paragraphs.push(line);
            }
        }
        let title = if paragraphs.is_empty() { None } else { Some(paragraphs.remove(0)) };
        slides.push(OfficeSlide { title, paragraphs });
    }
    if slides.is_empty() {
        return Err("This presentation has no slides.".to_owned());
    }
    Ok(OfficePresentation { slides, truncated })
}

fn parse_odp(bytes: &[u8]) -> Result<OfficePresentation, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "Could not open this presentation archive.".to_owned())?;
    let Some(raw_xml) = read_zip_text(&mut archive, "content.xml")? else {
        return Err("This presentation has no content.xml.".to_owned());
    };
    let xml = strip_odf_skipped_elements(&raw_xml);
    let mut slides = Vec::new();
    let mut truncated = false;
    let mut cursor = 0;
    while let Some(pos) = xml[cursor..].find("<draw:page ") {
        if slides.len() >= MAX_SLIDES {
            truncated = true;
            break;
        }
        let start = cursor + pos;
        let Some(gt) = xml[start..].find('>') else { break };
        let body_start = start + gt + 1;
        let Some(end) = xml[body_start..].find("</draw:page>") else { break };
        let page_xml = &xml[body_start..body_start + end];
        cursor = body_start + end + "</draw:page>".len();

        let mut paragraphs = Vec::new();
        let mut p_cur = 0;
        while let Some(p_pos) = page_xml[p_cur..].find("<text:p") {
            let p_start = p_cur + p_pos;
            let Some(p_gt) = page_xml[p_start..].find('>') else { break };
            let pb_start = p_start + p_gt + 1;
            let Some(p_end) = page_xml[pb_start..].find("</text:p>") else { break };
            let text = strip_xml_tags(&page_xml[pb_start..pb_start + p_end]).trim().to_owned();
            p_cur = pb_start + p_end + "</text:p>".len();
            if !text.is_empty() {
                paragraphs.push(text);
            }
        }
        let title = if paragraphs.is_empty() { None } else { Some(paragraphs.remove(0)) };
        slides.push(OfficeSlide { title, paragraphs });
    }
    if slides.is_empty() {
        return Err("This presentation has no slides.".to_owned());
    }
    Ok(OfficePresentation { slides, truncated })
}

fn strip_xml_tags(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut in_tag = false;
    for ch in xml.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if !out.ends_with('\n') && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    decode_xml_entities(&out)
}
