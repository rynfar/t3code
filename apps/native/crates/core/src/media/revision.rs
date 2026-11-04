//! Ports `packages/client-runtime/src/state/assets.ts::makeRevisionedAssetUrlFamily`.
use super::AssetUrl;

const KEEP_URL_MIN_LIFE_MS: f64 = 5. * 60_000.;
const MIN_RENEWAL_INTERVAL_MS: f64 = 30_000.;

#[derive(Debug, PartialEq)]
pub enum AssetFetch {
    Ready(AssetUrl),
    Mint,
    Waiting,
}

pub struct RevisionedAssetUrl {
    pub shown: Option<Result<AssetUrl, String>>,
    minted_under: Option<Option<String>>,
    requested: Option<String>,
    waiting: bool,
    rejected: bool,
    last_success_at: Option<f64>,
    last_settled_at: Option<f64>,
}

impl RevisionedAssetUrl {
    pub fn new(shown: Option<AssetUrl>, waiting: bool) -> Self {
        Self {
            shown: shown.map(Ok),
            minted_under: None,
            requested: None,
            waiting,
            rejected: false,
            last_success_at: None,
            last_settled_at: None,
        }
    }

    /// Returns true only when the caller must start a mint. A first read can adopt an
    /// existing mint; a later revision must wait for it and then mint again.
    pub fn read(&mut self, revision: Option<&str>) -> bool {
        self.requested = revision.map(str::to_owned);
        if self.minted_under.is_none() && self.waiting {
            self.minted_under = Some(self.requested.clone());
            return false;
        }
        if self.waiting || self.minted_under.as_ref() == Some(&self.requested) {
            return false;
        }
        self.minted_under = Some(self.requested.clone());
        self.waiting = true;
        true
    }

    pub fn refresh_delay(&self, now: f64, visible: bool) -> Option<f64> {
        super::asset_refresh_delay(if self.waiting { Some(now) } else { self.last_settled_at }, now, visible)
    }

    pub fn refresh(&mut self, now: f64, remount: bool, visible: bool) -> bool {
        let stale = if remount { super::asset_is_stale(self.last_settled_at, now) }
            else { super::asset_refresh_delay(self.last_settled_at, now, visible) == Some(0.) };
        if !visible || self.waiting || !stale { return false; }
        self.waiting = true;
        true
    }

    /// Short TTLs and clock skew must not prevent fetching a freshly signed URL.
    pub fn prepare_fetch(&mut self, now: f64) -> AssetFetch {
        let recently_minted = self
            .last_success_at
            .is_some_and(|at| now - at < MIN_RENEWAL_INTERVAL_MS);
        // Older responses can omit expiresAt; a rejected fetch still forces a new mint.
        if let Some(Ok(url)) = &self.shown
            && !self.rejected
            && (recently_minted
                || url.expires_at <= 0.
                || url.expires_at - now > KEEP_URL_MIN_LIFE_MS)
        {
            return AssetFetch::Ready(url.clone());
        }
        if self.waiting {
            return AssetFetch::Waiting;
        }
        self.minted_under = Some(self.requested.clone());
        self.waiting = true;
        AssetFetch::Mint
    }

    pub fn fetch_failed(&mut self, relative_url: &str) {
        if self
            .shown
            .as_ref()
            .is_some_and(|shown| matches!(shown, Ok(url) if url.relative_url == relative_url))
        {
            self.rejected = true;
        }
    }

    /// Preserves the displayed URL across query results, including across revision nodes.
    /// The return value starts the mint deferred by a revision change while waiting.
    pub fn settle(&mut self, next: Result<AssetUrl, String>, now: f64) -> bool {
        self.last_settled_at = Some(now);
        self.last_success_at = next.as_ref().ok().map(|_| now);
        let keep = match (&self.shown, &next) {
            (Some(Ok(shown)), Ok(next)) => {
                !self.rejected
                    && next.file_revision.is_some()
                    && next.file_revision == shown.file_revision
                    && shown.expires_at - now > KEEP_URL_MIN_LIFE_MS
            }
            _ => false,
        };
        if !keep {
            self.shown = Some(next);
        }
        self.waiting = false;
        self.rejected = false;
        let revision = self.requested.clone();
        self.read(revision.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn minted(n: usize, file: Option<&str>, expires_at: f64) -> AssetUrl {
        AssetUrl {
            relative_url: format!("/a/{n}"),
            dimensions: None,
            expires_at,
            file_revision: file.map(str::to_owned),
        }
    }
    fn url(state: &RevisionedAssetUrl) -> &str {
        &state.shown.as_ref().unwrap().as_ref().unwrap().relative_url
    }
    #[test]
    fn remints_settled_urls_on_first_read_and_new_revisions_but_not_remounts() {
        let mut state = RevisionedAssetUrl::new(Some(minted(0, None, 3600000.)), false);
        assert!(state.read(Some("mutation-1")));
        assert!(!state.settle(Ok(minted(1, None, 3600000.)), 0.));
        assert!(!state.read(Some("mutation-1")));
        assert_eq!(url(&state), "/a/1");
        assert!(state.read(Some("mutation-2")));
    }
    #[test]
    fn reverting_to_an_earlier_revision_remints() {
        let mut state = RevisionedAssetUrl::new(None, false);
        for revision in ["a", "b", "a"] {
            assert!(state.read(Some(revision)));
            assert!(!state.settle(Ok(minted(1, None, 3600000.)), 0.));
        }
    }
    #[test]
    fn first_read_shares_an_inflight_mint_including_null_revision() {
        let mut state = RevisionedAssetUrl::new(None, true);
        assert!(!state.read(None));
        assert!(!state.settle(Ok(minted(1, None, 3600000.)), 0.));
        assert!(!state.read(None));
        assert!(state.read(Some("edit")));
    }
    #[test]
    fn revision_changing_during_a_mint_waits_then_remints() {
        let mut state = RevisionedAssetUrl::new(Some(minted(0, None, 3600000.)), false);
        assert!(state.read(Some("mutation-1")));
        assert!(!state.settle(Ok(minted(1, None, 3600000.)), 0.));
        assert!(state.read(Some("mutation-2")));
        assert!(!state.read(Some("mutation-3")));
        assert!(state.settle(Ok(minted(2, None, 3600000.)), 0.));
        assert!(!state.settle(Ok(minted(3, None, 3600000.)), 0.));
        assert!(!state.read(Some("mutation-3")));
    }
    #[test]
    fn keeps_the_shown_url_across_revisions_until_the_file_changes() {
        let mut state = RevisionedAssetUrl::new(Some(minted(0, Some("file-a"), 3600000.)), false);
        for revision in 1..=3 {
            assert!(state.read(Some(&revision.to_string())));
            assert_eq!(url(&state), "/a/0");
            state.settle(Ok(minted(revision, Some("file-a"), 3600000.)), 0.);
            assert_eq!(url(&state), "/a/0");
        }
        assert!(state.read(Some("4")));
        state.settle(Ok(minted(4, Some("file-b"), 3600000.)), 0.);
        assert_eq!(url(&state), "/a/4");
    }
    #[test]
    fn missing_file_revision_errors_and_five_minute_boundary_do_not_keep_urls() {
        for (revision, expiry, now) in [
            (None, 3600000., 0.),
            (Some("file-a"), 300000., 0.),
            (Some("file-a"), 600000., 300000.),
        ] {
            let mut state = RevisionedAssetUrl::new(Some(minted(0, Some("file-a"), expiry)), false);
            state.read(None);
            state.settle(Ok(minted(1, revision, 3600000.)), now);
            assert_eq!(url(&state), "/a/1");
        }
        let mut state = RevisionedAssetUrl::new(Some(minted(0, Some("a"), 3600000.)), false);
        state.read(None);
        state.settle(Err("missing".into()), 0.);
        assert_eq!(state.shown, Some(Err("missing".into())));
        assert!(state.read(Some("retry")));
    }
    #[test]
    fn cache_miss_remints_within_five_minutes_of_expiry_without_a_new_mutation() {
        for now in [3_300_000., 3_600_000.] {
            let mut state = RevisionedAssetUrl::new(None, false);
            assert!(state.read(Some("mutation")));
            state.settle(Ok(minted(1, Some("same-file"), 3_600_000.)), 0.);
            assert!(matches!(
                state.prepare_fetch(3_299_999.),
                AssetFetch::Ready(_)
            ));
            assert_eq!(state.prepare_fetch(now), AssetFetch::Mint);
            assert_eq!(state.prepare_fetch(now), AssetFetch::Waiting);
            assert!(!state.read(Some("mutation")));
            assert!(!state.settle(Ok(minted(2, Some("same-file"), now + 3_600_000.)), now));
            assert_eq!(url(&state), "/a/2");
            assert!(matches!(state.prepare_fetch(now), AssetFetch::Ready(_)));
        }
    }

    #[test]
    fn signed_fetch_failure_resigns_and_never_keeps_the_rejected_url() {
        let mut state = RevisionedAssetUrl::new(None, false);
        state.read(None);
        state.settle(Ok(minted(1, Some("same-file"), 3_600_000.)), 0.);
        state.fetch_failed("/a/1");
        assert_eq!(state.prepare_fetch(0.), AssetFetch::Mint);
        assert_eq!(state.prepare_fetch(0.), AssetFetch::Waiting);
        assert!(!state.settle(Ok(minted(2, Some("same-file"), 3_600_000.)), 0.));
        assert_eq!(url(&state), "/a/2");
        state.fetch_failed("/a/1");
        assert!(matches!(state.prepare_fetch(0.), AssetFetch::Ready(_)));
        assert_eq!(url(&state), "/a/2");
    }

    #[test]
    fn signed_fetch_failure_during_a_mutation_mint_waits_for_the_fresh_signature() {
        let mut state = RevisionedAssetUrl::new(None, false);
        state.read(Some("a"));
        state.settle(Ok(minted(1, Some("same-file"), 3_600_000.)), 0.);
        assert!(state.read(Some("b")));
        state.fetch_failed("/a/1");
        assert_eq!(state.prepare_fetch(0.), AssetFetch::Waiting);
        assert!(!state.settle(Ok(minted(2, Some("same-file"), 3_600_000.)), 0.));
        assert_eq!(url(&state), "/a/2");
        assert!(matches!(state.prepare_fetch(0.), AssetFetch::Ready(_)));
    }
    #[test]
    fn mounted_refresh_shares_mints_and_remounts_only_after_five_minutes() {
        let mut state = RevisionedAssetUrl::new(None, false);
        assert!(state.read(None));
        state.settle(Ok(minted(0, Some("same"), 7_200_000.)), 0.);
        assert!(!state.refresh(300_000., true, false));
        assert!(!state.refresh(299_999., true, true));
        assert!(state.refresh(300_000., true, true));
        assert!(!state.refresh(300_000., true, true));
        state.settle(Ok(minted(1, Some("same"), 7_200_000.)), 300_000.);
        assert_eq!(url(&state), "/a/0");
        assert!(!state.refresh(2_100_000., false, false));
        assert!(state.refresh(2_100_000., false, true));
        assert!(!state.refresh(2_100_000., false, true));
    }

    #[test]
    fn periodic_refresh_recovers_from_a_failed_mint_at_a_bounded_interval() {
        let mut state = RevisionedAssetUrl::new(None, false);
        assert!(state.read(None));
        state.settle(Err("temporarily unavailable".into()), 0.);
        assert!(!state.refresh(1_799_999., false, true));
        assert!(state.refresh(1_800_000., false, true));
        assert!(!state.refresh(1_800_000., false, true));
    }

    #[test]
    fn successful_short_lived_renewals_are_fetchable_without_a_mint_loop() {
        for lifetime in [60_000., -60_000.] {
            let mut state = RevisionedAssetUrl::new(None, false);
            state.read(None);
            state.settle(Ok(minted(0, Some("same-file"), 3_600_000.)), 0.);
            let now = 3_600_000.;
            assert_eq!(state.prepare_fetch(now), AssetFetch::Mint);
            state.settle(Ok(minted(1, Some("same-file"), now + lifetime)), now);
            for elapsed in [0., 0., 1., 10_000., 29_999.] {
                assert_eq!(
                    state.prepare_fetch(now + elapsed),
                    AssetFetch::Ready(minted(1, Some("same-file"), now + lifetime))
                );
                assert!(!state.read(None));
            }
            let later = now + 30_000.;
            assert_eq!(state.prepare_fetch(later), AssetFetch::Mint);
            assert_eq!(state.prepare_fetch(later), AssetFetch::Waiting);
            state.settle(Ok(minted(2, Some("same-file"), later + lifetime)), later);
            assert_eq!(
                state.prepare_fetch(later),
                AssetFetch::Ready(minted(2, Some("same-file"), later + lifetime))
            );
            state.fetch_failed("/a/2");
            assert_eq!(state.prepare_fetch(later), AssetFetch::Mint);
        }
    }
}
