//! Native canonical evidence adapter for the loop's source-preserving input.

use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents_loop::claude_messages_body::reasoning_witness;
use gents_loop::loop_stream::{LoopReplayInput, TaggedMessage};

pub(crate) fn has_signed_reasoning(message: &gents_protocol::message::Message) -> bool {
    let gents_protocol::message::Message::Assistant { content, .. } = message else {
        return false;
    };
    !reasoning_witness(content).is_empty()
}

/// Resolve live and restored continuation evidence through physical canonical
/// facts. The configured backend and opaque signature bytes confer no origin.
pub(crate) fn owned_replay_input(
    node: Arc<EmbeddedNode>,
    request: crate::watcher::AgentRequest,
    request_commit_cid: String,
    expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
) -> LoopReplayInput {
    LoopReplayInput {
        request_doc_id: Some(request.doc_id.clone()),
        resolve: Some(Arc::new(move |tags, projection| {
            let node = node.clone();
            let request = request.clone();
            let request_commit_cid = request_commit_cid.clone();
            Box::pin(async move {
                let boundary = crate::provider_context_reduction::capture_source_boundary(
                    &node,
                    &request.session_id,
                    &request.agent_did,
                    request.requester_did.as_deref(),
                    &request.doc_id,
                    &request_commit_cid,
                )
                .await?;
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
                    &projection,
                )
                .await?;
                Ok(resolved
                    .into_iter()
                    .flat_map(|(tag, evidence)| {
                        evidence.into_iter().map(move |evidence| {
                            gents_loop::loop_stream::ReplayEvidenceRow {
                                tag: tag.clone(),
                                evidence,
                            }
                        })
                    })
                    .collect())
            })
        })),
        ..LoopReplayInput::default()
    }
}

/// Seed requirements before any lossy provider transform. Equal message bytes
/// and provider-assigned IDs never participate in the association.
pub(crate) fn tag_canonical_history(
    rows: &[crate::session::SequencedMessage],
    replay: &mut LoopReplayInput,
    profile: super::ProviderInputProfile,
) -> Vec<TaggedMessage> {
    rows.iter()
        .map(|row| {
            let source = row.provider_source.clone();
            if let Some(tag) = &source {
                if profile == super::ProviderInputProfile::ClaudeMessages
                    && has_signed_reasoning(&row.message)
                    && !replay.retired.contains(tag)
                    && !replay.candidates.contains(tag)
                {
                    replay.candidates.push(tag.clone());
                }
            }
            TaggedMessage {
                message: row.message.clone(),
                source,
                physical_header: row.canonical_header_doc_id.clone(),
                block_indices: row.block_indices.clone(),
            }
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
    fn current_coordinates_survive_equal_provider_ids_and_old_request_is_historical() {
        let first = provider_tag("current-request", 1);
        let second = provider_tag("current-request", 2);
        let old = provider_tag("older-request", 1);
        let rows = [
            assistant_row(3, old, true, true),
            assistant_row(4, first.clone(), true, true),
            assistant_row(5, second.clone(), true, true),
        ];
        let mut replay = LoopReplayInput {
            request_doc_id: Some("current-request".to_string()),
            ..LoopReplayInput::default()
        };

        let tagged = tag_canonical_history(
            &rows,
            &mut replay,
            super::super::ProviderInputProfile::ClaudeMessages,
        );

        assert_eq!(tagged.len(), 3);
        assert_eq!(tagged[0].source, None);
        assert_eq!(tagged[1].source, Some(first.clone()));
        assert_eq!(tagged[2].source, Some(second.clone()));
        assert_eq!(tagged[0].message, rows[0].message);
        assert_eq!(tagged[1].message, rows[1].message);
        assert_eq!(tagged[2].message, rows[2].message);
        assert_eq!(replay.candidates, vec![first, second]);
        assert!(replay.required.is_empty());
    }

    #[test]
    fn current_reasoning_rows_seed_provisional_sources_before_origin_check() {
        let tag = provider_tag("current-request", 1);
        let tool_turn = assistant_row(1, tag.clone(), true, true);
        let reasoning_final = assistant_row(2, provider_tag("current-request", 2), true, false);
        let mut replay = LoopReplayInput {
            request_doc_id: Some("current-request".to_string()),
            ..LoopReplayInput::default()
        };
        let tagged = tag_canonical_history(
            &[tool_turn.clone(), reasoning_final],
            &mut replay,
            super::super::ProviderInputProfile::ClaudeMessages,
        );
        assert_eq!(
            replay.candidates,
            vec![tag.clone(), provider_tag("current-request", 2)]
        );
        assert!(replay.required.is_empty());
        assert_eq!(tagged[0].source, Some(tag));
        assert_eq!(tagged[1].source, Some(provider_tag("current-request", 2)));
        // Source coordinates survive provider projection; canonical origin
        // selection decides which of them become required replay.
        let _projected = gents_loop::loop_stream::provider_view_tagged(
            super::super::ProviderInputProfile::ClaudeMessages,
            tagged,
        )
        .expect("canonical rows can be projected");
        assert_eq!(
            replay.candidates,
            vec![
                provider_tag("current-request", 1),
                provider_tag("current-request", 2)
            ]
        );

        let mut non_claude_replay = LoopReplayInput {
            request_doc_id: Some("current-request".to_string()),
            ..LoopReplayInput::default()
        };
        let non_claude = tag_canonical_history(
            &[tool_turn],
            &mut non_claude_replay,
            super::super::ProviderInputProfile::OpenAiChatCompletions,
        );
        assert!(non_claude_replay.required.is_empty());
        assert!(non_claude_replay.candidates.is_empty());
        assert_eq!(
            non_claude[0].source,
            Some(provider_tag("current-request", 1))
        );
    }
}
