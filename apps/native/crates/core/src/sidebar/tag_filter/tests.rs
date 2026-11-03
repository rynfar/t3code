use super::*;
use serde_json::{Value, json};
fn wire_thread(id: &str, extra: Value) -> Value {
    let mut value = json!({
        "id": id, "projectId": "p1", "title": id, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
        "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "latestRunId": null, "activeRunId": null, "status": "idle", "pendingRuntimeRequest": null,
        "latestVisibleMessage": null, "latestUserMessageAt": null, "hasActionableProposedPlan": false,
        "itemCount": 0, "visibleItemCount": 0, "createdAt": "2026-04-01T00:00:00.000Z",
        "updatedAt": "2026-04-01T00:00:00.000Z", "archivedAt": null, "settledOverride": null,
        "settledAt": null, "deletedAt": null
    });
    if let (Some(target), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    value
}

fn thread(id: &str, tags: &[&str], parent: Option<(&str, &str)>, delegated: Option<(&str, &str)>) -> ThreadShell {
    let mut value = wire_thread(id, json!({"tags": tags}));
    if let Some((id, relationship)) = parent {
        value["lineage"]["parentThreadId"] = json!(id);
        value["lineage"]["relationshipToParent"] = json!(relationship);
    }
    if let Some((env, id)) = delegated {
        value["delegatedFrom"] = json!({"environmentId":env,"threadId":id,"title":"parent"});
    }
    serde_json::from_value(value).unwrap()
}
fn set(keys: &[(&str, &str)]) -> HashSet<ThreadKey> {
    keys.iter().map(|(env, id)| ThreadKey::new(env, id)).collect()
}
#[test]
fn parent_visibility_and_linked_rows_match_ts_cases() {
    let local = [
        thread("private", &["private"], None, None),
        thread("private-subagent", &[], Some(("private", "subagent")), None),
        thread("private-fork", &[], Some(("private", "fork")), None),
        thread("plain", &[], None, None),
        thread("plain-private-side-chat", &["private"], Some(("plain", "subagent")), None),
    ];
    let linked = [thread("delegated-from-private", &[], None, Some(("local", "private"))), thread("linked-client", &["client"], None, None)];
    let local_refs = local.iter().collect::<Vec<_>>();
    let linked_refs = linked.iter().collect::<Vec<_>>();
    let private = vec!["private".into()];
    let client = vec!["client".into(), "private".into()];
    let hidden = |lt: Option<&[String]>, rt: Option<&[String]>| {
        hidden_thread_keys(
            &[
                Environment { environment_id: "local", threads: &local_refs, hidden_tags: lt },
                Environment { environment_id: "linked", threads: &linked_refs, hidden_tags: rt },
            ],
            &HashSet::new(),
            &HashSet::new(),
            lt.is_some() || rt.is_some(),
        )
    };
    assert!(hidden(None, None).is_empty());
    let hidden_private = hidden(Some(&private), None);
    assert_eq!(hidden_private, set(&[("local", "private"), ("local", "private-subagent"), ("linked", "delegated-from-private")]));
    assert_eq!(hidden(None, Some(&client)), set(&[("linked", "linked-client")]));
    let archived = [Environment { environment_id: "linked", threads: &linked_refs[..1], hidden_tags: None }];
    let parent = set(&[("local", "private")]);
    assert_eq!(hidden_thread_keys(&archived, &parent, &HashSet::new(), false), set(&[("linked", "delegated-from-private")]));
    assert_eq!(hidden_thread_keys(&archived, &parent, &parent, false), set(&[("linked", "delegated-from-private")]));
    let rows = ["private-subagent", "plain-private-side-chat", "off-shell"];
    let loaded = local.iter().map(|t| ThreadKey::new("local", &t.id)).collect();
    assert_eq!(visible_linked_threads(&rows, "local", |id| *id, &loaded, &hidden_private, true), vec![&"plain-private-side-chat"]);
    assert_eq!(visible_linked_threads(&rows, "local", |id| *id, &loaded, &HashSet::new(), true), vec![&"private-subagent", &"plain-private-side-chat"]);
    assert_eq!(visible_linked_threads(&rows, "local", |id| *id, &loaded, &HashSet::new(), false), rows.iter().collect::<Vec<_>>());
}
#[test]
fn missing_parents_and_cycles_match_ts_cases() {
    let orphan = thread("orphan-task", &[], None, Some(("local", "archived")));
    let refs = [&orphan];
    let tags = vec!["private".into()];
    for (lt, rt) in [(Some(tags.as_slice()), None), (None, Some(tags.as_slice())), (None, None)] {
        let hiding = lt.is_some() || rt.is_some();
        let environments =
            [Environment { environment_id: "local", threads: &[], hidden_tags: lt }, Environment { environment_id: "linked", threads: &refs, hidden_tags: rt }];
        assert_eq!(hidden_thread_keys(&environments, &HashSet::new(), &HashSet::new(), hiding).contains(&ThreadKey::new("linked", "orphan-task")), hiding);
        assert!(hidden_thread_keys(&environments, &HashSet::new(), &set(&[("local", "archived")]), hiding).is_empty());
    }
    let a = thread("a", &["private"], Some(("b", "subagent")), None);
    let b = thread("b", &[], Some(("a", "subagent")), None);
    assert!(
        hidden_thread_keys(&[Environment { environment_id: "local", threads: &[&a, &b], hidden_tags: Some(&tags) }], &HashSet::new(), &HashSet::new(), true)
            .is_empty()
    );
}
#[test]
fn tags_and_settings_decode_forward_compatibly() {
    for tags in [json!(["private", "Future tag!?", "PRIVATE"]), json!(["private", 42]), json!(null), json!({"future":true})] {
        let shell: ThreadShell = serde_json::from_value(wire_thread("t", json!({"tags":tags,"futureField":true}))).unwrap();
        let expected: Vec<String> = serde_json::from_value(tags).unwrap_or_default();
        assert_eq!(shell.tags, expected);
    }
    let mut settings: crate::server_config::ServerSettings =
        serde_json::from_value(json!({"hiddenThreadTags":[" private ",42,"  ","PRIVATE"],"hideTaggedThreads":true})).unwrap();
    assert_eq!(settings.hidden_tags(), Some(vec!["private".into(), "PRIVATE".into()]));
    settings.hide_tagged_threads = false;
    assert_eq!(settings.hidden_tags(), None);
    settings.hide_tagged_threads = true;
    settings.hidden_thread_tags = vec![" ".into()];
    assert_eq!(settings.hidden_tags(), None);
}
