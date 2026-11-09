use super::{ConfigAccess, ConfigApplyTxn, ConfigClientError};

pub use gents_protocol::pairing_reader::{PairingObservation, PairingReadReason};

/// The native relation that records `reason`'s reader grant.
pub(super) fn reader_relation(reason: PairingReadReason) -> &'static str {
    match reason {
        PairingReadReason::RouteAdmission => "route_reader",
        PairingReadReason::ToolView => "tool_reader",
    }
}

impl ConfigApplyTxn<'_> {
    /// Stage a reader change with other authorized configuration writes
    /// (`Identity.OwnerStaged`, pairing readers). The runtime that owns the
    /// observation stages it for a caller that may update the runtime's own
    /// Node. Only an embedded transaction decides it: a client reaches the
    /// same decision through the runtime's signed pairing-reader route
    /// (`pairing_outcome_is_transport_independent`). This does not authorize
    /// enrollment or permit writing the pairing observation.
    pub async fn set_pairing_reader(
        &self,
        observation: PairingObservation,
        doc_id: &str,
        reason: PairingReadReason,
        agent: &identity::Did,
        grant: bool,
    ) -> Result<(), ConfigClientError> {
        self.change_owner_staged_pairing_reader(observation, doc_id, reason, agent, !grant)
            .await
    }
}

impl ConfigAccess {
    /// Apply an explicit native grant or revocation in the caller's transaction.
    pub async fn set_pairing_reader(
        &self,
        observation: PairingObservation,
        doc_id: &str,
        reason: PairingReadReason,
        agent: &identity::Did,
        grant: bool,
    ) -> Result<(), ConfigClientError> {
        self.transact_write("p2p.set_pairing_reader", |txn| {
            Box::pin(async move {
                txn.set_pairing_reader(observation, doc_id, reason, agent, grant)
                    .await
            })
        })
        .await
    }

    /// [`Self::set_pairing_reader`] for the client this embedded access names,
    /// whose signed pairing-reader command the runtime has authenticated. The
    /// runtime holds no key for that client, so the transaction carries the
    /// client's DID without a signer; the change is still decided by the same
    /// `authorized` and owner checks, as that client.
    pub async fn set_verified_client_pairing_reader(
        &self,
        observation: PairingObservation,
        doc_id: &str,
        reason: PairingReadReason,
        agent: &identity::Did,
        grant: bool,
    ) -> Result<(), ConfigClientError> {
        self.transact_verified_client_pairing_reader(
            "p2p.set_verified_client_pairing_reader",
            |txn| {
                Box::pin(async move {
                    txn.set_pairing_reader(observation, doc_id, reason, agent, grant)
                        .await
                })
            },
        )
        .await
    }
}
