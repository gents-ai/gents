use super::*;
use crate::config_client::{ConfigAccess, IdempotentTransactionRetry};
use gents_protocol::request_lifecycle::RequestLifecycleState;

#[tokio::test]
async fn request_head_uses_exact_scope_and_timestamp_then_logical_id() {
    let temp = tempfile::tempdir().unwrap();
    let node = std::sync::Arc::new(
        crate::test_support::node_builder()
            .data_path(temp.path())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    ConfigAccess::transact_local_idempotent(
        &node, None, IdempotentTransactionRetry::Standard, "test.session.request_head",
        |txn| Box::pin(async move {
            for (id, agent, requester, time) in [
                ("a", "did:test:head", None, "2030-01-01T01:00:00+01:00"),
                ("z", "did:test:head", None, "2030-01-01T00:00:00Z"),
                ("remote", "did:test:head", Some("did:test:requester"), "2030-01-01T00:00:01Z"),
                ("foreign", "did:test:foreign", None, "2040-01-01T00:00:00Z"),
            ] {
                txn.execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"request_id": id, "purpose": "normal", "node_did": agent,
                        "requester_did": requester, "session_id": "head-session", "agent_id": "head-agent",
                        "content": "prompt", "created_at": time, "lifecycle_state": "pending"}}),
                ).await?;
            }
            let local = load_latest_request_in_txn(txn, "did:test:head", "head-session", Some(None)).await?.unwrap();
            assert_eq!(local.observed.request_id, "z");
            let all = load_latest_request_in_txn(txn, "did:test:head", "head-session", None).await?.unwrap();
            assert_eq!(all.observed.request_id, "remote");
            let exact = load_latest_request_in_txn(txn, "did:test:head", "head-session", Some(Some("did:test:requester"))).await?.unwrap();
            assert_eq!(exact, all);
            assert!(load_latest_request_in_txn(txn, "did:test:head", "head-session", Some(Some("did:test:absent"))).await?.is_none());
            Ok::<_, anyhow::Error>(())
        }),
    ).await.unwrap();
}

#[tokio::test]
async fn batched_request_heads_preserve_scope_order_and_bound_query_count() {
    let temp = tempfile::tempdir().unwrap();
    let node = std::sync::Arc::new(
        crate::test_support::node_builder()
            .data_path(temp.path())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let session_ids = (0..REQUEST_FACT_SESSION_BATCH_SIZE + 1)
        .map(|index| format!("batch-session-{index:02}"))
        .collect::<Vec<_>>();

    ConfigAccess::transact_local_idempotent(
        &node,
        None,
        IdempotentTransactionRetry::Standard,
        "test.session.batched_request_heads",
        |txn| {
            let session_ids = session_ids.clone();
            Box::pin(async move {
                for (index, session_id) in session_ids.iter().enumerate() {
                    let request_id = format!("request-{index:02}");
                    txn.execute_with_variables(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &serde_json::json!({"input": {
                            "request_id": request_id,
                            "purpose": "normal",
                            "node_did": "did:test:batch-heads",
                            "session_id": session_id,
                            "agent_id": "batch-agent",
                            "content": "prompt",
                            "created_at": "2030-01-01T00:00:00Z",
                            "lifecycle_state": "pending"
                        }}),
                    ).await?;
                }
                for (request_id, purpose, node_did, requester_did, created_at) in [
                    ("a-tie", "normal", "did:test:batch-heads", None, "2030-01-01T01:00:00+01:00"),
                    ("z-tie", "normal", "did:test:batch-heads", None, "2030-01-01T00:00:00Z"),
                    ("remote-head", "normal", "did:test:batch-heads", Some("did:test:requester"), "2030-01-01T00:00:01Z"),
                    ("foreign-node", "normal", "did:test:other-node", None, "2040-01-01T00:00:00Z"),
                    ("title-audit", "title-audit", "did:test:batch-heads", None, "2050-01-01T00:00:00Z"),
                ] {
                    txn.execute_with_variables(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &serde_json::json!({"input": {
                            "request_id": request_id,
                            "purpose": purpose,
                            "node_did": node_did,
                            "requester_did": requester_did,
                            "session_id": session_ids[0],
                            "agent_id": "batch-agent",
                            "content": "prompt",
                            "created_at": created_at,
                            "lifecycle_state": "pending"
                        }}),
                    ).await?;
                }

                let (heads, batch_queries) = count_request_fact_batch_queries(
                    load_latest_requests_in_txn(
                        txn,
                        "did:test:batch-heads",
                        &session_ids,
                        None,
                    ),
                )
                .await;
                let heads = heads?;
                assert_eq!(batch_queries, 2);
                assert_eq!(heads.len(), session_ids.len());
                assert_eq!(heads[&session_ids[0]].observed.request_id, "remote-head");
                assert_eq!(heads[&session_ids[1]].observed.request_id, "request-01");

                let absent_requester = load_latest_requests_in_txn(
                    txn,
                    "did:test:batch-heads",
                    &session_ids[..1],
                    Some(None),
                )
                .await?;
                assert_eq!(absent_requester[&session_ids[0]].observed.request_id, "z-tie");
                let exact_requester = load_latest_requests_in_txn(
                    txn,
                    "did:test:batch-heads",
                    &session_ids[..1],
                    Some(Some("did:test:requester")),
                )
                .await?;
                assert_eq!(exact_requester[&session_ids[0]].observed.request_id, "remote-head");
                Ok::<_, anyhow::Error>(())
            })
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn refresh_accepts_null_requester_and_rejects_changed_session_scope() {
    let temp = tempfile::tempdir().unwrap();
    let node = std::sync::Arc::new(
        crate::test_support::node_builder()
            .data_path(temp.path())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let response = node
        .execute(
            r#"mutation {
        create_AgentRequest(input: {
            request_id: "refresh-request", purpose: "normal", node_did: "did:test:refresh",
            session_id: "refresh-session", agent_id: "refresh-agent",
            content: "prompt", created_at: "2030-01-01T00:00:00Z",
            lifecycle_state: "processing"
        }) { _docID }
    }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    ConfigAccess::transact_local_idempotent(
        &node, None, IdempotentTransactionRetry::Standard, "test.session.refresh",
        |txn| Box::pin(async move {
            let now = "2030-01-01T00:00:01Z";
            crate::session::ensure_session_in_txn(
                txn, "refresh-session", "did:test:refresh", "refresh-agent",
                None, None, None, now,
            ).await?;
            let owner = load_agent_session_row_in_txn(
                txn, "did:test:refresh", "refresh-session", None,
            ).await?.unwrap();
            let facts = load_scoped_request_facts_in_txn(txn, &owner.session, false).await?;
            assert_eq!(facts.len(), 1);
            let fact = &facts[0];
            assert!(advance_session_request_observation_in_txn(txn, fact, "prompt", now).await?);
            let doc_id = escape_graphql_string(&fact.observed.request_doc_id);
            txn.execute_local_response(&format!(r#"mutation {{
                update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{ lifecycle_state: "completed" }}) {{ _docID }}
            }}"#)).await?;
            assert!(refresh_session_request_observation_in_txn(
                txn, "did:test:refresh", None, "refresh-session",
                SessionRequestRefresh { request_doc_id: &fact.observed.request_doc_id, request_id: "refresh-request", reply_preview: Some(Some("Finished")) }, now,
            ).await?);
            patch_session_in_txn(txn, &owner.doc_id, serde_json::json!({
                "closed_at": "2030-01-01T00:00:02Z", "tags": ["review"]
            })).await?;
            assert!(crate::session::reopen_session_in_txn(
                txn, "refresh-session", "did:test:refresh", None,
                "2030-01-01T00:00:03Z",
            ).await?);
            let reopened = load_agent_session_row_in_txn(
                txn, "did:test:refresh", "refresh-session", None,
            ).await?.unwrap().session;
            assert!(reopened.closed_at.is_none());
            assert_eq!(reopened.created_at, now);
            assert_eq!(reopened.tags, vec!["review"]);
            assert_eq!(reopened.observation.as_ref().unwrap().reply_preview.as_deref(), Some("Finished"));
            assert_eq!(reopened.observation.unwrap().latest_request.unwrap().lifecycle_state,
                RequestLifecycleState::Completed);
            // A malformed/stale observation must not import state from a
            // different immutable request scope, even when its logical
            // request label matches.
            txn.execute_with_variables(
                "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                &serde_json::json!({"input": {
                    "request_id": "refresh-request",
                    "purpose": "normal",
                    "node_did": "did:test:refresh",
                    "session_id": "other-session",
                    "agent_id": "refresh-agent",
                    "content": "foreign prompt",
                    "created_at": "2030-01-01T00:00:04Z",
                    "lifecycle_state": "failed"
                }}),
            ).await?;
            let foreign = txn.execute_local_response(r#"{
                AgentRequest(filter: {
                    request_id: {_eq: "refresh-request"},
                    session_id: {_eq: "other-session"}
                }) {_docID}
            }"#).await?;
            let foreign_doc_id = foreign.data.as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(serde_json::Value::as_array)
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(serde_json::Value::as_str)
                .context("foreign request omitted _docID")?;
            patch_session_in_txn(txn, &owner.doc_id, serde_json::json!({
                "observation": {
                    "last_activity_at": "2030-01-01T00:00:03Z",
                    "preview": "prompt",
                    "reply_preview": "Finished",
                    "latest_request": {
                        "request_doc_id": foreign_doc_id,
                        "request_id": "refresh-request",
                        "lifecycle_state": "completed"
                    }
                }
            })).await?;
            assert!(!refresh_session_request_observation_in_txn(
                txn, "did:test:refresh", None, "refresh-session",
                SessionRequestRefresh { request_doc_id: foreign_doc_id, request_id: "refresh-request", reply_preview: Some(Some("Wrong scope")) }, now,
            ).await?);
            let session = load_agent_session_row_in_txn(
                txn, "did:test:refresh", "refresh-session", None,
            ).await?.unwrap().session;
            assert_eq!(session.observation.as_ref().unwrap().reply_preview.as_deref(), Some("Finished"));
            assert_eq!(session.observation.unwrap().latest_request.unwrap().lifecycle_state,
                RequestLifecycleState::Completed);
            Ok::<_, anyhow::Error>(())
        }),
    ).await.unwrap();
}
