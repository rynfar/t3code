//! Clipboard/stash assembly from `useComposerContextImport.ts` and `useComposerStash.ts`.
use serde_json::Value;
use t3_core::composer::{
    attachments::{Attachment, Upload},
    records,
};

#[derive(Clone, Copy)]
pub(super) enum ImportMode {
    Copy,
    Move,
}
pub(super) struct ImportedAttachment {
    pub attachment: Attachment,
    pub source: Option<String>,
}
pub(super) struct ImportPlan {
    pub text: String,
    pub records: Vec<Value>,
    pub reviews: Vec<t3_core::composer::review::ReviewComment>,
    pub attachments: Vec<ImportedAttachment>,
}

pub(super) fn prepare(
    text: &str,
    incoming: Vec<Value>,
    existing: &[Value],
    environment: &str,
    mode: ImportMode,
    fresh: impl FnMut() -> String,
) -> Option<ImportPlan> {
    let mut held = existing.to_vec();
    let text = match mode {
        ImportMode::Copy => {
            records::import_copied_records(text, incoming, &mut held, environment, fresh)
        }
        ImportMode::Move => {
            records::import_records(text, incoming, &mut held, environment, environment, fresh)
        }
    }?;
    let mut records = Vec::new();
    let mut reviews = Vec::new();
    let mut attachments = Vec::new();
    for record in held.into_iter().filter(|r| !existing.contains(r)) {
        if let Some(mut attachment) = records::attachment_from_record(&record) {
            let source = match mode {
                ImportMode::Copy => {
                    let source = attachment.attachment_id().map(str::to_owned);
                    attachment.upload = Upload::Uploading;
                    source
                }
                ImportMode::Move => None,
            };
            attachments.push(ImportedAttachment { attachment, source });
        } else if !records::pull_request(&record)
            && let Some(review) = t3_core::composer::review::ReviewComment::from_record(&record)
        {
            reviews.push(review);
        } else {
            records.push(record);
        }
    }
    Some(ImportPlan {
        text,
        records,
        reviews,
        attachments,
    })
}

pub(crate) fn copy_chip(id: &str, all: &[Value], environment: &str) -> Option<(String, String)> {
    let text = records::reference(all.iter().find(|r| records::field(r, "contextId") == id)?)?;
    let fragment = records::copy_fragment(&text, all, environment, None, None)?;
    Some((text, fragment))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use t3_core::composer::{attachments::Upload, records};
    fn image() -> Value {
        json!({"version":1,"kind":"image","contextId":"image_a","label":"shot.png","attachmentId":"source-upload","name":"shot.png","mimeType":"image/png","sizeBytes":8})
    }
    #[test]
    fn copying_an_attachment_does_not_adopt_the_sources_upload() {
        let record = image();
        let text = records::reference(&record).unwrap();
        let plan = prepare(&text, vec![record], &[], "env", ImportMode::Copy, || {
            "fresh".into()
        })
        .unwrap();
        assert_eq!(plan.attachments.len(), 1);
        assert_eq!(plan.attachments[0].attachment.upload, Upload::Uploading);
        assert!(plan.attachments[0].attachment.attachment_id().is_none());
        assert_eq!(plan.attachments[0].source.as_deref(), Some("source-upload"));
    }
    #[test]
    fn pasting_a_ready_attachments_own_fragment_does_not_stage_another_copy() {
        for (kind, name, mime) in [
            ("image", "shot.png", "image/png"),
            ("file", "notes.txt", "text/plain"),
        ] {
            let record = json!({"version":1,"kind":kind,"contextId":format!("{kind}_a"),"label":name,"attachmentId":"source-upload","name":name,"mimeType":mime,"sizeBytes":8});
            let ready = records::attachment_from_record(&record).unwrap();
            let existing = vec![ready.context_record().unwrap()];
            let (text, fragment) = copy_chip(&ready.context_id(), &existing, "env").unwrap();
            let fragment = records::decode_fragment(&fragment).unwrap();
            let incoming =
                records::clipboard_records(&text, fragment["records"].as_array().unwrap());
            let plan = prepare(&text, incoming, &existing, "env", ImportMode::Copy, || {
                "duplicate".into()
            })
            .unwrap();
            assert!(
                plan.attachments.is_empty(),
                "an identical Ready attachment must not queue a copy"
            );
            assert!(plan.records.is_empty());
            assert!(plan.reviews.is_empty());
            assert_eq!(plan.text, text);
        }
    }
    #[test]
    fn overlapping_attachment_copies_have_independent_local_ids() {
        let record = image();
        let text = records::reference(&record).unwrap();
        let mut staged: Vec<ImportedAttachment> = Vec::new();
        for id in ["copy-one", "copy-two"] {
            let held = staged
                .iter()
                .filter_map(|a| a.attachment.context_record())
                .collect::<Vec<_>>();
            let plan = prepare(
                &text,
                vec![record.clone()],
                &held,
                "env",
                ImportMode::Copy,
                || id.into(),
            )
            .unwrap();
            let attachment = &plan.attachments[0].attachment;
            assert!(!staged.iter().any(|a| a.attachment.id == attachment.id));
            assert_eq!(
                plan.text,
                format!(
                    "![shot.png](t3-context://v1/image/{})",
                    attachment.context_id()
                )
            );
            staged.extend(plan.attachments);
        }
        for id in ["copy-one", "copy-two"] {
            staged
                .iter_mut()
                .find(|a| a.attachment.id == id)
                .unwrap()
                .attachment
                .upload = Upload::Ready {
                attachment_id: format!("upload-{id}"),
            };
        }
        assert!(
            staged
                .iter()
                .all(|a| a.attachment.attachment_id().is_some())
        );
    }
    #[test]
    fn stash_restore_preserves_both_colliding_payloads_and_rewrites_the_prompt() {
        let old = json!({"version":1,"kind":"preview-annotation","contextId":"preview-annotation_a","annotationId":"a","label":"Button","pageUrl":"http://localhost","comment":"old"});
        let mut stashed = old.clone();
        stashed["comment"] = json!("stashed");
        let review = json!({"version":1,"kind":"review-comment","contextId":"review-comment_a","label":"main.rs L1","sectionId":"diff","sectionTitle":"Changes","filePath":"main.rs","startIndex":0,"endIndex":0,"rangeLabel":"+1","text":"old","diff":"+old"});
        let mut stashed_review = review.clone();
        stashed_review["text"] = json!("stashed");
        let text = format!(
            "{} {}",
            records::reference(&stashed).unwrap(),
            records::reference(&stashed_review).unwrap()
        );
        let plan = prepare(
            &text,
            vec![stashed, stashed_review],
            &[old.clone(), review],
            "env",
            ImportMode::Move,
            || "fresh".into(),
        )
        .unwrap();
        assert_eq!(
            plan.text,
            "[Button](t3-context://v1/preview-annotation/preview-annotation_fresh) [main.rs L1](t3-context://v1/review-comment/review-comment_fresh)"
        );
        assert_eq!(plan.records.len(), 1);
        assert_eq!(plan.records[0]["comment"], "stashed");
        assert_eq!(plan.reviews.len(), 1);
        assert_eq!(plan.reviews[0].context_id(), "review-comment_fresh");
        assert_eq!(&*plan.reviews[0].text, "stashed");
        assert_eq!(old["comment"], "old");
    }
    #[test]
    fn chip_copy_uses_the_entire_collection_for_screenshot_dependencies() {
        let annotation = json!({"version":1,"kind":"preview-annotation","contextId":"preview-annotation_a","annotationId":"a","label":"Button","pageUrl":"http://localhost","comment":"fix","screenshotContextId":"image_a"});
        let all = vec![annotation, image()];
        let (_, fragment) = copy_chip("preview-annotation_a", &all, "env").unwrap();
        assert_eq!(
            records::decode_fragment(&fragment).unwrap()["records"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}

#[derive(Clone)]
pub(super) struct ChipReference {
    pub id: String,
    pub kind: String,
    pub label: String,
}
#[derive(Default)]
pub(super) struct PresentationCache {
    text: Option<String>,
    parsed: std::sync::Arc<[ChipReference]>,
}
impl PresentationCache {
    pub fn references(&mut self, text: &str) -> std::sync::Arc<[ChipReference]> {
        if self.text.as_deref() != Some(text) {
            self.parsed = t3_core::composer::menu::context_references(text)
                .into_iter()
                .map(|r| ChipReference {
                    id: r.context_id.into(),
                    kind: r.kind.into(),
                    label: r.label.into(),
                })
                .collect::<Vec<_>>()
                .into();
            self.text = Some(text.to_owned());
        }
        self.parsed.clone()
    }
}
#[cfg(test)]
mod cache_tests {
    use super::*;
    #[test]
    fn unchanged_prompt_reuses_parsed_references() {
        let mut cache = PresentationCache::default();
        let first = cache.references("Use [Mac](t3-context://v1/environment/environment_a)");
        let again = cache.references("Use [Mac](t3-context://v1/environment/environment_a)");
        assert!(std::sync::Arc::ptr_eq(&first, &again));
        assert_eq!(first[0].id, "environment_a");
        assert!(cache.references("no links").is_empty());
    }
}
