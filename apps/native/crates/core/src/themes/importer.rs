//! File import behavior from `ThemeImportDialog.tsx` and `vscodeThemeImport.ts`.
use super::*;

pub fn pair_vscode_themes(themes: Vec<ThemeDefinition>) -> Vec<ThemeDefinition> {
    struct Group {
        label: String,
        order: usize,
        light: Vec<ThemeDefinition>,
        dark: Vec<ThemeDefinition>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut out = Vec::new();
    for (order, theme) in themes.into_iter().enumerate() {
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let stripped = theme
            .label
            .split_inclusive(|c: char| !word(c))
            .map(|part| {
                let token = part.trim_end_matches(|c: char| !word(c));
                if token.eq_ignore_ascii_case("light") || token.eq_ignore_ascii_case("dark") {
                    format!(" {}", &part[token.len()..])
                } else {
                    part.to_owned()
                }
            })
            .collect::<String>();
        let key = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
        if !theme.variants.is_empty()
            || key == theme.label
            || !key.chars().any(|c| c.is_ascii_alphanumeric())
        {
            out.push((order, theme));
            continue;
        }
        let index = groups
            .iter()
            .position(|g| g.label == key)
            .unwrap_or_else(|| {
                groups.push(Group {
                    label: key,
                    order,
                    light: Vec::new(),
                    dark: Vec::new(),
                });
                groups.len() - 1
            });
        let Group { light, dark, .. } = &mut groups[index];
        if theme.appearance == "light" {
            light.push(theme);
        } else {
            dark.push(theme);
        }
    }
    for Group {
        label: key,
        order,
        light,
        dark,
    } in groups
    {
        if light.len() == 1 && dark.len() == 1 {
            let file = serde_json::json!({"version":1,"name":key,"appearance":"light","colors":light[0].colors,"variants":{"dark":dark[0].colors}});
            if let Ok(paired) = decode(&file, false) {
                out.push((order, paired));
                continue;
            }
        }
        out.extend(light.into_iter().chain(dark).map(|t| (order, t)));
    }
    out.sort_by_key(|(order, _)| *order);
    out.into_iter().map(|(_, theme)| theme).collect()
}
pub fn resolve_label_collisions(
    entries: Vec<(ThemeDefinition, Option<String>)>,
) -> Vec<ThemeDefinition> {
    let mut counts = BTreeMap::new();
    for (theme, _) in &entries {
        *counts.entry(theme.id.clone()).or_insert(0) += 1;
    }
    let mut seen = std::collections::BTreeSet::new();
    entries
        .into_iter()
        .map(|(theme, source)| {
            let theme = if counts[&theme.id] > 1 {
                source
                    .as_deref()
                    .map(|s| humanize(s.rsplit_once('.').map_or(s, |(stem, _)| stem)))
                    .filter(|name| {
                        !name.is_empty() && name.to_lowercase() != theme.label.to_lowercase()
                    })
                    .and_then(|name| rename(&theme, &name).ok())
                    .unwrap_or(theme)
            } else {
                theme
            };
            if seen.insert(theme.id.clone()) {
                return theme;
            }
            for suffix in 2..100 {
                if let Ok(candidate) = rename(&theme, &format!("{} {suffix}", theme.label))
                    && seen.insert(candidate.id.clone())
                {
                    return candidate;
                }
            }
            theme
        })
        .collect()
}

pub const MAX_FILE_BYTES: u64 = 256 * 1024;
pub const READ_FAILURE: &str = "Could not read that file. Paste the JSON below instead.";
pub fn oversized(bytes: u64) -> Option<String> {
    let size = if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024. * 1024.))
    } else if bytes >= 1024 {
        format!("{} KB", (bytes as f64 / 1024.).round())
    } else {
        format!("{bytes} bytes")
    };
    (bytes > MAX_FILE_BYTES).then(|| format!("That file is {size}. Theme files are only a few KB, so this one was not read (limit 256 KB)."))
}
pub fn humanize(raw: &str) -> String {
    let s = raw.trim();
    if s.contains(char::is_whitespace) || !s.contains(['-', '_', '.']) {
        return s.into();
    }
    s.split(['-', '_', '.'])
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut c = s.chars();
            c.next()
                .unwrap()
                .to_uppercase()
                .chain(c)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}
fn rename(theme: &ThemeDefinition, name: &str) -> Result<ThemeDefinition, String> {
    decode(
        &serde_json::json!({"version":1,"name":name.chars().take(48).collect::<String>(),"appearance":theme.appearance,"colors":theme.colors,"variants":theme.variants,"managed":theme.managed}),
        false,
    )
}
pub struct Batch {
    pub themes: Vec<ThemeDefinition>,
    pub failures: Vec<String>,
}
pub fn parse_batch(files: Vec<(String, Result<String, String>)>) -> Batch {
    let mut entries = Vec::new();
    let mut failures = Vec::new();
    for (name, text) in files {
        match text.and_then(|s| import(&s)) {
            Ok(theme) => entries.push((theme, Some(name))),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    Batch {
        themes: pair_vscode_themes(resolve_label_collisions(entries)),
        failures,
    }
}
#[derive(Clone, Default)]
pub struct ImportSession {
    pub json: String,
    pub file_name: Option<String>,
    pub error: Option<String>,
    pub reading: bool,
    pub conflicts: Vec<ThemeDefinition>,
    request: u64,
}
pub struct Installed {
    pub library: Option<Value>,
    pub installed: Vec<ThemeDefinition>,
    pub close: bool,
}
impl ImportSession {
    pub fn begin_read(&mut self) -> u64 {
        self.request += 1;
        self.reading = true;
        self.request
    }
    pub fn cancel(&mut self) {
        self.request += 1;
        self.reading = false;
    }
    pub fn current(&self, request: u64) -> bool {
        self.request == request
    }
    pub fn finish_single(
        &mut self,
        request: u64,
        name: String,
        result: Result<String, String>,
    ) -> bool {
        if !self.current(request) {
            return false;
        }
        self.reading = false;
        match result {
            Ok(text) => {
                self.json = text;
                self.file_name = Some(name);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
    pub fn finish_batch(
        &mut self,
        request: u64,
        batch: Batch,
        doc: &ClientSettingsDoc,
    ) -> Option<Installed> {
        if !self.current(request) {
            return None;
        }
        self.reading = false;
        let mut mutation = LibraryMutation::new(doc);
        let mut failures = batch.failures;
        let mut installed = Vec::new();
        self.conflicts.clear();
        for theme in batch.themes {
            if mutation
                .as_ref()
                .is_ok_and(|m| m.existing.contains_key(&theme.id))
            {
                self.conflicts.push(theme);
                continue;
            }
            match mutation
                .as_mut()
                .map_err(|e| e.clone())
                .and_then(|m| m.add(theme.clone()))
            {
                Ok(()) => installed.push(theme),
                Err(e) => failures.push(format!("{}: {e}", theme.label)),
            }
        }
        let value = if installed.is_empty() {
            None
        } else {
            mutation.ok().map(LibraryMutation::finish)
        };
        self.error = (!failures.is_empty()).then(|| failures.join("; "));
        let close = self.conflicts.is_empty() && !installed.is_empty() && self.error.is_none();
        Some(Installed {
            library: value,
            installed,
            close,
        })
    }
    pub fn resolve_conflicts(
        &mut self,
        doc: &ClientSettingsDoc,
        choice: ImportChoice,
    ) -> Installed {
        let preferred = (self.conflicts.len() == 1)
            .then(|| {
                self.file_name
                    .as_deref()
                    .map(|name| humanize(name.rsplit_once('.').map_or(name, |(stem, _)| stem)))
            })
            .flatten();
        let mut mutation = LibraryMutation::new(doc);
        let mut installed = Vec::new();
        let mut failures = Vec::new();
        for theme in self.conflicts.drain(..) {
            let resolved = mutation
                .as_mut()
                .map_err(|e| e.clone())
                .and_then(|m| match choice {
                    ImportChoice::Update => m.update(theme.clone()),
                    ImportChoice::KeepBoth => {
                        let candidate = copy_with_ids(&theme, preferred.as_deref(), |id| {
                            m.existing.contains_key(id)
                        })?;
                        m.add(candidate.clone())?;
                        Ok(candidate)
                    }
                });
            match resolved {
                Ok(t) => installed.push(t),
                Err(e) => failures.push(format!("{}: {e}", theme.label)),
            }
        }
        let value = if installed.is_empty() {
            None
        } else {
            mutation.ok().map(LibraryMutation::finish)
        };
        if !failures.is_empty() {
            let mut errors = self.error.take().into_iter().collect::<Vec<_>>();
            errors.extend(failures);
            self.error = Some(errors.join("; "));
        }
        Installed {
            library: value,
            installed,
            close: self.error.is_none(),
        }
    }
}
struct LibraryMutation {
    stored: Vec<Value>,
    existing: BTreeMap<String, ThemeDefinition>,
    ids: std::collections::BTreeSet<String>,
    added: Vec<ThemeDefinition>,
    updated: BTreeMap<String, ThemeDefinition>,
}
impl LibraryMutation {
    fn new(doc: &ClientSettingsDoc) -> Result<Self, String> {
        let stored = match doc.entries().get(LIBRARY_KEY) {
            None => Vec::new(),
            Some(v) => v
                .as_array()
                .cloned()
                .ok_or("Couldn’t read the theme library. Try again.")?,
        };
        let ids = stored
            .iter()
            .filter_map(|v| v["id"].as_str().map(str::to_owned))
            .collect();
        let existing = library(doc)
            .into_iter()
            .map(|t| (t.id.clone(), t))
            .collect();
        Ok(Self {
            stored,
            existing,
            ids,
            added: Vec::new(),
            updated: BTreeMap::new(),
        })
    }
    fn add(&mut self, theme: ThemeDefinition) -> Result<(), String> {
        if reserved(&theme.id) || !self.ids.insert(theme.id.clone()) {
            return Err(format!(
                "A theme named \"{}\" is already installed.",
                theme.label
            ));
        }
        self.existing.insert(theme.id.clone(), theme.clone());
        self.added.push(theme);
        Ok(())
    }
    fn update(&mut self, theme: ThemeDefinition) -> Result<ThemeDefinition, String> {
        let previous = self
            .existing
            .get(&theme.id)
            .ok_or("The theme is no longer installed.")?;
        let theme = ThemeDefinition {
            collection: previous.collection.clone().or(theme.collection.clone()),
            ..theme
        };
        self.existing.insert(theme.id.clone(), theme.clone());
        self.updated.insert(theme.id.clone(), theme.clone());
        Ok(theme)
    }
    fn finish(mut self) -> Value {
        let mut replaced = std::collections::BTreeSet::new();
        self.stored.retain_mut(|v| {
            let Some(t) = v["id"].as_str().and_then(|id| self.updated.get(id)) else {
                return true;
            };
            if !replaced.insert(t.id.clone()) {
                return false;
            }
            *v = serde_json::to_value(t).expect("serializable theme");
            true
        });
        self.stored.extend(
            self.added
                .iter()
                .map(|t| serde_json::to_value(t).expect("serializable theme")),
        );
        Value::Array(self.stored)
    }
}
pub(super) fn versioned_copy(
    doc: &ClientSettingsDoc,
    theme: &ThemeDefinition,
    preferred: Option<&str>,
) -> Result<ThemeDefinition, String> {
    let existing = library(doc);
    copy_with_ids(theme, preferred, |id| existing.iter().any(|t| t.id == id))
}
fn copy_with_ids(
    theme: &ThemeDefinition,
    preferred: Option<&str>,
    exists: impl Fn(&str) -> bool,
) -> Result<ThemeDefinition, String> {
    if let Some(name) =
        preferred.filter(|n| !n.is_empty() && n.to_lowercase() != theme.label.to_lowercase())
    {
        let candidate = rename(theme, name)?;
        if !exists(&candidate.id) {
            return Ok(candidate);
        }
    }
    for n in 1..100 {
        let suffix = format!(" ({n})");
        let candidate = rename(
            theme,
            &format!(
                "{}{}",
                theme
                    .label
                    .chars()
                    .take(48 - suffix.len())
                    .collect::<String>(),
                suffix
            ),
        )?;
        if !exists(&candidate.id) {
            return Ok(candidate);
        }
    }
    Err(format!("Too many copies of \"{}\".", theme.label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn theme(name: &str, dark: bool) -> ThemeDefinition {
        import(&json!({"name":name,"type":if dark {"dark"} else {"light"},"colors":{"editor.background":if dark {"#101014"} else {"#fdfdfd"}}}).to_string()).unwrap()
    }
    #[test]
    fn pairs_families_and_preserves_unmatched_order() {
        let result = pair_vscode_themes(vec![
            theme("github-dark", true),
            theme("github-light", false),
            theme("github-dark-colorblind", true),
            theme("github-light-colorblind", false),
            theme("github-dark-dimmed", true),
        ]);
        assert_eq!(
            result.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            ["Github", "Github Colorblind", "Github Dark Dimmed"]
        );
        assert_eq!(result[0].colors["canvas"], "#fdfdfd");
        assert_eq!(result[0].half(true).unwrap()["canvas"], "#101014");
        assert!(result[2].half(false).is_none());
    }
    #[test]
    fn avoids_symbols_dual_mode_reserved_and_ambiguous_families() {
        let mut dual = theme("Dual Light", false);
        dual.variants.insert("dark".into(), defaults(true));
        let input = vec![
            theme("Dark+", true),
            theme("Light+", false),
            dual,
            theme("T3 Chat Dark", true),
            theme("T3 Chat Light", false),
            theme("Twin dark", true),
            theme("Twin DARK", true),
            theme("Twin light", false),
        ];
        let result = pair_vscode_themes(input.clone());
        assert_eq!(
            result.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            [
                "Dark+",
                "Light+",
                "Dual Light",
                "T3 Chat Light",
                "T3 Chat Dark",
                "Twin light",
                "Twin dark",
                "Twin DARK"
            ]
        );
        assert_eq!(
            pair_vscode_themes(vec![
                theme("solar-dark", true),
                theme("solar-dark-soft", true),
                theme("solar-light", false)
            ])
            .iter()
            .map(|t| t.label.as_str())
            .collect::<Vec<_>>(),
            ["Solar", "Solar Dark Soft"]
        );
    }
    #[test]
    fn collisions_use_filenames_then_numbered_fallbacks() {
        let a = theme("Dracula", true);
        let result = resolve_label_collisions(vec![
            (a.clone(), Some("dracula.json".into())),
            (a.clone(), Some("dracula-soft.json".into())),
        ]);
        assert_eq!(
            result.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["dracula", "dracula-soft"]
        );
        let result = resolve_label_collisions(vec![(a.clone(), None), (a, None)]);
        assert_eq!(result[1].label, "Dracula 2");
    }
    #[test]
    fn collisions_reparse_ids_and_keep_variants_managed_but_drop_collection() {
        let mut a = theme("Same", true);
        a.variants.insert("light".into(), defaults(false));
        a.managed = true;
        a.collection = Some(Collection {
            id: "extension".into(),
            label: "Extension".into(),
        });
        let result = resolve_label_collisions(vec![
            (a.clone(), Some("renamed.json".into())),
            (a.clone(), Some("dark.json".into())),
        ]);
        assert_eq!(result[0].id, "renamed");
        assert_eq!(result[0].variants, a.variants);
        assert!(result[0].managed);
        assert!(result[0].collection.is_none());
        assert_eq!(result[1].id, "same");
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use serde_json::json;
    fn source(name: &str) -> String {
        json!({"version":1,"name":name,"appearance":"dark","colors":{"accent":"#123456"}})
            .to_string()
    }
    fn doc() -> ClientSettingsDoc {
        ClientSettingsDoc::from_json("{}")
    }
    #[test]
    fn newer_reads_and_closed_sessions_retire_old_completions() {
        let mut state = ImportSession::default();
        let old = state.begin_read();
        let new = state.begin_read();
        assert!(!state.finish_single(old, "old.json".into(), Ok(source("Old"))));
        assert!(state.reading);
        assert!(state.finish_single(new, "new.json".into(), Ok(source("New"))));
        assert!(!state.reading);
        assert_eq!(state.file_name.as_deref(), Some("new.json"));
        let pending = state.begin_read();
        state.cancel();
        assert!(!state.finish_single(pending, "old.json".into(), Err("failed".into())));
        assert!(state.error.is_none());
        let new = state.begin_read();
        assert!(state.finish_single(new, "bad.json".into(), Err("failed".into())));
        assert_eq!(state.error.as_deref(), Some("failed"));
    }
    #[test]
    fn batches_keep_failures_after_both_conflict_choices_without_activating() {
        for choice in [ImportChoice::Update, ImportChoice::KeepBoth] {
            let mut doc = doc();
            doc.apply_change(&crate::client_settings::ClientSettingChange::ThemeLibrary(
                install(&doc, &import(&source("Dusk")).unwrap()).unwrap(),
            ));
            let mut state = ImportSession::default();
            let request = state.begin_read();
            let batch = parse_batch(vec![
                ("Dawn.json".into(), Ok(source("Dawn"))),
                ("Dusk.json".into(), Ok(source("Dusk"))),
                ("broken.json".into(), Ok("{".into())),
            ]);
            let existing_count = library(&doc).len();
            super::super::STORED_DECODES.with(|n| n.set(0));
            let result = state.finish_batch(request, batch, &doc).unwrap();
            assert!(
                super::super::STORED_DECODES.with(|n| n.get()) <= existing_count,
                "decode the existing library once per batch"
            );
            assert!(!result.close);
            assert_eq!(result.installed.len(), 1);
            assert_eq!(state.conflicts.len(), 1);
            doc.apply_change(&crate::client_settings::ClientSettingChange::ThemeLibrary(
                result.library.unwrap(),
            ));
            let existing_count = library(&doc).len();
            super::super::STORED_DECODES.with(|n| n.set(0));
            let resolved = state.resolve_conflicts(&doc, choice);
            assert!(
                super::super::STORED_DECODES.with(|n| n.get()) <= existing_count,
                "decode the existing library once per conflict set"
            );
            assert!(!resolved.close);
            assert_eq!(resolved.installed.len(), 1);
            assert!(state.error.as_ref().unwrap().starts_with("broken.json:"));
            assert!(state.conflicts.is_empty());
            doc.apply_change(&crate::client_settings::ClientSettingChange::ThemeLibrary(
                resolved.library.unwrap(),
            ));
            assert_eq!(
                library(&doc).len(),
                if matches!(choice, ImportChoice::KeepBoth) {
                    3
                } else {
                    2
                }
            );
            assert_eq!(
                doc.theme(),
                ThemePreference::Default(AppearanceMode::System)
            );
        }
    }
    #[test]
    fn single_file_stages_text_and_conflict_copy_prefers_filename() {
        let mut state = ImportSession::default();
        let request = state.begin_read();
        assert!(state.finish_single(request, "night-soft.json".into(), Ok(source("Night"))));
        assert!(state.conflicts.is_empty());
        assert_eq!(state.json, source("Night"));
        let mut doc = doc();
        doc.apply_change(&crate::client_settings::ClientSettingChange::ThemeLibrary(
            install(&doc, &import(&state.json).unwrap()).unwrap(),
        ));
        state.conflicts = vec![import(&state.json).unwrap()];
        let result = state.resolve_conflicts(&doc, ImportChoice::KeepBoth);
        assert!(result.close);
        assert_eq!(result.installed[0].label, "Night Soft");
        state.file_name = Some(".json".into());
        state.conflicts = vec![import(&source("Night")).unwrap()];
        let result = state.resolve_conflicts(&doc, ImportChoice::KeepBoth);
        assert!(result.close);
        assert_eq!(result.installed[0].label, "Night (1)");
    }
    #[test]
    fn batch_collisions_pair_after_filename_resolution_and_stale_batch_cannot_install() {
        let source = |dark| {
            json!({"name":"Shared","type":if dark {"dark"} else {"light"},"colors":{"editor.background":if dark {"#101014"} else {"#fdfdfd"}}}).to_string()
        };
        let batch = parse_batch(vec![
            ("github-dark.json".into(), Ok(source(true))),
            ("github-light.json".into(), Ok(source(false))),
        ]);
        assert_eq!(batch.themes.len(), 1);
        assert_eq!(batch.themes[0].label, "Github");
        assert!(batch.themes[0].half(true).is_some());
        let mut state = ImportSession::default();
        let request = state.begin_read();
        state.cancel();
        assert!(state.finish_batch(request, batch, &doc()).is_none());
    }
    #[test]
    fn size_guard_matches_web_copy() {
        assert!(oversized(256 * 1024).is_none());
        assert_eq!(
            oversized(256 * 1024 + 1).unwrap(),
            "That file is 256 KB. Theme files are only a few KB, so this one was not read (limit 256 KB)."
        );
        assert!(oversized(1024 * 1024).unwrap().contains("1.0 MB"));
    }
}
