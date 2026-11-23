use gents_protocol::rendered_request::{
    CaptureOrderKey, CaptureScope, ParsedProvenance, ProvenanceManifest, ProvenanceStatus,
};

use super::events::TimelineRenderedRequestEvent;
use super::rows::TimelineRenderedRequestRow;

pub fn rendered_request_event(row: &TimelineRenderedRequestRow) -> TimelineRenderedRequestEvent {
    let scope = row
        .capture_scope
        .as_deref()
        .and_then(|scope| scope.parse::<CaptureScope>().ok());
    let (provenance_status, manifest_version, call_id, call_seq, reduction_keys) =
        match row.provenance_json.as_deref() {
            None => ("unavailable".to_string(), None, None, None, Vec::new()),
            Some(raw) => match ProvenanceManifest::parse(raw) {
                Ok(ParsedProvenance::Manifest(manifest)) => {
                    let reduction_keys = manifest.assembly_trace.reduction_keys.clone();
                    (
                        provenance_status_label(manifest.status).to_string(),
                        Some(i64::from(manifest.manifest_version)),
                        manifest
                            .admission
                            .as_ref()
                            .map(|admission| admission.call_id.clone()),
                        manifest
                            .admission
                            .as_ref()
                            .map(|admission| admission.call_seq),
                        reduction_keys,
                    )
                }
                Ok(ParsedProvenance::Unsupported { manifest_version }) => (
                    "unsupported_manifest".to_string(),
                    Some(i64::from(manifest_version)),
                    None,
                    None,
                    Vec::new(),
                ),
                Err(_) => ("malformed".to_string(), None, None, None, Vec::new()),
            },
        };

    TimelineRenderedRequestEvent {
        capture_key: row.capture_key.clone(),
        request_doc_id: row.request_doc_id.clone(),
        request_id: row.request_id.clone(),
        session_id: row.session_id.clone(),
        capture_scope: row.capture_scope.clone(),
        scope_kind: scope.map(|scope| scope.kind.as_str().to_string()),
        scope_seq: scope.map(|scope| i64::try_from(scope.seq).unwrap_or(i64::MAX)),
        turn_index: row.turn_index,
        attempt: row.attempt,
        capture_version: row.capture_version,
        model_name: row.model_name.clone(),
        source: row.source.clone(),
        prompt_hash: row.prompt_hash.clone(),
        tools_hash: row.tools_hash.clone(),
        provenance_status,
        manifest_version,
        call_id,
        call_seq,
        reduction_keys,
        created_at: row.created_at.clone(),
    }
}

fn provenance_status_label(status: ProvenanceStatus) -> &'static str {
    match status {
        ProvenanceStatus::CapturedOnly => "captured_only",
    }
}

pub fn rendered_request_tiebreak(event: &TimelineRenderedRequestEvent) -> String {
    let scope = event
        .capture_scope
        .as_deref()
        .and_then(|scope| scope.parse::<CaptureScope>().ok());
    match (scope, event.turn_index, event.attempt) {
        (Some(scope), Some(turn_index), Some(attempt)) => CaptureOrderKey {
            scope,
            turn_index,
            attempt,
        }
        .padded(),
        _ => event.capture_key.clone(),
    }
}
