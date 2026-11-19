//! Audio metadata/time formatting, Font SFNT/WOFF metadata & specimen constants, and
//! file-tree drag-to-mention logic.
//! Ports:
//! - `apps/web/src/components/files/AudioPreview.tsx` (`audioTime`)
//! - `apps/web/src/components/files/FontViewer.tsx` (`MAX_FONT_BYTES`, specimen layout)
//! - `apps/web/src/components/files/fileTreeDragMention.ts` (`createFileTreeDragMentionController`)

use super::{basename, composer_file_link};

/// `MAX_FONT_BYTES` (`FontViewer.tsx`).
pub const MAX_FONT_BYTES: usize = 10 * 1024 * 1024;
/// Maximum audio download size for in-panel playback.
pub const MAX_AUDIO_BYTES: usize = 64 * 1024 * 1024;

/// Specimen sizes from `FontViewer.tsx`.
pub const FONT_SPECIMEN_SIZES: [f32; 5] = [12.0, 16.0, 24.0, 40.0, 64.0];
pub const FONT_SPECIMEN_UPPER: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
pub const FONT_SPECIMEN_LOWER: &str = "abcdefghijklmnopqrstuvwxyz";
pub const FONT_SPECIMEN_SYMBOLS: &str = "0123456789 !?&@#%(){}[]";
pub const FONT_SPECIMEN_PANGRAM: &str = "The quick brown fox jumps over the lazy dog.";

/// `audioTime` (`AudioPreview.tsx`): formats seconds as `M:SS`.
pub fn format_audio_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0:00".to_owned();
    }
    let total = seconds.floor() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// Pure-Rust metadata extracted from common audio file headers.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioInfo {
    pub format: &'static str,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub bits_per_sample: Option<u16>,
    pub duration_secs: Option<f64>,
    pub byte_length: u64,
}

pub fn inspect_audio_bytes(path: &str, bytes: &[u8]) -> AudioInfo {
    let byte_length = bytes.len() as u64;
    if let Some(info) = parse_wav(bytes) {
        return info;
    }
    if let Some(info) = parse_flac(bytes) {
        return info;
    }
    if let Some(info) = parse_aiff(bytes) {
        return info;
    }
    let format = match super::extension(path).as_deref() {
        Some("mp3") => "MP3",
        Some("wav") => "WAV",
        Some("flac") => "FLAC",
        Some("ogg" | "oga") => "OGG",
        Some("opus") => "Opus",
        Some("aac") => "AAC",
        Some("m4a") => "M4A / AAC",
        Some("aiff") => "AIFF",
        Some("weba" | "webm-audio") => "WebM Audio",
        _ => "Audio",
    };
    AudioInfo {
        format,
        sample_rate: None,
        channels: None,
        bits_per_sample: None,
        duration_secs: None,
        byte_length,
    }
}

fn parse_wav(bytes: &[u8]) -> Option<AudioInfo> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut channels = None;
    let mut sample_rate = None;
    let mut byte_rate = None;
    let mut bits_per_sample = None;
    let mut data_bytes = None;

    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]) as usize;
        pos += 8;
        if id == b"fmt " && pos + 16 <= bytes.len() {
            channels = Some(u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]));
            sample_rate = Some(u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]));
            byte_rate = Some(u32::from_le_bytes([bytes[pos + 8], bytes[pos + 9], bytes[pos + 10], bytes[pos + 11]]));
            bits_per_sample = Some(u16::from_le_bytes([bytes[pos + 14], bytes[pos + 15]]));
        } else if id == b"data" {
            data_bytes = Some(size as u64);
        }
        let step = size + (size % 2);
        pos = pos.saturating_add(step);
    }

    let duration_secs = match (data_bytes, byte_rate) {
        (Some(d), Some(br)) if br > 0 => Some(d as f64 / f64::from(br)),
        _ => None,
    };
    Some(AudioInfo {
        format: "WAV",
        sample_rate,
        channels,
        bits_per_sample,
        duration_secs,
        byte_length: bytes.len() as u64,
    })
}

fn parse_flac(bytes: &[u8]) -> Option<AudioInfo> {
    if bytes.len() < 42 || &bytes[0..4] != b"fLaC" {
        return None;
    }
    // STREAMINFO block is type 0 at offset 4.
    let block_type = bytes[4] & 0x7f;
    let block_len = ((bytes[5] as usize) << 16) | ((bytes[6] as usize) << 8) | (bytes[7] as usize);
    if block_type != 0 || block_len < 34 || bytes.len() < 8 + 34 {
        return Some(AudioInfo {
            format: "FLAC",
            sample_rate: None,
            channels: None,
            bits_per_sample: None,
            duration_secs: None,
            byte_length: bytes.len() as u64,
        });
    }
    let si = &bytes[8..42];
    let sample_rate = ((u32::from(si[10])) << 12) | ((u32::from(si[11])) << 4) | (u32::from(si[12]) >> 4);
    let channels = (((si[12] >> 1) & 0x07) + 1) as u16;
    let bits_per_sample = ((((si[12] & 0x01) << 4) | (si[13] >> 4)) + 1) as u16;
    let total_samples = ((u64::from(si[13] & 0x0f)) << 32)
        | ((u64::from(si[14])) << 24)
        | ((u64::from(si[15])) << 16)
        | ((u64::from(si[16])) << 8)
        | u64::from(si[17]);
    let duration_secs = (sample_rate > 0 && total_samples > 0)
        .then(|| total_samples as f64 / f64::from(sample_rate));
    Some(AudioInfo {
        format: "FLAC",
        sample_rate: (sample_rate > 0).then_some(sample_rate),
        channels: Some(channels),
        bits_per_sample: Some(bits_per_sample),
        duration_secs,
        byte_length: bytes.len() as u64,
    })
}

fn parse_aiff(bytes: &[u8]) -> Option<AudioInfo> {
    if bytes.len() < 12 || &bytes[0..4] != b"FORM" || (&bytes[8..12] != b"AIFF" && &bytes[8..12] != b"AIFC") {
        return None;
    }
    Some(AudioInfo {
        format: "AIFF",
        sample_rate: None,
        channels: None,
        bits_per_sample: None,
        duration_secs: None,
        byte_length: bytes.len() as u64,
    })
}

/// Detected font container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFormat {
    TrueType,
    OpenTypeCff,
    Woff,
    Woff2,
}

impl FontFormat {
    pub fn label(self) -> &'static str {
        match self {
            FontFormat::TrueType => "TrueType (TTF)",
            FontFormat::OpenTypeCff => "OpenType (OTF)",
            FontFormat::Woff => "WOFF",
            FontFormat::Woff2 => "WOFF2",
        }
    }

    /// True when GPUI's native font loader (`TextSystem::add_fonts`) accepts the raw bytes directly.
    pub fn native_loadable(self) -> bool {
        matches!(self, FontFormat::TrueType | FontFormat::OpenTypeCff)
    }
}

/// Metadata parsed from a font binary (`FontViewer.tsx`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontInfo {
    pub format: FontFormat,
    pub family_name: Option<String>,
    pub subfamily_name: Option<String>,
    pub full_name: Option<String>,
    pub version: Option<String>,
    pub glyph_count: Option<u16>,
    pub units_per_em: Option<u16>,
    pub byte_length: u64,
}

/// Validates and parses SFNT (`TTF`/`OTF`) and WOFF/WOFF2 font headers & `name`/`maxp`/`head` tables.
pub fn parse_font_info(bytes: &[u8]) -> Result<FontInfo, String> {
    if bytes.len() > MAX_FONT_BYTES {
        return Err("Font previews are limited to 10 MB.".to_owned());
    }
    if bytes.len() < 12 {
        return Err("The font is damaged or unsupported.".to_owned());
    }
    let tag = &bytes[0..4];
    let format = if tag == [0x00, 0x01, 0x00, 0x00] || tag == b"true" {
        FontFormat::TrueType
    } else if tag == b"OTTO" {
        FontFormat::OpenTypeCff
    } else if tag == b"wOFF" {
        FontFormat::Woff
    } else if tag == b"wOF2" {
        FontFormat::Woff2
    } else {
        return Err("The font is damaged or unsupported.".to_owned());
    };

    let mut info = FontInfo {
        format,
        family_name: None,
        subfamily_name: None,
        full_name: None,
        version: None,
        glyph_count: None,
        units_per_em: None,
        byte_length: bytes.len() as u64,
    };

    if matches!(format, FontFormat::TrueType | FontFormat::OpenTypeCff) {
        parse_sfnt_tables(bytes, &mut info);
    }
    Ok(info)
}

fn read_u16_be(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn parse_sfnt_tables(bytes: &[u8], info: &mut FontInfo) {
    let Some(num_tables) = read_u16_be(bytes, 4) else { return };
    for index in 0..usize::from(num_tables) {
        let record_offset = 12 + index * 16;
        let Some(record) = bytes.get(record_offset..record_offset + 16) else { break };
        let tag = &record[0..4];
        let Some(offset) = read_u32_be(record, 8).map(|v| v as usize) else { continue };
        let Some(length) = read_u32_be(record, 12).map(|v| v as usize) else { continue };
        let Some(table) = bytes.get(offset..offset.saturating_add(length)) else { continue };
        match tag {
            b"maxp" => {
                info.glyph_count = read_u16_be(table, 4);
            }
            b"head" => {
                info.units_per_em = read_u16_be(table, 18);
            }
            b"name" => {
                parse_name_table(table, info);
            }
            _ => {}
        }
    }
}

fn parse_name_table(table: &[u8], info: &mut FontInfo) {
    let (Some(count), Some(string_offset)) = (read_u16_be(table, 2), read_u16_be(table, 4)) else {
        return;
    };
    let storage_start = usize::from(string_offset);
    for idx in 0..usize::from(count) {
        let rec = 6 + idx * 12;
        let (Some(platform_id), Some(name_id), Some(len), Some(str_off)) = (
            read_u16_be(table, rec),
            read_u16_be(table, rec + 6),
            read_u16_be(table, rec + 8),
            read_u16_be(table, rec + 10),
        ) else {
            break;
        };
        if !matches!(name_id, 1 | 2 | 4 | 5 | 16) {
            continue;
        }
        let start = storage_start.saturating_add(usize::from(str_off));
        let end = start.saturating_add(usize::from(len));
        let Some(raw) = table.get(start..end) else { continue };
        let decoded = match platform_id {
            // Unicode (0) or Windows (3): UTF-16BE
            0 | 3 => {
                let units: Vec<u16> = raw
                    .chunks_exact(2)
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                    .collect();
                String::from_utf16(&units).ok()
            }
            // Macintosh Roman (1): ASCII/Latin-1 fallback
            1 => Some(raw.iter().map(|&b| b as char).collect()),
            _ => None,
        };
        let Some(value) = decoded.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) else {
            continue;
        };
        match name_id {
            16 => info.family_name = Some(value),
            1 if info.family_name.is_none() => info.family_name = Some(value),
            2 if info.subfamily_name.is_none() => info.subfamily_name = Some(value),
            4 if info.full_name.is_none() => info.full_name = Some(value),
            5 if info.version.is_none() => info.version = Some(value),
            _ => {}
        }
    }
}

/// Payload carried when dragging one or more rows from the file tree (`fileTreeDragMention.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraggedFileMention {
    pub paths: Vec<String>,
    pub mention: String,
    pub label: String,
}

impl DraggedFileMention {
    /// Builds a drag mention payload for `item_path`, expanding to `selection` when `item_path` is in it.
    pub fn from_drag(item_path: &str, selection: &[String]) -> Option<Self> {
        let item = item_path.trim().trim_end_matches('/');
        if item.is_empty() {
            return None;
        }
        let dragged: Vec<String> = if selection.iter().any(|p| p.trim_end_matches('/') == item) {
            selection
                .iter()
                .map(|p| p.trim().trim_end_matches('/').to_owned())
                .filter(|p| !p.is_empty())
                .collect()
        } else {
            vec![item.to_owned()]
        };
        let mentions: Vec<String> = dragged.iter().map(|path| composer_file_link(path)).collect();
        if mentions.is_empty() {
            return None;
        }
        let label = if dragged.len() == 1 {
            basename(&dragged[0]).to_owned()
        } else {
            format!("{} files", dragged.len())
        };
        Some(Self {
            paths: dragged,
            mention: mentions.join(" "),
            label,
        })
    }
}

/// Pure controller for file-tree drag-to-mention (`createFileTreeDragMentionController`).
#[derive(Debug, Default, Clone)]
pub struct DragMentionController {
    selection: Vec<String>,
    dragged_paths: Vec<String>,
}

impl DragMentionController {
    pub fn is_drag_in_progress(&self) -> bool {
        !self.dragged_paths.is_empty()
    }

    pub fn set_selection(&mut self, paths: Vec<String>) {
        self.selection = paths;
    }

    pub fn start_drag(&mut self, item_path: Option<&str>) -> Option<DraggedFileMention> {
        let item_path = item_path?;
        let payload = DraggedFileMention::from_drag(item_path, &self.selection)?;
        self.dragged_paths = payload.paths.clone();
        Some(payload)
    }

    /// Ends the active drag and returns the dragged paths that should be deselected.
    pub fn end_drag(&mut self) -> Vec<String> {
        std::mem::take(&mut self.dragged_paths)
    }
}
