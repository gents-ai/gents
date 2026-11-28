use gents_lean_vocab::canonical_wire::{
    self as wire, Message as WireMessage, Observation, PayloadSpec, Presentation, Reconstructed,
    Segment,
};
use gents_lean_vocab::family::Json;
use gents_protocol::output::reconstruction::{
    reconstruct_message, reconstruct_stream, ObservedSegment,
};
use gents_protocol::output::recovery::plan_recovery_prefix;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputWriter,
    ReasoningPart, ReconstructionError, TranscriptMessage,
};
use gents_protocol::transcript::present_message;

fn observed(records: &[(String, OutputSegment)]) -> Vec<ObservedSegment<'_>> {
    records
        .iter()
        .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
        .collect()
}

#[test]
fn reasoning_signature_validation_follows_the_lean_model() {
    gents_lean_vocab::families::reasoning_audit_signature::replay(|input| {
        let records: Vec<Segment> = wire::decode(&input.records.0);
        let payload: PayloadSpec = wire::decode(&input.payload.0);
        assert!(
            matches!(payload.presentation, Presentation::Full),
            "{}: signature validation requires the modeled full payload",
            input.name
        );
        let records = wire::segments(&records, 0);
        let first = &records[0].1;
        let OutputWriter::RequestExecution {
            execution_generation,
        } = &first.writer
        else {
            panic!("{}: signature fixture has no request writer", input.name);
        };
        let header = TranscriptMessage {
            message_key: format!("reasoning-signature:{}", input.name),
            session_id: first.session_id.clone(),
            node_did: first.node_did.clone(),
            requester_did: None,
            request_doc_id: Some(first.request_doc_id.clone()),
            publication: MessagePublication::RequestExecution {
                execution_generation: execution_generation.clone(),
            },
            outcome: OutputOutcome::Complete,
            sequence: 0,
            role: MessageRole::Assistant,
            native_id: None,
            native_request_id: None,
            blocks: vec![MessageBlock::Reasoning {
                id: None,
                parts: vec![ReasoningPart::Text {
                    text: wire::payload_ref(&payload.reference),
                    signature: input.signature.clone(),
                }],
            }],
            created_at: first.created_at.clone(),
        };
        let actual = reconstruct_message(&observed(&records), &[], &[], &header);
        if let Err(error) = &actual {
            assert!(
                !error.is_incomplete(),
                "{}: mismatch must fail closed",
                input.name
            );
            assert!(
                matches!(error, ReconstructionError::InvalidStructure { detail } if detail.contains("reasoning signature")),
                "{}: rejected header must fail its signature check: {error:?}",
                input.name
            );
        }
        actual.is_ok()
    });
}

#[test]
fn reasoning_audit_prefix_follows_the_lean_model() {
    gents_lean_vocab::families::reasoning_audit_prefix::replay(|input| {
        let observation: Observation = wire::decode(&input.input.0);
        Json(wire::encode(&wire::audit(&observation)))
    });
}

#[test]
fn reasoning_audit_recovery_follows_the_lean_model() {
    use gents_lean_vocab::families::reasoning_audit_recovery::{RecoveryEntryWire, RecoveryOutput};
    use gents_protocol::output::extent::inspect_open_source;
    use gents_protocol::output::recovery::retained_block_indices_from_validated_extent;
    use gents_protocol::output::SourceClose;

    gents_lean_vocab::families::reasoning_audit_recovery::replay(|input| {
        let observation: Observation = wire::decode(&input.input.0);
        let audit = Json(wire::encode(&wire::audit(&observation)));
        let records = wire::segments(&observation.records, observation.session);
        let observed_records = observed(&records);
        let request_doc_id = wire::nat(observation.target.coordinate.request);
        let source = wire::source(&observation.target.coordinate.source);
        let writer = wire::writer(&observation.target.writer);
        match plan_recovery_prefix(&observed_records, &request_doc_id, &source, &writer) {
            Err(_) => RecoveryOutput {
                valid: false,
                close_stream_bytes: Vec::new(),
                retained: Vec::new(),
                audit,
            },
            Ok(plan) => {
                let SourceClose::Closed {
                    outcome: OutputOutcome::Partial,
                    stream_bytes,
                    ..
                } = &plan.close
                else {
                    panic!("{}: recovery plan must emit Partial close", input.name);
                };
                let extent =
                    inspect_open_source(&observed_records, &request_doc_id, &source, &writer)
                        .expect("planned open source inspects cleanly");
                let block_indices = retained_block_indices_from_validated_extent(&extent);
                let blocks = plan
                    .retained_blocks("recovery-close")
                    .expect("planned blocks bind cleanly");
                assert_eq!(
                    plan.retained_streams.len(),
                    block_indices.len(),
                    "{}: stream and block index counts must agree",
                    input.name
                );
                assert_eq!(
                    blocks.len(),
                    plan.retained_streams.len(),
                    "{}: planned blocks must match retained streams",
                    input.name
                );
                if !blocks.is_empty() {
                    let exemplar = records
                        .iter()
                        .map(|(_, segment)| segment)
                        .find(|segment| {
                            segment.request_doc_id == request_doc_id && segment.source == source
                        })
                        .expect("planned source has at least one segment");
                    let close_segment = OutputSegment {
                        node_did: exemplar.node_did.clone(),
                        requester_did: exemplar.requester_did.clone(),
                        session_id: exemplar.session_id.clone(),
                        request_doc_id: request_doc_id.clone(),
                        source: source.clone(),
                        writer: writer.clone(),
                        ordinal: None,
                        runs: Vec::new(),
                        payload: String::new(),
                        close: Some(plan.close.clone()),
                        created_at: extent
                            .last_created_at
                            .clone()
                            .unwrap_or_else(|| exemplar.created_at.clone()),
                    };
                    let mut with_close = records.clone();
                    with_close.push(("recovery-close".to_owned(), close_segment.clone()));
                    let header = TranscriptMessage {
                        message_key: format!("reasoning-recovery:{}", input.name),
                        session_id: exemplar.session_id.clone(),
                        node_did: exemplar.node_did.clone(),
                        requester_did: exemplar.requester_did.clone(),
                        request_doc_id: Some(request_doc_id.clone()),
                        publication: MessagePublication::RequestRecovery {
                            execution_generation: "recovery".to_owned(),
                        },
                        outcome: OutputOutcome::Partial,
                        sequence: 0,
                        role: MessageRole::Assistant,
                        native_id: None,
                        native_request_id: None,
                        blocks: blocks.clone(),
                        created_at: close_segment.created_at.clone(),
                    };
                    reconstruct_message(&observed(&with_close), &[], &[], &header).unwrap_or_else(
                        |error| {
                            panic!(
                                "{}: planned recovery header must reconstruct cleanly: {error:?}",
                                input.name
                            )
                        },
                    );
                }
                let retained = plan
                    .retained_streams
                    .iter()
                    .copied()
                    .zip(block_indices)
                    .zip(&blocks)
                    .map(|((stream, block_index), block)| match block {
                        MessageBlock::Text { .. } => RecoveryEntryWire {
                            stream: u64::from(stream),
                            block_index: u64::from(block_index),
                            kind: "text".to_owned(),
                            signature: None,
                        },
                        MessageBlock::Reasoning { parts, .. } => match parts.as_slice() {
                            [ReasoningPart::Text { signature, .. }] => RecoveryEntryWire {
                                stream: u64::from(stream),
                                block_index: u64::from(block_index),
                                kind: "reasoning".to_owned(),
                                signature: signature.clone(),
                            },
                            [ReasoningPart::Redacted { .. }] => RecoveryEntryWire {
                                stream: u64::from(stream),
                                block_index: u64::from(block_index),
                                kind: "redacted".to_owned(),
                                signature: None,
                            },
                            other => panic!(
                                "{}: unexpected reasoning recovery part {other:?}",
                                input.name
                            ),
                        },
                        other => panic!("{}: unexpected recovery block {other:?}", input.name),
                    })
                    .collect();
                RecoveryOutput {
                    valid: true,
                    close_stream_bytes: stream_bytes.clone(),
                    retained,
                    audit,
                }
            }
        }
    });
}

#[test]
fn canonical_output_projection_follows_the_lean_model() {
    gents_lean_vocab::families::canonical_output_projection_view::replay(|input| {
        let observation: Observation = wire::decode(&input.input.0);
        Json(wire::encode(&wire::view_wire(
            &wire::project(&observation),
            &observation,
        )))
    });
}

#[test]
fn persisted_reasoning_presentation_follows_the_lean_model() {
    gents_lean_vocab::families::canonical_output_projection_presentation::replay(|input| {
        let native: Reconstructed = wire::decode(&input.native.0);
        let presentation = present_message(&wire::native_assistant(&native));
        assert_eq!(presentation.body_markdown, "", "{}", input.name);
        presentation
            .reasoning_markdown
            .map(|text| text.split('\n').map(str::to_owned).collect())
            .unwrap_or_default()
    });
}

#[test]
fn auxiliary_output_projection_follows_the_lean_model() {
    gents_lean_vocab::families::auxiliary_output_projection::replay(|input| {
        let observation: Observation = wire::decode(&input.observation.0);
        let source = wire::source(&observation.target.coordinate.source);
        assert!(source.is_auxiliary_audit(), "{}", input.name);
        let records = wire::segments(&observation.records, observation.session);
        let planned = plan_recovery_prefix(
            &observed(&records),
            &wire::nat(observation.request),
            &source,
            &wire::writer(&observation.target.writer),
        );
        let recovery_close = planned.ok().map(|planned| {
            assert!(planned.retained_streams.is_empty(), "{}", input.name);
            assert!(
                planned.retained_blocks("native-close").unwrap().is_empty(),
                "{}",
                input.name
            );
            Json(wire::encode(&wire::closure_wire(&planned.close)))
        });
        Some(
            gents_lean_vocab::families::auxiliary_output_projection::ProjectionOutput {
                audit: Json(wire::encode(&wire::audit(&observation))),
                public_view: Json(wire::encode(&wire::view_wire(
                    &wire::project(&observation),
                    &observation,
                ))),
                recovery_close,
            },
        )
    });
}

fn payload_measurement(
    segments: &Json,
    message: &Json,
) -> Option<gents_lean_vocab::families::payload_presentation_contracts_lengths::Measurement> {
    let message: WireMessage<PayloadSpec> = wire::decode(&message.0);
    let records: Vec<Segment> = wire::decode(&segments.0);
    let records = wire::segments(&records, message.header.session);
    let observed = observed(&records);
    let header = wire::message(&message);
    let (verdict, stored, presented) = match reconstruct_message(&observed, &[], &[], &header) {
        Ok(native) => {
            let native = wire::reconstructed_wire(&native, &header);
            let presented = wire::payload_fields(&native)
                .iter()
                .map(|payload| payload.len() as u64)
                .sum::<u64>();
            let mut references = Vec::new();
            for reference in &message.header.refs {
                if !references.contains(reference) {
                    references.push(reference.clone());
                }
            }
            let stored = references
                .iter()
                .map(|reference| {
                    reconstruct_stream(&observed, &[], &[], &wire::payload_ref(reference))
                        .expect("a reconstructed message's stream reconstructs")
                        .text
                        .len() as u64
                })
                .sum::<u64>();
            ("measured", Some(stored), Some(presented))
        }
        Err(error) if error.is_incomplete() => ("missing", None, None),
        Err(_) => ("rejected", None, None),
    };
    Some(
        gents_lean_vocab::families::payload_presentation_contracts_lengths::Measurement {
            verdict: verdict.to_owned(),
            stored_payload_bytes: stored,
            presented_payload_bytes: presented,
        },
    )
}

#[test]
fn payload_presentation_lengths_follow_the_lean_model() {
    gents_lean_vocab::families::payload_presentation_contracts_lengths::replay(|input| {
        payload_measurement(&input.segments, &input.message)
    });
}

#[test]
fn malformed_payload_presentation_follows_the_lean_model() {
    gents_lean_vocab::families::payload_presentation_contracts_malformed_lengths::replay(|input| {
        payload_measurement(&input.segments, &input.message)
            .map(|measurement| gents_lean_vocab::family::recode(&measurement))
    });
}
