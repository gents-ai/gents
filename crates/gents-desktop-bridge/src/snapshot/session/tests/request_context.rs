use super::*;
use crate::tests::support::refresh_request_and_assert_fresh_projection;
use gents_protocol::rendered_request::{
    ContextAccounting, ContextCompactionReason, ContextInputComponents, CONTEXT_ACCOUNTING_VERSION,
};

fn accounting_json(estimated_input_tokens: usize) -> String {
    serde_json::to_string(&ContextAccounting {
        accounting_version: CONTEXT_ACCOUNTING_VERSION,
        turn_index: 0,
        attempt: 0,
        estimator: "test".to_string(),
        components: ContextInputComponents {
            messages: estimated_input_tokens,
            documents: 0,
            tool_schemas: 0,
            additional_parameters: 0,
            output_schema: 0,
        },
        estimated_input_tokens,
        raw_estimated_input_tokens: None,
        input_calibration_basis_points: None,
        anchored_input_tokens: None,
        context_window: 10_000,
        compaction_threshold_basis_points: 8_000,
        compaction_threshold_tokens: 8_000,
        configured_max_output_tokens: None,
        effective_max_output_tokens: None,
        compaction_reason: ContextCompactionReason::BelowThreshold,
        pre_compaction_input_tokens: None,
    })
    .expect("accounting json")
}

#[test]
fn newest_accounted_call_survives_a_newer_unaccounted_request() {
    let rows = vec![
        serde_json::json!({
            "request_id": "request-old",
            "call_id": "call-old",
            "call_seq": 1,
            "queued_at": "2026-08-24T12:00:00Z",
            "context_accounting_json": accounting_json(1_000),
        }),
        serde_json::json!({
            "request_id": "request-new",
            "call_id": "call-pending",
            "call_seq": 3,
            "queued_at": "2026-08-24T12:02:00Z",
            "context_accounting_json": null,
        }),
        serde_json::json!({
            "request_id": "request-middle",
            "call_id": "call-middle",
            "call_seq": 2,
            "queued_at": "2026-08-24T12:01:00Z",
            "context_accounting_json": accounting_json(2_000),
        }),
    ];

    let loaded = decode_latest_request_context(&rows)
        .expect("decode context")
        .expect("accounted call");
    assert_eq!(loaded.request_id, "request-middle");
    assert_eq!(loaded.call_id, "call-middle");
    assert_eq!(loaded.call_sequence, 2);
    assert_eq!(loaded.accounting.estimated_input_tokens, 2_000);
}

#[tokio::test]
async fn shared_snapshot_keeps_previous_accounting_until_new_request_dispatches() {
    // The User's requests come through client admission and the working
    // Agent records each inference call as a session child. A logical request
    // id names one physical request per User, so the newer accounting of the
    // same request is a later call of it, and another session's later
    // accounting is a request of that session.
    let served = crate::tests::support::served_desktop().await;
    let core = &served.core;
    // Subscription events cannot supersede the direct reads under test.
    core.stop_observer_for_test().await;
    let node_did = served.node_did().to_owned();
    let session_id = "session-context-side-channel";
    let requester_did = core.node_identity().did().to_owned();
    let submit = |session: &'static str, request: &'static str| {
        crate::tests::support::submit_request(
            core,
            &served.runtime,
            &served.agent,
            session,
            request,
        )
    };
    let accounted_doc = submit(session_id, "request-accounted").await;
    let pending_doc = submit(session_id, "request-pending").await;
    let foreign_doc = submit("session-other-owner", "request-foreign").await;

    for request_id in ["request-accounted", "request-pending"] {
        refresh_request_and_assert_fresh_projection(
            core,
            &node_did,
            session_id,
            Some(&requester_did),
            request_id,
        )
        .await;
        assert!(
            core.stores()
                .snapshot(&node_did)
                .requests
                .iter()
                .any(|row| row.request_id == request_id),
            "refreshed request {request_id} missing from store: {:?}",
            core.stores().snapshot(&node_did).requests
        );
    }

    let session_doc = |request_doc: &str| {
        let access = served.runtime.user_access();
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ session_doc_id }} }}"#,
            gents_store::graphql::escape_graphql_string(request_doc)
        );
        async move {
            access.execute(&query).await.unwrap()["data"]["AgentRequest"][0]["session_doc_id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let own_session = session_doc(&accounted_doc).await;
    let other_session = session_doc(&foreign_doc).await;
    let call = |call_id: &str,
                request_id: &str,
                request_doc: &str,
                call_seq: u32,
                queued_at: &str,
                accounting: Option<String>| {
        let mut call = serde_json::json!({
            "call_id": call_id, "request_id": request_id, "request_doc_id": request_doc,
            "call_seq": call_seq, "node_did": node_did, "agent_did": served.agent.agent_did,
            "call_kind": "inference", "queued_at": queued_at,
        });
        if let Some(accounting) = accounting {
            call["context_accounting_json"] = accounting.into();
        }
        call
    };
    let calls = [
        (
            call(
                "call-accounted",
                "request-accounted",
                &accounted_doc,
                1,
                "2026-08-24T12:01:00Z",
                Some(accounting_json(2_500)),
            ),
            &own_session,
        ),
        (
            call(
                "call-accounted-twin",
                "request-accounted",
                &accounted_doc,
                2,
                "2026-08-24T12:02:00Z",
                Some(accounting_json(3_000)),
            ),
            &own_session,
        ),
        (
            call(
                "call-pending",
                "request-pending",
                &pending_doc,
                3,
                "2026-08-24T12:03:00Z",
                None,
            ),
            &own_session,
        ),
        (
            call(
                "call-foreign-session",
                "request-foreign",
                &foreign_doc,
                4,
                "2026-08-24T12:04:00Z",
                Some(accounting_json(9_999)),
            ),
            &other_session,
        ),
    ];
    served
        .runtime
        .agent_access(&served.agent)
        .await
        .transact_write("test.context_accounting.calls", |txn| {
            let calls = &calls;
            Box::pin(async move {
                for (call, session) in calls {
                    let response = txn
                        .execute_with_variables(
                            "mutation($input: InferenceCallMutationInputArg!) { create_InferenceCall(input: $input) {_docID} }",
                            &serde_json::json!({"input": call}),
                        )
                        .await?;
                    let doc = gents_store::graphql::created_doc_id(&response, "InferenceCall")?;
                    txn.add_resource_relationship(
                        "InferenceCall",
                        &doc,
                        "parent",
                        "sessions",
                        session.as_str(),
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();

    assert!(
        core.stores()
            .snapshot(&node_did)
            .requests
            .iter()
            .any(|row| row.request_id == "request-pending"),
        "pending request lost before snapshot: {:?}",
        core.stores().snapshot(&node_did).requests
    );
    let snapshot = build_session_snapshot_for_node(
        core.as_ref(),
        Some(&node_did),
        session_id,
        Some("request-pending"),
    )
    .await
    .expect("session snapshot");
    assert_eq!(
        snapshot.latest_request_id.as_deref(),
        Some("request-pending")
    );
    let last = snapshot
        .context
        .last_request
        .expect("previous accounted request remains visible");
    assert_eq!(last.request_id, "request-accounted");
    assert_eq!(last.call_id, "call-accounted-twin");
    assert_eq!(last.estimated_input_tokens, 3_000);

    served.shutdown().await;
}
