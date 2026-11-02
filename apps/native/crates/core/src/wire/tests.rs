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
