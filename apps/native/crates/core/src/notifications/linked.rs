//! `delegatedUnderParentAtom` and `delegatedSubagentRequestsAtom` from
//! `packages/client-runtime/src/state/threadShell.ts`. Call with connected saved environments.

use std::collections::{HashMap, HashSet};

use crate::shell::ThreadShell;
use crate::sidebar::tag_filter::ThreadKey;
use crate::sidebar::thread::{SubagentRequest, pending_subagent_requests, requests_from_contributions, subagent_request, contributions_with_delegated, RequestContribution};

pub struct LinkedEnvironment<'a> {
    pub saved_id: &'a str,
    pub canonical_id: &'a str,
    pub threads: &'a [ThreadShell],
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LinkedRequests {
    pub delegated_under_parent: HashSet<ThreadKey>,
    pub requests: HashMap<String, HashMap<String, SubagentRequest>>,
    pub contributions: HashMap<String, Vec<RequestContribution>>,
}

impl LinkedRequests {
    pub fn build(environments: &[LinkedEnvironment<'_>]) -> Self {
        let mut result = Self::default();
        let mut tasks = Vec::new();
        for environment in environments {
            result
                .requests
                .insert(environment.saved_id.into(), pending_subagent_requests(environment.threads));
            for thread in environment.threads {
                let Some(from) = thread.delegated_from.as_ref() else { continue };
                let (Some(environment_id), Some(parent_id)) = (
                    from.get("environmentId").and_then(|v| v.as_str()),
                    from.get("threadId").and_then(|v| v.as_str()),
                ) else {
                    continue;
                };
                for parent in environments.iter().filter(|parent| parent.canonical_id == environment_id) {
                    if parent
                        .threads
                        .iter()
                        .any(|thread| thread.id == parent_id && thread.archived_at.is_none())
                    {
                        result
                            .delegated_under_parent
                            .insert(ThreadKey::new(environment.saved_id, &thread.id));
                    }
                    if environment.canonical_id != environment_id {
                        tasks.push((environment.saved_id, thread, parent.saved_id, parent_id));
                    }
                }
            }
        }
        // Each pass crosses one delegated edge; approval flags only grow, even in cycles.
        for _ in 0..=tasks.len() {
            let mut next = HashMap::new();
            let mut contributions = HashMap::new();
            for environment in environments {
                let delegated = tasks
                    .iter()
                    .filter(|(_, _, parent, _)| *parent == environment.saved_id)
                    .filter_map(|(runs_in, thread, _, parent_id)| {
                        let own = subagent_request(thread);
                        let nested = result.requests.get(*runs_in).and_then(|requests| requests.get(&thread.id)).copied();
                        let request = if own == Some(SubagentRequest::Approval) {
                            own
                        } else {
                            nested.or(own)
                        }?;
                        Some((*parent_id, request))
                    })
                    .collect::<Vec<_>>();
                let current = contributions_with_delegated(environment.threads, &delegated);
                let ancestry = environment.threads.iter().map(|thread| (thread.id.as_str(), &thread.lineage)).collect();
                next.insert(environment.saved_id.to_owned(), requests_from_contributions(&current, &ancestry));
                contributions.insert(environment.saved_id.to_owned(), current);
            }
            result.contributions = contributions;
            if next == result.requests {
                break;
            }
            result.requests = next;
        }
        result
    }
}
