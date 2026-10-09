use gents::document_config::EvalCapture;
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::{CaptureResult, ScriptedExecutor};
use gents::eval::OutcomeKind;
use gents::pack::{load_pack_config, PackInstallOptions, PackManifest};
use std::path::Path;

#[test]
fn pack_cases_grade_installation_and_preservation_without_graph_execution() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/configurator_evals/packs");
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            node_did: "did:key:eval-owner".into(),
        },
        &|p| Ok(std::fs::read(root.join(p))?),
        &|_| None,
    )
    .unwrap();
    let definition = &config.eval_definitions[0];
    definition.validate().unwrap();
    let registry = CheckRegistry::builtin();
    assert_eq!(definition.cases.len(), 4);
    for case in &definition.cases {
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
                assert!(matches!(capture, EvalCapture::Documents { .. }));
                evidence.captures.insert(
                    capture.name().into(),
                    CaptureResult::Documents { rows: vec![] },
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
                    "{} / {}: {}",
                    case.case_id,
                    stage.stage_id,
                    result.raw
                );
                if check.check == "crew_spec_match" {
                    assert_ne!(
                        result.score_bp,
                        Some(10000),
                        "empty state must not pass preservation or installation"
                    );
                    assert!(check.params["present"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|p| p["capture"] == "runs" && p["max"] == 0));
                }
            }
        }
    }
}
