use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::output::{MessagePublication, MessageRole, OutputOutcome, OutputSource};
use std::collections::BTreeSet;

use super::super::canonical_rows::{decode_transcript_message_row, AGENT_MESSAGE_FIELDS};
use super::super::query::session_scope_filter;
use super::super::SequencedMessage;
use super::access::{ReadAccess, ReadCache};
use super::reconstruct::{preload_request_output_segments, reconstruct_scoped_message_with_facts};
use super::replay_candidates::provider_coordinate_for_candidate;
use super::replay_violation;

/// The owned loop rebuilds context, prompt and folded messages from admission
/// input. Other publications belonging to that request (notably tool results
/// and background notifications) remain history. This binds PromptAssembly.CurrentInput;
/// request membership alone is not an input classification.
pub(super) fn is_current_admission_input(
    header: &gents_protocol::output::TranscriptMessage,
    request_doc_id: &str,
) -> bool {
    header.request_doc_id.as_deref() == Some(request_doc_id)
        && matches!(
            header.publication,
            MessagePublication::RequestExecution { .. }
        )
        && (["prompt", "context"].into_iter().any(|key| {
            header.message_key
                == super::super::canonical_rows::authored_message_key(request_doc_id, key)
        }) || header.message_key.starts_with(
            &super::super::canonical_rows::authored_message_key(
                request_doc_id,
                &crate::request::lifecycle::queue::folded_input_key(""),
            ),
        ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "independent transcript range filters"
)]
pub(in crate::session) async fn load_sequenced_messages(
    node: &EmbeddedNode,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
    after_sequence: Option<u32>,
    exclude_request_doc_id: Option<&str>,
    replay_profile: Option<crate::turn::provider_input::ProviderInputProfile>,
) -> Result<Vec<SequencedMessage>> {
    let session_scope = session_scope_filter(node_did, session_id, requester_did);
    let mut bounds = Vec::new();
    if let Some(sequence) = after_sequence {
        bounds.push(format!("_gt: {sequence}"));
    }
    if let Some(sequence) = through_sequence {
        bounds.push(format!("_le: {sequence}"));
    }
    let bounds = if !bounds.is_empty() {
        format!(", sequence: {{ {} }}", bounds.join(", "))
    } else {
        Default::default()
    };
    let query = format!(
        r#"{{
        AgentMessage(filter: {{ {session_scope}{bounds} }}, order: {{ sequence: ASC }}) {{ {AGENT_MESSAGE_FIELDS} }}
    }}"#
    );
    let response = crate::store::graphql::graphql_response_with_transaction_retry(
        node,
        &query,
        "load_canonical_history",
    )
    .await?;
    if response.has_errors() {
        anyhow::bail!(
            "loading canonical history for session_id={session_id}: {:?}",
            response.errors
        );
    }
    let data = response
        .data
        .as_ref()
        .context("canonical history query omitted data")?;
    let headers = data
        .get("AgentMessage")
        .and_then(serde_json::Value::as_array)
        .context("canonical history query omitted AgentMessage rows")?
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    let mut cache = ReadCache {
        reuse_node_scans: true,
        ..ReadCache::default()
    };
    let mut sequences = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for row in &headers {
        anyhow::ensure!(
            row.message.session_id == session_id
                && row.message.node_did == node_did
                && row.message.requester_did.as_deref() == requester_did,
            "canonical header crossed requested session scope"
        );
        anyhow::ensure!(
            sequences.insert(row.message.sequence),
            "ambiguous canonical session sequence {}",
            row.message.sequence
        );
        anyhow::ensure!(
            keys.insert(row.message.message_key.clone()),
            "ambiguous canonical session message key {}",
            row.message.message_key
        );
        cache.headers.insert(row.doc_id.clone(), row.clone());
    }
    let output_requests = headers
        .iter()
        .filter(|row| {
            exclude_request_doc_id.is_none_or(|id| !is_current_admission_input(&row.message, id))
                && !row.message.payload_references().is_empty()
        })
        .filter_map(|row| row.message.request_doc_id.clone())
        .collect::<Vec<_>>();
    preload_request_output_segments(node, &output_requests, node_did, requester_did, &mut cache)
        .await?;
    let mut messages = Vec::new();
    for row in headers.into_iter().filter(|row| {
        exclude_request_doc_id.is_none_or(|id| !is_current_admission_input(&row.message, id))
    }) {
        let sequence = row.message.sequence;
        let reconstructed = reconstruct_scoped_message_with_facts(
            ReadAccess::Node(node),
            &row.doc_id,
            node_did,
            requester_did,
            &mut cache,
        )
        .await
        .with_context(|| {
            format!(
                "reconstructing canonical message {} at sequence {sequence}",
                row.doc_id
            )
        })?;
        let provider_source = if replay_profile.is_some()
            && reconstructed.origin.message.role == MessageRole::Assistant
            && reconstructed.origin.message.outcome == OutputOutcome::Complete
            && matches!(
                reconstructed.origin.message.publication,
                MessagePublication::RequestExecution { .. }
            ) {
            match provider_coordinate_for_candidate(
                &reconstructed,
                gents_protocol::rendered_request::CaptureScopeKind::Inference,
            ) {
                Ok(Some(coordinate)) => Some(gents_loop::claude_messages_body::ReplayTag {
                    request_doc_id: reconstructed
                        .origin
                        .message
                        .request_doc_id
                        .clone()
                        .ok_or_else(|| {
                            replay_violation("provider coordinate requires request document")
                        })?,
                    source: OutputSource::ProviderTurn {
                        scope: coordinate.scope,
                        turn_index: coordinate.turn_index,
                        attempt: coordinate.attempt,
                    },
                }),
                Ok(None) => None,
                Err(error) => {
                    tracing::debug!(header_doc_id = %row.doc_id, error = %error,
                        "canonical assistant has no usable provider replay coordinate");
                    None
                }
            }
        } else {
            None
        };
        messages.push(SequencedMessage {
            provider_source,
            canonical_header_doc_id: Some(reconstructed.origin.doc_id),
            block_indices: match &reconstructed.message {
                gents_protocol::message::Message::Assistant { content, .. } => {
                    (0..content.len()).collect()
                }
                _ => Vec::new(),
            },
            sequence,
            message: reconstructed.message,
        });
    }
    Ok(messages)
}
