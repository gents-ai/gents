use std::sync::Arc;

use crate::store::config_client::ConfigAccess;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::RequestInput;

#[tokio::test]
async fn generated_captures_use_signed_storage_and_fresh_admission() {
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("capture.key"), None).unwrap();
    let user = KeyIdentity::load_or_create(temp.path().join("user.key"), None).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let owner = crate::store::graphql::escape_graphql_string(identity.did());
    let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
    let agent_identity =
        crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
    let agent = crate::store::graphql::escape_graphql_string(&binding.agent_did);
    let nonce = crate::store::graphql::escape_graphql_string(&binding.creation_nonce);
    // The runtime owns this fixture's Node, Agent and session setup; the
    // requester only appears as the session's user and signs nothing here.
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(identity.did()).unwrap());
    let node_response = access
        .write(
            "test.capture.node",
            &format!(
                r#"mutation {{ create_Node(input: {{ node_did: "{owner}" }}) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();
    let node_doc = crate::store::graphql::created_doc_id(&node_response, "Node").unwrap();
    let mut agent_doc = None;
    for mutation in [
        format!(
            r#"mutation {{ create_AgentContext(input: {{ context_id: "context", node_did: "{owner}" }}) {{ _docID }} }}"#
        ),
        format!(
            r#"mutation {{ create_Agent(input: {{ agent_did: "{agent}", creation_nonce: "{nonce}", node_did: "{owner}", context_id: "context", inference_profile_id: "inference", enabled: true }}) {{ _docID }} }}"#
        ),
    ] {
        let response = access.write("test.capture.setup", &mutation).await.unwrap();
        if mutation.contains("create_Agent(input:") {
            agent_doc = Some(crate::store::graphql::created_doc_id(&response, "Agent").unwrap());
        }
    }
    let agent_doc = agent_doc.unwrap();
    access
        .add_actor_relationship(
            "Node",
            &node_doc,
            "runtime_reader",
            &identity::Did::new(identity.did()).unwrap(),
        )
        .await
        .unwrap();
    access
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await
        .unwrap();
    access
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &identity::Did::new(&binding.agent_did).unwrap(),
        )
        .await
        .unwrap();
    access
        .transact_write("test.capture.session", |txn| {
            Box::pin(async {
                crate::session::ensure_session_in_txn(
                    txn,
                    "capture-session",
                    identity.did(),
                    &binding.agent_did,
                    Some(user.did()),
                    None,
                    None,
                    "2026-10-20T00:00:00Z",
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let session_doc = access
        .transact_readonly("test.capture.session_document", |txn| {
            Box::pin(async {
                crate::session::load_agent_session_owner_in_txn(
                    txn,
                    identity.did(),
                    "capture-session",
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap()
        .doc_id;
    crate::session::grant_created_session_relationships(
        &access,
        &session_doc,
        &agent_doc,
        &binding.agent_did,
    )
    .await
    .unwrap();
    let mut observed = 0;
    for (index, case) in crate::lean_vocab_test::lean_contract_snapshot()
        .request_input_cases
        .iter()
        .enumerate()
    {
        let input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        if input.captured_context.is_empty() || input.cwd.is_some() {
            continue;
        }
        observed += 1;
        let name = case["name"].as_str().unwrap();
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            format!("capture-{index}"),
            identity.did(),
            &binding.agent_did,
            &binding.agent_did,
            "capture-session",
            "Captured source",
            "interactive",
            "2026-10-20T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(&binding.agent_did),
        );
        create.session_doc_id = Some(session_doc.clone());
        create.agent_doc_id = Some(agent_doc.clone());
        create.input = input;
        crate::sign_agent_request_create(agent_identity.as_ref(), &mut create)
            .await
            .unwrap();
        let response = access
            .write("test.capture.request", &create.graphql_mutation().unwrap())
            .await
            .unwrap();
        let documents = crate::store::graphql::mutation_documents(&response, "create_AgentRequest")
            .unwrap()
            .unwrap();
        assert_eq!(documents.len(), 1);
        let doc = documents[0]["_docID"].as_str().unwrap().to_owned();
        access
            .add_resource_relationship("AgentRequest", &doc, "parent", "agents", &agent_doc)
            .await
            .unwrap();
        access
            .add_resource_relationship("AgentRequest", &doc, "session", "sessions", &session_doc)
            .await
            .unwrap();
        access
            .add_actor_relationship(
                "AgentRequest",
                &doc,
                "working_agent",
                &identity::Did::new(&binding.agent_did).unwrap(),
            )
            .await
            .unwrap();
        access.write("test.capture.request_relationships", &format!(
            r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ relationships_pending: false }}) {{ _docID }} }}"#,
            crate::store::graphql::escape_graphql_string(&doc),
        )).await.unwrap();
        let request = super::load_request_for_admission_test(&node, &doc)
            .await
            .unwrap();
        let result =
            super::verify_fresh_local_self_request(&node, &identity, &request, &binding.agent_did)
                .await;
        assert_eq!(
            result.is_ok(),
            case["expected_accepted"].as_bool().unwrap(),
            "{name}: actual signed admission: {result:?}",
        );
        let stored = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}) {{ {} }} }}"#,
                crate::store::graphql::escape_graphql_string(&doc),
                crate::SIGNED_REQUEST_FIELDS,
            ))
            .await
            .unwrap();
        let mut row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(stored["data"]["AgentRequest"][0].clone()).unwrap();
        crate::verify_request_receipt_signature(&row).unwrap();
        row.input.as_mut().unwrap().captured_context[0]
            .content
            .push('!');
        assert!(
            crate::verify_request_receipt_signature(&row).is_err(),
            "{name}"
        );
    }
    assert!(
        observed > 0,
        "generated captures must exercise fresh admission"
    );
    node.shutdown().await.expect("shut down test node");
}
