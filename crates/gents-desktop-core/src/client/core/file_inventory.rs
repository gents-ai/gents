use super::bootstrap::normalize_required;
use super::ClientCore;
use anyhow::{Context, Result};
use gents_protocol::file_inventory::{
    DirectFileInventoryRequest, FileInventoryObservation, FileInventoryReadAction,
    FILE_INVENTORY_OPERATOR_PATH,
};

impl ClientCore {
    pub async fn session_file_inventory(
        &self,
        node_did: &str,
        session_id: &str,
        action: FileInventoryReadAction,
    ) -> Result<Option<FileInventoryObservation>> {
        let node_did = normalize_required("node_did", node_did)?;
        let session_id = normalize_required("session_id", session_id)?;
        let endpoint = self
            .operator_observation_graphql(node_did)
            .context("Connect to the runtime to read file changes.")?;
        let mut url = reqwest::Url::parse(endpoint.url())?;
        let prefix = url
            .path()
            .strip_suffix("/api/v0/graphql")
            .context("runtime endpoint has no canonical GraphQL path")?;
        url.set_path(&format!("{prefix}{FILE_INVENTORY_OPERATOR_PATH}"));
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let response = endpoint
            .authorize(client.post(url))?
            .json(&DirectFileInventoryRequest {
                node_did: node_did.to_owned(),
                session_id: session_id.to_owned(),
                peer_id: self.local_peer_id().to_owned(),
                action,
            })
            .send()
            .await
            .map_err(|error| {
                crate::error::BridgeError::runtime_http_error(
                    error,
                    "Runtime file inventory is unavailable".to_owned(),
                )
            })?
            .error_for_status()
            .map_err(|error| {
                crate::error::BridgeError::runtime_http_error(
                    error,
                    "Runtime file inventory was rejected".to_owned(),
                )
            })?;
        response
            .json()
            .await
            .context("invalid runtime file inventory response")
    }
}
