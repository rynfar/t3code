//! `TypographySettings.tsx`, `FontFamilyPicker.tsx`, `appearanceFonts.ts` and `contracts/src/settings.ts`.

use super::{ClientSettingsDoc, DEFAULT_INTERFACE_FONT_SIZE, MAX_INTERFACE_FONT_SIZE, MIN_INTERFACE_FONT_SIZE};
use serde_json::Value;

pub const ADVANCED_KEY: &str = "t3code:typography-advanced";
pub const MONO_STACK: &[&str] = &["SF Mono", "SFMono-Regular", "Menlo", "Consolas", "Liberation Mono"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontRole {
    Interface,
    Prompt,
    Code,
    Terminal,
}

impl FontRole {
    pub const ALL: [Self; 4] = [Self::Interface, Self::Prompt, Self::Code, Self::Terminal];

    pub fn size_key(self) -> &'static str {
        match self {
            Self::Interface => "fontSizeInterface",
            Self::Prompt => "fontSizePrompt",
            Self::Code => "fontSizeCode",
            Self::Terminal => "fontSizeTerminal",
        }
    }
    pub fn family_key(self) -> &'static str {
        match self {
            Self::Interface => "fontFamilySans",
            Self::Prompt => "fontFamilyComposer",
            Self::Code => "fontFamilyCode",
            Self::Terminal => "fontFamilyTerminal",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Interface => "interface-font",
            Self::Prompt => "prompt-font",
            Self::Code => "code-font",
            Self::Terminal => "terminal-font",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Interface => "Interface font",
            Self::Prompt => "Prompt font",
            Self::Code => "Code font",
            Self::Terminal => "Terminal font",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Interface => "Everything outside code blocks and the terminal.",
            Self::Prompt => "Only the box you write prompts in. Mono works well here.",
            Self::Code => "Code blocks, diffs, and file previews.",
            Self::Terminal => "Terminal output, independent from code blocks and diffs.",
        }
    }
    pub fn range(self) -> std::ops::RangeInclusive<u32> {
        match self {
            Self::Interface => MIN_INTERFACE_FONT_SIZE..=MAX_INTERFACE_FONT_SIZE,
            Self::Prompt => 12..=20,
            Self::Code => 10..=18,
            Self::Terminal => 8..=20,
        }
    }
    pub fn default_size(self) -> u32 {
        match self {
            Self::Interface => DEFAULT_INTERFACE_FONT_SIZE,
            Self::Prompt => 14,
            Self::Code => 13,
            Self::Terminal => 12,
        }
    }
    pub fn clamp(self, size: f64) -> u32 {
        if !size.is_finite() {
            return self.default_size();
        }
        size.round().clamp(*self.range().start() as f64, *self.range().end() as f64) as u32
    }
    pub fn monospace(self) -> bool {
        matches!(self, Self::Code | Self::Terminal)
    }
}

#[derive(Clone, Copy)]
pub struct FontDefaults<'a> {
    pub sans: &'a str,
    pub mono: &'a str,
    pub serif: &'a str,
}

pub fn resolve_font_stack<'a>(stack: &str, installed: &'a [String], defaults: FontDefaults<'a>, fallback: &'a str) -> &'a str {
    stack
        .split(',')
        .map(|name| name.trim().trim_matches(['\"', '\'']).trim())
        .find_map(|name| match name.to_ascii_lowercase().as_str() {
            "system-ui" | "sans-serif" | "ui-sans-serif" | "-apple-system" | "blinkmacsystemfont" => Some(defaults.sans),
            "monospace" | "ui-monospace" => Some(defaults.mono),
            "serif" | "ui-serif" => Some(defaults.serif),
            _ => installed
                .iter()
                .find(|installed| installed.eq_ignore_ascii_case(name))
                .map(String::as_str),
        })
        .unwrap_or(fallback)
}

/// Installed families, already classified by the platform's glyph metrics.
pub fn family_options(families: impl IntoIterator<Item = (String, bool)>, monospace: bool) -> Vec<String> {
    let mut names: Vec<_> = families
        .into_iter()
        .filter(|(name, mono)| !name.starts_with('.') && (!monospace || *mono))
        .map(|(name, _)| name)
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

/// Like `queryInstalledFontFamilies`: one inventory per app session, including an empty result.
#[derive(Default)]
pub struct FontInventory {
    families: Option<Vec<(String, bool)>>,
}
impl FontInventory {
    pub fn families(&self) -> Option<&[(String, bool)]> {
        self.families.as_deref()
    }
    pub fn finish(&mut self, families: Vec<(String, bool)>) {
        if self.families.is_none() {
            self.families = Some(families);
        }
    }
    pub fn accepts(&self, role: FontRole, family: &str) -> bool {
        family.is_empty()
            || !role.monospace()
            || self
                .families()
                .unwrap_or_default()
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(family))
                .is_none_or(|(_, mono)| *mono)
    }
}
pub fn advances_are_monospace(advances: &[f32]) -> bool {
    let Some(first) = advances.first() else {
        return true;
    };
    if *first <= 0. || advances.iter().any(|width| !width.is_finite() || *width <= 0.) {
        return true;
    }
    advances.iter().all(|width| (width - first).abs() < 0.01)
}

impl ClientSettingsDoc {
    pub fn separate_fonts(&self) -> bool {
        self.flag(ADVANCED_KEY, false)
    }
    pub fn word_wrap(&self) -> bool {
        self.flag("wordWrap", true)
    }
    pub fn font_size(&self, role: FontRole) -> u32 {
        self.entries
            .get(role.size_key())
            .and_then(Value::as_u64)
            .and_then(|size| u32::try_from(size).ok())
            .filter(|size| role.range().contains(size))
            .unwrap_or(role.default_size())
    }
    pub fn font_family(&self, role: FontRole) -> &str {
        self.entries
            .get(role.family_key())
            .and_then(Value::as_str)
            .filter(|family| family.chars().count() <= 200)
            .unwrap_or("")
            .trim()
    }
    pub fn typography(&self) -> Typography {
        Typography {
            separate: self.separate_fonts(),
            families: FontRole::ALL.map(|role| self.font_family(role).to_owned()),
            sizes: FontRole::ALL.map(|role| self.font_size(role) as f32),
            word_wrap: self.word_wrap(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Typography {
    pub separate: bool,
    pub families: [String; 4],
    pub sizes: [f32; 4],
    pub word_wrap: bool,
}

impl Default for Typography {
    fn default() -> Self {
        ClientSettingsDoc::default().typography()
    }
}

impl Typography {
    pub fn family(&self, role: FontRole) -> &str {
        let role = match (self.separate, role) {
            (false, FontRole::Prompt) => FontRole::Interface,
            (false, FontRole::Terminal) => FontRole::Code,
            _ => role,
        };
        let family = self.families[role as usize].trim();
        if family.is_empty() && role == FontRole::Prompt {
            return self.family(FontRole::Interface);
        }
        family
    }
    pub fn size(&self, role: FontRole) -> f32 {
        match (self.separate, role) {
            (false, FontRole::Prompt) => self.sizes[FontRole::Interface as usize] * 14. / 16.,
            (false, FontRole::Terminal) => self.sizes[FontRole::Code as usize],
            _ => self.sizes[role as usize],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_settings::ClientSettingChange;

    #[test]
    fn picker_accepts_proportional_prompt_but_rejects_code_and_terminal() {
        let mut inventory = FontInventory::default();
        assert!(inventory.families().is_none());
        inventory.finish(vec![("Arial".into(), false), ("Menlo".into(), true)]);
        for role in FontRole::ALL {
            assert_eq!(inventory.accepts(role, "Arial"), !role.monospace());
            assert!(inventory.accepts(role, "Menlo"));
            assert!(inventory.accepts(role, ""));
        }
        assert!(inventory.accepts(FontRole::Code, "mEnLo"));
        inventory.finish(Vec::new());
        assert_eq!(
            inventory.families().unwrap().len(),
            2,
            "inventory stays cached until app reload, like web"
        );
        assert!(advances_are_monospace(&[10.; 8]));
        assert!(!advances_are_monospace(&[10., 11., 10.]));
        for widths in [&[][..], &[0., 10.], &[f32::NAN, 10.], &[10., f32::INFINITY]] {
            assert!(advances_are_monospace(widths), "web permits fonts when metrics cannot be probed");
        }
    }

    #[test]
    fn web_font_stacks_resolve_case_generics_and_order() {
        let installed = ["Courier New", "Menlo", "Times", "Arial"].map(str::to_owned);
        let defaults = FontDefaults {
            sans: ".SystemUIFont",
            mono: "Menlo",
            serif: "Times",
        };
        for (stack, expected) in [
            ("courier new", "Courier New"),
            ("missing, \"cOuRiEr NeW\", serif", "Courier New"),
            ("serif, Courier New", "Times"),
            ("SERIF", "Times"),
            ("ui-serif", "Times"),
            ("sans-serif", ".SystemUIFont"),
            ("ui-sans-serif", ".SystemUIFont"),
            ("system-ui", ".SystemUIFont"),
            ("-apple-system", ".SystemUIFont"),
            ("BlinkMacSystemFont", ".SystemUIFont"),
            ("monospace", "Menlo"),
            ("ui-monospace", "Menlo"),
            ("'arial', monospace", "Arial"),
            ("", "fallback"),
            ("missing", "fallback"),
        ] {
            assert_eq!(resolve_font_stack(stack, &installed, defaults, "fallback"), expected, "{stack}");
        }
    }

    #[test]
    fn defaults_ranges_and_clamping_match_contracts() {
        let doc =
            ClientSettingsDoc::from_json(r#"{"fontSizeCode":99,"fontSizePrompt":13.5,"fontSizeTerminal":"12","fontFamilySans":true}"#);
        assert_eq!(doc.typography(), Typography::default());
        for (role, default, min, max) in [
            (FontRole::Interface, 16, 12, 20),
            (FontRole::Prompt, 14, 12, 20),
            (FontRole::Code, 13, 10, 18),
            (FontRole::Terminal, 12, 8, 20),
        ] {
            assert_eq!(doc.font_size(role), default);
            assert_eq!(role.range().collect::<Vec<_>>(), (min..=max).collect::<Vec<_>>());
            assert_eq!(role.clamp(f64::NAN), default);
            assert_eq!(role.clamp(f64::INFINITY), default);
            assert_eq!(role.clamp(-10.), min);
            assert_eq!(role.clamp(99.), max);
            assert_eq!(role.clamp(12.5), 13);
        }
        assert!(doc.word_wrap());
        assert!(!doc.separate_fonts());
    }

    #[test]
    fn shared_and_separate_resolution_preserves_hidden_preferences_and_resets() {
        let mut doc = ClientSettingsDoc::from_json(
            r#"{"fontSizeInterface":20,"fontSizePrompt":18,"fontSizeCode":17,"fontSizeTerminal":9,"fontFamilySans":"Arial","fontFamilyComposer":"Menlo","fontFamilyCode":"Monaco","fontFamilyTerminal":"Courier"}"#,
        );
        let shared = doc.typography();
        assert_eq!((shared.family(FontRole::Prompt), shared.size(FontRole::Prompt)), ("Arial", 17.5));
        assert_eq!(
            (shared.family(FontRole::Terminal), shared.size(FontRole::Terminal)),
            ("Monaco", 17.)
        );
        doc.apply(&ClientSettingChange::SeparateFonts(true));
        let separate = doc.typography();
        assert_eq!((separate.family(FontRole::Prompt), separate.size(FontRole::Prompt)), ("Menlo", 18.));
        assert_eq!(
            (separate.family(FontRole::Terminal), separate.size(FontRole::Terminal)),
            ("Courier", 9.)
        );
        doc.apply(&ClientSettingChange::ResetFont(FontRole::Prompt));
        assert_eq!(doc.typography().family(FontRole::Prompt), "Arial");
        assert_eq!(doc.font_size(FontRole::Prompt), 14);
        doc.apply(&ClientSettingChange::FontSize(FontRole::Terminal, 99));
        doc.apply(&ClientSettingChange::WordWrap(false));
        let reread = ClientSettingsDoc::from_json(&doc.to_json());
        assert_eq!(reread.font_size(FontRole::Terminal), 20);
        assert!(!reread.word_wrap());
        assert_eq!(reread.typography(), doc.typography());
    }

    #[test]
    fn font_options_filter_sort_and_deduplicate() {
        let families = [
            (".Hidden", true),
            ("Menlo", true),
            ("Arial", false),
            ("menlo", true),
            ("Courier", true),
        ]
        .map(|(name, mono)| (name.to_owned(), mono));
        assert_eq!(family_options(families.clone(), false), ["Arial", "Courier", "Menlo"]);
        assert_eq!(family_options(families, true), ["Courier", "Menlo"]);
    }
}
