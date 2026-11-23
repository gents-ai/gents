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

fn publisher_input(device: &Did) -> serde_json::Value {
    serde_json::json!({"node_did": device.as_str(), "enabled": true})
}

fn publisher_schema() -> Result<schema::CollectionVersion, ConfigClientError> {
    use gents_protocol::schemas::{HomePolicyIds, NODE, NODE_NAME};
    super::client_signed_agent::signing_schema(
        NODE,
        NODE_NAME,
        HomePolicyIds::embedded_vera_transport_v2(),
    )
}

/// Verify the original Node registration independently of the live publisher
/// permission, which an account can delegate to several devices.
pub(crate) fn verified_publisher_node(
    evidence: serde_json::Value,
    device: &Did,
    account: &Did,
) -> Result<String, ConfigClientError> {
    let graph = db::block::builder::VerifiedClientCreate::verify(
        &storage::Crdts::default(),
        &publisher_schema()?,
        &publisher_input(device),
        serde_json::from_value(evidence)?,
    )
    .map_err(ConfigClientError::ClientAgentGraph)?;
    if graph.signer_did() != account.as_str() {
        return Err(ConfigClientError::SharedDeviceAdmission);
    }
    Ok(graph.doc_id().to_owned())
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

    pub async fn device_registration_evidence(
        &self,
        device: &Did,
        node: &str,
    ) -> Result<serde_json::Value, ConfigClientError> {
        let Self::Graphql(endpoint) = self else {
            return Err(ConfigClientError::ClientAgentTransportRequired);
        };
        let signer = endpoint.held_client_signing_capability()?.clone();
        let input = publisher_input(device);
        let expected = node.to_owned();
        tokio::task::spawn_blocking(move || {
            let graph = db::block::builder::ClientCreateSignatures::from_input(
                &storage::Crdts::default(),
                &publisher_schema()?,
                &input,
                &signer.config,
            )
            .map_err(ConfigClientError::ClientAgentGraph)?;
            if graph.doc_id() != expected {
                return Err(ConfigClientError::SharedDeviceAdmission);
            }
            Ok(serde_json::to_value(graph.evidence())?)
        })
        .await
        .map_err(ConfigClientError::ClientAgentGraphJoin)?
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
                            &serde_json::json!({"input":publisher_input(device)}), NODE,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_registration_binds_the_device_and_the_account() {
        let signer = crate::test_support::config_signer();
        let other = crate::test_support::config_signer();
        let account = Did::new(signer.did()).unwrap();
        let device = Did::new(other.did()).unwrap();
        let signed = db::block::builder::ClientCreateSignatures::from_input(
            &storage::Crdts::default(),
            &publisher_schema().unwrap(),
            &publisher_input(&device),
            &signer.config,
        )
        .unwrap();
        let evidence = serde_json::to_value(signed.evidence()).unwrap();
        assert_eq!(
            verified_publisher_node(evidence.clone(), &device, &account).unwrap(),
            signed.doc_id()
        );
        assert!(verified_publisher_node(evidence.clone(), &account, &account).is_err());
        assert!(verified_publisher_node(evidence, &device, &device).is_err());
    }
}
