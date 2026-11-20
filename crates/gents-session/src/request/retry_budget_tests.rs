use super::*;
use crate::request::lifecycle::test_support::{agent_access, agent_identity, test_node};
use crate::request::lifecycle::{
    ClaimOutcome, ExecutionBinding, ExecutionOrigin, RequestLifecycle,
};

#[test]
fn generated_claim_pins_match_the_model() {
    use gents_lean_vocab::families::request_pinned_retry_budget_claim_pins as family;
    family::replay(|input| {
        let result = claim_pin(
            input.normal,
            input.configured,
            input.retry_successor,
            input.parent_pin,
        );
        family::Outcome {
            accepted: result.is_ok(),
            error: result.as_ref().err().map(ToString::to_string),
            max_retries: result.ok(),
        }
    });
}

#[test]
fn generated_successor_budgets_match_the_model() {
    use gents_lean_vocab::families::request_pinned_retry_budget_successor_budgets as family;
    family::replay(|input| {
        successor_budget_remains(input.automated, input.retry_count, input.max_retries)
    });
}

/// Each modeled pin is stored as a real signed row: a Node-signed pin under
/// the Node's key, any other under a second key, and the reader verifies the
/// signature against the request's Node DID before choosing.
#[tokio::test]
async fn generated_pin_reads_match_the_model() {
    use gents_lean_vocab::families::request_pinned_retry_budget_read_pins_cases as family;
    let keys = tempfile::tempdir().unwrap();
    let node =
        gents_store::identity::KeyIdentity::load_or_create(keys.path().join("node"), None).unwrap();
    let other = gents_store::identity::KeyIdentity::load_or_create(keys.path().join("other"), None)
        .unwrap();
    let request = "bae-request";
    for case in family::cases() {
        let mut rows = Vec::new();
        for pin in &case.input.pins {
            let signer: &dyn NodeIdentity = if pin.signed_by_node { &node } else { &other };
            let signature = encode_signature(
                signer
                    .sign(&payload(node.did(), request, pin.max_retries))
                    .await
                    .unwrap(),
            );
            rows.push(serde_json::json!({
                "request_doc_id": request,
                "max_retries": pin.max_retries,
                "signature": signature,
            }));
        }
        let response = serde_json::json!({ "data": { PIN_COLLECTION: rows } });
        case.check(accepted_pins(node.did(), &response).remove(request));
    }
}

/// The working Agent claims its request and the Node pins the budget; ACP then
/// refuses the Agent's update and delete of that pin, and a pin the Agent
/// writes itself is not one the reader accepts (Lean
/// `Identity.RequestRetryBudget`).
#[tokio::test]
async fn the_working_agent_cannot_change_its_pinned_budget() {
    let (node, _db, identity, agent) = test_node().await;
    let working_agent = agent_identity(&identity, &agent);
    let mut lifecycle = RequestLifecycle::materialize_pending_with_execution_binding(
        node.clone(),
        &agent.agent_did,
        identity.clone(),
        "hello",
        ExecutionBinding {
            deadline_duration_secs: 60,
            execution_origin: ExecutionOrigin::Interactive,
            backend_id: "pin-test".into(),
        },
        Default::default(),
        Some(working_agent),
    )
    .await
    .unwrap();
    lifecycle.set_configured_max_request_retries(5, identity.clone());
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let request_doc = lifecycle.request().doc_id.clone();
    let node_did = identity.did().to_owned();
    let as_agent = agent_access(&node, &identity, &agent);
    assert_eq!(
        read_pin(&as_agent, &node_did, &request_doc).await.unwrap(),
        Some(5)
    );

    let pin_rows = |access: ConfigAccess| {
        let query = format!(
            r#"{{ {PIN_COLLECTION}(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID max_retries }} }}"#,
            escape_graphql_string(&request_doc)
        );
        async move {
            access.execute(&query).await.unwrap()["data"][PIN_COLLECTION]
                .as_array()
                .cloned()
                .unwrap()
        }
    };
    let rows = pin_rows(as_agent.clone()).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let pin_doc = rows[0]["_docID"].as_str().unwrap().to_owned();

    let update = as_agent
        .write(
            "test.retry_budget.agent_update",
            &format!(
                r#"mutation {{ update_{PIN_COLLECTION}(docID: "{}", input: {{ max_retries: 50 }}) {{ _docID }} }}"#,
                escape_graphql_string(&pin_doc)
            ),
        )
        .await;
    assert!(
        update.as_ref().map_or(true, |response| response["data"]
            [format!("update_{PIN_COLLECTION}")]
        .as_array()
        .is_none_or(Vec::is_empty)),
        "the working Agent updated the pin: {update:?}"
    );
    let delete = as_agent
        .write(
            "test.retry_budget.agent_delete",
            &format!(
                r#"mutation {{ delete_{PIN_COLLECTION}(docID: "{}") {{ _docID }} }}"#,
                escape_graphql_string(&pin_doc)
            ),
        )
        .await;
    assert!(
        delete.as_ref().map_or(true, |response| response["data"]
            [format!("delete_{PIN_COLLECTION}")]
        .as_array()
        .is_none_or(Vec::is_empty)),
        "the working Agent deleted the pin: {delete:?}"
    );
    let rows = pin_rows(as_agent.clone()).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["max_retries"], 5, "{rows:?}");

    let agent_signer = agent_identity(&identity, &agent);
    let forged = encode_signature(
        agent_signer
            .sign(&payload(&node_did, &request_doc, 100))
            .await
            .unwrap(),
    );
    as_agent
        .write(
            "test.retry_budget.agent_forge",
            &format!(
                r#"mutation {{ create_{PIN_COLLECTION}(input: {{ request_doc_id: "{}", node_did: "{}", max_retries: 100, signature: "{forged}" }}) {{ _docID }} }}"#,
                escape_graphql_string(&request_doc),
                escape_graphql_string(&node_did),
            ),
        )
        .await
        .unwrap();
    assert_eq!(pin_rows(as_agent.clone()).await.len(), 2);
    assert_eq!(
        read_pin(&as_agent, &node_did, &request_doc).await.unwrap(),
        Some(5),
        "a pin the Agent wrote is not the Node's"
    );
    node.shutdown().await.unwrap();
}
