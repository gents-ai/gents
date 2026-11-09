use anyhow::Result;
use gents_protocol::output::OutputSource;

use super::access::ReadAccess;
use super::replay_candidates::load_canonical_assistant_candidates_using;
use super::{replay_violation, CanonicalReplayScope};

/// Resolve all requested tags from one physically bounded canonical candidate
/// view. Preserve input order and every physical match (including zero or
/// multiple matches) for the replay owner to reject, without a cross-boundary
/// cache or a coordinate-to-single-winner map.
pub(crate) async fn resolve_canonical_replay_tags_on(
    access: &crate::store::config_client::ConfigAccess,
    scope: CanonicalReplayScope<'_>,
    boundary: &gents_protocol::transcript::SourceBoundary,
    tags: &[gents_loop::claude_messages_body::ReplayTag],
) -> Result<
    Vec<(
        gents_loop::claude_messages_body::ReplayTag,
        Vec<gents_loop::claude_messages_body::ResolvedReplayEvidence>,
    )>,
> {
    resolve_canonical_replay_tags_using(ReadAccess::Config(access), access, scope, boundary, tags)
        .await
}

#[cfg(test)]
pub(crate) async fn resolve_canonical_replay_tags(
    access: &crate::ConfigAccess,
    scope: CanonicalReplayScope<'_>,
    boundary: &gents_protocol::transcript::SourceBoundary,
    tags: &[gents_loop::claude_messages_body::ReplayTag],
) -> Result<
    Vec<(
        gents_loop::claude_messages_body::ReplayTag,
        Vec<gents_loop::claude_messages_body::ResolvedReplayEvidence>,
    )>,
> {
    resolve_canonical_replay_tags_on(access, scope, boundary, tags).await
}

async fn resolve_canonical_replay_tags_using(
    access: ReadAccess<'_, '_>,
    requests: &impl super::replay_request::ReplayRequestReader,
    scope: CanonicalReplayScope<'_>,
    boundary: &gents_protocol::transcript::SourceBoundary,
    tags: &[gents_loop::claude_messages_body::ReplayTag],
) -> Result<
    Vec<(
        gents_loop::claude_messages_body::ReplayTag,
        Vec<gents_loop::claude_messages_body::ResolvedReplayEvidence>,
    )>,
> {
    use gents_loop::claude_messages_body::{reasoning_witness, ResolvedReplayEvidence};

    for tag in tags {
        replay_ensure!(
            matches!(&tag.source, OutputSource::ProviderTurn { .. }),
            "canonical replay tag is not a provider source"
        );
    }
    let candidates =
        load_canonical_assistant_candidates_using(access, requests, scope, boundary, Some(tags))
            .await?;
    tags.iter()
        .map(|tag| {
            let OutputSource::ProviderTurn {
                scope: tag_scope,
                turn_index,
                attempt,
            } = &tag.source
            else {
                unreachable!("all tag sources validated above")
            };
            let evidence = candidates
                .iter()
                .filter(|candidate| {
                    candidate.request_doc_id == tag.request_doc_id
                        && candidate.coordinate.scope == *tag_scope
                        && candidate.coordinate.turn_index == *turn_index
                        && candidate.coordinate.attempt == *attempt
                })
                .filter(|candidate| candidate.capture.is_some())
                .map(|candidate| {
                    let gents_protocol::message::Message::Assistant { content, .. } =
                        &candidate.message
                    else {
                        return Err(replay_violation(
                            "canonical replay candidate is not an assistant message",
                        ));
                    };
                    let capture = candidate.capture.as_ref().expect("capture filtered above");
                    // An undecodable capture leaves the turn without evidence
                    // of its producing prefix, which makes it non-replayable.
                    let captured = gents_loop::provider_input::replay_frontier::flatten(
                        &capture.body,
                        capture.wire,
                    )
                    .ok();
                    Ok(ResolvedReplayEvidence {
                        origin: gents_loop::claude_messages_body::ReplayOrigin::AcceptedProvider,
                        reasoning: reasoning_witness(content),
                        issuer: capture.issuer.clone(),
                        wire: capture.wire,
                        physical_header: candidate.header_doc_id.clone(),
                        complete: true,
                        captured,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((tag.clone(), evidence))
        })
        .collect()
}

#[cfg(test)]
pub(crate) async fn resolve_canonical_replay_tag(
    access: &crate::ConfigAccess,
    scope: CanonicalReplayScope<'_>,
    boundary: &gents_protocol::transcript::SourceBoundary,
    tag: &gents_loop::claude_messages_body::ReplayTag,
) -> Result<Vec<gents_loop::claude_messages_body::ResolvedReplayEvidence>> {
    Ok(
        resolve_canonical_replay_tags(access, scope, boundary, std::slice::from_ref(tag))
            .await?
            .pop()
            .expect("singleton replay resolution returns one entry")
            .1,
    )
}
