pub const AGENT_OUTPUT_SEGMENT_FIELDS: &str = "node_did requester_did session_id \
request_doc_id source ordinal writer runs payload close created_at _docID";

pub const AGENT_MESSAGE_FIELDS: &str = "message_key session_id node_did requester_did \
request_doc_id publication outcome sequence role native_id blocks created_at _docID";

#[derive(Debug, thiserror::Error)]
pub enum CanonicalRowsError {
    #[error("{collection} row omitted _docID")]
    MissingDocId { collection: &'static str },
    #[error("{collection} row is not an object")]
    NotAnObject { collection: &'static str },
    #[error("decoding canonical AgentOutputSegment")]
    DecodeSegment(#[source] serde_json::Error),
    #[error("decoding canonical AgentMessage")]
    DecodeMessage(#[source] serde_json::Error),
    #[error("serializing canonical AgentOutputSegment")]
    EncodeSegment(#[source] serde_json::Error),
    #[error("serializing canonical AgentMessage")]
    EncodeMessage(#[source] serde_json::Error),
    #[error(transparent)]
    BlockEncode(serde_json::Error),
    #[error(transparent)]
    GraphqlInput(#[from] gents_protocol::graphql::GraphqlError),
}

#[derive(Debug, Clone)]
pub struct OutputSegmentRow {
    pub doc_id: String,
    pub segment: gents_protocol::output::OutputSegment,
}

#[derive(Debug, Clone)]
pub struct TranscriptMessageRow {
    pub doc_id: String,
    pub message: gents_protocol::output::TranscriptMessage,
}

fn split_physical_doc_id(
    row: &serde_json::Value,
    collection: &'static str,
) -> Result<(String, serde_json::Value), CanonicalRowsError> {
    let doc_id = row
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(CanonicalRowsError::MissingDocId { collection })?
        .to_string();
    let mut document = row.clone();
    document
        .as_object_mut()
        .ok_or(CanonicalRowsError::NotAnObject { collection })?
        .remove("_docID");
    Ok((doc_id, document))
}

pub fn decode_output_segment_row(
    row: &serde_json::Value,
) -> Result<OutputSegmentRow, CanonicalRowsError> {
    let (doc_id, document) = split_physical_doc_id(row, "AgentOutputSegment")?;
    let segment: gents_protocol::output::OutputSegment =
        serde_json::from_value(document).map_err(CanonicalRowsError::DecodeSegment)?;
    Ok(OutputSegmentRow { doc_id, segment })
}

pub fn decode_transcript_message_row(
    row: &serde_json::Value,
) -> Result<TranscriptMessageRow, CanonicalRowsError> {
    let (doc_id, document) = split_physical_doc_id(row, "AgentMessage")?;
    let message: gents_protocol::output::TranscriptMessage =
        serde_json::from_value(document).map_err(CanonicalRowsError::DecodeMessage)?;
    Ok(TranscriptMessageRow { doc_id, message })
}

pub fn request_output_segments_query(request_doc_id: &str) -> String {
    format!(
        r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
        gents_protocol::graphql::escape_graphql_string(request_doc_id)
    )
}

/// DefraDB indexes JSON leaves, not whole objects. Readers must check complete
/// typed source equality after this discriminator lookup, without a row limit
/// that could hide conflicting observations of the same source.
pub(crate) fn source_output_segments_query(
    request_doc_id: &str,
    source: &gents_protocol::output::OutputSource,
) -> Result<String, CanonicalRowsError> {
    let discriminator = output_source_discriminator(source);
    let filter = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "request_doc_id": {"_eq": request_doc_id},
        "source": discriminator,
    }))?;
    Ok(format!(
        "{{ AgentOutputSegment(filter: {filter}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"
    ))
}

/// DefraDB JSON filters match indexed scalar leaves. This discriminator may
/// also match another typed source; callers must verify full source equality.
pub fn output_source_discriminator(
    source: &gents_protocol::output::OutputSource,
) -> serde_json::Value {
    use gents_protocol::output::OutputSource;

    match source {
        OutputSource::ProviderTurn { turn_index, .. } => {
            serde_json::json!({"turn_index": {"_eq": turn_index}})
        }
        OutputSource::ToolCall { tool_call_doc_id }
        | OutputSource::ToolEffectArguments { tool_call_doc_id } => {
            serde_json::json!({"tool_call_doc_id": {"_eq": tool_call_doc_id}})
        }
        OutputSource::Authored { key } => serde_json::json!({"key": {"_eq": key}}),
    }
}

pub fn request_output_facts_query(request_doc_id: &str) -> String {
    let request_doc_id = gents_protocol::graphql::escape_graphql_string(request_doc_id);
    format!(
        r#"{{
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}
            AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {AGENT_MESSAGE_FIELDS} }}
        }}"#
    )
}

pub fn decode_scoped_request_output_segments(
    rows: &[serde_json::Value],
    node_did: &str,
    session_id: Option<&str>,
    requester_did: Option<&str>,
) -> Result<Vec<OutputSegmentRow>, CanonicalRowsError> {
    decode_scoped_canonical_rows(
        rows,
        node_did,
        session_id,
        requester_did,
        decode_output_segment_row,
    )
}

pub fn decode_scoped_canonical_rows<T>(
    rows: &[serde_json::Value],
    node_did: &str,
    session_id: Option<&str>,
    requester_did: Option<&str>,
    decode: impl Fn(&serde_json::Value) -> Result<T, CanonicalRowsError>,
) -> Result<Vec<T>, CanonicalRowsError> {
    fn text<'a>(row: &'a serde_json::Value, field: &str) -> Option<&'a str> {
        row.get(field).and_then(serde_json::Value::as_str)
    }
    rows.iter()
        .filter(|row| {
            text(row, "node_did") == Some(node_did)
                && text(row, "requester_did") == requester_did
                && session_id.is_none_or(|session| text(row, "session_id") == Some(session))
        })
        .map(decode)
        .collect()
}

/// The document travels as a typed GraphQL variable, never as rendered
/// GraphQL text: JSON keys and JSON empty arrays are preserved verbatim by
/// the variable encoder and are not nillable SDL lists.
pub const CREATE_AGENT_OUTPUT_SEGMENT_MUTATION: &str = "mutation($input: \
AgentOutputSegmentMutationInputArg!) { create_AgentOutputSegment(input: $input) { _docID } }";

pub const CREATE_AGENT_MESSAGE_MUTATION: &str = "mutation($input: \
AgentMessageMutationInputArg!) { create_AgentMessage(input: $input) { _docID } }";

pub(crate) fn authored_message_key(request_doc_id: &str, key: &str) -> String {
    format!("authored:{request_doc_id}:{key}")
}

pub fn output_segment_create_variables(
    segment: &gents_protocol::output::OutputSegment,
) -> Result<serde_json::Value, CanonicalRowsError> {
    let input = serde_json::to_value(segment).map_err(CanonicalRowsError::EncodeSegment)?;
    Ok(serde_json::Value::Object(serde_json::Map::from_iter([(
        "input".to_owned(),
        input,
    )])))
}

pub fn transcript_message_create_variables(
    message: &gents_protocol::output::TranscriptMessage,
) -> Result<serde_json::Value, CanonicalRowsError> {
    let mut input = serde_json::to_value(message).map_err(CanonicalRowsError::EncodeMessage)?;
    if let Some(compact) = gents_protocol::output::storage::encode_blocks(&message.blocks)
        .map_err(CanonicalRowsError::BlockEncode)?
    {
        input["blocks"] = compact;
    }
    Ok(serde_json::Value::Object(serde_json::Map::from_iter([(
        "input".to_owned(),
        input,
    )])))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_only_segment_row() -> serde_json::Value {
        serde_json::json!({
            "_docID": "seg-1",
            "node_did": "agent",
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
            "node_did": "agent",
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
    fn request_scope_drops_malformed_foreign_payloads_before_decoding() {
        let accepted = terminal_only_segment_row();
        let mut rows = vec![accepted.clone()];
        for (field, value) in [
            ("node_did", "foreign"),
            ("requester_did", "foreign"),
            ("session_id", "foreign"),
        ] {
            let mut row = accepted.clone();
            row[field] = serde_json::json!(value);
            row["source"] = serde_json::json!("malformed");
            rows.push(row);
        }
        let decoded =
            decode_scoped_request_output_segments(&rows, "agent", Some("session"), None).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(
            serde_json::to_value(&decoded[0].segment).unwrap(),
            serde_json::to_value(decode_output_segment_row(&accepted).unwrap().segment).unwrap()
        );
        let mut malformed = accepted;
        malformed["source"] = serde_json::json!("malformed");
        assert!(decode_scoped_request_output_segments(
            &[malformed],
            "agent",
            Some("session"),
            None
        )
        .is_err());
    }

    #[test]
    fn request_facts_use_request_indexes_and_drop_foreign_rows_before_decode() {
        let query = request_output_facts_query("request");
        assert!(
            query.contains("AgentOutputSegment(filter: { request_doc_id: { _eq: \"request\" } }")
        );
        assert!(query.contains("AgentMessage(filter: { request_doc_id: { _eq: \"request\" } }"));
        assert!(!query.contains("node_did:"));
        assert!(!query.contains("session_id:"));

        let accepted_message = serde_json::json!({
            "_docID": "message-1",
            "message_key": "key",
            "session_id": "session",
            "node_did": "agent",
            "request_doc_id": "request",
            "publication": {"kind": "request_execution", "execution_generation": "gen-1"},
            "outcome": "complete",
            "sequence": 3,
            "role": "assistant",
            "blocks": [],
            "created_at": "2026-09-09T22:30:00Z"
        });
        let mut message_rows = vec![accepted_message.clone()];
        for (field, value) in [
            ("node_did", "foreign"),
            ("requester_did", "foreign"),
            ("session_id", "foreign"),
        ] {
            let mut row = accepted_message.clone();
            row[field] = serde_json::json!(value);
            row["publication"] = serde_json::json!("malformed");
            message_rows.push(row);
        }
        let decoded = decode_scoped_canonical_rows(
            &message_rows,
            "agent",
            Some("session"),
            None,
            decode_transcript_message_row,
        )
        .unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].doc_id, "message-1");
        assert_eq!(decoded[0].message.sequence, 3);
    }

    #[test]
    fn hydration_request_scope_keeps_other_sessions_and_exact_requester() {
        let mut row = terminal_only_segment_row();
        row["requester_did"] = serde_json::json!("requester");
        assert_eq!(
            decode_scoped_request_output_segments(&[row.clone()], "agent", None, Some("requester"))
                .unwrap()
                .len(),
            1
        );
        assert!(decode_scoped_request_output_segments(
            &[row.clone()],
            "agent",
            Some("other-session"),
            Some("requester")
        )
        .unwrap()
        .is_empty());
        assert!(
            decode_scoped_request_output_segments(&[row], "agent", None, None)
                .unwrap()
                .is_empty()
        );
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
    fn message_row_accepts_defra_null_for_empty_blocks() {
        let mut row = transcript_message_row();
        row["blocks"] = serde_json::Value::Null;
        let decoded = decode_transcript_message_row(&row).unwrap();
        assert!(decoded.message.blocks.is_empty());
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

    fn closed_segment_no_streams() -> gents_protocol::output::OutputSegment {
        gents_protocol::output::OutputSegment {
            node_did: "agent".into(),
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
