use gents_protocol::schemas::HomePolicyIds;

use super::apply_txn::TxnBackend;
use crate::config_client::{ConfigApplyTxn, Result};
use crate::local::document_acp::{home_policy_ids, provider_of, DocumentAcp};

impl ConfigApplyTxn<'_> {
    /// The provider of the home this transaction runs on.
    pub(crate) async fn home_provider(&self) -> Result<DocumentAcp> {
        match &self.backend {
            TxnBackend::Embedded { node, .. } => Ok(provider_of(node)),
            TxnBackend::Http { endpoint, .. } => endpoint.home_provider().await,
        }
    }

    /// The IDs the built-in policies have in the home this transaction runs
    /// on.
    pub async fn home_policy_ids(&self) -> Result<&'static HomePolicyIds> {
        Ok(home_policy_ids(self.home_provider().await?))
    }
}
