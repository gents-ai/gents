use crate::store::graphql::escape_graphql_string;
use anyhow::{Context, Result};

pub const AGENT_OUTPUT_SEGMENT_FIELDS: &str = "node_did requester_did session_id \
request_doc_id source ordinal writer runs payload close created_at _docID";

pub const AGENT_MESSAGE_FIELDS: &str = "message_key session_id node_did requester_did \
request_doc_id publication outcome sequence role native_id blocks created_at _docID";

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

pub fn decode_output_segment_row(row: &serde_json::Value) -> Result<OutputSegmentRow> {
    let (doc_id, document) = split_physical_doc_id(row, "AgentOutputSegment")?;
    let segment: gents_protocol::output::OutputSegment =
        serde_json::from_value(document).context("decoding canonical AgentOutputSegment")?;
    Ok(OutputSegmentRow { doc_id, segment })
}

pub fn decode_transcript_message_row(row: &serde_json::Value) -> Result<TranscriptMessageRow> {
    let (doc_id, document) = split_physical_doc_id(row, "AgentMessage")?;
    let message: gents_protocol::output::TranscriptMessage =
        serde_json::from_value(document).context("decoding canonical AgentMessage")?;
    Ok(TranscriptMessageRow { doc_id, message })
}

pub(crate) fn request_output_segments_filter(request_doc_id: &str) -> String {
    format!(
        "request_doc_id: {{ _eq: \"{}\" }}",
        gents_protocol::graphql::escape_graphql_string(request_doc_id)
    )
}

pub fn request_output_segments_query(request_doc_id: &str) -> String {
    let filter = request_output_segments_filter(request_doc_id);
    format!("{{ AgentOutputSegment(filter: {{ {filter} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}")
}

pub(crate) fn source_output_segments_filter(
    request_doc_id: &str,
    source: &gents_protocol::output::OutputSource,
) -> Result<String> {
    let request = request_output_segments_filter(request_doc_id);
    let discriminator =
        gents_protocol::graphql::graphql_input_literal(&output_source_discriminator(source))?;
    Ok(format!("{request}, source: {discriminator}"))
}

/// DefraDB indexes JSON leaves, not whole objects. Readers must check complete
/// typed source equality after this discriminator lookup, without a row limit
/// that could hide conflicting observations of the same source.
pub(crate) fn source_output_segments_query(
    request_doc_id: &str,
    source: &gents_protocol::output::OutputSource,
) -> Result<String> {
    let filter = source_output_segments_filter(request_doc_id, source)?;
    Ok(format!(
        "{{ AgentOutputSegment(filter: {{ {filter} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"
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
) -> Result<Vec<OutputSegmentRow>> {
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
    decode: impl Fn(&serde_json::Value) -> Result<T>,
) -> Result<Vec<T>> {
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
) -> Result<serde_json::Value> {
    let input =
        serde_json::to_value(segment).context("serializing canonical AgentOutputSegment")?;
    Ok(serde_json::Value::Object(serde_json::Map::from_iter([(
        "input".to_owned(),
        input,
    )])))
}

pub fn transcript_message_create_variables(
    message: &gents_protocol::output::TranscriptMessage,
) -> Result<serde_json::Value> {
    let mut input = serde_json::to_value(message).context("serializing canonical AgentMessage")?;
    if let Some(compact) = gents_protocol::output::storage::encode_blocks(&message.blocks)? {
        input["blocks"] = compact;
    }
    Ok(serde_json::Value::Object(serde_json::Map::from_iter([(
        "input".to_owned(),
        input,
    )])))
}

/// Capture the physical session while creating a child so its creator can
/// restore the native parent relationship after the document commits.
pub(crate) async fn session_child_binding_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<(String, String)> {
    anyhow::ensure!(
        txn.actor_did().is_some(),
        "session child creation requires an explicit actor"
    );
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let response = txn
        .execute(&format!(
            "{{ AgentSession(filter: {{ {scope} }}, limit: 2) {{ _docID agent_did }} }}"
        ))
        .await?;
    let rows = response["data"]["AgentSession"]
        .as_array()
        .context("session child parent lookup omitted its authorized result")?;
    anyhow::ensure!(
        rows.len() == 1,
        "session child requires one visible physical session"
    );
    let session_doc = rows[0]["_docID"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("session child parent is missing its document ID")?;
    let agent = rows[0]["agent_did"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("session child parent is missing its selected agent")?;
    Ok((session_doc.to_owned(), agent.to_owned()))
}

#[derive(Clone, Copy)]
pub(crate) enum SessionChildSource<'a> {
    Request(gents_protocol::request_admission::RequestPurpose),
    Output(&'a gents_protocol::output::OutputSource),
    Capture(&'a gents_protocol::rendered_request::CaptureScope),
}

pub(crate) async fn session_child_binding_for_request_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
    source: SessionChildSource<'_>,
) -> Result<(String, String)> {
    use gents_protocol::output::OutputSource;
    use gents_protocol::request_admission::RequestPurpose;
    let purpose = match source {
        SessionChildSource::Request(purpose) => purpose,
        SessionChildSource::Output(OutputSource::ProviderTurn { scope, .. }) => {
            scope.kind.request_purpose()
        }
        SessionChildSource::Output(_) => RequestPurpose::Normal,
        SessionChildSource::Capture(scope) => scope.kind.request_purpose(),
    };
    if !purpose.is_auxiliary() {
        return agent_session_child_binding_in_txn(txn, node_did, session_id, requester_did).await;
    }
    async fn request_row(
        txn: &crate::store::config_client::ConfigApplyTxn<'_>,
        doc_id: &str,
    ) -> Result<gents_protocol::row::AgentRequestRow> {
        let response = txn
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ {} }} }}"#,
                escape_graphql_string(doc_id),
                crate::request::admission::SIGNED_REQUEST_FIELDS,
            ))
            .await?;
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .context("auxiliary request lookup omitted rows")?;
        anyhow::ensure!(rows.len() == 1, "auxiliary request is missing or ambiguous");
        let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(rows[0].clone())?;
        anyhow::ensure!(
            row.doc_id.as_deref() == Some(doc_id),
            "auxiliary request physical identity differs"
        );
        Ok(row)
    }
    let auxiliary = request_row(txn, request_doc_id)
        .await
        .context("load auxiliary child request")?;
    anyhow::ensure!(
        auxiliary.purpose == Some(purpose)
            && auxiliary.node_did.as_deref() == Some(node_did)
            && auxiliary.session_id.as_deref() == Some(session_id)
            && auxiliary.requester_did.as_deref() == requester_did
            && auxiliary.agent_did.as_deref() == txn.actor_did(),
        "auxiliary child differs from its request or selected actor"
    );
    let parent_doc = auxiliary
        .caused_by_parent_request_doc_id
        .as_deref()
        .context("auxiliary request lacks its physical parent")?;
    let parent = request_row(txn, parent_doc)
        .await
        .context("load auxiliary parent request")?;
    crate::request::admission::verify_historical_auxiliary_receipt(&auxiliary, &parent)?;
    let (session_doc, agent) = agent_session_child_binding_in_txn(
        txn,
        node_did,
        session_id,
        parent.requester_did.as_deref(),
    )
    .await?;
    anyhow::ensure!(
        auxiliary.agent_did.as_deref() == Some(agent.as_str())
            && parent
                .session_doc_id
                .as_deref()
                .is_none_or(|doc| doc == session_doc),
        "auxiliary parent differs from its physical session"
    );
    Ok((session_doc, agent))
}

pub(crate) async fn output_segment_bound_variables(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    segment: &gents_protocol::output::OutputSegment,
) -> Result<serde_json::Value> {
    let (session_doc, agent) = session_child_binding_for_request_in_txn(
        txn,
        &segment.node_did,
        &segment.session_id,
        segment.requester_did.as_deref(),
        &segment.request_doc_id,
        SessionChildSource::Output(&segment.source),
    )
    .await?;
    let mut variables = output_segment_create_variables(segment)?;
    variables["input"]["session_doc_id"] = session_doc.into();
    variables["input"]["agent_did"] = agent.into();
    variables["input"]["creator_did"] = txn
        .actor_did()
        .context("session child creator missing")?
        .into();
    variables["input"]["parent_linked"] = false.into();
    Ok(variables)
}

pub(crate) async fn transcript_message_bound_variables(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    message: &gents_protocol::output::TranscriptMessage,
) -> Result<serde_json::Value> {
    let (session_doc, agent) = agent_session_child_binding_in_txn(
        txn,
        &message.node_did,
        &message.session_id,
        message.requester_did.as_deref(),
    )
    .await?;
    transcript_message_variables_with_binding(
        message,
        session_doc,
        agent,
        txn.actor_did().context("session child creator missing")?,
    )
}

pub(crate) async fn fork_message_bound_variables(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    message: &gents_protocol::output::TranscriptMessage,
) -> Result<serde_json::Value> {
    let (session_doc, agent) = session_child_binding_in_txn(
        txn,
        &message.node_did,
        &message.session_id,
        message.requester_did.as_deref(),
    )
    .await?;
    transcript_message_variables_with_binding(
        message,
        session_doc,
        agent,
        txn.actor_did().context("session child creator missing")?,
    )
}

fn transcript_message_variables_with_binding(
    message: &gents_protocol::output::TranscriptMessage,
    session_doc: String,
    agent: String,
    creator: &str,
) -> Result<serde_json::Value> {
    let mut variables = transcript_message_create_variables(message)?;
    variables["input"]["session_doc_id"] = session_doc.into();
    variables["input"]["agent_did"] = agent.into();
    variables["input"]["creator_did"] = creator.into();
    variables["input"]["parent_linked"] = false.into();
    Ok(variables)
}

pub(crate) async fn agent_session_child_binding_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<(String, String)> {
    let binding = session_child_binding_in_txn(txn, node_did, session_id, requester_did).await?;
    anyhow::ensure!(
        txn.actor_did() == Some(binding.1.as_str()),
        "runtime session child requires its selected agent actor"
    );
    Ok(binding)
}

#[cfg(test)]
#[path = "canonical_rows/auxiliary_binding_contract_tests.rs"]
mod auxiliary_binding_contract_tests;

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
