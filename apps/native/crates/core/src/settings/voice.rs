//! Pure logic for Voice Settings (`VoiceSettingsPanel.tsx`, `speechModelBrowser.ts`,
//! `SpeechModelComparison.tsx`, `SpeechLanguageSelect.tsx`, `MicrophoneTest.tsx`,
//! `CloudVoiceConfiguration.tsx`).

use std::collections::{HashMap, HashSet};

use unicode_normalization::UnicodeNormalization;

use crate::voice_input::{
    CLOUD_SPEECH_LANGUAGES, CloudSpeechModel, CloudSpeechProvider, EnvironmentSpeechModel,
    EnvironmentSpeechModelState, EnvironmentSpeechState, EnvironmentSpeechStatus,
    SPEECH_MAX_ALIASES_PER_WORD, SPEECH_MAX_CUSTOM_WORD_LEN_UTF16, SPEECH_MAX_CUSTOM_WORDS,
    SpeechBenchmarkTier, SpeechCustomWordEntry, SpeechModelUnloadTimeout, SpeechRecommendationRole,
    VoiceShortcutMode, effective_speech_language, recommend_speech_models, speech_language_label,
    validate_cloud_speech_api_key, validate_cloud_speech_model_id,
};

pub const LOOPBACK_SECONDS: u32 = 10;

/// `SHORTCUT_MODES` (`VoiceSettingsPanel.tsx`).
pub const SHORTCUT_MODES: &[(VoiceShortcutMode, &str, &str)] = &[
    (VoiceShortcutMode::Auto, "Auto", "Tap to toggle, hold to talk"),
    (VoiceShortcutMode::Hold, "Hold", "Record while held"),
    (VoiceShortcutMode::Toggle, "Toggle", "Tap to start, tap to stop"),
];

pub fn shortcut_mode_meta(mode: VoiceShortcutMode) -> (&'static str, &'static str) {
    SHORTCUT_MODES
        .iter()
        .find(|(m, _, _)| *m == mode)
        .map(|(_, label, hint)| (*label, *hint))
        .unwrap_or(("Auto", "Tap to toggle, hold to talk"))
}

/// `UNLOAD_LABELS` (`VoiceSettingsPanel.tsx`).
pub const UNLOAD_TIMEOUT_OPTIONS: &[(SpeechModelUnloadTimeout, &str)] = &[
    (SpeechModelUnloadTimeout::Never, "Never"),
    (SpeechModelUnloadTimeout::Immediately, "Immediately"),
    (SpeechModelUnloadTimeout::Min2, "After 2 minutes"),
    (SpeechModelUnloadTimeout::Min5, "After 5 minutes"),
    (SpeechModelUnloadTimeout::Min10, "After 10 minutes"),
    (SpeechModelUnloadTimeout::Min15, "After 15 minutes"),
    (SpeechModelUnloadTimeout::Hour1, "After 1 hour"),
];

pub fn unload_timeout_label(timeout: SpeechModelUnloadTimeout) -> &'static str {
    UNLOAD_TIMEOUT_OPTIONS
        .iter()
        .find(|(t, _)| *t == timeout)
        .map(|(_, label)| *label)
        .unwrap_or("Never")
}

pub fn parse_unload_timeout(value: &str) -> Option<SpeechModelUnloadTimeout> {
    UNLOAD_TIMEOUT_OPTIONS
        .iter()
        .find(|(t, _)| t.as_str() == value)
        .map(|(t, _)| *t)
}

/// `formatSize` (`VoiceSettingsPanel.tsx`).
pub fn format_model_size_mb(bytes: f64) -> String {
    format!("{} MB", (bytes / 1024.0 / 1024.0).round() as u64)
}

/// `formatTime` (`MicrophoneTest.tsx`).
pub fn format_loopback_time(seconds: f32) -> String {
    let clamped = seconds.max(0.0);
    let total = clamped.floor() as u32;
    format!("{}:{:02}", total / 60, total % 60)
}

/// `SpeechModelSort` (`speechModelBrowser.ts`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SpeechModelSort {
    #[default]
    Recommended,
    Size,
    Accuracy,
    Speed,
}

impl SpeechModelSort {
    pub const ALL: &'static [Self] = &[Self::Recommended, Self::Size, Self::Accuracy, Self::Speed];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recommended => "recommended",
            Self::Size => "size",
            Self::Accuracy => "accuracy",
            Self::Speed => "speed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "recommended" => Some(Self::Recommended),
            "size" => Some(Self::Size),
            "accuracy" => Some(Self::Accuracy),
            "speed" => Some(Self::Speed),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Recommended => "Recommended",
            Self::Size => "Smallest",
            Self::Accuracy => "Most accurate",
            Self::Speed => "Fastest",
        }
    }

    pub fn menu_label(self, accuracy_language: &str) -> String {
        match self {
            Self::Accuracy => format!("Most accurate in {}", speech_language_label(accuracy_language)),
            _ => self.label().to_owned(),
        }
    }
}

fn model_sort_order(model: &EnvironmentSpeechModel) -> u8 {
    if model.active {
        return 0;
    }
    if model.state == EnvironmentSpeechModelState::Installed {
        return 1;
    }
    if matches!(
        model.state,
        EnvironmentSpeechModelState::Downloading | EnvironmentSpeechModelState::Verifying
    ) {
        return 2;
    }
    3
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechModelFilterOptions<'a> {
    pub search: &'a str,
    pub languages: &'a [String],
    pub downloaded_only: bool,
    pub sort: SpeechModelSort,
    pub accuracy_language: &'a str,
    pub benchmark_tier: Option<SpeechBenchmarkTier>,
}

/// `filterSpeechModels` (`apps/web/src/components/settings/speechModelBrowser.ts`, `pingdotgg/t3code#33336`).
pub fn filter_speech_models<'a>(
    models: &'a [EnvironmentSpeechModel],
    options: &SpeechModelFilterOptions<'_>,
) -> Vec<&'a EnvironmentSpeechModel> {
    let recommendations = recommend_speech_models(models, options.languages, options.benchmark_tier);
    filter_speech_models_with_recommendations(models, options, &recommendations)
}

pub fn filter_speech_models_with_recommendations<'a>(
    models: &'a [EnvironmentSpeechModel],
    options: &SpeechModelFilterOptions<'_>,
    recommendations: &[(String, SpeechRecommendationRole)],
) -> Vec<&'a EnvironmentSpeechModel> {
    let query = options.search.trim().to_lowercase();
    let ranks: HashMap<&str, usize> =
        recommendations.iter().enumerate().map(|(index, (id, _))| (id.as_str(), index)).collect();
    let score = |model: &EnvironmentSpeechModel| -> f64 {
        match options.sort {
            SpeechModelSort::Size => model.size,
            SpeechModelSort::Accuracy => model
                .benchmark
                .as_ref()
                .and_then(|b| b.accuracy.get(options.accuracy_language))
                .map_or(f64::INFINITY, |entry| entry.error_percent),
            SpeechModelSort::Speed => match options.benchmark_tier {
                None => f64::INFINITY,
                Some(tier) => {
                    let speed = model.benchmark.as_ref().and_then(|b| match tier {
                        SpeechBenchmarkTier::Cpu => b.seconds_per_30s.cpu.flatten(),
                        SpeechBenchmarkTier::Accelerated => b.seconds_per_30s.accelerated,
                    });
                    speed.unwrap_or(f64::INFINITY)
                }
            },
            SpeechModelSort::Recommended => {
                let rank = ranks.get(model.id.as_str()).copied().unwrap_or(3);
                (usize::from(model_sort_order(model)) * 4 + rank) as f64
            }
        }
    };

    let mut filtered: Vec<&'a EnvironmentSpeechModel> = models
        .iter()
        .filter(|model| {
            options
                .languages
                .iter()
                .all(|lang| model.languages.iter().any(|candidate| candidate == lang))
                && (!options.downloaded_only || model.state == EnvironmentSpeechModelState::Installed)
                && (query.is_empty()
                    || format!("{} {}", model.name, model.description)
                        .to_lowercase()
                        .contains(&query))
        })
        .collect();

    filtered.sort_by(|left, right| {
        let ls = score(left);
        let rs = score(right);
        let primary = match (ls.is_nan(), rs.is_nan()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => ls.total_cmp(&rs),
        };
        primary.then_with(|| model_sort_order(left).cmp(&model_sort_order(right)))
    });
    filtered
}

/// `commonLanguages` (`apps/web/src/components/settings/SpeechModelComparison.tsx`).
const COMMON_LANGUAGES: &[&str] = &[
    "en", "zh", "hi", "es", "fr", "bn", "pt", "id", "ur", "ru", "de", "ja", "mr", "vi", "te",
    "sw", "ha", "tr", "tl", "ta", "jw", "ko", "am", "th", "it", "gu", "kn", "yo",
];

/// `orderSpeechLanguages` (`apps/web/src/components/settings/SpeechModelComparison.tsx`).
pub fn order_speech_languages(languages: &[String], preferred: &[String]) -> Vec<String> {
    let mut order: Vec<&str> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for code in preferred.iter().map(String::as_str).chain(COMMON_LANGUAGES.iter().copied()) {
        if seen.insert(code) {
            order.push(code);
        }
    }
    let rank: HashMap<&str, usize> = order.iter().enumerate().map(|(i, &c)| (c, i)).collect();
    let fallback_rank = order.len();

    let mut out = languages.to_vec();
    out.sort_by(|left, right| {
        let lr = rank.get(left.as_str()).copied().unwrap_or(fallback_rank);
        let rr = rank.get(right.as_str()).copied().unwrap_or(fallback_rank);
        lr.cmp(&rr).then_with(|| {
            speech_language_label(left)
                .to_lowercase()
                .cmp(&speech_language_label(right).to_lowercase())
        })
    });
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAccuracySummary {
    pub language_code: String,
    pub language_label: String,
    pub grade: Option<String>,
    pub error_percent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSpeedSummary {
    Hidden,
    UpdateServer,
    NotBenchmarked,
    Seconds(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelBenchmarkDisplay {
    pub accuracies: Vec<ModelAccuracySummary>,
    pub speed: ModelSpeedSummary,
    pub facts: Vec<String>,
}

/// `ModelBenchmarks` (`apps/web/src/components/settings/SpeechModelComparison.tsx`, `pingdotgg/t3code#33336`).
pub fn model_benchmark_display(
    model: &EnvironmentSpeechModel,
    selected_languages: &[String],
    tier: Option<SpeechBenchmarkTier>,
) -> ModelBenchmarkDisplay {
    let data = model.benchmark.as_ref();
    let visible_languages: Vec<String> = match data {
        None => Vec::new(),
        Some(bench) => {
            if !selected_languages.is_empty() {
                selected_languages.to_vec()
            } else {
                model
                    .languages
                    .iter()
                    .find(|code| bench.accuracy.contains_key(code.as_str()))
                    .or_else(|| model.languages.first())
                    .cloned()
                    .into_iter()
                    .collect()
            }
        }
    };

    let accuracies = visible_languages
        .into_iter()
        .map(|code| {
            let score = data.and_then(|b| b.accuracy.get(&code));
            ModelAccuracySummary {
                language_label: speech_language_label(&code).into_owned(),
                language_code: code,
                grade: score.map(|s| s.grade.clone()),
                error_percent_label: score.map(|s| format_compact_number(s.error_percent)),
            }
        })
        .collect();

    let speed = match data {
        None => ModelSpeedSummary::Hidden,
        Some(bench) => match tier {
            None => ModelSpeedSummary::UpdateServer,
            Some(SpeechBenchmarkTier::Cpu) => match bench.seconds_per_30s.cpu {
                None => ModelSpeedSummary::UpdateServer,
                Some(None) => ModelSpeedSummary::NotBenchmarked,
                Some(Some(secs)) => ModelSpeedSummary::Seconds(format_compact_number(secs)),
            },
            Some(SpeechBenchmarkTier::Accelerated) => match bench.seconds_per_30s.accelerated {
                None => ModelSpeedSummary::NotBenchmarked,
                Some(secs) => ModelSpeedSummary::Seconds(format_compact_number(secs)),
            },
        },
    };

    let mut facts = Vec::new();
    if model.languages.len() > 1 {
        facts.push(format!("{} languages", model.languages.len()));
    } else if model.benchmark.is_none()
        && let Some(first) = model.languages.first()
    {
        facts.push(speech_language_label(first).into_owned());
    }
    facts.push(format_model_size_mb(model.size));

    ModelBenchmarkDisplay { accuracies, speed, facts }
}

fn format_compact_number(value: f64) -> String {
    if value.fract().abs() < 1e-9 {
        format!("{}", value as i64)
    } else {
        let s = format!("{value:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechLanguageDisplay {
    pub trigger_label: String,
    pub resolved: String,
    pub unavailable_label: Option<String>,
    pub sorted_languages: Vec<(String, String)>,
}

/// `SpeechLanguageSelect` (`apps/web/src/components/settings/SpeechLanguageSelect.tsx`).
pub fn resolve_speech_language_display(
    languages: &[String],
    supports_language_detection: bool,
    preference: &str,
) -> SpeechLanguageDisplay {
    let resolved = effective_speech_language(languages, supports_language_detection, preference);
    let effective_label = if resolved == "auto" {
        "Auto-detect".to_owned()
    } else {
        speech_language_label(&resolved).into_owned()
    };
    let unavailable_label = (preference != "auto" && preference != resolved)
        .then(|| format!("{} unavailable", speech_language_label(preference)));
    let trigger_label = if let Some(unavailable) = &unavailable_label {
        format!("{effective_label} ({unavailable})")
    } else if preference == "auto" && resolved != "auto" {
        format!("Auto ({effective_label})")
    } else {
        effective_label
    };
    let mut sorted_languages: Vec<(String, String)> = languages
        .iter()
        .map(|code| (code.clone(), speech_language_label(code).into_owned()))
        .collect();
    sorted_languages.sort_by_key(|(_, a)| a.to_lowercase());

    SpeechLanguageDisplay { trigger_label, resolved, unavailable_label, sorted_languages }
}

pub fn cloud_speech_language_list() -> Vec<String> {
    CLOUD_SPEECH_LANGUAGES.iter().map(|s| (*s).to_owned()).collect()
}

/// `normalizeTerm` (`VoiceSettingsPanel.tsx`).
pub fn normalize_dictionary_term(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .nfc()
        .collect()
}

fn spellings_set(words: &[SpeechCustomWordEntry]) -> HashSet<String> {
    let mut set = HashSet::new();
    for entry in words {
        set.insert(entry.term.to_lowercase());
        for alias in &entry.aliases {
            set.insert(alias.to_lowercase());
        }
    }
    set
}

/// Parses comma/newline-separated new terms not already in `words`.
pub fn parse_new_dictionary_terms(
    words: &[SpeechCustomWordEntry],
    draft: &str,
) -> Vec<String> {
    let mut seen = spellings_set(words);
    draft
        .split([',', '\n'])
        .map(normalize_dictionary_term)
        .filter(|term| {
            let key = term.to_lowercase();
            !key.is_empty() && seen.insert(key)
        })
        .collect()
}

pub fn can_add_dictionary_terms(
    words: &[SpeechCustomWordEntry],
    draft: &str,
    disabled: bool,
) -> bool {
    if disabled {
        return false;
    }
    let new_terms = parse_new_dictionary_terms(words, draft);
    !new_terms.is_empty()
        && new_terms
            .iter()
            .all(|term| term.encode_utf16().count() <= SPEECH_MAX_CUSTOM_WORD_LEN_UTF16)
        && words.len() + new_terms.len() <= SPEECH_MAX_CUSTOM_WORDS
}

pub fn add_dictionary_terms(
    words: &[SpeechCustomWordEntry],
    draft: &str,
) -> Option<Vec<SpeechCustomWordEntry>> {
    if !can_add_dictionary_terms(words, draft, false) {
        return None;
    }
    let mut next = words.to_vec();
    for term in parse_new_dictionary_terms(words, draft) {
        next.push(SpeechCustomWordEntry { term, aliases: Vec::new() });
    }
    Some(next)
}

pub fn add_dictionary_alias(
    words: &[SpeechCustomWordEntry],
    term: &str,
    alias_draft: &str,
) -> Option<Vec<SpeechCustomWordEntry>> {
    let value = normalize_dictionary_term(alias_draft);
    if value.is_empty() || value.encode_utf16().count() > SPEECH_MAX_CUSTOM_WORD_LEN_UTF16 {
        return None;
    }
    let spellings = spellings_set(words);
    if spellings.contains(&value.to_lowercase()) {
        return None;
    }
    let mut changed = false;
    let next = words
        .iter()
        .map(|entry| {
            if entry.term == term && entry.aliases.len() < SPEECH_MAX_ALIASES_PER_WORD {
                changed = true;
                let mut aliases = entry.aliases.clone();
                aliases.push(value.clone());
                SpeechCustomWordEntry { term: entry.term.clone(), aliases }
            } else {
                entry.clone()
            }
        })
        .collect();
    changed.then_some(next)
}

pub fn remove_dictionary_term(
    words: &[SpeechCustomWordEntry],
    term: &str,
) -> Vec<SpeechCustomWordEntry> {
    words.iter().filter(|entry| entry.term != term).cloned().collect()
}

/// `searchTargetFallbacks` (`VoiceSettingsPanel.tsx`).
pub fn voice_search_target_fallback(
    target_id: &str,
    has_local_prepared: bool,
    status: Option<&EnvironmentSpeechStatus>,
    has_cloud_provider: bool,
    cleanup_enabled: bool,
    has_scope: bool,
) -> Option<&'static str> {
    let supported_model = has_local_prepared
        .then_some(status)
        .flatten()
        .filter(|s| s.is_supported());
    match target_id {
        "speech-language" => {
            if supported_model.is_some() || has_cloud_provider {
                None
            } else {
                Some("voice-transcription-destination")
            }
        }
        "speech-models" => (!has_local_prepared).then_some("voice-transcription-destination"),
        "speech-hardware" | "speech-streaming" => {
            (!(has_local_prepared && supported_model.is_some())).then_some("voice-transcription-destination")
        }
        "transcription-test" => {
            if !has_local_prepared {
                Some("voice-transcription-destination")
            } else if matches!(
                supported_model,
                None | Some(EnvironmentSpeechStatus::Supported {
                    state: EnvironmentSpeechState::MissingModel,
                    ..
                })
            ) {
                Some("speech-models")
            } else {
                None
            }
        }
        "speech-post-processing-model" => {
            (!(cleanup_enabled && has_scope)).then_some("speech-post-processing")
        }
        "speech-correction-word" => (!cleanup_enabled).then_some("speech-post-processing"),
        _ => None,
    }
}

/// Validation helper for `CloudVoiceConfiguration` (`CloudVoiceConfiguration.tsx`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudConfigValidation {
    pub can_submit: bool,
    pub key_valid: bool,
    pub model_chosen: bool,
    pub delisted_model: Option<String>,
    pub invalid_key_message: Option<&'static str>,
}

#[allow(clippy::too_many_arguments)]
pub fn validate_cloud_voice_config(
    provider: CloudSpeechProvider,
    active: bool,
    saved_key_present: Option<bool>,
    key_draft: &str,
    using_secret_reference: bool,
    secret_reference_valid: bool,
    selected_model: &str,
    current_model: Option<&str>,
    listed_models: Option<&[CloudSpeechModel]>,
) -> CloudConfigValidation {
    let routed = provider == CloudSpeechProvider::Openrouter;
    let trimmed_key = key_draft.trim();
    let (key_valid, invalid_key_message) = if trimmed_key.is_empty() {
        (false, None)
    } else if using_secret_reference {
        if secret_reference_valid {
            (true, None)
        } else {
            (false, Some("Enter a valid secret reference."))
        }
    } else {
        match validate_cloud_speech_api_key(trimmed_key) {
            Ok(_) => (true, None),
            Err(_) => (false, Some("Enter an API key, not a URL or secret reference.")),
        }
    };
    let delisted_model = if routed {
        current_model
            .filter(|cur| listed_models.is_some_and(|list| !list.iter().any(|m| m.id == *cur)))
            .map(str::to_owned)
    } else {
        None
    };
    let model_chosen = routed
        && validate_cloud_speech_model_id(selected_model)
        && listed_models.is_some_and(|list| list.iter().any(|m| m.id == selected_model));
    let key_ready = if !trimmed_key.is_empty() {
        key_valid
    } else {
        saved_key_present == Some(true)
    };
    let model_changed = selected_model != current_model.unwrap_or("");
    let can_submit = if routed {
        model_chosen && key_ready && (!active || model_changed || key_valid)
    } else {
        key_valid
    };

    CloudConfigValidation { can_submit, key_valid, model_chosen, delisted_model, invalid_key_message }
}

/// Pure phase state for `MicrophoneTest` (`MicrophoneTest.tsx`).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum MicrophoneTestPhase {
    #[default]
    Idle,
    Starting,
    Recording {
        elapsed_secs: f32,
        level: f32,
    },
    Ready {
        duration_secs: f32,
        playing: bool,
        position_secs: f32,
        playback_failed: bool,
    },
}

impl MicrophoneTestPhase {
    pub fn progress_fraction(&self) -> f32 {
        match self {
            Self::Ready { duration_secs, position_secs, .. } if *duration_secs > 0.0 => {
                (position_secs / duration_secs).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::voice_input::{
        SpeechBenchmark, SpeechBenchmarkAccuracyEntry, SpeechBenchmarkTimes,
    };

    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn sample_model(
        id: &str,
        name: &str,
        size: f64,
        state: EnvironmentSpeechModelState,
        active: bool,
        recommended: bool,
        languages: &[&str],
        en_err: Option<f64>,
        gpu_speed: Option<f64>,
        cpu_speed: Option<Option<f64>>,
    ) -> EnvironmentSpeechModel {
        let mut accuracy = BTreeMap::new();
        if let Some(err) = en_err {
            accuracy.insert(
                "en".to_owned(),
                SpeechBenchmarkAccuracyEntry { grade: "A".to_owned(), error_percent: err },
            );
        }
        EnvironmentSpeechModel {
            id: id.to_owned(),
            name: name.to_owned(),
            description: format!("Description for {name}"),
            size,
            languages: languages.iter().map(|s| (*s).to_owned()).collect(),
            accuracy: 0.9,
            speed: 1.0,
            benchmark: Some(SpeechBenchmark {
                accuracy,
                seconds_per_30s: SpeechBenchmarkTimes { cpu: cpu_speed, accelerated: gpu_speed },
            }),
            recommended,
            supports_streaming: true,
            supports_language_detection: languages.len() > 1,
            supports_translation: false,
            active,
            state,
            downloaded: None,
            linked_from: None,
        }
    }

    #[test]
    fn filter_speech_models_respects_recommended_size_accuracy_speed_and_filters() {
        let models = vec![
            sample_model(
                "big",
                "Whisper Large",
                1_500_000_000.0,
                EnvironmentSpeechModelState::Downloadable,
                false,
                false,
                &["en", "fr"],
                Some(3.2),
                Some(4.5),
                Some(Some(12.0)),
            ),
            sample_model(
                "small",
                "Parakeet Small",
                300_000_000.0,
                EnvironmentSpeechModelState::Installed,
                false,
                true,
                &["en"],
                Some(5.5),
                Some(1.1),
                Some(Some(3.0)),
            ),
            sample_model(
                "active",
                "Whisper Medium",
                700_000_000.0,
                EnvironmentSpeechModelState::Installed,
                true,
                false,
                &["en", "es"],
                Some(4.1),
                Some(2.0),
                None,
            ),
        ];

        let rec = filter_speech_models(
            &models,
            &SpeechModelFilterOptions {
                search: "",
                languages: &[],
                downloaded_only: false,
                sort: SpeechModelSort::Recommended,
                accuracy_language: "en",
                benchmark_tier: Some(SpeechBenchmarkTier::Accelerated),
            },
        );
        assert_eq!(rec.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["active", "small", "big"]);

        let by_size = filter_speech_models(
            &models,
            &SpeechModelFilterOptions {
                search: "",
                languages: &[],
                downloaded_only: false,
                sort: SpeechModelSort::Size,
                accuracy_language: "en",
                benchmark_tier: Some(SpeechBenchmarkTier::Accelerated),
            },
        );
        assert_eq!(by_size.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["small", "active", "big"]);

        let by_acc = filter_speech_models(
            &models,
            &SpeechModelFilterOptions {
                search: "",
                languages: &[],
                downloaded_only: false,
                sort: SpeechModelSort::Accuracy,
                accuracy_language: "en",
                benchmark_tier: Some(SpeechBenchmarkTier::Accelerated),
            },
        );
        assert_eq!(by_acc.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["big", "active", "small"]);

        let by_speed = filter_speech_models(
            &models,
            &SpeechModelFilterOptions {
                search: "whisper",
                languages: &["fr".into()],
                downloaded_only: false,
                sort: SpeechModelSort::Speed,
                accuracy_language: "en",
                benchmark_tier: Some(SpeechBenchmarkTier::Cpu),
            },
        );
        assert_eq!(by_speed.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["big"]);

        let bench = model_benchmark_display(&models[2], &[], Some(SpeechBenchmarkTier::Cpu));
        assert_eq!(bench.speed, ModelSpeedSummary::UpdateServer);
        let no_tier = model_benchmark_display(&models[0], &[], None);
        assert_eq!(no_tier.speed, ModelSpeedSummary::UpdateServer);
        let mut unbenchmarked = models[0].clone();
        unbenchmarked.benchmark = None;
        assert_eq!(
            model_benchmark_display(&unbenchmarked, &[], Some(SpeechBenchmarkTier::Accelerated)).speed,
            ModelSpeedSummary::Hidden
        );

        let mut models_with_ultra = models.clone();
        models_with_ultra.push(sample_model(
            "ultra",
            "Whisper Ultra",
            3_000_000_000.0,
            EnvironmentSpeechModelState::Downloadable,
            false,
            false,
            &["en"],
            Some(2.0),
            Some(9.5),
            Some(Some(18.0)),
        ));
        let roles = recommend_speech_models(&models_with_ultra, &["en".into()], Some(SpeechBenchmarkTier::Accelerated));
        assert!(roles.iter().any(|(_, role)| *role == SpeechRecommendationRole::Recommended));
        assert!(roles.iter().any(|(_, role)| *role == SpeechRecommendationRole::Faster));
        assert!(roles.iter().any(|(_, role)| *role == SpeechRecommendationRole::Accurate));
        assert_eq!(
            voice_search_target_fallback("speech-streaming", false, None, false, false, false),
            Some("voice-transcription-destination")
        );
    }

    #[test]
    fn order_speech_languages_puts_preferred_then_common_then_alphabetical() {
        let langs = vec!["uk".into(), "de".into(), "en".into(), "pl".into(), "ja".into()];
        let ordered = order_speech_languages(&langs, &["pl".into()]);
        assert_eq!(ordered, vec!["pl", "en", "de", "ja", "uk"]);
    }

    #[test]
    fn speech_language_display_handles_auto_fixed_and_unavailable() {
        let multi = vec!["fr".into(), "en".into()];
        let d = resolve_speech_language_display(&multi, true, "auto");
        assert_eq!(d.trigger_label, "Auto-detect");
        assert_eq!(d.sorted_languages[0].0, "en");

        let single = vec!["en".into()];
        let d2 = resolve_speech_language_display(&single, false, "auto");
        assert_eq!(d2.trigger_label, "Auto (English)");

        let d3 = resolve_speech_language_display(&single, false, "fr");
        assert_eq!(d3.trigger_label, "English (French unavailable)");
        assert_eq!(d3.unavailable_label.as_deref(), Some("French unavailable"));
    }

    #[test]
    fn dictionary_editor_normalizes_deduplicates_and_manages_aliases() {
        let initial = Vec::new();
        let added = add_dictionary_terms(&initial, "  T3   Code, GPUI\ngpui  ").unwrap();
        assert_eq!(added.len(), 2);
        assert_eq!(added[0].term, "T3 Code");
        assert_eq!(added[1].term, "GPUI");

        let with_alias = add_dictionary_alias(&added, "T3 Code", "tee three code").unwrap();
        assert_eq!(with_alias[0].aliases, vec!["tee three code"]);
        assert!(add_dictionary_alias(&with_alias, "T3 Code", "TEE THREE CODE").is_none());

        let removed = remove_dictionary_term(&with_alias, "T3 Code");
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].term, "GPUI");
    }

    #[test]
    fn voice_search_target_fallbacks_match_web() {
        assert_eq!(
            voice_search_target_fallback("speech-models", false, None, false, false, true),
            Some("voice-transcription-destination")
        );
        assert_eq!(
            voice_search_target_fallback("speech-language", false, None, true, false, true),
            None
        );
        assert_eq!(
            voice_search_target_fallback("speech-correction-word", false, None, false, false, true),
            Some("speech-post-processing")
        );
        assert_eq!(
            voice_search_target_fallback("speech-correction-word", false, None, false, true, true),
            None
        );
    }

    #[test]
    fn cloud_voice_config_validation_handles_fixed_and_openrouter() {
        let fixed = validate_cloud_voice_config(
            CloudSpeechProvider::Openai,
            false,
            Some(false),
            "https://not-a-key",
            false,
            false,
            "",
            None,
            None,
        );
        assert!(!fixed.can_submit);
        assert!(fixed.invalid_key_message.is_some());

        let listed = vec![CloudSpeechModel {
            id: "openai/whisper-1".into(),
            name: "Whisper 1".into(),
        }];
        let routed = validate_cloud_voice_config(
            CloudSpeechProvider::Openrouter,
            false,
            Some(true),
            "",
            false,
            false,
            "openai/whisper-1",
            None,
            Some(&listed),
        );
        assert!(routed.can_submit);
        assert!(routed.model_chosen);

        let delisted = validate_cloud_voice_config(
            CloudSpeechProvider::Openrouter,
            true,
            Some(true),
            "",
            false,
            false,
            "old/delisted",
            Some("old/delisted"),
            Some(&listed),
        );
        assert!(!delisted.can_submit);
        assert_eq!(delisted.delisted_model.as_deref(), Some("old/delisted"));
    }
}
