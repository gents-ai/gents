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
    let node_did = "did:test:context-node";
    let session_id = "session-context-side-channel";
    let requester_did = "did:test:context-requester";
    let requests = format!(
        r#"mutation {{
            accounted: create_AgentRequest(input: {{ purpose: "normal", request_id: "request-accounted", node_did: "{node_did}", requester_did: "{requester_did}", agent_id: "default", session_id: "{session_id}", content: "test", lifecycle_state: "pending", backend_id: "backend", created_at: "2026-08-24T12:00:00Z", retry_count: 0 }}) {{ _docID }}
            pending: create_AgentRequest(input: {{ purpose: "normal", request_id: "request-pending", node_did: "{node_did}", requester_did: "{requester_did}", agent_id: "default", session_id: "{session_id}", content: "test", lifecycle_state: "pending", backend_id: "backend", created_at: "2026-08-24T12:02:00Z", retry_count: 0 }}) {{ _docID }}
            twin: create_AgentRequest(input: {{ purpose: "normal", request_id: "request-accounted", node_did: "{node_did}", requester_did: "{requester_did}", agent_id: "default", session_id: "{session_id}", content: "physical twin", lifecycle_state: "pending", backend_id: "backend", created_at: "2026-08-24T12:00:30Z", retry_count: 0 }}) {{ _docID }}
            foreign: create_AgentRequest(input: {{ purpose: "normal", request_id: "request-accounted", node_did: "{node_did}", requester_did: "{requester_did}", agent_id: "default", session_id: "session-other-owner", content: "other session", lifecycle_state: "pending", backend_id: "backend", created_at: "2026-08-24T12:01:00Z", retry_count: 0 }}) {{ _docID }}
            other_requester: create_AgentRequest(input: {{ purpose: "normal", request_id: "request-accounted", node_did: "{node_did}", requester_did: "did:foreign:requester", agent_id: "default", session_id: "{session_id}", content: "other requester", lifecycle_state: "pending", backend_id: "backend", created_at: "2026-08-24T12:01:30Z", retry_count: 0 }}) {{ _docID }}
        }}"#
    );
    let response = core.node().execute(&requests).await;
    assert!(
        !response.has_errors(),
        "seed requests failed: {:?}",
        response.errors
    );
    let doc_id = |field: &str| {
        gents::graphql::single_mutation_document(&response, field)
            .expect("decode create response")
            .and_then(|row| row.get("_docID"))
            .and_then(serde_json::Value::as_str)
            .expect("created request has physical identity")
            .to_owned()
    };
    let accounted_doc = doc_id("accounted");
    let pending_doc = doc_id("pending");
    let twin_doc = doc_id("twin");
    let foreign_doc = doc_id("foreign");
    let other_requester_doc = doc_id("other_requester");

    let session = gents_protocol::session::AgentSession {
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: Some(requester_did.into()),
        agent_id: "default".into(),
        created_at: "2026-08-24T12:00:00Z".into(),
        closed_at: None,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    };
    let session_input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(&session).expect("session json"),
    )
    .expect("session input");
    let mut updates = core.store_updates();
    let response = core
        .node()
        .execute(&format!(
            "mutation {{ create_AgentSession(input: {session_input}) {{ _docID }} }}"
        ))
        .await;
    assert!(
        !response.has_errors(),
        "seed session failed: {:?}",
        response.errors
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if core
                .store()
                .snapshot()
                .sessions
                .iter()
                .any(|row| row.node_did == node_did && row.session_id == session_id)
            {
                break;
            }
            updates
                .changed()
                .await
                .expect("observed store remains open while waiting for session projection");
        }
    })
    .await
    .expect("session projection arrives");
    for request_id in ["request-accounted", "request-pending"] {
        refresh_request_and_assert_fresh_projection(
            &core,
            node_did,
            session_id,
            Some(requester_did),
            request_id,
        )
        .await;
        assert!(
            core.store()
                .snapshot()
                .requests
                .iter()
                .any(|row| row.request_id == request_id),
            "refreshed request {request_id} missing from store: {:?}",
            core.store().snapshot().requests
        );
    }

    let accounted_json = gents::graphql::escape_graphql_string(&accounting_json(2_500));
    let twin_json = gents::graphql::escape_graphql_string(&accounting_json(3_000));
    let foreign_json = gents::graphql::escape_graphql_string(&accounting_json(9_999));
    let inference_calls = format!(
        r#"mutation {{
            accounted: create_InferenceCall(input: {{
                call_id: "call-accounted",
                request_id: "request-accounted",
                request_doc_id: "{accounted_doc}",
                call_seq: 1,
                node_did: "{node_did}",
                call_kind: "inference",
                queued_at: "2026-08-24T12:01:00Z",
                context_accounting_json: "{accounted_json}"
            }}) {{ _docID }}
            twin: create_InferenceCall(input: {{
                call_id: "call-accounted-twin",
                request_id: "request-accounted",
                request_doc_id: "{twin_doc}",
                call_seq: 2,
                node_did: "{node_did}",
                call_kind: "inference",
                queued_at: "2026-08-24T12:02:00Z",
                context_accounting_json: "{twin_json}"
            }}) {{ _docID }}
            pending: create_InferenceCall(input: {{
                call_id: "call-pending",
                request_id: "request-pending",
                request_doc_id: "{pending_doc}",
                call_seq: 3,
                node_did: "{node_did}",
                call_kind: "inference",
                queued_at: "2026-08-24T12:03:00Z"
            }}) {{ _docID }}
            foreign: create_InferenceCall(input: {{
                call_id: "call-foreign-session",
                request_id: "request-accounted",
                request_doc_id: "{foreign_doc}",
                call_seq: 4,
                node_did: "{node_did}",
                call_kind: "inference",
                queued_at: "2026-08-24T12:04:00Z",
                context_accounting_json: "{foreign_json}"
            }}) {{ _docID }}
            other_requester: create_InferenceCall(input: {{
                call_id: "call-other-requester",
                request_id: "request-accounted",
                request_doc_id: "{other_requester_doc}",
                call_seq: 5,
                node_did: "{node_did}",
                call_kind: "inference",
                queued_at: "2026-08-24T12:05:00Z",
                context_accounting_json: "{foreign_json}"
            }}) {{ _docID }}
        }}"#
    );
    let response = core.node().execute(&inference_calls).await;
    assert!(
        !response.has_errors(),
        "seed inference calls failed: {:?}",
        response.errors
    );

    assert!(
        core.store()
            .snapshot()
            .requests
            .iter()
            .any(|row| row.request_id == "request-pending"),
        "pending request lost before snapshot: {:?}",
        core.store().snapshot().requests
    );
    let snapshot = build_session_snapshot_for_node(
        core.as_ref(),
        Some(node_did),
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

    core.shutdown().await.expect("core shutdown");
}
