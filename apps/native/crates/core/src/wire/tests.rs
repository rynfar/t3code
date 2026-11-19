use serde_json::json;

use super::*;

#[test]
fn id_table_expands_id_keys_on_records_and_their_items_only() {
    let mut raw = json!({
        "ids": ["thread-1", "run-1", "item-1", "node-a", "node-b"],
        "kind": "snapshot",
        "projection": {
            "thread": {"id": 0, "lineage": {"rootThreadId": 0}, "ordinal": 3},
            "runs": [{"id": 1, "threadId": 0, "childNodeIds": [3, 4, "literal"], "count": 2}],
            "visibleTurnItems": [{"sourceItemId": 2, "item": {"id": 2, "runId": 1, "nested": {"nodeId": 3}}}],
            "missing": [{"id": 9}]
        }
    });
    let ids = take_id_table(&mut raw).unwrap();
    let Some(Value::Object(projection)) = raw.get_mut("projection") else { panic!() };
    expand_record_ids(projection, &ids);
    assert_eq!(raw.get("ids"), None);
    assert_eq!(
        raw["projection"],
        json!({
            "thread": {"id": "thread-1", "lineage": {"rootThreadId": 0}, "ordinal": 3},
            "runs": [{"id": "run-1", "threadId": "thread-1", "childNodeIds": ["node-a", "node-b", "literal"], "count": 2}],
            "visibleTurnItems": [{"sourceItemId": "item-1", "item": {"id": "item-1", "runId": "run-1", "nested": {"nodeId": 3}}}],
            "missing": [{"id": null}]
        })
    );
    assert_eq!(take_id_table(&mut json!({"ids": "not a table"})), None);
}

#[test]
fn id_table_preserves_thread_issue_links_on_thread_issue_synced_payload() {
    let mut raw = json!({
        "ids": ["thr-7", "proj-1"],
        "type": "thread.issue-synced",
        "payload": {
            "id": 0,
            "projectId": 1,
            "issues": [{
                "tracker": "github",
                "host": "github.com",
                "scope": "acme/web",
                "key": "42",
                "projectId": "proj-1",
                "url": "https://github.com/acme/web/issues/42",
                "source": "manual",
                "linkedAt": "2026-01-01T00:00:00Z",
                "displayKey": "#42",
                "title": "Crash on empty input"
            }]
        }
    });
    let ids = take_id_table(&mut raw).unwrap();
    let Some(obj) = raw.as_object_mut() else { panic!() };
    expand_record_ids(obj, &ids);
    assert_eq!(raw["payload"]["id"], "thr-7");
    let issues: Vec<crate::issues::wire::ThreadIssueLink> =
        serde_json::from_value(raw["payload"]["issues"].clone()).unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].display_key, "#42");
    assert_eq!(issues[0].project_id, "proj-1");
}
