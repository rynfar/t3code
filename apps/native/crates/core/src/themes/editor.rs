//! Editing sessions from `ThemeEditorPanel.tsx`, `ThemeEditorHost.tsx` and `themeEditorStore.ts`.
use super::{Colors, ThemeDefinition, color, decode, id_from_name, library, standard, vivid};
use crate::client_settings::{ClientSettingChange, ClientSettingsDoc, ThemePreference};
use serde_json::{Map, Value, json};

mod family;
pub mod picker;
pub use family::{FAMILIES, Family, update_family};

#[derive(Clone, Debug, PartialEq)]
struct Draft {
    colors: [Colors; 2],
    dirty: [bool; 2],
    regenerate: bool,
}
#[derive(Clone, Debug)]
pub struct Session {
    pub name: String,
    pub dark: bool,
    pub advanced: bool,
    pub error: Option<String>,
    editing: Option<ThemeDefinition>,
    draft: Draft,
    color_snapshot: Option<Draft>,
    merge_key: Option<String>,
}
#[derive(Debug)]
pub struct Save {
    pub theme: ThemeDefinition,
    pub patch: Map<String, Value>,
    pub created: bool,
    pub merged_dark: Option<bool>,
}
fn mode(dark: bool) -> &'static str {
    if dark { "dark" } else { "light" }
}
fn managed(dark: bool, colors: &Colors) -> Colors {
    let defaults = standard(dark);
    let seed = |role: &str| {
        if color::parse(&colors[role]).is_ok() {
            &colors[role]
        } else {
            &defaults[role]
        }
    };
    vivid::derive(dark, seed("canvas"), seed("accent")).expect("valid seeds")
}
impl Session {
    pub fn new(editing: Option<ThemeDefinition>, seed: Option<&ThemeDefinition>, name: Option<String>, dark: bool) -> Self {
        let source = editing.as_ref().or(seed);
        let mut colors = [standard(false), standard(true)];
        if let Some(t) = source {
            for d in [false, true] {
                if let Some(c) = t.half(d) {
                    colors[usize::from(d)] = c.clone();
                }
            }
        }
        let dark = source.filter(|t| t.half(dark).is_none()).map_or(dark, |t| t.appearance == "dark");
        let advanced = source.is_some_and(|t| !t.managed);
        Self {
            name: editing.as_ref().map(|t| t.label.clone()).or(name).unwrap_or_default(),
            dark,
            advanced,
            error: None,
            draft: Draft {
                colors,
                dirty: [false; 2],
                regenerate: advanced,
            },
            editing,
            color_snapshot: None,
            merge_key: None,
        }
    }
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }
    pub fn colors(&self) -> &Colors {
        &self.draft.colors[usize::from(self.dark)]
    }
    pub fn merge_target(&self, doc: &ClientSettingsDoc) -> Option<ThemeDefinition> {
        let normalized = self.name.trim().to_lowercase();
        if normalized.is_empty() {
            return None;
        }
        library(doc).into_iter().find(|t| {
            self.editing.as_ref().is_none_or(|e| e.id != t.id)
                && (t.id == id_from_name(&self.name) || t.label.trim().to_lowercase() == normalized)
        })
    }
    pub fn set_name(&mut self, name: String, doc: &ClientSettingsDoc) {
        self.name = name;
        self.error = None;
        let target = self.merge_target(doc).filter(|_| !self.is_editing());
        let key = target
            .as_ref()
            .map(|t| format!("{}:{}:{}", t.id, t.half(false).is_some(), t.half(true).is_some()));
        if self.merge_key != key {
            if let Some(t) = target
                && t.variants.is_empty()
                && t.half(self.dark).is_some()
            {
                self.dark = !self.dark;
            }
            self.merge_key = key;
        }
    }
    pub fn lock_reason(&self, dark: bool, doc: &ClientSettingsDoc) -> Option<String> {
        if let Some(t) = &self.editing {
            return t.half(dark).is_none().then(|| {
                format!(
                    "“{}” has no {} palette. Create a theme with the same name to add one.",
                    t.label,
                    mode(dark)
                )
            });
        }
        self.merge_target(doc)
            .filter(|t| t.half(dark).is_some())
            .map(|t| format!("“{}” already has a {} palette.", t.label, mode(dark)))
    }
    pub fn set_appearance(&mut self, dark: bool, doc: &ClientSettingsDoc) {
        if self.lock_reason(dark, doc).is_none() {
            self.dark = dark;
        }
    }
    pub fn update_color(&mut self, role: &str, value: String) {
        if !self.colors().contains_key(role) {
            return;
        }
        let i = usize::from(self.dark);
        if self.advanced {
            self.draft.colors[i] = update_family(self.dark, self.colors(), role, &value);
            self.draft.regenerate = true;
        } else {
            self.draft.colors[i].insert(role.into(), value.clone());
            if matches!(role, "canvas" | "accent") && color::parse(&value).is_ok() {
                self.draft.colors[i] = managed(self.dark, self.colors());
                self.draft.dirty[i] = true;
            }
        }
    }
    pub fn set_advanced(&mut self, advanced: bool) {
        self.advanced = advanced;
        if advanced || !self.draft.regenerate {
            return;
        }
        for dark in [false, true] {
            if dark == self.dark || self.editing.as_ref().is_some_and(|t| !t.variants.is_empty()) {
                let i = usize::from(dark);
                self.draft.colors[i] = managed(dark, &self.draft.colors[i]);
                self.draft.dirty[i] = true;
            }
        }
        self.draft.regenerate = false;
    }
    pub fn start_color_edit(&mut self) {
        self.color_snapshot = Some(self.draft.clone());
    }
    pub fn end_color_edit(&mut self, cancel: bool) {
        if let Some(snapshot) = self.color_snapshot.take()
            && cancel
        {
            self.draft = snapshot;
        }
    }
    pub fn save_label(&self, doc: &ClientSettingsDoc) -> String {
        match (self.is_editing(), self.merge_target(doc)) {
            (true, Some(t)) => format!("Merge into “{}”", t.label),
            (true, None) => "Save changes".into(),
            (false, Some(_)) => format!("Add {} palette", mode(self.dark)),
            (false, None) => "Create theme".into(),
        }
    }
    pub fn prepare_save(&self, doc: &ClientSettingsDoc) -> Result<Save, String> {
        if self.name.trim().is_empty() {
            return Err("Name your theme first.".into());
        }
        let mut colors = self.draft.colors.clone();
        if !self.advanced {
            for dark in [false, true] {
                let i = usize::from(dark);
                if self.draft.dirty[i] {
                    colors[i] = managed(dark, &colors[i]);
                }
            }
        }
        let target = self.merge_target(doc);
        let mut retired = None;
        let mut merged_dark = None;
        let mut theme = if let Some(target) = &target {
            let modes: Vec<_> = [false, true]
                .into_iter()
                .filter(|d| self.editing.as_ref().map_or(*d == self.dark, |t| t.half(*d).is_some()))
                .collect();
            if let Some(collision) = modes.iter().find(|d| target.half(**d).is_some()) {
                return Err(if self.is_editing() {
                    format!("“{}” already has a {} palette. Pick another name.", target.label, mode(*collision))
                } else {
                    format!("“{}” already has light and dark palettes. Pick another name.", target.label)
                });
            }
            let mut t = target.clone();
            for d in &modes {
                t.variants.insert(mode(*d).into(), colors[usize::from(*d)].clone());
            }
            t.managed = t.managed && !self.advanced;
            merged_dark = modes.first().copied();
            retired = self.editing.as_ref().map(|t| t.id.clone());
            t
        } else if let Some(t) = &self.editing {
            if !library(doc).iter().any(|existing| existing.id == t.id) {
                return Err(format!("The theme \"{}\" is not installed.", t.label));
            }
            let mut t = t.clone();
            t.label = self.name.clone();
            t.colors = colors[usize::from(t.appearance == "dark")].clone();
            for (m, c) in &mut t.variants {
                *c = colors[usize::from(m == "dark")].clone();
            }
            t.managed = !self.advanced;
            t
        } else {
            ThemeDefinition {
                id: id_from_name(&self.name),
                label: self.name.clone(),
                appearance: mode(self.dark).into(),
                colors: colors[usize::from(self.dark)].clone(),
                variants: Default::default(),
                collection: None,
                managed: !self.advanced,
            }
        };
        let mut file = serde_json::to_value(&theme).expect("theme");
        file["version"] = 1.into();
        file["name"] = theme.label.clone().into();
        theme = decode(&file, false)?;
        let mut stored = match doc.entries().get(super::LIBRARY_KEY) {
            None => Vec::new(),
            Some(v) => v.as_array().cloned().ok_or("Couldn’t read the theme library. Try again.")?,
        };
        let update = self.is_editing() || target.is_some();
        if !update && stored.iter().any(|v| v.get("id").and_then(Value::as_str) == Some(&theme.id)) {
            return Err(format!("A theme named \"{}\" is already installed.", theme.label));
        }
        let mut replaced = false;
        stored.retain_mut(|v| {
            let id = v.get("id").and_then(Value::as_str);
            if retired.as_deref().is_some_and(|r| Some(r) == id) {
                return false;
            }
            if id == Some(&theme.id) {
                if replaced {
                    return false;
                }
                *v = json!(theme);
                replaced = true;
            }
            true
        });
        if !replaced {
            stored.push(json!(theme));
        }
        let mut patch = Map::from_iter([(super::LIBRARY_KEY.into(), json!(stored))]);
        let created = !self.is_editing() && target.is_none();
        if created || merged_dark.is_some() {
            patch.extend(
                doc.clone()
                    .apply_change(&ClientSettingChange::Theme(ThemePreference::Named(theme.id.clone()))),
            );
        }
        Ok(Save {
            theme,
            patch,
            created,
            merged_dark,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub dark: bool,
    pub colors: Colors,
}
impl Preview {
    pub fn new(dark: bool, colors: &Colors) -> Self {
        Self {
            dark,
            colors: colors.clone(),
        }
    }
    pub fn update(&mut self, dark: bool, colors: &Colors) -> bool {
        let mut changed = self.dark != dark;
        self.dark = dark;
        for (role, value) in colors {
            if self.colors.get(role) != Some(value) && color::parse(value).is_ok() {
                self.colors.insert(role.clone(), value.clone());
                changed = true;
            }
        }
        changed
    }
}
