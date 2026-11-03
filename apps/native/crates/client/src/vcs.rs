//! Ref refresh I/O for `client-runtime/src/state/vcs.ts` and the ref-affecting commands'
//! `onSettled` hooks in `state/vcs.ts` and `state/git.ts`.

use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;
use t3_core::vcs::{ListRefsResult, method, refs::RefCache};
use tokio::sync::watch;

use crate::supervisor::ClientHandle;

#[derive(Clone)]
pub(crate) struct RefState {
    cache: Arc<Mutex<RefCache>>,
    revision: watch::Sender<u64>,
}

impl Default for RefState {
    fn default() -> Self {
        Self {
            cache: Arc::default(),
            revision: watch::channel(0).0,
        }
    }
}

impl RefState {
    fn lock(&self) -> MutexGuard<'_, RefCache> {
        self.cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn on_settled(&self, method: &'static str) -> Option<OnSettled> {
        t3_core::vcs::refs::invalidates_refs(method).then(|| OnSettled { state: self.clone(), method })
    }
}

pub(crate) struct OnSettled {
    state: RefState,
    method: &'static str,
}

impl Drop for OnSettled {
    fn drop(&mut self) {
        let mut cache = self.state.lock();
        if cache.settle(self.method) {
            self.state.revision.send_replace(cache.revision());
        }
    }
}

impl ClientHandle {
    pub fn refs_revision(&self) -> watch::Receiver<u64> {
        self.refs.revision.subscribe()
    }

    /// `None` means invalidation superseded this response; it must never reach the picker.
    pub async fn list_refs(&self, input: Value) -> Result<Option<ListRefsResult>, String> {
        let generation = *self.connection_generation().borrow();
        let request = self.refs.lock().begin(input);
        let result = self.request(method::VCS_LIST_REFS, request.input.clone()).await;
        if *self.connection_generation().borrow() != generation || self.refs.lock().revision() != request.revision() {
            return Ok(None);
        }
        let value = result.map_err(|error| error.to_string())?;
        let refs = serde_json::from_value::<ListRefsResult>(value).map_err(|error| error.to_string())?;
        Ok(self.refs.lock().commit(&request).then_some(refs))
    }
}
