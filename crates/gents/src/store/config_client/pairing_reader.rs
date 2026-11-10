use super::{ConfigAccess, ConfigApplyTxn, ConfigClientError};
use gents_protocol::owner_staged::OwnerStagedChange;

pub use gents_protocol::pairing_reader::{PairingObservation, PairingReadReason};

/// The native relation that records `reason`'s reader grant.
pub(super) fn reader_relation(reason: PairingReadReason) -> &'static str {
    match reason {
        PairingReadReason::RouteAdmission => "route_reader",
        PairingReadReason::ToolView => "tool_reader",
    }
}

fn change(
    observation: PairingObservation,
    doc_id: &str,
    reason: PairingReadReason,
    agent: &identity::Did,
) -> OwnerStagedChange {
    OwnerStagedChange::PairingReader {
        observation,
        doc_id: doc_id.to_owned(),
        reason,
        reader_did: agent.as_str().to_owned(),
    }
}

impl ConfigApplyTxn<'_> {
    /// Stage a reader change with other authorized configuration writes
    /// (`Identity.OwnerStaged`, pairing readers). The runtime that owns the
    /// observation stages it for a caller that may update the runtime's own
    /// Node, on either transport (`outcome_is_transport_independent`). This
    /// does not authorize enrollment or permit writing the pairing
    /// observation.
    pub async fn set_pairing_reader(
        &self,
        observation: PairingObservation,
        doc_id: &str,
        reason: PairingReadReason,
        agent: &identity::Did,
        grant: bool,
    ) -> Result<(), ConfigClientError> {
        self.request_owner_staged(&change(observation, doc_id, reason, agent), grant)
            .await
    }
}

impl ConfigAccess {
    /// Apply an explicit native grant or revocation as this access's actor.
    pub async fn set_pairing_reader(
        &self,
        observation: PairingObservation,
        doc_id: &str,
        reason: PairingReadReason,
        agent: &identity::Did,
        grant: bool,
    ) -> Result<(), ConfigClientError> {
        self.request_owner_staged(&change(observation, doc_id, reason, agent), grant)
            .await
    }
}
