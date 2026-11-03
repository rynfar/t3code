//! Theme library, decoding and palette application from `web/src/themePalette.ts`,
//! `ThemeSettings.tsx`, `appearanceContrast.ts` and `index.css`.
mod color;
pub mod editor;
pub mod importer;
#[cfg(test)]
mod tests;
mod vivid;
mod vscode;

use crate::client_settings::{AppearanceMode, ClientSettingsDoc, ThemePreference};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::LazyLock};
pub type Colors = BTreeMap<String, String>;
pub const LIBRARY_KEY: &str = "t3code:themes:v1";
pub const HALVES_KEY: &str = "t3code:theme-halves:v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub label: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeDefinition {
    pub id: String,
    #[serde(alias = "name")]
    pub label: String,
    pub appearance: String,
    pub colors: Colors,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variants: BTreeMap<String, Colors>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<Collection>,
    #[serde(default)]
    pub managed: bool,
}
impl ThemeDefinition {
    pub fn half(&self, dark: bool) -> Option<&Colors> {
        let mode = if dark { "dark" } else { "light" };
        if self.appearance == mode { Some(&self.colors) } else { self.variants.get(mode) }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    roles: Vec<String>,
    themes: Vec<ThemeDefinition>,
    recipes: Vec<Colors>,
    standard: Vec<Colors>,
    chip_accents: Colors,
    chip_contrast: f64,
    #[cfg(test)]
    expected: Vec<BTreeMap<String, u32>>,
}
static DATA: LazyLock<Data> = LazyLock::new(|| serde_json::from_str(include_str!("data.json")).expect("generated theme data"));
pub fn builtins() -> &'static [ThemeDefinition] {
    &DATA.themes
}
pub fn reserved(id: &str) -> bool {
    matches!(id, "system" | "light" | "dark" | "t3-chat-dark" | "t3-grove" | "t3-ocean" | "t3-ember" | "t3-iris")
        || builtins().iter().any(|t| t.id == id)
}
pub fn defaults(dark: bool) -> Colors {
    builtins()[0].half(dark).expect("T3 Chat half").clone()
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
fn valid_label(label: &str) -> bool {
    !label.trim().is_empty() && label.trim().chars().count() <= 48
}
pub fn id_from_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let mut id = String::new();
    for c in lower.chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c);
        } else if !id.ends_with('-') && !id.is_empty() {
            id.push('-');
        }
    }
    let id = id.trim_end_matches('-').chars().take(48).collect::<String>();
    if !id.is_empty() {
        return id;
    }
    if lower.is_empty() {
        return "custom-theme".into();
    }
    let mut hash = 2166136261u32;
    for c in lower.encode_utf16() {
        hash = (hash ^ u32::from(c)).wrapping_mul(16777619);
    }
    let mut n = hash;
    let mut digits = Vec::new();
    loop {
        digits.push(char::from_digit(n % 36, 36).unwrap());
        n /= 36;
        if n == 0 {
            break;
        }
    }
    format!("theme-{}", digits.into_iter().rev().collect::<String>())
}
fn overrides(v: &Value, strict: bool) -> Result<Colors, String> {
    let object = v.as_object().ok_or("Theme colors must be objects.")?;
    let mut colors = Colors::new();
    for (role, value) in object {
        if !DATA.roles.contains(role) {
            if strict {
                return Err(format!("\"{role}\" is not a supported theme color role."));
            }
            continue;
        }
        match value.as_str().and_then(|s| color::parse(s).ok().map(|_| s.to_owned())) {
            Some(c) => {
                colors.insert(role.clone(), c);
            }
            None if strict => {
                return Err(format!("The color for \"{role}\" must be a literal CSS color such as oklch(0.62 0.2 280)."));
            }
            _ => {}
        }
    }
    if strict && colors.is_empty() {
        return Err("Add at least one color role to the theme file.".into());
    }
    Ok(colors)
}
#[cfg(test)]
thread_local! { static STORED_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
pub fn decode(value: &Value, stored: bool) -> Result<ThemeDefinition, String> {
    #[cfg(test)]
    if stored { STORED_DECODES.with(|n|n.set(n.get()+1)); }
    let v = value.as_object().ok_or("Theme files must contain a JSON object.")?;
    if !stored && v.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("This theme file uses an unsupported version. Expected 1.".into());
    }
    let name = v
        .get(if stored { "label" } else { "name" })
        .and_then(Value::as_str)
        .filter(|s| valid_label(s))
        .ok_or("Theme files need a name (48 characters or fewer).")?;
    let appearance = v
        .get("appearance")
        .and_then(Value::as_str)
        .filter(|s| matches!(*s, "light" | "dark"))
        .ok_or("Theme files need an appearance of \"light\" or \"dark\".")?;
    let id = v.get("id").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| id_from_name(name));
    if !valid_id(&id) {
        return Err("Theme ids may only contain lowercase letters, numbers, and hyphens.".into());
    }
    if reserved(&id) {
        return Err(format!("The theme id \"{id}\" is reserved."));
    }
    let mut colors = defaults(appearance == "dark");
    colors.extend(overrides(v.get("colors").ok_or("Theme files need a colors object.")?, !stored)?);
    let mut variants = BTreeMap::new();
    if let Some(value) = v.get("variants") {
        for (mode, c) in value.as_object().ok_or("Theme variants must be an object.")? {
            if !matches!(mode.as_str(), "light" | "dark") {
                return Err("Theme variants may only be named \"light\" or \"dark\".".into());
            }
            if mode == appearance {
                if stored {
                    continue;
                }
                return Err(format!("Theme variants must not repeat the base appearance \"{appearance}\"."));
            }
            let mut colors = defaults(mode == "dark");
            colors.extend(overrides(c, !stored)?);
            variants.insert(mode.clone(), colors);
        }
    }
    let collection = if let Some(c) = v.get("collection") {
        let c = serde_json::from_value::<Collection>(c.clone()).map_err(|_| "Theme collections need a valid id and label.")?;
        if c.id.is_empty()
            || c.id.len() > 128
            || !c.id.as_bytes()[0].is_ascii_alphanumeric()
            || !c.id.bytes().all(|b| b.is_ascii_alphanumeric() || b".:-".contains(&b))
            || !valid_label(&c.label)
        {
            return Err("Theme collections need a valid id and label.".into());
        }
        Some(Collection { label: c.label.trim().into(), ..c })
    } else {
        None
    };
    Ok(ThemeDefinition {
        id,
        label: name.trim().into(),
        appearance: appearance.into(),
        colors,
        variants,
        collection,
        managed: v.get("managed") == Some(&Value::Bool(true)),
    })
}
pub fn library(doc: &ClientSettingsDoc) -> Vec<ThemeDefinition> {
    let mut out = Vec::new();
    if let Some(themes) = doc.entries().get(LIBRARY_KEY).and_then(Value::as_array) {
        for value in themes {
            if let Ok(t) = decode(value, true)
                && !out.iter().any(|t2: &ThemeDefinition| t2.id == t.id)
            {
                out.push(t);
            }
        }
    }
    out
}
pub fn install(doc: &ClientSettingsDoc, theme: &ThemeDefinition) -> Result<Value, String> {
    let mut stored = match doc.entries().get(LIBRARY_KEY) {
        None => Vec::new(),
        Some(v) => v.as_array().cloned().ok_or("Couldn’t read the theme library. Try again.")?,
    };
    if reserved(&theme.id) || stored.iter().any(|t| t.get("id").and_then(Value::as_str) == Some(&theme.id)) {
        return Err(format!("A theme named \"{}\" is already installed.", theme.label));
    }
    stored.push(serde_json::to_value(theme).map_err(|e| e.to_string())?);
    Ok(Value::Array(stored))
}
pub fn duplicate(doc: &ClientSettingsDoc, theme: &ThemeDefinition) -> ThemeDefinition {
    let label = format!("{} copy", theme.label.chars().take(43).collect::<String>());
    let base = id_from_name(&label);
    let mut id = base.clone();
    let mut n = 2;
    while reserved(&id)
        || doc
            .entries()
            .get(LIBRARY_KEY)
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|t| t.get("id") == Some(&Value::String(id.clone()))))
    {
        id = format!("{}-{n}", base.chars().take(43).collect::<String>());
        n += 1;
    }
    ThemeDefinition { id, label, collection: None, ..theme.clone() }
}
pub fn environment(values: &[Value]) -> Vec<ThemeDefinition> {
    values
        .iter()
        .filter_map(|v| {
            let id = v.get("id")?.as_str()?;
            if reserved(id) || !valid_id(id) {
                return None;
            }
            let mut file = v.clone();
            file["version"] = 1.into();
            let dark = v.get("appearance")?.as_str()? == "dark";
            let mut colors = defaults(dark);
            let seeds = v.get("canvas").and_then(Value::as_str).zip(v.get("accent").and_then(Value::as_str));
            let explicit = v.get("colors").and_then(|c| overrides(c, false).ok()).unwrap_or_default();
            let variants = v.get("variants").and_then(Value::as_object);
            if seeds.is_none()
                && explicit.is_empty()
                && variants.is_none_or(|a| a.values().all(|v| overrides(v, false).unwrap_or_default().is_empty()))
            {
                return None;
            }
            if let Some((canvas, accent)) = seeds {
                colors = vivid::derive(dark, canvas, accent)?;
            }
            colors.extend(explicit);
            file["colors"] = serde_json::to_value(colors).ok()?;
            if let Some(variants) = variants {
                let mut valid = serde_json::Map::new();
                for (mode, c) in variants {
                    if mode == if dark { "dark" } else { "light" } {
                        continue;
                    }
                    let o = overrides(c, false).ok()?;
                    if !o.is_empty() {
                        valid.insert(mode.clone(), serde_json::to_value(o).ok()?);
                    }
                }
                file["variants"] = Value::Object(valid);
            }
            decode(&file, false).ok()
        })
        .collect()
}
pub fn resolve<'a>(id: &str, custom: &'a [ThemeDefinition], environment: &'a [ThemeDefinition]) -> Option<&'a ThemeDefinition> {
    builtins().iter().chain(custom).chain(environment).find(|t| t.id == id)
}
pub fn selected_half<'a>(theme: &'a ThemePreference, halves: &'a BTreeMap<String, String>, dark: bool) -> Option<&'a str> {
    halves.get(if dark { "dark" } else { "light" }).map(String::as_str).or_else(|| theme.named_id())
}
pub fn palette(roles: Option<&Colors>, dark: bool, contrast: u32, glass: u32) -> Result<BTreeMap<String, u32>, String> {
    palette_for(None, roles, dark, contrast, glass)
}
pub fn palette_for(
    id: Option<&str>,
    roles: Option<&Colors>,
    dark: bool,
    contrast: u32,
    glass: u32,
) -> Result<BTreeMap<String, u32>, String> {
    let empty = Colors::new();
    let recipes = &DATA.recipes[usize::from(dark)
        + if id == Some("t3-chat") {
            4
        } else if roles.is_some() {
            2
        } else {
            0
        }];
    let c = contrast.clamp(50, 200);
    let mut out = BTreeMap::new();
    for (token, expression) in recipes {
        let expression = expression
            .replace("$base", &c.min(100).to_string())
            .replace("$boost", &c.saturating_sub(100).to_string())
            .replace("$borderBoost", &(f64::from(c.saturating_sub(100)) / 4.).to_string())
            .replace("$glass", &glass.clamp(40, 100).to_string());
        out.insert(token.clone(), color::packed(color::evaluate(&expression, roles.unwrap_or(&empty), dark)?));
    }
    let roles = roles.unwrap_or(&DATA.standard[usize::from(dark)]);
    let canvas = color::mapped(color::parse(&roles["canvas"])?, false);
    let on_canvas = |key: &str| -> Result<color::Color, String> {
        let c = color::mapped(color::parse(&roles[key])?, false);
        let alpha = c[3];
        let mut top = c;
        top[3] = 1.;
        let mut c = color::mix(top, canvas, alpha, "srgb");
        c[3] = 1.;
        Ok(c)
    };
    let mut surfaces = vec![canvas, on_canvas("surface")?, on_canvas("surfaceRaised")?];
    let message = on_canvas("messageSurface")?;
    if (color::luminance(message) < 0.179) == (color::luminance(canvas) < 0.179) {
        surfaces.push(message);
    }
    let worst = |c| surfaces.iter().map(|s| color::contrast(c, *s)).fold(f64::INFINITY, f64::min);
    let black = worst([0., 0., 0., 1.]);
    let white = worst([1.; 4]);
    let ratio = DATA.chip_contrast.min(black.max(white));
    for (kind, accent) in DATA
        .chip_accents
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .chain(std::iter::once(("invalid", roles["errorForeground"].as_str())))
    {
        let c = color::solve(color::lch(color::parse(accent)?), &surfaces, ratio, white > black);
        out.insert(format!("context_chip_{}", kind.replace('-', "_")), color::packed(c));
    }
    Ok(out)
}
pub fn standard(dark: bool) -> Colors {
    DATA.standard[usize::from(dark)].clone()
}
pub fn halves(doc: &ClientSettingsDoc) -> BTreeMap<String, String> {
    doc.entries().get(HALVES_KEY).and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
}
pub fn mode_dark(mode: AppearanceMode, system: bool) -> bool {
    match mode {
        AppearanceMode::System => system,
        AppearanceMode::Dark => true,
        AppearanceMode::Light => false,
    }
}

pub fn assign_half(doc: &ClientSettingsDoc, dark: bool, id: Option<&str>) -> serde_json::Map<String, Value> {
    use crate::client_settings::{THEME_APPEARANCE_MODE_KEY, THEME_KEY};
    let mode = if dark { "dark" } else { "light" };
    let other = if dark { "light" } else { "dark" };
    let mut halves = halves(doc);
    let mut patch = serde_json::Map::new();
    if id.is_none()
        && let Some(base) = doc.theme().named_id()
    {
        let other_id = halves.get(other).cloned().unwrap_or_else(|| base.to_owned());
        halves.clear();
        halves.insert(other.into(), other_id);
        patch.insert(THEME_KEY.into(), doc.appearance_mode().as_str().into());
        patch.insert(THEME_APPEARANCE_MODE_KEY.into(), doc.appearance_mode().as_str().into());
    } else if let Some(id) = id {
        halves.insert(mode.into(), id.into());
    } else {
        halves.remove(mode);
    }
    patch.insert(HALVES_KEY.into(), serde_json::json!(halves));
    patch
}
pub fn remove(doc: &ClientSettingsDoc, ids: &[String]) -> Result<serde_json::Map<String, Value>, String> {
    use crate::client_settings::THEME_KEY;
    let stored = doc.entries().get(LIBRARY_KEY).and_then(Value::as_array).ok_or("Couldn’t read the theme library. Try again.")?;
    let mut patch = serde_json::Map::new();
    patch.insert(
        LIBRARY_KEY.into(),
        Value::Array(
            stored.iter().filter(|t| t.get("id").and_then(Value::as_str).is_none_or(|id| !ids.iter().any(|i| i == id))).cloned().collect(),
        ),
    );
    let mut halves = halves(doc);
    halves.retain(|_, id| !ids.contains(id));
    if doc.theme().named_id().is_some_and(|id| ids.iter().any(|i| i == id)) {
        patch.insert(THEME_KEY.into(), doc.appearance_mode().as_str().into());
    }
    patch.insert(HALVES_KEY.into(), serde_json::json!(halves));
    Ok(patch)
}

pub fn swatch(colors: &Colors) -> [u32; 4] {
    ["canvas", "surface", "messageAction", "text"].map(|role| color::packed(color::parse(&colors[role]).unwrap_or([0.; 4])))
}

pub fn serialize(theme: &ThemeDefinition) -> String {
    let mut v = serde_json::to_value(theme).expect("serializable theme");
    v["version"] = 1.into();
    v["name"] = theme.label.clone().into();
    v.as_object_mut().unwrap().remove("label");
    format!("{}\n", serde_json::to_string_pretty(&v).expect("serializable theme"))
}

pub fn half_theme<'a>(
    theme: &ThemePreference,
    halves: &BTreeMap<String, String>,
    dark: bool,
    custom: &'a [ThemeDefinition],
    environment: &'a [ThemeDefinition],
) -> Option<&'a ThemeDefinition> {
    halves
        .get(if dark { "dark" } else { "light" })
        .and_then(|id| resolve(id, custom, environment))
        .filter(|t| t.half(dark).is_some())
        .or_else(|| theme.named_id().and_then(|id| resolve(id, custom, environment)))
}
pub fn resolved_dark(
    theme: &ThemePreference,
    halves: &BTreeMap<String, String>,
    requested: bool,
    custom: &[ThemeDefinition],
    environment: &[ThemeDefinition],
) -> bool {
    half_theme(theme, halves, requested, custom, environment)
        .filter(|t| t.half(requested).is_none())
        .map_or(requested, |t| t.appearance == "dark")
}

pub fn import(source: &str) -> Result<ThemeDefinition, String> {
    let value = jsonc_parser::parse_to_serde_value(
        source,
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
    .map_err(|_| "That theme file is not valid JSON.")?
    .ok_or("Theme files must contain a JSON object.")?;
    let vscode = value.get("version") != Some(&Value::from(1))
        && (value.get("tokenColors").is_some_and(Value::is_array)
            || value.get("colors").and_then(Value::as_object).is_some_and(|o| o.keys().any(|k| k.contains('.'))));
    if vscode {
        vscode::convert(&value)
    } else {
        decode(&serde_json::from_str::<Value>(source).map_err(|_| "That theme file is not valid JSON.")?, false)
    }
}

#[derive(Clone, Copy)]
pub enum ImportChoice {
    Update,
    KeepBoth,
}
pub fn resolve_import(doc: &ClientSettingsDoc, theme: &ThemeDefinition, choice: ImportChoice) -> Result<Value, String> {
    match choice {
        ImportChoice::Update => {
            let mut stored =
                doc.entries().get(LIBRARY_KEY).and_then(Value::as_array).cloned().ok_or("Couldn’t read the theme library. Try again.")?;
            let existing = library(doc).into_iter().find(|t| t.id == theme.id).ok_or("The theme is no longer installed.")?;
            let updated = ThemeDefinition { collection: existing.collection.or_else(|| theme.collection.clone()), ..theme.clone() };
            let mut replaced = false;
            stored.retain_mut(|value| {
                if value.get("id").and_then(Value::as_str) != Some(&theme.id) {
                    return true;
                }
                if replaced {
                    return false;
                }
                *value = serde_json::to_value(&updated).expect("serializable theme");
                replaced = true;
                true
            });
            Ok(Value::Array(stored))
        }
        ImportChoice::KeepBoth => install(doc, &importer::versioned_copy(doc, theme, None)?),
    }
}
pub fn select_collection(doc: &ClientSettingsDoc, themes: &[ThemeDefinition]) -> serde_json::Map<String, Value> {
    if themes.len() == 1 {
        let t = &themes[0];
        if t.variants.is_empty() {
            return assign_half(doc, t.appearance == "dark", Some(&t.id));
        }
        return doc.clone().apply_change(&crate::client_settings::ClientSettingChange::Theme(ThemePreference::Named(t.id.clone())));
    }
    let mut selected = halves(doc);
    for dark in [false, true] {
        if let Some(t) = themes.iter().find(|t| t.half(dark).is_some()) {
            selected.insert(if dark { "dark" } else { "light" }.into(), t.id.clone());
        }
    }
    serde_json::Map::from_iter([(HALVES_KEY.into(), serde_json::json!(selected))])
}

pub fn wireframe(colors:&Colors)->[u32;7] {
    ["canvas","sidebar","surface","accentSurface","messageSurface","messageAction","text"].map(|role|color::packed(color::parse(&colors[role]).unwrap_or([0.;4])))
}
