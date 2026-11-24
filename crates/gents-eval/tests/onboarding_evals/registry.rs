use std::path::{Path, PathBuf};

use gents::eval::OutcomeKind;
use gents_documents::{EvalCapture, EvalDefinition};
use gents_eval::checks::CheckRegistry;
use gents_eval::runner::{CaptureResult, ScriptedExecutor};
use gents_pack::pack::core_packs::CorePack;
use gents_pack::pack::{load_pack_config, PackInstallOptions, PackManifest};

pub(super) const OWNER: &str = "did:key:eval-owner";

pub(super) fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gents")
        .join("tests/fixtures/configurator_evals")
}

pub(super) fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A `suite.json` as `scripts/evals/suites.py` reads it.
pub(super) struct Suite {
    pub(super) id: String,
    pub(super) dir: PathBuf,
}

pub(super) fn suites() -> Vec<Suite> {
    let mut found = Vec::new();
    let mut pending = vec![fixtures()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name == "suite.json") {
                let suite: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                found.push(Suite {
                    id: suite["id"].as_str().unwrap().to_owned(),
                    dir: path.parent().unwrap().to_owned(),
                });
            }
        }
    }
    found.sort_by(|a, b| a.dir.cmp(&b.dir));
    found
}

pub(super) fn definition(dir: &Path) -> EvalDefinition {
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            node_did: OWNER.into(),
        },
        &|path| Ok(std::fs::read(dir.join(path))?),
        &|_| None,
    )
    .unwrap_or_else(|error| panic!("{}: {error:#}", dir.display()));
    let [definition] = config.eval_definitions.as_slice() else {
        panic!("{}: a suite's pack carries one definition", dir.display());
    };
    definition.clone()
}

#[test]
fn every_registered_suite_validates_and_uses_shipped_checks() {
    let registry = CheckRegistry::builtin();
    let suites = suites();
    let matrix: serde_json::Value =
        serde_json::from_slice(&std::fs::read(repo().join("scripts/evals/matrix.json")).unwrap())
            .unwrap();
    for default in matrix["default"].as_array().unwrap() {
        assert!(
            suites
                .iter()
                .any(|suite| suite.id == default.as_str().unwrap()),
            "matrix.json default {default} has no suite.json"
        );
    }
    for suite in &suites {
        let definition = definition(&suite.dir);
        definition
            .validate()
            .unwrap_or_else(|error| panic!("{}: {error:#}", suite.id));
        for case in definition.cases {
            for stage in case.stages {
                let mut evidence =
                    ScriptedExecutor::passed_evidence(OWNER, &stage.stage_id, "unused", Vec::new())
                        .stages
                        .remove(0);
                evidence.subject_agent = Some(gents_eval::runner::subject_binding::SubjectAgent {
                    slot: "engineer".into(),
                    agent_did: "did:key:test-engineer".into(),
                });
                evidence.captures.clear();
                for capture in &stage.capture {
                    evidence.captures.insert(
                        capture.name().into(),
                        match capture {
                            EvalCapture::Documents { .. } => {
                                CaptureResult::Documents { rows: vec![] }
                            }
                            EvalCapture::Schema { .. } => CaptureResult::Schema {
                                collections: vec![],
                            },
                            EvalCapture::File { .. } => CaptureResult::Files {
                                files: vec![],
                                outside_workspace: vec![],
                                non_files: vec![],
                            },
                        },
                    );
                }
                for check in stage.checks {
                    let verdict = registry
                        .evaluate_bound(&check.check, &check.params, &evidence)
                        .unwrap_or_else(|| panic!("{} is not a shipped check", check.check));
                    assert_ne!(
                        verdict.kind,
                        OutcomeKind::Grader,
                        "{} / {} / {} / {}: {}",
                        suite.id,
                        case.case_id,
                        stage.stage_id,
                        check.check,
                        verdict.raw
                    );
                }
            }
        }
    }
}

/// Every suite runs the Engineer `gents init --engineer` makes, so every
/// definition installs every core pack init installs, in init's order; a
/// suite may turn on one init leaves off, and nothing else.
#[test]
fn every_suite_installs_every_core_pack_init_installs() {
    let expected: Vec<String> = CorePack::ALL
        .iter()
        .map(|pack| pack.coordinate().to_owned())
        .collect();
    let suites = suites();
    assert!(!suites.is_empty());
    for suite in suites {
        let subject = definition(&suite.dir).subject;
        assert_eq!(
            subject.core_packs, expected,
            "{}: the core packs init installs, in init's order",
            suite.id
        );
        for pack in &subject.enabled_core_packs {
            let pack = CorePack::of_coordinate(pack).unwrap();
            assert_ne!(
                gents_pack::pack::core_packs::init_wiring(pack),
                gents_pack::pack::core_packs::enabled_wiring(pack),
                "{}: {} is already on as init leaves it",
                suite.id,
                pack.coordinate()
            );
        }
    }
}
