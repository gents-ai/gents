use gents_lean_vocab::families::session_operations_resolve_delivery::Input as DeliveryInput;
use gents_protocol::operations::request::DeliveryChoice;

use super::{reissue_decision, request_lookup};

#[test]
fn request_lookup_matches_lean() {
    gents_lean_vocab::families::session_operations_request_lookup::replay(|i| {
        request_lookup(i.found)
    });
}

#[test]
fn reissue_decision_matches_lean() {
    gents_lean_vocab::families::session_operations_reissue_decision::replay(|i| {
        reissue_decision(
            i.kind,
            i.parent,
            i.interactive,
            i.budget_remains,
            i.deadline_open,
            i.latest,
        )
    });
}

#[test]
fn delivery_resolution_matches_lean() {
    gents_lean_vocab::families::session_operations_resolve_delivery::replay(
        |i: &DeliveryInput<DeliveryChoice>| i.choice.resolve(),
    );
}

#[tokio::test]
async fn an_unknown_outcome_is_rechecked_under_its_own_identity() -> anyhow::Result<()> {
    use gents_protocol::operations::request::{MessageSendInput, RequestRef};
    use gents_store::identity::NodeIdentity;
    let cases = &gents_lean_vocab::lean_contract_snapshot().storage_failure_cases;
    let unknown: Vec<_> = cases
        .iter()
        .filter(|case| case["outcome_unknown"] == true)
        .collect();
    assert!(!unknown.is_empty());
    for case in unknown {
        assert_eq!(case["same_request"], true);
        assert_eq!(case["same_bytes"], true);
        assert_eq!(case["stop_automatic_retry"], true);
    }
    let (db, user, access, agent) = gents_inference::test_support::signed_test_node(
        "unknown-send-outcome",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let request_id = "storage-unknown-outcome";
    let session_id = "storage-unknown-outcome-session";
    let input = MessageSendInput {
        node_did: db.node_identity.did().to_owned(),
        session_id: Some(session_id.to_owned()),
        agent_did: Some(agent.agent_did),
        request_id: Some(request_id.to_owned()),
        content: "delivery whose outcome is unknown".to_owned(),
        ..Default::default()
    };
    let reference = RequestRef {
        node_did: input.node_did.clone(),
        requester_did: Some(user.did().to_owned()),
        request_id: request_id.to_owned(),
    };
    let first = super::send(&access, &user, input.clone()).await?;
    let committed = super::get(&access, &reference).await?;
    assert_eq!(
        committed.retry_key.as_deref(),
        Some(crate::request::lifecycle::client_request_key(user.did(), request_id).as_str())
    );
    assert!(committed.admission_signature.is_some());
    let rechecked = super::send(&access, &user, input).await?;
    assert_eq!(first, rechecked);
    let after = super::get(&access, &reference).await?;
    assert_eq!(after.request_id, committed.request_id);
    assert_eq!(after.retry_key, committed.retry_key);
    assert_eq!(after.admission_signature, committed.admission_signature);
    assert_eq!(after.content, committed.content);
    let request_rows = super::read::requests_by_id(
        &access,
        &reference.node_did,
        reference.requester_did.as_deref(),
        request_id,
    )
    .await?;
    assert_eq!(request_rows.len(), 1);
    use gents_protocol::document_query::{
        fields::agent_request as fields, rows, session_scope, Query, Selection,
    };
    let query = Query::select(Selection::new().field(fields::DOC_ID))
        .filter(session_scope(
            &reference.node_did,
            session_id,
            reference.requester_did.as_deref(),
        ))
        .render();
    let data = access.read(&query).await?;
    assert_eq!(rows(&data, "AgentRequest").len(), 1);
    db.node.shutdown().await?;
    Ok(())
}
