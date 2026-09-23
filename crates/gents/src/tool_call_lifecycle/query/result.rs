//! Canonical invocation-reply reads, keyed by physical tool identity.

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::llm::message::{AssistantContent, Message, ToolResultContent, UserContent};
use anyhow::{anyhow, Context, Result};
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, TranscriptMessage};
use serde::Deserialize;

/// Load the native tool-result delivery for one physical AgentToolCall
/// document.
///
/// Selection is exact physical identity end to end. The addressed
/// `AgentToolCall` row is resolved by `_docID` within its
/// agent/session/requester scope; the canonical `AgentMessage` headers of
/// that session scope are then strictly decoded and the single eligible
/// invocation reply is matched by `MessagePublication::ToolDelivery` naming
/// this exact physical document, the expected request/session/agent/requester
/// binding, and its one native `ToolResult` block bound to the same physical
/// document. The delivered user message is reconstructed through the session
/// owner (`session::load_canonical_message`) — no manual segment fetch, no
/// duplicate presentation application — and verified against the exact native
/// call identity. The background ordinary completion notification shares the
/// `ToolDelivery` publication but carries ordinary text blocks; it is a
/// distinct delivery and is never selected as the invocation reply. Ambiguous
/// eligible replies are an error; a missing reply (including a completion
/// notification published without a reply) is an error, never an empty
/// result. There is no `native_id` lookup, no name/native-id alias, and no
/// legacy fallback.
pub async fn load_tool_call_result(
    access: &ConfigAccess,
    tool_call_doc_id: &str,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Message> {
    load_tool_call_read(
        access,
        tool_call_doc_id,
        agent_did,
        session_id,
        requester_did,
    )
    .await?
    .1
    .ok_or_else(|| {
        anyhow!(
            "no invocation reply delivered for tool_call_doc_id={tool_call_doc_id}; the tool result has not been delivered"
        )
    })
}

async fn load_tool_call_read(
    access: &ConfigAccess,
    tool_call_doc_id: &str,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<(String, Option<Message>)> {
    let (call, headers, accepted_call_id, accepted_arguments) = load_accepted_tool_call(
        access,
        tool_call_doc_id,
        agent_did,
        session_id,
        requester_did,
    )
    .await?;
    let native_call_id = call.tool_call_id;
    let expected_request_doc_id = call.request_doc_id.ok_or_else(|| {
        anyhow!(
            "AgentToolCall tool_call_doc_id={tool_call_doc_id} carries no request \
             document; a tool delivery cannot be bound to it"
        )
    })?;

    let delivery_rows: Vec<&crate::session::canonical_rows::TranscriptMessageRow> = headers
        .iter()
        .filter(|row| {
            matches!(
                &row.message.publication,
                MessagePublication::ToolDelivery { tool_call_doc_id: named }
                    if named == tool_call_doc_id
            )
        })
        .collect();
    let replies: Vec<&crate::session::canonical_rows::TranscriptMessageRow> = delivery_rows
        .iter()
        .copied()
        .filter(|row| is_invocation_reply(&row.message, tool_call_doc_id))
        .collect();
    anyhow::ensure!(
        replies.len() <= 1,
        "ambiguous invocation reply header for tool_call_doc_id={tool_call_doc_id}: \
         {} eligible replies in session_id={session_id}",
        replies.len()
    );
    let Some(row) = replies.into_iter().next() else {
        anyhow::ensure!(
            delivery_rows.is_empty(),
            "a ToolDelivery publication exists for tool_call_doc_id={tool_call_doc_id} but carries no native tool-result invocation reply"
        );
        return Ok((accepted_arguments, None));
    };
    let header = &row.message;
    anyhow::ensure!(
        crate::lifecycle::is_exact_invocation_reply(
            header,
            &expected_request_doc_id,
            session_id,
            tool_call_doc_id,
            &native_call_id,
            &accepted_call_id,
        ),
        "tool invocation reply conflicts with its accepted native binding"
    );

    // The delivery header must carry the call's expected binding.
    anyhow::ensure!(
        header.agent_did == agent_did
            && header.session_id == session_id
            && header.requester_did.as_deref() == requester_did,
        "invocation reply for tool_call_doc_id={tool_call_doc_id} crossed the \
         requested agent/session/requester scope"
    );
    anyhow::ensure!(
        header.request_doc_id.as_deref() == Some(expected_request_doc_id.as_str()),
        "invocation reply for tool_call_doc_id={tool_call_doc_id} is bound to \
         request {:?} but the call expects request {expected_request_doc_id}",
        header.request_doc_id
    );

    // The single native ToolResult block carries the exact native identity.
    // `is_invocation_reply` guarantees exactly one block and that it is a
    // `ToolResult`, so the fallthrough below is unreachable in practice; it
    // exists so the shape is enforced by construction, not by indexing.
    let MessageBlock::ToolResult {
        id: block_id,
        call_id: block_call_id,
        ..
    } = header.blocks.first().ok_or_else(|| {
        anyhow!(
            "invocation reply for tool_call_doc_id={tool_call_doc_id} carries no \
                 blocks"
        )
    })?
    else {
        anyhow::bail!(
            "invocation reply for tool_call_doc_id={tool_call_doc_id} carries no \
             native ToolResult block"
        );
    };
    anyhow::ensure!(
        block_id == &native_call_id,
        "invocation reply for tool_call_doc_id={tool_call_doc_id} names native id \
         {block_id} but the call is {native_call_id}"
    );

    let (_header, message) =
        crate::session::load_canonical_message(access, &row.doc_id, agent_did, requester_did)
            .await?;
    verify_exact_call_identity(
        &message,
        &native_call_id,
        block_call_id.as_deref(),
        tool_call_doc_id,
    )?;
    Ok((accepted_arguments, Some(message)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalToolCallPresentation {
    pub arguments: String,
    /// `None` means the invocation reply has not been durably delivered yet;
    /// an empty delivered result is `Some("")`.
    pub result: Option<String>,
    /// Canonical open ToolOutput bytes for a still-running spawned process.
    pub live_output: Option<String>,
}

/// Read the canonical JSON presentation of accepted arguments and the optional
/// durable provider-presented result for one physical tool call. Malformed,
/// conflicting, or ambiguous canonical facts are errors, never an incomplete
/// observation.
pub async fn load_tool_call_presentation(
    access: &ConfigAccess,
    tool_call_doc_id: &str,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<CanonicalToolCallPresentation> {
    let (arguments, result) = load_tool_call_read(
        access,
        tool_call_doc_id,
        agent_did,
        session_id,
        requester_did,
    )
    .await?;
    let mut result = result.as_ref().map(render_tool_result).transpose()?;
    let mut live_output = None;
    let call = load_tool_call_identity(
        access,
        tool_call_doc_id,
        agent_did,
        session_id,
        requester_did,
    )
    .await?;
    if call.spawned_by_tool_call_doc_id.is_some() {
        anyhow::ensure!(
            result.is_none(),
            "spawned process must not fabricate a native ToolResult delivery"
        );
        let request_doc_id = call
            .request_doc_id
            .as_deref()
            .context("spawned process is missing request document identity")?;
        let output = crate::background_tools::observe_canonical_tool_output_with_access(
            access,
            tool_call_doc_id,
            request_doc_id,
            session_id,
            agent_did,
            requester_did,
        )
        .await?;
        match output {
            crate::background_tools::CanonicalToolOutputObservation::Closed(output) => {
                result = Some(output);
            }
            crate::background_tools::CanonicalToolOutputObservation::Open(output)
                if !output.is_empty() =>
            {
                live_output = Some(output);
            }
            crate::background_tools::CanonicalToolOutputObservation::Open(_) => {}
        }
    }
    Ok(CanonicalToolCallPresentation {
        arguments,
        result,
        live_output,
    })
}

/// Load the canonical JSON presentation of the arguments accepted for one
/// physical tool call.
///
/// This uses the same coordinator admission/header validation as result
/// reconstruction. It never reads the retired AgentToolCall payload columns
/// and never treats a logical/native ID as physical identity.
pub async fn load_tool_call_arguments(
    access: &ConfigAccess,
    tool_call_doc_id: &str,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<String> {
    let (arguments, _) = load_tool_call_read(
        access,
        tool_call_doc_id,
        agent_did,
        session_id,
        requester_did,
    )
    .await?;
    Ok(arguments)
}

async fn load_accepted_tool_call(
    access: &ConfigAccess,
    tool_call_doc_id: &str,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<(
    ToolCallIdentityRow,
    Vec<crate::session::canonical_rows::TranscriptMessageRow>,
    Option<String>,
    String,
)> {
    let call = load_tool_call_identity(
        access,
        tool_call_doc_id,
        agent_did,
        session_id,
        requester_did,
    )
    .await?;
    let admission = if let Some(parent_doc_id) = call.spawned_by_tool_call_doc_id.as_deref() {
        let parent =
            load_tool_call_identity(access, parent_doc_id, agent_did, session_id, requester_did)
                .await?;
        anyhow::ensure!(
            call.request_doc_id == parent.request_doc_id
                && call.message_sequence == parent.message_sequence
                && parent.tool_name == crate::toolset::SPAWN_PROCESS_TOOL_NAME,
            "spawned tool presentation has incoherent accepted-parent provenance"
        );
        parent
    } else {
        load_tool_call_identity(
            access,
            tool_call_doc_id,
            agent_did,
            session_id,
            requester_did,
        )
        .await?
    };
    let expected_request_doc_id = call.request_doc_id.as_deref().ok_or_else(|| {
        anyhow!("AgentToolCall tool_call_doc_id={tool_call_doc_id} carries no request document")
    })?;
    let headers = scoped_canonical_headers(access, agent_did, session_id, requester_did).await?;
    let accepted = headers
        .iter()
        .filter(|row| {
            row.message.sequence == admission.message_sequence
                && row.message.request_doc_id.as_deref() == Some(expected_request_doc_id)
                && row.message.role == MessageRole::Assistant
                && matches!(
                    row.message.publication,
                    MessagePublication::RequestExecution { .. }
                )
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        accepted.len() == 1,
        "tool call lacks a unique coordinator admission header"
    );
    let (_, accepted_message) = crate::session::load_canonical_message(
        access,
        &accepted[0].doc_id,
        agent_did,
        requester_did,
    )
    .await?;
    let bindings = accepted[0]
        .message
        .blocks
        .iter()
        .filter_map(|block| match block {
            MessageBlock::ToolCall {
                tool_call_doc_id: id,
                id: native_id,
                call_id,
                name,
                ..
            } if id == &admission.doc_id
                && native_id == &admission.tool_call_id
                && name == &admission.tool_name =>
            {
                Some(call_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        bindings.len() == 1,
        "admission header does not uniquely bind the physical tool"
    );
    let mut call_id = bindings.into_iter().next().expect("one binding checked");
    let Message::Assistant { content, .. } = accepted_message else {
        anyhow::bail!("accepted tool publication is not an assistant message");
    };
    let arguments = content
        .iter()
        .filter_map(|item| match item {
            AssistantContent::ToolCall(tool)
                if tool.id == admission.tool_call_id
                    && tool.function.name == admission.tool_name =>
            {
                Some(&tool.function.arguments)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        arguments.len() == 1,
        "accepted native message does not uniquely bind tool arguments"
    );
    let arguments = if call.spawned_by_tool_call_doc_id.is_some() {
        let input: crate::background_tools::BackgroundToolArgs =
            serde_json::from_value(arguments[0].clone())
                .context("decoding accepted spawn_process arguments")?;
        anyhow::ensure!(
            input.tool_name == call.tool_name,
            "spawned tool name differs from accepted spawn_process input"
        );
        serde_json::to_string(&input.args).context("serializing accepted spawned-tool arguments")?
    } else {
        serde_json::to_string(arguments[0]).context("serializing accepted native tool arguments")?
    };
    if call.spawned_by_tool_call_doc_id.is_some() {
        call_id = None;
    }
    Ok((call, headers, call_id, arguments))
}

/// The invocation reply shape for one physical tool call: a
/// `MessagePublication::ToolDelivery` header naming that exact physical
/// document whose blocks are exactly one native `ToolResult` block bound to
/// the same physical document. A background ordinary completion notification
/// shares the `ToolDelivery` publication but carries ordinary text blocks; it
/// is a distinct delivery and never an invocation reply.
fn is_invocation_reply(header: &TranscriptMessage, tool_call_doc_id: &str) -> bool {
    let named = match &header.publication {
        MessagePublication::ToolDelivery { tool_call_doc_id } => tool_call_doc_id.as_str(),
        _ => return false,
    };
    header.role == MessageRole::User
        && named == tool_call_doc_id
        && matches!(
            header.blocks.as_slice(),
            [MessageBlock::ToolResult {
                tool_call_doc_id: block_doc_id,
                ..
            }] if block_doc_id == tool_call_doc_id
        )
}

/// All canonical `AgentMessage` headers of one exact
/// agent/session/requester scope, strictly decoded through the canonical row
/// owner. The `ToolDelivery` publication is a JSON scalar column, so exact
/// publication matching happens after decode; the scope bounds the scan and
/// no `limit` may hide a matching later header.
async fn scoped_canonical_headers(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<crate::session::canonical_rows::TranscriptMessageRow>> {
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
    let query = format!(
        r#"{{ AgentMessage(filter: {{ {scope} }}, order: {{ sequence: ASC }}) {{ {fields} }} }}"#,
        scope = scope,
        fields = crate::session::canonical_rows::AGENT_MESSAGE_FIELDS,
    );

    let resp = access.execute(&query).await?;
    if resp
        .get("errors")
        .is_some_and(|errors| !errors.is_null() && !errors.as_array().is_some_and(Vec::is_empty))
    {
        anyhow::bail!("loading scoped canonical headers for session_id={session_id}: {resp}");
    }
    let rows: Vec<serde_json::Value> = resp
        .get("data")
        .and_then(|data| data.get("AgentMessage"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .context("canonical header query omitted AgentMessage rows")?;
    rows.iter()
        .map(crate::session::canonical_rows::decode_transcript_message_row)
        .collect()
}

/// The exact AgentToolCall row addressed by its physical `_docID` within its
/// agent/requester/session scope. Absence and ambiguity are errors: the
/// caller addresses one lifecycle document, never a logical lookup.
#[derive(Debug, Deserialize)]
struct ToolCallIdentityRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    agent_did: String,
    requester_did: Option<String>,
    session_id: String,
    tool_call_id: String,
    tool_name: String,
    message_sequence: u32,
    request_doc_id: Option<String>,
    spawned_by_tool_call_doc_id: Option<String>,
}

async fn load_tool_call_identity(
    access: &ConfigAccess,
    tool_call_doc_id: &str,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<ToolCallIdentityRow> {
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
    let escaped_doc_id = escape_graphql_string(tool_call_doc_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ {scope}, _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 2
            ) {{
                _docID agent_did requester_did session_id tool_call_id tool_name message_sequence request_doc_id spawned_by_tool_call_doc_id
            }}
        }}"#,
        scope = scope,
    );

    let resp = access.execute(&query).await?;
    if resp
        .get("errors")
        .is_some_and(|errors| !errors.is_null() && !errors.as_array().is_some_and(Vec::is_empty))
    {
        anyhow::bail!("loading AgentToolCall tool_call_doc_id={tool_call_doc_id}: {resp}");
    }
    let rows: Vec<ToolCallIdentityRow> = serde_json::from_value(
        resp.get("data")
            .and_then(|data| data.get("AgentToolCall"))
            .cloned()
            .unwrap_or_default(),
    )
    .map_err(|error| anyhow!("AgentToolCall query omitted readable rows: {error}"))?;
    anyhow::ensure!(
        rows.len() <= 1,
        "ambiguous AgentToolCall document tool_call_doc_id={tool_call_doc_id}"
    );
    let row = rows.into_iter().next().ok_or_else(|| {
        anyhow!(
            "no AgentToolCall document tool_call_doc_id={tool_call_doc_id} within \
             agent/requester/session scope (session_id={session_id})"
        )
    })?;
    anyhow::ensure!(
        row.doc_id == tool_call_doc_id
            && row.agent_did == agent_did
            && row.requester_did.as_deref() == requester_did
            && row.session_id == session_id,
        "AgentToolCall {tool_call_doc_id} belongs to session {}",
        row.session_id
    );
    anyhow::ensure!(
        !row.tool_call_id.trim().is_empty(),
        "AgentToolCall {tool_call_doc_id} is missing its native tool_call_id"
    );
    Ok(row)
}

/// Deterministic text rendering of a delivered tool result: the native
/// tool-result message's text parts joined with newlines. A media-bearing or
/// non-tool-result message cannot be narrowed to text and is an explicit error.
pub fn render_tool_result(message: &Message) -> Result<String> {
    let Message::User { content } = message else {
        anyhow::bail!("tool result delivery is not a user message");
    };
    let mut texts = Vec::new();
    for item in content {
        let UserContent::ToolResult(tool_result) = item else {
            anyhow::bail!("tool result delivery contains non-tool-result content");
        };
        for part in &tool_result.content {
            let ToolResultContent::Text(text) = part else {
                anyhow::bail!("cannot narrow a media tool result to text");
            };
            texts.push(text.text.as_str());
        }
    }
    Ok(texts.join("\n"))
}

/// A published result must name this call's exact native id and match the
/// delivery header's own call-id binding; the header's physical
/// `tool_call_doc_id` already bound it to the lifecycle document.
fn verify_exact_call_identity(
    result: &Message,
    native_call_id: &str,
    expected_call_id: Option<&str>,
    doc_id: &str,
) -> Result<()> {
    let Message::User { content } = result else {
        anyhow::bail!("tool result delivery for {doc_id} is not a user message");
    };
    let [UserContent::ToolResult(tool_result)] = content.as_slice() else {
        anyhow::bail!("tool result delivery for {doc_id} is not a single tool result");
    };
    anyhow::ensure!(
        tool_result.id == native_call_id,
        "tool result for tool_call_doc_id={doc_id} names native id {} but the call is {native_call_id}",
        tool_result.id
    );
    anyhow::ensure!(
        tool_result.call_id.as_deref() == expected_call_id,
        "tool result for tool_call_doc_id={doc_id} carries call_id {:?} but the delivery header bound {expected_call_id:?}",
        tool_result.call_id
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    /// One canonical header, as it travels over the GraphQL boundary, plus the
    /// physical `_docID` the reader addresses it by.
    fn header_row(
        doc_id: &str,
        message_key: &str,
        publication: serde_json::Value,
        blocks: serde_json::Value,
        request_doc_id: Option<&str>,
    ) -> serde_json::Value {
        json!({
            "message_key": message_key,
            "session_id": "session-1",
            "agent_did": "did:test:agent",
            "requester_did": "did:test:requester",
            "request_doc_id": request_doc_id,
            "publication": publication,
            "outcome": "complete",
            "sequence": 1,
            "role": "user",
            "blocks": blocks,
            "created_at": "2026-01-01T00:00:00Z",
            "_docID": doc_id,
        })
    }

    fn tool_result_blocks(doc_id: &str, id: &str) -> serde_json::Value {
        json!([{ "type": "tool_result", "tool_call_doc_id": doc_id, "id": id,
            "parts": [{ "type": "text", "text": {
                "output": { "close_doc_id": "close-1", "stream": 0 },
                "presentation": { "kind": "full" }
            } }],
        }])
    }

    fn delivery_publication(doc_id: &str) -> serde_json::Value {
        json!({ "kind": "tool_delivery", "tool_call_doc_id": doc_id })
    }

    fn text_blocks() -> serde_json::Value {
        json!([{ "type": "text", "text": {
            "output": { "close_doc_id": "close-1", "stream": 0 },
            "presentation": { "kind": "full" }
        } }])
    }

    #[test]
    fn delivery_publication_with_native_tool_result_is_an_invocation_reply() {
        let decoded = crate::session::canonical_rows::decode_transcript_message_row(&header_row(
            "doc-1",
            "delivery-key",
            delivery_publication("doc-1"),
            tool_result_blocks("doc-1", "call-1"),
            Some("request-doc-1"),
        ))
        .expect("decoded header");
        assert!(is_invocation_reply(&decoded.message, "doc-1"));
    }

    #[test]
    fn delivery_publication_with_ordinary_text_is_not_an_invocation_reply() {
        // The background ordinary completion notification shares the
        // ToolDelivery publication but carries ordinary text blocks; it is a
        // distinct delivery and must never satisfy the invocation reply.
        let decoded = crate::session::canonical_rows::decode_transcript_message_row(&header_row(
            "doc-2",
            "notification-key",
            delivery_publication("doc-1"),
            text_blocks(),
            Some("request-doc-1"),
        ))
        .expect("decoded header");
        assert!(!is_invocation_reply(&decoded.message, "doc-1"));
    }

    #[test]
    fn delivery_publication_for_another_document_is_not_an_invocation_reply() {
        let decoded = crate::session::canonical_rows::decode_transcript_message_row(&header_row(
            "doc-3",
            "other-call-key",
            delivery_publication("doc-other"),
            tool_result_blocks("doc-other", "call-1"),
            Some("request-doc-1"),
        ))
        .expect("decoded header");
        assert!(!is_invocation_reply(&decoded.message, "doc-1"));
    }

    #[test]
    fn non_delivery_publications_are_never_invocation_replies() {
        for publication in [
            json!({ "kind": "request_execution", "execution_generation": "g1" }),
            json!({ "kind": "fork", "origin_message_doc_id": "origin" }),
        ] {
            let decoded =
                crate::session::canonical_rows::decode_transcript_message_row(&header_row(
                    "doc-4",
                    "other-key",
                    publication,
                    tool_result_blocks("doc-4", "call-1"),
                    Some("request-doc-1"),
                ))
                .expect("decoded header");
            assert!(!is_invocation_reply(&decoded.message, "doc-4"));
        }
    }

    #[test]
    fn request_document_mismatch_binds_the_reply_to_the_call() {
        // Exercise the same binding predicate as the actual reader, not merely
        // successful decoding of a header with a mismatched request.
        let decoded = crate::session::canonical_rows::decode_transcript_message_row(&header_row(
            "doc-5",
            "delivery-key-5",
            delivery_publication("doc-5"),
            tool_result_blocks("doc-5", "call-1"),
            Some("request-doc-OTHER"),
        ))
        .expect("decoded header");
        assert!(is_invocation_reply(&decoded.message, "doc-5"));
        assert!(!crate::lifecycle::is_exact_invocation_reply(
            &decoded.message,
            "request-doc-1",
            "session-1",
            "doc-5",
            "call-1",
            &None,
        ));
    }
}
