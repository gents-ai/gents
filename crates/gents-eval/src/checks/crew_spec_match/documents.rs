use serde_json::Value;

use super::params::{Receipt, TemplateSpec};
use super::tally::Tally;
use super::{capture_rows, StageEvidence};
use super::{doc_fields, find};

pub(super) fn check_templates(
    templates: &[TemplateSpec],
    stage: &StageEvidence,
    tally: &mut Tally,
) {
    let rows = |name: &str| capture_rows(stage, name);
    for spec in templates {
        let category = spec.category.as_deref().or(Some("templates"));
        let row = rows(&spec.capture).and_then(|rows| find(rows, &spec.key, &spec.id));
        let Some(row) = row else {
            tally.record(category, false, || {
                format!("{} {} absent, no template to check", spec.capture, spec.id)
            });
            continue;
        };
        for field in &spec.fields {
            let Some(template) = row.get(field).and_then(Value::as_str) else {
                continue;
            };
            let unknown: Vec<String> = doc_fields(template)
                .into_iter()
                .filter(|name| !spec.allowed.contains(name))
                .collect();
            tally.record(category, unknown.is_empty(), || {
                format!(
                    "{} {}.{field} reads doc.{} outside its source collection",
                    spec.capture,
                    spec.id,
                    unknown.join(", doc.")
                )
            });
        }
    }
}

pub(super) fn check_receipt(receipt: &Receipt, stage: &StageEvidence, tally: &mut Tally) {
    let rows = |name: &str| capture_rows(stage, name);
    let category = receipt.category.as_deref().or(Some("receipt"));
    let text: String = rows(&receipt.capture)
        .unwrap_or_default()
        .iter()
        .flat_map(|row| {
            receipt.fields.iter().filter_map(|field| {
                row.get(field).map(|value| match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    for id in &receipt.ids {
        tally.record(category, text.contains(id.as_str()), || {
            format!("receipt does not name {id}")
        });
    }
}
