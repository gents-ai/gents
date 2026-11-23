use gents_protocol::schemas::{BuiltInPolicy, ACCOUNT_DEVICE_GROUP_NAME};
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
                let response = txn.execute_with_variables(
                    "mutation($account: String!) { create_AccountDeviceGroup(input: {account_did: $account}) { _docID } }",
                    &serde_json::json!({ "account": account.as_str() }),
                ).await?;
                let id = crate::graphql::created_doc_id(&response, ACCOUNT_DEVICE_GROUP_NAME)?;
                txn.add_actor_relationship(ACCOUNT_DEVICE_GROUP_NAME, &id, "member", first_device).await?;
                Ok(id)
            })
        }).await
    }
}
