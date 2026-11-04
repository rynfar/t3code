//! `web/src/components/settings/ProjectIconPickerDialog.tsx` and `projectIconOptions.ts`.

use super::{ProjectIcon, ProjectUpdate, icon};
use regex::Regex;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Lucide,
    Emoji,
    Monogram,
}

#[derive(Debug, Clone)]
pub struct Draft {
    pub mode: Mode,
    pub icon_name: String,
    pub color: String,
    pub letters: String,
    pub emoji: String,
}

impl Draft {
    pub fn new(current: Option<&ProjectIcon>, title: &str) -> Self {
        let ProjectIcon::Monogram { text, color } = icon::identity(title) else {
            unreachable!()
        };
        let mut draft = Self {
            mode: Mode::Lucide,
            icon_name: "folder-code".into(),
            color,
            letters: text,
            emoji: "💻".into(),
        };
        match current {
            Some(ProjectIcon::Lucide { name, color }) => {
                draft.icon_name = name.clone();
                draft.color = color.clone();
            }
            Some(ProjectIcon::Emoji { emoji }) => {
                draft.mode = Mode::Emoji;
                draft.emoji = emoji.clone();
            }
            Some(ProjectIcon::Monogram { text, color }) => {
                draft.mode = Mode::Monogram;
                draft.letters = text.clone();
                draft.color = color.clone();
            }
            None => {}
        }
        draft
    }

    pub fn custom_emoji(&mut self, value: &str) {
        if let Some(emoji) = first_emoji(value) {
            self.emoji = emoji.into();
        }
    }

    pub fn selected(&self) -> Option<ProjectIcon> {
        match self.mode {
            Mode::Lucide => Some(ProjectIcon::Lucide {
                name: self.icon_name.clone(),
                color: self.color.clone(),
            }),
            Mode::Emoji => Some(ProjectIcon::Emoji { emoji: self.emoji.clone() }),
            Mode::Monogram => normalized_monogram(&self.letters).map(|text| ProjectIcon::Monogram {
                text,
                color: self.color.clone(),
            }),
        }
    }
}

pub fn normalized_monogram(value: &str) -> Option<String> {
    static VALID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}][\p{L}\p{N}\p{M}\x{200c}\x{200d}]*$").unwrap());
    let value = value.nfkc().collect::<String>().trim().to_uppercase();
    (value.encode_utf16().count() <= 32 && (1..=2).contains(&value.graphemes(true).count()) && VALID.is_match(&value)).then_some(value)
}

pub fn first_emoji(value: &str) -> Option<&str> {
    static PICTOGRAPHIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Extended_Pictographic}").unwrap());
    static FLAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{Regional_Indicator}{2}$").unwrap());
    static KEYCAP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[#*0-9]\x{fe0f}?\x{20e3}$").unwrap());
    let first = value.trim().graphemes(true).next()?;
    (PICTOGRAPHIC.is_match(first) || FLAG.is_match(first) || KEYCAP.is_match(first)).then_some(first)
}

pub fn update(icon: ProjectIcon) -> ProjectUpdate {
    use serde_json::json;
    let value = match icon {
        ProjectIcon::Lucide { name, color } => json!({"kind":"lucide", "name":name, "color":color}),
        ProjectIcon::Emoji { emoji } => json!({"kind":"emoji", "emoji":emoji}),
        ProjectIcon::Monogram { text, color } => json!({"kind":"monogram", "text":text, "color":color}),
    };
    ProjectUpdate {
        favicon_path: Some(None),
        project_icon: Some(Some(value)),
        ..Default::default()
    }
}

/// Captured on open; membership and destination must still match before starting a save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub environment: String,
    pub members: Vec<(String, String)>,
}
impl Target {
    pub fn allows(&self, current: &Self, permitted: bool, saving: bool) -> bool {
        permitted
            && !saving
            && !self.members.is_empty()
            && self.environment == current.environment
            && self.members.len() == current.members.len()
            && self.members.iter().all(|member| current.members.contains(member))
    }
}

pub fn filter_names<'a>(query: &str, available: &'a [&'a str]) -> Vec<&'a str> {
    const POPULAR: &[&str] = &[
        "folder-code",
        "code-2",
        "terminal",
        "globe-2",
        "server",
        "database",
        "bot",
        "sparkles",
        "smartphone",
        "monitor",
        "cloud-cog",
        "package",
        "book-open",
        "flask-conical",
        "shield-check",
        "rocket",
        "gamepad-2",
        "music",
        "image",
        "shopping-bag",
        "git-branch",
        "workflow",
        "wrench",
        "layers-3",
    ];
    let normalized = query.split_whitespace().collect::<Vec<_>>().join("-").to_lowercase();
    if normalized.is_empty() {
        POPULAR.iter().copied().filter(|name| available.contains(name)).collect()
    } else {
        available.iter().copied().filter(|name| name.contains(&normalized)).take(60).collect()
    }
}

pub const EMOJIS: &[(&str, &str)] = &[
    ("💻", "Computer"),
    ("🛠️", "Tools"),
    ("🚀", "Rocket"),
    ("🤖", "Robot"),
    ("✨", "Sparkles"),
    ("⚡", "Lightning"),
    ("🌐", "Web"),
    ("📱", "Mobile"),
    ("🖥️", "Desktop"),
    ("⌨️", "Keyboard"),
    ("⚙️", "Gear"),
    ("🗄️", "Database"),
    ("☁️", "Cloud"),
    ("📦", "Package"),
    ("📚", "Books"),
    ("🧪", "Test tube"),
    ("🔒", "Lock"),
    ("🎮", "Game"),
    ("🎵", "Music"),
    ("🎬", "Movie"),
    ("🖼️", "Picture"),
    ("🛍️", "Shopping"),
    ("🔥", "Fire"),
    ("💡", "Idea"),
    ("🧩", "Puzzle"),
    ("📊", "Chart"),
    ("🧠", "Brain"),
    ("🦄", "Unicorn"),
    ("🐙", "Octopus"),
    ("🌱", "Seedling"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn drafts_initialize_each_override_and_preserve_modes_without_reusing_closed_edits() {
        for icon in [
            ProjectIcon::Emoji { emoji: "🇺🇸".into() },
            ProjectIcon::Lucide {
                name: "future-icon".into(),
                color: "red".into(),
            },
            ProjectIcon::Monogram {
                text: "T3".into(),
                color: "violet".into(),
            },
        ] {
            let mut draft = Draft::new(Some(&icon), "Alpha Beta");
            assert_eq!(draft.selected(), Some(icon.clone()));
            draft.mode = Mode::Emoji;
            draft.custom_emoji("👩🏽‍💻 hello");
            draft.mode = Mode::Monogram;
            draft.letters = " ｔ３ ".into();
            assert!(matches!(draft.selected(),Some(ProjectIcon::Monogram{text,..}) if text=="T3"));
            draft.mode = Mode::Emoji;
            assert_eq!(draft.emoji, "👩🏽‍💻");
            draft.custom_emoji("text 🚀");
            assert_eq!(draft.emoji, "👩🏽‍💻");
            assert_eq!(Draft::new(Some(&icon), "Alpha Beta").selected(), Some(icon));
        }
        let draft = Draft::new(None, "Alpha Beta");
        assert_eq!(draft.icon_name, "folder-code");
        assert_eq!(draft.letters, "AB");
        assert_eq!(
            ProjectIcon::Monogram {
                text: draft.letters,
                color: draft.color
            },
            icon::identity("Alpha Beta")
        );
    }
    #[test]
    fn unicode_input_keeps_complete_first_grapheme_and_validates_normalized_letters() {
        for emoji in ["👩🏽‍💻", "🇺🇸", "1️⃣", "#️⃣", "*⃣", "🛠️", "🏴\u{e0067}\u{e0062}\u{e007f}"] {
            assert_eq!(first_emoji(&format!("  {emoji} tail")), Some(emoji));
        }
        for text in ["", "plain text", "a🚀", "1", "🇺"] {
            assert_eq!(first_emoji(text), None);
        }
        for (input, expected) in [
            (" ｔ３ ", Some("T3")),
            ("e\u{301}", Some("É")),
            ("ß", Some("SS")),
            ("क्‍ष", Some("क्‍ष")),
            ("abc", None),
            ("a b", None),
            ("🦊", None),
            ("\u{301}a", None),
            ("", None),
        ] {
            assert_eq!(normalized_monogram(input).as_deref(), expected);
        }
    }
    #[test]
    fn save_clears_image_and_guards_captured_members_destination_and_duplicates() {
        let target = Target {
            environment: "env".into(),
            members: vec![("a".into(), "/a".into()), ("b".into(), "/b".into())],
        };
        assert!(target.allows(&target, true, false));
        assert!(!target.allows(&target, false, false));
        assert!(!target.allows(&target, true, true));
        for current in [
            Target {
                environment: "other".into(),
                ..target.clone()
            },
            Target {
                members: vec![target.members[0].clone()],
                ..target.clone()
            },
            Target {
                members: vec![("a".into(), "/new".into()), target.members[1].clone()],
                ..target.clone()
            },
        ] {
            assert!(!target.allows(&current, true, false));
        }
        for icon in [
            ProjectIcon::Lucide {
                name: "folder-code".into(),
                color: "blue".into(),
            },
            ProjectIcon::Emoji { emoji: "🚀".into() },
            ProjectIcon::Monogram {
                text: "T3".into(),
                color: "red".into(),
            },
        ] {
            let payload = update(icon.clone()).payload("cmd", "a");
            assert_eq!(payload["faviconPath"], json!(null));
            assert_eq!(payload["projectId"], "a");
            assert_eq!(ProjectIcon::decode(Some(&payload["projectIcon"])), Some(icon));
            assert!(payload.get("title").is_none());
        }
    }
    #[test]
    fn search_normalizes_spacing_filters_missing_glyphs_and_bounds_results() {
        assert_eq!(filter_names("", &["folder-code", "bot"]), vec!["folder-code", "bot"]);
        assert_eq!(filter_names("  FOLDER  Code ", &["folder-code"]), vec!["folder-code"]);
        assert!(filter_names("nothing", &["folder-code"]).is_empty());
        assert_eq!(filter_names("icon", &["icon"; 100]).len(), 60);
    }
}
