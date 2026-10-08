//! The configuration-only research desk capstone: a definition pack and one subject pack per
//! kickoff variant, run with `gents eval run factory-setup`. These tests keep
//! the packs loadable, the definition valid against the shipped check
//! registry, and each subject's Engineer the Engineer the desktop first run
//! creates, so a variant differs only in its `factory/` kickoff files.

use std::path::{Path, PathBuf};

use gents::document_config::{EvalDefinition, PackConfig, SurfaceToolDecl};
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::ScriptedExecutor;
use gents::eval::OutcomeKind;
use gents::pack::{declared_paths, load_pack_config, PackInstallOptions, PackManifest};

const OWNER: &str = "did:key:zFactorySetupEvalOwner";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/configurator_evals/factory_setup")
}

/// Every directory under the fixture root holding a subject pack.
fn subjects() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("manifest.json").exists() && !path.ends_with("definition"))
        .collect();
    dirs.sort();
    assert!(
        dirs.len() >= 2,
        "expected one subject pack per variant: {dirs:?}"
    );
    dirs
}

fn load(dir: &Path) -> (PackManifest, PackConfig) {
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    for path in declared_paths(&manifest) {
        assert!(
            dir.join(&path).is_file(),
            "{} declares missing {path}",
            dir.display()
        );
    }
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: OWNER.into(),
        },
        &|path| Ok(std::fs::read(dir.join(path))?),
        &|_| None,
    )
    .unwrap_or_else(|error| panic!("{}: {error:#}", dir.display()));
    (manifest, config)
}

#[test]
fn every_subject_is_the_desktop_engineer_on_an_eval_node() {
    for dir in subjects() {
        let (manifest, config) = load(&dir);
        let name = dir.display();
        let [behavior] = config.agent_behaviors.as_slice() else {
            panic!("{name}: one behavior, the Engineer");
        };
        assert_eq!(behavior.behavior_id, "engineer", "{name}");
        assert_eq!(
            config.agent_principal.default_behavior_id, None,
            "{name}: the trial selects the cell's behavior as the default"
        );
        assert!(
            behavior
                .tags
                .iter()
                .any(|tag| tag == gents::agent::persona_ops::SETUP_STEWARD_BEHAVIOR_TAG),
            "{name}: the Engineer carries the protected Setup tag"
        );
        assert_eq!(
            manifest.metadata.inference_slots.len(),
            1,
            "{name}: one slot binds the Engineer and, through its backend, the crew"
        );
        let context = &config.contexts[0];
        // A `_preview` variant appends one instruction to the shipped prompt
        // (the preview A/B); every other subject runs it unchanged.
        let prompt = context.system_prompt.as_deref().unwrap_or_default();
        if dir.to_string_lossy().ends_with("_preview") {
            assert!(
                prompt.starts_with(gents_protocol::SETUP_STEWARD_PROMPT.trim_end())
                    && prompt.trim_end().ends_with(
                        "Preview every configuration write and apply it only after the preview is clean."
                    ),
                "{name}: the shipped Setup prompt plus the preview line"
            );
        } else {
            assert_eq!(
                prompt,
                gents_protocol::SETUP_STEWARD_PROMPT,
                "{name}: engineer/system_prompt.md must equal the shipped Setup prompt"
            );
        }
        let tools = &config.tools[0];
        assert_eq!(
            tools.self_config,
            Some(gents::agent::persona_ops::setup_steward_self_config()),
            "{name}: the Setup self-config grant"
        );
        assert_eq!(
            tools.subagents.as_ref().and_then(|agents| agents.enabled),
            Some(true),
            "{name}"
        );
        let built_ins = tools.built_ins.as_ref().unwrap();
        assert_eq!(built_ins.enable_graph_tools, Some(true), "{name}");
        assert_eq!(built_ins.enable_session_history_tool, Some(true), "{name}");
        assert_eq!(built_ins.enable_schema_tool, Some(true), "{name}");
        let datastore = tools.datastore.as_ref().unwrap();
        assert_eq!(datastore.enable_defra_query, Some(true), "{name}");
        assert_eq!(
            datastore.datastore_tool_surface_ids.as_deref(),
            Some(&["engineer-mailbox".to_string()][..]),
            "{name}"
        );
        // Embedded trials refuse host bash; the Engineer configures through
        // the native config tool and reads its kickoff with file tools.
        let host = tools.host.as_ref().unwrap();
        assert_eq!(
            host.bash.as_ref().map(|bash| bash.mode),
            Some(gents::tool_surface::BashMode::Off),
            "{name}"
        );
        let [surface] = config.datastore_tool_surfaces.as_slice() else {
            panic!("{name}: one surface, engineer-mailbox");
        };
        let expected = vec![SurfaceToolDecl::Create(
            gents::mailbox::canonical_mailbox_write_decl(),
        )];
        assert_eq!(
            surface.entries.as_ref(),
            Some(&expected),
            "{name}: the canonical mailbox declaration, serialized as {}",
            serde_json::to_string(&expected).unwrap()
        );
        assert!(
            dir.join("factory/kickoff.md").is_file(),
            "{name}: every variant's kickoff is factory/kickoff.md"
        );
    }
}

pub(super) fn definition() -> EvalDefinition {
    let (_, config) = load(&root().join("definition"));
    let [definition] = config.eval_definitions.as_slice() else {
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
    let [stage, repair] = case.stages.as_slice() else {
        panic!("setup and repair stages");
    };
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
            gents::document_config::EvalTier::Development
        } else {
            gents::document_config::EvalTier::Acceptance
        }));
    assert!(repair
        .checks
        .iter()
        .all(|c| c.tier == gents::document_config::EvalTier::Acceptance));
    // Evidence with every capture empty: each check must reach a verdict about
    // the subject (or say no fire happened), never reject its own params.
    let mut evidence =
        ScriptedExecutor::passed_evidence(OWNER, &stage.stage_id, "unused", Vec::new())
            .stages
            .remove(0);
    evidence.captures.clear();
    for capture in &stage.capture {
        evidence.captures.insert(
            capture.name().to_string(),
            gents::eval::runner::CaptureResult::Documents { rows: Vec::new() },
        );
    }
    for check in stage.checks.iter().chain(&repair.checks) {
        let implementation = registry
            .get(&check.check)
            .unwrap_or_else(|| panic!("{} is not a shipped check", check.check));
        let verdict = implementation.evaluate(&check.params, &evidence);
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
    use gents::eval::runner::CaptureResult;
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
        .find(|agent| agent["behavior_id"] == "planner")
        .unwrap();
    let wiring = params["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| {
            link["id"] == "BriefRequest"
                && link["via"]
                    .as_array()
                    .is_some_and(|hops| hops.last().unwrap()["target_capture"] == "behaviors")
        })
        .unwrap();
    let permission = json!({"agents":{"behaviors":"behaviors","contexts":"contexts","tools":"tools","expect":[{
        "behavior_id":"planner","source_match":planner["source_match"],"tools":[{"field":"host.bash.mode","equals":"Off"}]
    }]},"links":[wiring]});
    let mut evidence = ScriptedExecutor::passed_evidence(OWNER, "setup", "unused", vec![])
        .stages
        .remove(0);
    evidence.captures.clear();
    for (name, rows) in [
        (
            "behaviors",
            vec![
                json!({"behavior_id":"did:x:research-desk-planner","display_name":"Research desk Planner","context_id":"c"}),
                json!({"behavior_id":"wrong","display_name":"Researcher"}),
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
            vec![json!({"task_id":"generated-task","behavior_id":"did:x:research-desk-planner"})],
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
    rows[0]["behavior_id"] = json!("wrong");
    assert!(check.evaluate(&permission, &evidence).score_bp.unwrap() < 10000);
}

#[test]
fn capstone_accepts_default_events_and_passes_keys_for_tool_backed_reads() {
    use gents::eval::runner::CaptureResult;
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
    use gents::eval::runner::CaptureResult;
    use serde_json::json;

    let definition = definition();
    let registry = CheckRegistry::builtin();
    let check = registry.get("crew_spec_match").unwrap();
    for stage in &definition.cases[0].stages {
        let captures = serde_json::to_value(&stage.capture).unwrap();
        let captures = captures.as_array().unwrap();
        let active = captures.iter().find(|c| c["name"] == "behaviors").unwrap();
        let all = captures
            .iter()
            .find(|c| c["name"] == "all_behaviors")
            .unwrap();
        assert_eq!(active["filter"]["enabled"], json!({"_eq":true}));
        assert!(all["filter"].get("enabled").is_none());
        assert_eq!(all["collection"], active["collection"]);
        assert_eq!(all["filter"]["agent_did"], active["filter"]["agent_did"]);

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
            .find(|p| p["capture"] == "all_behaviors")
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
        let planner = json!({"behavior_id":"planner-new","display_name":"Planner","enabled":true,"inference_profile_id":"profile"});
        let mut all_rows = vec![planner.clone()];
        all_rows.extend((0..6).map(|i| json!({"behavior_id":format!("other-{i}"),"enabled":true})));
        all_rows.push(json!({"behavior_id":"planner-old","display_name":"Planner","enabled":false,"inference_profile_id":"wrong-profile"}));
        let mut evidence =
            ScriptedExecutor::passed_evidence(OWNER, &stage.stage_id, "unused", vec![])
                .stages
                .remove(0);
        evidence.captures.clear();
        for (name, rows) in [
            ("behaviors", vec![planner.clone()]),
            ("all_behaviors", all_rows.clone()),
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
            .contains("all_behaviors"));

        all_rows.pop();
        evidence.captures.insert(
            "all_behaviors".into(),
            CaptureResult::Documents { rows: all_rows },
        );
        assert_eq!(check.evaluate(&subset, &evidence).score_bp, Some(10000));

        evidence.captures.insert("behaviors".into(), CaptureResult::Documents { rows: vec![planner.clone(), json!({"behavior_id":"planner-duplicate","display_name":"Planner","enabled":true,"inference_profile_id":"profile"})] });
        assert!(check.evaluate(&subset, &evidence).score_bp.unwrap() < 10000);
        evidence.captures.insert(
            "behaviors".into(),
            CaptureResult::Documents { rows: vec![] },
        );
        assert!(check.evaluate(&subset, &evidence).score_bp.unwrap() < 10000);
    }
}
