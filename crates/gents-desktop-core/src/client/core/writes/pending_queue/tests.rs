use std::sync::Arc;

use gents_protocol::session_input_edit::{
    SessionInputEditOutcome, SessionInputEditReceipt, SESSION_INPUT_EDIT_VERSION,
};

use super::*;

#[test]
fn authored_edit_uses_the_send_skill_parser() {
    let mut edit = gents_session::request::lifecycle::PendingQueueEdit {
        expected_request_doc_ids: vec!["request".into()],
        selected_request_doc_ids: vec!["request".into()],
        messages: vec![gents_session::request::lifecycle::PendingMessageEdit {
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

async fn seeded_projection_node(
    request_id: &str,
    node_did: &str,
) -> (Arc<defra_node::Node>, ConfigAccess, String) {
    use gents_store::identity::NodeIdentity as _;
    let directory = tempfile::tempdir().expect("node key directory");
    let identity =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("node.key"), None)
            .expect("node key");
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .expect("local projection node"),
    );
    crate::client::schema::ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let access = ConfigAccess::local_as(
        node.clone(),
        ::identity::Did::new(identity.did()).expect("node DID"),
    );
    let seed = access
        .write(
            "test.edit_projection_row",
            &format!(
                r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request_id}",
                    purpose: "normal",
                    node_did: "{node_did}",
                    requester_did: "did:test:requester",
                    agent_did: "default",
                    session_id: "session",
                    content: "projected row",
                    lifecycle_state: "pending",
                    execution_origin: "interactive",
                    created_at: "2026-10-14T00:00:00Z"
                }}) {{ _docID }}
            }}"#
            ),
        )
        .await
        .expect("seed projection row");
    let doc_id = seed["data"]["add_AgentRequest"][0]["_docID"]
        .as_str()
        .expect("physical ID")
        .to_owned();
    (node, access, doc_id)
}

#[tokio::test]
async fn generated_edit_receipt_outcome_survives_projection_refresh() {
    use gents_lean_vocab::families::fold_queue_contracts_input_edit_client_observation as observation;
    for case in
        gents_lean_vocab::families::fold_queue_contracts_input_edit_client_observation::cases::<
            SessionInputEditOutcome,
        >()
    {
        let store = crate::client::observe::ObservedStores::new().ensure("did:test:node");
        let before = store.projection_revision().store_version;
        let mut receipt = applied_receipt();
        receipt.outcome = case.input.receipt_outcome;
        let mut _node = None;
        let (access, selected) = if case.input.projection_refresh_succeeded {
            let (node, access, doc_id) =
                seeded_projection_node("replacement-request", "did:test:node").await;
            _node = Some(node);
            receipt.request_doc_ids = vec![doc_id];
            (access, Vec::new())
        } else {
            (
                ConfigAccess::Graphql(gents_store::config_client::GraphqlEndpoint::anonymous(
                    "invalid endpoint",
                )),
                vec!["selected-doc".to_string()],
            )
        };
        let (accepted, projection) = merge_accepted_edit_projection(
            &access,
            store.as_ref(),
            receipt,
            &selected,
            "did:test:node",
            "session",
            "did:test:requester",
        )
        .await;
        let projection_fresh = projection.is_ok();
        if projection_fresh {
            assert!(store.projection_revision().store_version > before);
            assert_eq!(store.snapshot().requests.len(), 1);
        } else {
            assert_eq!(store.projection_revision().store_version, before);
            assert!(store.snapshot().requests.is_empty());
        }
        case.check(observation::InputEditClientObservationOutput {
            command_outcome: accepted.outcome,
            projection_fresh,
        });
    }
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
                    agent_did: "default",
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
    let store = crate::client::observe::ObservedStores::new().ensure("did:test:node");
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

    for (receipt, _) in [missing, foreign, empty] {
        assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
    }
    assert_eq!(store.projection_revision().store_version, before);
    assert!(store.snapshot().requests.is_empty());
}
