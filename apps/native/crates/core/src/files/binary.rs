//! Binary / Hex viewer, magic signature detection, and bounded Archive/SQLite inspection.
//! Ports `apps/web/src/components/files/BinaryViewer.tsx`, `binaryPreview.worker.ts`, and
//! `binaryPreviewProtocol.ts`, plus a bounded Hex + ASCII dump with an offset ruler.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

/// Maximum file download size for binary/archive inspection (`BINARY_PREVIEW_MAX_BYTES`).
pub const BINARY_PREVIEW_MAX_BYTES: usize = 50 * 1024 * 1024;
/// Maximum bytes indexed into hex rows so huge binaries stay fast and bounded.
pub const HEX_PREVIEW_MAX_BYTES: usize = 256 * 1024;
/// Bytes displayed per hex dump row.
pub const HEX_BYTES_PER_ROW: usize = 16;
/// Maximum entries read from an archive (`ARCHIVE_MAX_ENTRIES`).
pub const ARCHIVE_MAX_ENTRIES: usize = 2_048;
/// Maximum extracted entry size (`ARCHIVE_ENTRY_MAX_BYTES`).
pub const ARCHIVE_ENTRY_MAX_BYTES: usize = 10 * 1024 * 1024;

/// One row of a bounded Hex + ASCII dump with an offset ruler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexRow {
    pub offset: u64,
    pub offset_label: String,
    pub hex_left: String,
    pub hex_right: String,
    pub ascii: String,
    pub byte_count: usize,
}

/// Formats up to 16 bytes into a single hex + ASCII row.
pub fn format_hex_row(offset: u64, chunk: &[u8]) -> HexRow {
    let mut hex_left = String::with_capacity(23);
    let mut hex_right = String::with_capacity(23);
    let mut ascii = String::with_capacity(HEX_BYTES_PER_ROW);
    for idx in 0..HEX_BYTES_PER_ROW {
        let target = if idx < 8 { &mut hex_left } else { &mut hex_right };
        if idx % 8 > 0 {
            target.push(' ');
        }
        if let Some(&byte) = chunk.get(idx) {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            target.push(HEX[(byte >> 4) as usize] as char);
            target.push(HEX[(byte & 0x0f) as usize] as char);
            ascii.push(if (0x20..=0x7e).contains(&byte) { byte as char } else { '.' });
        } else {
            target.push_str("  ");
            ascii.push(' ');
        }
    }
    HexRow {
        offset,
        offset_label: format!("{offset:08x}"),
        hex_left,
        hex_right,
        ascii,
        byte_count: chunk.len().min(HEX_BYTES_PER_ROW),
    }
}

/// Detects common binary file signatures from leading magic bytes.
pub fn detect_binary_signature(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"SQLite format 3\0") {
        return "SQLite 3 database";
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return "PNG image";
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return "JPEG image";
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "GIF image";
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "WebP image";
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return "WAV audio";
    }
    if bytes.starts_with(b"%PDF-") {
        return "PDF document";
    }
    if bytes.starts_with(&[0x50, 0x4b, 0x03, 0x04]) || bytes.starts_with(&[0x50, 0x4b, 0x05, 0x06]) {
        return "ZIP archive";
    }
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return "GZIP compressed data";
    }
    if bytes.len() >= 263 && &bytes[257..262] == b"ustar" {
        return "TAR archive";
    }
    if bytes.starts_with(b"\x7fELF") {
        return "ELF executable";
    }
    if bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
        || bytes.starts_with(&[0xce, 0xfa, 0xed, 0xfe])
        || bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe])
        || bytes.starts_with(&[0xfe, 0xed, 0xfa, 0xcf])
    {
        return "Mach-O binary";
    }
    if bytes.starts_with(b"MZ") {
        return "Windows PE executable";
    }
    if bytes.starts_with(b"\0asm") {
        return "WebAssembly module";
    }
    if bytes.starts_with(b"wOFF") {
        return "WOFF font";
    }
    if bytes.starts_with(b"wOF2") {
        return "WOFF2 font";
    }
    if bytes.starts_with(b"OTTO") {
        return "OpenType font";
    }
    if bytes.starts_with(&[0x00, 0x01, 0x00, 0x00]) || bytes.starts_with(b"true") {
        return "TrueType font";
    }
    if bytes.starts_with(b"fLaC") {
        return "FLAC audio";
    }
    if bytes.starts_with(b"OggS") {
        return "OGG container";
    }
    if bytes.starts_with(b"ID3") || (bytes.len() >= 2 && bytes[0] == 0xff && (bytes[1] & 0xe0) == 0xe0) {
        return "MP3 audio";
    }
    "Binary file"
}

/// Summary extracted from a SQLite 3 database header (first 100 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteHeaderInfo {
    pub page_size: u32,
    pub page_count: u32,
    pub schema_format: u32,
    pub text_encoding: &'static str,
    pub user_version: u32,
    pub sqlite_version: u32,
}

pub fn parse_sqlite_header(bytes: &[u8]) -> Option<SqliteHeaderInfo> {
    if bytes.len() < 100 || !bytes.starts_with(b"SQLite format 3\0") {
        return None;
    }
    let raw_page = u16::from_be_bytes([bytes[16], bytes[17]]);
    let page_size = if raw_page == 1 { 65_536 } else { u32::from(raw_page) };
    let page_count = u32::from_be_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);
    let schema_format = u32::from_be_bytes([bytes[44], bytes[45], bytes[46], bytes[47]]);
    let encoding_code = u32::from_be_bytes([bytes[56], bytes[57], bytes[58], bytes[59]]);
    let text_encoding = match encoding_code {
        1 => "UTF-8",
        2 => "UTF-16le",
        3 => "UTF-16be",
        _ => "Unknown",
    };
    let user_version = u32::from_be_bytes([bytes[60], bytes[61], bytes[62], bytes[63]]);
    let sqlite_version = u32::from_be_bytes([bytes[96], bytes[97], bytes[98], bytes[99]]);
    Some(SqliteHeaderInfo {
        page_size,
        page_count,
        schema_format,
        text_encoding,
        user_version,
        sqlite_version,
    })
}

/// Bounded hex + ASCII view over binary bytes.
#[derive(Debug, Clone)]
pub struct HexDocument {
    pub total_bytes: u64,
    pub shown_bytes: usize,
    pub truncated: bool,
    pub signature: &'static str,
    pub sqlite: Option<SqliteHeaderInfo>,
    pub bytes: Vec<u8>,
}

impl HexDocument {
    pub fn from_bytes(bytes: &[u8], total_bytes: Option<u64>) -> Self {
        let total = total_bytes.unwrap_or(bytes.len() as u64);
        let shown = bytes.len().min(HEX_PREVIEW_MAX_BYTES);
        let truncated = total > shown as u64 || bytes.len() > shown;
        let signature = detect_binary_signature(bytes);
        let sqlite = parse_sqlite_header(bytes);
        Self {
            total_bytes: total,
            shown_bytes: shown,
            truncated,
            signature,
            sqlite,
            bytes: bytes[..shown].to_vec(),
        }
    }

    pub fn row_count(&self) -> usize {
        if self.bytes.is_empty() {
            0
        } else {
            self.bytes.len().div_ceil(HEX_BYTES_PER_ROW)
        }
    }

    pub fn row(&self, index: usize) -> Option<HexRow> {
        let start = index.checked_mul(HEX_BYTES_PER_ROW)?;
        if start >= self.bytes.len() {
            return None;
        }
        let end = (start + HEX_BYTES_PER_ROW).min(self.bytes.len());
        Some(format_hex_row(start as u64, &self.bytes[start..end]))
    }
}

/// One entry in a ZIP or TAR archive (`ArchiveEntry` in `binaryPreviewProtocol.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub path: String,
    pub size: u64,
    pub directory: bool,
    pub modified: Option<String>,
}

/// Visible child in an archive folder view (`BinaryViewer.tsx` lines 177-192).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveChild {
    pub path: String,
    pub name: String,
    pub directory: bool,
    pub size: u64,
    pub modified: Option<String>,
}

/// Direct children of `folder` (`""` or `"dir/"`) inside an archive, directories first then by name.
pub fn archive_children(entries: &[ArchiveEntry], folder: &str) -> Vec<ArchiveChild> {
    let mut map: BTreeMap<String, ArchiveChild> = BTreeMap::new();
    for item in entries {
        if !item.path.starts_with(folder) {
            continue;
        }
        let rest = item.path[folder.len()..].trim_end_matches('/');
        if rest.is_empty() {
            continue;
        }
        let slash = rest.find('/');
        let path = match slash {
            None => item.path.trim_end_matches('/').to_owned(),
            Some(idx) => format!("{folder}{}", &rest[..idx]),
        };
        let directory = slash.is_some() || item.directory;
        let key = format!("{path}{}", if directory { "/" } else { "" });
        if let Some(existing) = map.get_mut(&key) {
            existing.size = existing.size.saturating_add(item.size);
        } else {
            let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
            map.insert(
                key,
                ArchiveChild {
                    path,
                    name,
                    directory,
                    size: item.size,
                    modified: item.modified.clone(),
                },
            );
        }
    }
    let mut children: Vec<ArchiveChild> = map.into_values().collect();
    children.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    children
}

/// Parent folder of `folder` (`"a/b/"` -> `"a/"`, `"a/"` -> `""`).
pub fn archive_parent_folder(folder: &str) -> String {
    let trimmed = folder.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(idx) => format!("{}/", &trimmed[..idx]),
        None => String::new(),
    }
}

/// Inspects a ZIP/JAR archive into bounded entries (`binaryPreview.worker.ts`).
pub fn list_zip_entries(bytes: &[u8]) -> Result<Vec<ArchiveEntry>, String> {
    if bytes.len() > BINARY_PREVIEW_MAX_BYTES {
        return Err("Preview limited to files under 50 MB.".to_owned());
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "This archive could not be opened.".to_owned())?;
    if archive.len() > ARCHIVE_MAX_ENTRIES {
        return Err("This archive contains more than 2,048 entries.".to_owned());
    }
    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|_| "An archive entry is damaged.".to_owned())?;
        let name = file.name().trim_start_matches("./").to_owned();
        if name.is_empty() || name.contains("..") {
            continue;
        }
        let dt = file.last_modified();
        let modified = if dt.year() >= 1980 {
            Some(format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute()
            ))
        } else {
            None
        };
        entries.push(ArchiveEntry {
            path: name,
            size: file.size(),
            directory: file.is_dir(),
            modified,
        });
    }
    if entries.is_empty() {
        return Err("This archive has no readable entries.".to_owned());
    }
    Ok(entries)
}

/// Extracts one bounded entry from a ZIP/JAR archive.
pub fn read_zip_entry(bytes: &[u8], entry_path: &str) -> Result<Vec<u8>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "This archive could not be opened.".to_owned())?;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|_| "An archive entry is damaged.".to_owned())?;
        let normalized = file.name().trim_start_matches("./").trim_end_matches('/');
        if normalized == entry_path.trim_end_matches('/') {
            if file.size() > ARCHIVE_ENTRY_MAX_BYTES as u64 {
                return Err("Entry previews are limited to 10 MB.".to_owned());
            }
            let mut out = Vec::with_capacity(file.size().min(ARCHIVE_ENTRY_MAX_BYTES as u64) as usize);
            let mut limited = file.take((ARCHIVE_ENTRY_MAX_BYTES + 1) as u64);
            limited
                .read_to_end(&mut out)
                .map_err(|_| "Could not extract archive entry.".to_owned())?;
            if out.len() > ARCHIVE_ENTRY_MAX_BYTES {
                return Err("Entry previews are limited to 10 MB.".to_owned());
            }
            return Ok(out);
        }
    }
    Err("This archive entry is unavailable.".to_owned())
}

/// Parsed TAR entries along with byte offsets `(start, end)` for non-directory entries.
pub type TarListing = (Vec<ArchiveEntry>, BTreeMap<String, (usize, usize)>);

/// Parses an uncompressed TAR archive (`tarEntries` in `binaryPreview.worker.ts`).
pub fn list_tar_entries(bytes: &[u8]) -> Result<TarListing, String> {
    if bytes.len() > BINARY_PREVIEW_MAX_BYTES {
        return Err("Preview limited to files under 50 MB.".to_owned());
    }
    let mut entries = Vec::new();
    let mut ranges = BTreeMap::new();
    let mut long_path: Option<String> = None;
    let mut offset = 0usize;

    while offset + 512 <= bytes.len() {
        let header = &bytes[offset..offset + 512];
        if header.iter().all(|&b| b == 0) {
            break;
        }
        if entries.len() >= ARCHIVE_MAX_ENTRIES {
            return Err("This archive contains more than 2,048 entries.".to_owned());
        }
        let stored_checksum = parse_tar_octal(&header[148..156])?;
        let computed: u64 = header
            .iter()
            .enumerate()
            .map(|(i, &b)| u64::from(if (148..156).contains(&i) { b' ' } else { b }))
            .sum();
        if stored_checksum != computed {
            return Err("The TAR header is damaged.".to_owned());
        }
        let kind = header[156];
        let size = parse_tar_octal(&header[124..136])? as usize;
        if size > BINARY_PREVIEW_MAX_BYTES || offset + 512 + size > bytes.len() {
            return Err("An archive entry is too large or damaged.".to_owned());
        }
        let content_start = offset + 512;
        let content_end = content_start + size;
        if kind == b'L' {
            let raw = String::from_utf8_lossy(&bytes[content_start..content_end]);
            long_path = Some(raw.trim_end_matches('\0').to_owned());
        } else if kind == 0 || kind == b'0' || kind == b'5' {
            let prefix = if &header[257..262] == b"ustar" {
                tar_cstr(&header[345..500])
            } else {
                String::new()
            };
            let base = tar_cstr(&header[0..100]);
            let path = long_path.take().unwrap_or_else(|| {
                if prefix.is_empty() {
                    base
                } else {
                    format!("{prefix}/{base}")
                }
            });
            let path = path.trim_start_matches("./").to_owned();
            if path.len() > 1024 {
                return Err("An archive entry is too large or damaged.".to_owned());
            }
            let directory = kind == b'5';
            if !path.is_empty() {
                if !directory {
                    ranges.insert(path.trim_end_matches('/').to_owned(), (content_start, content_end));
                }
                entries.push(ArchiveEntry {
                    path,
                    size: size as u64,
                    directory,
                    modified: None,
                });
            }
        } else {
            long_path = None;
        }
        let padded = size.div_ceil(512) * 512;
        offset = offset.saturating_add(512 + padded);
    }

    if entries.is_empty() {
        return Err("This archive has no readable entries.".to_owned());
    }
    Ok((entries, ranges))
}

fn tar_cstr(slice: &[u8]) -> String {
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    String::from_utf8_lossy(&slice[..end]).into_owned()
}

fn parse_tar_octal(slice: &[u8]) -> Result<u64, String> {
    let text = tar_cstr(slice);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(trimmed, 8)
        .map_err(|_| "This TAR uses an unsupported numeric format.".to_owned())
}
