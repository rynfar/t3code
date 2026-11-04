//! `web/src/components/ProjectFavicon.tsx`, `ProjectMonogram.tsx`, and `web/src/projectIdentity.ts`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use super::ProjectIcon;

pub const COLORS: [&str; 18] = [
    "gray", "red", "orange", "amber", "yellow", "lime", "green", "emerald", "teal", "cyan", "sky", "blue", "indigo", "violet", "purple", "fuchsia", "pink",
    "rose",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presentation {
    override_icon: Option<ProjectIcon>,
    fallback: Option<ProjectIcon>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Source<'a> {
    Image,
    Icon(&'a ProjectIcon),
    Folder,
}

impl Presentation {
    pub fn new(value: Option<&Value>, title: &str) -> Self {
        Self { override_icon: ProjectIcon::decode(value), fallback: (!title.trim().is_empty()).then(|| identity(title)) }
    }

    pub fn source(&self, has_image: bool) -> Source<'_> {
        if let Some(icon) = &self.override_icon {
            Source::Icon(icon)
        } else if has_image {
            Source::Image
        } else if let Some(icon) = &self.fallback {
            Source::Icon(icon)
        } else {
            Source::Folder
        }
    }

    pub fn automatic(&self) -> bool {
        self.override_icon.is_none()
    }
}

pub fn monogram_width(text: &str) -> f32 {
    if text.graphemes(true).count() == 1 { 6. } else { 12. }
}

pub fn identity(title: &str) -> ProjectIcon {
    static WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").expect("project word pattern"));
    static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{N}").expect("project number pattern"));
    let normalized: String = title.nfkc().collect();
    let normalized = normalized.trim();
    let mut words = WORDS.find_iter(normalized);
    let text = if let Some(first_word) = words.next() {
        let first_word = first_word.as_str();
        let mut glyphs = first_word.chars();
        let first = glyphs.next().expect("nonempty word");
        let second = glyphs
            .find(|ch| NUMBER.is_match(ch.encode_utf8(&mut [0; 4])))
            .or_else(|| words.last().map(|word| word.as_str().chars().next().expect("nonempty word")))
            .or_else(|| first_word.chars().last())
            .unwrap_or(first);
        format!("{first}{second}").to_uppercase().chars().take(2).collect()
    } else {
        "PR".into()
    };
    let seed = normalized.to_lowercase();
    let seed = if seed.is_empty() { "project" } else { &seed };
    let index = seed.chars().fold(0, |index, ch| (index * 31 + ch as usize) % COLORS.len());
    ProjectIcon::Monogram { text, color: COLORS[index].into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overrides_win_over_cached_images_and_reset_returns_to_automatic() {
        for value in [
            json!({"kind":"emoji","emoji":"👩🏽‍💻"}),
            json!({"kind":"monogram","text":"T3","color":"violet"}),
            json!({"kind":"lucide","name":"future-icon","color":"red"}),
            json!({"kind":"lucide","name":"folder-code","color":"blue","monogramText":"é","monogram":"XX"}),
        ] {
            let presentation = Presentation::new(Some(&value), "Fallback");
            assert!(!presentation.automatic());
            assert_eq!(presentation.source(true), Source::Icon(&ProjectIcon::decode(Some(&value)).unwrap()));
            assert_eq!(presentation.source(false), presentation.source(true));
        }
        for value in [Value::Null, json!({"kind":"future-kind"})] {
            let presentation = Presentation::new(Some(&value), "T3 Code");
            assert!(presentation.automatic());
            assert_eq!(presentation.source(true), Source::Image);
            assert_eq!(presentation.source(false), Source::Icon(&identity("T3 Code")));
        }
        assert_eq!(Presentation::new(None, " \n").source(false), Source::Folder);
        assert_eq!(monogram_width("é"), 6.);
        assert_eq!(monogram_width("क्‍ष"), 6.);
        assert_eq!(monogram_width("T3"), 12.);
    }

    #[test]
    fn project_identity_matches_normalization_digits_words_and_code_point_hash() {
        for (name, text, color) in [
            ("T3 Code", "T3", "pink"),
            ("alpha beta", "AB", "purple"),
            ("foo", "FO", "gray"),
            (" Ｔ３ Code ", "T3", "pink"),
            ("🦊", "PR", "pink"),
            ("", "PR", "fuchsia"),
            ("Straße", "SE", "yellow"),
            ("a² foo", "A2", "blue"),
        ] {
            assert_eq!(identity(name), ProjectIcon::Monogram { text: text.into(), color: color.into() }, "{name}");
        }
    }
}
