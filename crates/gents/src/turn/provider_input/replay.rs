use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_loop::loop_stream::{
    LoopReplayInput, ReplayEvidenceResolveError, ReplayEvidenceViolation, TaggedMessage,
};

pub(crate) fn owned_replay_input(
    node: Arc<EmbeddedNode>,
    request: gents_protocol::row::AgentRequest,
    request_commit_cid: String,
    expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
    issuer: Option<gents_loop::claude_messages_body::ReplayIssuer>,
    profile: super::ProviderInputProfile,
) -> LoopReplayInput {
    let estimate_seed = gents_loop::loop_stream::ReplayEstimateSeed::default();
    let recording_seed = estimate_seed.clone();
    LoopReplayInput {
        request_doc_id: Some(request.doc_id.clone()),
        issuer,
        wire: profile.replay_wire(),
        estimate_seed: Some(estimate_seed),
        resolve: Some(Arc::new(move |tags| {
            let node = node.clone();
            let request = request.clone();
            let request_commit_cid = request_commit_cid.clone();
            let recording_seed = recording_seed.clone();
            Box::pin(async move {
                let actor = identity::Did::new(request.agent_did.clone()).map_err(|error| {
                    ReplayEvidenceResolveError::Storage {
                        source: error.into(),
                    }
                })?;
                let access =
                    gents_store::config_client::ConfigAccess::local_as(node.clone(), actor);
                let started = std::time::Instant::now();
                let boundary =
                    gents_session::session::provider_context_reduction::capture_source_boundary(
                        &access,
                        &request.session_id,
                        &request.node_did,
                        request.requester_did.as_deref(),
                        &request.doc_id,
                        &request_commit_cid,
                    )
                    .await
                    .map_err(|error| ReplayEvidenceResolveError::Storage {
                        source: error.into(),
                    })?;
                let boundary_elapsed_ms = started.elapsed().as_millis() as u64;
                let resolved = gents_session::session::resolve_canonical_replay_tags_on(
                    &access,
                    gents_session::session::CanonicalReplayScope {
                        node_did: &request.node_did,
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
                .await
                .map_err(replay_resolution_error)?;
                tracing::info!(
                    target: "gents::turn::provider_input",
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
                recording_seed.record(rows.clone());
                Ok(rows)
            })
        })),
        ..LoopReplayInput::default()
    }
}

pub(crate) fn tag_canonical_history(
    rows: &[gents_session::session::SequencedMessage],
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

/// A violation anywhere under the resolver's context is a request error the
/// loop refuses; every other failure is storage the loop reports.
fn replay_resolution_error(error: anyhow::Error) -> ReplayEvidenceResolveError {
    match ReplayEvidenceViolation::in_chain(error.as_ref()).cloned() {
        Some(violation) => ReplayEvidenceResolveError::Violation(violation),
        None => ReplayEvidenceResolveError::Storage {
            source: error.into(),
        },
    }
}
