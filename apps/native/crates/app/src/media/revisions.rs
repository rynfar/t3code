//! Revisioned URLs from `packages/client-runtime/src/state/assets.ts`; records outlive rows,
//! but cap at 512 resources. An evicted resource safely re-mints on its next read.

use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gpui::{Context, EntityId, Task};
use t3_core::media::{AssetResource, AssetUrl, MediaSource, RevisionedAssetUrl};

use super::cache::{Key, MediaCache, RETRY_AFTER};

const MAX_RECORDS: usize = 512;

pub(super) struct Mint {
    state: RevisionedAssetUrl,
    task: Option<Task<()>>,
    waiters: Vec<EntityId>,
    used: u64,
    failed_at: Option<Instant>,
}

#[derive(Default)]
pub(super) struct Revisions {
    records: HashMap<Key, Mint>,
}

impl Revisions {
    pub fn shown_asset(&self, key: &Key) -> Option<Result<&AssetUrl, &str>> {
        self.records
            .get(key)?
            .state
            .shown
            .as_ref()
            .map(|result| result.as_ref().map_err(String::as_str))
    }

    pub fn shown_url(&self, key: &Key) -> Option<Result<&str, &str>> {
        self.shown_asset(key)
            .map(|result| result.map(|asset| asset.relative_url.as_str()))
    }
}

impl MediaCache {
    pub(super) fn refresh_scope(&mut self, scope: &super::MediaScope, cx: &mut Context<Self>) {
        let keys: Vec<_> = self.revisions.records.iter().filter(|(key, mint)| {
            mint.used + 2 >= self.frame && matches!(&key.source, MediaSource::Asset(AssetResource::MediaFile { thread_id, .. }) if Some(thread_id) == scope.thread_id.as_ref())
        }).map(|(key, _)| key.clone()).collect();
        for key in keys {
            self.refresh_asset(
                &key,
                scope.workspace_mutation_id.as_deref(),
                scope.owner,
                cx,
            );
        }
    }

    pub(super) fn refresh_asset(
        &mut self,
        key: &Key,
        revision: Option<&str>,
        owner: EntityId,
        cx: &mut Context<Self>,
    ) {
        if !matches!(
            key.source,
            MediaSource::Asset(AssetResource::MediaFile { .. })
        ) {
            return;
        }
        if !self.revisions.records.contains_key(key) {
            if self.revisions.records.len() >= MAX_RECORDS {
                let oldest = self
                    .revisions
                    .records
                    .iter()
                    .filter(|(_, mint)| mint.task.is_none())
                    .min_by_key(|(_, mint)| mint.used)
                    .map(|(key, _)| key.clone());
                let Some(oldest) = oldest else { return };
                self.revisions.records.remove(&oldest);
            }
            self.revisions.records.insert(
                key.clone(),
                Mint {
                    state: RevisionedAssetUrl::new(None, false),
                    task: None,
                    waiters: Vec::new(),
                    used: self.frame,
                    failed_at: None,
                },
            );
        }
        let mint = self.revisions.records.get_mut(key).unwrap();
        mint.used = self.frame;
        if mint.failed_at.is_some_and(|at| at.elapsed() >= RETRY_AFTER) {
            mint.state = RevisionedAssetUrl::new(None, false);
            mint.failed_at = None;
        }
        let start = mint.state.read(revision);
        if (start || mint.task.is_some()) && !mint.waiters.contains(&owner) {
            mint.waiters.push(owner);
        }
        if start {
            mint.failed_at = None;
            self.start_mint(key.clone(), cx);
        }
    }

    fn start_mint(&mut self, key: Key, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else {
            self.revisions.records.remove(&key);
            return;
        };
        if client.environment() != key.environment.as_ref() {
            self.revisions.records.remove(&key);
            return;
        }
        let runtime = state.runtime().clone();
        let MediaSource::Asset(resource) = key.source.clone() else {
            return;
        };
        let permits = self.permits.clone();
        let job = runtime.spawn(async move {
            let _permit = permits
                .acquire_owned()
                .await
                .map_err(|_| "Image unavailable.".to_owned())?;
            t3_client::assets::mint_media_url(&client, &resource).await
        });
        let target = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = job
                .await
                .unwrap_or_else(|_| Err("Image unavailable.".into()));
            let _ = this.update(cx, |cache, cx| {
                let Some(mint) = cache.revisions.records.get_mut(&target) else {
                    return;
                };
                mint.task = None;
                mint.failed_at = result.as_ref().err().map(|_| Instant::now());
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64()
                    * 1000.;
                let again = mint.state.settle(result, now);
                if again {
                    mint.failed_at = None;
                }
                let waiters = std::mem::take(&mut mint.waiters);
                if again {
                    cache.start_mint(target, cx);
                }
                let app: &mut gpui::App = cx;
                for owner in waiters {
                    app.notify(owner);
                }
            });
        });
        if let Some(mint) = self.revisions.records.get_mut(&key) {
            mint.task = Some(task);
        }
    }
}
