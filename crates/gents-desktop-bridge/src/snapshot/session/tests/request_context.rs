use super::*;
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
    let (core, _tempdir) = crate::tests::support::boot_core().await;
    let agent_did = "did:test:context-agent";
    let session_id = "session-context-side-channel";
    for (request_id, created_at) in [
        ("request-accounted", "2026-08-24T12:00:00Z"),
        ("request-pending", "2026-08-24T12:02:00Z"),
    ] {
        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "default",
                    session_id: "{session_id}",
                    content: "test",
                    status: "pending",
                    lifecycle_state: "pending",
                    backend_id: "backend",
                    created_at: "{created_at}",
                    retry_count: 0
                }}) {{ _docID }}
            }}"#
        );
        let response = core.node().execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "seed request failed: {:?}",
            response.errors
        );
        core.refresh_local_request(agent_did, request_id)
            .await
            .expect("refresh request");
    }

    let accounted_json = gents::graphql::escape_graphql_string(&accounting_json(2_500));
    let inference_calls = format!(
        r#"mutation {{
            accounted: create_InferenceCall(input: {{
                call_id: "call-accounted",
                request_id: "request-accounted",
                call_seq: 1,
                agent_did: "{agent_did}",
                call_kind: "inference",
                queued_at: "2026-08-24T12:01:00Z",
                context_accounting_json: "{accounted_json}"
            }}) {{ _docID }}
            pending: create_InferenceCall(input: {{
                call_id: "call-pending",
                request_id: "request-pending",
                call_seq: 2,
                agent_did: "{agent_did}",
                call_kind: "inference",
                queued_at: "2026-08-24T12:03:00Z"
            }}) {{ _docID }}
        }}"#
    );
    let response = core.node().execute(&inference_calls).await;
    assert!(
        !response.has_errors(),
        "seed inference calls failed: {:?}",
        response.errors
    );

    let snapshot = build_session_snapshot_for_agent(
        core.as_ref(),
        Some(agent_did),
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
    assert_eq!(last.call_id, "call-accounted");
    assert_eq!(last.estimated_input_tokens, 2_500);

    core.shutdown().await.expect("core shutdown");
}
