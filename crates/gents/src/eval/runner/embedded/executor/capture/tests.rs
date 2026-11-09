use super::*;
use crate::eval::runner::embedded::executor::install::{apply, desired};
use crate::eval::runner::embedded::home::EmbeddedHome;
use crate::eval::OutcomeKind;
use crate::store::config_client::DesiredStateApplyPlan;
use crate::{Collection, ConfigAccess};
use serde_json::json;

#[tokio::test]
async fn schema_capture_reads_active_indexes_and_records_absent_collections() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ConfigAccess::local(node.clone())
        .add_collection("type SearchNote { body: String @fulltext(language: \"english\") }")
        .await
        .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let captures = run_captures(
        &node,
        "did:test",
        "session-test",
        workspace.path(),
        &[
            Capture::Schema {
                name: "present".into(),
                collections: vec!["SearchNote".into()],
            },
            Capture::Schema {
                name: "absent".into(),
                collections: vec!["NoSuchCollection".into()],
            },
        ],
        CaptureBindings::default(),
    )
    .await;
    let CaptureResult::Schema { collections } = &captures["present"] else {
        panic!("schema evidence")
    };
    assert_eq!(collections[0]["FullTextIndexes"][0]["FieldName"], "body");
    assert_eq!(
        captures["absent"],
        CaptureResult::Schema {
            collections: vec![]
        }
    );
}

#[tokio::test]
async fn delegation_case_capture_excludes_session_title_requests() {
    let home = EmbeddedHome::create_temp("eval-normal-request-capture")
        .await
        .unwrap();
    let did = escape_graphql_string(home.did());
    for purpose in ["normal", "title-audit"] {
        let purpose = escape_graphql_string(purpose);
        ConfigAccess::write_local(
            &home.node,
            "eval.test.normal_request_capture",
            &format!(r#"mutation {{ create_AgentRequest(input: {{request_id: "{purpose}", purpose: "{purpose}", node_did: "{did}", agent_did: "{did}:research-helper", content: "TRAIN-111"}}) {{ _docID }} }}"#),
        )
        .await
        .unwrap();
    }
    let case: Value = serde_json::from_slice(
        &std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "tests/fixtures/configurator_evals/ladder/l5_agents_tools/cases/train_minimal_self_target.json",
        ))
        .unwrap(),
    )
    .unwrap();
    let capture = case["stages"][1]["capture"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capture| capture["name"] == "helper-request")
        .unwrap();
    let fields: Vec<String> = serde_json::from_value(capture["fields"].clone()).unwrap();
    let role = BTreeMap::from([(
        "role_agents".into(),
        CaptureResult::Documents {
            rows: vec![
                json!({"display_name":"Research Helper", "agent_did":format!("{did}:research-helper")}),
            ],
        },
    )]);
    let bound = bind_capture_subject(
        &capture["filter"],
        "unused",
        CaptureBindings::default(),
        &role,
    )
    .unwrap();
    let mut all_purposes = bound.clone();
    all_purposes.as_object_mut().unwrap().remove("purpose");
    let all = capture_documents(
        &home.node,
        "AgentRequest",
        &all_purposes,
        &fields,
        home.did(),
    )
    .await
    .unwrap();
    assert_eq!(all.len(), 2);
    let normal = capture_documents(&home.node, "AgentRequest", &bound, &fields, home.did())
        .await
        .unwrap();
    assert_eq!(normal.len(), 1);
    assert_eq!(normal[0]["request_id"], "normal");
    home.node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn document_capture_excludes_other_sessions_with_the_same_sequence() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let access = ConfigAccess::local(node.clone());
    access
        .add_collection("type CaptureHeader { session_id: String sequence: Int }")
        .await
        .unwrap();
    let session_id = "trial\"session";
    for session in [session_id, "background-session"] {
        access.write("eval.test.capture_session", &format!(
            "mutation {{ create_CaptureHeader(input: {{session_id: \"{}\", sequence: 1}}) {{ _docID }} }}",
            escape_graphql_string(session)
        )).await.unwrap();
    }
    let workspace = tempfile::tempdir().unwrap();
    let captures = run_captures(
        &node,
        "did:test",
        session_id,
        workspace.path(),
        &[Capture::Documents {
            name: "source_headers".into(),
            collection: "CaptureHeader".into(),
            filter: json!({"session_id": {"_eq": "$session"}, "sequence": {"_eq": 1}}),
            fields: vec!["session_id".into(), "sequence".into()],
        }],
        CaptureBindings::default(),
    )
    .await;
    let CaptureResult::Documents { rows } = &captures["source_headers"] else {
        panic!("document capture")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["session_id"], session_id);
    assert_eq!(rows[0]["sequence"], 1);
    assert!(rows[0]["_docID"].as_str().is_some());
    assert_eq!(
        bind_capture_session(&json!("literal $session text"), session_id),
        json!("literal $session text")
    );
    node.shutdown().await.expect("shutdown node");
}

#[test]
fn capture_query_renders_filter_and_escapes_strings() {
    let query = documents_capture_query(
        "AgentRequest",
        &json!({"requester_did": {"_eq": "$trial"}}),
        &["content".to_string()],
        r#"did:x"y"#,
    )
    .unwrap();
    assert!(
        query.contains(r#"filter: {requester_did: {_eq: "did:x\"y"}}"#),
        "{query}"
    );
    assert!(
        query.contains("{ _docID content requester_did }"),
        "{query}"
    );
}

#[test]
fn document_captures_reject_secret_fields_and_oauth_documents() {
    let error = documents_capture_query(
        "InferenceBackend",
        &json!({"node_did": {"_eq": "did:test"}}),
        &["backend_id".into(), "auth".into()],
        "did:test",
    )
    .unwrap_err();
    assert!(error.to_string().contains("InferenceBackend"));
    assert!(error.to_string().contains("restricted"));

    let error = documents_capture_query(
        "InferenceBackend",
        &json!({"_or": [{"auth": {"_eq": {"kind": "api_key"}}}]}),
        &["backend_id".into()],
        "did:test",
    )
    .unwrap_err();
    assert!(error.to_string().contains("restricted"));

    let error = documents_capture_query(
        "OAuthCredential",
        &json!({}),
        &["credential_id".into()],
        "did:test",
    )
    .unwrap_err();
    assert!(error.to_string().contains("credential collection"));

    let safe = documents_capture_query(
        "Task",
        &json!({"node_did": {"_eq": "did:test"}}),
        &["task_id".into(), "display_name".into()],
        "did:test",
    )
    .unwrap();
    assert!(safe.contains("task_id display_name"));
}

#[test]
fn generated_document_capture_cases_match_lean() {
    use std::collections::BTreeSet;

    let cases = crate::lean_vocab_test::lean_eval_document_capture_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let mut filter_fields = Vec::new();
        crate::tools::builtin::defra_query::query::collect_filter_field_keys(
            &case.filter,
            &mut filter_fields,
        );
        assert_eq!(
            filter_fields.into_iter().collect::<BTreeSet<_>>(),
            case.filter_fields.iter().cloned().collect::<BTreeSet<_>>(),
            "{case:?}"
        );
        let query =
            documents_capture_query(&case.collection, &case.filter, &case.fields, "did:test");
        assert_eq!(query.is_ok(), case.allowed, "{case:?}: {query:?}");
    }
}

#[tokio::test]
async fn l1_backend_capture_grades_metadata_without_retaining_auth() {
    let mut home = EmbeddedHome::create_temp("eval-backend-metadata")
        .await
        .unwrap();
    let evaluator = crate::eval::runner::embedded::home::bind_test_evaluator(&mut home);
    let evaluator_did = crate::NodeIdentity::did(evaluator.as_ref());
    crate::eval::runner::embedded::executor::install::grant_trial_operations(&home, evaluator_did)
        .await
        .unwrap();
    let access = home.client_access(evaluator_did).unwrap();
    crate::test_support::bootstrap_signed_test_node(&access, home.did()).await;
    let plan = DesiredStateApplyPlan::new(vec![desired(
        Collection::InferenceBackend,
        json!({
            "node_did": home.did(), "backend_id": "metadata-backend", "name": "metadata",
            "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:9/v1",
            "auth": {"kind": "api_key", "key": "eval-secret-sentinel"}, "enabled": true
        }),
    )])
    .unwrap();
    apply(&access, "eval.test.backend_metadata", &plan)
        .await
        .unwrap();

    let case: Value = serde_json::from_slice(
        &std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "tests/fixtures/configurator_evals/ladder/l1_inference/cases/val_minimal_limits.json",
        ))
        .unwrap(),
    )
    .unwrap();
    let stage = &case["stages"][1];
    let capture: Capture = serde_json::from_value(
        stage["capture"]
            .as_array()
            .unwrap()
            .iter()
            .find(|capture| capture["name"] == "backends")
            .unwrap()
            .clone(),
    )
    .unwrap();
    let mut evidence = crate::eval::runner::ScriptedExecutor::passed_evidence(
        home.did(),
        "configure",
        "session-metadata",
        Vec::new(),
    )
    .stages
    .remove(0);
    evidence.captures = run_captures_with_access(
        &access,
        home.did(),
        "session-metadata",
        home.path(),
        &[capture],
        CaptureBindings {
            evaluator_did: Some(evaluator_did),
            ..CaptureBindings::default()
        },
    )
    .await;
    let CaptureResult::Documents { rows } = &evidence.captures["backends"] else {
        panic!("backend metadata capture must contain documents");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["backend_id"], "metadata-backend");
    assert!(rows[0].get("auth").is_none());
    assert!(!serde_json::to_string(&evidence.captures)
        .unwrap()
        .contains("eval-secret-sentinel"));

    let check = stage["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "captured_rows_count")
        .unwrap();
    let registry = crate::eval::checks::CheckRegistry::builtin();
    let verdict = registry
        .get("captured_rows_count")
        .unwrap()
        .evaluate(&check["params"], &evidence);
    assert_eq!(verdict.kind, OutcomeKind::Passed, "{:?}", verdict.raw);
    home.node.shutdown().await.expect("shutdown node");
}

fn captured(workspace: &Path, glob: &str) -> (Vec<FileRef>, Vec<String>) {
    match capture_files(workspace, glob) {
        Some(CaptureResult::Files {
            files,
            outside_workspace,
        }) => (files, outside_workspace),
        other => panic!("expected a file capture, got {other:?}"),
    }
}

#[test]
fn file_capture_refuses_parent_components() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("report.txt"), b"hello").unwrap();
    std::fs::write(dir.path().join("outside.txt"), b"secret").unwrap();

    assert_eq!(capture_files(&workspace, "../*.txt"), None);
    assert_eq!(capture_files(&workspace, "/etc/*"), None);
    let (found, outside) = captured(&workspace, "*.txt");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, "report.txt");
    assert_eq!(found[0].bytes, 5);
    assert!(outside.is_empty());
}

#[cfg(unix)]
#[test]
fn file_capture_does_not_follow_a_symlink_out_of_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("mine.txt"), b"mine").unwrap();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, b"not the trial's").unwrap();
    std::os::unix::fs::symlink(&secret, workspace.join("escape.txt")).unwrap();

    let (files, outside) = captured(&workspace, "*.txt");
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["mine.txt"],
        "only the file the trial itself holds is captured"
    );
    assert_eq!(
        outside,
        ["escape.txt"],
        "the skipped match is recorded, not silently dropped"
    );
}

#[cfg(unix)]
#[test]
fn a_file_capture_that_cannot_be_read_is_omitted() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let locked = workspace.join("locked.txt");
    std::fs::write(&locked, b"unreadable").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&locked).is_ok() {
        return;
    }

    assert_eq!(
        capture_files(&workspace, "*.txt"),
        None,
        "a capture that cannot be read is absent, not empty"
    );

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (files, outside) = captured(&workspace, "*.txt");
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["locked.txt"]
    );
    assert!(outside.is_empty());
}

#[test]
fn capture_filters_bind_only_the_selected_subject() {
    use crate::eval::runner::subject_binding::SubjectAgent;
    let subject = SubjectAgent {
        slot: "engineer".into(),
        agent_did: "did:key:actual-subject".into(),
    };
    let bindings = CaptureBindings {
        subject: Some(&subject),
        evaluator_did: Some("did:key:evaluator"),
        request_id: None,
    };
    let filter = json!({"agent_did":{"_ne":"gents:agent-slot:engineer"}, "session_id":{"_eq":"$session"}, "requester_did":{"_eq":"$evaluator"}});
    assert_eq!(
        bind_capture_subject(&filter, "session", bindings, &BTreeMap::new()).unwrap(),
        json!({"agent_did":{"_ne":"did:key:actual-subject"}, "session_id":{"_eq":"session"}, "requester_did":{"_eq":"did:key:evaluator"}})
    );
    assert!(bind_capture_subject(
        &filter,
        "session",
        CaptureBindings::default(),
        &BTreeMap::new()
    )
    .is_err());
    assert!(bind_capture_subject(
        &filter,
        "session",
        CaptureBindings {
            subject: Some(&subject),
            evaluator_did: None,
            request_id: None
        },
        &BTreeMap::new()
    )
    .is_err());
    assert!(bind_capture_subject(
        &json!({"agent_did":{"_eq":"gents:agent-slot:foreign#engineer"}}),
        "session",
        bindings,
        &BTreeMap::new()
    )
    .is_err());
    assert_eq!(
        bind_capture_subject(
            &json!({"requester_did":{"_eq":"literal $evaluator"}}),
            "session",
            bindings,
            &BTreeMap::new()
        )
        .unwrap(),
        json!({"requester_did":{"_eq":"literal $evaluator"}})
    );
}

#[tokio::test]
async fn retained_capture_uses_the_evaluator_actor_for_document_access() {
    use crate::store::config_client::NodePermission;
    use crate::store::identity::{KeyIdentity, NodeIdentity};

    let home = EmbeddedHome::create_temp("capture-evaluator-acp")
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let reader = KeyIdentity::load_or_create(directory.path().join("reader.key"), None).unwrap();
    let owner =
        ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(home.did()).unwrap());
    owner.write("eval.test.capture_actor", &format!(
        "mutation {{create_AgentRequest(input: {{request_id: \"owned\", node_did: \"{}\", agent_did: \"owned-agent\", purpose: \"normal\"}}) {{_docID}}}}",
        escape_graphql_string(home.did()),
    )).await.unwrap();
    let reader_did = ::identity::Did::new(reader.did()).unwrap();
    for permission in [NodePermission::CollectionGet, NodePermission::DocumentRead] {
        owner
            .grant_node_permission(&reader_did, permission)
            .await
            .unwrap();
    }
    let captures = [Capture::Documents {
        name: "requests".into(),
        collection: "AgentRequest".into(),
        filter: json!({"request_id":{"_eq":"owned"}}),
        fields: vec!["request_id".into()],
    }];
    let owner_rows = run_captures(
        &home.node,
        home.did(),
        "s",
        directory.path(),
        &captures,
        CaptureBindings {
            subject: None,
            evaluator_did: Some(home.did()),
            request_id: None,
        },
    )
    .await;
    let CaptureResult::Documents { rows } = &owner_rows["requests"] else {
        panic!("owner capture")
    };
    assert_eq!(rows.len(), 1);
    let reader_rows = run_captures(
        &home.node,
        home.did(),
        "s",
        directory.path(),
        &captures,
        CaptureBindings {
            subject: None,
            evaluator_did: Some(reader.did()),
            request_id: None,
        },
    )
    .await;
    let CaptureResult::Documents { rows } = &reader_rows["requests"] else {
        panic!("reader capture")
    };
    assert!(rows.is_empty());
    home.node.shutdown().await.unwrap();
}

#[test]
fn capture_reference_requires_one_named_agent_and_its_actual_did() {
    let filter = json!({"agent_did":{"_eq":{"$capture":{"capture":"agents", "source_match":{"field":"display_name","matches":"(?i)^research[ -]helper$"},"field":"agent_did"}}}});
    let bind = |rows| {
        bind_capture_subject(
            &filter,
            "s",
            CaptureBindings::default(),
            &BTreeMap::from([("agents".into(), CaptureResult::Documents { rows })]),
        )
    };
    let role = json!({"display_name":"Research Helper","agent_did":"did:key:random"});
    let decoy = json!({"display_name":"Wrong Helper","agent_did":"did:key:research-helper"});
    assert_eq!(
        bind(vec![role.clone(), decoy.clone()]).unwrap(),
        json!({"agent_did":{"_eq":"did:key:random"}})
    );
    assert!(bind(vec![decoy]).is_err());
    assert!(bind(vec![role.clone(), role]).is_err());
    assert!(bind(vec![json!({"display_name":"Research Helper"})]).is_err());
}

#[test]
fn nightly_reviewer_fixture_captures_requests_for_the_actual_unique_agent() {
    let case: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/configurator_evals/ladder/l1_inference/cases/held_nightly_automation_reviewer.json"
    )))
    .unwrap();
    let fire = case["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stage| stage["stage_id"] == "fire")
        .unwrap();
    let captures = fire["capture"].as_array().unwrap();
    assert_eq!(captures[0]["name"], "role_agents");
    let request = captures
        .iter()
        .find(|capture| capture["name"] == "completed_review")
        .unwrap();
    let bind = |rows| {
        bind_capture_subject(
            &request["filter"],
            "unused",
            CaptureBindings::default(),
            &BTreeMap::from([("role_agents".into(), CaptureResult::Documents { rows })]),
        )
    };
    let reviewer =
        json!({"display_name":"Nightly Reviewer", "agent_did":"did:key:actual-reviewer"});
    let decoy = json!({"display_name":"Other Reviewer", "agent_did":"did:key:nightly-reviewer"});
    let bound = bind(vec![reviewer.clone(), decoy.clone()]).unwrap();
    assert_eq!(bound["agent_did"], json!({"_eq":"did:key:actual-reviewer"}));
    assert_eq!(bound["content"], json!({"_like":"%nightly-digest%"}));
    assert_eq!(bound["lifecycle_state"], json!({"_eq":"completed"}));
    assert!(bind(vec![decoy]).is_err());
    assert!(bind(vec![reviewer.clone(), reviewer]).is_err());
}

#[test]
fn capture_request_binding_requires_the_current_stage_request() {
    let filter = json!({"request_id":{"_eq":"$request"}});
    let bindings = CaptureBindings {
        request_id: Some("current"),
        ..Default::default()
    };
    assert_eq!(
        bind_capture_subject(&filter, "s", bindings, &BTreeMap::new()).unwrap(),
        json!({"request_id":{"_eq":"current"}})
    );
    assert!(
        bind_capture_subject(&filter, "s", CaptureBindings::default(), &BTreeMap::new()).is_err()
    );
}

#[tokio::test]
async fn logical_backend_capture_and_live_counts_use_held_user_metadata() {
    use crate::eval::runner::embedded::executor::install::install;
    use crate::eval::runner::embedded::executor::tests::{
        frozen_binding, materialized_digest, write_slot_pack,
    };
    use crate::eval::runner::embedded::live::LiveObserver;
    use crate::eval::runner::executor::{StageSpec, TrialSpec};

    let dir = tempfile::tempdir().unwrap();
    let pack = dir.path().join("pack");
    write_slot_pack(&pack, "gents:inference-slot:primary");
    let mut home = EmbeddedHome::create_temp("backend-capture").await.unwrap();
    let evaluator = Arc::new(
        crate::KeyIdentity::load_or_create(dir.path().join("evaluator.key"), None).unwrap(),
    );
    let evaluator_did = crate::NodeIdentity::did(evaluator.as_ref()).to_owned();
    let capability = evaluator.client_signing_capability().unwrap();
    home.set_evaluator(evaluator, capability);
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack),
        pack_dir: pack,
        inference: frozen_binding(json!("frozen-profile")),
        agent_slot: "subject".into(),
        evaluator_did: evaluator_did.clone(),
        ..TrialSpec::empty_for_tests("backend-capture")
    };
    install(&spec, &home, &dir.path().join("workspace"))
        .await
        .unwrap();
    let access = home.client_access(&evaluator_did).unwrap();
    let fields = vec!["backend_id".into(), "endpoint".into(), "enabled".into()];
    let filter = json!({"_and": [
        {"node_did": {"_eq": "$trial"}},
        {"backend_id": {"_eq": "frozen-backend"}}
    ]});
    let rows =
        capture_documents_with_access(&access, "InferenceBackend", &filter, &fields, home.did())
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["backend_id"], "frozen-backend");
    assert_eq!(rows[0]["endpoint"], "http://127.0.0.1:1/v1");
    assert_eq!(rows[0]["enabled"], true);
    assert!(rows[0]["_docID"].as_str().is_some_and(|id| !id.is_empty()));
    assert_eq!(rows[0].as_object().unwrap().len(), 4);
    let absent = json!({"node_did": {"_eq": "$trial"}, "backend_id": {"_eq": "other"}});
    assert!(capture_documents_with_access(
        &access,
        "InferenceBackend",
        &absent,
        &fields,
        home.did()
    )
    .await
    .unwrap()
    .is_empty());
    for (filter, fields) in [
        (json!({}), vec!["auth".to_owned()]),
        (json!({"auth": {"_eq": "secret"}}), Vec::new()),
        (json!({"credential_id": {"_eq": "secret"}}), Vec::new()),
        (json!({}), vec!["credential_id".to_owned()]),
    ] {
        assert!(capture_documents_with_access(
            &access,
            "InferenceBackend",
            &filter,
            &fields,
            home.did()
        )
        .await
        .is_err());
    }
    let stages = vec![StageSpec {
        stage_id: "check".into(),
        prompt: String::new(),
        seed: None,
        deadline_secs: None,
        settle: false,
        continuation: None,
        review_previous: false,
        captures: vec![Capture::Documents {
            name: "backends".into(),
            collection: "InferenceBackend".into(),
            filter,
            fields,
        }],
    }];
    let live = LiveObserver::new(&home.node, &access, home.did(), &stages)
        .await
        .snapshot()
        .await
        .unwrap();
    assert_eq!(live.documents.get("InferenceBackend"), Some(&1));
    assert_eq!(live.captures.get("backends"), Some(&1));
    home.node.shutdown().await.unwrap();
}
