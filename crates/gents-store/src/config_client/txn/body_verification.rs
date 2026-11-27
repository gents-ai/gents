use super::apply_txn::TxnBackend;
use crate::collection::Collection;
use crate::config_client::body_verification::MergeCheck;
use crate::config_client::{ConfigApplyTxn, ConfigClientError, Result};
use crate::local::document_acp::{node_policy_ids, provider_of, DocumentAcp};

impl ConfigApplyTxn<'_> {
    /// The policy this node's home binds its configuration documents to.
    /// Within an account it is the home an Agent's owner issues placements
    /// from.
    pub fn home_policy(&self) -> Option<&'static str> {
        match &self.backend {
            TxnBackend::Embedded { node, .. } => {
                Some(node_policy_ids(node).id(gents_protocol::schemas::BuiltInPolicy::Household))
            }
            TxnBackend::Http { .. } => None,
        }
    }

    /// How the node this transaction runs in treats a replicated change. A
    /// transaction over HTTP is a client's, which merges nothing.
    ///
    /// External premise: DefraDB turns its strict check of replicated changes
    /// on exactly when the node's document access control is Vera, and offers
    /// no accessor for the setting itself.
    pub fn merge_check(&self) -> MergeCheck {
        match (&self.backend, self.home_policy()) {
            (TxnBackend::Embedded { node, .. }, Some(home))
                if provider_of(node) == DocumentAcp::Vera =>
            {
                MergeCheck::Strict {
                    home: home.to_owned(),
                }
            }
            _ => MergeCheck::Unchecked,
        }
    }

    /// Whether `doc_id` of `collection` is registered under the policy
    /// `home`. The provider answers from its own state, outside this
    /// transaction's snapshot.
    pub async fn document_registered(
        &self,
        home: &str,
        collection: Collection,
        doc_id: &str,
    ) -> Result<bool> {
        let TxnBackend::Embedded { node, .. } = &self.backend else {
            return Ok(false);
        };
        let Some(policy) = node
            .get_collection(collection.graphql_type())?
            .and_then(|collection| collection.policy)
        else {
            return Ok(false);
        };
        if policy.id != home {
            return Ok(false);
        }
        node.document_acp()
            .is_doc_registered(&policy.id, &policy.resource_name, doc_id)
            .await
            .map_err(|error| ConfigClientError::Node(Box::new(error.into())))
    }
}
