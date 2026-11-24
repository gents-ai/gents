use std::path::{Path, PathBuf};

use gents::eval::OutcomeKind;
use gents_documents::EvalDefinition;
use gents_eval::checks::CheckRegistry;
use gents_eval::runner::ScriptedExecutor;
use gents_pack::pack::{
    declared_paths, load_pack_authoring_config, AuthoredPackConfig, PackInstallOptions,
    PackManifest,
};

const OWNER: &str = "did:key:zFactorySetupEvalOwner";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gents")
        .join("tests/fixtures/configurator_evals/factory_setup")
}

fn load(dir: &Path) -> (PackManifest, AuthoredPackConfig) {
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    for path in declared_paths(&manifest) {
        assert!(
            dir.join(&path).is_file(),
            "{} declares missing {path}",
            dir.display()
        );
    }
    let config = load_pack_authoring_config(
        &manifest,
        &PackInstallOptions {
            node_did: OWNER.into(),
        },
        &|path| Ok(std::fs::read(dir.join(path))?),
        &|_| None,
    )
    .unwrap_or_else(|error| panic!("{}: {error:#}", dir.display()));
    (manifest, config)
}

pub(super) fn definition() -> EvalDefinition {
    let (_, config) = load(&root().join("definition"));
    let [definition] = config.documents.eval_definitions.as_slice() else {
        panic!("the definition pack carries one eval definition");
    };
    definition.clone()
}

#[test]
fn the_definition_validates_and_every_check_accepts_its_params() {
    let definition = definition();
    definition.validate().unwrap();
    assert_eq!(definition.definition_id, "factory-setup");
    let registry = CheckRegistry::builtin();
    let [case] = definition.cases.as_slice() else {
        panic!("one case");
    };
    assert_eq!(case.fixtures.as_ref().unwrap().assets, ["factory"]);
    let [stage, repair, run] = case.stages.as_slice() else {
        panic!("setup, repair and run stages");
    };
    assert!(run.settle && run.continuation.is_some());
    assert!(stage.settle);
    assert!(stage.checks.iter().all(|check| matches!(
        check.check.as_str(),
        "crew_spec_match" | "tool_calls_expected" | "captured_rows_count"
    )));
    assert!(definition.subject.host_bash);
    assert!(!stage.review_previous && repair.review_previous);
    assert!(stage.continuation.is_none() && repair.continuation.is_none());
    assert!(stage.checks.iter().all(|c| c.tier
        == if c.check == "crew_spec_match" {
            gents_documents::EvalTier::Development
        } else {
            gents_documents::EvalTier::Acceptance
        }));
    assert!(repair
        .checks
        .iter()
        .all(|c| c.tier == gents_documents::EvalTier::Acceptance));
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
            capture.name().to_string(),
            gents_eval::runner::CaptureResult::Documents { rows: Vec::new() },
        );
    }
    for check in stage.checks.iter().chain(&repair.checks) {
        let verdict = registry
            .evaluate_bound(&check.check, &check.params, &evidence)
            .unwrap_or_else(|| panic!("{} is not a shipped check", check.check));
        assert_ne!(
            verdict.kind,
            OutcomeKind::Grader,
            "{} rejected its params: {}",
            check.check,
            verdict.raw
        );
    }
}

#[test]
fn capstone_role_checks_follow_generated_ids_through_automation() {
    use gents_eval::runner::CaptureResult;
    use serde_json::json;
    let definition = definition();
    let params = &definition.cases[0].stages[0]
        .checks
        .iter()
        .find(|check| check.check == "crew_spec_match")
        .unwrap()
        .params;
    let planner = params["agents"]["expect"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_did"] == "planner")
        .unwrap();
    let wiring = params["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| {
            link["id"] == "BriefRequest"
                && link["via"]
                    .as_array()
                    .is_some_and(|hops| hops.last().unwrap()["target_capture"] == "agents")
        })
        .unwrap();
    let permission = json!({"agents":{"agents":"agents","contexts":"contexts","tools":"tools","expect":[{
        "agent_did":"planner","source_match":planner["source_match"],"tools":[{"field":"host.bash.mode","equals":"Off"}]
    }]},"links":[wiring]});
    let mut evidence = ScriptedExecutor::passed_evidence(OWNER, "setup", "unused", vec![])
        .stages
        .remove(0);
    evidence.captures.clear();
    for (name, rows) in [
        (
            "agents",
            vec![
                json!({"agent_did":"did:x:research-desk-planner","display_name":"Research desk Planner","context_id":"c"}),
                json!({"agent_did":"wrong","display_name":"Researcher"}),
            ],
        ),
        ("contexts", vec![json!({"context_id":"c","tools_id":"t"})]),
        (
            "tools",
            vec![json!({"tools_id":"t","host":{"bash":{"mode":"Off"}}})],
        ),
        (
            "sources",
            vec![json!({"source_collection":"BriefRequest","event_source_id":"generated-source"})],
        ),
        (
            "triggers",
            vec![
                json!({"source":{"event_source_id":"generated-source"},"task_id":"generated-task"}),
            ],
        ),
        (
            "tasks",
            vec![json!({"task_id":"generated-task","agent_did":"did:x:research-desk-planner"})],
        ),
    ] {
        evidence
            .captures
            .insert(name.into(), CaptureResult::Documents { rows });
    }
    let registry = CheckRegistry::builtin();
    let check = registry.get("crew_spec_match").unwrap();
    assert_eq!(check.evaluate(&permission, &evidence).score_bp, Some(10000));
    let CaptureResult::Documents { rows } = evidence.captures.get_mut("tasks").unwrap() else {
        unreachable!()
    };
    rows[0]["agent_did"] = json!("wrong");
    assert!(check.evaluate(&permission, &evidence).score_bp.unwrap() < 10000);
}

#[test]
fn capstone_accepts_default_events_and_passes_keys_for_tool_backed_reads() {
    use gents_eval::runner::CaptureResult;
    use serde_json::json;
    let definition = definition();
    let params = &definition.cases[0].stages[0]
        .checks
        .iter()
        .find(|check| check.check == "crew_spec_match")
        .unwrap()
        .params;
    let rows: Vec<_> = params["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["id"] == "BriefRequest")
        .collect();
    let links: Vec<_> = params["links"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            r["id"] == "BriefRequest" && r["via"].as_array().is_none_or(|hops| hops.len() < 2)
        })
        .collect();
    let subset = json!({"rows":rows,"links":links});
    let mut evidence = ScriptedExecutor::passed_evidence(OWNER, "setup", "unused", vec![])
        .stages
        .remove(0);
    evidence.captures.clear();
    for (name, rows) in [
        (
            "sources",
            vec![
                json!({"source_collection":"BriefRequest","event_source_id":"s","event_kind":null}),
            ],
        ),
        (
            "triggers",
            vec![
                json!({"source":{"event_source_id":"s"},"task_id":"t","enabled":true,"concurrency":null}),
            ],
        ),
        (
            "tasks",
            vec![
                json!({"task_id":"t","enabled":true,"emit_outcome":true,"prompt_template":"Read the request for batch {{ doc.batch }} with your record tool."}),
            ],
        ),
    ] {
        evidence
            .captures
            .insert(name.into(), CaptureResult::Documents { rows });
    }
    let registry = CheckRegistry::builtin();
    let check = registry.get("crew_spec_match").unwrap();
    assert_eq!(check.evaluate(&subset, &evidence).score_bp, Some(10000));
    for mutation in 0..3 {
        let mut bad = evidence.clone();
        let (capture, field, value) = match mutation {
            0 => ("sources", "event_kind", json!("updated")),
            1 => ("triggers", "concurrency", json!("queued_serial")),
            _ => (
                "tasks",
                "prompt_template",
                json!("Read some request without a batch key"),
            ),
        };
        let CaptureResult::Documents { rows } = bad.captures.get_mut(capture).unwrap() else {
            unreachable!()
        };
        rows[0][field] = value;
        assert!(check.evaluate(&subset, &bad).score_bp.unwrap() < 10000);
    }
}

#[test]
fn capstone_disabled_roles_fail_cleanup_without_shadowing_active_roles() {
    use gents_eval::runner::CaptureResult;
    use serde_json::json;

    let definition = definition();
    let registry = CheckRegistry::builtin();
    let check = registry.get("crew_spec_match").unwrap();
    for stage in definition.cases[0]
        .stages
        .iter()
        .filter(|stage| stage.stage_id != "run")
    {
        let captures = serde_json::to_value(&stage.capture).unwrap();
        let captures = captures.as_array().unwrap();
        let active = captures.iter().find(|c| c["name"] == "agents").unwrap();
        let all = captures.iter().find(|c| c["name"] == "all_agents").unwrap();
        assert_eq!(active["filter"]["enabled"], json!({"_eq":true}));
        assert!(all["filter"].get("enabled").is_none());
        assert_eq!(all["collection"], active["collection"]);
        assert_eq!(all["filter"]["node_did"], active["filter"]["node_did"]);

        let params = &stage
            .checks
            .iter()
            .find(|c| c.check == "crew_spec_match")
            .unwrap()
            .params;
        let count = params["present"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["capture"] == "all_agents")
            .unwrap();
        let link = params["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| {
                l["category"] == "inference" && l["source_match"]["matches"] == "(?i)\\bplanner\\b"
            })
            .unwrap();
        let subset = json!({"present":[count],"links":[link]});
        let planner = json!({"agent_did":"planner-new","display_name":"Planner","enabled":true,"inference_profile_id":"profile"});
        let mut all_rows = vec![planner.clone()];
        all_rows.extend((0..6).map(|i| json!({"agent_did":format!("other-{i}"),"enabled":true})));
        all_rows.push(json!({"agent_did":"planner-old","display_name":"Planner","enabled":false,"inference_profile_id":"wrong-profile"}));
        let mut evidence =
            ScriptedExecutor::passed_evidence(OWNER, &stage.stage_id, "unused", vec![])
                .stages
                .remove(0);
        evidence.captures.clear();
        for (name, rows) in [
            ("agents", vec![planner.clone()]),
            ("all_agents", all_rows.clone()),
            (
                "profiles",
                vec![json!({"profile_id":"profile","reasoning_effort":"high"})],
            ),
        ] {
            evidence
                .captures
                .insert(name.into(), CaptureResult::Documents { rows });
        }
        let verdict = check.evaluate(&subset, &evidence);
        assert_eq!(verdict.raw["categories"]["inference"]["satisfied"], 2);
        assert_eq!(verdict.raw["categories"]["inference"]["total"], 2);
        assert_eq!(verdict.raw["categories"]["completeness"]["satisfied"], 0);
        assert_eq!(verdict.raw["categories"]["completeness"]["total"], 1);
        assert_eq!(verdict.raw["unmet"].as_array().unwrap().len(), 1);
        assert!(verdict.raw["unmet"][0]
            .as_str()
            .unwrap()
            .contains("all_agents"));

        all_rows.pop();
        evidence.captures.insert(
            "all_agents".into(),
            CaptureResult::Documents { rows: all_rows },
        );
        assert_eq!(check.evaluate(&subset, &evidence).score_bp, Some(10000));

        evidence.captures.insert("agents".into(), CaptureResult::Documents { rows: vec![planner.clone(), json!({"agent_did":"planner-duplicate","display_name":"Planner","enabled":true,"inference_profile_id":"profile"})] });
        assert!(check.evaluate(&subset, &evidence).score_bp.unwrap() < 10000);
        evidence
            .captures
            .insert("agents".into(), CaptureResult::Documents { rows: vec![] });
        assert!(check.evaluate(&subset, &evidence).score_bp.unwrap() < 10000);
    }
}
