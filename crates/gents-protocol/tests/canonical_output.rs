use gents_lean_vocab::canonical_wire::{
    self as wire, AuditResult, Observation, PayloadSpec, Presentation, Segment,
};
use gents_lean_vocab::family::Json;
use gents_protocol::output::live::{reconstruct_dense_prefix, DensePrefixError};
use gents_protocol::output::reconstruction::{reconstruct_message, ObservedSegment};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputWriter,
    ReasoningPart, ReconstructionError, TranscriptMessage,
};

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
        let records = wire::segments(&observation.records, observation.session);
        let actual = reconstruct_dense_prefix(
            &observed(&records),
            &wire::nat(observation.target.coordinate.request),
            &wire::source(&observation.target.coordinate.source),
            &wire::writer(&observation.target.writer),
            None,
        );
        Json(wire::encode(&match actual {
            Ok(prefix) => AuditResult::Ok {
                streams: prefix.streams.iter().map(wire::stream_wire).collect(),
            },
            Err(DensePrefixError::Loading) => AuditResult::Loading,
            Err(DensePrefixError::Conflicted { .. }) => AuditResult::Conflicted,
            Err(DensePrefixError::Invalid { .. }) => AuditResult::Invalid,
        }))
    });
}
