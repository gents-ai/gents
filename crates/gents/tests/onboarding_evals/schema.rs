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
    assert_eq!(definition.cases.len(), 9);
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
