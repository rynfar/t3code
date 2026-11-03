//! Preserve per-theme edits across asynchronous settings commits.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

fn indexed(value: Option<&Value>) -> BTreeMap<String, Value> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|theme| Some((theme["id"].as_str()?.to_owned(), theme.clone())))
        .collect()
}
fn apply(
    current: Option<&Value>,
    changes: BTreeMap<String, Option<Value>>,
    order: &Value,
) -> Value {
    let mut values = Vec::new();
    let mut seen = BTreeSet::new();
    for value in current.and_then(Value::as_array).into_iter().flatten() {
        let Some(id) = value["id"].as_str() else {
            values.push(value.clone());
            continue;
        };
        let first = seen.insert(id.to_owned());
        match changes.get(id) {
            Some(Some(next)) if first => values.push(next.clone()),
            Some(_) => {}
            None => values.push(value.clone()),
        }
    }
    for value in order.as_array().into_iter().flatten() {
        if let Some(id) = value["id"].as_str()
            && seen.insert(id.to_owned())
            && let Some(Some(value)) = changes.get(id)
        {
            values.push(value.clone());
        }
    }
    Value::Array(values)
}

/// Replays only the themes changed by an edit over the library at writer time.
pub fn rebase_library(before: Option<&Value>, after: &Value, current: Option<&Value>) -> Value {
    let order = after;
    let before = indexed(before);
    let after = indexed(Some(after));
    let changes = before
        .keys()
        .chain(after.keys())
        .filter(|id| before.get(*id) != after.get(*id))
        .map(|id| (id.clone(), after.get(id).cloned()))
        .collect();
    apply(current, changes, order)
}

#[derive(Default)]
pub struct LibraryVersions(BTreeMap<String, u64>);
impl LibraryVersions {
    pub fn edited(&mut self, before: Option<&Value>, after: &Value, sequence: u64) {
        let before = indexed(before);
        let after = indexed(Some(after));
        for id in before.keys().chain(after.keys()) {
            if before.get(id) != after.get(id) {
                self.0.insert(id.clone(), sequence);
            }
        }
    }
    pub fn committed(
        &mut self,
        current: Option<&Value>,
        committed: &Value,
        sequence: u64,
    ) -> Value {
        let before = indexed(current);
        let after = indexed(Some(committed));
        let mut changes = BTreeMap::new();
        for id in before.keys().chain(after.keys()) {
            if self.0.get(id).copied().unwrap_or_default() <= sequence {
                changes.insert(id.clone(), after.get(id).cloned());
                self.0.insert(id.clone(), sequence);
            }
        }
        apply(current, changes, committed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn rebasing_preserves_unedited_themes_and_receipts_preserve_newer_edits_and_removals() {
        let old = json!([{"id":"keep"},{"id":"remove"}]);
        let imported = json!([{"id":"keep"},{"id":"remove"},{"id":"imported"}]);
        let edited = json!([{"id":"keep"},{"id":"other"}]);
        assert_eq!(
            rebase_library(Some(&old), &edited, Some(&imported)),
            json!([{"id":"keep"},{"id":"imported"},{"id":"other"}])
        );
        let ordered = json!([{"id":"keep"},{"id":"remove"},{"id":"zulu"},{"id":"alpha"}]);
        assert_eq!(rebase_library(Some(&old), &ordered, Some(&old)), ordered);
        assert_eq!(
            LibraryVersions::default().committed(Some(&old), &ordered, 1),
            ordered
        );
        let mut versions = LibraryVersions::default();
        versions.edited(Some(&old), &edited, 2);
        assert_eq!(
            versions.committed(Some(&edited), &imported, 1),
            json!([{"id":"keep"},{"id":"other"},{"id":"imported"}])
        );
        // A removed theme stays removed even if the delayed receipt predates its first UI publication.
        let empty = json!([]);
        versions.edited(Some(&empty), &imported, 3);
        versions.edited(Some(&imported), &empty, 4);
        assert_eq!(versions.committed(Some(&empty), &imported, 1), empty);
    }
}
