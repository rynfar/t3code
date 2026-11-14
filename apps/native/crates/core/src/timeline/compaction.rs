//! Compaction bookkeeping of `deriveMessagesTimelineRows` (`MessagesTimeline.logic.ts`): a bare
//! `/compact` turn is the compaction itself, so one compaction row stands in for its prompt and
//! working indicator, and an answered idle-resume compaction prompt folds into the compaction row.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use super::folds::{entry_run_id, is_compaction, is_compaction_command};
use super::{ChatMessage, TimelineEntry, WorkLogEntry};

const CLAUDE_RESUME_COMPACTION_COMPACT_ANSWER: &str = "Compact and continue";

fn is_age_label(text: &str) -> bool {
    if let Some(mins) = text.strip_suffix('m') {
        if !mins.is_empty() && mins.bytes().all(|b| b.is_ascii_digit()) {
            return true;
        }
        if let Some((hours, mins)) = mins.split_once("h ") {
            return !hours.is_empty()
                && hours.bytes().all(|b| b.is_ascii_digit())
                && !mins.is_empty()
                && mins.bytes().all(|b| b.is_ascii_digit());
        }
    }
    false
}

fn is_comma_grouped_digits(text: &str) -> bool {
    let mut parts = text.split(',');
    let Some(first) = parts.next() else { return false };
    if !(1..=3).contains(&first.len()) || !first.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    parts.all(|part| part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()))
}

fn parse_resume_compaction_age(question: &str) -> Option<&str> {
    let rest = question.strip_prefix("This session is ")?;
    let (age, rest) = rest.split_once(" old and uses ")?;
    if !is_age_label(age) {
        return None;
    }
    let tokens = rest.strip_suffix(" tokens. Compact it before continuing?")?;
    is_comma_grouped_digits(tokens).then_some(age)
}

/// `claudeResumeCompactionAge` (`packages/shared/src/claudeCompaction.ts`).
pub(super) fn claude_resume_compaction_age(question_answer: &Value) -> Option<String> {
    let answers = question_answer.get("answers")?.as_object()?;
    let texts = question_answer.get("questionTextById").and_then(Value::as_object);
    for (id, answer) in answers {
        let question = texts.and_then(|map| map.get(id)).and_then(Value::as_str).unwrap_or(id);
        let Some(age) = parse_resume_compaction_age(question) else { continue };
        let chose_compact = match answer {
            Value::String(text) => text == CLAUDE_RESUME_COMPACTION_COMPACT_ANSWER,
            Value::Array(items) => items.iter().any(|item| item.as_str() == Some(CLAUDE_RESUME_COMPACTION_COMPACT_ANSWER)),
            _ => false,
        };
        if chose_compact {
            return Some(age.to_owned());
        }
    }
    None
}

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
    pub(super) resume_age_by_entry: HashMap<String, String>,
    pub(super) running_run: Option<String>,
    /// Compacting on return holds the sent message back, or a resume compaction runs mid-turn.
    pub(super) active_run: Option<String>,
    pub(super) resume_started_at: Option<String>,
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
        let mut pending_resume_age: HashMap<String, String> = HashMap::new();
        let mut resume_age_by_entry = HashMap::new();
        for entry in entries {
            if let Some(message) = entry.message().filter(|m| is_compaction_command(m))
                && let Some(run) = &message.run_id
            {
                runs.insert(run.clone(), idle_reason(message));
            } else if let Some(work) = entry.work()
                && let Some(run) = &work.run_id
            {
                if is_compaction(entry) {
                    compacted.insert(run.clone());
                    if let Some(age) = pending_resume_age.remove(run) {
                        resume_age_by_entry.insert(entry.id.clone(), age);
                    }
                }
                if let Some(age) = work.question_answer.as_ref().and_then(claude_resume_compaction_age) {
                    pending_resume_age.insert(run.clone(), age);
                }
            }
        }
        let active_compaction = entries.iter().find(|e| is_compaction(e) && e.work().is_some_and(|w| in_active_run(w)));
        let active_entry = active_compaction.is_some();
        let active_run_is_compaction = is_working && unsettled.is_some_and(|run| runs.contains_key(run));
        let run_finished = active_run_is_compaction && unsettled.is_some_and(|run| compacted.contains(run)) && !active_entry;
        let only_run_finished = run_finished
            && !entries.iter().any(|e| entry_run_id(e) == unsettled && !is_compaction(e) && !e.message().is_some_and(|m| is_compaction_command(m)));
        let running_run = (active_run_is_compaction && !run_finished).then(|| unsettled.unwrap_or_default().to_owned());
        let compacts_on_return =
            running_run.as_ref().is_some_and(|run| runs.get(run).is_some_and(|reason| reason.as_deref() == Some("onReturn")));
        let resume_compaction = active_compaction.filter(|e| {
            e.work().and_then(|w| w.run_id.as_ref()).is_some() && resume_age_by_entry.contains_key(&e.id)
        });
        let active_run = if compacts_on_return {
            running_run.clone()
        } else {
            resume_compaction.and_then(|e| e.work()).and_then(|w| w.run_id.clone())
        };
        let resume_started_at = (!compacts_on_return)
            .then(|| resume_compaction.map(|e| e.created_at.clone()))
            .flatten();
        Compaction {
            runs,
            compacted,
            resume_age_by_entry,
            running_run,
            active_run,
            resume_started_at,
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

    pub(super) fn resume_age(&self, entry_id: &str) -> Option<&str> {
        self.resume_age_by_entry.get(entry_id).map(String::as_str)
    }

    pub(super) fn resume_started_at(&self) -> Option<&str> {
        self.resume_started_at.as_deref()
    }
}
