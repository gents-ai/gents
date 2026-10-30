use anyhow::Result;
use defra_node::EmbeddedNode;
use gents_protocol::output::reconstruction::{reconstruct_message, ObservedSegment};
use gents_protocol::output::OutputSource;
use rapidhash::{HashSetExt, RapidHashSet};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::super::canonical_rows::{
    decode_output_segment_row, decode_scoped_request_output_segments, AGENT_OUTPUT_SEGMENT_FIELDS,
};
use super::access::{rows_value, ReadAccess, ReadCache, RequestOutputRows};
#[cfg(test)]
use super::{record_request_output_scan, SOURCE_OUTPUT_QUERIES};
use super::headers::{load_header, validate_origin_chain};
use super::ReconstructedScopedMessage;

async fn request_rows(
    access: ReadAccess<'_, '_>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &ReadCache,
    local: &mut BTreeMap<String, Arc<RequestOutputRows>>,
) -> Result<Arc<RequestOutputRows>> {
    if let Some(rows) = cache
        .requests
        .get(request_doc_id)
        .or_else(|| local.get(request_doc_id))
    {
        return Ok(Arc::clone(rows));
    }
    #[cfg(test)]
    record_request_output_scan();
    let values = access
        .request_segment_values(request_doc_id, "load_output_source")
        .await?;
    let rows = decode_scoped_request_output_segments(&values, node_did, None, requester_did)?;
    let rows = Arc::new(RequestOutputRows::from(rows));
    local.insert(request_doc_id.to_owned(), Arc::clone(&rows));
    Ok(rows)
}

pub(super) async fn source_rows(
    access: ReadAccess<'_, '_>,
    request_doc_id: &str,
    source: &OutputSource,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Vec<super::super::canonical_rows::OutputSegmentRow>> {
    #[cfg(test)]
    let _ = SOURCE_OUTPUT_QUERIES
        .try_with(|queries| queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let values = if let ReadAccess::Txn(txn) = access {
        txn.cursor_rows(
            "AgentOutputSegment",
            AGENT_OUTPUT_SEGMENT_FIELDS,
            &super::super::canonical_rows::source_output_segments_filter(request_doc_id, source)?,
        )
        .await?
    } else {
        let query =
            super::super::canonical_rows::source_output_segments_query(request_doc_id, source)?;
        let response = access.query(&query, "load_output_source").await?;
        rows_value(&response, "AgentOutputSegment")?.to_vec()
    };
    Ok(
        decode_scoped_request_output_segments(&values, node_did, None, requester_did)?
            .into_iter()
            .filter(|row| row.segment.source == *source)
            .collect(),
    )
}

/// Load output segments for the pass's referenced request documents
/// through the indexed request field, in chunks. The per-pass cache still
/// owns freshness: if reconstruction detects an incomplete reused scan, its
/// existing retry evicts and rereads that request alone.
pub(super) async fn preload_request_output_segments(
    node: &EmbeddedNode,
    request_doc_ids: &[String],
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<()> {
    let distinct = request_doc_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    for chunk in distinct.chunks(crate::rendered_request::commits::FIELD_COMMIT_ALIAS_BATCH) {
        let filter = chunk
            .iter()
            .map(|request_doc_id| {
                format!(
                    r#""{}""#,
                    crate::graphql::escape_graphql_string(request_doc_id)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _in: [{filter}] }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
        );
        #[cfg(test)]
        record_request_output_scan();
        let response = ReadAccess::Node(node)
            .query(&query, "preload_canonical_replay_output_segments")
            .await?;
        let rows = decode_scoped_request_output_segments(
            rows_value(&response, "AgentOutputSegment")?,
            node_did,
            None,
            requester_did,
        )?;
        let allowed = chunk.iter().cloned().collect::<BTreeSet<_>>();
        let mut grouped: BTreeMap<String, Vec<super::super::canonical_rows::OutputSegmentRow>> =
            BTreeMap::new();
        for row in rows {
            anyhow::ensure!(
                allowed.contains(&row.segment.request_doc_id),
                "canonical replay output batch returned an unrequested physical request"
            );
            grouped
                .entry(row.segment.request_doc_id.clone())
                .or_default()
                .push(row);
        }
        for request_doc_id in chunk {
            cache.requests.insert(
                request_doc_id.clone(),
                Arc::new(RequestOutputRows::from(
                    grouped.remove(request_doc_id).unwrap_or_default(),
                )),
            );
        }
    }
    Ok(())
}

pub(super) async fn load_referenced_segments(
    access: ReadAccess<'_, '_>,
    header: &super::super::canonical_rows::TranscriptMessageRow,
    request_hint: Option<&str>,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<Vec<super::super::canonical_rows::OutputSegmentRow>> {
    let requester = requester_did
        .map(|did| format!(r#""{}""#, crate::graphql::escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let close_ids = header
        .message
        .payload_references()
        .into_iter()
        .map(|reference| reference.close_doc_id.clone())
        .collect::<BTreeSet<_>>();
    let mut local = BTreeMap::new();
    let hinted = match request_hint {
        Some(request) if !cache.source_reads && !close_ids.is_empty() => {
            Some(request_rows(access, request, node_did, requester_did, cache, &mut local).await?)
        }
        _ => None,
    };
    let mut closures = Vec::new();
    for close_id in close_ids {
        let found = hinted
            .iter()
            .flat_map(|rows| rows.with_doc_id(&close_id))
            .cloned()
            .collect::<Vec<_>>();
        if !found.is_empty() {
            closures.extend(found);
            continue;
        }
        let query = format!(
            r#"{{ AgentOutputSegment(filter: {{ _docID: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}, limit: 2) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
            crate::graphql::escape_graphql_string(&close_id),
            crate::graphql::escape_graphql_string(node_did)
        );
        let response = access.query(&query, "load_output_closure").await?;
        let rows = rows_value(&response, "AgentOutputSegment")?;
        closures.extend(
            rows.iter()
                .map(decode_output_segment_row)
                .collect::<Result<Vec<_>>>()?,
        );
    }
    for closure in &closures {
        if cache
            .requests
            .get(&closure.segment.request_doc_id)
            .is_some_and(|rows| rows.with_doc_id(&closure.doc_id).next().is_none())
        {
            cache.requests.remove(&closure.segment.request_doc_id);
        }
    }
    let mut sources = Vec::new();
    let mut seen = RapidHashSet::new();
    for closure in &closures {
        if seen.insert((&closure.segment.request_doc_id, &closure.segment.source)) {
            sources.push((
                closure.segment.request_doc_id.clone(),
                closure.segment.source.clone(),
            ));
        }
    }
    // Do not key these observations by document ID: contradictory observations
    // of one physical ID must reach the shared integrity checker, not become
    // last-writer-wins here. Exact duplicates are harmless to reconstruction.
    let mut records = Vec::new();
    for (request_doc_id, source) in sources {
        if cache.source_reads {
            if let Some(rows) = cache.sources.get(&(request_doc_id.clone(), source.clone())) {
                records.extend(rows.iter().cloned());
            } else {
                records.extend(
                    source_rows(access, &request_doc_id, &source, node_did, requester_did).await?,
                );
            }
            continue;
        }
        let rows = request_rows(
            access,
            &request_doc_id,
            node_did,
            requester_did,
            cache,
            &mut local,
        )
        .await?;
        records.extend(rows.for_source(&source).cloned());
    }
    // A transaction has a fixed view; a node read may still be receiving
    // additional facts, so reuse its scan only within one opted-in pass.
    if matches!(access, ReadAccess::Txn(_)) || cache.reuse_node_scans {
        cache.requests.extend(local);
    }
    Ok(records)
}

pub(super) async fn reconstruct_scoped_message(
    access: ReadAccess<'_, '_>,
    header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<(
    gents_protocol::output::TranscriptMessage,
    gents_protocol::message::Message,
)> {
    let reconstructed = reconstruct_scoped_message_with_facts(
        access,
        header_doc_id,
        node_did,
        requester_did,
        cache,
    )
    .await?;
    Ok((reconstructed.header.message, reconstructed.message))
}

pub(super) async fn reconstruct_scoped_message_with_facts(
    access: ReadAccess<'_, '_>,
    header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<ReconstructedScopedMessage> {
    let header = load_header(access, header_doc_id, node_did, requester_did, cache).await?;
    let origin = validate_origin_chain(access, &header, node_did, requester_did, cache).await?;
    let request_hint = origin.message.request_doc_id.clone();
    let reconstruct = |segments: &[super::super::canonical_rows::OutputSegmentRow]| {
        let observed = segments
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        if origin.doc_id != header.doc_id {
            // Fork equality alone is insufficient: the origin's publication/source
            // constraints must hold too. Fork publication deliberately relaxes those
            // constraints on the child, not on the original authored message.
            reconstruct_message(&observed, &[], &[], &origin.message).map_err(|error| {
                (
                    error,
                    format!("reconstructing fork origin {}", origin.doc_id),
                )
            })?;
        }
        reconstruct_message(&observed, &[], &[], &header.message).map_err(|error| {
            (
                error,
                format!("reconstructing exact canonical message {header_doc_id}"),
            )
        })
    };
    // A node scan reused from an earlier message may predate this message's
    // payload segments; an incomplete result from it is re-read once.
    let reused = if cache.reuse_node_scans && !matches!(access, ReadAccess::Txn(_)) {
        cache.requests.keys().cloned().collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let mut segments = load_referenced_segments(
        access,
        &header,
        request_hint.as_deref(),
        node_did,
        requester_did,
        cache,
    )
    .await?;
    let mut reconstructed = reconstruct(&segments);
    if let Err((error, _)) = &reconstructed {
        let stale = segments
            .iter()
            .map(|row| &row.segment.request_doc_id)
            .chain(request_hint.as_ref())
            .filter(|request| reused.contains(*request))
            .cloned()
            .collect::<BTreeSet<_>>();
        if error.is_incomplete() && !stale.is_empty() {
            for request in &stale {
                cache.requests.remove(request);
            }
            segments = load_referenced_segments(
                access,
                &header,
                request_hint.as_deref(),
                node_did,
                requester_did,
                cache,
            )
            .await?;
            reconstructed = reconstruct(&segments);
        }
    }
    let message =
        reconstructed.map_err(|(error, context)| anyhow::Error::new(error).context(context))?;
    Ok(ReconstructedScopedMessage {
        header,
        origin,
        message,
        segments,
    })
}
