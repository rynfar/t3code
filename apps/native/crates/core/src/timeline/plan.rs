//! Proposed-plan text, follow-up, export, and actionability rules.
//! Ports `apps/web/src/proposedPlan.ts`, `packages/shared/src/proposedPlanText.ts`,
//! and the proposed-plan helpers in `apps/web/src/session-logic.ts`.

use crate::server_config::InteractionMode;
use crate::thread::ThreadProjection;
use crate::timeline::activity::{plan_title, strip_displayed_plan_markdown as strip_plan_markdown_body};
use crate::turn_item::TurnItemKind;

pub const PLAN_IMPLEMENTATION_PROMPT_PREFIX: &str = "PLEASE IMPLEMENT THIS PLAN:\n";

/// A proposed plan eligible for follow-up actions in the composer (`LatestProposedPlanState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveProposedPlan {
    pub id: String,
    pub run_id: Option<String>,
    pub markdown: String,
    pub title: Option<String>,
}

/// Follow-up message computed from the current composer draft and ready proposed plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFollowUpSubmission {
    pub text: String,
    pub interaction_mode: InteractionMode,
}

/// Extract the first markdown heading from `plan_markdown` (`proposedPlanTitle`).
pub fn proposed_plan_title(plan_markdown: &str) -> Option<String> {
    plan_title(plan_markdown)
}

/// Strip the leading title heading and an immediate `Summary` subheading (`stripDisplayedPlanMarkdown`).
pub fn strip_displayed_plan_markdown(plan_markdown: &str) -> String {
    strip_plan_markdown_body(plan_markdown)
}

/// Collapsed preview markdown for `ProposedPlanCard` (`buildCollapsedProposedPlanPreviewMarkdown`).
pub fn build_collapsed_proposed_plan_preview_markdown(plan_markdown: &str, max_lines: usize) -> (String, bool) {
    let display = strip_displayed_plan_markdown(plan_markdown);
    let mut lines: Vec<&str> = display.lines().collect();
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        let fallback = proposed_plan_title(plan_markdown).unwrap_or_else(|| "Plan preview unavailable.".to_owned());
        return (fallback, false);
    }
    if lines.len() <= max_lines {
        return (lines.join("\n"), false);
    }
    let mut preview = lines[..max_lines].join("\n");
    preview.push_str("\n...");
    (preview, true)
}

fn sanitize_plan_file_segment(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in input.to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { "plan".to_owned() } else { out }
}

/// Export filename for downloading or saving a plan (`buildProposedPlanMarkdownFilename`).
pub fn build_proposed_plan_markdown_filename(plan_markdown: &str) -> String {
    let title = proposed_plan_title(plan_markdown);
    format!("{}.md", sanitize_plan_file_segment(title.as_deref().unwrap_or("plan")))
}

/// Normalize markdown for export (`normalizePlanMarkdownForExport`).
pub fn normalize_plan_markdown_for_export(plan_markdown: &str) -> String {
    format!("{}\n", plan_markdown.trim_end())
}

/// Implementation prompt sent when clicking `Implement` (`buildPlanImplementationPrompt`).
pub fn build_plan_implementation_prompt(plan_markdown: &str) -> String {
    format!("{PLAN_IMPLEMENTATION_PROMPT_PREFIX}{}", plan_markdown.trim())
}

/// Decide text and interaction mode for a plan-mode follow-up (`resolvePlanFollowUpSubmission`).
pub fn resolve_plan_follow_up_submission(draft_text: &str, plan_markdown: &str) -> PlanFollowUpSubmission {
    let trimmed = draft_text.trim();
    if !trimmed.is_empty() {
        return PlanFollowUpSubmission { text: trimmed.to_owned(), interaction_mode: InteractionMode::Plan };
    }
    PlanFollowUpSubmission { text: build_plan_implementation_prompt(plan_markdown), interaction_mode: InteractionMode::Default }
}

/// Title used when implementing a plan in a new thread (`buildPlanImplementationThreadTitle`).
pub fn build_plan_implementation_thread_title(plan_markdown: &str) -> String {
    match proposed_plan_title(plan_markdown) {
        Some(title) => format!("Implement {title}"),
        None => "Implement plan".to_owned(),
    }
}

/// Find the latest actionable proposed plan on a settled projection (`findLatestProposedPlan`).
pub fn find_active_proposed_plan(projection: &ThreadProjection) -> Option<ActiveProposedPlan> {
    let latest_run = projection.runs.last()?;
    let settled = matches!(latest_run.status.as_str(), "completed" | "cancelled" | "interrupted" | "failed");
    if !settled {
        return None;
    }
    let latest_run_id = latest_run.id.as_str();
    let item = projection.turn_items.iter().rev().find(|item| {
        matches!(item.kind, TurnItemKind::ProposedPlan { streaming: false, .. })
            && item.run_id.as_deref() == Some(latest_run_id)
    })?;
    let TurnItemKind::ProposedPlan { plan_id, markdown, streaming } = &item.kind else {
        return None;
    };
    if *streaming || markdown.trim().is_empty() {
        return None;
    }
    let status = projection
        .plans
        .iter()
        .find(|p| p.id == *plan_id && p.value.get("kind").and_then(|v| v.as_str()) == Some("proposed_plan"))
        .and_then(|p| p.value.get("status").and_then(|v| v.as_str()))
        .unwrap_or("active");
    if matches!(status, "implemented" | "superseded") {
        return None;
    }
    Some(ActiveProposedPlan {
        id: plan_id.clone(),
        run_id: item.run_id.clone(),
        markdown: markdown.clone(),
        title: proposed_plan_title(markdown),
    })
}

/// Whether the composer should display the plan follow-up banner and Implement/Refine actions.
pub fn should_show_plan_follow_up_prompt(
    pending_user_input_count: usize,
    interaction_mode: InteractionMode,
    running: bool,
    active_plan: Option<&ActiveProposedPlan>,
    has_composer_attachments: bool,
) -> bool {
    pending_user_input_count == 0
        && interaction_mode == InteractionMode::Plan
        && !running
        && active_plan.is_some()
        && !has_composer_attachments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_displayed_removes_title_and_summary_heading() {
        let md = "# My Plan\n\n## Summary\n\n- Step 1\n- Step 2\n";
        assert_eq!(strip_displayed_plan_markdown(md), "- Step 1\n- Step 2");
        assert_eq!(proposed_plan_title(md).as_deref(), Some("My Plan"));
    }

    #[test]
    fn collapsed_preview_truncates_and_falls_back_to_title() {
        let md = "# Empty Body";
        assert_eq!(build_collapsed_proposed_plan_preview_markdown(md, 8), ("Empty Body".to_owned(), false));

        let many: String = (1..=12).map(|i| format!("line {i}\n")).collect();
        let (preview, truncated) = build_collapsed_proposed_plan_preview_markdown(&many, 8);
        assert!(truncated);
        assert!(preview.ends_with("\n..."));
        assert_eq!(preview.lines().count(), 9);
    }

    #[test]
    fn follow_up_submission_matches_proposed_plan_spec() {
        let context_draft = "Consider this [PR #42](t3-context://v1/review-comment/pr-42)";
        assert_eq!(
            resolve_plan_follow_up_submission(context_draft, "# Plan"),
            PlanFollowUpSubmission { text: context_draft.to_owned(), interaction_mode: InteractionMode::Plan }
        );
        assert_eq!(
            resolve_plan_follow_up_submission("   ", "## Ship it\n\n- step 1\n"),
            PlanFollowUpSubmission {
                text: "PLEASE IMPLEMENT THIS PLAN:\n## Ship it\n\n- step 1".to_owned(),
                interaction_mode: InteractionMode::Default,
            }
        );
    }

    #[test]
    fn filename_and_thread_title_sanitize_headings() {
        let md = "# Refactor: Transcript / Messages (Wave 1C)\n\nBody";
        assert_eq!(build_proposed_plan_markdown_filename(md), "refactor-transcript-messages-wave-1c.md");
        assert_eq!(build_plan_implementation_thread_title(md), "Implement Refactor: Transcript / Messages (Wave 1C)");
        assert_eq!(build_plan_implementation_thread_title("plain body"), "Implement plan");
        assert_eq!(normalize_plan_markdown_for_export("# Hi\n\n  "), "# Hi\n");
    }
}
