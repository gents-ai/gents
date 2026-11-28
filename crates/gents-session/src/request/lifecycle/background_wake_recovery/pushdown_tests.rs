use super::{actor_wake_context_query, load_candidates};
use crate::request::lifecycle::{background_wake_next_retry_at, background_wake_retry_delay};
use crate::test_support::AgentHome;
use gents_store::identity::NodeIdentity;
use serde_json::json;

#[test]
fn background_wake_backoff_uses_exponential_delay_up_to_the_cap() {
    assert_eq!(background_wake_retry_delay(0), chrono::Duration::seconds(5));
    assert_eq!(
        background_wake_retry_delay(1),
        chrono::Duration::seconds(10)
    );
    assert_eq!(
        background_wake_retry_delay(2),
        chrono::Duration::seconds(20)
    );
    assert_eq!(
        background_wake_retry_delay(3),
        chrono::Duration::seconds(40)
    );
    // From the fourth attempt on, the delay saturates so the recovery sweep
    // keeps a bounded cadence instead of backing off unboundedly.
    assert_eq!(
        background_wake_retry_delay(4),
        chrono::Duration::seconds(60)
    );
}

#[test]
fn background_wake_backoff_stays_bounded_for_extreme_retry_counts() {
    for retry_count in [0, 1, 2, 3, 4, 10, 30, 31, 63, 64, i64::MAX] {
        let delay = background_wake_retry_delay(retry_count);
        assert!(
            delay <= chrono::Duration::seconds(60),
            "retry_count={retry_count} exceeded the 60s cap: {delay:?}"
        );
        assert!(
            delay >= chrono::Duration::seconds(5),
            "retry_count={retry_count} fell below the 5s base: {delay:?}"
        );
    }
    // A negative persisted retry count must clamp to the base delay.
    assert_eq!(
        background_wake_retry_delay(-7),
        chrono::Duration::seconds(5)
    );
}

#[test]
fn absent_or_malformed_terminal_timestamp_returns_none() {
    // No persisted terminal timestamp yields no backoff anchor.
    assert_eq!(background_wake_next_retry_at(None, 1), None);
    // A malformed timestamp yields None: no panic, no invented delay.
    assert_eq!(
        background_wake_next_retry_at(Some("not-a-timestamp"), 1),
        None
    );
    assert_eq!(
        background_wake_next_retry_at(Some("2026-13-45T99:99:99Z"), 1),
        None
    );
    // A valid anchor yields terminalized_at + the retry delay.
    let anchored = background_wake_next_retry_at(Some("2026-08-12T22:00:00Z"), 1)
        .expect("valid rfc3339 terminal timestamp yields a backoff anchor");
    assert_eq!(
        anchored,
        chrono::DateTime::parse_from_rfc3339("2026-08-12T22:00:10Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );
}

#[test]
fn wake_context_query_scopes_successors_and_pending_by_agent_did() {
    let query = actor_wake_context_query("did:key:node-1", "did:key:agent-1");
    assert_eq!(
        query
            .matches(r#"agent_did: { _eq: "did:key:agent-1" }"#)
            .count(),
        2,
        "both successors and pending must filter by agent_did: {query}"
    );
}

#[tokio::test]
async fn load_candidates_short_circuits_when_no_failed_wakes_exist() {
    let db = AgentHome::start("wake-short-circuit").await;
    let node_did = db.node_did();
    let agent_did = db.agent_did();
    let requester = db.user.did();
    let session = "wake-short-circuit-session";
    let session_doc = crate::test_support::create_agent_owned_session_at(
        &db.agent_access,
        node_did,
        session,
        Some(requester),
        "2026-01-01T00:00:00Z",
    )
    .await;

    let literal = gents_protocol::graphql::graphql_input_literal(&json!({
        "request_id": "pending-only-req",
        "purpose": "normal",
        "node_did": node_did,
        "requester_did": requester,
        "agent_did": agent_did,
        "session_id": session,
        "content": "pending request",
        "execution_origin": "interactive",
        "lifecycle_state": "pending",
        "created_at": "2026-01-01T00:00:01Z",
        "retry_count": 0,
        "retry_root_request": "pending-only-req",
        "terminal_redrive_attempts": 0,
        "request_hop": 0,
    }))
    .unwrap();
    db.agent_access
        .transact_write("test.wake_short_circuit.pending", |txn| {
            let literal = &literal;
            let session_doc = &session_doc;
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        "mutation {{ create_AgentRequest(input: {literal}) {{ _docID }} }}"
                    ))
                    .await?;
                let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
                let (agent_doc, _) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("fixture Agent unavailable"))?;
                crate::request::lifecycle::stage_request_relationships_in_txn(
                    txn,
                    &doc,
                    &agent_doc,
                    session_doc,
                    agent_did,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();

    let accesses =
        std::collections::BTreeMap::from([(agent_did.to_owned(), db.agent_access.clone())]);
    let (failed, successors, pending) = load_candidates(node_did, &accesses).await.unwrap();
    assert!(failed.is_empty());
    assert!(
        successors.is_empty() && pending.is_empty(),
        "successors and pending must be short-circuited when no failed scheduled wakes exist"
    );
    db.node.shutdown().await.expect("shut down test node");
}
