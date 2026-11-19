//! Branch comparison-base choices and filtering for the diff panel
//! (`apps/web/src/lib/baseRefChoices.ts` and `apps/web/src/components/DiffPanel.tsx`).

use serde_json::{Map, Value, json};

use crate::vcs::{ListRefsResult, VcsRef};

pub const AUTOMATIC_BASE_REF: &str = "__automatic_base_ref__";
pub const BASE_REF_LIST_LIMIT: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseRefKind {
    Local,
    Remote,
}

impl BaseRefKind {
    pub fn wire(self) -> &'static str {
        match self {
            BaseRefKind::Local => "local",
            BaseRefKind::Remote => "remote",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseRefChoice {
    pub id: String,
    pub label: String,
    pub local: Option<VcsRef>,
    pub remote: Option<VcsRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseRefItem {
    Automatic,
    Choice(Box<BaseRefChoice>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseRefHighlightKey {
    Automatic,
    Choice(String),
}

impl BaseRefItem {
    pub fn key(&self) -> BaseRefHighlightKey {
        match self {
            BaseRefItem::Automatic => BaseRefHighlightKey::Automatic,
            BaseRefItem::Choice(choice) => BaseRefHighlightKey::Choice(choice.id.clone()),
        }
    }

    pub fn selection_value(&self, selected_base_ref: Option<&str>) -> Option<String> {
        match self {
            BaseRefItem::Automatic => None,
            BaseRefItem::Choice(choice) => {
                Some(value_for_base_ref_choice(choice, selected_base_ref).to_owned())
            }
        }
    }
}

fn remote_branch_name(ref_item: &VcsRef) -> &str {
    if let Some(remote_name) = ref_item.remote_name.as_deref().filter(|name| !name.is_empty())
        && let Some(stripped) = ref_item
            .name
            .strip_prefix(remote_name)
            .and_then(|rest| rest.strip_prefix('/'))
    {
        return stripped;
    }
    &ref_item.name
}

/// Pairs local branches with matching remote branches (`origin` preferred), then appends
/// unmatched remote branches (`buildBaseRefChoices`).
pub fn build_base_ref_choices(
    local_refs: &[VcsRef],
    remote_refs: &[VcsRef],
) -> Vec<BaseRefChoice> {
    let mut used_remote = vec![false; remote_refs.len()];
    let mut choices = Vec::with_capacity(local_refs.len() + remote_refs.len());

    for local in local_refs {
        let mut matched_idx = None;
        for (idx, remote) in remote_refs.iter().enumerate() {
            if used_remote[idx] || remote_branch_name(remote) != local.name {
                continue;
            }
            if remote.remote_name.as_deref() == Some("origin") {
                matched_idx = Some(idx);
                break;
            }
            if matched_idx.is_none() {
                matched_idx = Some(idx);
            }
        }
        let remote = matched_idx.map(|idx| {
            used_remote[idx] = true;
            remote_refs[idx].clone()
        });
        choices.push(BaseRefChoice {
            id: format!("local:{}", local.name),
            label: local.name.clone(),
            local: Some(local.clone()),
            remote,
        });
    }

    for (idx, remote) in remote_refs.iter().enumerate() {
        if used_remote[idx] {
            continue;
        }
        choices.push(BaseRefChoice {
            id: format!("remote:{}", remote.name),
            label: remote.name.clone(),
            local: None,
            remote: Some(remote.clone()),
        });
    }

    choices
}

/// Builds comparison-base choices excluding the current `comparison_head_ref` from local refs
/// (`DiffPanel.tsx`).
pub fn build_diff_base_ref_choices(
    local_refs: &[VcsRef],
    remote_refs: &[VcsRef],
    comparison_head_ref: Option<&str>,
) -> Vec<BaseRefChoice> {
    let filtered_local: Vec<VcsRef> = match comparison_head_ref {
        Some(head) => local_refs.iter().filter(|item| item.name != head).cloned().collect(),
        None => local_refs.to_vec(),
    };
    build_base_ref_choices(&filtered_local, remote_refs)
}

/// Case-insensitive substring filter over choice labels and underlying local/remote ref names
/// (`filterBaseRefChoices`).
pub fn filter_base_ref_choices(choices: &[BaseRefChoice], query: &str) -> Vec<BaseRefChoice> {
    let normalized = query.trim().to_lowercase();
    if normalized.is_empty() {
        return choices.to_vec();
    }
    choices
        .iter()
        .filter(|choice| {
            choice.label.to_lowercase().contains(&normalized)
                || choice
                    .local
                    .as_ref()
                    .is_some_and(|item| item.name.to_lowercase().contains(&normalized))
                || choice
                    .remote
                    .as_ref()
                    .is_some_and(|item| item.name.to_lowercase().contains(&normalized))
        })
        .cloned()
        .collect()
}

/// Resolves the active ref value for a choice: keeps the remote ref when currently selected,
/// otherwise prefers local over remote (`valueForBaseRefChoice`).
pub fn value_for_base_ref_choice<'a>(
    choice: &'a BaseRefChoice,
    selected_base_ref: Option<&str>,
) -> &'a str {
    if let Some(selected) = selected_base_ref
        && choice
            .remote
            .as_ref()
            .is_some_and(|remote| remote.name == selected)
    {
        return choice
            .remote
            .as_ref()
            .map_or(choice.id.as_str(), |remote| remote.name.as_str());
    }
    choice
        .local
        .as_ref()
        .map(|local| local.name.as_str())
        .or_else(|| choice.remote.as_ref().map(|remote| remote.name.as_str()))
        .unwrap_or(choice.id.as_str())
}

pub fn include_automatic_choice(query: &str) -> bool {
    query.trim().is_empty()
}

pub fn base_ref_items(choices: &[BaseRefChoice], query: &str) -> Vec<BaseRefItem> {
    let filtered = filter_base_ref_choices(choices, query);
    let mut items = Vec::with_capacity(filtered.len() + usize::from(include_automatic_choice(query)));
    if include_automatic_choice(query) {
        items.push(BaseRefItem::Automatic);
    }
    items.extend(filtered.into_iter().map(Box::new).map(BaseRefItem::Choice));
    items
}

/// Request payload for `vcs.listRefs` in `DiffPanel.tsx`.
pub fn list_base_refs_request(cwd: &str, ref_kind: BaseRefKind, query: Option<&str>) -> Value {
    let mut payload = Map::new();
    payload.insert("cwd".into(), json!(cwd));
    payload.insert("includeMatchingRemoteRefs".into(), json!(true));
    payload.insert("refKind".into(), json!(ref_kind.wire()));
    if let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) {
        payload.insert("query".into(), json!(query.chars().take(256).collect::<String>()));
    }
    payload.insert("limit".into(), json!(BASE_REF_LIST_LIMIT));
    Value::Object(payload)
}

pub fn decode_list_refs_result(value: Value) -> Result<ListRefsResult, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_ref(name: &str) -> VcsRef {
        VcsRef {
            name: name.to_owned(),
            is_remote: Some(false),
            remote_name: None,
            current: false,
            is_default: name == "main",
            worktree_path: None,
        }
    }

    fn remote_ref(name: &str, remote_name: Option<&str>) -> VcsRef {
        VcsRef {
            name: name.to_owned(),
            is_remote: Some(true),
            remote_name: remote_name.map(str::to_owned),
            current: false,
            is_default: false,
            worktree_path: None,
        }
    }

    #[test]
    fn builds_paired_and_remote_only_choices_preferring_origin() {
        let local = vec![local_ref("main"), local_ref("feature"), local_ref("wip")];
        let remote = vec![
            remote_ref("upstream/main", Some("upstream")),
            remote_ref("origin/main", Some("origin")),
            remote_ref("upstream/feature", Some("upstream")),
            remote_ref("origin/release", Some("origin")),
        ];

        let choices = build_base_ref_choices(&local, &remote);
        assert_eq!(choices.len(), 5);

        assert_eq!(choices[0].id, "local:main");
        assert_eq!(choices[0].label, "main");
        assert_eq!(
            choices[0].remote.as_ref().map(|item| item.name.as_str()),
            Some("origin/main")
        );

        assert_eq!(choices[1].id, "local:feature");
        assert_eq!(
            choices[1].remote.as_ref().map(|item| item.name.as_str()),
            Some("upstream/feature")
        );

        assert_eq!(choices[2].id, "local:wip");
        assert_eq!(choices[2].remote, None);

        assert_eq!(choices[3].id, "remote:upstream/main");
        assert_eq!(choices[3].local, None);
        assert_eq!(choices[3].label, "upstream/main");

        assert_eq!(choices[4].id, "remote:origin/release");
        assert_eq!(choices[4].local, None);
        assert_eq!(choices[4].label, "origin/release");
    }

    #[test]
    fn excludes_comparison_head_ref_from_local_choices() {
        let local = vec![local_ref("dev"), local_ref("main")];
        let remote = vec![remote_ref("origin/dev", Some("origin")), remote_ref("origin/main", Some("origin"))];

        let choices = build_diff_base_ref_choices(&local, &remote, Some("dev"));
        assert_eq!(
            choices.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["local:main", "remote:origin/dev"]
        );
    }

    #[test]
    fn filters_choices_across_label_local_and_remote_names() {
        let choices = build_base_ref_choices(
            &[local_ref("main"), local_ref("topic")],
            &[remote_ref("origin/main", Some("origin")), remote_ref("origin/release", Some("origin"))],
        );

        assert_eq!(filter_base_ref_choices(&choices, "   ").len(), 3);

        let origin_matches = filter_base_ref_choices(&choices, "ORIGIN/");
        assert_eq!(
            origin_matches.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["local:main", "remote:origin/release"]
        );

        let topic_matches = filter_base_ref_choices(&choices, "pic");
        assert_eq!(
            topic_matches.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["local:topic"]
        );
    }

    #[test]
    fn resolves_choice_value_and_automatic_visibility() {
        let paired = BaseRefChoice {
            id: "local:main".into(),
            label: "main".into(),
            local: Some(local_ref("main")),
            remote: Some(remote_ref("origin/main", Some("origin"))),
        };
        let remote_only = BaseRefChoice {
            id: "remote:origin/orphan".into(),
            label: "origin/orphan".into(),
            local: None,
            remote: Some(remote_ref("origin/orphan", Some("origin"))),
        };

        assert_eq!(value_for_base_ref_choice(&paired, None), "main");
        assert_eq!(value_for_base_ref_choice(&paired, Some("main")), "main");
        assert_eq!(value_for_base_ref_choice(&paired, Some("origin/main")), "origin/main");
        assert_eq!(value_for_base_ref_choice(&remote_only, None), "origin/orphan");

        let empty_query_items = base_ref_items(std::slice::from_ref(&paired), "  ");
        assert_eq!(empty_query_items.len(), 2);
        assert_eq!(empty_query_items[0], BaseRefItem::Automatic);
        assert_eq!(empty_query_items[0].selection_value(None), None);
        assert_eq!(
            empty_query_items[1].selection_value(Some("origin/main")),
            Some("origin/main".into())
        );

        let search_items = base_ref_items(&[paired], "main");
        assert_eq!(search_items.len(), 1);
        assert!(matches!(search_items[0], BaseRefItem::Choice(_)));
    }

    #[test]
    fn formats_list_refs_requests_and_decodes_wire_response() {
        assert_eq!(
            list_base_refs_request("/repo", BaseRefKind::Local, Some("  ")),
            json!({
                "cwd": "/repo",
                "includeMatchingRemoteRefs": true,
                "refKind": "local",
                "limit": 100
            })
        );
        assert_eq!(
            list_base_refs_request("/repo", BaseRefKind::Remote, Some("  origin/main  ")),
            json!({
                "cwd": "/repo",
                "includeMatchingRemoteRefs": true,
                "refKind": "remote",
                "query": "origin/main",
                "limit": 100
            })
        );

        let long_query = "a".repeat(300);
        let payload = list_base_refs_request("/repo", BaseRefKind::Local, Some(&long_query));
        assert_eq!(payload["query"].as_str().map(str::len), Some(256));

        let decoded = decode_list_refs_result(json!({
            "isAtRepositoryRoot": true,
            "isRepo": true,
            "hasPrimaryRemote": true,
            "nextCursor": null,
            "totalCount": 1,
            "refs": [{
                "name": "origin/main",
                "isRemote": true,
                "remoteName": "origin",
                "current": false,
                "isDefault": false,
                "worktreePath": null,
                "futureField": "ignored"
            }]
        }))
        .expect("decodes ListRefsResult");
        assert!(decoded.is_repo);
        assert_eq!(decoded.refs.len(), 1);
        assert_eq!(decoded.refs[0].name, "origin/main");
        assert_eq!(decoded.refs[0].remote_name.as_deref(), Some("origin"));
    }
}
