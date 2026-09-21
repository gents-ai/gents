//! Physical row envelopes for the canonical durable output collections
//! (#1571): `AgentOutputSegment` and `AgentMessage`.
//!
//! Reader envelopes only. The document shapes are owned by
//! `gents_protocol::output` and the SDL in `gents-schemas`; nothing here
//! duplicates or redefines them. Each envelope pairs the strict protocol
//! value with the physical `_docID` the owner uses to address the exact row
//! in a patch transaction. `_docID` is physical database identity, not
//! protocol data: it is stripped before strict protocol deserialization so
//! it can never leak into the canonical document shape, and an absent or
//! blank `_docID` is rejected.

use anyhow::{Context, Result};

/// Fields for one canonical `AgentOutputSegment` read, mirroring the SDL
/// field list plus the physical `_docID`. Nested values (`source`, `writer`,
/// `runs`, `close`) are DefraDB JSON scalar columns: read the bare field
/// name, never an object subselection, and decode through the canonical
/// protocol owners.
pub const AGENT_OUTPUT_SEGMENT_FIELDS: &str = "agent_did requester_did session_id \
request_doc_id source ordinal writer runs payload close created_at _docID";

/// Fields for one canonical `AgentMessage` (`TranscriptMessage`) read,
/// mirroring the SDL field list plus the physical `_docID`. `publication`
/// and `blocks` are DefraDB JSON scalar columns: read the bare field name,
/// never an object subselection.
pub const AGENT_MESSAGE_FIELDS: &str = "message_key session_id agent_did requester_did \
request_doc_id publication outcome sequence role native_id blocks created_at _docID";

/// One canonical `AgentOutputSegment` row: the strict protocol segment plus
/// the physical `_docID` used to address it. Not a second writable
/// representation; segments are create-only through the existing owners.
#[derive(Debug, Clone)]
pub struct OutputSegmentRow {
    pub doc_id: String,
    pub segment: gents_protocol::output::OutputSegment,
}

/// One canonical `AgentMessage` row: the strict protocol transcript message
/// plus the physical `_docID` used to address it.
#[derive(Debug, Clone)]
pub struct TranscriptMessageRow {
    pub doc_id: String,
    pub message: gents_protocol::output::TranscriptMessage,
}

/// Split one physical row into its nonblank `_docID` and the remaining
/// document, ready for strict protocol deserialization.
fn split_physical_doc_id(
    row: &serde_json::Value,
    collection: &str,
) -> Result<(String, serde_json::Value)> {
    let doc_id = row
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{collection} row omitted _docID"))?
        .to_string();
    let mut document = row.clone();
    document
        .as_object_mut()
        .with_context(|| format!("{collection} row is not an object"))?
        .remove("_docID");
    Ok((doc_id, document))
}

/// Decode one `AgentOutputSegment` row: strip the physical `_docID`, then
/// deserialize strictly through `gents_protocol::output::OutputSegment`
/// (`deny_unknown_fields` rejects retired or unknown fields).
pub fn decode_output_segment_row(row: &serde_json::Value) -> Result<OutputSegmentRow> {
    let (doc_id, document) = split_physical_doc_id(row, "AgentOutputSegment")?;
    let segment: gents_protocol::output::OutputSegment =
        serde_json::from_value(document).context("decoding canonical AgentOutputSegment")?;
    Ok(OutputSegmentRow { doc_id, segment })
}

/// Decode one `AgentMessage` row: strip the physical `_docID`, then
/// deserialize strictly through `gents_protocol::output::TranscriptMessage`.
pub fn decode_transcript_message_row(row: &serde_json::Value) -> Result<TranscriptMessageRow> {
    let (doc_id, document) = split_physical_doc_id(row, "AgentMessage")?;
    let message: gents_protocol::output::TranscriptMessage =
        serde_json::from_value(document).context("decoding canonical AgentMessage")?;
    Ok(TranscriptMessageRow { doc_id, message })
}

/// Create mutation for one canonical `AgentOutputSegment` row. The document
/// travels as a typed GraphQL variable, never as rendered GraphQL text: JSON
/// keys (including arbitrary `additional_params` object keys) and JSON empty
/// arrays are preserved verbatim by the variable encoder and are not nillable
/// SDL lists.
pub const CREATE_AGENT_OUTPUT_SEGMENT_MUTATION: &str = "mutation($input: \
AgentOutputSegmentMutationInputArg!) { create_AgentOutputSegment(input: $input) { _docID } }";

/// Create mutation for one canonical `AgentMessage` row. The document travels
/// as a typed GraphQL variable, never as rendered GraphQL text: JSON keys
/// (including arbitrary `additional_params` object keys) and JSON empty arrays
/// are preserved verbatim by the variable encoder and are not nillable SDL
/// lists.
pub const CREATE_AGENT_MESSAGE_MUTATION: &str = "mutation($input: \
AgentMessageMutationInputArg!) { create_AgentMessage(input: $input) { _docID } }";

/// Build the `execute_with_variables` variables for one canonical
/// `AgentOutputSegment` create: the strict protocol segment serialized as the
/// `$input` variable, with no GraphQL-text rendering of the JSON.
pub fn output_segment_create_variables(
    segment: &gents_protocol::output::OutputSegment,
) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "input": serde_json::to_value(segment).context("serializing canonical AgentOutputSegment")?
    }))
}

/// Build the `execute_with_variables` variables for one canonical
/// `AgentMessage` create: the strict protocol message serialized as the
/// `$input` variable, with no GraphQL-text rendering of the JSON.
pub fn transcript_message_create_variables(
    message: &gents_protocol::output::TranscriptMessage,
) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "input": serde_json::to_value(message).context("serializing canonical AgentMessage")?
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::output::{OutputOutcome, SourceClose};

    fn terminal_only_segment_row() -> serde_json::Value {
        serde_json::json!({
            "_docID": "seg-1",
            "agent_did": "agent",
            "session_id": "session",
            "request_doc_id": "request",
            "source": {"kind": "authored", "key": "prompt"},
            "writer": {"kind": "request_execution", "execution_generation": "gen-1"},
            "close": {"kind": "retracted"},
            "created_at": "2026-09-09T22:30:00Z"
        })
    }

    fn transcript_message_row() -> serde_json::Value {
        serde_json::json!({
            "_docID": "msg-1",
            "message_key": "key",
            "session_id": "session",
            "agent_did": "agent",
            "publication": {"kind": "fork", "origin_message_doc_id": "origin"},
            "outcome": "complete",
            "sequence": 3,
            "role": "assistant",
            "blocks": [
                {"type": "text", "text": {
                    "output": {"close_doc_id": "close-1", "stream": 0},
                    "presentation": {"kind": "full"}
                }}
            ],
            "created_at": "2026-09-09T22:30:00Z"
        })
    }

    #[test]
    fn field_selections_end_with_the_physical_doc_id() {
        assert!(AGENT_OUTPUT_SEGMENT_FIELDS.ends_with("_docID"));
        assert!(AGENT_MESSAGE_FIELDS.ends_with("_docID"));
    }

    #[test]
    fn segment_row_splits_physical_identity_from_strict_payload() {
        let decoded = decode_output_segment_row(&terminal_only_segment_row()).unwrap();
        assert_eq!(decoded.doc_id, "seg-1");
        assert_eq!(decoded.segment.session_id, "session");
        assert_eq!(decoded.segment.request_doc_id, "request");
        assert_eq!(decoded.segment.runs, Vec::new());
        assert_eq!(
            decoded.segment.close,
            Some(SourceClose::Retracted),
            "terminal-only record carries closure without a flush"
        );
        assert_eq!(decoded.segment.ordinal, None);
    }

    #[test]
    fn segment_row_rejects_absent_blank_and_unknown_fields() {
        let mut row = terminal_only_segment_row();
        row.as_object_mut().unwrap().remove("_docID");
        assert!(decode_output_segment_row(&row).is_err());

        row["_docID"] = serde_json::json!("   ");
        assert!(decode_output_segment_row(&row).is_err());

        row["_docID"] = serde_json::json!("seg-1");
        row["retired_field"] = serde_json::json!(true);
        assert!(decode_output_segment_row(&row).is_err());
    }

    #[test]
    fn message_row_splits_physical_identity_from_strict_payload() {
        let decoded = decode_transcript_message_row(&transcript_message_row()).unwrap();
        assert_eq!(decoded.doc_id, "msg-1");
        assert_eq!(decoded.message.sequence, 3);
        assert_eq!(decoded.message.outcome, OutputOutcome::Complete);
        assert_eq!(decoded.message.blocks.len(), 1);
    }

    #[test]
    fn message_row_rejects_absent_blank_and_unknown_fields() {
        let mut row = transcript_message_row();
        row.as_object_mut().unwrap().remove("_docID");
        assert!(decode_transcript_message_row(&row).is_err());

        row["_docID"] = serde_json::json!("  ");
        assert!(decode_transcript_message_row(&row).is_err());

        row["_docID"] = serde_json::json!("msg-1");
        row["content"] = serde_json::json!("retired serialized native message");
        assert!(decode_transcript_message_row(&row).is_err());
    }

    /// A minimal closed segment whose closing record declares no streams: the
    /// serialized `stream_bytes` is a JSON empty array, which must survive as
    /// a JSON array in the `$input` variable (not a nillable SDL list `null`).
    fn closed_segment_no_streams() -> gents_protocol::output::OutputSegment {
        gents_protocol::output::OutputSegment {
            agent_did: "agent".into(),
            requester_did: None,
            session_id: "session".into(),
            request_doc_id: "request".into(),
            source: gents_protocol::output::OutputSource::Authored {
                key: "prompt".into(),
            },
            writer: gents_protocol::output::OutputWriter::RequestExecution {
                execution_generation: "gen-1".into(),
            },
            ordinal: None,
            runs: Vec::new(),
            payload: String::new(),
            close: Some(gents_protocol::output::SourceClose::Closed {
                outcome: gents_protocol::output::OutputOutcome::Complete,
                segments: 0,
                stream_bytes: Vec::new(),
            }),
            created_at: "2026-09-09T22:30:00Z".into(),
        }
    }

    #[test]
    fn segment_create_variables_preserve_json_empty_arrays() {
        let variables = output_segment_create_variables(&closed_segment_no_streams()).unwrap();
        let input = variables.get("input").unwrap();
        assert_eq!(input["close"]["stream_bytes"], serde_json::json!([]));
        assert!(input["close"]["stream_bytes"].is_array());
        assert!(input["runs"].is_null(), "empty runs skip-serialize");
    }

    #[test]
    fn segment_create_variables_shape_matches_execute_with_variables_pattern() {
        let variables = output_segment_create_variables(&closed_segment_no_streams()).unwrap();
        let object = variables.as_object().unwrap();
        assert_eq!(object.len(), 1, "exactly the $input variable");
        assert!(object.contains_key("input"));
        assert!(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION
            .contains("$input: AgentOutputSegmentMutationInputArg!"));
        assert!(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION
            .contains("create_AgentOutputSegment(input: $input)"));
    }

    #[test]
    fn message_create_variables_preserve_unicode_payloads() {
        let mut message = decode_transcript_message_row(&transcript_message_row())
            .unwrap()
            .message;
        // 18 UTF-8 bytes, 10 chars: byte-exactness must survive the variable.
        let unicode = "héllo 🌍 中文";
        message.blocks = vec![gents_protocol::output::MessageBlock::Text {
            text: gents_protocol::output::PresentedPayload {
                output: gents_protocol::output::PayloadRef {
                    close_doc_id: "close-1".into(),
                    stream: 0,
                },
                presentation: gents_protocol::output::PayloadPresentation::Composed {
                    parts: vec![gents_protocol::output::PresentationPart::Literal {
                        text: unicode.into(),
                    }],
                },
            },
        }];
        let variables = transcript_message_create_variables(&message).unwrap();
        let input = variables.get("input").unwrap();
        let literal = &input["blocks"][0]["text"]["presentation"]["parts"][0]["text"];
        assert_eq!(literal.as_str().unwrap(), unicode);
        assert_eq!(
            literal.as_str().unwrap().len(),
            18,
            "UTF-8 byte length preserved"
        );
        assert_eq!(literal.as_str().unwrap().chars().count(), 10);
        assert!(CREATE_AGENT_MESSAGE_MUTATION.contains("$input: AgentMessageMutationInputArg!"));
        assert!(CREATE_AGENT_MESSAGE_MUTATION.contains("create_AgentMessage(input: $input)"));
    }

    #[test]
    fn message_create_variables_preserve_arbitrary_additional_params_keys() {
        let mut message = decode_transcript_message_row(&transcript_message_row())
            .unwrap()
            .message;
        // Arbitrary provider-opaque object keys, including empty-string and
        // GraphQL-hostile characters, must pass through the variable verbatim.
        message.blocks = vec![gents_protocol::output::MessageBlock::ToolCall {
            tool_call_doc_id: "call-1".into(),
            id: "id-1".into(),
            call_id: None,
            name: "bash".into(),
            arguments: gents_protocol::output::PayloadRef {
                close_doc_id: "close-1".into(),
                stream: 0,
            },
            signature: None,
            additional_params: Some(serde_json::json!({
                "": "empty key",
                "key with spaces": 1,
                "$weird/\\\"key\\\"": [true, null],
                "nested": {"ünïcode": "🌍"}
            })),
        }];
        let variables = transcript_message_create_variables(&message).unwrap();
        let params = &variables["input"]["blocks"][0]["additional_params"];
        let params = params.as_object().unwrap();
        assert_eq!(params.len(), 4);
        assert_eq!(params.get("").unwrap(), "empty key");
        assert_eq!(params.get("key with spaces").unwrap(), 1);
        assert_eq!(
            params.get("$weird/\\\"key\\\"").unwrap(),
            &serde_json::json!([true, null])
        );
        assert_eq!(params.get("nested").unwrap()["ünïcode"], "🌍");
    }
}
