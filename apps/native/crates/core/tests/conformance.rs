//! Replays the fixtures `apps/native/conformance/generate.ts` writes from the real TypeScript
//! reducers, controller logic, and server wire code. After every operation it serializes the
//! Rust state and compares every field the Rust types model against the TypeScript state.
//! Regenerate with `node apps/native/conformance/generate.ts`.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};
use t3_core::history::{HistoryStep, decode_history_page};
use t3_core::shell::{ShellState, ShellStreamItem};
use t3_core::thread::{ApplyOptions, DomainEvent, ThreadProjection, ThreadState, apply_event, decode_thread_stream_item};

/// One case per line.
fn fixture(name: &str) -> Vec<Value> {
    let path = format!("{}/tests/conformance/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let cases: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).expect("each line is a JSON case")).collect();
    assert!(cases.len() >= 5, "{path} has too few cases");
    cases
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
}

/// Rebuilds a state the generator split into scalars, objects and lists of table refs.
fn resolve(state: &Value, table: &[Value]) -> Value {
    if state.is_null() {
        return Value::Null;
    }
    let entry = |reference: &Value| reference.as_u64().and_then(|i| table.get(i as usize)).cloned().expect("table ref");
    let mut out = Map::new();
    for (key, value) in state["scalars"].as_object().into_iter().flatten() {
        out.insert(key.clone(), value.clone());
    }
    for (key, reference) in state["objects"].as_object().into_iter().flatten() {
        out.insert(key.clone(), entry(reference));
    }
    for (key, refs) in state["lists"].as_object().into_iter().flatten() {
        let rows = refs.as_array().into_iter().flatten().map(|element| match element {
            Value::Array(pair) => {
                let mut row = entry(&pair[1]);
                if let Value::Object(fields) = &mut row {
                    fields.insert("position".into(), pair[0].clone());
                }
                row
            }
            reference => entry(reference),
        });
        out.insert(key.clone(), Value::Array(rows.collect()));
    }
    Value::Object(out)
}

/// The expected value cut to the shape the Rust value serializes: object keys the Rust type
/// models (a missing key reads as null), arrays element by element. A field Rust does not
/// model is not compared; one it models but decodes wrong is.
fn shaped(expected: &Value, rust: &Value) -> Value {
    match (expected, rust) {
        (Value::Object(expected), Value::Object(rust)) => Value::Object(
            rust.iter().map(|(key, value)| (key.clone(), shaped(expected.get(key).unwrap_or(&Value::Null), value))).collect(),
        ),
        (Value::Array(expected), Value::Array(rust)) => Value::Array(
            expected.iter().enumerate().map(|(i, value)| rust.get(i).map_or_else(|| value.clone(), |r| shaped(value, r))).collect(),
        ),
        _ => expected.clone(),
    }
}

/// The path to the first difference, for a readable report.
fn first_difference(rust: &Value, typescript: &Value, path: &str) -> Option<String> {
    match (rust, typescript) {
        (Value::Object(r), Value::Object(t)) => r.keys().chain(t.keys()).find_map(|key| {
            first_difference(r.get(key).unwrap_or(&Value::Null), t.get(key).unwrap_or(&Value::Null), &format!("{path}.{key}"))
        }),
        (Value::Array(r), Value::Array(t)) if r.len() == t.len() => {
            r.iter().zip(t).enumerate().find_map(|(i, (r, t))| first_difference(r, t, &format!("{path}[{i}]")))
        }
        _ if rust == typescript => None,
        _ => Some(format!("{path}\n    rust:       {}\n    typescript: {}", clip(rust), clip(typescript))),
    }
}

fn clip(value: &Value) -> String {
    let text = value.to_string();
    match text.char_indices().nth(400) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}

/// Records, per path, whether TypeScript ever sent a key the Rust side serializes. A key it
/// never sends is either unexercised or misnamed in Rust (always `null` against an absent key),
/// and both would let a comparison pass vacuously.
fn coverage(rust: &Value, expected: &Value, path: &str, seen: &mut BTreeMap<String, bool>) {
    match (rust, expected) {
        (Value::Object(rust), expected) => {
            for (key, value) in rust {
                let sent = expected.get(key);
                let pattern = format!("{path}.{key}");
                *seen.entry(pattern.clone()).or_default() |= sent.is_some();
                coverage(value, sent.unwrap_or(&Value::Null), &pattern, seen);
            }
        }
        (Value::Array(rust), Value::Array(expected)) => {
            for (rust, expected) in rust.iter().zip(expected) {
                coverage(rust, expected, &format!("{path}[]"), seen);
            }
        }
        _ => {}
    }
}

/// Collects every divergence so one run reports all of them.
#[derive(Default)]
struct Report {
    divergences: Vec<String>,
    compared: usize,
    seen: BTreeMap<String, bool>,
}

impl Report {
    fn value(&mut self, at: &str, rust: &impl Serialize, expected: &Value) {
        self.compared += 1;
        let rust = serde_json::to_value(rust).expect("state serializes");
        coverage(&rust, expected, "", &mut self.seen);
        let expected = shaped(expected, &rust);
        if let Some(difference) = first_difference(&rust, &expected, "") {
            self.divergences.push(format!("{at}: {difference}"));
        }
    }

    fn check<T: PartialEq + std::fmt::Debug>(&mut self, at: &str, rust: T, typescript: T) {
        if rust != typescript {
            self.divergences.push(format!("{at}\n    rust:       {rust:?}\n    typescript: {typescript:?}"));
        }
    }

    fn finish(self, family: &str, minimum: usize) {
        assert!(self.divergences.is_empty(), "{} {family} divergence(s):\n{}", self.divergences.len(), self.divergences.join("\n\n"));
        assert!(self.compared >= minimum, "{family}: only {} states compared", self.compared);
        let unsent: Vec<&String> = self.seen.iter().filter(|(_, sent)| !**sent).map(|(path, _)| path).collect();
        assert!(
            unsent.is_empty(),
            "{family}: no fixture sends {unsent:?}. Set them in a scenario (conformance/builders.ts `rich`), or fix the Rust field name."
        );
    }
}

fn projection_view(report: &mut Report, at: &str, rust: Option<&ThreadProjection>, expected: &Value) {
    match (rust, expected.is_null()) {
        (Some(projection), false) => report.value(at, projection, expected),
        (None, true) => {}
        (rust, _) => report.check(&format!("{at}: projection present"), rust.is_some(), !expected.is_null()),
    }
}

fn has_rows(state: &Value) -> bool {
    !state.is_null() && !list(&state["lists"], "visibleTurnItems").is_empty()
}

#[test]
fn reducer_matches_typescript() {
    let mut report = Report::default();
    for case in fixture("reducer.jsonl") {
        let name = text(&case, "name").unwrap_or_default();
        let (table, states, steps) = (list(&case, "table"), list(&case, "states"), list(&case, "steps"));
        assert!(!steps.is_empty(), "{name}: no steps");
        assert_eq!(states.len(), steps.len() + 1, "{name}: one state per step plus the decoded initial one");
        let mut projection: ThreadProjection =
            serde_json::from_value(case["initial"].clone()).unwrap_or_else(|e| panic!("{name}: initial: {e}"));
        report.value(&format!("{name} initial"), &projection, &resolve(&states[0], table));
        for (index, step) in steps.iter().enumerate() {
            let event: DomainEvent =
                serde_json::from_value(step["event"].clone()).unwrap_or_else(|e| panic!("{name}: event: {e}"));
            let options = match step.get("options").filter(|o| !o.is_null()) {
                Some(options) => ApplyOptions {
                    partial_timeline: options["partialTimeline"].as_bool() == Some(true),
                    latest_local_turn_ordinal: options["latestLocalTurnOrdinal"].as_u64(),
                },
                None => ApplyOptions::default(),
            };
            apply_event(&mut projection, event, options);
            report.value(&format!("{name} step {index}"), &projection, &resolve(&states[index + 1], table));
        }
    }
    report.finish("reducer", 400);
}

fn step_name(step: HistoryStep) -> String {
    match step {
        HistoryStep::Continue(cursor) => format!("continue:{cursor}"),
        HistoryStep::Done => "done".into(),
        HistoryStep::Stale => "stale".into(),
        HistoryStep::Failed(message) => format!("failed:{message}"),
    }
}

#[test]
fn thread_stream_and_history_match_typescript() {
    let mut report = Report::default();
    for case in fixture("thread.jsonl") {
        let name = text(&case, "name").unwrap_or_default();
        let (table, ops, states) = (list(&case, "table"), list(&case, "ops"), list(&case, "states"));
        assert!(!ops.is_empty() && ops.len() == states.len(), "{name}: one state per operation");
        assert!(states.iter().any(|s| has_rows(&s["projection"])), "{name}: never shows a row");
        let mut state = ThreadState::default();
        for (index, (op, expected)) in ops.iter().zip(states).enumerate() {
            let at = format!("{name} op {index}");
            if let Some(item) = op.get("stream") {
                state.apply(decode_thread_stream_item(item.clone()).unwrap_or_else(|e| panic!("{at}: {e}")));
            } else if let Some(cursor) = op.get("begin") {
                report.check(&format!("{at} begin"), state.begin_history_load(), cursor.as_str().map(str::to_owned));
            } else if let Some(page) = op.get("page") {
                let page = decode_history_page(page.clone()).unwrap_or_else(|e| panic!("{at}: {e}"));
                let step = state.apply_history_page(&text(op, "requestCursor").unwrap_or_default(), page);
                report.check(&format!("{at} page"), step_name(step), text(op, "step").unwrap_or_default());
            } else if let Some(message) = text(op, "fail") {
                let step = state.fail_history_load(&text(op, "requestCursor").unwrap_or_default(), message);
                report.check(&format!("{at} fail"), step_name(step), text(op, "step").unwrap_or_default());
            } else {
                panic!("{at}: unknown operation");
            }
            projection_view(&mut report, &at, state.projection.as_ref(), &resolve(&expected["projection"], table));
            report.check(&format!("{at} sequence"), state.sequence, expected["sequence"].as_u64());
            report.check(&format!("{at} deleted"), state.deleted, expected["deleted"].as_bool() == Some(true));
            let history = &expected["history"];
            let meta = &state.history;
            report.check(
                &format!("{at} history"),
                (meta.history_cursor.clone(), meta.has_more_history, meta.loading, meta.error.clone(), meta.expanded, meta.latest_local_turn_ordinal),
                (
                    text(history, "historyCursor"),
                    history["hasMoreHistory"].as_bool() == Some(true),
                    history["loading"].as_bool() == Some(true),
                    text(history, "error"),
                    history["expanded"].as_bool() == Some(true),
                    history["latestLocalTurnOrdinal"].as_u64(),
                ),
            );
        }
    }
    report.finish("thread", 50);
}

#[test]
fn shell_matches_typescript() {
    let mut report = Report::default();
    for case in fixture("shell.jsonl") {
        let name = text(&case, "name").unwrap_or_default();
        let (table, items, states) = (list(&case, "table"), list(&case, "items"), list(&case, "states"));
        assert!(!items.is_empty() && items.len() == states.len(), "{name}: one state per item");
        assert!(states.iter().any(|s| !s.is_null()), "{name}: never holds a snapshot");
        let mut state = ShellState::default();
        for (index, (item, expected)) in items.iter().zip(states).enumerate() {
            let at = format!("{name} item {index}");
            let item: ShellStreamItem = serde_json::from_value(item.clone()).unwrap_or_else(|e| panic!("{at}: {e}"));
            state.apply(item);
            match (&state.snapshot, expected.is_null()) {
                (Some(snapshot), false) => report.value(&at, snapshot, &resolve(expected, table)),
                (None, true) => {}
                (rust, _) => report.check(&format!("{at}: snapshot present"), rust.is_some(), !expected.is_null()),
            }
        }
    }
    report.finish("shell", 40);
}
