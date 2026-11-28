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
    let query = actor_wake_context_query(
        "did:key:node-1",
        "did:key:agent-1",
        &["bae-doc-1"],
        &["session-1"],
    );
    assert_eq!(
        query
            .matches(r#"agent_did: { _eq: "did:key:agent-1" }"#)
            .count(),
        2,
        "both successors and pending must filter by agent_did: {query}"
    );
    assert!(
        query.contains(r#"retry_parent_request_doc_id: { _in: ["bae-doc-1"] }"#),
        "successors must filter by candidate doc IDs: {query}"
    );
    assert!(
        query.contains(r#"session_id: { _in: ["session-1"] }"#),
        "pending must filter by candidate sessions: {query}"
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

#[tokio::test]
async fn redrive_skips_pin_queries_for_already_redriven_and_non_wake_rows() {
    use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};

    let db = AgentHome::start("wake-skip-pins").await;
    let node_did = db.node_did();
    let agent_did = db.agent_did();
    let requester = db.user.did();
    let session = "wake-skip-pins-session";
    let session_doc = crate::test_support::create_agent_owned_session_at(
        &db.agent_access,
        node_did,
        session,
        Some(requester),
        "2026-01-01T00:00:00Z",
    )
    .await;

    let wake_input = RequestInput {
        queue: Some(RequestQueue {
            delivery: Default::default(),
            position: None,
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(QueueSource::BackgroundCompletion.queue_key(session)),
            queued_after_request_id: None,
            interrupted_request_id: None,
            background_completion_wake_version: Some(1),
        }),
        ..Default::default()
    };

    let create_row = |input: serde_json::Value| {
        let access = db.agent_access.clone();
        let session_doc = session_doc.clone();
        async move {
            let literal = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
            access
                .transact_write("test.wake_skip_pins.create", |txn| {
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
                        Ok::<_, anyhow::Error>(doc)
                    })
                })
                .await
                .unwrap()
        }
    };

    // Failed scheduled non-wake request (e.g., cron job).
    create_row(json!({
        "request_id": "failed-cron-req",
        "purpose": "normal",
        "node_did": node_did,
        "requester_did": requester,
        "agent_did": agent_did,
        "session_id": session,
        "content": "scheduled task",
        "execution_origin": "scheduled",
        "lifecycle_state": "failed",
        "created_at": "2026-01-01T00:00:01Z",
        "terminalized_at": "2026-01-01T00:00:02Z",
        "retry_count": 0,
        "retry_root_request": "failed-cron-req",
        "terminal_redrive_attempts": 0,
        "request_hop": 0,
    }))
    .await;

    // Failed background-completion wake that already has a successor.
    let failed_wake_doc = create_row(json!({
        "request_id": "failed-wake-req",
        "purpose": "normal",
        "node_did": node_did,
        "requester_did": requester,
        "agent_did": agent_did,
        "session_id": session,
        "content": "wake",
        "input": wake_input,
        "execution_origin": "scheduled",
        "lifecycle_state": "failed",
        "created_at": "2026-01-01T00:00:03Z",
        "terminalized_at": "2026-01-01T00:00:04Z",
        "retry_count": 0,
        "retry_root_request": "failed-wake-req",
        "terminal_redrive_attempts": 0,
        "request_hop": 0,
    }))
    .await;
    create_row(json!({
        "request_id": "successor-wake-req",
        "purpose": "normal",
        "node_did": node_did,
        "requester_did": requester,
        "agent_did": agent_did,
        "session_id": session,
        "content": "wake retry",
        "input": wake_input,
        "execution_origin": "scheduled",
        "lifecycle_state": "completed",
        "created_at": "2026-01-01T00:00:10Z",
        "retry_count": 1,
        "retry_parent_request": "failed-wake-req",
        "retry_parent_request_doc_id": failed_wake_doc,
        "retry_root_request": "failed-wake-req",
        "retry_key": format!("retry:doc:{failed_wake_doc}"),
        "terminal_redrive_attempts": 0,
        "request_hop": 0,
    }))
    .await;

    let agent_identity: std::sync::Arc<dyn NodeIdentity> = db.agent_identity.clone();
    let (report, counts) = gents_store::config_client::count_statements(
        crate::request::lifecycle::RequestLifecycle::redrive_failed_background_wakeups_with_identities(
            &db.node,
            node_did,
            std::slice::from_ref(&agent_identity),
        ),
    )
    .await;
    let report = report.unwrap();
    assert_eq!(report.scanned, 2);
    assert_eq!(report.ineligible, 1);
    assert_eq!(report.already_redriven, 1);
    assert_eq!(report.redriven, 0);
    assert_eq!(
        counts.total(),
        2,
        "sweep should run only the failed-candidate query and one scoped context query"
    );
    assert!(
        !counts
            .statements()
            .any(|(_, doc)| doc.contains(gents_protocol::schemas::AGENT_REQUEST_RETRY_BUDGET_NAME)),
        "already-redriven and non-wake rows must never query AgentRequestRetryBudget"
    );
    assert!(
        !counts
            .statements()
            .any(|(_, doc)| doc.contains("retry_parent_request_doc_id: { _neq: null }")),
        "successor lookup must be scoped to candidate doc IDs rather than scanning all retries"
    );
    db.node.shutdown().await.expect("shut down test node");
}
