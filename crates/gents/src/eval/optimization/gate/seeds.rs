use serde_json::{json, Value};

use super::{reject, StructuralRejection};
use crate::documents::template::{render_template, task_node_ctx, TemplateScope};
use crate::documents::{EvalCase, EvalDefinition, EventSource};
use crate::eval::optimization::subject::{task_seed_routes, MaterializedPack};
use crate::eval::optimization::target::TargetField;
use crate::eval::runner::embedded::executor::install::bind_fixture_owner;

pub(crate) fn seed_gate(
    baseline: &MaterializedPack,
    definition: &EvalDefinition,
    text: &str,
) -> Result<(), StructuralRejection> {
    let failures = failed_seeds(baseline, definition, text);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(reject("template_seed_invalid", failures.join("; ")))
    }
}

fn failed_seeds(
    baseline: &MaterializedPack,
    definition: &EvalDefinition,
    text: &str,
) -> Vec<String> {
    if baseline.target != TargetField::TaskPromptTemplate {
        return Vec::new();
    }
    let now = definition
        .updated_at
        .as_deref()
        .unwrap_or("1970-01-01T00:00:00Z");
    let identity = uuid::Uuid::nil().to_string();
    let (node, ctx) = task_node_ctx(&definition.node_did, &baseline.agent_id, now);
    let mut failures = Vec::new();
    for case in &definition.cases {
        for (index, stage) in case.stages.iter().enumerate() {
            let Some(seed) = &stage.seed else { continue };
            let document = seed_document(&seed.document, &definition.node_did, &identity);
            for (trigger, source) in task_seed_routes(&baseline.config, &baseline.target_id)
                .filter(|(_, source)| source.source_collection == seed.collection)
            {
                let correlation = source
                    .correlation_field
                    .as_deref()
                    .and_then(|field| document.get(field))
                    .cloned()
                    .unwrap_or(Value::Null);
                let group = source.group.as_ref().map(|group| {
                    let docs =
                        group_documents(definition, case, index, source, &correlation, &identity);
                    json!({
                        "state_doc_id": identity,
                        "correlation_value": correlation,
                        "count": docs.len(),
                        "complete": crate::triggers::engine::event_delivery::expected_group_count(
                            group.expected_count.as_ref(), &docs
                        ).ok().flatten() == Some(docs.len()),
                        "docs": docs,
                    })
                });
                let scope = TemplateScope {
                    session: Some(json!({"session_id": identity})),
                    request: Some(json!({"request_id": identity})),
                    event: json!({
                        "fired_at": now,
                        "trigger_id": trigger.trigger_id,
                        "trigger_kind": "event",
                        "source_collection": seed.collection,
                        "source_doc_id": document["_docID"],
                        "correlation": correlation,
                    }),
                    doc: Some(document.clone()),
                    args: None,
                    group,
                    node: node.clone(),
                    ctx: ctx.clone(),
                };
                if let Err(error) = render_template(text, &scope) {
                    failures.push(format!(
                        "case {:?}, stage {:?}, trigger {:?}: {error}",
                        case.case_id, stage.stage_id, trigger.trigger_id
                    ));
                }
            }
        }
    }
    failures
}

fn seed_document(document: &Value, owner: &str, identity: &str) -> Value {
    let mut document = bind_fixture_owner(document, owner);
    if let Some(fields) = document.as_object_mut() {
        fields.entry("_docID").or_insert_with(|| json!(identity));
    }
    document
}

fn group_documents(
    definition: &EvalDefinition,
    case: &EvalCase,
    index: usize,
    source: &EventSource,
    correlation: &Value,
    identity: &str,
) -> Vec<Value> {
    definition
        .fixtures
        .iter()
        .chain(case.fixtures.iter())
        .flat_map(|fixtures| fixtures.documents.iter())
        .chain(
            case.stages[..=index]
                .iter()
                .filter_map(|stage| stage.seed.as_ref()),
        )
        .filter(|document| document.collection == source.source_collection)
        .map(|document| seed_document(&document.document, &definition.node_did, identity))
        .filter(|document| {
            source
                .correlation_field
                .as_deref()
                .and_then(|field| document.get(field))
                == Some(correlation)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::TriggerSource;

    #[test]
    fn exact_seed_rendering_matches_lean() {
        for row in &crate::lean_vocab_test::lean_optimization_cases().seed_rendering {
            let dir = tempfile::tempdir().unwrap();
            crate::eval::optimization::subject::tests::write_task_fixture_pack(dir.path(), false);
            let fixture = crate::eval::optimization::subject::materialize_pack(
                dir.path(),
                crate::eval::runner::freeze::tests::OWNER,
                "monitor",
                &crate::eval::optimization::target::JobTarget::Task("plan".into()),
            )
            .unwrap();
            let mut baseline = fixture.clone();
            baseline.target = if row.target_task.is_some() {
                TargetField::TaskPromptTemplate
            } else {
                TargetField::AgentContextSystemPrompt
            };
            baseline.config.tasks[0].enabled = row.task_enabled;
            baseline.config.triggers.clear();
            baseline.config.event_sources.clear();
            for (index, trigger) in row.triggers.iter().enumerate() {
                let mut value = fixture.config.triggers[0].clone();
                value.trigger_id = format!("trigger-{index}");
                value.task_id = trigger.task_id.clone();
                value.enabled = trigger.enabled;
                value.source = match &trigger.event_source_id {
                    Some(id) => TriggerSource::Event {
                        event_source_id: id.clone(),
                    },
                    None => TriggerSource::Schedule {
                        schedule_id: "schedule".into(),
                    },
                };
                baseline.config.triggers.push(value);
            }
            for source in &row.sources {
                let mut value = fixture.config.event_sources[0].clone();
                value.event_source_id = source.event_source_id.clone();
                value.source_collection = source.source_collection.clone();
                value.event_kind = source.event_kind.clone();
                baseline.config.event_sources.push(value);
            }
            let mut definition: EvalDefinition = serde_json::from_value(json!({
                "node_did": "did:key:seed-check", "definition_id": "seed-check",
                "comparability_version": 1, "subject": {"kind":"agent", "inference_slots":["primary"]},
                "cases": [],
            })).unwrap();
            for seed in &row.seeds {
                let case: EvalCase = serde_json::from_value(json!({
                    "case_id": seed.case_id, "split": "validation", "stages": [{
                        "stage_id": seed.stage_id, "prompt":"", "deadline_secs": 600,
                        "seed":{"collection":seed.collection,"document":seed.document},
                        "checks":[],
                    }],
                }))
                .unwrap();
                let single = EvalDefinition {
                    cases: vec![case.clone()],
                    ..definition.clone()
                };
                let relevant = baseline.target == TargetField::TaskPromptTemplate
                    && task_seed_routes(&baseline.config, &baseline.target_id)
                        .any(|(_, source)| source.source_collection == seed.collection);
                if relevant {
                    assert_eq!(
                        failed_seeds(&baseline, &single, &row.text).is_empty(),
                        seed.rendered,
                        "{}",
                        row.name
                    );
                }
                definition.cases.push(case);
            }
            let expected = &row.failed_stages;
            let failures = failed_seeds(&baseline, &definition, &row.text);
            assert_eq!(failures.len(), expected.len(), "{}: {failures:?}", row.name);
            for ((case, stage), failure) in expected.iter().zip(&failures) {
                assert!(
                    failure.contains(&format!("case {case:?}, stage {stage:?}")),
                    "{failure}"
                );
            }
            assert_eq!(
                seed_gate(&baseline, &definition, &row.text).is_ok(),
                expected.is_empty(),
                "{}",
                row.name
            );
        }
    }
}
