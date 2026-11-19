//! Full-size theme background image tracking, offline/transient retry backoff, and
//! published-vs-saved collision handling from `apps/web/src/themeBackground.ts`
//! (`pingdotgg/t3code#36680`, `pingdotgg/t3code#37843`, `pingdotgg/t3code#37981`,
//! `pingdotgg/t3code#38132`).

use serde_json::{Value, json};

use super::ThemeDefinition;

/// Tagged `AssetResource` name in `packages/contracts/src/assets.ts`.
pub const ENVIRONMENT_THEME_BACKGROUND_ASSET: &str = "environment-theme-background";

/// Maximum accepted theme background image size (16 MiB), matching `apps/server/src/environmentTheme.ts`.
pub const MAX_BACKGROUND_BYTES: usize = 16 * 1024 * 1024;

/// Maximum accepted background revision string length in `packages/contracts/src/server.ts`.
pub const MAX_BACKGROUND_REVISION_LEN: usize = 64;

/// Backoff delays (in milliseconds) after failed background image downloads (`RETRY_DELAYS_MS`).
pub const RETRY_DELAYS_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

/// Builds the `assets.createUrl` payload for an environment theme's background image.
pub fn create_background_url_request(theme_id: &str) -> Value {
    json!({
        "resource": {
            "_tag": ENVIRONMENT_THEME_BACKGROUND_ASSET,
            "themeId": theme_id,
        }
    })
}

/// Returns the published background revision for `theme_id`, or `None` when no published
/// background applies.
///
/// A saved/custom theme wins its id over a published one, so the published image is never
/// shown beneath a saved theme sharing the same id (`pingdotgg/t3code#37981`).
pub fn background_revision<'a>(
    theme_id: Option<&str>,
    custom: &[ThemeDefinition],
    environment: &'a [ThemeDefinition],
) -> Option<&'a str> {
    let id = theme_id.filter(|id| !id.is_empty())?;
    if custom.iter().any(|theme| theme.id == id) {
        return None;
    }
    environment
        .iter()
        .find(|theme| theme.id == id)
        .and_then(|theme| theme.background.as_deref())
        .filter(|rev| !rev.is_empty())
}

/// Unique identity for one published background revision on one environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundKey {
    pub environment_id: String,
    pub theme_id: String,
    pub revision: String,
}

/// Action the host UI should take after evaluating the current theme background state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundAction {
    /// Current image (or absence of one) is already in sync; keep any shown image or in-flight load.
    NoChange,
    /// Clear the currently shown image and abort any in-flight download.
    Clear,
    /// Clear the previous image immediately, abort any prior download, and start fetching `key`.
    Fetch { generation: u64, key: BackgroundKey },
}

/// Retry decision after a single download attempt fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Stale attempt from a superseded generation; ignore.
    Stale,
    /// Retry after `delay_ms` milliseconds.
    RetryAfterMs(u64),
    /// All scheduled retries failed; cleared `shown_key` so the next repaint or reconnect retries once.
    Exhausted,
}

/// State machine mirroring `applyThemeBackground` and `startThemeBackground` in `themeBackground.ts`.
#[derive(Debug, Default)]
pub struct ThemeBackgroundTracker {
    /// Tracks the applied theme id independently of live editor draft previews (`#38132`).
    applied_theme_id: Option<String>,
    shown_key: Option<BackgroundKey>,
    generation: u64,
    in_flight: bool,
    retries: usize,
}

impl ThemeBackgroundTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn applied_theme_id(&self) -> Option<&str> {
        self.applied_theme_id.as_deref()
    }

    pub fn shown_key(&self) -> Option<&BackgroundKey> {
        self.shown_key.as_ref()
    }

    /// Evaluates whether the background image should be fetched, cleared, or left alone.
    pub fn apply(
        &mut self,
        environment_id: Option<&str>,
        theme_id: Option<&str>,
        custom: &[ThemeDefinition],
        environment: &[ThemeDefinition],
    ) -> BackgroundAction {
        self.applied_theme_id = theme_id.map(str::to_owned);
        self.evaluate(environment_id, custom, environment)
    }

    /// Re-evaluates using the last `applied_theme_id` (used when connectivity returns or
    /// environment themes update while a live editor preview may be active; `#37843`, `#38132`).
    pub fn reapply(
        &mut self,
        environment_id: Option<&str>,
        custom: &[ThemeDefinition],
        environment: &[ThemeDefinition],
    ) -> BackgroundAction {
        self.evaluate(environment_id, custom, environment)
    }

    fn evaluate(
        &mut self,
        environment_id: Option<&str>,
        custom: &[ThemeDefinition],
        environment: &[ThemeDefinition],
    ) -> BackgroundAction {
        let theme_id = self.applied_theme_id.as_deref();
        let revision = background_revision(theme_id, custom, environment);
        let key = match (environment_id.filter(|id| !id.is_empty()), theme_id, revision) {
            (Some(env), Some(id), Some(rev)) => Some(BackgroundKey {
                environment_id: env.to_owned(),
                theme_id: id.to_owned(),
                revision: rev.to_owned(),
            }),
            _ => None,
        };
        if key == self.shown_key {
            return BackgroundAction::NoChange;
        }
        self.shown_key = key.clone();
        self.generation = self.generation.wrapping_add(1);
        self.retries = 0;
        match key {
            Some(key) => {
                self.in_flight = true;
                BackgroundAction::Fetch { generation: self.generation, key }
            }
            None => {
                self.in_flight = false;
                BackgroundAction::Clear
            }
        }
    }

    /// Marks `generation` as successfully loaded. Returns `true` if still current.
    pub fn on_loaded(&mut self, generation: u64) -> bool {
        if generation != self.generation || !self.in_flight {
            return false;
        }
        self.in_flight = false;
        self.retries = 0;
        true
    }

    /// Records a failed download attempt for `generation` and returns the next retry decision.
    pub fn on_failed(&mut self, generation: u64) -> RetryDecision {
        if generation != self.generation || !self.in_flight {
            return RetryDecision::Stale;
        }
        if let Some(&delay_ms) = RETRY_DELAYS_MS.get(self.retries) {
            self.retries += 1;
            return RetryDecision::RetryAfterMs(delay_ms);
        }
        self.in_flight = false;
        self.shown_key = None;
        RetryDecision::Exhausted
    }

    pub fn clear(&mut self) {
        self.applied_theme_id = None;
        self.shown_key = None;
        self.generation = self.generation.wrapping_add(1);
        self.in_flight = false;
        self.retries = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::themes;
    use serde_json::json;

    fn published(id: &str, background: Option<&str>) -> ThemeDefinition {
        let mut raw = json!({
            "id": id,
            "name": "Nightfall",
            "appearance": "dark",
            "canvas": "#1a1b26",
            "accent": "#7aa2f7"
        });
        if let Some(bg) = background {
            raw["background"] = json!(bg);
        }
        themes::environment(&[raw]).into_iter().next().unwrap()
    }

    fn saved(id: &str) -> ThemeDefinition {
        themes::decode(
            &json!({
                "version": 1,
                "id": id,
                "name": "Nightfall",
                "appearance": "dark",
                "colors": themes::standard(true)
            }),
            false,
        )
        .unwrap()
    }

    #[test]
    fn retries_failed_download_with_1_2_4_8s_backoff_then_retries_once_on_online() {
        let env = vec![published("nightfall", Some("rev-1"))];
        let mut tracker = ThemeBackgroundTracker::new();

        let first = tracker.apply(Some("host"), Some("nightfall"), &[], &env);
        let gen1 = match first {
            BackgroundAction::Fetch { generation, ref key } => {
                assert_eq!(key.environment_id, "host");
                assert_eq!(key.theme_id, "nightfall");
                assert_eq!(key.revision, "rev-1");
                generation
            }
            other => panic!("expected Fetch, got {other:?}"),
        };

        assert_eq!(tracker.on_failed(gen1), RetryDecision::RetryAfterMs(1_000));
        assert_eq!(tracker.on_failed(gen1), RetryDecision::RetryAfterMs(2_000));
        assert_eq!(tracker.on_failed(gen1), RetryDecision::RetryAfterMs(4_000));
        assert_eq!(tracker.on_failed(gen1), RetryDecision::RetryAfterMs(8_000));
        assert_eq!(tracker.on_failed(gen1), RetryDecision::Exhausted);
        assert!(tracker.shown_key().is_none());

        // Connectivity returns -> reapply retries once, and after success further online events are no-ops.
        let retry = tracker.reapply(Some("host"), &[], &env);
        let gen2 = match retry {
            BackgroundAction::Fetch { generation, .. } => generation,
            other => panic!("expected Fetch on reconnect, got {other:?}"),
        };
        assert_ne!(gen1, gen2);
        assert!(tracker.on_loaded(gen2));
        assert_eq!(tracker.reapply(Some("host"), &[], &env), BackgroundAction::NoChange);
    }

    #[test]
    fn saved_theme_with_same_id_suppresses_published_background() {
        let env = vec![published("nightfall", Some("rev-1"))];
        let custom = vec![saved("nightfall")];
        let mut tracker = ThemeBackgroundTracker::new();

        assert_eq!(background_revision(Some("nightfall"), &custom, &env), None);
        assert_eq!(tracker.apply(Some("host"), Some("nightfall"), &custom, &env), BackgroundAction::NoChange);

        // If published was shown first, saving a custom theme with the same id clears it.
        let _ = tracker.apply(Some("host"), Some("nightfall"), &[], &env);
        assert_eq!(tracker.apply(Some("host"), Some("nightfall"), &custom, &env), BackgroundAction::Clear);
    }

    #[test]
    fn reapply_keeps_applied_theme_id_during_live_editor_preview() {
        let env = vec![published("nightfall", Some("rev-1"))];
        let mut tracker = ThemeBackgroundTracker::new();
        let gen1 = match tracker.apply(Some("host"), Some("nightfall"), &[], &env) {
            BackgroundAction::Fetch { generation, .. } => generation,
            other => panic!("expected Fetch, got {other:?}"),
        };
        for _ in 0..5 {
            let _ = tracker.on_failed(gen1);
        }
        // Even if the UI root is temporarily showing a draft preview id, reapply tracks `applied_theme_id`.
        assert_eq!(tracker.applied_theme_id(), Some("nightfall"));
        assert!(matches!(
            tracker.reapply(Some("host"), &[], &env),
            BackgroundAction::Fetch { key, .. } if key.theme_id == "nightfall"
        ));
    }

    #[test]
    fn request_payload_matches_assets_contract() {
        assert_eq!(
            create_background_url_request("nightfall"),
            json!({"resource": {"_tag": "environment-theme-background", "themeId": "nightfall"}})
        );
    }
}
