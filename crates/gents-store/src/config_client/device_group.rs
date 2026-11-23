use gents_protocol::schemas::{BuiltInPolicy, ACCOUNT_DEVICE_GROUP, ACCOUNT_DEVICE_GROUP_NAME};
use identity::Did;

use super::{assignments::AssignmentReadError, ConfigAccess, ConfigClientError};

#[derive(Debug, thiserror::Error)]
pub enum DeviceGroupReadError {
    #[error(transparent)]
    Assignments(#[from] AssignmentReadError),
    #[error("device group discovery requires an authenticated device")]
    MissingIdentity,
    #[error("the account has assigned this device to multiple groups")]
    Ambiguous,
}

impl ConfigAccess {
    /// Discover the device's group from a certified membership grant by the
    /// configured account. An empty assignment set means the device is unadmitted.
    pub async fn assigned_device_group(
        &self,
        account: &Did,
    ) -> Result<Option<String>, DeviceGroupReadError> {
        let node = self.local_node().ok_or(AssignmentReadError::Unsupported)?;
        let subject = self
            .actor_did()
            .ok_or(DeviceGroupReadError::MissingIdentity)?;
        let assignments = self
            .certified_assignments(defra_node::AssignmentQuery {
                policy: crate::local::document_acp::node_policy_ids(node)
                    .id(BuiltInPolicy::Household)
                    .to_owned(),
                resource: "device_groups".into(),
                relation: "member".into(),
                subject,
                issuer: account.to_string(),
                not_before: None,
            })
            .await?;
        let mut groups = assignments.object_ids.into_iter();
        let group = groups.next();
        if groups.next().is_some() {
            return Err(DeviceGroupReadError::Ambiguous);
        }
        Ok(group)
    }

    /// Update the publisher grant on this device's Node together with its group
    /// membership. Only the account holding both objects may enroll the device.
    pub async fn set_shared_device_membership(
        &self,
        group: &str,
        device: &Did,
        admit: bool,
    ) -> Result<String, ConfigClientError> {
        use gents_protocol::schemas::{HomePolicyIds, NODE, NODE_NAME};

        if !matches!(self, ConfigAccess::Graphql(_))
            || device.is_wildcard()
            || self.home_policy_ids().await? != HomePolicyIds::embedded_vera_transport_v2()
        {
            return Err(ConfigClientError::SharedDeviceAdmission);
        }
        self.transact_write("config.shared_device_membership", |txn| {
            Box::pin(async move {
                let account = txn.actor_did().ok_or(ConfigClientError::SharedDeviceAdmission)?;
                let owned_by_account = |owners: &[query::owners::DocumentOwnerObservation], id: &str| {
                    matches!(owners, [entry] if entry.doc_id == id && entry.visible && entry.owner.as_ref().map(Did::as_str) == Some(account))
                };
                if !owned_by_account(&txn.document_owners(ACCOUNT_DEVICE_GROUP_NAME, &[group.to_owned()]).await?, group) {
                    return Err(ConfigClientError::SharedDeviceAdmission);
                }
                let response = txn.execute_with_variables(
                    "query($did: String!) { Node(filter: {node_did: {_eq: $did}}, limit: 2) { _docID } }",
                    &serde_json::json!({"did": device.as_str()}),
                ).await?;
                let rows = response["data"][NODE_NAME].as_array().ok_or(ConfigClientError::SharedDeviceAdmission)?;
                let node = match rows.as_slice() {
                    [row] => {
                        let id = row["_docID"].as_str().ok_or(ConfigClientError::SharedDeviceAdmission)?.to_owned();
                        if !owned_by_account(&txn.document_owners(NODE_NAME, std::slice::from_ref(&id)).await?, &id) {
                            return Err(ConfigClientError::SharedDeviceAdmission);
                        }
                        id
                    }
                    [] if admit => {
                        let created = txn.execute_client_signed_create(
                            "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                            &serde_json::json!({"input":{"node_did":device.as_str(), "enabled":true}}), NODE,
                        ).await?;
                        crate::graphql::created_doc_id(&created, NODE_NAME)?
                    }
                    _ => return Err(ConfigClientError::SharedDeviceAdmission),
                };
                if admit {
                    txn.add_actor_relationship(NODE_NAME, &node, "runtime_reader", device).await?;
                    txn.add_actor_relationship(NODE_NAME, &node, "transport_publisher", device).await?;
                    txn.add_actor_relationship(ACCOUNT_DEVICE_GROUP_NAME, group, "member", device).await?;
                } else {
                    txn.delete_actor_relationship(ACCOUNT_DEVICE_GROUP_NAME, group, "member", device).await?;
                    txn.delete_actor_relationship(NODE_NAME, &node, "transport_publisher", device).await?;
                    txn.delete_actor_relationship(NODE_NAME, &node, "runtime_reader", device).await?;
                }
                Ok(node)
            })
        }).await
    }

    /// Create the account's group as its authenticated owner and admit the
    /// first device. Keep the returned document ID when joining other devices.
    /// An account DID in a document field identifies no authority on its own.
    /// Subsequent admissions and revocations use the account's signed
    /// `add_actor_relationship` and `delete_actor_relationship` on `member`.
    pub async fn create_device_group(
        &self,
        account: &Did,
        first_device: &Did,
    ) -> Result<String, ConfigClientError> {
        if self.actor_did().as_deref() != Some(account.as_str()) {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        self.transact_write("config.create_device_group", |txn| {
            Box::pin(async move {
                let response = txn.execute_client_signed_create(
                    "mutation($input: AccountDeviceGroupMutationInputArg!) { create_AccountDeviceGroup(input: $input) { _docID } }",
                    &serde_json::json!({ "input": { "account_did": account.as_str() } }),
                    ACCOUNT_DEVICE_GROUP,
                ).await?;
                let id = crate::graphql::created_doc_id(&response, ACCOUNT_DEVICE_GROUP_NAME)?;
                txn.add_actor_relationship(ACCOUNT_DEVICE_GROUP_NAME, &id, "member", first_device).await?;
                Ok(id)
            })
        }).await
    }
}
