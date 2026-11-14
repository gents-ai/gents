use defra_node::Node as EmbeddedNode;
use gents_lean_vocab::canonical_wire::{
    self as wire, AuxiliaryKind, Block, Closure, Message as WireMessage, Observation, PayloadKind,
    PayloadSpec, Presentation, Publication, ReasoningPart as WireReasoningPart, Role, Segment,
    Source,
};
use gents_lean_vocab::family::Json;
use gents_loop::provider_audit::{AuxiliaryOutputEvent, AuxiliaryOutputObservation};
use gents_loop::provider_input::ProviderInputProfile;
use gents_protocol::message::{AssistantContent, Message, Reasoning, ReasoningContent};
use gents_protocol::output::live::{
    project_live, reconstruct_dense_prefix, LiveObservation, LiveTarget, OwnerLiveness,
};
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
use gents_protocol::output::{
    OutputSegment, OutputSource, OutputWriter, SegmentRun, SourceClose, StreamPayload,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use gents_session::request::lifecycle::{ClaimOutcome, RequestLifecycle};
use gents_session::request::streaming::{auxiliary, canonical, native_encoding, DefraStreamWriter};
use gents_store::config_client::ConfigAccess;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

fn modeled_scope(name: &str, raw: &Segment) -> (CaptureScope, usize, u32) {
    let Source::Auxiliary {
        auxiliary_kind,
        scope,
        turn,
        attempt,
    } = &raw.coordinate.source
    else {
        panic!("{name} has no auxiliary source");
    };
    let kind = match auxiliary_kind {
        AuxiliaryKind::Compaction => CaptureScopeKind::Compaction,
        AuxiliaryKind::CompactionFallback => CaptureScopeKind::CompactionFallback,
        AuxiliaryKind::Title => CaptureScopeKind::Title,
        AuxiliaryKind::AccessReview => CaptureScopeKind::AccessReview,
    };
    (
        CaptureScope { kind, seq: *scope },
        usize::try_from(*turn).expect("modeled turn fits usize"),
        u32::try_from(*attempt).expect("modeled attempt fits u32"),
    )
}

fn modeled_message(name: &str, raw: &Segment) -> Message {
    let flush = raw.flush.as_ref().expect("modeled raw flush");
    assert_eq!(flush.ordinal, 0, "{}: unsupported initial ordinal", name);
    let mut cursor = 0usize;
    let mut parts: BTreeMap<u64, (Option<ReasoningContent>, Option<String>)> = BTreeMap::new();
    for (stream_index, run) in flush.runs.iter().enumerate() {
        assert_eq!(
            run.stream as usize, stream_index,
            "{}: unsupported stream order",
            name
        );
        let length = usize::try_from(run.bytes).expect("modeled run fits usize");
        let end = cursor
            .checked_add(length)
            .expect("modeled run length overflow");
        let bytes = flush
            .payload
            .get(cursor..end)
            .expect("modeled run exceeds payload");
        cursor = end;
        let text = String::from_utf8(bytes.to_vec()).expect("native payload requires UTF-8");
        let declaration = run.declaration.as_ref().expect("modeled declaration");
        assert_eq!(declaration.block, 0, "{}: unsupported block", name);
        assert!(declaration.tool.is_none() && declaration.media_kind.is_none());
        let (body, signature) = parts.entry(declaration.part).or_default();
        match declaration.kind {
            PayloadKind::Reasoning => {
                assert!(body
                    .replace(ReasoningContent::Text {
                        text,
                        signature: None
                    })
                    .is_none());
            }
            PayloadKind::Signature => {
                assert!(signature.replace(text).is_none());
            }
            PayloadKind::Encrypted => {
                assert!(body.replace(ReasoningContent::Encrypted(text)).is_none());
            }
            PayloadKind::Redacted => {
                assert!(body
                    .replace(ReasoningContent::Redacted { data: text })
                    .is_none());
            }
            _ => panic!("{}: unsupported native reasoning field", name),
        }
    }
    assert_eq!(cursor, flush.payload.len(), "{}: unconsumed payload", name);
    let content = parts
        .into_iter()
        .enumerate()
        .map(|(index, (part, (body, signature)))| {
            assert_eq!(part, index as u64, "{}: non-dense native parts", name);
            match (body.expect("modeled reasoning body"), signature) {
                (ReasoningContent::Text { text, .. }, signature) => {
                    ReasoningContent::Text { text, signature }
                }
                (body, None) => body,
                _ => panic!("{}: signature has no text body", name),
            }
        })
        .collect();
    Message::Assistant {
        request_id: None,
        id: None,
        content: vec![AssistantContent::Reasoning(Reasoning { id: None, content })],
    }
}

async fn claimed(node: &Arc<EmbeddedNode>, name: &str) -> RequestLifecycle {
    let request_id = format!("auxiliary-{name}-{}", uuid::Uuid::new_v4());
    let session_id = format!("session-{request_id}");
    let now = chrono::Utc::now().to_rfc3339();
    let agent_did = crate::triggers::callback::test_agent_did();
    let node_did = node
        .node_identity_did()
        .expect("fixture node identity")
        .to_owned();
    let mutation = format!(
        r#"mutation {{ create_AgentRequest(input: {{ request_id: "{}", purpose: "normal", node_did: "{}", agent_did: "{}", session_id: "{}", retry_parent_request: "", retry_root_request: "{}", superseded_by_request: "", content: "hello", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{}", retry_count: 0, max_retries: 3, request_hop: 0 }}) {{ _docID }} }}"#,
        gents_store::graphql::escape_graphql_string(&request_id),
        gents_store::graphql::escape_graphql_string(&node_did),
        gents_store::graphql::escape_graphql_string(&agent_did),
        gents_store::graphql::escape_graphql_string(&session_id),
        gents_store::graphql::escape_graphql_string(&request_id),
        gents_store::graphql::escape_graphql_string(&now),
    );
    // The runtime root creates the row and grants its working Agent; every
    // later lifecycle and output write is the Agent's own signed write.
    ConfigAccess::local_as(node.clone(), node_did.parse().unwrap())
        .transact_write("test.auxiliary_request", |txn| {
            let (mutation, agent_did) = (&mutation, &agent_did);
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
                txn.add_actor_relationship(
                    "AgentRequest",
                    &doc,
                    "working_agent",
                    &::identity::Did::new(agent_did)?,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let access = crate::triggers::callback::test_agent_access(node);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
        gents_store::graphql::escape_graphql_string(&request_id),
        gents_protocol::row::AGENT_REQUEST_FIELDS,
    );
    let response = access.execute(&query).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let mut lifecycle =
        RequestLifecycle::new_with_node_did(node.clone(), row.try_into().unwrap(), 60);
    lifecycle
        .bind_managed_agent(crate::triggers::callback::test_agent_identity().as_ref())
        .unwrap();
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

fn native_raw(
    name: &str,
    modeled_raw: &Segment,
    lifecycle: &RequestLifecycle,
    generation: &str,
) -> OutputSegment {
    let encoded =
        native_encoding::encode_native_message(&modeled_message(name, modeled_raw)).unwrap();
    let flush = modeled_raw.flush.as_ref().expect("modeled raw flush");
    assert_eq!(flush.runs.len(), encoded.streams.len(), "{}", name);
    let mut payload = String::new();
    let runs = encoded
        .streams
        .iter()
        .enumerate()
        .map(|(index, stream)| {
            let modeled = &flush.runs[index];
            assert_eq!(modeled.stream as usize, index, "{}", name);
            assert_eq!(modeled.bytes as usize, stream.payload.len(), "{}", name);
            let declaration = modeled.declaration.as_ref().expect("modeled declaration");
            assert_eq!(
                declaration.block as u32, stream.declaration.block_index,
                "{}",
                name
            );
            assert_eq!(
                declaration.part as u32, stream.declaration.part_index,
                "{}",
                name
            );
            let payload_kind = match declaration.kind {
                PayloadKind::Reasoning => StreamPayload::Reasoning,
                PayloadKind::Signature => StreamPayload::ReasoningSignature,
                PayloadKind::Encrypted => StreamPayload::ReasoningEncrypted,
                PayloadKind::Redacted => StreamPayload::ReasoningRedacted,
                _ => panic!("{name}: unsupported native audit declaration"),
            };
            assert_eq!(payload_kind, stream.declaration.payload, "{}", name);
            payload.push_str(&stream.payload);
            SegmentRun {
                stream: u32::try_from(index).unwrap(),
                bytes: u32::try_from(stream.payload.len()).unwrap(),
                declaration: Some(stream.declaration.clone()),
            }
        })
        .collect();
    assert_eq!(payload.as_bytes(), flush.payload, "{}", name);
    let request = lifecycle.request();
    let (scope, turn, attempt) = modeled_scope(name, modeled_raw);
    OutputSegment {
        node_did: request.node_did.clone(),
        requester_did: request.requester_did.clone(),
        session_id: request.session_id.clone(),
        request_doc_id: request.doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope,
            turn_index: u32::try_from(turn).unwrap(),
            attempt,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.to_owned(),
        },
        ordinal: Some(u32::try_from(flush.ordinal).unwrap()),
        runs,
        payload,
        close: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

async fn output_state(node: &Arc<EmbeddedNode>, request_doc_id: &str) -> serde_json::Value {
    let request_doc_id = gents_store::graphql::escape_graphql_string(request_doc_id);
    let response = crate::triggers::callback::test_agent_access(node)
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID }} }}"#,
        ))
        .await
        .unwrap();
    response["data"].clone()
}

fn output_counts(state: &serde_json::Value) -> (usize, usize) {
    (
        state["AgentOutputSegment"].as_array().unwrap().len(),
        state["AgentMessage"].as_array().unwrap().len(),
    )
}

fn close_kind(name: &str, outcome: &str) -> canonical::ProviderAttemptClose {
    match outcome {
        "complete" => canonical::ProviderAttemptClose::AuxiliaryComplete,
        "partial" => canonical::ProviderAttemptClose::Partial,
        other => panic!("{name}: unsupported modeled close {other}"),
    }
}

#[tokio::test]
async fn auxiliary_owned_guards_follow_the_lean_model() {
    for case in gents_lean_vocab::families::auxiliary_output_owned::cases() {
        let input = &case.input;
        let modeled_raw: Segment = wire::decode(&input.raw.0);
        let native_publication_closing: Segment = wire::decode(&input.native_publication_closing.0);
        let native_publication_message: WireMessage<PayloadSpec> =
            wire::decode(&input.native_publication_message.0);
        let db = crate::triggers::callback::test_node_builder(
            "generated-auxiliary-cases-drive-owned-be",
        )
        .await;
        let node = db.node.clone();
        crate::triggers::callback::seed_test_agent(
            &node,
            gents_session::request::lifecycle::test_support::PIN_FIXED_DID,
        )
        .await;
        let mut lifecycle = claimed(&node, &input.name).await;
        let writer = DefraStreamWriter::new_for_lifecycle(
            node.clone(),
            &lifecycle,
            Duration::from_millis(1),
        )
        .unwrap();
        let generation = lifecycle.execution_generation().unwrap().to_owned();
        let request_doc_id = lifecycle.request().doc_id.clone();
        let empty_state = output_state(&node, &request_doc_id).await;
        assert_eq!(output_counts(&empty_state), (0, 0), "{}", input.name);
        let before = native_raw(&input.name, &modeled_raw, &lifecycle, &generation);
        let claimed_append = canonical::append_provider_segment_with_signer(
            &node,
            identity::Did::new(crate::triggers::callback::test_agent_did()).unwrap(),
            lifecycle.managed_agent_signer(),
            &generation,
            &before,
        )
        .await;
        let claimed_append_accepted = claimed_append.is_ok();
        if let Err(error) = claimed_append {
            assert!(
                matches!(
                    error.downcast_ref::<canonical::ProviderAppendRejection>(),
                    Some(canonical::ProviderAppendRejection::LostLease)
                ),
                "{}: Claimed append failed outside the lease owner",
                input.name
            );
        }
        assert_eq!(
            output_state(&node, &request_doc_id).await,
            empty_state,
            "{}",
            input.name
        );

        let begin = lifecycle.begin_owned_execution(&writer).await;
        let begin_accepted = begin.is_ok();
        begin.unwrap();
        assert_eq!(lifecycle.execution_generation().unwrap(), generation);
        let raw = native_raw(&input.name, &modeled_raw, &lifecycle, &generation);
        let append = canonical::append_provider_segment_with_signer(
            &node,
            identity::Did::new(crate::triggers::callback::test_agent_did()).unwrap(),
            lifecycle.managed_agent_signer(),
            &generation,
            &raw,
        )
        .await;
        let begun_append_accepted = append.is_ok();
        append.unwrap();
        let appended_state = output_state(&node, &request_doc_id).await;
        assert_eq!(output_counts(&appended_state), (1, 0), "{}", input.name);

        let empty_message = Message::Assistant {
            request_id: None,
            id: None,
            content: Vec::new(),
        };
        assert!(
            native_encoding::encode_native_message(&empty_message)
                .unwrap_err()
                .to_string()
                .contains("native assistant message has no blocks"),
            "{}: empty native assistant passed the wire encoder",
            input.name
        );
        assert!(
            matches!(
                native_publication_message.header.publication,
                Publication::RequestExecution { .. }
            ) && native_publication_message.header.role == Role::Assistant,
            "{}: native publication is not a request-owned assistant header",
            input.name
        );
        let [Block::Reasoning {
            id: modeled_id,
            parts: modeled_parts,
        }] = native_publication_message.blocks.as_slice()
        else {
            panic!(
                "{}: native publication is not one reasoning block",
                input.name
            );
        };
        let message = modeled_message(&input.name, &modeled_raw);
        let Message::Assistant { content, .. } = &message else {
            unreachable!()
        };
        let [AssistantContent::Reasoning(reasoning)] = content.as_slice() else {
            unreachable!()
        };
        assert_eq!(modeled_id, &reasoning.id, "{}", input.name);
        assert_eq!(
            modeled_parts.len(),
            reasoning.content.len(),
            "{}",
            input.name
        );
        let encoded = native_encoding::encode_native_message(&message).unwrap();
        let mut referenced = Vec::new();
        for (part_index, (modeled, native)) in
            modeled_parts.iter().zip(&reasoning.content).enumerate()
        {
            let (payload, payload_kind) = match (modeled, native) {
                (
                    WireReasoningPart::Text { payload, signature },
                    ReasoningContent::Text {
                        signature: native_signature,
                        ..
                    },
                ) => {
                    assert_eq!(signature, native_signature, "{}", input.name);
                    (payload, StreamPayload::Reasoning)
                }
                (WireReasoningPart::Encrypted { payload }, ReasoningContent::Encrypted(_)) => {
                    (payload, StreamPayload::ReasoningEncrypted)
                }
                (WireReasoningPart::Redacted { payload }, ReasoningContent::Redacted { .. }) => {
                    (payload, StreamPayload::ReasoningRedacted)
                }
                _ => panic!("{}: native publication reasoning part differs", input.name),
            };
            assert!(matches!(&payload.presentation, Presentation::Full));
            assert_eq!(
                payload.reference.close_id, native_publication_closing.id,
                "{}",
                input.name
            );
            let stream = encoded
                .streams
                .iter()
                .position(|stream| {
                    stream.declaration.block_index == 0
                        && stream.declaration.part_index == part_index as u32
                        && stream.declaration.payload == payload_kind
                })
                .expect("modeled publication stream exists");
            assert_eq!(payload.reference.stream as usize, stream, "{}", input.name);
            referenced.push(payload.reference.clone());
        }
        assert_eq!(
            native_publication_message.header.refs, referenced,
            "{}",
            input.name
        );
        let Some(Closure::Closed {
            segments,
            stream_bytes,
            ..
        }) = native_publication_closing.close.as_ref()
        else {
            panic!("{}: modeled native publication has no close", input.name);
        };
        let mut publication_closing = raw.clone();
        publication_closing.ordinal = None;
        publication_closing.runs.clear();
        publication_closing.payload.clear();
        publication_closing.close = Some(SourceClose::Closed {
            outcome: gents_protocol::output::OutputOutcome::Complete,
            segments: u32::try_from(*segments).unwrap(),
            stream_bytes: stream_bytes.clone(),
        });
        let publication = canonical::publish_provider_turn_with_signer(
            &node,
            identity::Did::new(crate::triggers::callback::test_agent_did()).unwrap(),
            lifecycle.managed_agent_signer(),
            &generation,
            canonical::ProviderPublicationPlan {
                final_flush: Some(publication_closing),
                message_key: native_publication_message.key.clone(),
                encoded: Arc::new(encoded),
                expected: Arc::new(message),
                tool_deadline_at: chrono::Utc::now().to_rfc3339(),
                background_calls: Vec::new(),
                consumes_folded: None,
            },
        )
        .await;
        let native_publication_accepted = publication.is_ok();
        if let Err(error) = publication {
            let diagnostic = format!("{error:#}");
            assert!(
                diagnostic.contains("provider publication does not bind its request generation"),
                "{}: auxiliary header failed outside publication owner: {diagnostic}",
                input.name
            );
        }
        assert_eq!(
            output_state(&node, &request_doc_id).await,
            appended_state,
            "{}",
            input.name
        );

        let close_kind = close_kind(&input.name, &input.outcome);
        let close = canonical::close_provider_attempt_with_signer(
            &node,
            identity::Did::new(crate::triggers::callback::test_agent_did()).unwrap(),
            lifecycle.managed_agent_signer(),
            &generation,
            &raw,
            close_kind,
        )
        .await;
        let close_accepted = close.is_ok();
        close.unwrap();
        let closed_state = output_state(&node, &request_doc_id).await;
        assert_eq!(output_counts(&closed_state), (2, 0), "{}", input.name);
        let stale_generation = format!("{generation}-modeled-stale");
        let mut stale = raw.clone();
        stale.writer = OutputWriter::RequestExecution {
            execution_generation: stale_generation.clone(),
        };
        let stale_replay = canonical::close_provider_attempt_with_signer(
            &node,
            identity::Did::new(crate::triggers::callback::test_agent_did()).unwrap(),
            lifecycle.managed_agent_signer(),
            &stale_generation,
            &stale,
            close_kind,
        )
        .await;
        let stale_close_replay_accepted = stale_replay.is_ok();
        if let Err(error) = stale_replay {
            let diagnostic = format!("{error:#}");
            assert!(
                matches!(
                    error.downcast_ref::<canonical::ProviderCloseRejection>(),
                    Some(canonical::ProviderCloseRejection::WriterMismatch)
                ),
                "{}: stale close failed outside immutable writer check: {diagnostic}",
                input.name
            );
        }
        assert_eq!(
            output_state(&node, &request_doc_id).await,
            closed_state,
            "{}",
            input.name
        );
        node.shutdown().await.expect("shut down test node");
        case.check(Some(
            gents_lean_vocab::families::auxiliary_output_owned::OwnedOutput {
                claimed_append_accepted,
                begin_accepted,
                begun_append_accepted,
                native_publication_accepted,
                close_accepted,
                stale_close_replay_accepted,
            },
        ));
    }
}

#[tokio::test]
async fn auxiliary_sink_audit_follows_the_lean_model() {
    for case in gents_lean_vocab::families::auxiliary_output_sink::cases() {
        let input = &case.input;
        let modeled_raw: Segment = wire::decode(&input.raw.0);
        let observation: Observation = wire::decode(&input.observation.0);
        let db = crate::triggers::callback::test_node_builder(
            "generated-auxiliary-cases-drive-non-clau",
        )
        .await;
        let node = db.node.clone();
        crate::triggers::callback::seed_test_agent(
            &node,
            gents_session::request::lifecycle::test_support::PIN_FIXED_DID,
        )
        .await;
        let mut lifecycle = claimed(&node, &input.name).await;
        let writer = DefraStreamWriter::new_for_lifecycle(
            node.clone(),
            &lifecycle,
            Duration::from_millis(1),
        )
        .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        let request = lifecycle.request().clone();
        let generation = lifecycle.execution_generation().unwrap().to_owned();
        let (scope, turn, attempt) = modeled_scope(&input.name, &modeled_raw);
        let source = OutputSource::ProviderTurn {
            scope,
            turn_index: u32::try_from(turn).unwrap(),
            attempt,
        };
        let output_writer = OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        };
        let message = modeled_message(&input.name, &modeled_raw);
        let Message::Assistant { content, .. } = &message else {
            unreachable!()
        };
        let [AssistantContent::Reasoning(reasoning)] = content.as_slice() else {
            panic!("{}: unsupported modeled native content", input.name);
        };
        let mut writer =
            DefraStreamWriter::new(node.clone(), &request.agent_did, Duration::from_millis(1));
        writer.agent_signer = lifecycle.managed_agent_signer();
        let sink = auxiliary::sink_with_writer(
            Arc::new(writer),
            request.clone(),
            generation,
            ProviderInputProfile::OpenAiChatCompletions,
        );
        for event in [
            AuxiliaryOutputEvent::AttemptStarted,
            AuxiliaryOutputEvent::Reasoning(reasoning.clone()),
        ] {
            (sink.observe)(AuxiliaryOutputObservation {
                capture_scope: scope,
                turn,
                attempt,
                event,
            })
            .await
            .unwrap();
        }
        let event = match close_kind(&input.name, &input.outcome) {
            canonical::ProviderAttemptClose::AuxiliaryComplete => {
                AuxiliaryOutputEvent::TurnReady { message }
            }
            _ => AuxiliaryOutputEvent::ClosePartial,
        };
        (sink.observe)(AuxiliaryOutputObservation {
            capture_scope: scope,
            turn,
            attempt,
            event,
        })
        .await
        .unwrap();

        let request_doc_id = gents_store::graphql::escape_graphql_string(&request.doc_id);
        let response = crate::triggers::callback::test_agent_access(&node).execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID }} }}"#,
        )).await.unwrap();
        let data = &response["data"];
        let rows = data["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let close = rows
            .iter()
            .find_map(|row| row.segment.close.as_ref())
            .expect("durable close");
        assert!(
            data["AgentMessage"].as_array().unwrap().is_empty(),
            "{}",
            input.name
        );
        let observed = rows
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        let audit = wire::audit_wire(reconstruct_dense_prefix(
            &observed,
            &request.doc_id,
            &source,
            &output_writer,
            None,
        ));
        let view = project_live(&LiveObservation {
            request_doc_id: &request.doc_id,
            session_id: &request.session_id,
            target: LiveTarget {
                request_doc_id: &request.doc_id,
                source: &source,
                writer: &output_writer,
                message_id: None,
            },
            messages: &[],
            node_did: &request.node_did,
            requester_did: request.requester_did.as_deref(),
            records: &observed,
            denied_headers: &[],
            denied_segments: &[],
            dependency_denials: &[],
            owner: OwnerLiveness {
                current_request: Some((
                    &request.doc_id,
                    match &output_writer {
                        OutputWriter::RequestExecution {
                            execution_generation,
                        } => execution_generation,
                        _ => unreachable!(),
                    },
                )),
                live_tools: vec![],
            },
            request_terminal: false,
            terminal_selection: None,
        });
        let output = gents_lean_vocab::families::auxiliary_output_sink::SinkOutput {
            close: Json(wire::encode(&wire::closure_wire(close))),
            segments: Some(rows.len() as u64),
            audit: Json(wire::encode(&audit)),
            public_view: Json(wire::encode(&wire::view_wire(&view, &observation))),
        };
        node.shutdown().await.expect("shut down test node");
        case.check(Some(output));
    }
}
