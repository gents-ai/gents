use super::*;
use crate::request::lifecycle::{
    ClaimOutcome, RequestLifecycle, RequestTerminalOutcome, TerminalizeResult,
};
use crate::request::streaming::DefraStreamWriter;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use crate::tools::call::lifecycle::{AwaitMode, ToolCallLifecycle};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

/// A Node whose runtime root is `root`. Agents derived from `root` are then the
/// Node's own Agents, and their rows are written through their managed signer.
async fn rooted_node(root: &KeyIdentity) -> Arc<EmbeddedNode> {
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(root.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    node
}

/// An Agent DID derives from the runtime root and a creation nonce.
fn derived_agent(root: &KeyIdentity) -> Arc<dyn NodeIdentity> {
    let binding = crate::store::identity::provision_agent_identity(root).unwrap();
    crate::store::identity::resolve_agent_identity(root, &binding).unwrap()
}

async fn persist_request(
    node: &Arc<EmbeddedNode>,
    agent: &dyn NodeIdentity,
    create: &AgentRequestCreate,
) -> AgentRequestRow {
    let actor = identity::Did::new(&create.agent_did).unwrap();
    let access = crate::ConfigAccess::local_with_optional_agent_signer(
        node.clone(),
        actor.clone(),
        agent.managed_agent_signer(),
    )
    .unwrap();
    let session = escape_graphql_string(&create.session_id);
    let node_did = escape_graphql_string(&create.node_did);
    let existing = access.execute(&format!(
        r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{session}" }} }}) {{ _docID }} }}"#,
    )).await.unwrap();
    let rows = existing["data"]["AgentSession"].as_array().unwrap();
    let session_doc = if rows.is_empty() {
        let created = access.write("test.output_obligation.session", &format!(
            r#"mutation {{ create_AgentSession(input: {{ node_did: "{node_did}", session_id: "{session}", requester_did: "{}", agent_did: "{}", created_at: "2026-09-05T00:00:00Z" }}) {{ _docID }} }}"#,
            escape_graphql_string(&create.requester_did), escape_graphql_string(&create.agent_did),
        )).await.unwrap();
        let doc = crate::store::graphql::created_doc_id(&created, "AgentSession").unwrap();
        access
            .add_actor_relationship("AgentSession", &doc, "working_agent", &actor)
            .await
            .unwrap();
        doc
    } else {
        assert_eq!(rows.len(), 1);
        rows[0]["_docID"].as_str().unwrap().to_owned()
    };
    let created = access
        .write(
            "test.output_obligation.request",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let request_doc = crate::store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
    access
        .add_actor_relationship("AgentRequest", &request_doc, "working_agent", &actor)
        .await
        .unwrap();
    access
        .add_resource_relationship(
            "AgentRequest",
            &request_doc,
            "session",
            "sessions",
            &session_doc,
        )
        .await
        .unwrap();
    let data = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&request_doc),
            crate::request::admission::SIGNED_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    serde_json::from_value(data["data"]["AgentRequest"][0].clone()).unwrap()
}

async fn claimed(
    node: &Arc<EmbeddedNode>,
    agent: &dyn NodeIdentity,
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
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        node.clone(),
        request.agent_did.as_deref().expect("fixture agent DID"),
        request.node_did.as_deref().expect("fixture node DID"),
        request.clone().try_into().unwrap(),
        60,
    );
    if agent.managed_agent_signer().is_some() {
        lifecycle.bind_managed_agent(agent).unwrap();
    }
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

async fn begin_writer(
    lifecycle: &mut RequestLifecycle,
    node: &Arc<EmbeddedNode>,
) -> DefraStreamWriter {
    let writer = DefraStreamWriter::new(
        node.clone(),
        &lifecycle.request().agent_did,
        Duration::from_millis(1),
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    writer
}

async fn publish_accepted_turn(
    writer: &DefraStreamWriter,
    lifecycle: &mut RequestLifecycle,
    turn: usize,
    attempt: u32,
    tool_call_id: &str,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> (crate::tools::call::lifecycle::AcceptedToolCall, String) {
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            turn,
            attempt,
            "inference.1".parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        request_id: None,
        id: Some("provider-message".into()),
        content: vec![gents_protocol::message::AssistantContent::ToolCall(
            gents_protocol::message::ToolCall {
                raw_arguments: None,
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

async fn terminalize_accepted_tool(
    node: &Arc<EmbeddedNode>,
    lifecycle: &mut RequestLifecycle,
    accepted: crate::tools::call::lifecycle::AcceptedToolCall,
    completed: bool,
) -> ToolCallLifecycle {
    let deadline = lifecycle
        .claimed_deadline_at()
        .expect("claimed request deadline");
    let mut tool = ToolCallLifecycle::from_accepted(
        node.clone(),
        lifecycle.request().node_did.clone(),
        lifecycle.request().requester_did.clone(),
        accepted,
        deadline,
        AwaitMode::Foreground,
    )
    .unwrap();
    tool.start_running().await.unwrap();
    if completed {
        assert!(tool.complete_owned("persisted output", None).await.unwrap());
    } else {
        assert!(tool
            .fail_owned(
                "write failed",
                crate::tools::call::lifecycle::FailureClass::External,
                None
            )
            .await
            .unwrap());
    }
    tool
}

#[tokio::test]
async fn non_deadline_request_terminalization_does_not_timeout_running_wait() {
    for interrupted in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
        let node = rooted_node(&identity).await;
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
            identity.did(),
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
        let row = persist_request(&node, &identity, &create).await;
        let mut lifecycle = claimed(&node, &identity, &row, None).await;
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
            lifecycle.request().node_did.clone(),
            lifecycle.request().requester_did.clone(),
            accepted,
            lifecycle.claimed_deadline_at().expect("claimed deadline"),
            AwaitMode::Foreground,
        )
        .unwrap();
        tool.start_running().await.unwrap();

        if interrupted {
            crate::request::interrupt::interrupt_request_by_doc_id_with_access(
                &crate::ConfigAccess::local_as(
                    node.clone(),
                    identity::Did::new(identity.did()).unwrap(),
                ),
                &lifecycle.request().doc_id,
                &lifecycle.request().node_did,
                lifecycle.request().requester_did.as_deref(),
            )
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

        let response = crate::ConfigAccess::local_as(
            node.clone(), identity::Did::new(identity.did()).unwrap(),
        ).execute(
            &format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state cancel_cause tool_failure_class stuck_since }} }}"#,
                escape_graphql_string(&tool_call_doc_id),
            ),
        )
        .await.unwrap();
        let observed = &response["data"]["AgentToolCall"][0];
        assert_eq!(observed["lifecycle_state"], "running");
        assert!(observed["cancel_cause"].is_null());
        assert!(observed["tool_failure_class"].is_null());
        assert!(observed["stuck_since"].is_string());
    }
}

#[tokio::test]
async fn generated_logical_output_obligation_cases_drive_signed_requests_and_durable_writes() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().logical_output_obligation_cases;
    assert_eq!(cases.len(), 11);
    for case in cases {
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
        let node = rooted_node(&identity).await;
        let agent = derived_agent(&identity);
        assert_ne!(identity.did(), agent.did());
        let automated = case["automated_root"].as_bool().unwrap();
        let mut root = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "output-root",
            identity.did(),
            agent.did(),
            agent.did(),
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
                    agent.did(),
                    "output-trigger",
                )
            } else {
                AgentRequestAdmissionRecord::local_self(agent.did())
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
        crate::sign_agent_request_create(agent.as_ref(), &mut root)
            .await
            .unwrap();
        let root_row = persist_request(&node, agent.as_ref(), &root).await;
        let root_request =
            crate::request::watcher::AgentRequest::try_from(root_row.clone()).unwrap();
        let mut child = crate::request::lifecycle::queue::prepare_goal_continuation(
            &root_request,
            agent.did().into(),
            "output-goal",
            "Finish missing writes",
            1,
            false,
            "2026-09-05T00:00:01Z",
            root_request.request_hop,
        )
        .unwrap();
        if !case["authenticated_child"].as_bool().unwrap() {
            child.caused_by_parent_request_doc_id = Some("wrong-physical-parent".into());
        }
        crate::sign_agent_request_create(agent.as_ref(), &mut child)
            .await
            .unwrap();
        let child_row = persist_request(&node, agent.as_ref(), &child).await;
        assert_eq!(
            child_row.caused_by_trigger_context,
            root_row.caused_by_trigger_context
        );
        assert_eq!(
            child_row.caused_by_source_doc_id,
            root_row.caused_by_source_doc_id
        );
        let request = crate::request::watcher::AgentRequest::try_from(child_row.clone()).unwrap();
        let mut outsider = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "output-outsider",
            identity.did(),
            agent.did(),
            agent.did(),
            "output-session",
            "Independent request",
            "interactive",
            "2026-09-05T00:00:02Z",
            AgentRequestAdmissionRecord::local_self(agent.did()),
        );
        crate::sign_agent_request_create(agent.as_ref(), &mut outsider)
            .await
            .unwrap();
        let outsider_row = persist_request(&node, agent.as_ref(), &outsider).await;
        let mut stored = rapidhash::RapidHashSet::default();
        let mut previous: Option<(RequestLifecycle, Option<String>)> = None;
        let mut writes_by_request: std::collections::BTreeMap<u64, Vec<&Value>> =
            Default::default();
        for write in case["writes"].as_array().unwrap() {
            let call = write["call_doc"].as_u64().unwrap();
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
            let mut lifecycle = claimed(&node, agent.as_ref(), row, previous.take()).await;
            if writes.is_empty() {
                previous = Some((lifecycle, None));
                continue;
            }
            let writer = begin_writer(&mut lifecycle, &node).await;
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
        assert!(writes_by_request.is_empty());
        let configured = vec![(
            "write_result".into(),
            crate::documents::WriteToolOutputObligation {
                scope: if case["request_scope"].as_bool().unwrap() {
                    crate::documents::WriteToolOutputObligationScope::Request
                } else {
                    crate::documents::WriteToolOutputObligationScope::Trigger
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
                Ok(Some(gate)) => {
                    assert_eq!(gate.access.actor_did().as_deref(), Some(agent.did()));
                    match gate.unmet().await {
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
                    }
                }
            };
        assert_eq!(
            observed,
            case["expected_decision"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn dynamic_count_failures_are_observed_not_defaulted() {
    for (arguments, reason) in [
        (
            json!({"other_field": 1}),
            "the write is missing the expected_count_field",
        ),
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
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
        let node = rooted_node(&identity).await;
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "request-count-fail-closed",
            identity.did(),
            identity.did(),
            identity.did(),
            "session-count-fail-closed",
            "Count write",
            "interactive",
            "2026-09-05T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        crate::sign_agent_request_create(&identity, &mut create)
            .await
            .unwrap();
        let row = persist_request(&node, &identity, &create).await;
        let gate = OutputObligationGate::new(
            node.clone(),
            identity::Did::new(identity.did()).unwrap(),
            row.doc_id
                .as_deref()
                .expect("persisted physical request ID"),
            vec![ActiveOutputObligation {
                tool_name: "write_result".to_string(),
                contract: crate::documents::WriteToolOutputObligation {
                    scope: crate::documents::WriteToolOutputObligationScope::Trigger,
                    minimum_writes: 1,
                    expected_count_field: Some("expected_total".to_string()),
                },
            }],
        );
        let mut lifecycle = claimed(&node, &identity, &row, None).await;
        let writer = begin_writer(&mut lifecycle, &node).await;
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
        node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn same_tool_calls_with_conflicting_declared_counts_reject_the_gate() {
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
    let node = rooted_node(&identity).await;
    let mut create = AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "request-count-conflict",
        identity.did(),
        identity.did(),
        identity.did(),
        "session-count-conflict",
        "Conflicting count writes",
        "interactive",
        "2026-09-05T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    crate::sign_agent_request_create(&identity, &mut create)
        .await
        .unwrap();
    let row = persist_request(&node, &identity, &create).await;
    let mut lifecycle = claimed(&node, &identity, &row, None).await;
    let writer = begin_writer(&mut lifecycle, &node).await;
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        request_id: None,
        id: Some("provider-message".into()),
        content: vec![
            gents_protocol::message::AssistantContent::ToolCall(
                gents_protocol::message::ToolCall {
                    raw_arguments: None,
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
                    raw_arguments: None,
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
            lifecycle.request().node_did.clone(),
            lifecycle.request().requester_did.clone(),
            accepted,
            deadline,
            AwaitMode::Foreground,
        )
        .unwrap();
        tool.start_running().await.unwrap();
        tool.complete("persisted output").await.unwrap();
    }

    let gate = OutputObligationGate::new(
        node.clone(),
        identity::Did::new(identity.did()).unwrap(),
        lifecycle.request().doc_id.clone(),
        vec![ActiveOutputObligation {
            tool_name: "write_result".to_string(),
            contract: crate::documents::WriteToolOutputObligation {
                scope: crate::documents::WriteToolOutputObligationScope::Trigger,
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
    node.shutdown().await.expect("node shutdown");
}
