//! Canonical invocation-reply reads, keyed by physical tool identity.

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::llm::message::{AssistantContent, Message, ToolResultContent, UserContent};
use anyhow::{anyhow, Context, Result};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSource, PresentedPayload,
    StreamPayload, ToolResultPart, TranscriptMessage,
};
use serde::Deserialize;
use serde_json::Value;

use super::ToolCallState;

#[derive(Clone, Copy)]
enum ReadSource<'a, 'txn> {
    Access(&'a ConfigAccess),
    Txn(&'a ConfigApplyTxn<'txn>),
}

impl ReadSource<'_, '_> {
    async fn execute(self, query: &str) -> Result<Value> {
        match self {
            Self::Access(access) => access.execute(query).await,
            Self::Txn(txn) => txn.execute(query).await,
        }
    }

    async fn canonical_payload(
        self,
        request_doc_id: &str,
        node_did: &str,
        requester_did: Option<&str>,
        payload: &PresentedPayload,
        tool_call_doc_id: &str,
    ) -> Result<gents_protocol::output::reconstruction::ReconstructedStream> {
        let expected_source = OutputSource::ToolCall {
            tool_call_doc_id: tool_call_doc_id.to_owned(),
        };
        anyhow::ensure!(
            payload.output.stream == 0,
            "plugin terminal output must select stream zero"
        );
        match self {
            Self::Access(access) => {
                crate::session::load_canonical_payload_with_access(
                    access,
                    request_doc_id,
                    node_did,
                    requester_did,
                    &payload.output,
                    &expected_source,
                )
                .await
            }
            Self::Txn(txn) => {
                crate::session::load_canonical_payload_in_txn(
                    txn,
                    request_doc_id,
                    node_did,
                    requester_did,
                    &payload.output,
                    &expected_source,
                )
                .await
            }
        }
    }

    async fn canonical_message(
        self,
        header_doc_id: &str,
        node_did: &str,
        requester_did: Option<&str>,
    ) -> Result<(TranscriptMessage, Message)> {
        match self {
            Self::Access(access) => {
                crate::session::load_canonical_message(
                    access,
                    header_doc_id,
                    node_did,
                    requester_did,
                )
                .await
            }
            Self::Txn(txn) => {
                crate::session::load_canonical_message_in_txn(
                    txn,
                    header_doc_id,
                    node_did,
                    requester_did,
                )
                .await
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct CanonicalToolCallRead {
    pub(crate) request_doc_id: String,
    pub(crate) tool_name: String,
    pub(crate) lifecycle_state: ToolCallState,
    pub(crate) arguments: String,
    pub(crate) result: Option<Message>,
    /// Exact persisted ToolOutput bytes behind the verified invocation reply.
    /// Direct invocation replies populate this only in transaction reads;
    /// plugin effects expose their canonical source to both read paths.
    pub(crate) raw_result: Option<String>,
}

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
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Message> {
    load_tool_call_read(
        access,
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await?
    .result
    .ok_or_else(|| {
        anyhow!(
            "no invocation reply delivered for tool_call_doc_id={tool_call_doc_id}; the tool result has not been delivered"
        )
    })
}

pub(crate) async fn load_tool_call_read(
    access: &ConfigAccess,
    tool_call_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<CanonicalToolCallRead> {
    load_tool_call_read_source(
        ReadSource::Access(access),
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await
}

pub(crate) async fn load_tool_call_arguments_in_txn(
    txn: &ConfigApplyTxn<'_>,
    tool_call_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<String> {
    let (_, _, _, arguments) = load_accepted_tool_call(
        ReadSource::Txn(txn),
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await?;
    Ok(arguments)
}

pub(crate) async fn load_tool_call_read_in_txn(
    txn: &ConfigApplyTxn<'_>,
    tool_call_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<CanonicalToolCallRead> {
    load_tool_call_read_source(
        ReadSource::Txn(txn),
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await
}

async fn load_tool_call_read_source(
    source: ReadSource<'_, '_>,
    tool_call_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<CanonicalToolCallRead> {
    let (call, headers, accepted_call_id, accepted_arguments) = load_accepted_tool_call(
        source,
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await?;
    let lifecycle_state =
        ToolCallState::from_persisted(&call.lifecycle_state).ok_or_else(|| {
            anyhow!(
                "AgentToolCall tool_call_doc_id={tool_call_doc_id} has invalid lifecycle state {}",
                call.lifecycle_state
            )
        })?;
    if call.plugin_parent_tool_call_doc_id.is_some() {
        anyhow::ensure!(headers.iter().all(|row| !matches!(
            &row.message.publication,
            MessagePublication::ToolDelivery { tool_call_doc_id: named } if named == tool_call_doc_id
        )), "plugin effect must not publish a transcript invocation reply");
        let request_doc_id = call
            .request_doc_id
            .clone()
            .context("plugin effect lacks request identity")?;
        let (result, raw_result) = if let Some(payload) = &call.terminal_output {
            anyhow::ensure!(
                lifecycle_state.is_terminal(),
                "nonterminal plugin effect carries terminal output"
            );
            let output = source
                .canonical_payload(
                    &request_doc_id,
                    node_did,
                    requester_did,
                    payload,
                    tool_call_doc_id,
                )
                .await?;
            anyhow::ensure!(
                matches!(output.declaration.payload, StreamPayload::ToolOutput),
                "plugin terminal output selects a non-tool-output stream"
            );
            let presented =
                super::super::delivery::render_presentation(&output.text, &payload.presentation)?;
            (
                Some(Message::User {
                    content: vec![UserContent::tool_result(
                        call.tool_call_id.clone(),
                        vec![ToolResultContent::text(presented)],
                    )],
                }),
                Some(output.text),
            )
        } else {
            anyhow::ensure!(
                !lifecycle_state.is_terminal()
                    || (lifecycle_state == ToolCallState::Cancelled && call.started_at.is_none()),
                "terminal plugin effect is missing its canonical presentation"
            );
            (None, None)
        };
        return Ok(CanonicalToolCallRead {
            request_doc_id,
            tool_name: call.tool_name,
            lifecycle_state,
            arguments: accepted_arguments,
            result,
            raw_result,
        });
    }
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
        return Ok(CanonicalToolCallRead {
            request_doc_id: expected_request_doc_id,
            tool_name: call.tool_name,
            lifecycle_state,
            arguments: accepted_arguments,
            result: None,
            raw_result: None,
        });
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
        header.node_did == node_did
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
        parts,
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

    let (_header, message) = source
        .canonical_message(&row.doc_id, node_did, requester_did)
        .await?;
    verify_exact_call_identity(
        &message,
        &native_call_id,
        block_call_id.as_deref(),
        tool_call_doc_id,
    )?;
    let raw_result = if let (ReadSource::Txn(txn), [ToolResultPart::Text { text }]) =
        (source, parts.as_slice())
    {
        let stream = crate::session::load_canonical_payload_in_txn(
            txn,
            &expected_request_doc_id,
            node_did,
            requester_did,
            &text.output,
            &OutputSource::ToolCall {
                tool_call_doc_id: tool_call_doc_id.to_owned(),
            },
        )
        .await?;
        anyhow::ensure!(
            matches!(stream.declaration.payload, StreamPayload::ToolOutput),
            "invocation reply for tool_call_doc_id={tool_call_doc_id} references another output source"
        );
        Some(stream.text)
    } else {
        None
    };
    Ok(CanonicalToolCallRead {
        request_doc_id: expected_request_doc_id,
        tool_name: call.tool_name,
        lifecycle_state,
        arguments: accepted_arguments,
        result: Some(message),
        raw_result,
    })
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
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<CanonicalToolCallPresentation> {
    let read = load_tool_call_read(
        access,
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await?;
    let mut result = read.result.as_ref().map(render_tool_result).transpose()?;
    let mut live_output = None;
    let call = load_tool_call_identity(
        ReadSource::Access(access),
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await?;
    if call.spawned_by_tool_call_doc_id.is_some() || call.plugin_parent_tool_call_doc_id.is_some() {
        anyhow::ensure!(
            call.plugin_parent_tool_call_doc_id.is_some() || result.is_none(),
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
            node_did,
            requester_did,
        )
        .await?;
        match output {
            crate::background_tools::CanonicalToolOutputObservation::Closed(output) => {
                if call.plugin_parent_tool_call_doc_id.is_none() {
                    result = Some(output);
                }
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
        arguments: read.arguments,
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
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<String> {
    let read = load_tool_call_read(
        access,
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await?;
    Ok(read.arguments)
}

async fn load_accepted_tool_call(
    source: ReadSource<'_, '_>,
    tool_call_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<(
    ToolCallIdentityRow,
    Vec<crate::session::canonical_rows::TranscriptMessageRow>,
    Option<String>,
    String,
)> {
    let call = load_tool_call_identity(
        source,
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await?;
    anyhow::ensure!(
        call.plugin_parent_tool_call_doc_id.is_some() == call.plugin_effect_ordinal.is_some(),
        "plugin effect provenance requires both parent and ordinal"
    );
    anyhow::ensure!(
        call.plugin_parent_tool_call_doc_id.is_none() || call.spawned_by_tool_call_doc_id.is_none(),
        "plugin effect cannot also be a spawned process"
    );
    let parent = if let Some(parent_doc_id) = call.plugin_parent_tool_call_doc_id.as_deref() {
        let ordinal = call
            .plugin_effect_ordinal
            .context("plugin effect lacks ordinal")?;
        let parent =
            load_tool_call_identity(source, parent_doc_id, node_did, session_id, requester_did)
                .await?;
        let child_deadline = chrono::DateTime::parse_from_rfc3339(
            call.deadline_at
                .as_deref()
                .context("plugin child has no deadline")?,
        )
        .context("plugin child deadline is invalid")?;
        let parent_deadline = chrono::DateTime::parse_from_rfc3339(
            parent
                .deadline_at
                .as_deref()
                .context("plugin parent has no deadline")?,
        )
        .context("plugin parent deadline is invalid")?;
        anyhow::ensure!(
            (1..=64).contains(&ordinal)
                && child_deadline <= parent_deadline
                && call.await_mode.as_deref() == Some("foreground")
                && call.tool_call_id == format!("plugin-effect:{parent_doc_id}:{ordinal}")
                && call.tool_call_key.as_deref() == Some(call.tool_call_id.as_str())
                && call.request_doc_id == parent.request_doc_id
                && call.message_sequence == parent.message_sequence
                && parent.spawned_by_tool_call_doc_id.is_none()
                && parent.plugin_parent_tool_call_doc_id.is_none()
                && parent.plugin_effect_ordinal.is_none(),
            "plugin effect presentation has incoherent direct-parent provenance"
        );
        Some(parent)
    } else if let Some(parent_doc_id) = call.spawned_by_tool_call_doc_id.as_deref() {
        let parent =
            load_tool_call_identity(source, parent_doc_id, node_did, session_id, requester_did)
                .await?;
        anyhow::ensure!(
            call.request_doc_id == parent.request_doc_id
                && call.message_sequence == parent.message_sequence
                && parent.tool_name == crate::toolset::SPAWN_PROCESS_TOOL_NAME,
            "spawned tool presentation has incoherent accepted-parent provenance"
        );
        Some(parent)
    } else {
        None
    };
    let admission = parent.as_ref().unwrap_or(&call);
    let expected_request_doc_id = call.request_doc_id.as_deref().ok_or_else(|| {
        anyhow!("AgentToolCall tool_call_doc_id={tool_call_doc_id} carries no request document")
    })?;
    let headers = scoped_canonical_headers(source, node_did, session_id, requester_did).await?;
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
    let (_, accepted_message) = source
        .canonical_message(&accepted[0].doc_id, node_did, requester_did)
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
    let arguments = if call.plugin_parent_tool_call_doc_id.is_some() {
        anyhow::ensure!(
            accepted[0].message.outcome == OutputOutcome::Complete,
            "plugin effect requires a complete parent admission"
        );
        let generation = match &accepted[0].message.publication {
            MessagePublication::RequestExecution {
                execution_generation,
            } => execution_generation,
            _ => unreachable!("accepted header requires request execution"),
        };
        let response = source
            .execute(
                &crate::session::canonical_rows::request_output_segments_query(
                    expected_request_doc_id,
                ),
            )
            .await?;
        let rows = response["data"]["AgentOutputSegment"]
            .as_array()
            .context("plugin effect arguments query omitted segments")?;
        let (_, arguments) = super::super::plugin_effect::arguments_from_rows(
            rows,
            node_did,
            session_id,
            requester_did,
            expected_request_doc_id,
            &call.doc_id,
            generation,
            &call.tool_call_id,
            &call.tool_name,
        )?;
        arguments
    } else if call.spawned_by_tool_call_doc_id.is_some() {
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
    if call.spawned_by_tool_call_doc_id.is_some() || call.plugin_parent_tool_call_doc_id.is_some() {
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
    source: ReadSource<'_, '_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<crate::session::canonical_rows::TranscriptMessageRow>> {
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let query = format!(
        r#"{{ AgentMessage(filter: {{ {scope} }}, order: {{ sequence: ASC }}) {{ {fields} }} }}"#,
        scope = scope,
        fields = crate::session::canonical_rows::AGENT_MESSAGE_FIELDS,
    );

    let resp = source.execute(&query).await?;
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
    node_did: String,
    requester_did: Option<String>,
    session_id: String,
    tool_call_id: String,
    #[serde(default)]
    tool_call_key: Option<String>,
    tool_name: String,
    message_sequence: u32,
    request_doc_id: Option<String>,
    spawned_by_tool_call_doc_id: Option<String>,
    #[serde(default)]
    plugin_parent_tool_call_doc_id: Option<String>,
    #[serde(default)]
    plugin_effect_ordinal: Option<u32>,
    #[serde(default)]
    deadline_at: Option<String>,
    #[serde(default)]
    await_mode: Option<String>,
    #[serde(default)]
    terminal_output: Option<PresentedPayload>,
    #[serde(default)]
    started_at: Option<String>,
    lifecycle_state: String,
}

async fn load_tool_call_identity(
    source: ReadSource<'_, '_>,
    tool_call_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<ToolCallIdentityRow> {
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let escaped_doc_id = escape_graphql_string(tool_call_doc_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ {scope}, _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 2
            ) {{
                _docID node_did requester_did session_id tool_call_id tool_call_key tool_name message_sequence request_doc_id spawned_by_tool_call_doc_id plugin_parent_tool_call_doc_id plugin_effect_ordinal deadline_at await_mode terminal_output started_at lifecycle_state
            }}
        }}"#,
        scope = scope,
    );

    let resp = source.execute(&query).await?;
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
            && row.node_did == node_did
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
            "node_did": "did:test:agent",
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

    #[tokio::test]
    async fn generated_plugin_child_terminal_presentation_matches_execution_owner() {
        use gents_protocol::output::{PayloadPresentation, PresentationPart};
        let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
        let cases = snapshot.plugin_resource_cases["tool_effect_terminals"]
            .as_array()
            .expect("generated plugin terminal cases");
        for (index, case) in cases.iter().enumerate() {
            let (fixture, request_owner) =
                crate::tool_call_lifecycle::admission_fixture::published_admission_with_owner(
                    crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                        name: (&format!("effect-terminal-{index}")).to_owned(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let crate::tool_call_lifecycle::admission_fixture::PublishedAdmission {
                node,
                path,
                tool: parent,
                ..
            } = fixture;
            let mut child = parent
                .admit_plugin_effect(1, "fixture_effect", "{}")
                .await
                .unwrap();
            child.start_running().await.unwrap();
            let presentation = PayloadPresentation::Composed {
                parts: vec![
                    PresentationPart::Literal {
                        text: "error: ".into(),
                    },
                    PresentationPart::OutputRange {
                        start_byte: 1,
                        end_byte: 3,
                    },
                ],
            };
            let mut replay = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
                node.clone(),
                child.doc_id().unwrap(),
                parent.node_did(),
                parent.session_id(),
                None,
            )
            .await
            .unwrap()
            .unwrap();
            child
                .complete_raw_with_presentation("abc", "error: bc", presentation.clone())
                .await
                .unwrap();
            let accepted = if case["replay"].as_bool().unwrap() {
                let conflict = case["conflict"].as_bool().unwrap();
                replay
                    .complete_raw_with_presentation(
                        "abc",
                        if conflict { "abc" } else { "error: bc" },
                        if conflict {
                            PayloadPresentation::Full
                        } else {
                            presentation
                        },
                    )
                    .await
                    .is_ok()
            } else {
                load_tool_call_presentation(
                    &ConfigAccess::Local(node.clone()),
                    child.doc_id().unwrap(),
                    parent.node_did(),
                    parent.session_id(),
                    None,
                )
                .await
                .unwrap()
                .result
                .as_deref()
                    == Some("error: bc")
            };
            assert_eq!(
                accepted,
                case["expected"].as_bool().unwrap(),
                "case {index}"
            );
            drop(request_owner);
            node.shutdown().await;
            let _ = std::fs::remove_dir_all(path);
        }
    }

    #[tokio::test]
    async fn plugin_child_reads_canonical_sources_after_parent_terminal_without_transcript() {
        let (fixture, request_owner) =
            crate::tool_call_lifecycle::admission_fixture::published_admission_with_owner(
                crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                    name: "plugin-child-reader".to_owned(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let crate::tool_call_lifecycle::admission_fixture::PublishedAdmission {
            node,
            path,
            tool: mut parent,
            ..
        } = fixture;
        let arguments = r#"{"value":"input"}"#;
        let mut child = parent
            .admit_plugin_effect(1, "fixture_effect", arguments)
            .await
            .unwrap();
        let child_doc = child.doc_id().unwrap().to_owned();
        let node_did = parent.node_did().to_owned();
        let session_id = parent.session_id().to_owned();
        let access = ConfigAccess::Local(node.clone());
        assert_eq!(
            load_tool_call_arguments(&access, &child_doc, &node_did, &session_id, None)
                .await
                .unwrap(),
            arguments
        );
        child.start_running().await.unwrap();
        let mut replay = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &child_doc,
            &node_did,
            &session_id,
            None,
        )
        .await
        .unwrap()
        .unwrap();
        child
            .complete_raw_with_presentation(
                "effect result",
                "[bounded effect]",
                gents_protocol::output::PayloadPresentation::Composed {
                    parts: vec![gents_protocol::output::PresentationPart::Literal {
                        text: "[bounded effect]".into(),
                    }],
                },
            )
            .await
            .unwrap();
        assert!(!replay
            .complete_raw_with_presentation(
                "effect result",
                "[bounded effect]",
                gents_protocol::output::PayloadPresentation::Composed {
                    parts: vec![gents_protocol::output::PresentationPart::Literal {
                        text: "[bounded effect]".into()
                    }]
                },
            )
            .await
            .unwrap());
        replay.set_state(ToolCallState::Running);
        assert!(replay
            .complete_raw_with_presentation(
                "effect result",
                "[changed presentation]",
                gents_protocol::output::PayloadPresentation::Composed {
                    parts: vec![gents_protocol::output::PresentationPart::Literal {
                        text: "[changed presentation]".into()
                    }]
                },
            )
            .await
            .is_err());
        parent.complete("parent result").await.unwrap();
        let presentation =
            load_tool_call_presentation(&access, &child_doc, &node_did, &session_id, None)
                .await
                .unwrap();
        assert_eq!(presentation.arguments, arguments);
        assert_eq!(presentation.result.as_deref(), Some("[bounded effect]"));
        let read = ConfigAccess::transact_local(&node, None, "test.plugin_child_read", |txn| {
            Box::pin(load_tool_call_read_in_txn(
                txn,
                &child_doc,
                &node_did,
                &session_id,
                None,
            ))
        })
        .await
        .unwrap();
        assert_eq!(read.raw_result.as_deref(), Some("effect result"));
        assert_eq!(
            render_tool_result(read.result.as_ref().unwrap()).unwrap(),
            "[bounded effect]"
        );
        let headers =
            scoped_canonical_headers(ReadSource::Access(&access), &node_did, &session_id, None)
                .await
                .unwrap();
        assert!(headers.iter().all(|row| !matches!(&row.message.publication,
            MessagePublication::ToolDelivery { tool_call_doc_id } if tool_call_doc_id == &child_doc)));
        let foreign = PresentedPayload {
            output: child.arguments.clone().unwrap(),
            presentation: gents_protocol::output::PayloadPresentation::Full,
        };
        ConfigAccess::transact_local(&node, None, "test.forged_child_output_reference", |txn| {
            let foreign = foreign.clone();
            let doc = escape_graphql_string(&child_doc);
            Box::pin(async move {
                txn.execute_with_variables(&format!(
                    r#"mutation($output: JSON) {{ update_AgentToolCall(docID: "{doc}", input: {{ terminal_output: $output }}) {{ _docID }} }}"#
                ), &serde_json::json!({"output": foreign})).await?;
                Ok(())
            })
        }).await.unwrap();
        assert!(
            load_tool_call_result(&access, &child_doc, &node_did, &session_id, None)
                .await
                .is_err()
        );
        drop(request_owner);
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn plugin_child_timeout_preserves_diagnostic_presentation_and_raw_output() {
        let (fixture, request_owner) =
            crate::tool_call_lifecycle::admission_fixture::published_admission_with_owner(
                crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                    name: "plugin-child-timeout-reader".to_owned(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let crate::tool_call_lifecycle::admission_fixture::PublishedAdmission {
            node,
            path,
            tool: parent,
            ..
        } = fixture;
        let mut child = parent
            .admit_plugin_effect(1, "fixture_effect", "{}")
            .await
            .unwrap();
        child.start_running().await.unwrap();
        let binding = child.tool_output_binding().unwrap();
        crate::tool_call_lifecycle::delivery::append_tool_output(&binding, "partial output")
            .await
            .unwrap();
        child.timeout().await.unwrap();
        let child_doc = child.doc_id().unwrap().to_owned();
        let node_did = child.node_did().to_owned();
        let session_id = child.session_id().to_owned();
        let read = ConfigAccess::transact_local(&node, None, "test.plugin_child_timeout", |txn| {
            Box::pin(load_tool_call_read_in_txn(
                txn,
                &child_doc,
                &node_did,
                &session_id,
                None,
            ))
        })
        .await
        .unwrap();
        assert_eq!(read.raw_result.as_deref(), Some("partial output"));
        let presented = render_tool_result(read.result.as_ref().unwrap()).unwrap();
        assert!(presented.contains("partial output"));
        assert!(presented.contains("tool call deadline exceeded"));
        drop(request_owner);
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn plugin_child_reader_rejects_foreign_stable_identity_before_loading_arguments() {
        let (fixture, request_owner) =
            crate::tool_call_lifecycle::admission_fixture::published_admission_with_owner(
                crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                    name: "plugin-child-identity-reader".to_owned(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let crate::tool_call_lifecycle::admission_fixture::PublishedAdmission {
            node,
            path,
            tool: parent,
            ..
        } = fixture;
        let parent_doc = escape_graphql_string(parent.doc_id().unwrap());
        let request_doc = escape_graphql_string(parent.request_doc_id().unwrap());
        let session_id = parent.session_id().to_owned();
        let node_did = parent.node_did().to_owned();
        let access = ConfigAccess::Local(node.clone());
        let created = access.write("test.forged_plugin_child", &format!(
            r#"mutation {{ create_AgentToolCall(input: {{
                tool_call_key: "forged-effect", tool_call_id: "forged-effect", tool_name: "fixture_effect",
                request_doc_id: "{request_doc}", session_id: "{}", node_did: "{}",
                message_sequence: {}, lifecycle_state: "pending", plugin_parent_tool_call_doc_id: "{parent_doc}",
                plugin_effect_ordinal: 1, await_mode: "foreground", deadline_at: "{}"
            }}) {{ _docID }} }}"#,
            escape_graphql_string(&session_id), escape_graphql_string(&node_did), parent.message_sequence,
            escape_graphql_string(&parent.deadline_at.to_rfc3339()),
        )).await.unwrap();
        let doc_id = crate::graphql::created_doc_id(&created, "AgentToolCall").unwrap();
        let error = load_tool_call_arguments(&access, &doc_id, &node_did, &session_id, None)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("incoherent direct-parent provenance"),
            "{error:#}"
        );
        drop(request_owner);
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn transaction_reader_preserves_raw_beyond_bounded_reply_and_rejects_ambiguity() {
        use crate::tool_call_lifecycle::admission_fixture::published_spawn_parent;
        use gents_protocol::output::{PayloadPresentation, PresentationPart};

        let (node, path, mut tool) = published_spawn_parent("txn-result-reader").await;
        let raw = r#"{"ok":false,"error":{"reason":"wait_timeout"}}"#;
        let bounded = "[bounded presentation]";
        tool.complete_raw_with_presentation(
            raw,
            bounded,
            PayloadPresentation::Composed {
                parts: vec![PresentationPart::Literal {
                    text: bounded.to_owned(),
                }],
            },
        )
        .await
        .unwrap();
        let doc_id = tool.doc_id().unwrap().to_owned();
        let node_did = tool.node_did().to_owned();
        let session_id = tool.session_id().to_owned();
        let request_doc_id = tool.request_doc_id().unwrap().to_owned();
        let read =
            ConfigAccess::transact_local(node.as_ref(), None, "tool_result.txn_read", |txn| {
                Box::pin(load_tool_call_read_in_txn(
                    txn,
                    &doc_id,
                    &node_did,
                    &session_id,
                    None,
                ))
            })
            .await
            .unwrap();
        assert_eq!(read.request_doc_id, request_doc_id);
        assert_eq!(read.tool_name, crate::toolset::SPAWN_PROCESS_TOOL_NAME);
        assert_eq!(read.lifecycle_state, ToolCallState::Completed);
        assert!(!read.arguments.is_empty());
        assert_eq!(
            render_tool_result(read.result.as_ref().expect("canonical reply")).unwrap(),
            bounded
        );
        assert_eq!(read.raw_result.as_deref(), Some(raw));

        ConfigAccess::transact_local(node.as_ref(), None, "tool_result.ambiguous_reply", |txn| {
            Box::pin(async {
                let headers =
                    scoped_canonical_headers(ReadSource::Txn(txn), &node_did, &session_id, None)
                        .await?;
                let mut duplicate = headers
                    .iter()
                    .find(|row| is_invocation_reply(&row.message, &doc_id))
                    .expect("fixture completed invocation reply")
                    .message
                    .clone();
                duplicate.message_key.push_str(":duplicate");
                duplicate.sequence += 1;
                txn.execute_with_variables(
                    crate::session::canonical_rows::CREATE_AGENT_MESSAGE_MUTATION,
                    &crate::session::canonical_rows::transcript_message_create_variables(
                        &duplicate,
                    )?,
                )
                .await?;
                let error = load_tool_call_read_in_txn(txn, &doc_id, &node_did, &session_id, None)
                    .await
                    .unwrap_err();
                assert!(error.to_string().contains("ambiguous invocation reply"));
                Ok(())
            })
        })
        .await
        .unwrap();
        node.shutdown().await;
        std::fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn transaction_reader_rejects_tool_delivery_without_native_reply() {
        use crate::tool_call_lifecycle::admission_fixture::published_spawn_parent;

        let (node, path, tool) = published_spawn_parent("txn-malformed-delivery").await;
        let doc_id = tool.doc_id().unwrap().to_owned();
        let node_did = tool.node_did().to_owned();
        let session_id = tool.session_id().to_owned();
        ConfigAccess::transact_local(node.as_ref(), None, "tool_result.malformed_reply", |txn| {
            Box::pin(async {
                let headers =
                    scoped_canonical_headers(ReadSource::Txn(txn), &node_did, &session_id, None)
                        .await?;
                let mut malformed = headers
                    .iter()
                    .find(|row| row.message.role == MessageRole::Assistant)
                    .expect("fixture accepted assistant header")
                    .message
                    .clone();
                malformed.message_key.push_str(":malformed-delivery");
                malformed.sequence += 1;
                malformed.publication = MessagePublication::ToolDelivery {
                    tool_call_doc_id: doc_id.clone(),
                };
                txn.execute_with_variables(
                    crate::session::canonical_rows::CREATE_AGENT_MESSAGE_MUTATION,
                    &crate::session::canonical_rows::transcript_message_create_variables(
                        &malformed,
                    )?,
                )
                .await?;
                let error = load_tool_call_read_in_txn(txn, &doc_id, &node_did, &session_id, None)
                    .await
                    .unwrap_err();
                assert!(error.to_string().contains("carries no native tool-result"));
                Ok(())
            })
        })
        .await
        .unwrap();
        node.shutdown().await;
        std::fs::remove_dir_all(path).unwrap();
    }
}
