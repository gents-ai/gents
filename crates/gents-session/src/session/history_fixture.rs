use super::*;
use gents_inference::llm::message::{Text, ToolResult, ToolResultContent, UserContent};

#[expect(
    clippy::too_many_arguments,
    reason = "shared test helper with call sites in modules outside session"
)]
pub async fn import_history_observation(
    node: &std::sync::Arc<defra_node::Node>,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    text: &str,
    message_key_suffix: &str,
    sequence: u32,
    tool_call: Option<(&str, &str)>,
) {
    use gents_protocol::output::rows::*;
    let segment = history_observation_segment(
        request_id,
        session_id,
        node_did,
        requester_did,
        text,
        message_key_suffix,
        tool_call,
    );
    let response = node
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row =
        gents_store::graphql::single_mutation_document(&response, "create_AgentOutputSegment")
            .unwrap()
            .unwrap();
    let header = history_observation_header(
        request_id,
        session_id,
        node_did,
        requester_did,
        message_key_suffix,
        sequence,
        tool_call,
        row["_docID"].as_str().unwrap(),
        &segment.created_at,
    );
    let response = node
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&header).unwrap()),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = gents_store::graphql::single_mutation_document(&response, "create_AgentMessage")
        .unwrap()
        .unwrap();
    let (_, reconstructed) = load_canonical_message_from_node(
        node,
        row["_docID"].as_str().unwrap(),
        node_did,
        requester_did,
    )
    .await
    .unwrap();
    let expected = history_observation_message(text, tool_call);
    assert_eq!(reconstructed, expected);
}

fn history_observation_segment(
    request_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    text: &str,
    message_key_suffix: &str,
    tool_call: Option<(&str, &str)>,
) -> gents_protocol::output::OutputSegment {
    use gents_protocol::output::*;
    OutputSegment {
        node_did: node_did.into(),
        requester_did: requester_did.map(str::to_owned),
        session_id: session_id.into(),
        request_doc_id: request_id.into(),
        source: match tool_call {
            Some((doc_id, _)) => OutputSource::ToolCall {
                tool_call_doc_id: doc_id.into(),
            },
            None => OutputSource::Authored {
                key: message_key_suffix.into(),
            },
        },
        writer: match tool_call {
            Some((doc_id, _)) => OutputWriter::ToolExecution {
                tool_call_doc_id: doc_id.into(),
            },
            None => OutputWriter::RequestExecution {
                execution_generation: "observed-generation".into(),
            },
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: text.len().try_into().unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: if tool_call.is_some() {
                    StreamPayload::ToolOutput
                } else {
                    StreamPayload::Text
                },
            }),
        }],
        payload: text.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![text.len() as u64],
        }),
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the shared history observation fixture's arguments"
)]
fn history_observation_header(
    request_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    message_key_suffix: &str,
    sequence: u32,
    tool_call: Option<(&str, &str)>,
    close_doc_id: &str,
    now: &str,
) -> gents_protocol::output::TranscriptMessage {
    use gents_protocol::output::*;
    TranscriptMessage {
        native_request_id: None,
        message_key: message_key_suffix.into(),
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: requester_did.map(str::to_owned),
        request_doc_id: Some(request_id.into()),
        publication: match tool_call {
            Some((doc_id, _)) => MessagePublication::ToolDelivery {
                tool_call_doc_id: doc_id.into(),
            },
            None => MessagePublication::RequestExecution {
                execution_generation: "observed-generation".into(),
            },
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![{
            let payload = PresentedPayload {
                output: PayloadRef {
                    close_doc_id: close_doc_id.into(),
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            };
            match tool_call {
                Some((doc_id, call_id)) => MessageBlock::ToolResult {
                    tool_call_doc_id: doc_id.into(),
                    id: call_id.into(),
                    call_id: Some(call_id.into()),
                    parts: vec![ToolResultPart::Text { text: payload }],
                },
                None => MessageBlock::Text { text: payload },
            }
        }],
        created_at: now.into(),
    }
}

fn history_observation_message(text: &str, tool_call: Option<(&str, &str)>) -> Message {
    match tool_call {
        Some((_, call_id)) => Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: call_id.into(),
                call_id: Some(call_id.into()),
                content: vec![ToolResultContent::Text(Text { text: text.into() })],
            })],
        },
        None => Message::user(text),
    }
}

/// [`import_history_observation`] written and read back by `access`, so the
/// rows are owned by the actor (such as the session's Agent) that reads them.
#[expect(
    clippy::too_many_arguments,
    reason = "shared test helper with call sites in modules outside session"
)]
pub async fn import_history_observation_as(
    access: &gents_store::config_client::ConfigAccess,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    text: &str,
    message_key_suffix: &str,
    sequence: u32,
    tool_call: Option<(&str, &str)>,
) {
    use gents_protocol::output::rows::*;
    let segment = history_observation_segment(
        request_id,
        session_id,
        node_did,
        requester_did,
        text,
        message_key_suffix,
        tool_call,
    );
    let header_doc = access
        .transact_write("test.history_observation", |txn| {
            let segment = &segment;
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &output_segment_create_variables(segment)?,
                    )
                    .await?;
                let close = gents_store::graphql::created_doc_id(&response, "AgentOutputSegment")?;
                let header = history_observation_header(
                    request_id,
                    session_id,
                    node_did,
                    requester_did,
                    message_key_suffix,
                    sequence,
                    tool_call,
                    &close,
                    &segment.created_at,
                );
                let response = txn
                    .execute_with_variables(
                        CREATE_AGENT_MESSAGE_MUTATION,
                        &transcript_message_create_variables(&header)?,
                    )
                    .await?;
                Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(
                    &response,
                    "AgentMessage",
                )?)
            })
        })
        .await
        .unwrap();
    let (_, reconstructed) = load_canonical_message(access, &header_doc, node_did, requester_did)
        .await
        .unwrap();
    assert_eq!(reconstructed, history_observation_message(text, tool_call));
}

pub async fn publish_signed_history(
    db: &gents_inference::test_support::TestDb,
    user: &dyn gents_store::identity::NodeIdentity,
    client: &gents_store::config_client::ConfigAccess,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    session_id: &str,
    messages: &[String],
) -> Vec<String> {
    let mut keys = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let request_id = format!("{session_id}-request-{index}");
        let mut lifecycle =
            signed_claimed_authored_request(db, user, client, binding, &request_id, session_id)
                .await;
        let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
            db.node.clone(),
            &lifecycle,
            std::time::Duration::ZERO,
        )
        .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        let key = gents_protocol::output::rows::authored_message_key(
            &lifecycle.request().doc_id,
            "prompt",
        );
        writer
            .publish_authored_message(&lifecycle, "prompt", &Message::user(message))
            .await
            .unwrap();
        lifecycle
            .terminalize_owned(
                crate::request::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();
        keys.push(key);
    }
    keys
}

pub async fn signed_claimed_authored_request(
    db: &gents_inference::test_support::TestDb,
    user: &dyn gents_store::identity::NodeIdentity,
    client: &gents_store::config_client::ConfigAccess,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    request_id: &str,
    session_id: &str,
) -> crate::request::lifecycle::RequestLifecycle {
    signed_claimed_authored_request_with_retry(
        db, user, client, binding, request_id, session_id, None,
    )
    .await
}

pub async fn signed_claimed_authored_request_with_retry(
    db: &gents_inference::test_support::TestDb,
    user: &dyn gents_store::identity::NodeIdentity,
    client: &gents_store::config_client::ConfigAccess,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    request_id: &str,
    session_id: &str,
    retry: Option<crate::request::lifecycle::RetryLink>,
) -> crate::request::lifecycle::RequestLifecycle {
    use crate::request::lifecycle::{
        ClaimOutcome, RequestIdentity, RequestLifecycle, RequestSigner, RequestSpec,
    };
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), binding).unwrap();
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let mut spec = RequestSpec::new(
        gents_protocol::request_admission::RequestPurpose::Normal,
        RequestIdentity {
            request_id: request_id.into(),
            node_did: db.node_identity.did().into(),
            requester_did: Some(user.did().into()),
            agent_did: binding.agent_did.clone(),
            session_id: session_id.into(),
            content: "race".into(),
            execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        },
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
    );
    spec.retry = retry;
    let create = crate::request::lifecycle::build_signed_client_request(
        client,
        spec,
        RequestSigner::Identity(user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(client, &create)
        .await
        .unwrap();
    let response = agent_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}, limit: 2) {{ {} }} }}"#,
            escape_graphql_string(request_id),
            gents_protocol::row::AGENT_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    let [row] = response["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("signed fixture request must be unique");
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone()).unwrap();
    let request = row.try_into().unwrap();
    let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        db.node.clone(),
        db.node_identity.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, agent.as_ref())
        .await
        .unwrap();
    assert!(
        admission.is_some(),
        "client request needs verified claim admission"
    );
    let mut lifecycle = RequestLifecycle::new_with_node_did(db.node.clone(), request, 60);
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    lifecycle
}
