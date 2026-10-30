use std::sync::Arc;

use crate::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};
use crate::llm::tool::Tool;
use crate::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use crate::store::identity::{KeyIdentity, NodeIdentity};

use super::inference::{aggregate_inference_usage, inference_turns};
use super::*;

#[test]
fn usage_duration_requires_valid_provider_intervals_and_excludes_queue_time() {
    let row: InferenceDetailRow = serde_json::from_value(json!({
        "queued_at":"2026-09-01T12:00:00Z",
        "started_at":"2026-09-01T12:00:10Z",
        "ended_at":"2026-09-01T12:00:11.250Z"
    }))
    .unwrap();
    assert_eq!(
        aggregate_inference_usage(&[row.clone(), row.clone()])
            .unwrap()
            .api_duration_ms,
        Some(2500)
    );
    let mut invalid = row.clone();
    invalid.ended_at = Some("2026-09-01T12:00:09Z".into());
    assert_eq!(
        aggregate_inference_usage(&[invalid])
            .unwrap()
            .api_duration_ms,
        None
    );
    let mut missing = row;
    missing.ended_at = None;
    assert_eq!(
        aggregate_inference_usage(&[missing])
            .unwrap()
            .api_duration_ms,
        None
    );
    assert_eq!(
        aggregate_inference_usage(&[]).unwrap().api_duration_ms,
        Some(0)
    );
}

#[test]
fn inference_rounds_deduplicate_retries_by_physical_owner_not_logical_alias() {
    let accounting = json!({
        "accounting_version":1, "turn_index":2, "attempt":0,
        "estimator":"test", "components":{
            "messages":100, "tool_schemas":0, "documents":0,
            "additional_parameters":0, "output_schema":0},
        "estimated_input_tokens":100, "context_window":2000,
        "compaction_threshold_basis_points":7500, "compaction_threshold_tokens":1500,
        "compaction_reason":"below_threshold"
    });
    let first: InferenceDetailRow = serde_json::from_value(json!({
        "request_doc_id":"physical-a", "request_id":"shared-alias",
        "call_id":"first", "call_kind":"inference",
        "context_accounting_json":accounting.to_string()
    }))
    .unwrap();
    let mut retry = first.clone();
    retry.call_id = "retry".into();
    let mut retry_accounting = accounting.clone();
    retry_accounting["attempt"] = json!(1);
    retry.context_accounting_json = Some(retry_accounting.to_string());
    let mut other = first.clone();
    other.request_doc_id = Some("physical-b".into());
    let mut compact = first.clone();
    compact.call_kind = Some("compaction".into());
    compact.context_accounting_json = None;
    let mut oneoff = compact.clone();
    oneoff.call_kind = Some("oneoff".into());
    assert_eq!(
        inference_turns(&[first.clone(), retry, other, compact, oneoff]).unwrap(),
        Some(2)
    );
    let mut missing = first.clone();
    missing.request_doc_id = None;
    assert_eq!(inference_turns(&[missing]).unwrap(), None);
    let mut unknown = first;
    unknown.call_kind = None;
    assert_eq!(inference_turns(&[unknown]).unwrap(), None);
    assert_eq!(inference_turns(&[]).unwrap(), Some(0));
}

async fn seeded_node() -> Arc<EmbeddedNode> {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identities = tempfile::tempdir().unwrap();
    let node_identity = pin_fixed_signing_identity(identities.path());
    let other_identity =
        KeyIdentity::load_or_create(identities.path().join("other.key"), None).unwrap();
    assert_eq!(node_identity.did(), PIN_FIXED_DID);

    for mutation in [
        r#"mutation {
                create_InferenceProfile(input: {
                    node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                    profile_id: "profile-a",
                    backend_id: "backend-a",
                    model_name: "model-a",
                    context_window: 20000
                }) { _docID }
            }"#,
        r#"mutation {
                create_CompactionConfig(input: {
                    compaction_id: "compaction-a",
                    node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                    strategy: "StripThenSummarize",
                    threshold: 0.9
                }) { _docID }
            }"#,
        r#"mutation {
                create_AgentContext(input: {
                    context_id: "context-a",
                    node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                    compaction_id: "compaction-a"
                }) { _docID }
            }"#,
        r#"mutation {
                create_Agent(input: {
                    agent_id: "agent-a",
                    node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                    inference_profile_id: "profile-a",
                    context_id: "context-a",
                    enabled: true
                }) { _docID }
            }"#,
        r#"mutation {
                create_AgentSession(input: {
                    session_id: "session-a",
                    node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                    agent_id: "agent-a",
                    title: {text: "OpenAI Agent", source: "user"},
                    tags: ["review"],
                    created_at: "2026-06-03T09:55:00Z"
                }) { _docID }
            }"#,
        r#"mutation {
                create_AgentSession(input: {
                    session_id: "session-b",
                    node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                    agent_id: "agent-b",
                    title: {text: "OpenAI Agent", source: "user"},
                    created_at: "2026-06-03T10:55:00Z",
                    closed_at: "2026-06-03T11:01:00Z"
                }) { _docID }
            }"#,
        r#"mutation {
                create_CompactionEntry(input: {
                    compaction_key: "session-a:1",
                    session_id: "session-a",
                    node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                    sequence: 1,
                    original_tokens: 800,
                    compacted_tokens: 400,
                    created_at: "2026-06-03T10:07:00Z"
                }) { _docID }
            }"#,
        r#"mutation {
                create_AgentToolCall(input: {
                    tool_call_key: "session-a:tool:1",
                    request_id: "request-a-new",
                    session_id: "session-a",
                    node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                    tool_name: "read_file",
                    status: "completed",
                    lifecycle_state: "completed",
                    latency_ms: 25
                }) { _docID }
            }"#,
        r#"mutation {
                create_CompactionEntry(input: {
                    compaction_key: "session-a:foreign",
                    session_id: "session-a",
                    node_did: "did:key:z-other",
                    sequence: 99,
                    original_tokens: 9999,
                    compacted_tokens: 9000,
                    created_at: "2026-06-03T10:08:00Z"
                }) { _docID }
            }"#,
    ] {
        let response = node.execute(mutation).await;
        assert!(!response.has_errors(), "seed failed: {:?}", response.errors);
    }

    for (request_id, signer, agent, session, state, created_at) in [
        (
            "request-a-old",
            &node_identity,
            "agent-a",
            "session-a",
            "completed",
            "2026-06-03T10:00:00Z",
        ),
        (
            "request-a-new",
            &node_identity,
            "agent-a",
            "session-a",
            "processing",
            "2026-06-03T10:05:00Z",
        ),
        (
            "request-b-new",
            &node_identity,
            "agent-b",
            "session-b",
            "completed",
            "2026-06-03T11:00:00Z",
        ),
        (
            "request-other-agent",
            &other_identity,
            "agent-c",
            "session-c",
            "completed",
            "2026-06-03T12:00:00Z",
        ),
    ] {
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            request_id,
            signer.did(),
            signer.did(),
            agent,
            session,
            "session history fixture",
            "interactive",
            created_at,
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                signer.did(),
            ),
        );
        crate::sign_agent_request_create(signer, &mut create)
            .await
            .unwrap();
        let response = crate::config_client::ConfigAccess::write_local_response(
            &node,
            "test.session_history.signed_request",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
        let doc_id = crate::graphql::single_mutation_document(&response, "create_AgentRequest")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap();
        let request_doc_id = escape_graphql_string(doc_id);
        let transition = format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }}, input: {{ lifecycle_state: "{state}" }}) {{ _docID }} }}"#
        );
        let response = crate::config_client::ConfigAccess::write_local_response(
            &node,
            "test.session_history.request_state",
            &transition,
        )
        .await
        .unwrap();
        assert!(
            crate::graphql::single_mutation_document(&response, "update_AgentRequest")
                .unwrap()
                .is_some()
        );
        if session != "session-c" {
            let observation = format!(
                r#"mutation {{ update_AgentSession(filter: {{ _and: [
                        {{ node_did: {{ _eq: "{}" }} }},
                        {{ session_id: {{ _eq: "{}" }} }}
                    ] }}, input: {{ observation: {{
                        last_activity_at: "{}",
                        latest_request: {{ request_doc_id: "{}", request_id: "{}", lifecycle_state: "{}" }}
                    }} }}) {{ _docID }} }}"#,
                escape_graphql_string(signer.did()),
                escape_graphql_string(session),
                created_at,
                request_doc_id,
                escape_graphql_string(request_id),
                state
            );
            let response = crate::config_client::ConfigAccess::write_local_response(
                &node,
                "test.session_history.session_observation",
                &observation,
            )
            .await
            .unwrap();
            assert!(
                crate::graphql::single_mutation_document(&response, "update_AgentSession")
                    .unwrap()
                    .is_some()
            );
        }
    }

    let response = node
        .execute(
            r#"{
                    AgentRequest(filter: {request_id: {_eq: "request-a-new"}}) { _docID }
                }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "request lookup failed: {:?}",
        response.errors
    );
    let request_doc_id = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .expect("request-a-new document ID")
        .to_string();

    let node_did: &str = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";
    let canonical_messages = [
        (
            "session-a:1",
            1u32,
            gents_protocol::output::MessageRole::User,
            "2026-06-03T10:01:00Z",
            gents_protocol::output::OutputSource::Authored {
                key: "prompt".to_string(),
            },
        ),
        (
            "session-a:2",
            2u32,
            gents_protocol::output::MessageRole::Assistant,
            "2026-06-03T10:06:00Z",
            gents_protocol::output::OutputSource::ProviderTurn {
                scope: gents_protocol::rendered_request::CaptureScope {
                    kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                    seq: 1,
                },
                turn_index: 1,
                attempt: 0,
            },
        ),
    ];
    for (index, (message_key, sequence, role, created_at, source)) in
        canonical_messages.iter().enumerate()
    {
        let payload = if index == 0 { "hello" } else { "hi" };
        let segment = gents_protocol::output::OutputSegment {
            node_did: node_did.to_string(),
            requester_did: None,
            session_id: "session-a".to_string(),
            request_doc_id: request_doc_id.clone(),
            source: source.clone(),
            writer: gents_protocol::output::OutputWriter::RequestExecution {
                execution_generation: "generation-a".to_string(),
            },
            ordinal: Some(0),
            runs: vec![gents_protocol::output::SegmentRun {
                stream: 0,
                bytes: payload.len() as u32,
                declaration: Some(gents_protocol::output::StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: gents_protocol::output::StreamPayload::Text,
                }),
            }],
            payload: payload.to_string(),
            close: Some(gents_protocol::output::SourceClose::Closed {
                outcome: gents_protocol::output::OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![payload.len() as u64],
            }),
            created_at: (*created_at).to_string(),
        };
        let segment_response = node
            .execute_request(
                defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(&segment).unwrap()),
            )
            .await;
        assert!(
            !segment_response.has_errors(),
            "canonical segment fixture create failed: {:?}",
            segment_response.errors
        );
        let segment_doc_id = segment_response
            .data
            .as_ref()
            .and_then(|data| data.get("add_AgentOutputSegment"))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .expect("pinned add_AgentOutputSegment response shape");
        let close_doc_id = segment_doc_id;
        let message = gents_protocol::output::TranscriptMessage {
            message_key: (*message_key).to_string(),
            session_id: "session-a".to_string(),
            node_did: node_did.to_string(),
            requester_did: None,
            request_doc_id: Some(request_doc_id.clone()),
            publication: gents_protocol::output::MessagePublication::RequestExecution {
                execution_generation: "generation-a".to_string(),
            },
            outcome: gents_protocol::output::OutputOutcome::Complete,
            sequence: *sequence,
            role: *role,
            native_id: None,
            blocks: vec![gents_protocol::output::MessageBlock::Text {
                text: gents_protocol::output::PresentedPayload {
                    output: gents_protocol::output::PayloadRef {
                        close_doc_id: close_doc_id.clone(),
                        stream: 0,
                    },
                    presentation: gents_protocol::output::PayloadPresentation::Full,
                },
            }],
            created_at: (*created_at).to_string(),
        };
        let message_response = node
            .execute_request(
                defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                    .with_variables(transcript_message_create_variables(&message).unwrap()),
            )
            .await;
        assert!(
            !message_response.has_errors(),
            "canonical message fixture create failed: {:?}",
            message_response.errors
        );
        assert!(
            message_response
                .data
                .as_ref()
                .and_then(|data| data.get("add_AgentMessage"))
                .and_then(|rows| rows.as_array())
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(Value::as_str)
                .is_some(),
            "pinned add_AgentMessage response shape: {:?}",
            message_response.data
        );
    }

    let accounting = gents_protocol::rendered_request::ContextAccounting {
        accounting_version: gents_protocol::rendered_request::CONTEXT_ACCOUNTING_VERSION,
        turn_index: 2,
        attempt: 0,
        estimator: "serialized_json_bytes_div_4_v1".to_string(),
        components: gents_protocol::rendered_request::ContextInputComponents {
            messages: 500,
            documents: 25,
            tool_schemas: 100,
            additional_parameters: 20,
            output_schema: 5,
        },
        estimated_input_tokens: 650,
        raw_estimated_input_tokens: None,
        input_calibration_basis_points: None,
        anchored_input_tokens: None,
        context_window: 10_000,
        compaction_threshold_basis_points: 5_700,
        compaction_threshold_tokens: 5_700,
        configured_max_output_tokens: Some(1_000),
        effective_max_output_tokens: Some(1_000),
        compaction_reason:
            gents_protocol::rendered_request::ContextCompactionReason::BelowThreshold,
        pre_compaction_input_tokens: None,
    };
    let accounting = escape_graphql_string(&serde_json::to_string(&accounting).unwrap());
    let mutation = format!(
        r#"mutation {{
            create_InferenceCall(input: {{
                call_id: "session-a:call:1"
                request_id: "request-a-new"
                request_doc_id: "{request_doc_id}"
                node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7"
                call_kind: "inference"
                backend_id: "backend-a"
                call_state: "completed"
                call_seq: 1
                queued_at: "2026-06-03T10:05:30Z"
                prompt_tokens: 100
                completion_tokens: 20
                usage_completeness: "complete"
                cached_input_tokens: 40
                context_accounting_json: "{accounting}"
            }}) {{ _docID }}
        }}"#,
        request_doc_id = escape_graphql_string(&request_doc_id)
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "inference seed failed: {:?}",
        response.errors
    );

    node
}

mod accounting;
mod investigation;
mod listing;
