use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;
use gents_loop::provider_audit::{AuxiliaryOutputEvent, AuxiliaryOutputObservation};
use gents_loop::provider_input::ProviderInputProfile;
use gents_protocol::message::{AssistantContent, Message, Reasoning, ReasoningContent};
use gents_protocol::output::live::{
    project_live, reconstruct_dense_prefix, LiveObservation, LiveTarget, LiveView, OwnerLiveness,
};
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{
    OutputSegment, OutputSource, OutputWriter, SegmentRun, SourceClose, StreamPayload,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

use super::{auxiliary, canonical, native_encoding, DefraStreamWriter};
use crate::config_client::ConfigAccess;
use crate::lean_vocab_test::{
    lean_auxiliary_output_cases, LeanAuxiliaryKind, LeanAuxiliaryOutputCase, LeanCanonicalClosure,
    LeanCanonicalSource, LeanCanonicalWriter, LeanMessageBlock, LeanMessagePublication,
    LeanMessageRole, LeanOutcome, LeanPayloadKind, LeanPresentation, LeanReasoningAuditResult,
    LeanReasoningPart,
};
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};

fn modeled_scope(case: &LeanAuxiliaryOutputCase) -> (CaptureScope, usize, u32) {
    let LeanCanonicalSource::Auxiliary {
        auxiliary_kind,
        scope,
        turn,
        attempt,
    } = &case.raw.coordinate.source
    else {
        panic!("{} has no auxiliary source", case.name);
    };
    let kind = match auxiliary_kind {
        LeanAuxiliaryKind::Compaction => CaptureScopeKind::Compaction,
        LeanAuxiliaryKind::CompactionFallback => CaptureScopeKind::CompactionFallback,
        LeanAuxiliaryKind::Title => CaptureScopeKind::Title,
    };
    (
        CaptureScope { kind, seq: *scope },
        usize::try_from(*turn).expect("modeled turn fits usize"),
        u32::try_from(*attempt).expect("modeled attempt fits u32"),
    )
}

// The native one-shot reasoning event carries the same block/part fields as
// the modeled raw flush. Unsupported modeled shapes fail this adapter loudly.
fn modeled_message(case: &LeanAuxiliaryOutputCase) -> Message {
    let flush = case.raw.flush.as_ref().expect("modeled raw flush");
    assert_eq!(
        flush.ordinal, 0,
        "{}: unsupported initial ordinal",
        case.name
    );
    let mut cursor = 0usize;
    let mut parts: BTreeMap<u64, (Option<ReasoningContent>, Option<String>)> = BTreeMap::new();
    for (stream_index, run) in flush.runs.iter().enumerate() {
        assert_eq!(
            run.stream as usize, stream_index,
            "{}: unsupported stream order",
            case.name
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
        assert_eq!(declaration.block, 0, "{}: unsupported block", case.name);
        assert!(declaration.tool.is_none() && declaration.media_kind.is_none());
        let (body, signature) = parts.entry(declaration.part).or_default();
        match declaration.kind {
            LeanPayloadKind::Reasoning => {
                assert!(body
                    .replace(ReasoningContent::Text {
                        text,
                        signature: None
                    })
                    .is_none());
            }
            LeanPayloadKind::Signature => {
                assert!(signature.replace(text).is_none());
            }
            LeanPayloadKind::Encrypted => {
                assert!(body.replace(ReasoningContent::Encrypted(text)).is_none());
            }
            LeanPayloadKind::Redacted => {
                assert!(body
                    .replace(ReasoningContent::Redacted { data: text })
                    .is_none());
            }
            _ => panic!("{}: unsupported native reasoning field", case.name),
        }
    }
    assert_eq!(
        cursor,
        flush.payload.len(),
        "{}: unconsumed payload",
        case.name
    );
    let content = parts
        .into_iter()
        .enumerate()
        .map(|(index, (part, (body, signature)))| {
            assert_eq!(part, index as u64, "{}: non-dense native parts", case.name);
            match (body.expect("modeled reasoning body"), signature) {
                (ReasoningContent::Text { text, .. }, signature) => {
                    ReasoningContent::Text { text, signature }
                }
                (body, None) => body,
                _ => panic!("{}: signature has no text body", case.name),
            }
        })
        .collect();
    Message::Assistant {
        id: None,
        content: vec![AssistantContent::Reasoning(Reasoning { id: None, content })],
    }
}

async fn claimed(node: &Arc<EmbeddedNode>, name: &str) -> RequestLifecycle {
    let request_id = format!("auxiliary-{name}-{}", uuid::Uuid::new_v4());
    let session_id = format!("session-{request_id}");
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{ create_AgentRequest(input: {{ request_id: "{}", purpose: "normal", agent_did: "did:test:test", behavior_id: "general", session_id: "{}", retry_parent_request: "", retry_root_request: "{}", superseded_by_request: "", content: "hello", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{}", retry_count: 0, max_retries: 3, subagent_depth: 0 }}) {{ _docID }} }}"#,
        crate::graphql::escape_graphql_string(&request_id),
        crate::graphql::escape_graphql_string(&session_id),
        crate::graphql::escape_graphql_string(&request_id),
        crate::graphql::escape_graphql_string(&now),
    );
    let access = ConfigAccess::Local(node.clone());
    access
        .write("test.auxiliary_request", &mutation)
        .await
        .unwrap();
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
        crate::graphql::escape_graphql_string(&request_id),
        crate::watcher::AGENT_REQUEST_FIELDS,
    );
    let response = access.execute(&query).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        "did:test:test",
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

fn native_raw(
    case: &LeanAuxiliaryOutputCase,
    lifecycle: &RequestLifecycle,
    generation: &str,
) -> OutputSegment {
    let encoded = native_encoding::encode_native_message(&modeled_message(case)).unwrap();
    let flush = case.raw.flush.as_ref().expect("modeled raw flush");
    assert_eq!(flush.runs.len(), encoded.streams.len(), "{}", case.name);
    let mut payload = String::new();
    let runs = encoded
        .streams
        .iter()
        .enumerate()
        .map(|(index, stream)| {
            let modeled = &flush.runs[index];
            assert_eq!(modeled.stream as usize, index, "{}", case.name);
            assert_eq!(
                modeled.bytes as usize,
                stream.payload.len(),
                "{}",
                case.name
            );
            let declaration = modeled.declaration.as_ref().expect("modeled declaration");
            assert_eq!(
                declaration.block as u32, stream.declaration.block_index,
                "{}",
                case.name
            );
            assert_eq!(
                declaration.part as u32, stream.declaration.part_index,
                "{}",
                case.name
            );
            let payload_kind = match declaration.kind {
                LeanPayloadKind::Reasoning => StreamPayload::Reasoning,
                LeanPayloadKind::Signature => StreamPayload::ReasoningSignature,
                LeanPayloadKind::Encrypted => StreamPayload::ReasoningEncrypted,
                LeanPayloadKind::Redacted => StreamPayload::ReasoningRedacted,
                _ => panic!("{}: unsupported native audit declaration", case.name),
            };
            assert_eq!(payload_kind, stream.declaration.payload, "{}", case.name);
            payload.push_str(&stream.payload);
            SegmentRun {
                stream: u32::try_from(index).unwrap(),
                bytes: u32::try_from(stream.payload.len()).unwrap(),
                declaration: Some(stream.declaration.clone()),
            }
        })
        .collect();
    assert_eq!(payload.as_bytes(), flush.payload, "{}", case.name);
    let request = lifecycle.request();
    let (scope, turn, attempt) = modeled_scope(case);
    OutputSegment {
        agent_did: request.agent_did.clone(),
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
    let request_doc_id = crate::graphql::escape_graphql_string(request_doc_id);
    let response = ConfigAccess::Local(node.clone())
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

#[tokio::test]
async fn generated_auxiliary_cases_drive_owned_begin_close_and_publication_guards() {
    let cases = lean_auxiliary_output_cases();
    assert!(!cases.is_empty(), "generated auxiliary cases are empty");
    for case in cases {
        let path = std::env::temp_dir().join(format!(
            "auxiliary-owner-{}-{}",
            case.name,
            uuid::Uuid::new_v4()
        ));
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(&path)
                .build()
                .await
                .unwrap(),
        );
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        let mut lifecycle = claimed(&node, &case.name).await;
        assert_eq!(
            case.authority.claimed_request_state, "claimed",
            "{}",
            case.name
        );
        assert_eq!(
            case.authority.begin_boundary, "mutation_write_gate",
            "{}",
            case.name
        );
        let LeanCanonicalWriter::Request {
            generation: raw_generation,
        } = &case.raw.writer
        else {
            panic!("{}: raw has no request writer", case.name);
        };
        assert_eq!(*raw_generation, case.authority.generation, "{}", case.name);
        let writer =
            DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(1));
        let generation = lifecycle.execution_generation().unwrap().to_owned();
        let request_doc_id = lifecycle.request().doc_id.clone();
        let empty_state = output_state(&node, &request_doc_id).await;
        assert_eq!(output_counts(&empty_state), (0, 0), "{}", case.name);
        let before = native_raw(case, &lifecycle, &generation);
        let claimed_append = canonical::append_provider_segment(&node, &generation, &before).await;
        assert_eq!(
            claimed_append.is_ok(),
            case.expected.claimed_append_accepted,
            "{}",
            case.name
        );
        if !case.expected.claimed_append_accepted {
            assert!(
                matches!(
                    claimed_append
                        .unwrap_err()
                        .downcast_ref::<canonical::ProviderAppendRejection>(),
                    Some(canonical::ProviderAppendRejection::LostLease)
                ),
                "{}: Claimed append failed outside the lease owner",
                case.name
            );
        }
        assert_eq!(
            output_state(&node, &request_doc_id).await,
            empty_state,
            "{}",
            case.name
        );

        let begin = lifecycle.begin_owned_execution(&writer).await;
        assert_eq!(begin.is_ok(), case.expected.begin_accepted, "{}", case.name);
        begin.unwrap();
        assert_eq!(lifecycle.execution_generation().unwrap(), generation);
        let raw = native_raw(case, &lifecycle, &generation);
        let append = canonical::append_provider_segment(&node, &generation, &raw).await;
        assert_eq!(
            append.is_ok(),
            case.expected.begun_append_accepted,
            "{}",
            case.name
        );
        append.unwrap();
        let appended_state = output_state(&node, &request_doc_id).await;
        assert_eq!(output_counts(&appended_state), (1, 0), "{}", case.name);

        assert!(case.publication_message.blocks.is_empty(), "{}", case.name);
        assert!(
            case.publication_message.header.refs.is_empty(),
            "{}",
            case.name
        );
        assert_eq!(
            case.publication_message.header.role,
            LeanMessageRole::Assistant
        );
        let LeanMessagePublication::RequestExecution {
            generation: publication_generation,
        } = &case.publication_message.header.publication
        else {
            panic!("{}: empty header is not request-owned", case.name);
        };
        assert_eq!(
            *publication_generation, case.authority.generation,
            "{}",
            case.name
        );
        let empty_message = Message::Assistant {
            id: None,
            content: Vec::new(),
        };
        assert!(
            native_encoding::encode_native_message(&empty_message)
                .unwrap_err()
                .to_string()
                .contains("native assistant message has no blocks"),
            "{}: empty native assistant passed the wire encoder",
            case.name
        );
        assert_eq!(
            case.native_publication_closing.coordinate, case.raw.coordinate,
            "{}",
            case.name
        );
        assert_eq!(
            case.native_publication_closing.writer, case.raw.writer,
            "{}",
            case.name
        );
        assert!(matches!(
            case.native_publication_closing.close.as_ref(),
            Some(LeanCanonicalClosure::Closed {
                outcome: LeanOutcome::Complete,
                ..
            })
        ));
        let LeanMessagePublication::RequestExecution {
            generation: native_publication_generation,
        } = &case.native_publication_message.header.publication
        else {
            panic!("{}: native publication is not request-owned", case.name);
        };
        assert_eq!(
            *native_publication_generation, case.authority.generation,
            "{}",
            case.name
        );
        assert_eq!(
            case.native_publication_message.header.role,
            LeanMessageRole::Assistant,
            "{}",
            case.name
        );
        let [LeanMessageBlock::Reasoning {
            id: modeled_id,
            parts: modeled_parts,
        }] = case.native_publication_message.blocks.as_slice()
        else {
            panic!(
                "{}: native publication is not one reasoning block",
                case.name
            );
        };
        let message = modeled_message(case);
        let Message::Assistant { content, .. } = &message else {
            unreachable!()
        };
        let [AssistantContent::Reasoning(reasoning)] = content.as_slice() else {
            unreachable!()
        };
        assert_eq!(modeled_id, &reasoning.id, "{}", case.name);
        assert_eq!(
            modeled_parts.len(),
            reasoning.content.len(),
            "{}",
            case.name
        );
        let encoded = native_encoding::encode_native_message(&message).unwrap();
        let mut referenced = Vec::new();
        for (part_index, (modeled, native)) in
            modeled_parts.iter().zip(&reasoning.content).enumerate()
        {
            let (payload, payload_kind) = match (modeled, native) {
                (
                    LeanReasoningPart::Text { payload, signature },
                    ReasoningContent::Text {
                        signature: native_signature,
                        ..
                    },
                ) => {
                    assert_eq!(signature, native_signature, "{}", case.name);
                    (payload, StreamPayload::Reasoning)
                }
                (LeanReasoningPart::Encrypted { payload }, ReasoningContent::Encrypted(_)) => {
                    (payload, StreamPayload::ReasoningEncrypted)
                }
                (LeanReasoningPart::Redacted { payload }, ReasoningContent::Redacted { .. }) => {
                    (payload, StreamPayload::ReasoningRedacted)
                }
                _ => panic!("{}: native publication reasoning part differs", case.name),
            };
            assert!(matches!(&payload.presentation, LeanPresentation::Full));
            assert_eq!(
                payload.reference.close_id, case.native_publication_closing.id,
                "{}",
                case.name
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
            assert_eq!(payload.reference.stream as usize, stream, "{}", case.name);
            referenced.push(payload.reference.clone());
        }
        assert_eq!(
            case.native_publication_message.header.refs, referenced,
            "{}",
            case.name
        );
        let LeanCanonicalClosure::Closed {
            outcome: LeanOutcome::Complete,
            segments,
            stream_bytes,
        } = case
            .native_publication_closing
            .close
            .as_ref()
            .expect("modeled native publication close")
        else {
            unreachable!()
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
        let publication = canonical::publish_provider_turn(
            &node,
            &generation,
            canonical::ProviderPublicationPlan {
                final_flush: Some(publication_closing),
                message_key: case.native_publication_message.key.clone(),
                encoded: Arc::new(encoded),
                expected: Arc::new(message),
                tool_deadline_at: chrono::Utc::now().to_rfc3339(),
                spawn_admissions: Vec::new(),
            },
        )
        .await;
        assert_eq!(
            publication.is_ok(),
            case.expected.native_publication_accepted,
            "{}",
            case.name
        );
        if !case.expected.native_publication_accepted {
            let error = publication.unwrap_err();
            let diagnostic = format!("{error:#}");
            assert!(
                diagnostic.contains("provider publication does not bind its request generation"),
                "{}: empty auxiliary header failed outside publication owner: {diagnostic}",
                case.name
            );
        }
        assert_eq!(
            output_state(&node, &request_doc_id).await,
            appended_state,
            "{}",
            case.name
        );

        let close_kind = match case.closing.close.as_ref().expect("modeled close") {
            LeanCanonicalClosure::Closed {
                outcome: LeanOutcome::Complete,
                ..
            } => canonical::ProviderAttemptClose::AuxiliaryComplete,
            LeanCanonicalClosure::Closed {
                outcome: LeanOutcome::Partial,
                ..
            } => canonical::ProviderAttemptClose::Partial,
            _ => panic!("{}: unsupported modeled close", case.name),
        };
        let close = canonical::close_provider_attempt(&node, &generation, &raw, close_kind).await;
        assert_eq!(close.is_ok(), case.expected.close_accepted, "{}", case.name);
        close.unwrap();
        let closed_state = output_state(&node, &request_doc_id).await;
        assert_eq!(output_counts(&closed_state), (2, 0), "{}", case.name);
        let LeanCanonicalWriter::Request {
            generation: stale_generation,
        } = &case.stale_closing.writer
        else {
            panic!("{}: stale close has no request writer", case.name);
        };
        assert_ne!(
            *stale_generation, case.authority.generation,
            "{}",
            case.name
        );
        let stale_generation = format!("{generation}-modeled-{stale_generation}");
        let mut stale = raw.clone();
        stale.writer = OutputWriter::RequestExecution {
            execution_generation: stale_generation.clone(),
        };
        let stale_replay =
            canonical::close_provider_attempt(&node, &stale_generation, &stale, close_kind).await;
        assert_eq!(
            stale_replay.is_ok(),
            case.expected.stale_close_replay_accepted,
            "{}",
            case.name
        );
        if !case.expected.stale_close_replay_accepted {
            let error = stale_replay.unwrap_err();
            let diagnostic = format!("{error:#}");
            assert!(
                matches!(
                    error.downcast_ref::<canonical::ProviderCloseRejection>(),
                    Some(canonical::ProviderCloseRejection::WriterMismatch)
                ),
                "{}: stale close failed outside immutable writer check: {diagnostic}",
                case.name
            );
        }
        assert_eq!(
            output_state(&node, &request_doc_id).await,
            closed_state,
            "{}",
            case.name
        );
        node.shutdown().await;
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[tokio::test]
async fn generated_auxiliary_cases_drive_non_claude_sink_audit_without_publication() {
    let cases = lean_auxiliary_output_cases();
    assert!(!cases.is_empty(), "generated auxiliary cases are empty");
    for case in cases {
        assert!(
            case.expected.append_accepted && case.expected.close_accepted,
            "{}",
            case.name
        );
        assert_eq!(
            case.raw.coordinate, case.closing.coordinate,
            "{}",
            case.name
        );
        assert_eq!(
            case.raw.coordinate, case.observation.target.coordinate,
            "{}",
            case.name
        );
        let path = std::env::temp_dir().join(format!(
            "auxiliary-sink-{}-{}",
            case.name,
            uuid::Uuid::new_v4()
        ));
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(&path)
                .build()
                .await
                .unwrap(),
        );
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        let mut lifecycle = claimed(&node, &case.name).await;
        let writer =
            DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(1));
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        let request = lifecycle.request().clone();
        let generation = lifecycle.execution_generation().unwrap().to_owned();
        let (scope, turn, attempt) = modeled_scope(case);
        let source = OutputSource::ProviderTurn {
            scope,
            turn_index: u32::try_from(turn).unwrap(),
            attempt,
        };
        let output_writer = OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        };
        let message = modeled_message(case);
        let Message::Assistant { content, .. } = &message else {
            unreachable!()
        };
        let [AssistantContent::Reasoning(reasoning)] = content.as_slice() else {
            panic!("{}: unsupported modeled native content", case.name);
        };
        let sink = auxiliary::sink(
            node.clone(),
            request.clone(),
            generation,
            ProviderInputProfile::OpenAiChatCompletions,
            Duration::from_millis(1),
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
        let event = match case.closing.close.as_ref().expect("modeled close") {
            LeanCanonicalClosure::Closed { outcome, .. } => match outcome {
                LeanOutcome::Complete => AuxiliaryOutputEvent::TurnReady { message },
                LeanOutcome::Partial => AuxiliaryOutputEvent::ClosePartial,
            },
            _ => panic!("{}: unsupported auxiliary close", case.name),
        };
        (sink.observe)(AuxiliaryOutputObservation {
            capture_scope: scope,
            turn,
            attempt,
            event,
        })
        .await
        .unwrap();

        let request_doc_id = crate::graphql::escape_graphql_string(&request.doc_id);
        let response = ConfigAccess::Local(node.clone()).execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID }} }}"#,
        )).await.unwrap();
        let data = &response["data"];
        let rows = data["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<anyhow::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows.len(),
            case.observation.records.len() + 1,
            "{}",
            case.name
        );
        let close = rows
            .iter()
            .find(|row| row.segment.close.is_some())
            .expect("durable close");
        let LeanCanonicalClosure::Closed {
            outcome,
            segments,
            stream_bytes,
        } = case.closing.close.as_ref().unwrap()
        else {
            unreachable!()
        };
        let expected_outcome = match outcome {
            LeanOutcome::Complete => gents_protocol::output::OutputOutcome::Complete,
            LeanOutcome::Partial => gents_protocol::output::OutputOutcome::Partial,
        };
        assert!(
            matches!(close.segment.close.as_ref(), Some(SourceClose::Closed {
            outcome, segments: actual_segments, stream_bytes: actual_bytes,
        }) if *outcome == expected_outcome && u64::from(*actual_segments) == *segments
            && actual_bytes == stream_bytes),
            "{}",
            case.name
        );
        assert!(!case.expected.publication_accepted, "{}", case.name);
        assert!(
            data["AgentMessage"].as_array().unwrap().is_empty(),
            "{}",
            case.name
        );
        let observed = rows
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        let audit =
            reconstruct_dense_prefix(&observed, &request.doc_id, &source, &output_writer, None)
                .unwrap();
        let LeanReasoningAuditResult::Ok { streams } = &case.expected.audit else {
            panic!("{}: unsupported modeled audit result", case.name);
        };
        assert_eq!(audit.streams.len(), streams.len(), "{}", case.name);
        for (actual, expected) in audit.streams.iter().zip(streams) {
            assert_eq!(
                actual.declaration.block_index as u64, expected.declaration.block,
                "{}",
                case.name
            );
            assert_eq!(
                actual.declaration.part_index as u64, expected.declaration.part,
                "{}",
                case.name
            );
            let expected_payload = match expected.declaration.kind {
                LeanPayloadKind::Reasoning => StreamPayload::Reasoning,
                LeanPayloadKind::Signature => StreamPayload::ReasoningSignature,
                LeanPayloadKind::Encrypted => StreamPayload::ReasoningEncrypted,
                LeanPayloadKind::Redacted => StreamPayload::ReasoningRedacted,
                _ => panic!("{}: unsupported modeled audit field", case.name),
            };
            assert_eq!(
                actual.declaration.payload, expected_payload,
                "{}",
                case.name
            );
            assert_eq!(
                actual.text.as_bytes(),
                expected.bytes.as_slice(),
                "{}",
                case.name
            );
        }
        assert!(matches!(
            case.expected.public_view,
            crate::lean_vocab_test::LeanCanonicalOutputView::Absent
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
            agent_did: &request.agent_did,
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
        assert_eq!(view, LiveView::Absent, "{}", case.name);
        node.shutdown().await;
        std::fs::remove_dir_all(path).unwrap();
    }
}
