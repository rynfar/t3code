//! Compaction bookkeeping of `deriveMessagesTimelineRows` (`MessagesTimeline.logic.ts`): a bare
//! `/compact` turn is the compaction itself, so one compaction row stands in for its prompt and
//! working indicator. Idle-resume compaction ages are not ported.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::folds::{entry_run_id, is_compaction, is_compaction_command};
use super::{ChatMessage, TimelineEntry, WorkLogEntry};

/// `IDLE_COMPACTING_LABELS`.
pub(super) fn compacting_label(reason: &str) -> &'static str {
    match reason {
        "beforeCacheExpires" => "Compacting before the cache expires…",
        _ => "Compacting…",
    }
}

/// `IDLE_COMPACTION_TITLES`.
pub(super) fn compaction_title(reason: &str) -> &'static str {
    match reason {
        "beforeCacheExpires" => "Context automatically compacted before cache expiry",
        _ => "Context compacted before sending",
    }
}

/// `idleCompactionOf`: servers before `idleCompaction` marked their own as agent-created.
fn idle_reason(message: &ChatMessage) -> Option<String> {
    message.meta.idle_compaction.clone().or_else(|| {
        (message.meta.created_by.as_deref() == Some("agent") && message.meta.creation_source.as_deref() == Some("server"))
            .then(|| "beforeCacheExpires".to_owned())
    })
}

pub(super) struct Compaction {
    /// Runs started by a `/compact` prompt, with T3 Code's reason when it compacted on its own.
    pub(super) runs: HashMap<String, Option<String>>,
    pub(super) compacted: HashSet<String>,
    pub(super) running_run: Option<String>,
    /// Compacting on return holds the sent message back, so the working row says why.
    pub(super) active_run: Option<String>,
    pub(super) only_run_finished: bool,
    pub(super) active: bool,
}

impl Compaction {
    pub(super) fn derive(
        entries: &[Arc<TimelineEntry>],
        is_working: bool,
        unsettled: Option<&str>,
        in_active_run: &dyn Fn(&WorkLogEntry) -> bool,
    ) -> Self {
        let mut runs = HashMap::new();
        let mut compacted = HashSet::new();
        for entry in entries {
            if let Some(message) = entry.message().filter(|m| is_compaction_command(m))
                && let Some(run) = &message.run_id
            {
                runs.insert(run.clone(), idle_reason(message));
            } else if is_compaction(entry)
                && let Some(run) = entry.work().and_then(|w| w.run_id.clone())
            {
                compacted.insert(run);
            }
        }
        let active_entry = entries.iter().any(|e| is_compaction(e) && e.work().is_some_and(|w| in_active_run(w)));
        let active_run_is_compaction = is_working && unsettled.is_some_and(|run| runs.contains_key(run));
        let run_finished = active_run_is_compaction && unsettled.is_some_and(|run| compacted.contains(run)) && !active_entry;
        let only_run_finished = run_finished
            && !entries.iter().any(|e| entry_run_id(e) == unsettled && !is_compaction(e) && !e.message().is_some_and(|m| is_compaction_command(m)));
        let running_run = (active_run_is_compaction && !run_finished).then(|| unsettled.unwrap_or_default().to_owned());
        let active_run = running_run.clone().filter(|run| runs.get(run).is_some_and(|reason| reason.as_deref() == Some("onReturn")));
        Compaction {
            runs,
            compacted,
            running_run,
            active_run,
            only_run_finished,
            active: active_entry || (active_run_is_compaction && !run_finished),
        }
    }

    /// `appendWorkingRow`'s guard; `Some(compacting)` when the working row shows.
    pub(super) fn working_row(&self) -> Option<bool> {
        if self.only_run_finished || (self.running_run.is_some() && self.active_run.is_none()) {
            return None;
        }
        Some(self.active_run.is_some())
    }

    pub(super) fn reason(&self, run: Option<&str>) -> Option<&str> {
        run.and_then(|run| self.runs.get(run)).and_then(|reason| reason.as_deref())
    }
}
