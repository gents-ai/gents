//! Native canonical evidence adapter for the loop's source-preserving input.

use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents_loop::loop_stream::{LoopReplayInput, TaggedMessage};

/// Resolve live and restored continuation evidence through physical canonical
/// facts. The configured backend and opaque signature bytes confer no origin.
pub(crate) fn owned_replay_input(
    node: Arc<EmbeddedNode>,
    request: crate::watcher::AgentRequest,
    request_commit_cid: String,
    expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
    issuer: Option<gents_loop::claude_messages_body::ReplayIssuer>,
    profile: super::ProviderInputProfile,
) -> LoopReplayInput {
    let pending = Arc::new(PendingReplay::default());
    LoopReplayInput {
        request_doc_id: Some(request.doc_id.clone()),
        issuer,
        wire: profile.replay_wire(),
        resolve: Some(Arc::new(move |tags| {
            let node = node.clone();
            let request = request.clone();
            let request_commit_cid = request_commit_cid.clone();
            let pending = pending.clone();
            Box::pin(async move {
                if let Some(rows) = pending.take(&tags) {
                    return Ok(rows);
                }
                let started = std::time::Instant::now();
                let boundary = crate::provider_context_reduction::capture_source_boundary(
                    &node,
                    &request.session_id,
                    &request.agent_did,
                    request.requester_did.as_deref(),
                    &request.doc_id,
                    &request_commit_cid,
                )
                .await?;
                let boundary_elapsed_ms = started.elapsed().as_millis() as u64;
                let resolved = crate::session::resolve_canonical_replay_tags(
                    &node,
                    crate::session::CanonicalReplayScope {
                        agent_did: &request.agent_did,
                        requester_did: request.requester_did.as_deref(),
                        session_id: &request.session_id,
                        request_id: &request.request_id,
                        request_doc_id: &request.doc_id,
                        request_commit_cid: &request_commit_cid,
                        expected_scope_kind,
                    },
                    &boundary,
                    &tags,
                )
                .await?;
                tracing::info!(
                    target: "gents::provider_input",
                    request_id = %request.request_id,
                    request_doc_id = %request.doc_id,
                    session_id = %request.session_id,
                    tag_count = tags.len(),
                    boundary_elapsed_ms,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "resolved canonical provider replay evidence"
                );
                let rows = resolved
                    .into_iter()
                    .flat_map(|(tag, evidence)| {
                        evidence.into_iter().map(move |evidence| {
                            gents_loop::loop_stream::ReplayEvidenceRow {
                                tag: tag.clone(),
                                evidence,
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                pending.store(tags, rows.clone());
                Ok(rows)
            })
        })),
        ..LoopReplayInput::default()
    }
}

/// The first-turn admission estimate and the loop's first dispatch assemble
/// the same request back to back. The last resolution serves the next
/// identical lookup once; any other lookup re-reads the store.
#[derive(Default)]
struct PendingReplay(
    std::sync::Mutex<
        Option<(
            Vec<gents_loop::claude_messages_body::ReplayTag>,
            Vec<gents_loop::loop_stream::ReplayEvidenceRow>,
        )>,
    >,
);

impl PendingReplay {
    fn take(
        &self,
        tags: &[gents_loop::claude_messages_body::ReplayTag],
    ) -> Option<Vec<gents_loop::loop_stream::ReplayEvidenceRow>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .and_then(|(previous, rows)| (previous == tags).then_some(rows))
    }

    fn store(
        &self,
        tags: Vec<gents_loop::claude_messages_body::ReplayTag>,
        rows: Vec<gents_loop::loop_stream::ReplayEvidenceRow>,
    ) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((tags, rows));
    }
}

/// Attach each canonical row's physical provenance before any lossy provider
/// transform. Equal message bytes and provider-assigned IDs never participate
/// in the association.
pub(crate) fn tag_canonical_history(
    rows: &[crate::session::SequencedMessage],
) -> Vec<TaggedMessage> {
    rows.iter()
        .map(|row| TaggedMessage {
            message: row.message.clone(),
            source: row.provider_source.clone(),
            physical_header: row.canonical_header_doc_id.clone(),
            block_indices: row.block_indices.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_loop::claude_messages_body::ReplayTag;
    use gents_protocol::message::{
        AssistantContent, Message, Reasoning, Text, ToolCall, ToolFunction,
    };
    use gents_protocol::output::OutputSource;
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    #[test]
    fn pending_replay_serves_one_identical_lookup() {
        let pending = PendingReplay::default();
        let tags = vec![provider_tag("doc-a", 0)];
        pending.store(tags.clone(), Vec::new());
        assert_eq!(pending.take(&tags).map(|rows| rows.len()), Some(0));
        assert!(pending.take(&tags).is_none(), "served once");

        pending.store(tags, Vec::new());
        assert!(pending.take(&[provider_tag("doc-a", 1)]).is_none());
        assert!(
            pending.take(&[provider_tag("doc-a", 0)]).is_none(),
            "a differing lookup discards the pending result"
        );
    }

    fn provider_tag(request_doc_id: &str, turn_index: u32) -> ReplayTag {
        ReplayTag {
            request_doc_id: request_doc_id.to_string(),
            source: OutputSource::ProviderTurn {
                scope: CaptureScope {
                    kind: CaptureScopeKind::Inference,
                    seq: 1,
                },
                turn_index,
                attempt: 0,
            },
        }
    }

    fn assistant_row(
        sequence: u32,
        tag: ReplayTag,
        with_reasoning: bool,
        with_tool: bool,
    ) -> crate::session::SequencedMessage {
        let mut content = vec![AssistantContent::Text(Text {
            text: "same visible text".to_string(),
        })];
        if with_reasoning {
            content.push(AssistantContent::Reasoning(Reasoning::new_with_signature(
                "same thinking",
                Some("signed".to_string()),
            )));
        }
        if with_tool {
            content.push(AssistantContent::ToolCall(ToolCall::new(
                "same tool call".to_string(),
                ToolFunction {
                    name: "echo".to_string(),
                    arguments: serde_json::json!({}),
                },
            )));
        }
        crate::session::SequencedMessage {
            provider_source: Some(tag),
            canonical_header_doc_id: Some(format!("header-{sequence}")),
            block_indices: (0..content.len()).collect(),
            sequence,
            message: Message::Assistant {
                id: Some("same-provider-message-id".to_string()),
                content,
            },
        }
    }

    #[test]
    fn every_physical_source_survives_equal_provider_ids_across_requests() {
        let old = provider_tag("older-request", 1);
        let first = provider_tag("current-request", 1);
        let second = provider_tag("current-request", 2);
        let rows = [
            assistant_row(3, old.clone(), true, true),
            assistant_row(4, first.clone(), true, true),
            assistant_row(5, second.clone(), true, false),
        ];

        let tagged = tag_canonical_history(&rows);

        assert_eq!(
            tagged
                .iter()
                .map(|row| row.source.clone())
                .collect::<Vec<_>>(),
            [Some(old), Some(first.clone()), Some(second)]
        );
        for (tagged, row) in tagged.iter().zip(&rows) {
            assert_eq!(tagged.message, row.message);
            assert_eq!(tagged.physical_header, row.canonical_header_doc_id);
            assert_eq!(tagged.block_indices, row.block_indices);
        }
        let projected = gents_loop::loop_stream::provider_view_tagged(tagged)
            .expect("canonical rows can be projected");
        assert_eq!(projected[1].source, Some(first));
    }
}
