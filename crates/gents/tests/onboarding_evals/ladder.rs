use std::path::{Path, PathBuf};

use gents::document_config::{EvalCapture, EvalDefinition};
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::{CaptureResult, ScriptedExecutor};
use gents::eval::OutcomeKind;
use gents::pack::{load_pack_config, PackInstallOptions, PackManifest};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/configurator_evals/ladder")
}

fn definitions() -> Vec<EvalDefinition> {
    let mut definitions = Vec::new();
    for entry in std::fs::read_dir(root()).unwrap() {
        let dir = entry.unwrap().path();
        if !dir.file_name().unwrap().to_string_lossy().starts_with('l') {
            continue;
        }
        let manifest: PackManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let config = load_pack_config(
            &manifest,
            &PackInstallOptions {
                agent_did: "did:key:eval-owner".into(),
            },
            &|path| Ok(std::fs::read(dir.join(path))?),
            &|_| None,
        )
        .unwrap_or_else(|error| panic!("{}: {error:#}", dir.display()));
        definitions.extend(config.eval_definitions);
    }
    definitions
}

#[test]
fn every_native_case_validates_and_uses_shipped_checks() {
    let registry = CheckRegistry::builtin();
    let definitions = definitions();
    assert_eq!(definitions.len(), 6);
    let dir = root().parent().unwrap().join("engineer_datastore");
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    let application = load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: "did:key:eval-owner".into(),
        },
        &|path| Ok(std::fs::read(dir.join(path))?),
        &|_| None,
    )
    .unwrap();
    for definition in definitions.into_iter().chain(application.eval_definitions) {
        definition.validate().unwrap();
        for case in definition.cases {
            for stage in case.stages {
                let mut evidence = ScriptedExecutor::passed_evidence(
                    "did:key:eval-owner",
                    &stage.stage_id,
                    "unused",
                    Vec::new(),
                )
                .stages
                .remove(0);
                evidence.captures.clear();
                for capture in &stage.capture {
                    evidence.captures.insert(
                        capture.name().into(),
                        match capture {
                            EvalCapture::Documents { .. } => {
                                CaptureResult::Documents { rows: vec![] }
                            }
                            EvalCapture::File { .. } => CaptureResult::Files {
                                files: vec![],
                                outside_workspace: vec![],
                            },
                        },
                    );
                }
                for check in stage.checks {
                    let verdict = registry
                        .get(&check.check)
                        .unwrap()
                        .evaluate(&check.params, &evidence);
                    assert_ne!(
                        verdict.kind,
                        OutcomeKind::Grader,
                        "{} / {} / {}: {}",
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

#[test]
fn l2_checks_follow_selected_context_and_tools_and_reject_a_wrong_grant() {
    let definition = definitions()
        .into_iter()
        .find(|d| d.definition_id == "configurator-l2-agent")
        .unwrap();
    let case = definition
        .cases
        .iter()
        .find(|c| c.case_id == "analyst-grants-file-tools")
        .unwrap();
    let stage = &case.stages[0];
    let check = stage
        .checks
        .iter()
        .find(|c| c.check == "crew_spec_match")
        .unwrap();
    let mut evidence =
        ScriptedExecutor::passed_evidence("did:key:eval-owner", "configure", "unused", vec![])
            .stages
            .remove(0);
    let subject: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("engineer_subject/pack_config.json")).unwrap(),
    )
    .unwrap();
    let instructions = "You are Analyst, a read-only file assistant for research notes. Read and report; never modify files.";
    evidence.captures.insert("behaviors".into(), CaptureResult::Documents { rows: vec![
        serde_json::json!({"behavior_id":"scope:analyst", "display_name":"analyst", "enabled":true, "context_id":"analyst-context"}),
        serde_json::json!({"behavior_id":"scope:engineer", "display_name":"The Engineer", "context_id":"engineer-context"}),
    ]});
    evidence.captures.insert("contexts".into(), CaptureResult::Documents { rows: vec![
        serde_json::json!({"context_id":"analyst-context", "tools_id":"analyst-tools", "system_prompt": instructions}),
        serde_json::json!({"context_id":"engineer-context", "tools_id":"engineer-tools", "system_prompt": gents_protocol::SETUP_STEWARD_PROMPT}),
    ]});
    evidence.captures.insert("tools".into(), CaptureResult::Documents { rows: vec![
        serde_json::json!({"tools_id":"analyst-tools", "host":{"files":{"mode":"ReadOnly"},"bash":{"mode":"Off"}}}),
        serde_json::to_value(serde_json::from_value::<gents::document_config::Tools>(serde_json::json!({
            "agent_did": "did:key:eval-owner",
            "tools_id": "engineer-tools",
            "host": {"bash": {"mode":"Off"}, "root":"/eval/workspace"},
            "self_config": subject["tools"][0]["self_config"],
        })).unwrap()).unwrap(),
    ]});
    let registry = CheckRegistry::builtin();
    let grader = registry.get("crew_spec_match").unwrap();
    let good = grader.evaluate(&check.params, &evidence);
    assert_eq!(good.score_bp, Some(10000), "{}", good.raw);
    let CaptureResult::Documents { rows } = evidence.captures.get_mut("tools").unwrap() else {
        unreachable!()
    };
    rows[0]["host"]["files"]["mode"] = "ReadWrite".into();
    let bad = grader.evaluate(&check.params, &evidence);
    assert!(bad.score_bp.unwrap() < 10000, "{}", bad.raw);
    let CaptureResult::Documents { rows } = evidence.captures.get_mut("contexts").unwrap() else {
        unreachable!()
    };
    rows[0]["tools_id"] = "engineer-tools".into();
    let wrong_selection = grader.evaluate(&check.params, &evidence);
    assert!(
        wrong_selection.score_bp.unwrap() < good.score_bp.unwrap(),
        "{}",
        wrong_selection.raw
    );
}

#[test]
fn delegation_requires_delivery_of_the_matching_child_reply() {
    let definition = definitions()
        .into_iter()
        .find(|d| d.definition_id == "configurator-l5-agents-tools")
        .unwrap();
    let stage = definition
        .cases
        .iter()
        .find(|c| c.split == gents::document_config::EvalSplit::Train)
        .unwrap()
        .stages
        .iter()
        .find(|s| s.stage_id == "delegate")
        .unwrap();
    let check = stage
        .checks
        .iter()
        .find(|c| c.check == "crew_spec_match")
        .unwrap();
    let mut evidence =
        ScriptedExecutor::passed_evidence("did:key:eval-owner", "delegate", "TRAIN-111", vec![])
            .stages
            .remove(0);
    evidence.captures.insert("helper-request".into(), CaptureResult::Documents { rows: vec![
        serde_json::json!({"behavior_id":"scope:research-helper", "caused_by_parent_tool_call_doc_id":"matching-call"})
    ]});
    let delivered = serde_json::json!({"_docID":"matching-call", "lifecycle_state":"completed", "completion_notification_delivered_at":"2026-09-30T00:00:00Z"});
    evidence.captures.insert(
        "delegation_calls".into(),
        CaptureResult::Documents {
            rows: vec![delivered.clone()],
        },
    );
    let registry = CheckRegistry::builtin();
    let grader = registry.get("crew_spec_match").unwrap();
    assert_eq!(
        grader.evaluate(&check.params, &evidence).score_bp,
        Some(10000)
    );
    for (field, value) in [
        (
            "completion_notification_delivered_at",
            serde_json::Value::Null,
        ),
        ("_docID", serde_json::json!("unrelated-call")),
    ] {
        let mut row = delivered.clone();
        row[field] = value;
        evidence.captures.insert(
            "delegation_calls".into(),
            CaptureResult::Documents { rows: vec![row] },
        );
        let verdict = grader.evaluate(&check.params, &evidence);
        assert!(verdict.score_bp.unwrap() < 10000, "{}", verdict.raw);
    }
}

#[test]
fn runtime_captures_use_current_schema_fields() {
    use std::collections::{BTreeMap, BTreeSet};
    let types = regex::Regex::new(r"type\s+(\w+)[^{]*\{([^}]+)\}").unwrap();
    let fields = regex::Regex::new(r"(?m)^\s*(\w+)\s*:").unwrap();
    let mut schemas = BTreeMap::new();
    for sdl in gents_protocol::schemas::ALL
        .iter()
        .chain(gents_protocol::schemas::RUNTIME_ALL)
    {
        let sdl = sdl
            .lines()
            .map(|line| line.split('#').next().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        for definition in types.captures_iter(&sdl) {
            let mut names = fields
                .captures_iter(&definition[2])
                .map(|field| field[1].to_string())
                .collect::<BTreeSet<_>>();
            names.insert("_docID".into());
            schemas.insert(definition[1].to_string(), names);
        }
    }
    for definition in definitions()
        .into_iter()
        .chain([super::factory_setup::definition()])
    {
        for case in definition.cases {
            for stage in case.stages {
                for capture in stage.capture {
                    if let EvalCapture::Documents {
                        collection, fields, ..
                    } = capture
                    {
                        if let Some(known) = schemas.get(&collection) {
                            for field in fields {
                                assert!(
                                    known.contains(&field),
                                    "{} / {}: {collection}.{field} is not in the canonical schema",
                                    case.case_id,
                                    stage.stage_id
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn streaming_discovery_checks_streams_and_waits_without_budget_keywords() {
    use gents::eval::runner::embedded::observe::MessageEvidence;
    let definition = definitions()
        .into_iter()
        .find(|d| d.definition_id == "configurator-l1-inference")
        .unwrap();
    let stage = &definition
        .cases
        .iter()
        .find(|c| c.case_id == "val-stream-settings")
        .unwrap()
        .stages[0];
    let check = stage
        .checks
        .iter()
        .find(|c| c.check == "final_message_matches")
        .unwrap();
    let registry = CheckRegistry::builtin();
    for (message, expected) in [("Stream batches persist every second. Provider silence times out after five minutes; reducing the batching interval may make updates smoother.",10000),("I recommend changing the budget.",0)] {
        let mut evidence = ScriptedExecutor::passed_evidence("did:x","discover","unused",vec![]).stages.remove(0);
        evidence.messages = vec![MessageEvidence { role: "assistant".into(), content: message.into(), created_at: None }];
        assert_eq!(registry.get(&check.check).unwrap().evaluate(&check.params,&evidence).score_bp,Some(expected));
    }
}

#[test]
fn parallel_automation_accepts_default_concurrency_and_still_checks_its_task() {
    let definition = definitions()
        .into_iter()
        .find(|d| d.definition_id == "configurator-l4-automation")
        .unwrap();
    let configured = &definition
        .cases
        .iter()
        .find(|c| c.split == gents::document_config::EvalSplit::Train)
        .unwrap()
        .stages[0];
    let registry = CheckRegistry::builtin();
    for (concurrency, task, accepted) in [
        (serde_json::Value::Null, "task", true),
        (serde_json::json!("parallel"), "task", true),
        (serde_json::json!("serial"), "task", false),
        (serde_json::Value::Null, "missing", false),
    ] {
        let mut evidence =
            ScriptedExecutor::passed_evidence("did:x", "configure", "unused", vec![])
                .stages
                .remove(0);
        evidence.captures.insert("trigger_config".into(), CaptureResult::Documents { rows:vec![serde_json::json!({"concurrency":concurrency,"task_id":task,"enabled":true,"source":{"event_source_id":"source"}})] });
        evidence.captures.insert("task_config".into(), CaptureResult::Documents { rows:vec![serde_json::json!({"task_id":"task","behavior_id":"engineer","emit_outcome":true,"enabled":true})] });
        evidence.captures.insert("eventsource_config".into(), CaptureResult::Documents { rows:vec![serde_json::json!({"event_source_id":"source","source_collection":"TrainPing","event_kind":null})] });
        let all_pass = configured
            .checks
            .iter()
            .filter(|c| {
                matches!(
                    c.check.as_str(),
                    "captured_fields_match" | "crew_spec_match"
                )
            })
            .all(|c| {
                registry
                    .get(&c.check)
                    .unwrap()
                    .evaluate(&c.params, &evidence)
                    .kind
                    == OutcomeKind::Passed
            });
        assert_eq!(all_pass, accepted);
    }
}

#[test]
fn seeded_outcome_cases_supply_the_source_handoff_contract() {
    let types = regex::Regex::new(r"type\s+(\w+)[^{]*\{([^}]+)\}").unwrap();
    let handoff = regex::Regex::new(r"\bhandoff_id\s*:\s*String\b").unwrap();
    let mut covered_clerk = false;
    for definition in definitions() {
        for case in definition.cases {
            for stage in &case.stages {
                let Some(seed) = &stage.seed else { continue };
                if !stage.capture.iter().any(|capture| {
                    matches!(capture,
                        EvalCapture::Documents { collection, .. } if collection == "FireOutcome"
                    )
                }) {
                    continue;
                }
                assert!(
                    seed.document
                        .get("handoff_id")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| !id.is_empty()),
                    "{} / {}: outcome input needs a nonempty handoff_id",
                    case.case_id,
                    stage.stage_id
                );
                assert!(
                    case.fixtures.as_ref().is_some_and(|fixtures| {
                        fixtures.schemas.iter().any(|sdl| {
                            types.captures_iter(sdl).any(|schema| {
                                schema[1] == seed.collection && handoff.is_match(&schema[2])
                            })
                        })
                    }),
                    "{}: {} must declare handoff_id as String",
                    case.case_id,
                    seed.collection
                );
                covered_clerk |= case.case_id == "clerk-automation-seed-fire";
            }
        }
    }
    assert!(covered_clerk, "the Clerk outcome fixture remains covered");
}

#[test]
fn repointed_delegation_grades_the_named_role_and_its_delivered_reply() {
    use serde_json::json;
    let definition = definitions()
        .into_iter()
        .find(|d| d.definition_id == "configurator-l5-agents-tools")
        .unwrap();
    let case = definition
        .cases
        .iter()
        .find(|c| c.case_id == "val-repoint-target-in-place")
        .unwrap();
    let check = |index: usize| {
        case.stages[index]
            .checks
            .iter()
            .find(|c| c.check == "crew_spec_match")
            .unwrap()
    };
    let mut evidence = ScriptedExecutor::passed_evidence("did:x", "repoint", "unused", vec![])
        .stages
        .remove(0);
    for (name, rows) in [
        (
            "behaviors",
            vec![
                json!({"behavior_id":"engineer","context_id":"context"}),
                json!({"behavior_id":"did:x:the-specialist","display_name":"The Specialist"}),
            ],
        ),
        (
            "contexts",
            vec![json!({"context_id":"context","tools_id":"tools"})],
        ),
        (
            "tools",
            vec![json!({"tools_id":"tools","subagents":{"enabled":true,"target_ids":["helper"]}})],
        ),
        (
            "targets",
            vec![
                json!({"target_id":"helper","agent_did":"did:x","target_agent_did":"did:x","behavior_id":"did:x:the-specialist"}),
            ],
        ),
        (
            "role_behaviors",
            vec![json!({"behavior_id":"did:x:the-specialist","display_name":"The Specialist"})],
        ),
        (
            "repointed-request",
            vec![
                json!({"behavior_id":"did:x:the-specialist","content":"Reply REP-555","caused_by_parent_tool_call_doc_id":"call"}),
            ],
        ),
        (
            "delegation_calls",
            vec![
                json!({"_docID":"call","lifecycle_state":"completed","completion_notification_delivered_at":"2026-10-01T00:00:00Z"}),
            ],
        ),
    ] {
        evidence
            .captures
            .insert(name.into(), CaptureResult::Documents { rows });
    }
    let registry = CheckRegistry::builtin();
    let grader = registry.get("crew_spec_match").unwrap();
    for index in [1, 2] {
        let verdict = grader.evaluate(&check(index).params, &evidence);
        assert_eq!(verdict.score_bp, Some(10000), "{}", verdict.raw);
    }
    for (capture, field, wrong, index) in [
        ("targets", "target_agent_did", json!("did:foreign"), 1),
        ("role_behaviors", "display_name", json!("Publisher"), 2),
        ("repointed-request", "behavior_id", json!("other"), 2),
        (
            "delegation_calls",
            "completion_notification_delivered_at",
            serde_json::Value::Null,
            2,
        ),
    ] {
        let mut bad = evidence.clone();
        let CaptureResult::Documents { rows } = bad.captures.get_mut(capture).unwrap() else {
            unreachable!()
        };
        rows[0][field] = wrong;
        let verdict = grader.evaluate(&check(index).params, &bad);
        assert!(
            verdict.score_bp.unwrap() < 10000,
            "{capture}.{field}: {}",
            verdict.raw
        );
    }
    let CaptureResult::Documents { rows } = evidence.captures.get_mut("repointed-request").unwrap()
    else {
        unreachable!()
    };
    rows.push(rows[0].clone());
    let verdict = grader.evaluate(&check(2).params, &evidence);
    assert!(verdict.score_bp.unwrap() < 10000, "{}", verdict.raw);
}

#[test]
fn delegation_execution_allows_inspection_but_rejects_config_mutations() {
    let definition = definitions()
        .into_iter()
        .find(|d| d.definition_id == "configurator-l5-agents-tools")
        .unwrap();
    let registry = CheckRegistry::builtin();
    let grader = registry.get("tool_calls_expected").unwrap();
    let mut checked = 0;
    for case in definition.cases {
        for stage in case.stages {
            for check in stage.checks.iter().filter(|check| {
                check.check == "tool_calls_expected" && check.params["config_read_only"] == true
            }) {
                checked += 1;
                let mut evidence = ScriptedExecutor::passed_evidence(
                    "did:key:eval-owner",
                    &stage.stage_id,
                    "reply",
                    vec![],
                )
                .stages
                .remove(0);
                evidence.tool_calls = check.params["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|name| {
                        serde_json::from_value(serde_json::json!({
                            "tool_name": name, "status": "completed", "args": {}, "result": {}
                        }))
                        .unwrap()
                    })
                    .collect();
                assert_eq!(
                    grader.evaluate(&check.params, &evidence).score_bp,
                    Some(10000)
                );
                evidence.tool_calls.push(
                    serde_json::from_value(serde_json::json!({
                        "tool_name": "config", "status": "completed",
                        "args": {"argv": ["tools", "get"]},
                        "result": {"config_execution": {"version": 1, "mutation_entered": false}}
                    }))
                    .unwrap(),
                );
                assert_eq!(
                    grader.evaluate(&check.params, &evidence).score_bp,
                    Some(10000)
                );
                evidence.tool_calls.last_mut().unwrap().result["config_execution"]
                    ["mutation_entered"] = true.into();
                assert!(grader.evaluate(&check.params, &evidence).score_bp.unwrap() < 10000);
                evidence.tool_calls.last_mut().unwrap().result = serde_json::Value::Null;
                assert!(grader.evaluate(&check.params, &evidence).score_bp.unwrap() < 10000);
            }
        }
    }
    assert_eq!(checked, 7);
}

#[test]
fn automation_outcomes_accept_request_and_goal_success_but_reject_other_states() {
    use gents::goal::GoalStatus;
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    use serde_json::json;

    let definition = definitions()
        .into_iter()
        .find(|d| d.definition_id == "configurator-l4-automation")
        .unwrap();
    assert_eq!(definition.comparability_version, 10);
    let registry = CheckRegistry::builtin();
    let grader = registry.get("crew_spec_match").unwrap();
    let mut checked = 0;
    for case in definition.cases {
        for stage in case.stages {
            for check in stage.checks {
                let Some(rows) = check.params.get("rows").and_then(|rows| rows.as_array()) else {
                    continue;
                };
                for row in rows.iter().filter(|row| row["capture"] == "fire_outcomes") {
                    checked += 1;
                    for (state, success) in [
                        (RequestLifecycleState::Completed.as_str(), true),
                        (GoalStatus::Complete.as_str(), true),
                        (GoalStatus::Active.as_str(), false),
                        (GoalStatus::Blocked.as_str(), false),
                        (GoalStatus::BudgetLimited.as_str(), false),
                        (RequestLifecycleState::Failed.as_str(), false),
                        ("incomplete", false),
                    ] {
                        let mut evidence = ScriptedExecutor::passed_evidence(
                            "did:key:eval-owner",
                            &stage.stage_id,
                            "reply",
                            vec![],
                        )
                        .stages
                        .remove(0);
                        let mut outcome = json!({"terminal_state":state});
                        outcome[row["key"].as_str().unwrap()] = row["id"].clone();
                        for expected in row["expect"].as_array().unwrap() {
                            if expected["field"] != "terminal_state" {
                                outcome[expected["field"].as_str().unwrap()] =
                                    expected["equals"].clone();
                            }
                        }
                        evidence.captures.insert(
                            "fire_outcomes".into(),
                            CaptureResult::Documents {
                                rows: vec![outcome],
                            },
                        );
                        let verdict = grader.evaluate(&json!({"rows":[row]}), &evidence);
                        assert_eq!(
                            verdict.score_bp == Some(10000),
                            success,
                            "{} / {} / {state}: {}",
                            case.case_id,
                            stage.stage_id,
                            verdict.raw
                        );
                    }
                }
            }
        }
    }
    assert_eq!(checked, 10);
}
