use serde_json::Value;

use super::{OutputSegment, OutputSource, TranscriptMessage};
use crate::graphql::{escape_graphql_string, graphql_input_literal, GraphqlError};

pub const AGENT_OUTPUT_SEGMENT_FIELDS: &str = "node_did requester_did session_id \
request_doc_id source ordinal writer runs payload close created_at _docID";

pub const AGENT_MESSAGE_FIELDS: &str = "message_key session_id node_did requester_did \
request_doc_id publication outcome sequence role native_id blocks created_at _docID";

#[derive(Debug, thiserror::Error)]
pub enum CanonicalRowError {
    #[error("{collection} row omitted _docID")]
    MissingDocId { collection: &'static str },
    #[error("{collection} row is not an object")]
    NotAnObject { collection: &'static str },
    #[error("decoding canonical {collection}")]
    Decode {
        collection: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("serializing canonical {collection}")]
    Encode {
        collection: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Blocks(serde_json::Error),
    #[error(transparent)]
    Graphql(#[from] GraphqlError),
}

#[derive(Debug, Clone)]
pub struct OutputSegmentRow {
    pub doc_id: String,
    pub segment: OutputSegment,
}

#[derive(Debug, Clone)]
pub struct TranscriptMessageRow {
    pub doc_id: String,
    pub message: TranscriptMessage,
}

fn split_physical_doc_id(
    row: &Value,
    collection: &'static str,
) -> Result<(String, Value), CanonicalRowError> {
    let doc_id = row
        .get("_docID")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(CanonicalRowError::MissingDocId { collection })?
        .to_string();
    let mut document = row.clone();
    document
        .as_object_mut()
        .ok_or(CanonicalRowError::NotAnObject { collection })?
        .remove("_docID");
    Ok((doc_id, document))
}

pub fn decode_output_segment_row(row: &Value) -> Result<OutputSegmentRow, CanonicalRowError> {
    let collection = "AgentOutputSegment";
    let (doc_id, document) = split_physical_doc_id(row, collection)?;
    let segment: OutputSegment = serde_json::from_value(document)
        .map_err(|source| CanonicalRowError::Decode { collection, source })?;
    Ok(OutputSegmentRow { doc_id, segment })
}

pub fn decode_transcript_message_row(
    row: &Value,
) -> Result<TranscriptMessageRow, CanonicalRowError> {
    let collection = "AgentMessage";
    let (doc_id, document) = split_physical_doc_id(row, collection)?;
    let message: TranscriptMessage = serde_json::from_value(document)
        .map_err(|source| CanonicalRowError::Decode { collection, source })?;
    Ok(TranscriptMessageRow { doc_id, message })
}

pub fn request_output_segments_filter(request_doc_id: &str) -> String {
    format!(
        "request_doc_id: {{ _eq: \"{}\" }}",
        escape_graphql_string(request_doc_id)
    )
}

pub fn request_output_segments_query(request_doc_id: &str) -> String {
    let filter = request_output_segments_filter(request_doc_id);
    format!("{{ AgentOutputSegment(filter: {{ {filter} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}")
}

pub fn source_output_segments_filter(
    request_doc_id: &str,
    source: &OutputSource,
) -> Result<String, CanonicalRowError> {
    let request = request_output_segments_filter(request_doc_id);
    let discriminator = graphql_input_literal(&output_source_discriminator(source))?;
    Ok(format!("{request}, source: {discriminator}"))
}

/// DefraDB indexes JSON leaves, not whole objects. Readers must check complete
/// typed source equality after this discriminator lookup, without a row limit
/// that could hide conflicting observations of the same source.
pub fn source_output_segments_query(
    request_doc_id: &str,
    source: &OutputSource,
) -> Result<String, CanonicalRowError> {
    let filter = source_output_segments_filter(request_doc_id, source)?;
    Ok(format!(
        "{{ AgentOutputSegment(filter: {{ {filter} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"
    ))
}

/// DefraDB JSON filters match indexed scalar leaves. This discriminator may
/// also match another typed source; callers must verify full source equality.
pub fn output_source_discriminator(source: &OutputSource) -> Value {
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
    let request_doc_id = escape_graphql_string(request_doc_id);
    format!(
        r#"{{
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}
            AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {AGENT_MESSAGE_FIELDS} }}
        }}"#
    )
}

pub fn decode_scoped_request_output_segments(
    rows: &[Value],
    node_did: &str,
    session_id: Option<&str>,
    requester_did: Option<&str>,
) -> Result<Vec<OutputSegmentRow>, CanonicalRowError> {
    decode_scoped_canonical_rows(
        rows,
        node_did,
        session_id,
        requester_did,
        decode_output_segment_row,
    )
}

pub fn decode_scoped_canonical_rows<T, E, F>(
    rows: &[Value],
    node_did: &str,
    session_id: Option<&str>,
    requester_did: Option<&str>,
    decode: F,
) -> Result<Vec<T>, E>
where
    F: Fn(&Value) -> Result<T, E>,
{
    fn text<'a>(row: &'a Value, field: &str) -> Option<&'a str> {
        row.get(field).and_then(Value::as_str)
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

pub fn authored_message_key(request_doc_id: &str, key: &str) -> String {
    format!("authored:{request_doc_id}:{key}")
}

pub fn output_segment_create_variables(
    segment: &OutputSegment,
) -> Result<Value, CanonicalRowError> {
    let input = serde_json::to_value(segment).map_err(|source| CanonicalRowError::Encode {
        collection: "AgentOutputSegment",
        source,
    })?;
    Ok(Value::Object(serde_json::Map::from_iter([(
        "input".to_owned(),
        input,
    )])))
}

pub fn transcript_message_create_variables(
    message: &TranscriptMessage,
) -> Result<Value, CanonicalRowError> {
    let mut input = serde_json::to_value(message).map_err(|source| CanonicalRowError::Encode {
        collection: "AgentMessage",
        source,
    })?;
    if let Some(compact) =
        super::storage::encode_blocks(&message.blocks).map_err(CanonicalRowError::Blocks)?
    {
        input["blocks"] = compact;
    }
    Ok(Value::Object(serde_json::Map::from_iter([(
        "input".to_owned(),
        input,
    )])))
}
