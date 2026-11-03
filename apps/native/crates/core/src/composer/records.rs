//! Record/clipboard rules from `web/src/lib/composerContextRecords.ts`,
//! `components/composerInlineTokenPaste.ts`, `chat/useComposerContextImport.ts` and
//! `shared/src/composerContextClipboard.ts`. The prompt retains its exact context links on send.
use super::{
    context::{Linked, format_reference},
    menu::{context_references, kind_scoped_context_id, sanitize_context_label},
};
use serde_json::{Value, json};

#[cfg(test)]
mod tests;

pub fn environment_record(environment_id: &str, label: &str) -> Value {
    json!({"version":1,"kind":"environment","contextId":kind_scoped_context_id("environment", environment_id),"label":sanitize_context_label(label,"environment"),"environmentId":environment_id})
}

pub fn field<'a>(record: &'a Value, key: &str) -> &'a str {
    record.get(key).and_then(Value::as_str).unwrap_or_default()
}
pub fn reference(record: &Value) -> Option<String> {
    Some(format_reference(
        record.get("kind")?.as_str()?,
        record.get("contextId")?.as_str()?,
        field(record, "label"),
    ))
}
impl Linked for Value {
    fn link_id(&self) -> String {
        field(self, "contextId").to_owned()
    }
}

pub fn referenced(prompt: &str, records: &[Value]) -> Vec<Value> {
    let mut found = Vec::new();
    for occurrence in context_references(prompt) {
        if let Some(record) = records.iter().find(|record| {
            field(record, "contextId") == occurrence.context_id
                && field(record, "kind") == occurrence.kind
        }) && !found
            .iter()
            .any(|seen| field(seen, "contextId") == occurrence.context_id)
        {
            found.push(record.clone());
        }
    }
    found
}

pub fn message_context(prompt: &str, records: &[Value]) -> Option<Value> {
    let mut selected = referenced(prompt, records);
    for record in records
        .iter()
        .filter(|r| matches!(field(r, "kind"), "file" | "image"))
    {
        if !selected
            .iter()
            .any(|r| field(r, "contextId") == field(record, "contextId"))
        {
            selected.push(record.clone());
        }
    }
    let mut records = selected;
    // buildMessageContext groups records by producer; chip order follows the prompt instead.
    records.sort_by_key(|r| match field(r, "kind") {
        "terminal" => 0,
        "review-comment" => 1,
        "thread" => 2,
        "environment" => 3,
        "preview-annotation" | "element" => 4,
        _ => 5,
    });
    (!records.is_empty()).then(|| json!({"version":1,"records":records}))
}

fn clipboard_records(markdown: &str, records: &[Value]) -> Vec<Value> {
    let mut selected = referenced(markdown, records);
    let screenshots: Vec<String> = selected
        .iter()
        .filter(|r| field(r, "kind") == "preview-annotation")
        .filter_map(|r| {
            r.get("screenshotContextId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    for id in screenshots {
        if !selected.iter().any(|r| field(r, "contextId") == id)
            && let Some(image) = records
                .iter()
                .find(|r| field(r, "contextId") == id && field(r, "kind") == "image")
        {
            selected.push(image.clone());
        }
    }
    selected
}

pub fn copy_fragment(
    markdown: &str,
    records: &[Value],
    environment: &str,
    thread: Option<&str>,
    message: Option<&str>,
) -> Option<String> {
    let records = clipboard_records(markdown, records);
    if records.is_empty() || records.len() > 200 {
        return None;
    }
    let mut source = json!({"environmentId":environment});
    if let Some(id) = thread {
        source["threadId"] = json!(id);
    }
    if let Some(id) = message {
        source["messageId"] = json!(id);
    }
    let encoded = json!({"version":1,"source":source,"records":records}).to_string();
    (encoded.encode_utf16().count() <= 16_000_000).then_some(encoded)
}

fn valid_record(record: &Value) -> bool {
    record.get("version").and_then(Value::as_u64) == Some(1)
        && super::menu::is_context_id(field(record, "contextId"))
        && record
            .get("label")
            .and_then(Value::as_str)
            .is_some_and(|s| s.encode_utf16().count() <= 200)
        && match field(record, "kind") {
            "thread" => {
                !field(record, "threadId").is_empty() && !field(record, "environmentId").is_empty()
            }
            "environment" => !field(record, "environmentId").is_empty(),
            "terminal" => {
                ["terminalId", "terminalLabel", "text"]
                    .iter()
                    .all(|key| record.get(key).is_some_and(Value::is_string))
                    && ["lineStart", "lineEnd"].iter().all(|key| {
                        record
                            .get(key)
                            .and_then(Value::as_u64)
                            .is_some_and(|n| n > 0)
                    })
            }
            "review-comment" => super::review::ReviewComment::from_record(record).is_some(),
            "preview-annotation" => {
                record.get("pageUrl").is_some_and(Value::is_string)
                    && record.get("comment").is_some_and(Value::is_string)
            }
            "element" => record.get("selector").is_some_and(Value::is_string),
            "mention" => record.get("path").is_some_and(Value::is_string),
            "skill" => record.get("name").is_some_and(Value::is_string),
            "image" | "file" => {
                !field(record, "attachmentId").is_empty()
                    && record.get("sizeBytes").and_then(Value::as_u64).is_some()
            }
            _ => false,
        }
}

pub fn decode_fragment(raw: &str) -> Option<Value> {
    if raw.encode_utf16().count() > 16_000_000 {
        return None;
    }
    let mut value: Value = serde_json::from_str(raw).ok()?;
    if value.get("version")?.as_u64()? != 1
        || field(value.get("source")?, "environmentId").is_empty()
    {
        return None;
    }
    let records = value.get_mut("records")?.as_array_mut()?;
    if records.len() > 200 {
        return None;
    }
    records.retain(valid_record);
    Some(value)
}

fn same_payload(a: &Value, b: &Value) -> bool {
    let strip = |v: &Value| {
        let mut v = v.clone();
        if let Some(o) = v.as_object_mut() {
            o.remove("contextId");
            o.remove("label");
        }
        v
    };
    strip(a) == strip(b)
}

pub fn import_fragment(
    markdown: &str,
    raw: &str,
    held: &mut Vec<Value>,
    environment: &str,
    mut fresh: impl FnMut() -> String,
) -> Option<String> {
    let fragment = decode_fragment(raw)?;
    let source = field(&fragment["source"], "environmentId");
    let mut selected = clipboard_records(markdown, fragment["records"].as_array()?);
    selected.sort_by_key(|r| field(r, "kind") != "preview-annotation");
    let mut rewrites = std::collections::HashMap::new();
    let mut skipped = std::collections::HashSet::new();
    let mut imported = std::collections::HashSet::new();
    for mut record in selected {
        if skipped.contains(field(&record, "contextId")) {
            continue;
        }
        let kind = field(&record, "kind").to_owned();
        if (kind == "thread" && field(&record, "environmentId") != environment)
            || (matches!(kind.as_str(), "image" | "file") && source != environment)
        {
            continue;
        }
        let original = field(&record, "contextId").to_owned();
        let producer = original
            .strip_prefix(&format!("{kind}_"))
            .unwrap_or(&original);
        let canonical = kind_scoped_context_id(&kind, producer);
        let existing = held
            .iter()
            .find(|r| field(r, "contextId") == canonical || field(r, "contextId") == original);
        if let Some(existing) = existing
            && same_payload(existing, &record)
        {
            if kind == "preview-annotation"
                && let Some(screenshot) = record.get("screenshotContextId").and_then(Value::as_str)
            {
                skipped.insert(screenshot.to_owned());
            }
            rewrites.insert(original, field(existing, "contextId").to_owned());
            continue;
        }
        let reminted = existing.is_some() && !matches!(kind.as_str(), "thread" | "environment");
        let id = if reminted {
            kind_scoped_context_id(&kind, &fresh())
        } else {
            canonical
        };
        if kind == "preview-annotation" && reminted {
            record["annotationId"] = json!(id.strip_prefix("preview-annotation_").unwrap_or(&id));
        }
        record["contextId"] = json!(id);
        imported.insert(id.clone());
        held.retain(|r| field(r, "contextId") != id);
        held.push(record);
        rewrites.insert(original, id);
    }
    for record in held.iter_mut().filter(|r| {
        imported.contains(field(r, "contextId")) && field(r, "kind") == "preview-annotation"
    }) {
        if let Some(next) = rewrites.get(field(record, "screenshotContextId")) {
            record["screenshotContextId"] = json!(next);
        }
    }
    let mut result = markdown.to_owned();
    for occurrence in context_references(markdown).into_iter().rev() {
        if let Some(id) = rewrites.get(occurrence.context_id) {
            result.replace_range(
                occurrence.start..occurrence.end,
                &format_reference(occurrence.kind, id, occurrence.label),
            );
        }
    }
    Some(result)
}

/// PR metadata is a review-comment producer; legacy PR summaries have no metadata.
pub fn pull_request(record: &Value) -> bool {
    record.get("pullRequest").is_some_and(Value::is_object)
        || (field(record, "sectionId").starts_with("pull-request:")
            && field(record, "diff").trim().is_empty()
            && field(record, "filePath")
                .strip_prefix("PR #")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
}
pub fn pr_state(record: &Value) -> &str {
    let pr = &record["pullRequest"];
    match field(pr, "state") {
        "open" if pr["isDraft"] == true => "draft",
        "open" => "open",
        "closed" => "closed",
        "merged" => "merged",
        _ => "unknown",
    }
}
pub fn label(record: &Value) -> String {
    if pull_request(record) {
        if let Some(number) = record["pullRequest"]["number"].as_u64() {
            return format!("#{number}");
        }
        if let Some(number) = field(record, "filePath").strip_prefix("PR #") {
            return format!("#{number}");
        }
    }
    field(record, "label").to_owned()
}

pub fn terminal_from_record(
    record: &Value,
    thread: &str,
    created_at: &str,
) -> Option<super::context::TerminalContext> {
    if field(record, "kind") != "terminal" {
        return None;
    }
    let mut value = record.clone();
    value["id"] = json!(
        field(record, "contextId")
            .strip_prefix("terminal_")
            .unwrap_or(field(record, "contextId"))
    );
    value["threadId"] = json!(thread);
    value["createdAt"] = json!(created_at);
    serde_json::from_value(value).ok()
}
pub fn attachment_from_record(record: &Value) -> Option<super::attachments::Attachment> {
    use super::attachments::{Attachment, AttachmentKind, Upload};
    let kind = match field(record, "kind") {
        "image" => AttachmentKind::Image,
        "file" => AttachmentKind::File,
        _ => return None,
    };
    Some(Attachment {
        id: field(record, "contextId")
            .strip_prefix(&format!("{}_", kind.as_str()))
            .unwrap_or(field(record, "contextId"))
            .into(),
        kind,
        name: field(record, "name").into(),
        mime_type: field(record, "mimeType").into(),
        size_bytes: record["sizeBytes"].as_u64()?,
        pasted_text: false,
        upload: Upload::Ready {
            attachment_id: field(record, "attachmentId").into(),
        },
    })
}

pub fn environment_items<'a>(
    environments: impl Iterator<Item = (&'a str, &'a str)>,
    active: &str,
    query: &str,
) -> Vec<super::menu::MenuItem> {
    use super::menu::{MenuAction, MenuItem};
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    environments
        .filter(|(id, label)| *id != active && label.to_lowercase().contains(&query))
        .map(|(id, label)| MenuItem {
            id: format!("environment:{id}"),
            label: label.into(),
            description: "Machine".into(),
            action: MenuAction::Environment {
                environment_id: id.into(),
                label: label.into(),
            },
        })
        .collect()
}

pub fn presentation_records<'a>(
    text: &str,
    records: &'a [Value],
) -> Vec<std::borrow::Cow<'a, Value>> {
    use std::borrow::Cow;
    let mut linked: Vec<Cow<'a, Value>> = Vec::new();
    for r in context_references(text) {
        if linked.iter().any(|v| field(v, "contextId") == r.context_id) {
            continue;
        }
        linked.push(records.iter().find(|v|field(v,"contextId")==r.context_id && field(v,"kind")==r.kind).map(Cow::Borrowed).unwrap_or_else(||Cow::Owned(json!({"version":1,"kind":"unresolved","contextId":r.context_id,"label":r.label}))));
    }
    linked
}

pub fn display_text(text: &str) -> String {
    let mut display = text.to_owned();
    for r in context_references(text).into_iter().rev() {
        display.replace_range(r.start..r.end, if r.kind == "image" { "" } else { r.label });
    }
    display
}

fn uri_component(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

pub fn clipboard_html(text: &str, fragment: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<pre data-t3-context-fragment=\"{}\">{escaped}</pre>",
        uri_component(fragment)
    )
}
pub fn fragment_from_html(html: &str) -> Option<String> {
    if html.len() > 16_000_000 * 9 + 4096 {
        return None;
    }
    let rest = html.split_once("data-t3-context-fragment=")?.1;
    let quote = rest.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let encoded = rest[1..].split_once(quote)?.0;
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut bytes = encoded.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?
        } else {
            byte
        });
    }
    let fragment = String::from_utf8(decoded).ok()?;
    decode_fragment(&fragment)?;
    Some(fragment)
}
