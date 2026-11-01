use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context, Result};
use gents::inference::rendered_request::{
    canonical_json_string, capture_key, decode_capture_pair, AssemblyTrace, CaptureScopeKind,
    ParsedProvenance, ProvenanceManifest,
};
use gents::store::config_client::ConfigAccess;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};

use super::stability::{FieldFingerprint, Observation};
use super::validation::{
    capture_order, digest, integer, string, validate_body, validate_owner, validate_request,
};
use super::{ReceiptLimits, ReceiptRequest};

/// Check the session's request set from the same snapshot as the request rows.
pub(super) fn validate_session_request_sets(
    observations: &[Observation],
    requests: &[ReceiptRequest],
    max_requests: usize,
) -> Result<()> {
    let mut groups = BTreeMap::<(&str, &str), Vec<&ReceiptRequest>>::new();
    for request in requests {
        groups
            .entry((&request.node_did, &request.session_id))
            .or_default()
            .push(request);
    }
    for ((node, session), expected) in groups {
        let observation = observations
            .iter()
            .find(|observation| {
                observation.request["node_did"] == json!(node)
                    && observation.request["session_id"] == json!(session)
            })
            .context("snapshot lacks the session request set")?;
        ensure!(
            observation.session_requests.len() <= max_requests,
            "session request set exceeds its bound"
        );
        let mut seen = BTreeSet::new();
        for row in &observation.session_requests {
            let logical = string(row, "request_id")?;
            let request = expected
                .iter()
                .find(|request| request.request_id == logical)
                .context("session request set includes an unlisted request")?;
            validate_request(row, request)?;
            ensure!(
                seen.insert(logical),
                "session request set has duplicate logical identities"
            );
            if let Some(physical) = &request.request_doc_id {
                ensure!(
                    string(row, "_docID")? == physical,
                    "session request physical identity mismatch"
                );
            }
        }
        ensure!(
            seen.len() == expected.len(),
            "session request set is missing an expected request"
        );
    }
    Ok(())
}

/// Validate one request's evidence from a single snapshot, then decode its
/// capture payloads. Decoding reads content-addressed blocks and delta bases
/// outside the snapshot; the existing decoder verifies each against the commit
/// the snapshot recorded, so a rewritten block fails instead of publishing.
pub(super) async fn collect_request(
    access: &ConfigAccess,
    observation: &Observation,
    expected: &ReceiptRequest,
    limits: ReceiptLimits,
) -> Result<Value> {
    for value in [
        &expected.node_did,
        &expected.requester_did,
        &expected.agent_id,
        &expected.session_id,
        &expected.request_id,
        &expected.lifecycle_state,
    ] {
        ensure!(
            !value.trim().is_empty(),
            "blank expected request identity or lifecycle"
        );
    }
    ensure!(
        RequestLifecycleState::parse(&expected.lifecycle_state)?.is_terminal(),
        "receipt requires terminal request lifecycle evidence"
    );
    let request = &observation.request;
    validate_request(request, expected)?;
    let physical = string(request, "_docID")?;
    if let Some(wanted) = &expected.request_doc_id {
        ensure!(physical == wanted, "request physical identity mismatch");
    }
    let lifecycle_commit = required_commit(observation, physical, "lifecycle_state")?;
    let captures = &observation.captures;
    let calls = &observation.calls;
    ensure!(
        !captures.is_empty() && !calls.is_empty(),
        "receipt has no durable attempts"
    );
    let mut remaining_calls = BTreeSet::new();
    for call in calls {
        validate_owner(call, expected, physical, false)?;
        ensure!(
            matches!(
                string(call, "call_state")?,
                "completed" | "failed" | "cancelled"
            ),
            "receipt contains a nonterminal provider call"
        );
        string(call, "started_at")?;
        string(call, "ended_at")?;
        ensure!(
            remaining_calls.insert(string(call, "call_id")?),
            "duplicate inference call identity"
        );
    }
    let mut order = BTreeSet::new();
    let mut capture_keys = BTreeSet::new();
    let mut ordinary_count = 0;
    let mut summary_count = 0;
    let mut attempts = Vec::new();
    for capture in captures {
        validate_owner(capture, expected, physical, true)?;
        let key = capture_order(capture)?;
        ensure!(order.insert(key), "duplicate capture scope/turn/attempt");
        ensure!(
            string(capture, "capture_key")?
                == capture_key(
                    &expected.node_did,
                    &expected.session_id,
                    physical,
                    string(capture, "capture_scope")?,
                    usize::try_from(key.turn_index)?,
                    u32::try_from(key.attempt)?,
                )?,
            "capture key does not match its owned identity"
        );
        ensure!(
            capture_keys.insert(string(capture, "capture_key")?),
            "duplicate capture key"
        );
        let (target, kind) = match key.scope.kind {
            CaptureScopeKind::Inference => {
                ordinary_count += 1;
                (&expected.ordinary, "inference")
            }
            CaptureScopeKind::Compaction | CaptureScopeKind::CompactionFallback => {
                summary_count += 1;
                (
                    expected
                        .summary
                        .as_ref()
                        .context("unexpected summary attempt")?,
                    "compaction",
                )
            }
            _ => anyhow::bail!("unexpected capture scope in live receipt"),
        };
        let version = u32::try_from(integer(capture, "capture_version")?)?;
        let stored = string(capture, "request_json")?;
        ensure!(
            stored.len() <= limits.max_decoded_bytes_per_capture,
            "encoded capture exceeds byte bound"
        );
        let (body, payload) = decode_capture_pair(access, version, stored)
            .await
            .context("decoding durable live capture")?;
        let body_bytes = canonical_json_string(&body)?;
        let payload_bytes = canonical_json_string(&payload)?;
        ensure!(
            body_bytes
                .len()
                .checked_add(payload_bytes.len())
                .is_some_and(|length| length <= limits.max_decoded_bytes_per_capture),
            "decoded capture exceeds byte bound"
        );
        let provenance = string(capture, "provenance_json")?;
        ensure!(
            provenance.len() <= limits.max_decoded_bytes_per_capture,
            "provenance exceeds byte bound"
        );
        let ParsedProvenance::Manifest(manifest) = ProvenanceManifest::parse(provenance)? else {
            anyhow::bail!("unsupported provenance manifest")
        };
        let trace: AssemblyTrace = serde_json::from_value(payload.clone())
            .context("decoding canonical provenance assembly trace")?;
        ensure!(
            manifest.assembly_trace == (&trace).into(),
            "provenance metadata/payload mismatch"
        );
        ensure!(
            manifest.capture_scope == string(capture, "capture_scope")?,
            "provenance scope mismatch"
        );
        let join = manifest
            .admission
            .as_ref()
            .context("capture lacks admission identity")?;
        ensure!(
            remaining_calls.remove(join.call_id.as_str()),
            "unexpected or repeated capture admission"
        );
        let call = calls
            .iter()
            .find(|call| call["call_id"] == join.call_id)
            .context("missing inference call")?;
        ensure!(
            integer(call, "call_seq")? == join.call_seq && join.call_seq > 0,
            "admission sequence mismatch"
        );
        // Admission counts outer request attempts; capture counts retries in
        // this completion loop. The durable admission join connects them.
        ensure!(integer(call, "attempt")? > 0, "invalid admission attempt");
        ensure!(
            string(call, "call_kind")? == kind,
            "admission scope kind mismatch"
        );
        ensure!(
            string(call, "backend_id")? == target.backend_id,
            "authored backend mismatch"
        );
        ensure!(
            string(call, "model_name")? == target.model,
            "admission model mismatch"
        );
        let observed = validate_body(capture, &body, &manifest, target)?;
        let capture_doc = string(capture, "_docID")?;
        attempts.push(json!({
            "capture_doc_id": capture_doc,
            "capture_key": string(capture, "capture_key")?,
            "request_commit_cid": string(capture, "request_commit_cid")?,
            "capture_scope": manifest.capture_scope,
            "turn_index": key.turn_index, "attempt": key.attempt,
            "capture_version": version,
            "request_json_commit": required_commit(observation, capture_doc, "request_json")?,
            "provenance_json_commit": required_commit(observation, capture_doc, "provenance_json")?,
            "provenance": {
                "manifest_version": manifest.manifest_version,
                "status": manifest.status, "capture_seam": manifest.capture_seam,
                "request_body_sha256": digest(body_bytes.as_bytes()),
                "request_body_bytes": body_bytes.len(),
                "payload_sha256": digest(payload_bytes.as_bytes()),
                "payload_bytes": payload_bytes.len(),
                "trace_version": manifest.assembly_trace.trace_version,
            },
            "observed": observed,
            "call": call,
            "call_state_commit": required_commit(observation, string(call, "_docID")?, "call_state")?,
        }));
    }
    ensure!(
        remaining_calls.is_empty(),
        "admitted attempts lack durable captures"
    );
    ensure!(ordinary_count > 0, "receipt lacks ordinary inference");
    ensure!(
        !expected.require_summary || summary_count > 0,
        "receipt lacks required summary attempt"
    );
    Ok(json!({"request": request, "lifecycle_commit": lifecycle_commit, "attempts": attempts}))
}

fn required_commit(observation: &Observation, doc_id: &str, field: &str) -> Result<Value> {
    let FieldFingerprint { cid, height } = observation
        .commits
        .get(&(doc_id.to_owned(), field.to_owned()))
        .with_context(|| format!("receipt lacks durable {field} commit for {doc_id}"))?;
    ensure!(!cid.is_empty() && *height > 0, "invalid field commit");
    Ok(json!({"cid": cid, "height": height}))
}
