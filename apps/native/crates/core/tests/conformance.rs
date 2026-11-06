//! Replays the fixtures `apps/native/conformance/generate.ts` writes from the real TypeScript
//! reducers, controller logic, and server wire code. After every operation it serializes the
//! Rust state and compares every field the Rust types model against the TypeScript state.
//! Regenerate with `node apps/native/conformance/generate.ts`.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use serde_json::{Map, Value};
use t3_core::command_output::{CommandOutputFrame, CommandOutputView};
use t3_core::families::{FamilyRead, rows_missing_from_shell};
use t3_core::history::{HistoryStep, decode_history_page};
use t3_core::sidebar::older_settled::{
    Cursor, OlderSettled, PageStatus, SettledPages, count_key, is_listed_settled, unloaded_count,
};
use t3_core::sidebar::time::{format_iso, parse_iso};
use t3_core::shell::{ShellState, ShellStreamItem, ThreadShell};
use t3_core::notifications::{NotificationGate, NotificationKind, Observation};
use t3_core::terminal_output::{DARK_PALETTE, LIGHT_PALETTE, MAX_SPANS, Rgb, SpanStyle, TerminalOutputState, terminal_output_spans};
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

fn shells(value: &Value) -> Vec<ThreadShell> {
    serde_json::from_value(value.clone()).expect("thread shells decode")
}

fn removals(value: &Value) -> HashMap<String, u64> {
    list(value, "removedAt").iter().filter_map(|pair| Some((pair[0].as_str()?.to_owned(), pair[1].as_u64()?))).collect()
}

fn cursor(value: &Value) -> Option<Cursor> {
    Some(Cursor { settled_at: parse_iso(value.get("settledAt")?.as_str()?)?, thread_id: text(value, "threadId") })
}

fn cursor_json(cursor: Option<&Cursor>) -> Value {
    cursor.map_or(Value::Null, |cursor| serde_json::json!({"settledAt": format_iso(cursor.settled_at), "threadId": cursor.thread_id}))
}

#[test]
fn shelf_matches_typescript() {
    let mut report = Report::default();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for case in fixture("shelf.jsonl") {
        let name = text(&case, "name").unwrap_or_default();
        let kind = text(&case, "kind").unwrap_or_default();
        *kinds.entry(kind.clone()).or_default() += 1;
        let before = text(&case, "before").and_then(|before| parse_iso(&before));
        match kind.as_str() {
            "countKey" => {
                let key = count_key(&shells(&case["threads"]), before.expect("cut"), &removals(&case));
                report.check(&name, key, text(&case, "expected").unwrap_or_default());
            }
            "listed" => {
                for (index, check) in list(&case, "checks").iter().enumerate() {
                    let thread: ThreadShell = serde_json::from_value(check["thread"].clone()).expect("thread decodes");
                    let settled_at = thread.settled_at.as_deref().and_then(parse_iso);
                    let listed = is_listed_settled(&thread.id, settled_at, cursor(&check["cursor"]).as_ref());
                    report.check(&format!("{name} check {index}"), listed, check["expected"].as_bool() == Some(true));
                }
            }
            "unloaded" => {
                let threads = shells(&case["threads"]);
                let rows: Vec<&ThreadShell> = threads.iter().collect();
                let counts = list(&case, "counts").iter().filter_map(|pair| Some((pair[0].as_str()?.to_owned(), pair[1].as_u64()?))).collect();
                let older = OlderSettled { before: text(&case, "before").unwrap_or_default(), before_ms: before.expect("cut"), counts };
                let scope: Option<HashSet<String>> =
                    case["projectIds"].as_array().map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_owned).collect());
                report.check(&name, unloaded_count(&older, scope.as_ref(), &rows) as u64, case["expected"].as_u64().unwrap_or(u64::MAX));
            }
            "shelf" => {
                let older = text(&case, "older").and_then(|before| {
                    Some(OlderSettled { before_ms: parse_iso(&before)?, before, counts: HashMap::new() })
                });
                let requested = case["requested"].as_u64().unwrap_or_default() as usize;
                let mut pages = SettledPages::default();
                pages.reads = vec![FamilyRead::default(); requested];
                pages.frontier = cursor(&case["frontier"]);
                pages.status = match text(&case, "status").as_deref() {
                    Some("pending") => PageStatus::Pending,
                    Some("failed") => PageStatus::Failed,
                    _ => PageStatus::Ready,
                };
                pages.requested = requested;
                report.check(&format!("{name} frontier"), cursor_json(pages.listing_frontier(older.as_ref())), case["expectedFrontier"].clone());
                let listed = case["listedCount"].as_u64().unwrap_or_default() as usize;
                let visible = case["visibleCount"].as_u64().unwrap_or_default() as usize;
                pages.grow(older.as_ref(), listed, visible);
                report.check(&format!("{name} requested"), pages.requested as u64, case["expectedRequested"].as_u64().unwrap_or(u64::MAX));
            }
            "rowsMissing" => {
                let shell_threads = shells(&case["shellThreads"]);
                let families: Vec<FamilyRead> = serde_json::from_value(case["families"].clone()).expect("families decode");
                let missing: Vec<String> =
                    rows_missing_from_shell(&shell_threads, &families, &removals(&case)).into_iter().map(|t| t.id.clone()).collect();
                let expected: Vec<String> = list(&case, "expected").iter().filter_map(Value::as_str).map(str::to_owned).collect();
                report.check(&name, missing, expected);
            }
            other => panic!("{name}: unknown kind {other}"),
        }
        report.compared += 1;
    }
    assert_eq!(kinds.len(), 5, "every kind has cases: {kinds:?}");
    report.finish("shelf", 80);
}

fn observation(value: &Value) -> Option<Observation> {
    (!value.is_null()).then(|| Observation {
        attention: text(value, "attention"),
        settled: value["settled"].as_bool() == Some(true),
        completed_at: value["completedAt"].as_i64(),
    })
}

#[test]
fn notification_gate_matches_typescript() {
    let mut report = Report::default();
    for case in fixture("notify.jsonl") {
        let name = text(&case, "name").unwrap_or_default();
        let mut gate = NotificationGate::<String>::default();
        for (index, step) in list(&case, "steps").iter().enumerate() {
            if let Some(kept) = step.get("retain") {
                let kept: HashSet<String> = kept.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
                gate.retain(|key| kept.contains(key));
                continue;
            }
            let key = text(step, "key").unwrap_or_default();
            let next = observation(&step["next"]).expect("next observation");
            let fired = gate.observe(&key, observation(&step["prior"]).as_ref(), &next, step["archived"] == true, step["quiet"] == true);
            let expected = list(step, "fired").first().and_then(Value::as_str).map(|kind| match kind {
                "input" => NotificationKind::Input,
                _ => NotificationKind::Completion,
            });
            report.check(&format!("{name} step {index}"), fired, expected);
            report.compared += 1;
        }
    }
    report.finish("notify", 500);
}

/// A CSS colour `terminalOutputSpans` resolved: `#rrggbb` or `rgb(r,g,b)`; anything else paints
/// as unset, which is how the Rust port models it.
fn css_color(value: &Value) -> Option<Rgb> {
    let text = value.as_str()?;
    let channels: Vec<u8> = match text.strip_prefix('#') {
        Some(hex) => (0..3).map(|at| hex.get(at * 2..at * 2 + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok())).collect::<Option<_>>()?,
        None => text.strip_prefix("rgb(")?.strip_suffix(')')?.split(',').map(|part| part.parse().ok()).collect::<Option<_>>()?,
    };
    match channels[..] {
        [r, g, b] => Some(Rgb { r, g, b }),
        _ => None,
    }
}

fn typescript_spans(value: &Value) -> Vec<(String, Option<SpanStyle>)> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|span| {
            let style = &span["style"];
            let decoration = style["textDecorationLine"].as_str().unwrap_or_default();
            let style = (!style.is_null()).then(|| SpanStyle {
                color: css_color(&style["color"]),
                background: css_color(&style["backgroundColor"]),
                bold: style["fontWeight"] == "600",
                dim: style.get("opacity").is_some(),
                italic: style["fontStyle"] == "italic",
                underline: decoration.contains("underline"),
                strike: decoration.contains("line-through"),
            });
            (text(span, "text").unwrap_or_default(), style)
        })
        .collect()
}

fn rust_spans(text: &str, palette: &[Rgb; 16]) -> Vec<(String, Option<SpanStyle>)> {
    terminal_output_spans(text, palette, MAX_SPANS).into_iter().map(|span| (span.text, span.style)).collect()
}

#[test]
fn command_output_matches_typescript() {
    let mut report = Report::default();
    for case in fixture("command-output.jsonl") {
        let name = text(&case, "name").unwrap_or_default();
        let palette = if case["dark"] == true { &DARK_PALETTE } else { &LIGHT_PALETTE };
        let expected = |step: &Value| (text(step, "text").unwrap_or_default(), step["truncated"] == true, text(step, "pending").unwrap_or_default());
        if let Some(max_chars) = case["maxChars"].as_u64() {
            let mut state = TerminalOutputState::default();
            for (index, step) in list(&case, "steps").iter().enumerate() {
                state.append(&text(step, "chunk").unwrap_or_default(), max_chars as usize);
                let at = format!("{name} step {index} after {:?}", text(step, "chunk").unwrap_or_default());
                report.check(&at, (state.text.clone(), state.truncated, state.pending()), expected(step));
                if let Some(spans) = step.get("spans") {
                    report.check(&format!("{at} spans"), rust_spans(&state.text, palette), typescript_spans(spans));
                }
                report.compared += 1;
            }
            continue;
        }
        let mut view = CommandOutputView::default();
        for (index, step) in list(&case, "steps").iter().enumerate() {
            let frame: CommandOutputFrame = serde_json::from_value(step["frame"].clone()).expect("frames decode");
            view.apply(&frame);
            let at = format!("{name} frame {index} {frame:?}");
            report.check(&at, (view.output.text.clone(), view.output.truncated, view.output.pending()), expected(step));
            report.check(&format!("{at} running"), view.running, step["running"] == true);
            report.compared += 1;
        }
        report.check(&format!("{name} spans"), rust_spans(&view.output.text, palette), typescript_spans(&case["spans"]));
    }
    report.finish("command-output", 1000);
}

/// Each non-whitespace character's capture, `None` where nothing is captured.
fn captures_by_char(runs: impl IntoIterator<Item = (String, Option<String>)>) -> Vec<(char, Option<String>)> {
    runs.into_iter().flat_map(|(text, capture)| text.chars().map(move |c| (c, capture.clone())).collect::<Vec<_>>()).filter(|(c, _)| !c.is_whitespace()).collect()
}

#[test]
fn item_detail_matches_typescript() {
    use t3_core::json_text::{is_json_text, json_tokens};
    use t3_core::timeline::{ToolCallLines, decode_detail, detail_revision, needs_detail_fetch, turn_item_has_detail, turn_item_output_images, turn_item_output_text};
    use t3_core::turn_item::TurnItemKind;

    let mut report = Report::default();
    for case in fixture("item-detail.jsonl") {
        let name = text(&case, "name").unwrap_or_default();
        if let Some(json) = text(&case, "json") {
            let is_json = case["isJson"] == true;
            report.check(&format!("{name} is JSON"), is_json_text(&json), is_json);
            if is_json {
                let rust = json_tokens(&json).into_iter().map(|(range, token)| (json[range].to_owned(), Some(token.capture().to_owned())));
                let typescript = list(&case, "runs").iter().map(|run| (run[0].as_str().unwrap_or_default().to_owned(), run[1].as_str().map(str::to_owned)));
                report.check(&format!("{name} runs"), captures_by_char(rust), captures_by_char(typescript));
            }
            report.compared += 1;
            continue;
        }
        let item = decode_detail(case["result"].clone()).expect("getTurnItem results decode");
        let Some(item) = item else {
            report.check(&format!("{name} item"), case["result"]["item"].is_null(), true);
            report.compared += 1;
            continue;
        };
        report.check(&format!("{name} needs fetch"), needs_detail_fetch(&item), case["needsFetch"] == true);
        report.check(&format!("{name} revision"), detail_revision(&item), text(&case, "revision").unwrap_or_default());
        report.check(&format!("{name} output text"), turn_item_output_text(&item), text(&case, "outputText"));
        let images: Vec<Value> = turn_item_output_images(&item).iter().map(|image| image.to_json()).collect();
        report.check(&format!("{name} output images"), images, list(&case, "outputImages").to_vec());
        report.check(&format!("{name} has detail"), turn_item_has_detail(&item), case["hasDetail"] == true);
        let lines = match &item.kind {
            TurnItemKind::CommandExecution { input, .. } => Some(ToolCallLines::command(input)),
            TurnItemKind::DynamicTool { input, .. } => Some(ToolCallLines::args(input)),
            _ => None,
        };
        let expected = (!case["callLines"].is_null()).then(|| {
            let call = &case["callLines"];
            let args = call["args"].as_array().map(|args| {
                args.iter().map(|pair| (pair[0].as_str().unwrap_or_default().to_owned(), pair[1].as_str().unwrap_or_default().to_owned())).collect()
            });
            ToolCallLines { command: text(call, "command"), args, args_text: text(call, "argsText") }
        });
        report.check(&format!("{name} call lines"), lines, expected);
        report.compared += 1;
    }
    report.finish("item-detail", 40);
}


#[test]
fn grouping_current_typescript_parity() {
    use t3_core::sidebar::grouping::*;
    for case in fixture("grouping.jsonl") {
        let kind = case["kind"].as_str().unwrap();
        if kind == "reorder" {
            let strings = |key: &str| list(&case, key).iter().map(|value| value.as_str().unwrap().to_owned()).collect::<Vec<_>>();
            let mut preferences = ProjectPreferences::default();
            preferences.reorder(&strings("current"), &strings("dragged"), &strings("target"));
            assert_eq!(serde_json::json!(preferences.project_order), case["expected"], "{}", case["name"]);
            continue;
        }
        let project_values = if kind == "expansion" { vec![serde_json::json!({"environmentId":"a","id":"p","title":"Repo","workspaceRoot":"/repo","repositoryIdentity":{"canonicalKey":"repo"},"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z"})] } else { list(&case, "projects").to_vec() };
        let projects = project_values.iter().map(|project| {
            let saved = project["environmentId"].as_str().unwrap();
            ScopedProject { owner: EnvironmentOwner { saved_id: saved.into(), canonical_id: Some(saved.into()), ..Default::default() }, project: serde_json::from_value(project.clone()).unwrap(), is_scratch: project["isScratch"].as_bool().unwrap_or(false), environment_label: saved.into() }
        }).collect::<Vec<_>>();
        let settings = GroupingSettings { mode: case["settings"]["sidebarProjectGroupingMode"].as_str().and_then(GroupingMode::parse).unwrap_or_default(), overrides: case["settings"]["sidebarProjectGroupingOverrides"].as_object().into_iter().flatten().filter_map(|(key, value)| Some((key.clone(), GroupingMode::parse(value.as_str()?)?))).collect(), ..Default::default() };
        let mut preferences = ProjectPreferences::default();
        let groups = build_scoped_project_groups(&GroupInput { projects: &projects, threads: &[], settings: &settings, preferences: &preferences, preferred_environment: case["preferred"].as_str(), consumer: if case["scratch"].as_bool() == Some(true) { GroupConsumer::Sidebar } else { GroupConsumer::Palette }, sort: ProjectSortOrder::Manual });
        if kind == "expansion" {
            preferences.project_expanded = serde_json::from_value(case["record"].clone()).unwrap();
            assert_eq!(serde_json::json!(preferences.expanded(&groups[0])), case["expected"], "{}", case["name"]);
        } else if kind == "scope" {
            preferences.scope_keys = serde_json::from_value(case["scope"].clone()).unwrap();
            assert_eq!(serde_json::json!(preferences.resolved_scope(&groups, true)), case["expectedScope"], "{}", case["name"]);
            assert_eq!(serde_json::json!(preferences.resolved_scope(&groups, false)), case["expectedLoading"], "{}", case["name"]);
            let context = serde_json::from_value::<ProjectRef>(case["context"].clone()).unwrap();
            let targets = groups.iter().filter_map(|group| pick_project_target(group, Some(&context))).map(|member| member.project.reference()).collect::<Vec<_>>();
            assert_eq!(serde_json::json!(targets), case["expectedTargets"], "{}", case["name"]);
        } else {
            let actual = groups.iter().map(|group| serde_json::json!({
                "key":group.key, "label":group.label, "representative":group.representative, "refs":group.member_refs,
                "members":group.members.iter().map(|member| serde_json::json!({"physical":member.physical_key,"environmentId":member.project.owner.saved_id,"projectId":member.project.project.id})).collect::<Vec<_>>(),
                "pinned":group.pinned, "archived":group.archived,
            })).collect::<Vec<_>>();
            assert_eq!(serde_json::json!(actual), case["expected"], "{}", case["name"]);
        }
    }
}
