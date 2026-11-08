use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::templates::PairingFilters;

#[derive(Debug, Error)]
pub enum RemoteP2pAdminError {
    #[error("remote admin RPC timed out")]
    RpcTimeout,

    #[error("remote admin RPC failed: {0}")]
    RpcError(String),

    #[error("remote reports not-found: {0}")]
    RemoteNotFound(String),

    #[error("remote admin rejected request as unauthorized")]
    RemoteUnauthorized,

    #[error("collection {0:?} is runtime-local; use authenticated GraphQL access")]
    RuntimeLocalCollection(String),

    #[error("local error: {0}")]
    LocalError(String),
}

pub type RemoteP2pAdminResult<T> = Result<T, RemoteP2pAdminError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteReplicator {
    pub id: Option<String>,
    pub collections: Vec<String>,
    pub address: Option<String>,
    #[serde(default)]
    pub filters: Option<defra_p2p_adapter::ReplicationFilters>,
}

#[async_trait]
pub trait RemoteP2pAdmin: Send + Sync {
    async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>>;

    async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>>;

    async fn connect(&self, addresses: &[String]) -> RemoteP2pAdminResult<()>;

    async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>>;

    async fn add_replicator(
        &self,
        addresses: &[String],
        collections: &[String],
        filters: &PairingFilters,
    ) -> RemoteP2pAdminResult<()>;

    async fn delete_replicator(&self, id: &str, collections: &[String])
        -> RemoteP2pAdminResult<()>;

    async fn resolve_collection_id(&self, name: &str) -> RemoteP2pAdminResult<Option<String>>;

    async fn resolve_collection_name(&self, id: &str) -> RemoteP2pAdminResult<Option<String>>;
}
