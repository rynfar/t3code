//! Answered-question previews: `packages/client-runtime/src/work-log/userInput.ts`.

use serde_json::Value;

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `getQuestionAnswerText`.
pub fn question_answer_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
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
