use super::*;
use crate::identity::{AgentIdentity, KeyIdentity};
use crate::lifecycle::{ClaimOutcome, RequestLifecycle, RequestTerminalOutcome, TerminalizeResult};
use crate::streaming::DefraStreamWriter;
use crate::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

async fn execute(node: &EmbeddedNode, query: &str) -> Value {
    let response = node.execute(query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()
}

async fn persist_request(node: &EmbeddedNode, create: &AgentRequestCreate) -> AgentRequestRow {
    execute(node, &create.graphql_mutation().unwrap()).await;
    let data = execute(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&create.request_id),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ),
    )
    .await;
    serde_json::from_value(data["AgentRequest"][0].clone()).unwrap()
}

/// Claim a signed request through the production claim path and begin its
/// owned execution so a canonical provider attempt can publish under it.
/// Claim deduplication orders same-session requests by (created_at,
/// request_id); every earlier same-session request must already be terminal,
/// so the caller passes the still-claimed previous lifecycle plus the
/// assistant header doc ID its publication produced for terminalization
/// before the next claim. A zero-write predecessor published nothing, so it
/// terminalizes Failed with `TerminalOutput::NoMessage` — the only selection
/// valid without an owned assistant header, and one the finalize policy
/// admits directly from the claimed state.
async fn claimed(
    node: &Arc<EmbeddedNode>,
    request: &AgentRequestRow,
    previous: Option<(RequestLifecycle, Option<String>)>,
) -> RequestLifecycle {
    if let Some((mut previous, message_doc_id)) = previous {
        let selection = match message_doc_id {
            Some(message_doc_id) => {
                gents_protocol::output::TerminalOutput::Message { message_doc_id }
            }
            None => gents_protocol::output::TerminalOutput::NoMessage,
        };
        previous
            .terminalize_owned(
                RequestTerminalOutcome::Failed,
                selection,
                Some("fixture request terminal before the next same-session claim"),
            )
            .await
            .unwrap();
    }
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        request.agent_did.as_deref().expect("fixture agent DID"),
        request.clone().try_into().unwrap(),
        60,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

async fn begin_writer(
    lifecycle: &mut RequestLifecycle,
    node: &Arc<EmbeddedNode>,
) -> DefraStreamWriter {
    let writer = DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(1));
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    writer
}

/// Publish one canonical provider turn carrying the actual accepted tool call
/// arguments. ONE close for all streams of the source (the native encoder
/// emits exactly one stream, and the single close is terminal-only).
async fn publish_accepted_turn(
    writer: &DefraStreamWriter,
    lifecycle: &mut RequestLifecycle,
    turn: usize,
    attempt: u32,
    tool_call_id: &str,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> (crate::tool_call_lifecycle::AcceptedToolCall, String) {
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            turn,
            attempt,
            "inference.1".parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        id: Some("provider-message".into()),
        content: vec![gents_protocol::message::AssistantContent::ToolCall(
            gents_protocol::message::ToolCall {
                id: tool_call_id.into(),
                call_id: None,
                function: gents_protocol::message::ToolFunction::new(
                    tool_name.into(),
                    arguments.clone(),
                ),
                signature: None,
                additional_params: None,
            },
        )],
    };
    let mut published = writer
        .publish_native_turn(lifecycle, turn, attempt, &message)
        .await
        .unwrap();
    assert_eq!(
        published.accepted_tools.len(),
        1,
        "canonical publication must accept exactly one tool call"
    );
    let accepted = published.accepted_tools.pop().unwrap();
    (accepted, published.message_doc_id)
}

/// Drive the modeled completion flag through the production terminal owner.
/// Failed writes retain their output but must not satisfy the obligation.
async fn terminalize_accepted_tool(
    node: &Arc<EmbeddedNode>,
    lifecycle: &mut RequestLifecycle,
    accepted: crate::tool_call_lifecycle::AcceptedToolCall,
    completed: bool,
) -> ToolCallLifecycle {
    let deadline = lifecycle
        .claimed_deadline_at()
        .expect("claimed request deadline");
    let mut tool = ToolCallLifecycle::from_accepted(
        node.clone(),
        lifecycle.request().agent_did.clone(),
        lifecycle.request().requester_did.clone(),
        accepted,
        deadline,
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
    )
    .unwrap();
    tool.start_running().await.unwrap();
    if completed {
        assert!(tool.complete_owned("persisted output", None).await.unwrap());
    } else {
        assert!(tool
            .fail_owned(
                "write failed",
                crate::tool_call_lifecycle::FailureClass::External,
                None
            )
            .await
            .unwrap());
    }
    tool
}

/// A running observational wait is deadline-owned only when a Failed request
/// is terminalized at or after its valid-until boundary. Interruption and an
/// ordinary pre-deadline failure retain the running invocation as uncertain;
/// neither may manufacture a deadline timeout for the accepted call.
#[tokio::test]
async fn non_deadline_request_terminalization_does_not_timeout_running_wait() {
    for interrupted in [true, false] {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
        let now = chrono::Utc::now();
        let created_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let valid_until =
            (now + chrono::Duration::minutes(5)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let request_id = if interrupted {
            "running-wait-interrupted"
        } else {
            "running-wait-predeadline-failed"
        };
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            request_id,
            identity.did(),
            identity.did(),
            "output-behavior",
            request_id,
            "Wait for the background process",
            "interactive",
            &created_at,
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        create.valid_until = Some(valid_until);
        crate::sign_agent_request_create(&identity, &mut create)
            .await
            .unwrap();
        let row = persist_request(&node, &create).await;
        let mut lifecycle = claimed(&node, &row, None).await;
        let writer = begin_writer(&mut lifecycle, &node).await;
        let (accepted, message_doc_id) = publish_accepted_turn(
            &writer,
            &mut lifecycle,
            0,
            0,
            "native-running-wait",
            "wait_process",
            &json!({"handle":"process-1"}),
        )
        .await;
        let tool_call_doc_id = accepted.tool_call_doc_id.clone();
        let mut tool = ToolCallLifecycle::from_accepted(
            node.clone(),
            lifecycle.request().agent_did.clone(),
            lifecycle.request().requester_did.clone(),
            accepted,
            lifecycle.claimed_deadline_at().expect("claimed deadline"),
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
        )
        .unwrap();
        tool.start_running().await.unwrap();

        if interrupted {
            crate::interrupt::interrupt_request(&node, request_id)
                .await
                .unwrap();
        }
        assert_eq!(
            lifecycle
                .terminalize_owned(
                    RequestTerminalOutcome::Failed,
                    gents_protocol::output::TerminalOutput::Message { message_doc_id },
                    Some(if interrupted {
                        "operator interrupted request"
                    } else {
                        "provider failed before request deadline"
                    }),
                )
                .await
                .unwrap(),
            TerminalizeResult::Won
        );

        let response = execute(
            &node,
            &format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state cancel_cause tool_failure_class stuck_since cancel_cascade_intent_at }} }}"#,
                escape_graphql_string(&tool_call_doc_id),
            ),
        )
        .await;
        let observed = &response["AgentToolCall"][0];
        assert_eq!(observed["lifecycle_state"], "running");
        assert!(observed["cancel_cause"].is_null());
        assert!(observed["tool_failure_class"].is_null());
        assert!(observed["stuck_since"].is_string());
        assert!(observed["cancel_cascade_intent_at"].is_null());
    }
}

#[tokio::test]
async fn generated_logical_output_obligation_cases_drive_signed_requests_and_durable_writes() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().logical_output_obligation_cases;
    assert_eq!(cases.len(), 11);
    for case in cases {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
        let automated = case["automated_root"].as_bool().unwrap();
        let mut root = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "output-root",
            identity.did(),
            identity.did(),
            "output-behavior",
            "output-session",
            "Write the result",
            if automated {
                "scheduled"
            } else {
                "interactive"
            },
            "2026-09-05T00:00:00Z",
            if automated {
                AgentRequestAdmissionRecord::runtime_automated_trigger(
                    identity.did(),
                    "output-trigger",
                )
            } else {
                AgentRequestAdmissionRecord::local_self(identity.did())
            },
        );
        if automated {
            root.caused_by_trigger_kind = Some("event".into());
            root.caused_by_trigger_id = Some("output-trigger".into());
            root.caused_by_trigger_doc_id = Some("original-trigger-doc".into());
            root.caused_by_source_doc_id = Some("original-area-doc".into());
            root.caused_by_trigger_context =
                Some(json!({"source_fields":{"area_id":"area-a","expected_total":2}}).to_string());
        }
        crate::sign_agent_request_create(&identity, &mut root)
            .await
            .unwrap();
        let root_row = persist_request(&node, &root).await;
        let root_request = crate::watcher::AgentRequest::try_from(root_row.clone()).unwrap();
        let mut child = crate::lifecycle::queue::prepare_goal_continuation(
            &root_request,
            "output-behavior".into(),
            "output-goal",
            "Finish missing writes",
            1,
            false,
            "2026-09-05T00:00:01Z",
        )
        .unwrap();
        if !case["authenticated_child"].as_bool().unwrap() {
            child.caused_by_parent_request_doc_id = Some("wrong-physical-parent".into());
        }
        crate::sign_agent_request_create(&identity, &mut child)
            .await
            .unwrap();
        let child_row = persist_request(&node, &child).await;
        assert_eq!(
            child_row.caused_by_trigger_context,
            root_row.caused_by_trigger_context
        );
        assert_eq!(
            child_row.caused_by_source_doc_id,
            root_row.caused_by_source_doc_id
        );
        let request = crate::watcher::AgentRequest::try_from(child_row.clone()).unwrap();
        let mut outsider = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "output-outsider",
            identity.did(),
            identity.did(),
            "output-behavior",
            "output-session",
            "Independent request",
            "interactive",
            "2026-09-05T00:00:02Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        crate::sign_agent_request_create(&identity, &mut outsider)
            .await
            .unwrap();
        let outsider_row = persist_request(&node, &outsider).await;
        let mut stored = std::collections::HashSet::new();
        // Claim deduplication orders same-session requests by (created_at,
        // request_id): root, child, outsider. The fixture must therefore
        // process EVERY admitted same-session request in that actual queue
        // order — including zero-write requests, which publish nothing yet
        // still gate every later claim — not only the requests that carry
        // writes. Each request runs under ONE claimed lifecycle, and its
        // predecessor is terminalized before the next claim so no claim is
        // ever queued behind an active same-session request. A zero-write
        // request terminalizes with `TerminalOutput::NoMessage`; the last
        // lifecycle stays active until the gate runs.
        let mut previous: Option<(RequestLifecycle, Option<String>)> = None;
        let mut writes_by_request: std::collections::BTreeMap<u64, Vec<&Value>> =
            Default::default();
        for write in case["writes"].as_array().unwrap() {
            let call = write["call_doc"].as_u64().unwrap();
            // Reobserving one physical call cannot create another DB document.
            if !stored.insert(call) {
                continue;
            }
            writes_by_request
                .entry(write["request_doc"].as_u64().unwrap())
                .or_default()
                .push(write);
        }
        for request_doc in [10_u64, 20, 30] {
            let row: &AgentRequestRow = match request_doc {
                10 => &root_row,
                20 => &child_row,
                30 => &outsider_row,
                other => panic!("unmapped request {other}"),
            };
            let writes = writes_by_request.remove(&request_doc).unwrap_or_default();
            let mut lifecycle = claimed(&node, row, previous.take()).await;
            if writes.is_empty() {
                // Zero-write request: claimed and immediately terminalized;
                // it produced no assistant header to select.
                previous = Some((lifecycle, None));
                continue;
            }
            let mut writer = begin_writer(&mut lifecycle, &node).await;
            let mut last_message_doc_id: Option<String> = None;
            for (turn, write) in writes.iter().enumerate() {
                let call = write["call_doc"].as_u64().unwrap();
                let expected = if !case["count_valid"].as_bool().unwrap() {
                    if call == 100 {
                        2
                    } else {
                        3
                    }
                } else {
                    case["expected_count"].as_u64().unwrap_or(2)
                };
                let (accepted, message_doc_id) = publish_accepted_turn(
                    &writer,
                    &mut lifecycle,
                    turn,
                    0,
                    &format!("native-write-{call}"),
                    if write["tool"] == 1 {
                        "write_result"
                    } else {
                        "other_tool"
                    },
                    &json!({"expected_total": expected}),
                )
                .await;
                terminalize_accepted_tool(
                    &node,
                    &mut lifecycle,
                    accepted,
                    write["completed"]
                        .as_bool()
                        .expect("modeled write completion"),
                )
                .await;
                last_message_doc_id = Some(message_doc_id);
            }
            let doc_id = last_message_doc_id
                .expect("each fixture request publishes at least one owned assistant header");
            previous = Some((lifecycle, Some(doc_id)));
        }
        // Every admitted same-session request was processed in queue order.
        assert!(writes_by_request.is_empty());
        let configured = vec![(
            "write_result".into(),
            crate::document_config::WriteToolOutputObligation {
                scope: if case["request_scope"].as_bool().unwrap() {
                    crate::document_config::WriteToolOutputObligationScope::Request
                } else {
                    crate::document_config::WriteToolOutputObligationScope::Trigger
                },
                minimum_writes: case["minimum"].as_u64().unwrap() as usize,
                expected_count_field: (case["expected_count"].is_number()
                    || !case["count_valid"].as_bool().unwrap())
                .then(|| "expected_total".into()),
            },
        )];
        let observed =
            match OutputObligationGate::for_request(node.clone(), &request, &configured).await {
                Err(error) => {
                    assert!(
                        !case["authenticated_child"].as_bool().unwrap(),
                        "{}: unexpected ancestry error {error:#}",
                        case["name"]
                    );
                    assert!(error.to_string().contains("parent is absent"), "{error:#}");
                    "reject"
                }
                Ok(None) => "complete",
                Ok(Some(gate)) => match gate.unmet().await {
                    Err(error) => {
                        assert!(
                            !case["count_valid"].as_bool().unwrap(),
                            "{}: unexpected gate error {error:#}",
                            case["name"]
                        );
                        assert!(error.to_string().contains("disagree"), "{error:#}");
                        "reject"
                    }
                    Ok(unmet) if unmet.is_empty() => "complete",
                    Ok(_) => "continue",
                },
            };
        assert_eq!(
            observed,
            case["expected_decision"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        node.shutdown().await;
    }
}

// Malformed durable counts must fail the actual output gate.
#[tokio::test]
async fn dynamic_count_failures_are_observed_not_defaulted() {
    for (arguments, reason) in [
        (
            json!({"other_field": 1}),
            "the write is missing the expected_count_field",
        ),
        // `canonical_positive_count` rejects leading zeroes, zero, and values
        // above the closed-set maximum: textual variants cannot smuggle a
        // second representation of the same count past the consistency check.
        (
            json!({"expected_total": "03"}),
            "a leading-zero count is not canonical",
        ),
        (
            json!({"expected_total": 0}),
            "zero is not a canonical positive count",
        ),
        (
            json!({"expected_total": -2}),
            "a negative count is not a canonical positive integer",
        ),
    ] {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "request-count-fail-closed",
            identity.did(),
            identity.did(),
            "count-behavior",
            "session-count-fail-closed",
            "Count write",
            "interactive",
            "2026-09-05T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        crate::sign_agent_request_create(&identity, &mut create)
            .await
            .unwrap();
        let row = persist_request(&node, &create).await;
        let gate = OutputObligationGate::new(
            node.clone(),
            row.doc_id
                .as_deref()
                .expect("persisted physical request ID"),
            vec![ActiveOutputObligation {
                tool_name: "write_result".to_string(),
                contract: crate::document_config::WriteToolOutputObligation {
                    scope: crate::document_config::WriteToolOutputObligationScope::Trigger,
                    minimum_writes: 1,
                    expected_count_field: Some("expected_total".to_string()),
                },
            }],
        );
        let mut lifecycle = claimed(&node, &row, None).await;
        let mut writer = begin_writer(&mut lifecycle, &node).await;
        let (accepted, _) = publish_accepted_turn(
            &writer,
            &mut lifecycle,
            0,
            0,
            "native-count-write",
            "write_result",
            &arguments,
        )
        .await;
        terminalize_accepted_tool(&node, &mut lifecycle, accepted, true).await;

        let error = gate
            .unmet()
            .await
            .expect_err("a malformed durable count must fail the gate closed");
        assert!(
            error.to_string().contains("expected_count_field"),
            "{reason}: unexpected error {error:#}"
        );
        node.shutdown().await;
    }
}

// Two same-tool completed writes declaring different expected counts inside
// ONE canonical header must surface as a per-tool count disagreement, never
// as name ambiguity.
#[tokio::test]
async fn same_tool_calls_with_conflicting_declared_counts_reject_the_gate() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
    let mut create = AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "request-count-conflict",
        identity.did(),
        identity.did(),
        "conflict-behavior",
        "session-count-conflict",
        "Conflicting count writes",
        "interactive",
        "2026-09-05T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    crate::sign_agent_request_create(&identity, &mut create)
        .await
        .unwrap();
    let row = persist_request(&node, &create).await;
    let mut lifecycle = claimed(&node, &row, None).await;
    let mut writer = begin_writer(&mut lifecycle, &node).await;
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        id: Some("provider-message".into()),
        content: vec![
            gents_protocol::message::AssistantContent::ToolCall(
                gents_protocol::message::ToolCall {
                    id: "native-write-a".into(),
                    call_id: None,
                    function: gents_protocol::message::ToolFunction::new(
                        "write_result".into(),
                        json!({"expected_total": 2}),
                    ),
                    signature: None,
                    additional_params: None,
                },
            ),
            gents_protocol::message::AssistantContent::ToolCall(
                gents_protocol::message::ToolCall {
                    id: "native-write-b".into(),
                    call_id: None,
                    function: gents_protocol::message::ToolFunction::new(
                        "write_result".into(),
                        json!({"expected_total": 3}),
                    ),
                    signature: None,
                    additional_params: None,
                },
            ),
        ],
    };
    let mut published = writer
        .publish_native_turn(&lifecycle, 0, 0, &message)
        .await
        .unwrap();
    assert_eq!(
        published.accepted_tools.len(),
        2,
        "one canonical header must accept both physical tool calls"
    );
    let deadline = lifecycle.claimed_deadline_at().unwrap();
    for accepted in published.accepted_tools.drain(..) {
        let mut tool = ToolCallLifecycle::from_accepted(
            node.clone(),
            lifecycle.request().agent_did.clone(),
            lifecycle.request().requester_did.clone(),
            accepted,
            deadline,
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
        )
        .unwrap();
        tool.start_running().await.unwrap();
        tool.complete("persisted output").await.unwrap();
    }

    let gate = OutputObligationGate::new(
        node.clone(),
        lifecycle.request().doc_id.clone(),
        vec![ActiveOutputObligation {
            tool_name: "write_result".to_string(),
            contract: crate::document_config::WriteToolOutputObligation {
                scope: crate::document_config::WriteToolOutputObligationScope::Trigger,
                minimum_writes: 1,
                expected_count_field: Some("expected_total".to_string()),
            },
        }],
    );
    let error = gate
        .unmet()
        .await
        .expect_err("conflicting declared counts must fail the gate closed");
    assert!(
        error.to_string().contains("disagree"),
        "the per-tool count conflict must be detected as a disagreement: {error:#}"
    );
    assert!(
        !error.to_string().contains("unique physical tool binding"),
        "two same-tool writes must resolve individually, not surface as name ambiguity: {error:#}"
    );
    node.shutdown().await;
}
