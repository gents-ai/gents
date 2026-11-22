use gents_protocol::owner_staged::{OwnerStagedChange, SelectedConfiguration};

use super::apply_txn::TxnBackend;
use crate::config_client::{ConfigApplyTxn, ConfigClientError, Result};
use crate::local::document_acp::{configuration_grant_route, ConfigurationGrantRoute, DocumentAcp};

impl ConfigApplyTxn<'_> {
    /// Grant or revoke `agent` `reader` of the selected configuration document
    /// `doc_id`, as the home's provider routes a change only the document's
    /// owner manages (`Identity.OwnerStaged.configurationGrantRoute`), the
    /// provider being the one the runtime authenticates the client by.
    pub async fn change_configuration_reader(
        &self,
        configuration: SelectedConfiguration,
        doc_id: &str,
        agent: &identity::Did,
        grant: bool,
    ) -> Result<()> {
        let provider = self.authenticated_provider().await?;
        let actor_owns = provider == DocumentAcp::Local || self.actor_owns(doc_id).await?;
        match configuration_grant_route(provider, actor_owns) {
            ConfigurationGrantRoute::Direct if grant => {
                self.add_actor_relationship(configuration.collection(), doc_id, "reader", agent)
                    .await
            }
            ConfigurationGrantRoute::Direct => {
                self.delete_actor_relationship(configuration.collection(), doc_id, "reader", agent)
                    .await
            }
            ConfigurationGrantRoute::OwnerStaged => {
                self.request_owner_staged(
                    &OwnerStagedChange::ConfigurationReader {
                        configuration,
                        doc_id: doc_id.to_owned(),
                        agent_did: agent.to_string(),
                    },
                    grant,
                )
                .await
            }
        }
    }

    /// Over HTTP the runtime's owner-staged route decides ownership. In the
    /// runtime's process the owner is read outside the transaction, which an
    /// external provider cannot answer within one; a document with no
    /// registered owner yet was created in this transaction by its only
    /// identity.
    async fn actor_owns(&self, doc_id: &str) -> Result<bool> {
        let TxnBackend::Embedded { node, identity, .. } = &self.backend else {
            return Ok(false);
        };
        let owner = node
            .document_acp()
            .get_doc_owner(
                crate::local::document_acp::node_policy_ids(node)
                    .id(gents_protocol::schemas::BuiltInPolicy::Household),
                "configuration",
                doc_id,
            )
            .await
            .map_err(|error| ConfigClientError::Node(Box::new(error.into())))?;
        Ok(match owner {
            Some(owner) => identity.as_ref() == Some(&owner),
            None => true,
        })
    }
}
