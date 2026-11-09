use super::{ConfigAccess, ConfigApplyTxn, ConfigClientError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingObservation {
    Desired,
    Applied,
}

impl PairingObservation {
    pub fn collection(self) -> &'static str {
        match self {
            Self::Desired => "PeerPairingDesired",
            Self::Applied => "PeerPairingApplied",
        }
    }
}

/// An explicit native grant for one physical pairing observation. Enabling a
/// tool or choosing an Agent does not itself publish or revoke this grant.
/// Either reason grants native read access to the complete document; the
/// relation records independent revocation reasons, not field-level access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingReadReason {
    RouteAdmission,
    ToolView,
}

impl PairingReadReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RouteAdmission => "route-admission",
            Self::ToolView => "tool-view",
        }
    }

    fn relation(self) -> &'static str {
        match self {
            Self::RouteAdmission => "route_reader",
            Self::ToolView => "tool_reader",
        }
    }
}

impl ConfigApplyTxn<'_> {
    /// Stage a reader change with other authorized configuration writes.
    /// Native ACP authorizes the caller against the exact document; this does
    /// not authorize enrollment or permit writing the pairing observation.
    pub async fn set_pairing_reader(
        &self,
        observation: PairingObservation,
        doc_id: &str,
        reason: PairingReadReason,
        agent: &identity::Did,
        grant: bool,
    ) -> Result<(), ConfigClientError> {
        if grant {
            self.add_actor_relationship(observation.collection(), doc_id, reason.relation(), agent)
                .await
        } else {
            self.delete_actor_relationship(
                observation.collection(),
                doc_id,
                reason.relation(),
                agent,
            )
            .await
        }
    }
}

impl ConfigAccess {
    /// Apply an explicit native grant or revocation in the caller's transaction.
    /// The native owner or a reader manager is responsible for this operation.
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
}
