use chrono::{DateTime, Utc};

use super::rows::PairingStateRow;
use super::EnrollmentEndpointEntry;

pub(super) fn enrollment_entry_is_fresh_at(
    entry: &EnrollmentEndpointEntry,
    now: DateTime<Utc>,
) -> bool {
    entry.authorization_sequence > 0
        && entry.authorization_sequence <= i64::MAX as u64
        && gents_protocol::enrollment::authorization_lease_is_fresh_at(
            &entry.authorization_expires_at,
            now,
        )
}

pub(super) fn materialized_enrollment_entry(
    entries: &[EnrollmentEndpointEntry],
    peer_id: &str,
    self_did: &str,
) -> Option<MaterializedDataPlaneEntry> {
    entries
        .iter()
        .find(|entry| entry.desired_id == peer_id && entry.node_did != self_did)
        .cloned()
        .map(|endpoint| MaterializedDataPlaneEntry {
            endpoint,
            source: "enrollment",
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MaterializedDataPlaneEntry {
    pub(super) endpoint: EnrollmentEndpointEntry,
    source: &'static str,
}

impl std::ops::Deref for MaterializedDataPlaneEntry {
    type Target = EnrollmentEndpointEntry;

    fn deref(&self) -> &Self::Target {
        &self.endpoint
    }
}

pub(super) fn enrollment_base_row(
    mut row: PairingStateRow,
    entry: &MaterializedDataPlaneEntry,
    self_did: &str,
) -> Option<PairingStateRow> {
    if row.source.as_deref() != Some(entry.source) {
        return None;
    }
    if row.enrollment_request_digest.as_deref() != Some(&entry.request_digest)
        || row.enrollment_authorization_sequence != Some(entry.authorization_sequence as i64)
        || row.enrollment_authorization_expires_at.as_deref()
            != Some(&entry.authorization_expires_at)
    {
        return None;
    }
    // Enrollment authority owns the full base route; the document is only a
    // materialization witness.
    row.peer_id = Some(entry.endpoint.desired_id.clone());
    row.node_did = Some(self_did.to_string());
    row.collections = None;
    row.replicator_addresses = Some(vec![entry.endpoint.address.clone()]);
    row.template = Some(super::super::templates::CLIENT_TEMPLATE.to_string());
    Some(row)
}

pub(super) fn local_data_plane_row(
    mut row: PairingStateRow,
    entry: &MaterializedDataPlaneEntry,
    self_did: &str,
) -> Option<PairingStateRow> {
    let source = row.source.as_deref()?.trim();
    if source.is_empty() || source == entry.source {
        return None;
    }
    // A local data-plane document may choose only the non-protocol collection
    // overlay; enrollment remains the transport and identity gate.
    row.peer_id = Some(entry.endpoint.peer_id.clone());
    row.node_did = Some(self_did.to_string());
    row.replicator_addresses = Some(vec![entry.endpoint.address.clone()]);
    Some(row)
}
