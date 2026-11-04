//! Open VSX parsing and bounded VSIX extraction from `web/src/openVsxThemes.ts`.
use super::{Collection, ThemeDefinition, importer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
};
use url::Url;
pub const MAX_VSIX_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_SEARCH_BYTES: usize = 512 * 1024;
pub const MAX_JSON_BYTES: usize = 256 * 1024;
pub const DEBOUNCE_MS: u64 = 350;
pub const SUGGESTIONS: [&str; 4] = ["Dracula", "Catppuccin", "Nord", "Tokyo Night"];
pub const SORTS: [(&str, &str); 4] = [
    ("downloadCount", "Most downloaded"),
    ("rating", "Best rated"),
    ("timestamp", "Newest"),
    ("relevance", "Most relevant"),
];
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Extension {
    pub id: String,
    pub collection_id: String,
    pub name: String,
    pub publisher: String,
    pub description: String,
    pub download_count: f64,
    pub icon_url: Option<String>,
    pub source_url: Option<String>,
    pub manifest_url: String,
    pub sha256_url: String,
    pub vsix_url: String,
    pub version: String,
    pub license: String,
}
fn short_hash(s: &str) -> String {
    Sha256::digest(s.as_bytes())[..6]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn theme_id(extension: &str, source: &str) -> String {
    format!("ovx-theme-{}", short_hash(&format!("{extension}:{source}")))
}
fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
pub fn trusted_url(s: &str) -> Option<String> {
    Url::parse(s)
        .ok()
        .filter(|u| u.scheme() == "https" && u.host_str() == Some("open-vsx.org"))
        .map(Into::into)
}
fn source_url(v: &Value) -> Option<String> {
    Url::parse(v.as_str().unwrap_or_else(|| text(v, "url")))
        .ok()
        .filter(|u| u.scheme() == "https" && u.username().is_empty() && u.password().is_none())
        .map(Into::into)
}
pub fn search_url(query: &str, sort: &str) -> Option<Url> {
    if query.trim().is_empty() {
        return None;
    }
    let mut url = Url::parse("https://open-vsx.org/api/-/search").unwrap();
    url.query_pairs_mut().extend_pairs([
        ("query", query.trim()),
        ("category", "Themes"),
        (
            "sortBy",
            if SORTS.iter().any(|(key, _)| *key == sort) {
                sort
            } else {
                "downloadCount"
            },
        ),
        ("sortOrder", "desc"),
        ("size", "16"),
    ]);
    Some(url)
}
pub fn identities(bytes: &[u8]) -> Result<Vec<Url>, String> {
    if bytes.len() > MAX_SEARCH_BYTES {
        return Err("Open VSX returned an unexpectedly large response.".into());
    }
    let v: Value =
        serde_json::from_slice(bytes).map_err(|_| "Open VSX returned an unreadable response.")?;
    let entries = v["extensions"]
        .as_array()
        .ok_or("Open VSX returned an unreadable search response.")?;
    Ok(entries
        .iter()
        .filter(|v| !text(v, "namespace").is_empty() && !text(v, "name").is_empty())
        .take(16)
        .map(|v| {
            let mut u = Url::parse("https://open-vsx.org/api/").unwrap();
            u.path_segments_mut()
                .unwrap()
                .pop_if_empty()
                .push(text(v, "namespace"))
                .push(text(v, "name"));
            u
        })
        .collect())
}
pub fn detail(bytes: &[u8]) -> Result<Option<Extension>, String> {
    let error = "Open VSX returned malformed theme details.";
    if bytes.len() > MAX_JSON_BYTES {
        return Err("Open VSX returned an unexpectedly large detail response.".into());
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|_| error)?;
    let namespace = text(&v, "namespace").trim();
    let name = text(&v, "name").trim();
    let version = text(&v, "version").trim();
    let license = text(&v, "license").trim();
    let files = &v["files"];
    let manifest_url = trusted_url(text(files, "manifest")).ok_or(error)?;
    let sha256_url = trusted_url(text(files, "sha256")).ok_or(error)?;
    let vsix_url = trusted_url(text(files, "download")).ok_or(error)?;
    if namespace.is_empty() || name.is_empty() || version.is_empty() {
        return Err(error.into());
    }
    if ![
        "0BSD",
        "Apache-2.0",
        "BSD-2-Clause",
        "BSD-3-Clause",
        "CC0-1.0",
        "ISC",
        "MIT",
        "MPL-2.0",
        "Unlicense",
    ]
    .contains(&license)
    {
        return Ok(None);
    }
    let id = format!("{namespace}.{name}");
    let normalized = format!("open-vsx:{}", id.to_lowercase());
    let collection_id = if normalized.len() <= 128
        && normalized
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".:-".contains(&b))
    {
        normalized
    } else {
        format!("open-vsx:{}", short_hash(&id))
    };
    Ok(Some(Extension {
        id,
        collection_id,
        name: if text(&v, "displayName").trim().is_empty() {
            name
        } else {
            text(&v, "displayName").trim()
        }
        .into(),
        publisher: namespace.into(),
        description: text(&v, "description").into(),
        download_count: v["downloadCount"]
            .as_f64()
            .filter(|n| n.is_finite())
            .unwrap_or(0.),
        icon_url: trusted_url(text(files, "icon")),
        source_url: source_url(&v["repository"])
            .or_else(|| source_url(&v["homepage"]))
            .or_else(|| source_url(&v["url"])),
        manifest_url,
        sha256_url,
        vsix_url,
        version: version.into(),
        license: license.into(),
    }))
}
pub const IMPORT_CANCELLED: &str = "Theme import was cancelled.";
const MAX_JSON_DEPTH: usize = 64;
type Cancel<'a> = &'a dyn Fn() -> bool;
fn checkpoint(cancelled: Cancel<'_>) -> Result<(), String> {
    if cancelled() {
        Err(IMPORT_CANCELLED.into())
    } else {
        Ok(())
    }
}
fn check_json_depth(bytes: &[u8], description: &str, cancelled: Cancel<'_>) -> Result<(), String> {
    enum Lex {
        Normal,
        String,
        Escape,
        LineComment,
        BlockComment,
    }
    let mut lex = Lex::Normal;
    let mut depth = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        if index % 4096 == 0 {
            checkpoint(cancelled)?;
        }
        let byte = bytes[index];
        match lex {
            Lex::String => match byte {
                b'\\' => lex = Lex::Escape,
                b'"' => lex = Lex::Normal,
                _ => {}
            },
            Lex::Escape => lex = Lex::String,
            Lex::LineComment => {
                if byte == b'\n' || (byte == b'\r' && bytes.get(index + 1) == Some(&b'\n')) {
                    lex = Lex::Normal;
                }
            }
            Lex::BlockComment => {
                if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    lex = Lex::Normal;
                    index += 1;
                }
            }
            Lex::Normal => match byte {
                b'\'' => return Err(format!("{description} is not valid JSON.")),
                b'"' => lex = Lex::String,
                b'/' if bytes.get(index + 1) == Some(&b'/') => {
                    lex = Lex::LineComment;
                    index += 1;
                }
                b'/' if bytes.get(index + 1) == Some(&b'*') => {
                    lex = Lex::BlockComment;
                    index += 1;
                }
                b'{' | b'[' => {
                    depth += 1;
                    if depth > MAX_JSON_DEPTH {
                        return Err(format!("{description} is nested too deeply."));
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            },
        }
        index += 1;
    }
    checkpoint(cancelled)
}
pub fn json_object(bytes: &[u8], description: &str) -> Result<Value, String> {
    json_object_cancellable(bytes, description, &|| false)
}
fn json_object_cancellable(
    bytes: &[u8],
    description: &str,
    cancelled: Cancel<'_>,
) -> Result<Value, String> {
    checkpoint(cancelled)?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err(format!("{description} is too large."));
    }
    check_json_depth(bytes, description, cancelled)?;
    let source = String::from_utf8_lossy(bytes);
    let value = jsonc_parser::parse_to_serde_value(
        &source,
        &jsonc_parser::ParseOptions {
            allow_comments: true,
            allow_trailing_commas: true,
            allow_loose_object_property_names: false,
            allow_missing_commas: false,
            allow_single_quoted_strings: false,
            allow_hexadecimal_numbers: false,
            allow_unary_plus_numbers: false,
        },
    )
    .ok()
    .flatten()
    .filter(Value::is_object)
    .ok_or_else(|| format!("{description} is not valid JSON."))?;
    checkpoint(cancelled)?;
    Ok(value)
}

fn contributions(v: &Value) -> Vec<&Value> {
    v["contributes"]["themes"]
        .as_array()
        .map(|a| a.iter().filter(|v| v.is_object()).collect())
        .unwrap_or_default()
}
fn license_matches(v: &Value, e: &Extension) -> bool {
    text(v, "license").trim().eq_ignore_ascii_case(&e.license)
}
pub fn eligible(
    e: &Extension,
    manifest: &[u8],
    package_length: Option<u64>,
) -> Result<bool, String> {
    let v = json_object(manifest, "Extension manifest")?;
    Ok(!contributions(&v).is_empty()
        && license_matches(&v, e)
        && package_length.is_none_or(|n| n <= MAX_VSIX_BYTES as u64))
}
pub fn ranked_results(
    details: Vec<Result<Option<Extension>, String>>,
) -> Result<Vec<Extension>, String> {
    if !details.is_empty() && details.iter().all(Result::is_err) {
        return Err("Open VSX theme details are unavailable right now.".into());
    }
    Ok(details
        .into_iter()
        .filter_map(Result::ok)
        .flatten()
        .take(8)
        .collect())
}
pub fn compact_downloads(mut count: f64) -> String {
    let units = ["", "K", "M", "B", "T"];
    let mut unit = 0;
    while count.abs() >= 1000. && unit < units.len() - 1 {
        count /= 1000.;
        unit += 1;
    }
    count = (count * 10.).round() / 10.;
    if count.abs() >= 1000. && unit < units.len() - 1 {
        count /= 1000.;
        unit += 1;
    }
    format!("{count}{}", units[unit])
}

pub fn package_path(path: &str, relative: &str) -> Result<String, String> {
    if path.len() > 1024
        || path.contains('\0')
        || path.starts_with('/')
        || path.as_bytes().get(1) == Some(&b':')
    {
        return Err("Theme path is not a safe relative package path.".into());
    }
    let mut segments: Vec<&str> = relative.split('/').collect();
    segments.pop();
    let normalized = path.replace('\\', "/");
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if segments.len() <= 1 {
                    return Err("Theme path escapes the extension package.".into());
                }
                segments.pop();
            }
            _ => segments.push(part),
        }
    }
    if segments.first() != Some(&"extension") {
        segments.insert(0, "extension");
    }
    Ok(segments.join("/"))
}
pub fn inspect_directory(bytes: &[u8]) -> Result<Vec<u8>, String> {
    inspect_directory_cancellable(bytes, &|| false).map(|directory| directory.bytes)
}
struct InspectedDirectory {
    bytes: Vec<u8>,
    entries: usize,
}
fn inspect_directory_cancellable(
    bytes: &[u8],
    cancelled: Cancel<'_>,
) -> Result<InspectedDirectory, String> {
    checkpoint(cancelled)?;
    let bad = "That extension package has an invalid ZIP directory.";
    if bytes.len() > MAX_VSIX_BYTES {
        return Err("That theme extension is too large to import safely.".into());
    }
    let u16at =
        |o: usize| -> usize { u16::from_le_bytes(bytes[o..o + 2].try_into().unwrap()) as usize };
    let u32at =
        |o: usize| -> usize { u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()) as usize };
    let mut end = None;
    for offset in (bytes.len().saturating_sub(65557)..=bytes.len().saturating_sub(22)).rev() {
        if offset % 256 == 0 {
            checkpoint(cancelled)?;
        }
        if offset + 22 <= bytes.len()
            && bytes[offset..offset + 4] == *b"PK\x05\x06"
            && offset + 22 + u16at(offset + 20) == bytes.len()
        {
            end = Some(offset);
            break;
        }
    }
    let end = end.ok_or("That extension package has no ZIP directory.")?;
    // zip 0.6 follows a locator here even when it is hidden inside an ordinary entry comment.
    if end >= 20 && bytes[end - 20..end - 16] == *b"PK\x06\x07" {
        return Err("That extension package has unsupported ZIP64 metadata.".into());
    }
    for (offset, signature) in bytes[..end].windows(4).enumerate() {
        if offset % 4096 == 0 {
            checkpoint(cancelled)?;
        }
        if signature == b"PK\x06\x06" {
            return Err("That extension package has unsupported ZIP64 metadata.".into());
        }
    }
    let start = u32at(end + 16);
    if start.checked_add(u32at(end + 12)) != Some(end) {
        return Err(bad.into());
    }
    let mut offset = start;
    let mut count = 0;
    let mut total = 0u64;
    while offset < end {
        checkpoint(cancelled)?;
        if offset + 46 > end || bytes[offset..offset + 4] != [0x50, 0x4b, 1, 2] {
            return Err(bad.into());
        }
        count += 1;
        if count > 5000 {
            return Err("That extension package has too many files.".into());
        }
        let compressed = u32at(offset + 20);
        let expanded = u32at(offset + 24);
        if compressed == 0xffffffff || expanded == 0xffffffff {
            return Err("That extension package has unsupported ZIP64 metadata.".into());
        }
        total += expanded as u64;
        if total > 100 * 1024 * 1024 {
            return Err("That extension package expands beyond the safe import limit.".into());
        }
        if expanded > 0 && (compressed == 0 || expanded as f64 / compressed as f64 > 200.) {
            return Err("That extension package has an unsafe compression ratio.".into());
        }
        offset += 46 + u16at(offset + 28) + u16at(offset + 30) + u16at(offset + 32);
    }
    if offset != end {
        return Err(bad.into());
    }
    if u16at(end + 8) != count || u16at(end + 10) != count {
        return Err(bad.into());
    }
    checkpoint(cancelled)?;
    let mut stripped = bytes[..end + 22].to_vec();
    stripped[end + 20..end + 22].fill(0);
    Ok(InspectedDirectory {
        bytes: stripped,
        entries: count,
    })
}
fn read_text(
    zip: &mut zip::ZipArchive<Cursor<Vec<u8>>>,
    path: &str,
    cancelled: Cancel<'_>,
) -> Result<Vec<u8>, String> {
    checkpoint(cancelled)?;
    let mut file = zip
        .by_name(path)
        .map_err(|_| format!("{path} is missing from the extension package."))?;
    if file.size() > MAX_JSON_BYTES as u64 {
        return Err(format!("{path} is too large."));
    }
    let mut out = Vec::new();
    let mut chunk = [0; 16 * 1024];
    loop {
        checkpoint(cancelled)?;
        let read = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        if out.len() + read > MAX_JSON_BYTES {
            return Err(format!("{path} is too large."));
        }
        out.extend_from_slice(&chunk[..read]);
    }
    checkpoint(cancelled)?;
    Ok(out)
}

fn load_theme(
    zip: &mut zip::ZipArchive<Cursor<Vec<u8>>>,
    path: &str,
    cache: &mut BTreeMap<String, Value>,
    ancestors: &mut BTreeSet<String>,
    files: &mut usize,
    cancelled: Cancel<'_>,
) -> Result<Value, String> {
    checkpoint(cancelled)?;
    if ancestors.len() >= 8 {
        return Err("Theme includes are nested too deeply.".into());
    }
    if ancestors.contains(path) {
        return Err("Theme includes contain a cycle.".into());
    }
    if let Some(v) = cache.get(path) {
        return Ok(v.clone());
    }
    *files += 1;
    if *files > 320 {
        return Err("That extension references too many theme files.".into());
    }
    let value = json_object_cancellable(&read_text(zip, path, cancelled)?, path, cancelled)?;
    let mut colors = serde_json::Map::new();
    if let Some(c) = value["colors"].as_object() {
        for (key, color) in c {
            if USED_COLORS.contains(&key.as_str()) && color.as_str().is_some_and(|s| s.len() <= 128)
            {
                colors.insert(key.clone(), color.clone());
            }
        }
    }
    let mut resolved = json!({"colors":colors});
    if let Some(include) = value["include"].as_str() {
        ancestors.insert(path.into());
        let include = package_path(include, path)?;
        let base = load_theme(zip, &include, cache, ancestors, files, cancelled)?;
        ancestors.remove(path);
        let mut merged = base["colors"].as_object().cloned().unwrap_or_default();
        merged.extend(colors);
        resolved["colors"] = Value::Object(merged);
    }
    cache.insert(path.into(), resolved.clone());
    Ok(resolved)
}
pub fn extract(
    e: &Extension,
    advertised: &[u8],
    bytes: &[u8],
    checksum: &[u8],
) -> Result<Vec<ThemeDefinition>, String> {
    extract_cancellable(e, advertised, bytes, checksum, &|| false)
}
pub fn extract_cancellable(
    e: &Extension,
    advertised: &[u8],
    bytes: &[u8],
    checksum: &[u8],
    cancelled: Cancel<'_>,
) -> Result<Vec<ThemeDefinition>, String> {
    checkpoint(cancelled)?;
    let advertised = json_object_cancellable(advertised, "Extension manifest", cancelled)?;
    check_contributions(&advertised)?;
    let checksum = String::from_utf8_lossy(checksum);
    let expected = checksum.split_whitespace().next().unwrap_or("");
    if checksum.len() > 256
        || expected.len() != 64
        || !expected.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("That Open VSX theme has an invalid checksum.".into());
    }
    let mut hash = Sha256::new();
    for chunk in bytes.chunks(64 * 1024) {
        checkpoint(cancelled)?;
        hash.update(chunk);
    }
    checkpoint(cancelled)?;
    if !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(expected) {
        return Err("That Open VSX theme failed its integrity check.".into());
    }
    let inspected = inspect_directory_cancellable(bytes, cancelled)?;
    checkpoint(cancelled)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(inspected.bytes))
        .map_err(|_| "That Open VSX extension package could not be opened.")?;
    checkpoint(cancelled)?;
    if zip.len() != inspected.entries {
        return Err("That extension package has an invalid ZIP directory.".into());
    }
    for i in 0..zip.len() {
        checkpoint(cancelled)?;
        let file = zip
            .by_index(i)
            .map_err(|_| "That Open VSX extension package could not be opened.")?;
        package_path(file.name(), "extension/")?;
    }
    let manifest = json_object_cancellable(
        &read_text(&mut zip, "extension/package.json", cancelled)?,
        "Extension manifest",
        cancelled,
    )?;
    if !text(&manifest, "publisher").eq_ignore_ascii_case(&e.publisher)
        || !format!(
            "{}.{}",
            text(&manifest, "publisher"),
            text(&manifest, "name")
        )
        .eq_ignore_ascii_case(&e.id)
        || text(&manifest, "version") != e.version
    {
        return Err("That extension package does not match the selected Open VSX theme.".into());
    }
    if !license_matches(&manifest, e) {
        return Err("That extension package does not match its advertised license.".into());
    }
    check_contributions(&manifest)?;
    let mut entries = Vec::new();
    let mut paths = Vec::new();
    let mut cache = BTreeMap::new();
    let mut files = 0;
    for c in contributions(&manifest) {
        checkpoint(cancelled)?;
        let parsed = (|| {
            let path = package_path(
                c["path"].as_str().ok_or("theme path is missing")?,
                "extension/",
            )?;
            let mut value = load_theme(
                &mut zip,
                &path,
                &mut cache,
                &mut BTreeSet::new(),
                &mut files,
                cancelled,
            )?;
            value["displayName"] = json!(
                c["label"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::trim)
                    .unwrap_or(&e.name)
            );
            match text(c, "uiTheme") {
                "vs" => value["type"] = json!("light"),
                "vs-dark" => value["type"] = json!("dark"),
                "hc-black" | "hc-light" => value["type"] = c["uiTheme"].clone(),
                _ => {}
            }
            let theme = super::vscode::convert(&value)?;
            checkpoint(cancelled)?;
            Ok::<_, String>((theme, path))
        })()
        .map_err(|_| {
            if cancelled() {
                IMPORT_CANCELLED.to_string()
            } else {
                "One or more color themes in that extension could not be imported safely.".into()
            }
        })?;
        entries.push((parsed.0, parsed.1.rsplit('/').next().map(str::to_owned)));
        paths.push(parsed.1);
    }
    checkpoint(cancelled)?;
    let extension = e.id.to_lowercase();
    let mut occurrences = BTreeMap::<String, usize>::new();
    let resolved = importer::resolve_label_collisions(entries)
        .into_iter()
        .zip(paths)
        .map(|(mut t, path)| {
            let n = occurrences.entry(path.clone()).or_default();
            let source = if *n == 0 {
                path
            } else {
                format!("{path}\0{n}")
            };
            *n += 1;
            t.id = theme_id(&extension, &source);
            t
        })
        .collect();
    let paired = importer::pair_vscode_themes_with(resolved, |light, dark, _| {
        let mut ids = [light.id.as_str(), dark.id.as_str()];
        ids.sort();
        theme_id(&extension, &ids.join(":"))
    });
    checkpoint(cancelled)?;
    let result =
        importer::resolve_label_collisions(paired.into_iter().map(|t| (t, None)).collect())
            .into_iter()
            .map(|mut t| {
                t.collection = Some(Collection {
                    id: e.collection_id.clone(),
                    label: e.name.chars().take(48).collect(),
                });
                t
            })
            .collect();
    checkpoint(cancelled)?;
    Ok(result)
}
fn check_contributions(v: &Value) -> Result<(), String> {
    match contributions(v).len() {
        0 => Err("That extension does not contain color themes.".into()),
        41.. => Err("That extension contains too many color themes to import safely.".into()),
        _ => Ok(()),
    }
}

const USED_COLORS: &[&str] = &[
    "activityBar.background",
    "activityBarBadge.background",
    "badge.background",
    "button.background",
    "button.foreground",
    "contrastBorder",
    "descriptionForeground",
    "disabledForeground",
    "dropdown.background",
    "dropdown.border",
    "editor.background",
    "editor.foreground",
    "editor.findMatchBackground",
    "editor.findMatchForeground",
    "editor.findMatchHighlightBackground",
    "editor.findMatchHighlightForeground",
    "editor.selectionBackground",
    "editorCursor.foreground",
    "editorError.foreground",
    "editorGroup.border",
    "editorPane.background",
    "editorWarning.foreground",
    "editorWidget.background",
    "errorForeground",
    "focusBorder",
    "foreground",
    "input.border",
    "input.placeholderForeground",
    "list.activeSelectionBackground",
    "list.hoverBackground",
    "list.inactiveSelectionBackground",
    "menu.background",
    "panel.background",
    "panel.border",
    "progressBar.background",
    "quickInput.background",
    "scrollbarSlider.background",
    "sideBar.background",
    "sideBar.border",
    "sideBar.foreground",
    "terminal.background",
    "terminal.foreground",
    "terminal.selectionBackground",
    "terminalCursor.foreground",
    "textCodeBlock.background",
    "textLink.foreground",
];
#[cfg(test)]
mod tests;

pub fn stored_collection(
    doc: &crate::client_settings::ClientSettingsDoc,
    id: &str,
) -> Vec<ThemeDefinition> {
    super::library(doc)
        .into_iter()
        .filter(|t| t.collection.as_ref().is_some_and(|c| c.id == id))
        .collect()
}
pub fn replace_collection(
    doc: &crate::client_settings::ClientSettingsDoc,
    id: &str,
    themes: Vec<ThemeDefinition>,
    expected: &[ThemeDefinition],
) -> Result<serde_json::Map<String, Value>, String> {
    if stored_collection(doc, id) != expected {
        return Err(
            "Your installed themes changed while this package was downloading. Try again.".into(),
        );
    }
    if themes.is_empty()
        || themes
            .iter()
            .any(|t| t.collection.as_ref().is_none_or(|c| c.id != id))
        || themes.iter().map(|t| &t.id).collect::<BTreeSet<_>>().len() != themes.len()
    {
        return Err("That theme collection is invalid.".into());
    }
    let mut stored = match doc.entries().get(super::LIBRARY_KEY) {
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .cloned()
            .ok_or("Couldn’t read the theme library. Try again.")?,
    };
    let belongs = |v: &Value| v["collection"]["id"].as_str() == Some(id);
    let occupied: BTreeSet<_> = stored
        .iter()
        .filter(|v| !belongs(v))
        .filter_map(|v| v["id"].as_str())
        .collect();
    for t in &themes {
        if super::reserved(&t.id) || occupied.contains(t.id.as_str()) {
            return Err(format!(
                "A theme named \"{}\" is already installed.",
                t.label
            ));
        }
    }
    let insertion = stored.iter().position(belongs).unwrap_or(stored.len());
    stored.retain(|v| !belongs(v));
    stored.splice(
        insertion..insertion,
        themes.iter().map(|t| serde_json::to_value(t).unwrap()),
    );
    let mut current = doc.clone();
    if !current.entries().contains_key(super::LIBRARY_KEY) {
        current.apply_change(&crate::client_settings::ClientSettingChange::ThemeLibrary(
            json!([]),
        ));
    }
    let mut patch = super::remove(
        &current,
        &expected
            .iter()
            .filter(|old| !themes.iter().any(|t| t.id == old.id))
            .map(|t| t.id.clone())
            .collect::<Vec<_>>(),
    )?;
    patch.insert(super::LIBRARY_KEY.into(), Value::Array(stored));
    Ok(patch)
}

#[derive(Clone, Debug)]
pub struct MarketplaceImport {
    pub collection_id: String,
    pub expected: Vec<ThemeDefinition>,
    pub themes: Vec<ThemeDefinition>,
}
impl MarketplaceImport {
    pub fn prepare(
        &self,
        doc: &crate::client_settings::ClientSettingsDoc,
        choice: super::ImportChoice,
    ) -> Result<(serde_json::Map<String, Value>, Vec<ThemeDefinition>), String> {
        if matches!(choice, super::ImportChoice::Update) {
            Ok((
                replace_collection(
                    doc,
                    &self.collection_id,
                    self.themes.clone(),
                    &self.expected,
                )?,
                self.themes.clone(),
            ))
        } else {
            let mut session = importer::ImportSession::default();
            session.conflicts = self.themes.clone();
            let result = session.resolve_conflicts(doc, super::ImportChoice::KeepBoth);
            if let Some(error) = session.error {
                return Err(error);
            }
            let library = result
                .library
                .ok_or("A theme collection cannot be empty.")?;
            Ok((
                serde_json::Map::from_iter([(super::LIBRARY_KEY.into(), library)]),
                result.installed,
            ))
        }
    }
}
#[derive(Default)]
pub struct SearchSession {
    pub query: String,
    pub sort: String,
    pub shown: Option<String>,
    handled: Option<String>,
    pub installing: bool,
    interrupted: bool,
    pub epoch: u64,
    pub cancel_request: bool,
}
impl SearchSession {
    pub fn new() -> Self {
        Self {
            sort: "downloadCount".into(),
            ..Default::default()
        }
    }
    pub fn key(&self) -> String {
        format!("{}\0{}", self.query, self.sort)
    }
    pub fn set_query(&mut self, query: &str) -> bool {
        let query = query.trim();
        if query == self.query {
            return false;
        }
        self.query = query.into();
        true
    }
    pub fn settle(&mut self, force: bool) -> bool {
        self.cancel_request = false;
        if self.installing || self.query.is_empty() {
            return false;
        }
        let key = self.key();
        if !force && self.handled.as_ref() == Some(&key) && !self.interrupted {
            return false;
        }
        self.handled = Some(key.clone());
        self.interrupted = false;
        self.cancel_request = true;
        force || self.shown.as_ref() != Some(&key)
    }
    pub fn retire(&mut self) {
        self.epoch += 1;
    }
    pub fn clear(&mut self) {
        self.retire();
        self.shown = None;
        self.handled = None;
    }
    pub fn begin_install(&mut self) {
        self.retire();
        self.installing = true;
        self.interrupted = true;
    }
    pub fn end_install(&mut self) {
        self.installing = false;
    }
}
