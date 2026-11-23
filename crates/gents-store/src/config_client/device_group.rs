use gents_protocol::schemas::ACCOUNT_DEVICE_GROUP_NAME;
use identity::Did;

use super::{ConfigAccess, ConfigClientError};

impl ConfigAccess {
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
