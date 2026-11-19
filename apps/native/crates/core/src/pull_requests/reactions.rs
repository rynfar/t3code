//! Pull request reactions (`packages/client-runtime/src/pullRequestReactions.ts`).

use std::collections::HashMap;

use serde_json::{Value, json};

use super::PrRef;
use super::wire::{CommentKind, Detail, Reaction, ReactionContent};

pub const REACTION_ORDER: [ReactionContent; 8] = [
    ReactionContent::ThumbsUp,
    ReactionContent::ThumbsDown,
    ReactionContent::Laugh,
    ReactionContent::Hooray,
    ReactionContent::Confused,
    ReactionContent::Heart,
    ReactionContent::Rocket,
    ReactionContent::Eyes,
];

/// `canReactPullRequestComment`.
pub fn can_react_comment(detail: &Detail, kind: Option<CommentKind>) -> bool {
    detail.capabilities.reactions == Some(true)
        && !(detail.provider == "gitcafe" && kind == Some(CommentKind::Review))
}

pub fn reaction_emoji(content: ReactionContent) -> &'static str {
    match content {
        ReactionContent::ThumbsUp => "👍",
        ReactionContent::ThumbsDown => "👎",
        ReactionContent::Laugh => "😄",
        ReactionContent::Hooray => "🎉",
        ReactionContent::Confused => "😕",
        ReactionContent::Heart => "❤️",
        ReactionContent::Rocket => "🚀",
        ReactionContent::Eyes => "👀",
        ReactionContent::Unknown => "❓",
    }
}

pub fn reaction_name(content: ReactionContent) -> &'static str {
    match content {
        ReactionContent::ThumbsUp => "thumbs up",
        ReactionContent::ThumbsDown => "thumbs down",
        ReactionContent::Laugh => "laugh",
        ReactionContent::Hooray => "hooray",
        ReactionContent::Confused => "confused",
        ReactionContent::Heart => "heart",
        ReactionContent::Rocket => "rocket",
        ReactionContent::Eyes => "eyes",
        ReactionContent::Unknown => "unknown",
    }
}

const NAMED_ACTOR_LIMIT: usize = 3;

fn join_names(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
    }
}

fn count_remainder(count: u64, named: bool) -> String {
    if named {
        format!("{count} {}", if count == 1 { "other" } else { "others" })
    } else {
        format!("{count} {}", if count == 1 { "person" } else { "people" })
    }
}

/// `pullRequestReactionTooltip`.
pub fn reaction_tooltip(reaction: &Reaction) -> String {
    let viewer_has_room = (reaction.actors.len() as u64) < reaction.count;
    let mut names: Vec<String> = Vec::new();
    if reaction.viewer_has_reacted && viewer_has_room {
        names.push("You".to_owned());
    }
    names.extend(reaction.actors.iter().cloned());
    let limit = NAMED_ACTOR_LIMIT.min(reaction.count as usize);
    let shown: Vec<String> = names.into_iter().take(limit).collect();
    let others = reaction.count.saturating_sub(shown.len() as u64);
    let mut parts = shown.clone();
    if others > 0 {
        parts.push(count_remainder(others, !shown.is_empty()));
    }
    format!("{} reacted with {} emoji", join_names(&parts), reaction_name(reaction.content))
}

/// `pullRequestReactionsSignature`.
pub fn reactions_signature(reactions: &[Reaction]) -> String {
    reactions
        .iter()
        .map(|reaction| format!("{}:{}:{}", reaction.content.as_str(), reaction.count, u8::from(reaction.viewer_has_reacted)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `applyPendingPullRequestReactions`.
pub fn apply_pending_reactions(reactions: &[Reaction], pending: &HashMap<ReactionContent, bool>) -> Vec<Reaction> {
    if pending.is_empty() {
        return reactions.to_vec();
    }
    let mut by_content: HashMap<ReactionContent, Reaction> = reactions.iter().cloned().map(|r| (r.content, r)).collect();
    for (&content, &reacted) in pending {
        match by_content.get(&content).cloned() {
            None => {
                if reacted {
                    by_content.insert(content, Reaction { content, count: 1, actors: Vec::new(), viewer_has_reacted: true });
                }
            }
            Some(current) => {
                if current.viewer_has_reacted == reacted {
                    continue;
                }
                if reacted {
                    by_content.insert(content, Reaction { count: current.count + 1, viewer_has_reacted: true, ..current });
                } else if current.count <= 1 {
                    by_content.remove(&content);
                } else {
                    by_content.insert(content, Reaction { count: current.count - 1, viewer_has_reacted: false, ..current });
                }
            }
        }
    }
    REACTION_ORDER.iter().filter_map(|content| by_content.remove(content)).collect()
}

/// `PullRequestReactionInput`.
pub fn set_reaction_input(reference: &PrRef, subject_id: Option<&str>, content: ReactionContent, reacted: bool) -> Value {
    let mut payload = reference.payload();
    if let Some(subject_id) = subject_id {
        payload["subjectId"] = json!(subject_id);
    }
    payload["content"] = json!(content.as_str());
    payload["reacted"] = json!(reacted);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_formats_viewer_actors_and_others() {
        let one = Reaction { content: ReactionContent::ThumbsUp, count: 1, actors: vec![], viewer_has_reacted: true };
        assert_eq!(reaction_tooltip(&one), "You reacted with thumbs up emoji");
        let many = Reaction {
            content: ReactionContent::Rocket,
            count: 5,
            actors: vec!["alice".into(), "bob".into(), "carol".into()],
            viewer_has_reacted: true,
        };
        assert_eq!(reaction_tooltip(&many), "You, alice, bob, and 2 others reacted with rocket emoji");
        let anon = Reaction { content: ReactionContent::Heart, count: 2, actors: vec![], viewer_has_reacted: false };
        assert_eq!(reaction_tooltip(&anon), "2 people reacted with heart emoji");
    }

    #[test]
    fn pending_overlay_adds_increments_and_removes_reactions_in_canonical_order() {
        let existing = vec![
            Reaction { content: ReactionContent::Heart, count: 1, actors: vec![], viewer_has_reacted: true },
            Reaction { content: ReactionContent::ThumbsUp, count: 2, actors: vec!["alice".into()], viewer_has_reacted: false },
        ];
        let mut pending = HashMap::new();
        pending.insert(ReactionContent::Heart, false);
        pending.insert(ReactionContent::ThumbsUp, true);
        pending.insert(ReactionContent::Eyes, true);
        let applied = apply_pending_reactions(&existing, &pending);
        assert_eq!(applied.len(), 2);
        assert_eq!((applied[0].content, applied[0].count, applied[0].viewer_has_reacted), (ReactionContent::ThumbsUp, 3, true));
        assert_eq!((applied[1].content, applied[1].count, applied[1].viewer_has_reacted), (ReactionContent::Eyes, 1, true));
    }

    #[test]
    fn set_reaction_input_includes_optional_subject_id() {
        let reference = PrRef { project_id: "p".into(), host: Some("github.com".into()), repository: "a/b".into(), number: 9 };
        let on_pr = set_reaction_input(&reference, None, ReactionContent::ThumbsUp, true);
        assert!(on_pr.get("subjectId").is_none());
        assert_eq!((on_pr["content"].as_str(), on_pr["reacted"].as_bool()), (Some("thumbs-up"), Some(true)));
        let on_comment = set_reaction_input(&reference, Some("c1"), ReactionContent::Laugh, false);
        assert_eq!(on_comment["subjectId"].as_str(), Some("c1"));
    }
}
