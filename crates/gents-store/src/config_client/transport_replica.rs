use std::sync::Arc;

use acp::{ReplicaGrant, ReplicaGroup, Subject};
use defra_node::{ReplicaOwnership, ReplicaRegistration};
use gents_protocol::schemas::{
    identity_policy_resource, BuiltInPolicy, REPLICATED_PROTOCOL_COLLECTION_NAMES,
};
use identity::Did;

use super::{ConfigAccess, DeviceGroupReadError};

#[derive(Clone)]
pub(crate) struct TransportReplica(ReplicaGroup);

impl TransportReplica {
    pub(crate) async fn check_receiver(&self, node: &defra_node::Node) -> acp::Result<bool> {
        let provider = node.document_acp();
        let receiver = provider
            .own_actor()
            .ok_or(acp::Error::UnsupportedReplicaMembership)?;
        provider.replica_group_member(&self.0, &receiver).await
    }

    pub(crate) fn reader_manager(&self) -> Subject {
        Subject::entity_set(&self.0.resource, &self.0.object, &self.0.relation)
    }

    pub(crate) fn registration(self) -> ReplicaRegistration {
        ReplicaRegistration::new(
            REPLICATED_PROTOCOL_COLLECTION_NAMES.iter().copied(),
            Arc::new(self),
        )
    }
}

#[async_trait::async_trait]
impl ReplicaOwnership for TransportReplica {
    fn is_own_actor(&self, _collection: &str, actor: &Did) -> bool {
        actor == &self.0.owner
    }

    async fn allows_foreign_registration(
        &self,
        collection: &schema::CollectionVersion,
        creator: &Did,
        provider: &dyn acp::DocumentACP,
    ) -> acp::Result<bool> {
        let expected_resource = identity_policy_resource(&collection.name);
        if !collection.is_branchable
            || !collection.policy.as_ref().is_some_and(|policy| {
                policy.id == self.0.policy
                    && Some(policy.resource_name.as_str()) == expected_resource
            })
        {
            return Ok(false);
        }
        let member = provider.replica_group_member(&self.0, creator).await?;
        Ok(!member)
    }

    fn creator_grants(&self, _collection: &str, creator: &Did, _doc: &str) -> Vec<ReplicaGrant> {
        vec![
            ReplicaGrant::new("reader_manager", self.reader_manager()),
            ReplicaGrant::new("writer", Subject::entity(creator.clone())),
        ]
    }
}

impl ConfigAccess {
    /// Build the receiving node's registration rules from its account's certified
    /// group. Defra rechecks membership before each foreign registration.
    pub(crate) async fn transport_replica(
        &self,
        account: &Did,
    ) -> Result<Option<TransportReplica>, DeviceGroupReadError> {
        let Some(object) = self.assigned_device_group(account).await? else {
            return Ok(None);
        };
        let node = self
            .local_node()
            .ok_or(super::assignments::AssignmentReadError::Unsupported)?;
        let group = ReplicaGroup {
            owner: account.clone(),
            policy: crate::local::document_acp::node_policy_ids(node)
                .id(BuiltInPolicy::Household)
                .to_owned(),
            resource: "device_groups".into(),
            relation: "member".into(),
            object,
        };
        Ok(Some(TransportReplica(group)))
    }
}
