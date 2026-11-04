//! Ports `packages/client-runtime/src/state/assets.ts::makeRevisionedAssetUrlFamily`.
use super::AssetUrl;

pub struct RevisionedAssetUrl {
    pub shown: Option<Result<AssetUrl, String>>,
    minted_under: Option<Option<String>>,
    requested: Option<String>,
    waiting: bool,
}

impl RevisionedAssetUrl {
    pub fn new(shown: Option<AssetUrl>, waiting: bool) -> Self {
        Self {
            shown: shown.map(Ok),
            minted_under: None,
            requested: None,
            waiting,
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

    /// Preserves the displayed URL across query results, including across revision nodes.
    /// The return value starts the mint deferred by a revision change while waiting.
    pub fn settle(&mut self, next: Result<AssetUrl, String>, now: f64) -> bool {
        let keep = match (&self.shown, &next) {
            (Some(Ok(shown)), Ok(next)) => {
                next.file_revision.is_some()
                    && next.file_revision == shown.file_revision
                    && shown.expires_at - now > 5. * 60_000.
            }
            _ => false,
        };
        if !keep {
            self.shown = Some(next);
        }
        self.waiting = false;
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
}
