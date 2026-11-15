//! Answered-question previews: `packages/client-runtime/src/work-log/userInput.ts`.

use serde_json::Value;

const CONTEXT_ENVELOPE_TAG: &str = "t3_context";
const MARKER_ENVELOPE_PREFIX: &str = "\n\n<t3_context version=\"1\">\n<context kind=\"";
const ENVELOPE_CLOSE: &str = "</t3_context>";

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Readable form of `projectComposerContextForProvider` output (`composerContextProviderTextForDisplay`).
/// Only an envelope right after its own `[Kind: Label; ref=id]` marker is removed; literal XML stays.
pub fn composer_context_provider_text_for_display(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(rel_start) = text[cursor..].find('[') {
        let start = cursor + rel_start;
        let Some(rel_end) = text[start..].find(']') else { break };
        let marker_end = start + rel_end + 1;
        let inner = &text[start + 1..start + rel_end];
        let parsed = inner.split_once(": ").and_then(|(kind, rest)| {
            let (label, context_id) = rest.rsplit_once("; ref=")?;
            let valid_kind = kind.starts_with(|c: char| c.is_ascii_uppercase())
                && !kind.contains(['[', ']', ':', ';', '\n']);
            let valid_label = !label.contains(['[', ']', ';', '\n']);
            let valid_id = !context_id.is_empty()
                && !context_id.contains(|c: char| c == '[' || c == ']' || c.is_whitespace());
            (valid_kind && valid_label && valid_id).then_some((kind, label, context_id))
        });
        let Some((kind, label, context_id)) = parsed else {
            result.push_str(&text[cursor..start + 1]);
            cursor = start + 1;
            continue;
        };
        if !text[marker_end..].starts_with(MARKER_ENVELOPE_PREFIX) {
            result.push_str(&text[cursor..start + 1]);
            cursor = start + 1;
            continue;
        }
        let kind_start = marker_end + MARKER_ENVELOPE_PREFIX.len();
        let Some(rel_kind_end) = text[kind_start..].find('"') else {
            result.push_str(&text[cursor..start + 1]);
            cursor = start + 1;
            continue;
        };
        let after_kind = kind_start + rel_kind_end + 1;
        let expected_id = format!(" id=\"{}\"", escape_attribute(context_id));
        if !text[after_kind..].starts_with(&expected_id) {
            result.push_str(&text[cursor..start + 1]);
            cursor = start + 1;
            continue;
        }
        let Some(rel_close) = text[after_kind..].find(ENVELOPE_CLOSE) else {
            result.push_str(&text[cursor..start + 1]);
            cursor = start + 1;
            continue;
        };
        let close_end = after_kind + rel_close + ENVELOPE_CLOSE.len();
        let _ = CONTEXT_ENVELOPE_TAG;
        result.push_str(&text[cursor..start]);
        result.push('[');
        result.push_str(kind);
        result.push_str(": ");
        result.push_str(label);
        result.push(']');
        cursor = close_end;
    }
    result.push_str(&text[cursor..]);
    result
}

/// `getQuestionAnswerText`.
pub fn question_answer_text(value: &Value) -> String {
    match value {
        Value::String(text) => composer_context_provider_text_for_display(text),
        Value::Array(items) => items.iter().map(question_answer_text).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(", "),
        Value::Object(object) => object.get("answers").map(question_answer_text).unwrap_or_default(),
        _ => String::new(),
    }
}

fn values<'a>(answer: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    answer.get(key).and_then(Value::as_object).into_iter().flat_map(|map| map.values())
}

/// `getQuestionTextPreview`.
pub fn question_text_preview(answer: &Value) -> String {
    values(answer, "questionTextById").filter_map(Value::as_str).map(collapse).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" · ")
}

fn attachment_names(answer: &Value) -> Vec<String> {
    values(answer, "attachmentsByQuestionId")
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|attachment| attachment.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

/// `getQuestionAnswerPreview`.
pub fn question_answer_preview(answer: &Value) -> String {
    let answers: Vec<String> = values(answer, "answers").map(question_answer_text).filter(|t| !t.is_empty()).collect();
    let attachments = attachment_names(answer);
    let text = if !answers.is_empty() {
        answers.join(" · ")
    } else if !attachments.is_empty() {
        attachments.join(", ")
    } else {
        values(answer, "questionTextById").filter_map(Value::as_str).collect::<Vec<_>>().join(" · ")
    };
    collapse(&text)
}

/// `hasQuestionAnswer`.
pub fn has_question_answer(answer: &Value) -> bool {
    values(answer, "answers").any(|value| !question_answer_text(value).is_empty()) || !attachment_names(answer).is_empty()
}
