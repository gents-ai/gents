use std::collections::BTreeMap;
use std::path::PathBuf;

use gents::store::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::{Collection, NodeIdentity};
use serde_json::json;

use super::super::bound_behavior::load_bound_model_selection_id_for_state;
use super::super::turn_projection::lean_reasoning_contracts::{
    lean_codex_shim_agent_selection_cases, lean_codex_shim_thread_status_cases,
};
use super::{codex_thread_json, projected_thread_status, CausedThread, CodexThreadRecord};

fn record() -> CodexThreadRecord {
    CodexThreadRecord {
        session_id: "thread".into(),
        cwd: PathBuf::from("/workspace"),
        archived: false,
        loaded: true,
        memory_mode: "disabled".into(),
        name: String::new(),
        settings_json: String::new(),
        git_info: None,
        projection_started: None,
        session: None,
        latest_request: None,
        caused: None,
    }
}

fn caused_thread(node_did: &str, agent_id: &str) -> CausedThread {
    CausedThread {
        session_id: "thread".into(),
        node_did: node_did.into(),
        requester_did: None,
        agent_id: agent_id.into(),
        parent_session_id: "parent".into(),
        root_session_id: "root".into(),
        depth: 1,
        nickname: "child".into(),
        model: None,
        client_projection: None,
        failure_reason: None,
        created_at: None,
        latest_request_id: "request".into(),
        latest_request_doc_id: "request-doc".into(),
        latest_request_content: String::new(),
        latest_request_created_at: None,
    }
}

#[test]
fn generated_thread_status_cases_drive_thread_projection() {
    let cases = lean_codex_shim_thread_status_cases();
    assert_eq!(cases.len(), 10);
    for case in cases {
        let head = case.request_state.as_deref().map(|state| {
            gents_protocol::client_protocol::project_persisted_attempt(state, false)
                .unwrap_or_else(|| panic!("{}: invalid lifecycle {state:?}", case.witness))
        });
        let status = serde_json::to_value(projected_thread_status(head)).unwrap();
        assert_eq!(status["type"], case.projected_status, "{}", case.witness);
        assert!(
            status.pointer("/activeFlags/0").is_none(),
            "{}",
            case.witness
        );

        let mut record = record();
        record.latest_request = case.request_state.as_deref().map(|state| {
            serde_json::from_value(json!({
                "request": {
                    "_docID": "request-doc", "request_id": "request", "lifecycle_state": state
                },
                "response": null
            }))
            .unwrap()
        });
        assert_eq!(
            codex_thread_json(&record, false)["status"],
            status,
            "{}: root thread",
            case.witness
        );

        let mut child = caused_thread("did:test:child", "child");
        child.client_projection = head;
        record.latest_request = None;
        record.caused = Some(child);
        assert_eq!(
            codex_thread_json(&record, false)["status"],
            status,
            "{}: caused thread",
            case.witness
        );
    }
}

#[tokio::test]
async fn generated_agent_selection_cases_drive_scoped_model_binding() {
    let cases = lean_codex_shim_agent_selection_cases();
    assert_eq!(cases.len(), 5);
    for case in cases {
        let temp = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            crate::shared::test_support::node_builder()
                .data_path(temp.path().join("db"))
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();

        let mut identities = BTreeMap::new();
        for owner in [&case.selected_owner, &case.actual_owner] {
            if !identities.contains_key(owner) {
                let path = temp.path().join(format!("owner-{}.key", identities.len()));
                let identity = gents::KeyIdentity::load_or_create(&path, None).unwrap();
                gents::ensure_node(&node, identity.did()).await.unwrap();
                identities.insert(owner.clone(), identity);
            }
        }
        let selected_owner = identities[&case.selected_owner].did();
        let actual_owner = identities[&case.actual_owner].did();
        if let Some(model) = &case.resolved_model {
            let plan = DesiredStateApplyPlan::new(
                [
                    (
                        Collection::Agent,
                        json!({"node_did": actual_owner, "agent_id": case.actual_agent,
                            "inference_profile_id": "profile"}),
                    ),
                    (
                        Collection::InferenceProfile,
                        json!({"node_did": actual_owner, "profile_id": "profile",
                            "backend_id": "backend", "model_name": model}),
                    ),
                    (
                        Collection::InferenceBackend,
                        json!({"node_did": actual_owner, "backend_id": "backend", "name": "Test",
                            "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
                            "auth": {"kind": "unauthenticated"}}),
                    ),
                ]
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
            )
            .unwrap();
            ConfigAccess::transact_local_write(
                &node,
                None,
                "codex.agent_selection.fixture",
                |txn| {
                    let plan = &plan;
                    Box::pin(async move { apply_desired_state_plan(txn, plan).await })
                },
            )
            .await
            .unwrap();
        }

        let mut record = record();
        record.caused = case
            .thread_agent_id
            .as_deref()
            .map(|agent| caused_thread(selected_owner, agent));
        let agent = record.projection_agent_id(&case.root_agent_id).to_owned();
        let actual_model =
            load_bound_model_selection_id_for_state(&node, actual_owner, &case.actual_agent).await;
        let model = load_bound_model_selection_id_for_state(&node, selected_owner, &agent).await;
        node.shutdown().await;

        if let Some(expected) = &case.resolved_model {
            assert_eq!(
                actual_model.unwrap_or_else(|error| panic!("{}: {error:#}", case.witness)),
                format!("backend::{expected}"),
                "{}: installed binding",
                case.witness
            );
        }
        assert_eq!(agent, case.projected_agent_id, "{}", case.witness);
        match &case.projected_model {
            Some(expected) => assert_eq!(
                model.unwrap_or_else(|error| panic!("{}: {error:#}", case.witness)),
                format!("backend::{expected}"),
                "{}",
                case.witness
            ),
            None => assert!(
                model.is_err(),
                "{}: unexpected model {model:?}",
                case.witness
            ),
        }
    }
}
