//! `RemoteP2pAdmin` trait definition, error types, and value types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::templates::PairingFilters;

/// Errors any `RemoteP2pAdmin` implementation can produce.
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

    #[error("local error: {0}")]
    LocalError(String),
}

pub type RemoteP2pAdminResult<T> = Result<T, RemoteP2pAdminError>;

/// Subset of remote replicator info needed by the reconcile diff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteReplicator {
    pub id: Option<String>,
    pub collections: Vec<String>,
    pub address: Option<String>,
}

/// P2P document subscription record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteP2pDocument {
    pub collection: String,
    pub doc_id: String,
}

/// Talking-to-a-remote-peer admin surface.
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

    async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>>;

    async fn add_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()>;

    async fn delete_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()>;

    async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>>;

    async fn add_p2p_documents(&self, doc_ids: &[String]) -> RemoteP2pAdminResult<()>;

    async fn delete_p2p_documents(&self, doc_ids: &[String]) -> RemoteP2pAdminResult<()>;

    async fn sync_documents(
        &self,
        collection_name: &str,
        doc_ids: &[String],
        timeout: Option<std::time::Duration>,
    ) -> RemoteP2pAdminResult<()>;

    async fn sync_collection_versions(
        &self,
        version_ids: &[String],
        timeout: Option<std::time::Duration>,
    ) -> RemoteP2pAdminResult<()>;

    async fn sync_branchable_collection(
        &self,
        collection_id: &str,
        timeout: Option<std::time::Duration>,
    ) -> RemoteP2pAdminResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_trait_object_safe(_: &dyn RemoteP2pAdmin) {}

    #[test]
    fn error_classes_are_distinct() {
        use RemoteP2pAdminError::*;
        let errors = [
            RpcTimeout,
            RpcError("x".into()),
            RemoteNotFound("c".into()),
            RemoteUnauthorized,
            LocalError("y".into()),
        ];

        for e in &errors {
            match e {
                RpcTimeout => {}
                RpcError(_) => {}
                RemoteNotFound(_) => {}
                RemoteUnauthorized => {}
                LocalError(_) => {}
            }
        }
    }
}
