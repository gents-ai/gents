use std::sync::Arc;

use gents_protocol::session_input_edit::{
    SessionInputEditOutcome, SessionInputEditReceipt, SESSION_INPUT_EDIT_VERSION,
};

use super::*;

#[test]
fn authored_edit_uses_the_send_skill_parser() {
    let mut edit = gents::request::lifecycle::PendingQueueEdit {
        expected_request_doc_ids: vec!["request".into()],
        selected_request_doc_ids: vec!["request".into()],
        messages: vec![gents::request::lifecycle::PendingMessageEdit {
            request_doc_id: "request".into(),
            content: "/skill review\nPlease review".into(),
            authored_input: Some(Default::default()),
        }],
    };
    normalize_pending_edit_skills(&mut edit);
    assert_eq!(edit.messages[0].content, "Please review");
    assert_eq!(
        edit.messages[0]
            .authored_input
            .as_ref()
            .unwrap()
            .selected_skill_ids,
        ["review"]
    );
}

fn applied_receipt() -> SessionInputEditReceipt {
    SessionInputEditReceipt {
        version: SESSION_INPUT_EDIT_VERSION,
        command_id: "accepted-edit".to_string(),
        command_digest: "sha256:test".to_string(),
        requester_did: "did:test:requester".to_string(),
        node_did: "did:test:node".to_string(),
        session_id: "session".to_string(),
        outcome: SessionInputEditOutcome::Applied,
        reason: String::new(),
        request_doc_ids: vec!["replacement-doc".to_string()],
        request_ids: vec!["replacement-request".to_string()],
        processed_at: "2026-10-14T00:00:00Z".to_string(),
        signature: vec![0; 64],
    }
}

fn modeled_command_outcome(case: &serde_json::Value) -> SessionInputEditOutcome {
    serde_json::from_value(case["command_outcome"].clone()).expect("modeled receipt outcome")
}

#[tokio::test]
async fn generated_accepted_receipt_survives_projection_refresh_failure() {
    let contracts: serde_json::Value =
        gents_lean_contract::load_contract_snapshot().expect("Lean contract snapshot");
    let case = contracts["session_input_edit_client_observation_cases"]
        .as_array()
        .and_then(|cases| {
            cases
                .iter()
                .find(|case| case["name"] == "applied_receipt_projection_failure")
        })
        .expect("modeled applied receipt with failed projection");
    let (store, _version) = ObservedStore::new(ClientStore::default());
    let before = store.projection_revision().store_version;
    let receipt = applied_receipt();
    let accepted = merge_accepted_edit_projection(
        &ConfigAccess::Graphql(gents::config_client::GraphqlEndpoint::anonymous(
            "invalid endpoint",
        )),
        store.as_ref(),
        receipt,
        &["selected-doc".to_string()],
        "did:test:node",
        "session",
        "did:test:requester",
    )
    .await;

    assert_eq!(
        serde_json::to_value(accepted.outcome).expect("serialize outcome"),
        serde_json::to_value(modeled_command_outcome(case)).expect("serialize modeled outcome")
    );
    assert_eq!(case["projection_fresh"], false);
    assert_eq!(store.projection_revision().store_version, before);
    assert!(store.snapshot().requests.is_empty());
}

#[tokio::test]
async fn incomplete_or_foreign_edit_projection_keeps_store_and_receipt() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("local projection node"),
    );
    crate::client::schema::ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let access = ConfigAccess::local(node.clone());
    let seed = access
        .write(
            "test.foreign_edit_projection_row",
            r#"mutation {
                create_AgentRequest(input: {
                    request_id: "foreign-projection-row",
                    purpose: "normal",
                    node_did: "did:test:other-node",
                    requester_did: "did:test:requester",
                    agent_id: "default",
                    session_id: "other-session",
                    content: "foreign row",
                    lifecycle_state: "pending",
                    execution_origin: "interactive",
                    created_at: "2026-10-14T00:00:00Z"
                }) { _docID }
            }"#,
        )
        .await
        .expect("seed foreign row");
    let foreign_id = seed["data"]["add_AgentRequest"][0]["_docID"]
        .as_str()
        .expect("physical ID")
        .to_owned();
    let (store, _version) = ObservedStore::new(ClientStore::default());
    let before = store.projection_revision().store_version;

    let missing = merge_accepted_edit_projection(
        &access,
        store.as_ref(),
        applied_receipt(),
        &["physically-missing".to_string()],
        "did:test:node",
        "session",
        "did:test:requester",
    )
    .await;
    let foreign = merge_accepted_edit_projection(
        &access,
        store.as_ref(),
        applied_receipt(),
        &[foreign_id],
        "did:test:node",
        "session",
        "did:test:requester",
    )
    .await;
    let mut empty_receipt = applied_receipt();
    empty_receipt.request_doc_ids.clear();
    empty_receipt.request_ids.clear();
    let empty = merge_accepted_edit_projection(
        &access,
        store.as_ref(),
        empty_receipt,
        &[],
        "did:test:node",
        "session",
        "did:test:requester",
    )
    .await;

    for receipt in [missing, foreign, empty] {
        assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
    }
    assert_eq!(store.projection_revision().store_version, before);
    assert!(store.snapshot().requests.is_empty());
}
