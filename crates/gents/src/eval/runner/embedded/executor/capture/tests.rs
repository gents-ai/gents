use super::*;
use crate::config_client::DesiredStateApplyPlan;
use crate::eval::runner::embedded::executor::install::{apply, desired};
use crate::eval::runner::embedded::home::EmbeddedHome;
use crate::eval::OutcomeKind;
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
            &format!(r#"mutation {{ create_AgentRequest(input: {{request_id: "{purpose}", purpose: "{purpose}", node_did: "{did}", agent_id: "{did}:research-helper", content: "TRAIN-111"}}) {{ _docID }} }}"#),
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
    let mut all_purposes = capture["filter"].clone();
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
    let normal = capture_documents(
        &home.node,
        "AgentRequest",
        &capture["filter"],
        &fields,
        home.did(),
    )
    .await
    .unwrap();
    assert_eq!(normal.len(), 1);
    assert_eq!(normal[0]["request_id"], "normal");
    home.node.shutdown().await;
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
    node.shutdown().await;
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
        crate::defra_query::query::collect_filter_field_keys(&case.filter, &mut filter_fields);
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
    let home = EmbeddedHome::create_temp("eval-backend-metadata")
        .await
        .unwrap();
    let plan = DesiredStateApplyPlan::new(vec![desired(
        Collection::InferenceBackend,
        json!({
            "node_did": home.did(), "backend_id": "metadata-backend", "name": "metadata",
            "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:9/v1",
            "auth": {"kind": "api_key", "key": "eval-secret-sentinel"}, "enabled": true
        }),
    )])
    .unwrap();
    apply(
        &ConfigAccess::local(home.node.clone()),
        "eval.test.backend_metadata",
        &plan,
    )
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
    evidence.captures = run_captures(
        &home.node,
        home.did(),
        "session-metadata",
        home.path(),
        &[capture],
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
    home.node.shutdown().await;
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
