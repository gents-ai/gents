use super::*;
use crate::eval::runner::scripted::ScriptedExecutor;
use crate::eval::OutcomeKind;

fn stage(captures: &[(&str, Vec<Value>)]) -> StageEvidence {
    let mut stage = ScriptedExecutor::passed_evidence("did:x", "s1", "unused", Vec::new())
        .stages
        .remove(0);
    stage.captures.clear();
    for (name, rows) in captures {
        stage.captures.insert(
            (*name).to_string(),
            CaptureResult::Documents { rows: rows.clone() },
        );
    }
    stage
}

fn home() -> StageEvidence {
    stage(&[
        (
            "agents",
            vec![
                json!({"agent_id": "worker-a", "context_id": "c1", "inference_profile_id": "glm-a"}),
            ],
        ),
        (
            "contexts",
            vec![
                json!({"context_id": "c1", "tools_id": "t1", "system_prompt": "You are a GLM worker"}),
            ],
        ),
        (
            "tools",
            vec![
                json!({"tools_id": "t1", "host": "{\"files\":{\"mode\":\"ReadWrite\"}}", "agents": {"enabled": false}}),
            ],
        ),
        (
            "tasks",
            vec![
                json!({"task_id": "work-a", "emit_outcome": true, "prompt_template": "Shard {{ doc.shard_id }} {{doc.owner}}"}),
            ],
        ),
        ("c_assignment", vec![]),
        (
            "mailbox",
            vec![json!({"title": "Setup receipt", "payload": "worker-a work-a"})],
        ),
    ])
}

fn params() -> Value {
    json!({
        "present": [{"capture": "c_assignment", "category": "collections"}, {"capture": "c_result", "category": "collections"}],
        "rows": [{"capture": "tasks", "key": "task_id", "id": "work-a", "category": "emit_outcome",
                  "expect": [{"field": "emit_outcome", "equals": true}]}],
        "agents": {"agents": "agents", "contexts": "contexts", "tools": "tools", "expect": [
            {"agent_id": "worker-a", "category": "least_privilege",
             "agent": [{"field": "inference_profile_id", "equals": "glm-a"}],
             "context": [{"field": "system_prompt", "contains": "GLM worker"}],
             "tools": [{"field": "host.files.mode", "equals": "ReadWrite"}, {"field": "agents.enabled", "equals": false}]}
        ]},
        "templates": [{"capture": "tasks", "key": "task_id", "id": "work-a", "fields": ["prompt_template"], "allowed": ["shard_id"]}],
        "receipt": {"capture": "mailbox", "fields": ["title", "payload"], "ids": ["worker-a", "work-a", "work-b"]}
    })
}

#[test]
fn multiple_hops_require_the_connected_limit_and_the_reported_model() {
    let params = json!({"links":[
        {"capture":"agents","key":"agent_id","id":"engineer","field":"inference_profile_id","target_capture":"profiles","target_key":"profile_id","reported_fields":["model_name"]},
        {"capture":"agents","key":"agent_id","id":"engineer","field":"inference_profile_id","target_capture":"profiles","target_key":"profile_id","via":[{"field":"execution_id","target_capture":"executions","target_key":"execution_id"}],"expect":[{"field":"max_turns","equals":30}]}
    ]});
    assert!(
        jsonschema::validator_for(&CrewSpecMatch.describe().params_schema)
            .unwrap()
            .is_valid(&params)
    );
    let make = |execution: &str, report: &str| {
        let mut e = stage(&[
            (
                "agents",
                vec![json!({"agent_id":"engineer","inference_profile_id":"chosen"})],
            ),
            (
                "profiles",
                vec![
                    json!({"profile_id":"chosen","model_name":"actual-model","execution_id":execution}),
                ],
            ),
            (
                "executions",
                vec![
                    json!({"execution_id":"good","max_turns":30}),
                    json!({"execution_id":"wrong","max_turns":2}),
                ],
            ),
        ]);
        e.messages = vec![crate::eval::runner::embedded::MessageEvidence {
            role: "assistant".into(),
            content: report.into(),
            created_at: None,
        }];
        e
    };
    assert_eq!(
        CrewSpecMatch
            .evaluate(&params, &make("good", "We use ACTUAL-MODEL."))
            .score_bp,
        Some(10000)
    );
    for (exec, report) in [
        ("wrong", "actual-model"),
        ("missing", "actual-model"),
        ("good", "another-model"),
    ] {
        let v = CrewSpecMatch.evaluate(&params, &make(exec, report));
        assert!(v.score_bp.unwrap() < 10000, "{}", v.raw);
    }
}

#[test]
fn reverse_reference_hops_do_not_accept_a_disconnected_task() {
    let params = json!({"links":[{"capture":"sources","key":"source_collection","id":"Note","field":"event_source_id","target_capture":"triggers","target_key":"source.event_source_id","via":[{"field":"task_id","target_capture":"tasks","target_key":"task_id"}],"expect":[{"field":"agent_id","equals":"reviewer"}]}]});
    for (bound, score) in [("wanted", 10000), ("decoy", 5000)] {
        let e = stage(&[
            (
                "sources",
                vec![json!({"source_collection":"Note","event_source_id":"watch"})],
            ),
            (
                "triggers",
                vec![json!({"source": "{\"event_source_id\":\"watch\"}","task_id":bound})],
            ),
            (
                "tasks",
                vec![
                    json!({"task_id":"wanted","agent_id":"reviewer"}),
                    json!({"task_id":"decoy","agent_id":"writer"}),
                ],
            ),
        ]);
        assert_eq!(CrewSpecMatch.evaluate(&params, &e).score_bp, Some(score));
    }
}

#[test]
fn completion_record_requires_a_write_path_for_its_metadata() {
    let params = json!({"agents":{"agents":"agents","contexts":"contexts","tools":"tools","expect":[{"agent_id":"worker-a","datastore":{"surfaces":"surfaces","create":["Result"],"query":[],"required_fields":{"Result":["handoff_id"]}}}]}});
    assert!(
        jsonschema::validator_for(&CrewSpecMatch.describe().params_schema)
            .unwrap()
            .is_valid(&params)
    );
    for (field, met) in [
        (None, false),
        (Some(json!({"name":"handoff_id"})), false),
        (Some(json!({"name":"handoff_id","required":true})), true),
        (
            Some(json!({"name":"handoff_id","fill":"correlation"})),
            true,
        ),
    ] {
        let mut evidence = home();
        evidence.captures.insert(
            "tools".into(),
            CaptureResult::Documents {
                rows: vec![
                    json!({"tools_id":"t1","datastore":{"datastore_tool_surface_ids":["results"]}}),
                ],
            },
        );
        let mut fields = vec![json!({"name":"text","required":true})];
        fields.extend(field);
        evidence.captures.insert("surfaces".into(), CaptureResult::Documents { rows: vec![json!({"surface_id":"results","node_did":"did:x","enabled":true,"entries":[{"tool_name":"write_result","collection":"Result","description":"Record the result","fields":fields}]})] });
        let verdict = CrewSpecMatch.evaluate(&params, &evidence);
        assert_eq!(verdict.score_bp == Some(10000), met, "{}", verdict.raw);
        if !met {
            assert!(verdict
                .feedback
                .unwrap()
                .contains("write_result must write handoff_id"));
        }
    }
}

#[test]
fn multi_hop_feedback_names_the_field_owner() {
    let params = json!({"links":[{"capture":"sources","key":"source_collection","id":"Note","field":"event_source_id","target_capture":"triggers","target_key":"source.event_source_id","via":[{"field":"task_id","target_capture":"tasks","target_key":"task_id"}],"expect":[{"field":"emit_outcome","equals":true}]}]});
    let evidence = stage(&[
        (
            "sources",
            vec![json!({"source_collection":"Note","event_source_id":"notes"})],
        ),
        (
            "triggers",
            vec![json!({"source":{"event_source_id":"notes"},"task_id":"ack"})],
        ),
        ("tasks", vec![json!({"task_id":"ack","emit_outcome":false})]),
    ]);
    let feedback = CrewSpecMatch.evaluate(&params, &evidence).feedback.unwrap();
    assert!(
        feedback.contains("tasks \"ack\": emit_outcome"),
        "{feedback}"
    );
}

#[test]
fn selected_profile_label_uses_its_id_only_when_the_name_is_null() {
    let params = json!({"links":[{"capture":"agents","key":"agent_id","id":"engineer","field":"inference_profile_id","target_capture":"profiles","target_key":"profile_id","expect":[{"field":"display_name","fallback_field":"profile_id","equals":"Research"}]}]});
    for (profile, met) in [
        (json!({"profile_id":"Research"}), true),
        (json!({"profile_id":"Research","display_name":null}), true),
        (
            json!({"profile_id":"Research","display_name":"Wrong"}),
            false,
        ),
        (json!({"profile_id":"Research","display_name":""}), false),
    ] {
        let evidence = stage(&[
            (
                "agents",
                vec![json!({"agent_id":"engineer","inference_profile_id":"Research"})],
            ),
            (
                "profiles",
                vec![
                    profile.clone(),
                    json!({"profile_id":"unused","display_name":"Research"}),
                ],
            ),
        ]);
        let verdict = CrewSpecMatch.evaluate(&params, &evidence);
        assert_eq!(
            verdict.score_bp == Some(10000),
            met,
            "{profile}: {}",
            verdict.raw
        );
    }
}

#[test]
fn selected_datastore_contract_rejects_runtime_keys_decoys_and_missing_calls() {
    let params = json!({"agents":{"agents":"agents","contexts":"contexts","tools":"tools","expect":[{"agent_id":"worker-a","datastore":{"surfaces":"surfaces","create":["Result"],"query":["Result"],"caller_fields":{"Result":["correlation","result"]},"query_fields":{"Result":["correlation","result"]},"lookup_by":"correlation","called_create":["Result"],"called_query":["Result"]}}]}});
    let mut e = home();
    let tool = json!({"tools_id":"t1","datastore":{"datastore_tool_surface_ids":["arbitrary-id"]}});
    let surface = json!({"_docID":"physical-surface","surface_id":"arbitrary-id","node_did":"did:x","entries":[{"kind":"create","tool_name":"save_result","collection":"Result","description":"Save the caller's result","fields":[{"name":"correlation","required":true},{"name":"result","required":true}]},{"kind":"query","tool_name":"find_result","collection":"Result","description":"Find results by the caller's key","fields":["correlation","result"],"filter_fields":[{"name":"correlation"}]}]});
    e.captures.insert(
        "tools".into(),
        CaptureResult::Documents {
            rows: vec![tool.clone()],
        },
    );
    e.captures.insert(
        "surfaces".into(),
        CaptureResult::Documents {
            rows: vec![surface.clone()],
        },
    );
    e.tool_calls = ["save_result", "find_result"]
        .into_iter()
        .map(|name| crate::eval::runner::embedded::ToolCallEvidence {
            tool_name: name.into(),
            status: Some("completed".into()),
            lifecycle_state: Some("completed".into()),
            tool_failure_class: None,
            started_at: None,
            completed_at: None,
            args: Value::Null,
            result: Value::Null,
        })
        .collect();
    assert!(
        jsonschema::validator_for(&CrewSpecMatch.describe().params_schema)
            .unwrap()
            .is_valid(&params)
    );
    let good = CrewSpecMatch.evaluate(&params, &e);
    assert_eq!(good.score_bp, Some(10000), "{}", good.raw);
    for mutation in 0..6 {
        let mut bad = e.clone();
        match mutation {
            0 => {
                let mut s = surface.clone();
                s["entries"][0]["fields"][0] = json!({"name":"correlation","fill":"correlation"});
                bad.captures.insert(
                    "surfaces".into(),
                    CaptureResult::Documents { rows: vec![s] },
                );
            }
            1 => {
                let mut t = tool.clone();
                t["datastore"]["datastore_tool_surface_ids"] = json!(["unselected"]);
                bad.captures
                    .insert("tools".into(), CaptureResult::Documents { rows: vec![t] });
            }
            2 => {
                bad.tool_calls.pop();
            }
            3 => {
                let mut s = surface.clone();
                s["entries"][1]["filter_fields"] =
                    json!([{"name":"correlation"},{"name":"result","required":true}]);
                bad.captures.insert(
                    "surfaces".into(),
                    CaptureResult::Documents { rows: vec![s] },
                );
            }
            4 => {
                bad.tool_calls[0].status = Some("failed".into());
            }
            _ => {
                let mut s = surface.clone();
                s["entries"][1]["fields"] = json!(["correlation"]);
                bad.captures.insert(
                    "surfaces".into(),
                    CaptureResult::Documents { rows: vec![s] },
                );
            }
        }
        let v = CrewSpecMatch.evaluate(&params, &bad);
        if mutation == 3 {
            assert!(v
                .feedback
                .as_deref()
                .unwrap()
                .contains("find_result query must accept caller-supplied correlation alone"));
        }
        assert!(
            v.score_bp.unwrap() < 10000,
            "mutation {mutation}: {}",
            v.raw
        );
    }
}

#[test]
fn named_roles_follow_actual_ids_and_reject_ambiguous_or_wrong_grants() {
    let params = json!({"agents":{"agents":"agents","contexts":"contexts","tools":"tools","expect":[
        {"agent_id":"coordinator","source_match":{"field":"display_name","matches":"(?i)\\bcoordinator\\b"},"delegates":{"targets":"targets","agents":["reviewer"]}},
        {"agent_id":"reviewer","source_match":{"field":"display_name","matches":"(?i)\\breviewer\\b"},"tools":[{"field":"host.bash.mode","equals":"Off"}]}
    ]}});
    assert!(
        jsonschema::validator_for(&CrewSpecMatch.describe().params_schema)
            .unwrap()
            .is_valid(&params)
    );
    let good = stage(&[
        (
            "agents",
            vec![
                json!({"agent_id":"did:x:desk-coordinator","display_name":"Research desk Coordinator","context_id":"c"}),
                json!({"agent_id":"did:x:random-42","display_name":"Research desk Reviewer","context_id":"r"}),
            ],
        ),
        (
            "contexts",
            vec![
                json!({"context_id":"c","tools_id":"ct"}),
                json!({"context_id":"r","tools_id":"rt"}),
            ],
        ),
        (
            "tools",
            vec![
                json!({"tools_id":"ct","agents":{"enabled":true,"target_ids":["review-route"]}}),
                json!({"tools_id":"rt","host":{"bash":{"mode":"Off"}}}),
            ],
        ),
        (
            "targets",
            vec![
                json!({"target_id":"review-route","node_did":"did:x","target_node_did":"did:x","agent_id":"did:x:random-42"}),
            ],
        ),
    ]);
    assert_eq!(CrewSpecMatch.evaluate(&params, &good).score_bp, Some(10000));
    for mutation in 0..7 {
        let mut bad = good.clone();
        let capture = match mutation {
            0..=2 => "targets",
            3..=5 => "agents",
            _ => "tools",
        };
        let CaptureResult::Documents { rows } = bad.captures.get_mut(capture).unwrap() else {
            unreachable!()
        };
        match mutation {
            0 => rows[0]["agent_id"] = json!("reviewer"),
            1 => rows[0]["target_node_did"] = json!("did:foreign"),
            2 => rows[0]["agent_id"] = json!("did:x:desk-coordinator"),
            3 => {
                rows.push(json!({"agent_id":"another","display_name":"Reviewer","context_id":"r"}))
            }
            4 => {
                rows.pop();
            }
            5 => {
                rows.pop();
                rows[0]["display_name"] = json!("Coordinator Reviewer");
            }
            _ => rows[1]["host"]["bash"]["mode"] = json!("On"),
        }
        let result = CrewSpecMatch.evaluate(&params, &bad);
        assert!(
            result.score_bp.unwrap() < 10000,
            "mutation {mutation}: {}",
            result.raw
        );
    }
}

#[test]
fn delegates_follow_selected_targets_and_reject_foreign_or_extra_grants() {
    let params = json!({"agents":{"agents":"agents","contexts":"contexts","tools":"tools","expect":[{"agent_id":"worker-a","delegates":{"targets":"targets","agents":["reviewer"]}}]}});
    for (selected, owner, score) in [
        ("right", "did:x", 10000),
        ("wrong", "did:x", 5000),
        ("right", "did:other", 5000),
    ] {
        let mut e = home();
        e.captures.insert(
            "tools".into(),
            CaptureResult::Documents {
                rows: vec![
                    json!({"tools_id":"t1","agents":{"enabled":true,"target_ids":[selected]}}),
                ],
            },
        );
        e.captures.insert("targets".into(),CaptureResult::Documents{rows:vec![json!({"target_id":"right","node_did":"did:x","target_node_did":owner,"agent_id":"scope:reviewer"}),json!({"target_id":"wrong","node_did":"did:x","target_node_did":"did:x","agent_id":"scope:writer"})]});
        assert_eq!(CrewSpecMatch.evaluate(&params, &e).score_bp, Some(score));
    }
}

#[test]
fn links_check_the_selected_source_and_fail_missing_or_wrong_references() {
    let params = json!({"links": [{
        "capture": "triggers", "key": "trigger_id", "id": "dispatch",
        "field": "source.event_source_id", "target_capture": "sources", "target_key": "event_source_id",
        "expect": [{"field": "source_collection", "equals": "Assignment"}]
    }]});
    let schema = CrewSpecMatch.describe().params_schema;
    assert!(jsonschema::validator_for(&schema)
        .unwrap()
        .is_valid(&params));
    for (source, expected) in [("wanted", 10000), ("decoy", 5000), ("missing", 0)] {
        let evidence = stage(&[
            (
                "triggers",
                vec![
                    json!({"trigger_id":"dispatch", "source": json!({"kind":"event", "event_source_id":source}).to_string()}),
                ],
            ),
            (
                "sources",
                vec![
                    json!({"event_source_id":"wanted", "source_collection":"Assignment"}),
                    json!({"event_source_id":"decoy", "source_collection":"Result"}),
                ],
            ),
        ]);
        let verdict = CrewSpecMatch.evaluate(&params, &evidence);
        assert_eq!(verdict.score_bp, Some(expected), "{}", verdict.raw);
    }
}

#[test]
fn source_match_follows_one_role_and_rejects_ambiguity_and_decoys() {
    let params = json!({"links":[{"capture":"agents","source_match":{"field":"display_name","matches":"(?i)^(code )?reviewer$"},"field":"inference_profile_id","target_capture":"profiles","target_key":"profile_id","expect":[{"field":"max_turns","equals":30}]}]});
    assert!(
        jsonschema::validator_for(&CrewSpecMatch.describe().params_schema)
            .unwrap()
            .is_valid(&params)
    );
    for (name, selected, duplicate, expected) in [
        ("Code Reviewer", "right", false, 10000),
        ("Reviewer", "wrong", false, 5000),
        ("Writer", "right", false, 0),
        ("Reviewer", "right", true, 0),
    ] {
        let mut agents = vec![
            json!({"agent_id":"unpredictable-id","display_name":name,"inference_profile_id":selected}),
        ];
        if duplicate {
            agents.push(agents[0].clone());
        }
        let evidence = stage(&[
            ("agents", agents),
            (
                "profiles",
                vec![
                    json!({"profile_id":"right","max_turns":30}),
                    json!({"profile_id":"wrong","max_turns":100}),
                ],
            ),
        ]);
        assert_eq!(
            CrewSpecMatch.evaluate(&params, &evidence).score_bp,
            Some(expected)
        );
    }
}

#[test]
fn null_tools_selection_has_no_grants_but_a_dangling_reference_fails() {
    let params = json!({"agents":{"agents":"agents","contexts":"contexts","tools":"tools","expect":[{"agent_id":"planner","tools":[{"field":"host.bash.mode","matches":"^(Off|null)$"},{"field":"agents.enabled","matches":"^(false|null)$"}]}]}});
    for (context, expected) in [
        (json!({"context_id":"c","tools_id":null}), 10000),
        (json!({"context_id":"c","tools_id":"missing"}), 3333),
        (json!({"context_id":"c"}), 3333),
    ] {
        let evidence = stage(&[
            (
                "agents",
                vec![json!({"agent_id":"planner","context_id":"c"})],
            ),
            ("contexts", vec![context]),
            ("tools", vec![]),
        ]);
        assert_eq!(
            CrewSpecMatch.evaluate(&params, &evidence).score_bp,
            Some(expected)
        );
    }
}

#[test]
fn the_score_is_the_satisfied_fraction_broken_down_by_category() {
    let verdict = CrewSpecMatch.evaluate(&params(), &home());
    assert_eq!(
        verdict.kind,
        OutcomeKind::ModelAcceptance,
        "{}",
        verdict.raw
    );
    let categories = &verdict.raw["categories"];
    assert_eq!(
        categories["collections"],
        json!({"satisfied": 1, "total": 2})
    );
    assert_eq!(
        categories["emit_outcome"],
        json!({"satisfied": 2, "total": 2})
    );
    assert_eq!(
        categories["least_privilege"],
        json!({"satisfied": 5, "total": 5})
    );
    assert_eq!(categories["templates"], json!({"satisfied": 0, "total": 1}));
    assert_eq!(categories["receipt"], json!({"satisfied": 2, "total": 3}));
    assert_eq!(verdict.score_bp, Some(7_692));
    let feedback = verdict.feedback.unwrap();
    assert!(feedback.contains("doc.owner"), "{feedback}");
    assert!(
        feedback.contains("capture c_result has None rows"),
        "{feedback}"
    );
    assert_eq!(verdict.raw["activity"]["tool_calls"], 0);
}

#[test]
fn a_missing_agent_fails_its_whole_chain() {
    let mut evidence = home();
    evidence
        .captures
        .insert("agents".into(), CaptureResult::Documents { rows: vec![] });
    let verdict = CrewSpecMatch.evaluate(&params(), &evidence);
    assert_eq!(
        verdict.raw["categories"]["least_privilege"],
        json!({"satisfied": 0, "total": 5})
    );
}

#[test]
fn a_node_scoped_agent_id_matches_its_declared_id() {
    let mut evidence = home();
    evidence.captures.insert(
        "agents".into(),
        CaptureResult::Documents {
            rows: vec![json!({"agent_id": "did:key:z6Mk:worker-a", "context_id": "c1", "inference_profile_id": "glm-a"})],
        },
    );
    let verdict = CrewSpecMatch.evaluate(&params(), &evidence);
    assert_eq!(
        verdict.raw["categories"]["least_privilege"],
        json!({"satisfied": 5, "total": 5})
    );
    assert!(is_id("did:key:z6Mk:worker-a", "worker-a"));
    assert!(!is_id("did:key:z6Mk:big-worker-a", "worker-a"));
    assert!(!is_id("worker-ab", "worker-a"));
}

#[test]
fn each_prod_needed_costs_one_continuation_requirement() {
    let mut evidence = home();
    evidence.prods = 2;
    let verdict = CrewSpecMatch.evaluate(&json!({"continuation": {"max": 5}}), &evidence);
    assert_eq!(
        verdict.raw["categories"]["continuation"],
        json!({"satisfied": 3, "total": 5})
    );
    assert_eq!(verdict.raw["activity"]["prods"], 2);
}

#[test]
fn doc_fields_reads_each_name_once_and_ignores_other_roots() {
    assert_eq!(
        doc_fields("{{ doc.a }} {{doc.b_c}} {{ doc.a }} {{ session.session_id }} {{ mydoc.x }}"),
        vec!["a".to_string(), "b_c".to_string()]
    );
}

#[test]
fn params_that_require_nothing_are_the_grader() {
    let verdict = CrewSpecMatch.evaluate(&json!({}), &home());
    assert_eq!(verdict.kind, OutcomeKind::Grader);
}
