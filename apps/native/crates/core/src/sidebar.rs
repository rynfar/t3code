//! Sidebar rows derived from the shell. First cut of the web's grouping (`Sidebar.logic.ts`,
//! `client-runtime/src/state/threadSort.ts`): pinned threads lead, then projects with their
//! unsettled threads by latest activity, then settled threads. Tracked as partial in the ledger.

use crate::shell::{ProjectShell, ShellSnapshot, ThreadShell};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarRow {
    Section { title: &'static str },
    Project { project_id: String },
    Thread { thread_id: String, depth: u8 },
}

/// `DateTimeUtc` always encodes as `toISOString()`, so ISO strings order chronologically.
fn activity(thread: &ThreadShell) -> &str {
    thread.latest_user_message_at.as_deref().unwrap_or(&thread.updated_at)
}

fn listed(thread: &ThreadShell) -> bool {
    thread.lineage.is_listed() && thread.archived_at.is_none() && thread.deleted_at.is_none()
}

pub fn sidebar_rows(snapshot: &ShellSnapshot) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    let threads: Vec<&ThreadShell> = snapshot.threads.iter().filter(|t| listed(t)).collect();

    let mut pinned: Vec<&ThreadShell> = threads.iter().copied().filter(|t| t.pinned_at.is_some()).collect();
    pinned.sort_by(|a, b| a.pin_order_key.cmp(&b.pin_order_key).then_with(|| activity(b).cmp(activity(a))));
    if !pinned.is_empty() {
        rows.push(SidebarRow::Section { title: "Pinned" });
        rows.extend(pinned.iter().map(|t| SidebarRow::Thread { thread_id: t.id.clone(), depth: 0 }));
    }

    let mut projects: Vec<&ProjectShell> = snapshot.projects.iter().filter(|p| p.archived_at.is_none()).collect();
    let latest_in = |project: &ProjectShell| {
        threads
            .iter()
            .filter(|t| t.project_id == project.id && !t.is_settled())
            .map(|t| activity(t))
            .max()
            .unwrap_or("")
            .to_owned()
    };
    projects.sort_by(|a, b| {
        b.pinned_at.is_some().cmp(&a.pinned_at.is_some()).then_with(|| latest_in(b).cmp(&latest_in(a)))
    });
    if !projects.is_empty() {
        rows.push(SidebarRow::Section { title: "Projects" });
    }
    for project in projects {
        rows.push(SidebarRow::Project { project_id: project.id.clone() });
        let mut active: Vec<&ThreadShell> = threads
            .iter()
            .copied()
            .filter(|t| t.project_id == project.id && t.pinned_at.is_none() && !t.is_settled())
            .collect();
        active.sort_by(|a, b| activity(b).cmp(activity(a)).then_with(|| a.id.cmp(&b.id)));
        rows.extend(active.iter().map(|t| SidebarRow::Thread { thread_id: t.id.clone(), depth: 1 }));
    }

    let mut settled: Vec<&ThreadShell> =
        threads.iter().copied().filter(|t| t.pinned_at.is_none() && t.is_settled()).collect();
    settled.sort_by(|a, b| {
        let at = |t: &ThreadShell| t.settled_at.clone().unwrap_or_else(|| activity(t).to_owned());
        at(b).cmp(&at(a)).then_with(|| a.id.cmp(&b.id))
    });
    if !settled.is_empty() {
        rows.push(SidebarRow::Section { title: "Settled" });
        rows.extend(settled.iter().map(|t| SidebarRow::Thread { thread_id: t.id.clone(), depth: 0 }));
    }
    rows
}
