use gents::document_config::EvalCapture;
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::{CaptureResult, ScriptedExecutor};
use gents::eval::OutcomeKind;
use gents::pack::{load_pack_config, PackInstallOptions, PackManifest};
use std::path::Path;

#[test]
fn schema_cases_use_valid_native_sdl_and_grade_saved_state() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/configurator_evals/schema_experience");
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: "did:key:eval-owner".into(),
        },
        &|p| Ok(std::fs::read(root.join(p))?),
        &|_| None,
    )
    .unwrap();
    let registry = CheckRegistry::builtin();
    let definition = &config.eval_definitions[0];
    definition.validate().unwrap();
    assert_eq!(definition.cases.len(), 10);
    for case in &definition.cases {
        assert!(case.stages[0]
            .capture
            .iter()
            .any(|c| matches!(c, EvalCapture::Schema { .. })));
        for stage in &case.stages {
            let mut evidence = ScriptedExecutor::passed_evidence(
                "did:key:eval-owner",
                &stage.stage_id,
                "unused",
                vec![],
            )
            .stages
            .remove(0);
            for capture in &stage.capture {
                evidence.captures.insert(
                    capture.name().into(),
                    match capture {
                        EvalCapture::Schema { .. } => CaptureResult::Schema {
                            collections: vec![],
                        },
                        EvalCapture::Documents { .. } => CaptureResult::Documents { rows: vec![] },
                        EvalCapture::File { .. } => panic!("unexpected file capture"),
                    },
                );
            }
            for check in &stage.checks {
                let owner = registry.get(&check.check).unwrap();
                jsonschema::validator_for(&owner.describe().params_schema)
                    .unwrap()
                    .validate(&check.params)
                    .unwrap();
                let result = owner.evaluate(&check.params, &evidence);
                assert_ne!(
                    result.kind,
                    OutcomeKind::Grader,
                    "{} / {} / {}: {}",
                    case.case_id,
                    stage.stage_id,
                    check.check,
                    result.raw
                );
                if check.check == "schema_matches" {
                    assert_eq!(result.score_bp, Some(0), "a missing schema must not pass");
                }
            }
        }
    }
}

/// Historical fixture premises come from NHHC biographies, paraphrased:
/// <https://www.history.navy.mil/browse-by-topic/people/chiefs-of-naval-operations/leahy.html>
/// <https://www.history.navy.mil/research/library/biographical-files/modern-biographical-files-ndl/modern-bios-h/halsey-william-f.html>
/// <https://www.history.navy.mil/browse-by-topic/people/chiefs-of-naval-operations/fleet-admiral-chester-w--nimitz.html>
/// <https://www.history.navy.mil/content/history/nhhc/browse-by-topic/people/chiefs-of-naval-operations/fleet-admiral-ernest-j--king.html>
/// <https://www.history.navy.mil/research/library/biographical-files/modern-biographical-files-ndl/modern-bios-s/spruance-raymond-a.html>
#[tokio::test]
async fn admiral_search_fixture_grades_native_writes_and_ranked_results() {
    use gents::application_write::{WriteParams, WriteTool};
    use gents::config_client::ConfigAccess;
    use gents::defra_query::{execute_command, CollectionScope, QueryParams};
    use gents::eval::runner::embedded::observe::ToolCallEvidence;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    let case: Value = serde_json::from_str(include_str!(
        "../fixtures/configurator_evals/schema_experience/cases/admiral_report_search.json"
    ))
    .unwrap();
    let registry = CheckRegistry::builtin();
    let node = Arc::new(
        gents::defra_node::EmbeddedNode::builder()
            .build()
            .await
            .unwrap(),
    );
    let access = ConfigAccess::Local(node.clone());
    let configure = &case["stages"][0];
    let schema_params = &configure["checks"][0]["params"];
    access
        .add_schema(schema_params["sdl"].as_str().unwrap())
        .await
        .unwrap();
    let mut schema_evidence =
        ScriptedExecutor::passed_evidence("did:test", "configure", "unused", vec![])
            .stages
            .remove(0);
    let collections = access.collection_versions().await.unwrap();
    schema_evidence.captures.insert(
        "schema".into(),
        CaptureResult::Schema {
            collections: collections.clone(),
        },
    );
    let schema_grader = registry.get("schema_matches").unwrap();
    assert_eq!(
        schema_grader
            .evaluate(schema_params, &schema_evidence)
            .score_bp,
        Some(10000)
    );
    let mut unindexed = collections;
    for collection in &mut unindexed {
        collection["FullTextIndexes"] = json!([]);
    }
    schema_evidence.captures.insert(
        "schema".into(),
        CaptureResult::Schema {
            collections: unindexed,
        },
    );
    assert_ne!(
        schema_grader
            .evaluate(schema_params, &schema_evidence)
            .score_bp,
        Some(10000)
    );

    let populate = &case["stages"][1];
    let writer = WriteTool::new(
        access.clone(),
        BTreeSet::from(["AdmiralReport".into()]),
        None,
    );
    for expected in populate["checks"][0]["params"]["rows"].as_array().unwrap() {
        let mut row = json!({"name":expected["id"]});
        for field in expected["expect"].as_array().unwrap() {
            row[field["field"].as_str().unwrap()] = field["equals"].clone();
        }
        for value in row.as_object().unwrap().values() {
            assert!(
                populate["prompt"]
                    .as_str()
                    .unwrap()
                    .contains(value.as_str().unwrap()),
                "the model must receive every persisted value the fixture grades"
            );
        }
        let preview = writer
            .execute(
                &serde_json::from_value::<WriteParams>(json!({
                    "argv":["preview","create"],"collection":"AdmiralReport","options":{"input":row}
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        writer
            .execute(&serde_json::from_value(preview["next_call"]["args"].clone()).unwrap())
            .await
            .unwrap();
    }
    let saved = access
        .execute("{ AdmiralReport { name rank duty } }")
        .await
        .unwrap();
    let mut populated = ScriptedExecutor::passed_evidence("did:test", "populate", "unused", vec![])
        .stages
        .remove(0);
    populated.captures.insert(
        "reports".into(),
        CaptureResult::Documents {
            rows: saved["data"]["AdmiralReport"].as_array().unwrap().clone(),
        },
    );
    for check in populate["checks"].as_array().unwrap() {
        let result = registry
            .get(check["check"].as_str().unwrap())
            .unwrap()
            .evaluate(&check["params"], &populated);
        assert_eq!(result.score_bp, Some(10000), "{}", result.raw);
    }

    let args = json!({"argv":["search"],"collection":"AdmiralReport","options":{
        "fields":["name","rank","duty"],"search_fields":["duty"],"text":"antisubmarine U-boats","limit":1
    }});
    let result = execute_command(
        &access,
        &serde_json::from_value::<QueryParams>(args.clone()).unwrap(),
        &CollectionScope::all(),
    )
    .await
    .unwrap();
    assert!(result["results"][0]["_score"].as_f64().unwrap() > 0.0);
    let mut evidence = ScriptedExecutor::passed_evidence("did:test", "search", "unused", vec![])
        .stages
        .remove(0);
    evidence.tool_calls = vec![ToolCallEvidence {
        tool_name: "query".into(),
        status: Some("completed".into()),
        lifecycle_state: Some("completed".into()),
        tool_failure_class: None,
        started_at: None,
        completed_at: None,
        args: args.clone(),
        result: result.clone(),
    }];
    let grader = registry.get("tool_result_matches").unwrap();
    let params = &case["stages"][2]["checks"][0]["params"];
    assert_eq!(grader.evaluate(params, &evidence).score_bp, Some(10000));

    let mut ordinary_read = result.clone();
    ordinary_read.as_object_mut().unwrap().remove("ranking");
    evidence.tool_calls[0].result = ordinary_read;
    assert_ne!(
        grader.evaluate(params, &evidence).score_bp,
        Some(10000),
        "an ordinary read is not a ranked search"
    );
    let mut other_query = args;
    other_query["options"]["text"] = json!("Midway Fifth Fleet");
    evidence.tool_calls[0].result = execute_command(
        &access,
        &serde_json::from_value(other_query).unwrap(),
        &CollectionScope::all(),
    )
    .await
    .unwrap();
    assert_eq!(
        evidence.tool_calls[0].result["results"][0]["name"],
        "Raymond A. Spruance"
    );
    assert_ne!(
        grader.evaluate(params, &evidence).score_bp,
        Some(10000),
        "an unrelated top result must fail"
    );
    evidence.tool_calls[0].result = result;
    evidence.tool_calls[0].status = Some("failed".into());
    evidence.tool_calls[0].lifecycle_state = Some("failed".into());
    assert_eq!(grader.evaluate(params, &evidence).score_bp, Some(0));
    evidence.tool_calls.clear();
    assert_eq!(grader.evaluate(params, &evidence).score_bp, Some(0));
    node.shutdown().await;
}
