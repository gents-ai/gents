use anyhow::Result;
use defra_node::EmbeddedNode;
use std::collections::{BTreeMap, BTreeSet};

use super::access::{rows_value, ReadAccess};
use super::replay_candidates::SelectedReplayCandidate;
use super::replay_request::required_row_str;
#[cfg(test)]
use super::CAPTURE_ROW_QUERIES;
use super::{replay_violation, CanonicalProviderCoordinate, CanonicalReplayCapture};

pub(super) fn replay_capture_key(
    node_did: &str,
    session_id: &str,
    request_doc_id: &str,
    coordinate: CanonicalProviderCoordinate,
) -> Result<String> {
    let CanonicalProviderCoordinate {
        scope: capture_scope,
        turn_index,
        attempt,
    } = coordinate;
    Ok(gents_loop::rendered_request::capture_key(
        node_did,
        session_id,
        request_doc_id,
        &capture_scope.to_string(),
        usize::try_from(turn_index)?,
        attempt,
    )?)
}

/// Batched `capture_key` read for the pass's selected coordinates: one chunked
/// `_in` statement instead of one per candidate, with rows grouped per key so
/// a key with zero or multiple rows still leaves its turn without evidence,
/// never an error. Rows are queried by the exact key only: a contradictory row
/// under a key must be detected, not filtered away.
pub(super) async fn load_replay_capture_rows(
    node: &EmbeddedNode,
    selected: &[SelectedReplayCandidate],
) -> Result<BTreeMap<String, Vec<serde_json::Value>>> {
    let mut keys = Vec::new();
    for item in selected {
        if !keys.contains(&item.capture_key) {
            keys.push(item.capture_key.clone());
        }
    }
    let mut rows: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for chunk in keys.chunks(crate::inference::rendered_request::commits::FIELD_COMMIT_ALIAS_BATCH)
    {
        let filter = chunk
            .iter()
            .map(|key| format!("\"{}\"", crate::store::graphql::escape_graphql_string(key)))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            r#"{{ RenderedRequest(filter: {{ capture_key: {{ _in: [{filter}] }} }}) {{ capture_key capture_version request_doc_id request_commit_cid request_id session_id node_did requester_did capture_scope turn_index attempt source request_json provenance_json }} }}"#
        );
        #[cfg(test)]
        let _ = CAPTURE_ROW_QUERIES
            .try_with(|queries| queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let response = ReadAccess::Node(node)
            .query(&query, "load_canonical_replay_captures")
            .await?;
        for row in rows_value(&response, "RenderedRequest")? {
            // An `_in` on capture_key only returns rows carrying that field;
            // a row without one is attributable to no requested key.
            let Some(key) = row.get("capture_key").and_then(serde_json::Value::as_str) else {
                continue;
            };
            rows.entry(key.to_owned()).or_default().push(row.clone());
        }
    }
    Ok(rows)
}

/// Read every manifest block the pass's unambiguous captures name in one
/// shared batched pass, so per-candidate verification resolves from positive
/// cache observations only. A capture whose record does not decode locally is
/// left to its verifier, which reports it per capture exactly as an unbatched
/// read would; the shared block read isolates a verification failure per
/// manifest the same way.
pub(super) async fn preload_replay_capture_blocks(
    node: &EmbeddedNode,
    capture_rows: &BTreeMap<String, Vec<serde_json::Value>>,
    cache: &mut crate::inference::rendered_request::CaptureReadCache,
) -> Result<()> {
    let mut manifests = Vec::new();
    let mut trees = Vec::new();
    for rows in capture_rows.values() {
        let [capture] = rows.as_slice() else {
            continue;
        };
        let Some(version) = capture["capture_version"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
        else {
            continue;
        };
        let Some(stored) = capture
            .get("request_json")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        match crate::inference::rendered_request::encoding::decode_capture_record(
            version,
            stored,
            crate::inference::rendered_request::CapturePayloadKind::RequestBody,
        ) {
            Ok(crate::inference::rendered_request::encoding::DecodedRecord::Manifest {
                blocks,
            }) => manifests.push(blocks),
            Ok(crate::inference::rendered_request::encoding::DecodedRecord::Tree(tree)) => {
                trees.push(tree)
            }
            _ => {}
        }
    }
    crate::inference::rendered_request::preload_manifest_blocks(node, &manifests, cache).await?;
    crate::inference::rendered_request::preload_manifest_trees(node, &trees, cache).await
}

/// Verify the one capture row the pass's batched read returned under
/// `capture_key`. Zero rows (no capture) and multiple rows (an ambiguous key)
/// both leave the turn without evidence, never an error.
#[allow(clippy::too_many_arguments)]
pub(super) async fn replay_capture_from_row(
    node: &EmbeddedNode,
    capture_rows: &BTreeMap<String, Vec<serde_json::Value>>,
    capture_key: &str,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    request_id: &str,
    request_doc_id: &str,
    request_commits: &BTreeSet<String>,
    coordinate: CanonicalProviderCoordinate,
    cache: &mut crate::inference::rendered_request::CaptureReadCache,
) -> Result<Option<CanonicalReplayCapture>> {
    let Some([capture]) = capture_rows.get(capture_key).map(Vec::as_slice) else {
        return Ok(None);
    };
    // An unverifiable capture makes only this turn non-replayable; it must not
    // fail every later request in the session.
    match verify_replay_capture(
        node,
        capture,
        capture_key,
        node_did,
        requester_did,
        session_id,
        request_id,
        request_doc_id,
        request_commits,
        coordinate,
        cache,
    )
    .await
    {
        Ok(verified) => Ok(verified),
        Err(error)
            if error
                .downcast_ref::<crate::inference::rendered_request::CaptureStoreReadError>()
                .is_some() =>
        {
            Err(error)
        }
        Err(error) => {
            warn_unverifiable_replay_capture(capture_key, &error);
            Ok(None)
        }
    }
}

/// A bad capture is re-read on every request of its session; one warning a
/// minute, with the suppressed count, keeps it visible without flooding.
fn warn_unverifiable_replay_capture(capture_key: &str, error: &anyhow::Error) {
    use crate::log_rate::{CallsiteRateLimiter, Decision, RateLimitConfig};
    static LIMITER: std::sync::LazyLock<std::sync::Mutex<CallsiteRateLimiter<()>>> =
        std::sync::LazyLock::new(|| {
            std::sync::Mutex::new(CallsiteRateLimiter::new(RateLimitConfig {
                max_per_window: 1,
                window: std::time::Duration::from_secs(60),
            }))
        });
    let decision = LIMITER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .check((), std::time::Instant::now());
    let suppressed = match decision {
        Decision::Suppress => return,
        Decision::Allow => 0,
        Decision::AllowWithSummary { suppressed } => suppressed,
    };
    tracing::warn!(
        capture_key = %capture_key,
        suppressed,
        error = %format!("{error:#}"),
        "accepted turn capture is unverifiable; its reasoning is not replayed"
    );
}

/// Capture versions whose stored request body can be decoded for reasoning
/// replay. v1 rows keep their own readers and never carry a replayable
/// provenance manifest; later versions decode through the versioned container.
const REPLAY_CAPTURE_VERSIONS: &[u32] = &[2, 3, 4];

#[allow(clippy::too_many_arguments)]
async fn verify_replay_capture(
    node: &EmbeddedNode,
    capture: &serde_json::Value,
    capture_key: &str,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    request_id: &str,
    request_doc_id: &str,
    request_commits: &BTreeSet<String>,
    coordinate: CanonicalProviderCoordinate,
    cache: &mut crate::inference::rendered_request::CaptureReadCache,
) -> Result<Option<CanonicalReplayCapture>> {
    let CanonicalProviderCoordinate {
        scope: capture_scope,
        turn_index,
        attempt,
    } = coordinate;
    let capture_scope_label = capture_scope.to_string();
    // Rows are immutable, so every container version ever written stays in the
    // store; replay accepts the ones whose request body decodes through the
    // versioned container. A v1 row never carries a replayable provenance
    // manifest, and an unknown version is reported rather than reinterpreted.
    let capture_version = capture["capture_version"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default();
    replay_ensure!(
        required_row_str(capture, "capture_key")? == capture_key
            && required_row_str(capture, "request_doc_id")? == request_doc_id
            && required_row_str(capture, "request_id")? == request_id
            && required_row_str(capture, "session_id")? == session_id
            && required_row_str(capture, "node_did")? == node_did
            && required_row_str(capture, "requester_did")? == requester_did.unwrap_or("")
            && required_row_str(capture, "capture_scope")? == capture_scope_label
            && REPLAY_CAPTURE_VERSIONS.contains(&capture_version)
            && capture["turn_index"].as_u64() == Some(u64::from(turn_index))
            && capture["attempt"].as_u64() == Some(u64::from(attempt)),
        "canonical replay capture disagrees with its exact provider close coordinate"
    );
    replay_ensure!(
        request_commits.contains(required_row_str(capture, "request_commit_cid")?),
        "canonical replay capture request CID is not a commit of its physical request"
    );
    let source: gents_protocol::rendered_request::RenderedRequestSource = serde_json::from_value(
        capture
            .get("source")
            .cloned()
            .ok_or_else(|| replay_violation("capture has no source"))?,
    )
    .map_err(|error| {
        replay_violation(format!(
            "canonical replay capture has malformed source: {error}"
        ))
    })?;
    let wire = match source {
        gents_protocol::rendered_request::RenderedRequestSource::ClaudeCliSubscription => {
            gents_loop::claude_messages_body::ReplayWire::ClaudeMessages
        }
        gents_protocol::rendered_request::RenderedRequestSource::OpenAiResponses => {
            gents_loop::claude_messages_body::ReplayWire::Responses
        }
        _ => return Ok(None),
    };
    let manifest = match gents_protocol::rendered_request::ProvenanceManifest::parse(
        required_row_str(capture, "provenance_json")?,
    ) {
        Ok(gents_protocol::rendered_request::ParsedProvenance::Manifest(manifest))
            if manifest.capture_scope == capture_scope_label =>
        {
            manifest
        }
        _ => return Ok(None),
    };
    let Some(issuer) = manifest
        .provider_family
        .as_deref()
        .zip(
            manifest
                .provider_endpoint
                .as_deref()
                .zip(manifest.provider_route_path_sha256.as_deref()),
        )
        .and_then(|(family, (endpoint, path))| {
            gents_loop::rendered_request::transport::replay_issuer_from_capture(
                family, endpoint, path,
            )
        })
    else {
        return Ok(None);
    };
    let body = crate::inference::rendered_request::decode_capture_json_embedded_cached(
        node,
        capture_version,
        required_row_str(capture, "request_json")?,
        crate::inference::rendered_request::CapturePayloadKind::RequestBody,
        cache,
    )
    .await?;
    Ok(Some(CanonicalReplayCapture { issuer, wire, body }))
}
