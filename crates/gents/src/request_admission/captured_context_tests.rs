use std::sync::Arc;

use crate::config_client::ConfigAccess;
use crate::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::RequestInput;

#[tokio::test]
async fn generated_captures_use_signed_storage_and_fresh_admission() {
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("capture.key"), None).unwrap();
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    let owner = crate::graphql::escape_graphql_string(identity.did());
    for mutation in [
        format!(
            r#"mutation {{ create_AgentContext(input: {{ context_id: "context", node_did: "{owner}" }}) {{ _docID }} }}"#
        ),
        format!(
            r#"mutation {{ create_Agent(input: {{ agent_id: "coding", node_did: "{owner}", context_id: "context", inference_profile_id: "inference", enabled: true }}) {{ _docID }} }}"#
        ),
    ] {
        ConfigAccess::write_local(&node, "test.capture.setup", &mutation)
            .await
            .unwrap();
    }
    let access = ConfigAccess::local(node.clone());
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
            identity.did(),
            "coding",
            "capture-session",
            "Captured source",
            "interactive",
            "2026-10-20T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        create.input = input;
        crate::sign_agent_request_create(&identity, &mut create)
            .await
            .unwrap();
        let response = ConfigAccess::write_local_response(
            &node,
            "test.capture.request",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
        let doc = crate::graphql::single_mutation_document(&response, "create_AgentRequest")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        let request = super::load_request_for_admission_test(&node, &doc)
            .await
            .unwrap();
        let result =
            super::verify_fresh_local_self_request(&node, &identity, &request, "coding").await;
        assert_eq!(
            result.is_ok(),
            case["expected_accepted"].as_bool().unwrap(),
            "{name}: actual signed admission: {result:?}",
        );
        let stored = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}) {{ {} }} }}"#,
                crate::graphql::escape_graphql_string(&doc),
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
    node.shutdown().await;
}
