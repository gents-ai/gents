use gents_protocol::schemas::HomePolicyIds;

use super::apply_txn::TxnBackend;
use crate::config_client::{ConfigApplyTxn, Result};
use crate::local::document_acp::{node_policy_ids, provider_of, DocumentAcp};

impl ConfigApplyTxn<'_> {
    /// The provider as the runtime authenticates this transaction's client
    /// over HTTP; the node's provider for one in the runtime's process.
    pub(crate) async fn authenticated_provider(&self) -> Result<DocumentAcp> {
        match &self.backend {
            TxnBackend::Embedded { node, .. } => Ok(provider_of(node)),
            TxnBackend::Http { endpoint, .. } => endpoint.authenticated_provider().await,
        }
    }

    /// The IDs the built-in policies have in the home this transaction runs
    /// on.
    pub async fn home_policy_ids(&self) -> Result<&'static HomePolicyIds> {
        match &self.backend {
            TxnBackend::Embedded { node, .. } => Ok(node_policy_ids(node)),
            TxnBackend::Http { endpoint, .. } => endpoint.home_policy_ids().await,
        }
    }
}
