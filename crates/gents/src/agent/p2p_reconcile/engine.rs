mod desired;
mod enrollment;
mod reconcile;
mod remote_topology;
mod rows;
mod store;
mod sweep;

use crate::agent::p2p_reconcile::error::P2pReconcileError;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use async_trait::async_trait;

use super::{DiffOp, PairingApplied, PairingDesired};

pub use desired::merge_layered_desired;
pub use reconcile::{
    observe_owned_pairing_live_matches, reconcile_peer_tick, update_applied_after_success,
};
pub use remote_topology::teardown_owned_replicators_at_endpoint;
pub use store::GraphqlPairingStateStore;
pub use sweep::run_pairing_reconciler;

#[cfg(test)]
use desired::{data_plane_desired_from_pairing_row, desired_from_pairing_row};
#[cfg(test)]
use rows::PairingStateRow;

type Result<T, E = P2pReconcileError> = std::result::Result<T, E>;

pub const PAIRING_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
pub const MAX_CONCURRENT_PEER_PREPARATIONS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentEndpointEntry {
    /// Durable materialization key, not a transport peer id.
    pub desired_id: String,
    pub peer_id: String,
    pub node_did: String,
    pub address: String,
    pub request_digest: String,
    pub authorization_sequence: u64,
    pub authorization_expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentRouteGeneration {
    pub member_did: String,
    pub member_peer: String,
    pub member_ticket: String,
    pub request_digest: String,
    pub authorization_sequence: u64,
    pub authorization_expires_at: String,
}

impl From<&super::enrollment_reconcile::EnrollmentAuthorizationFence>
    for EnrollmentRouteGeneration
{
    fn from(fence: &super::enrollment_reconcile::EnrollmentAuthorizationFence) -> Self {
        Self {
            member_did: fence.member_did.clone(),
            member_peer: fence.member_peer.clone(),
            member_ticket: fence.member_ticket.clone(),
            request_digest: fence.request_digest.clone(),
            authorization_sequence: fence.authorization_sequence,
            authorization_expires_at: fence.authorization_expires_at.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedPairingDesired {
    pub state: Option<PairingDesired>,
    pub enrollment_generation: Option<EnrollmentRouteGeneration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTickOutcome {
    pub peer_id: String,
    pub ops_applied: Vec<DiffOp>,
    pub replayed_replicators: Vec<String>,
    pub desired_read_failed: bool,
    pub peer_active: bool,
    pub live_route_matches: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedPairingApplied {
    pub doc_id: Option<String>,
    pub state: PairingApplied,
    pub duplicate_doc_ids: Vec<String>,
}

#[async_trait]
pub trait PairingStateStore: Send + Sync {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>>;

    async fn load_desired_with_authority(&self, peer_id: &str) -> Result<LoadedPairingDesired> {
        Ok(LoadedPairingDesired {
            state: self.load_desired(peer_id).await?,
            enrollment_generation: None,
        })
    }

    async fn enrollment_generation_is_current(
        &self,
        _generation: &EnrollmentRouteGeneration,
    ) -> Result<bool> {
        Ok(true)
    }

    async fn load_applied(&self, peer_id: &str) -> Result<LoadedPairingApplied>;

    async fn load_applied_many(
        &self,
        peer_ids: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, LoadedPairingApplied>> {
        let mut applied = BTreeMap::new();
        for peer_id in peer_ids {
            applied.insert(peer_id.clone(), self.load_applied(peer_id).await?);
        }
        Ok(applied)
    }

    async fn persist_applied(&self, peer_id: &str, applied: &LoadedPairingApplied) -> Result<()>;

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>>;

    async fn begin_sweep(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
