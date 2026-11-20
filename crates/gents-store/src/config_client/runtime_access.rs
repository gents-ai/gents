use chrono::Utc;
use gents_protocol::client_access::{
    ClientAccessReceipt, ClientAccessRequest, DecideClientAccess, DECIDE_PATH, REQUEST_PATH,
};

use super::{ConfigClientError, GraphqlEndpoint};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeAccessError {
    #[error(fmt = gents_protocol::error_chain::held)]
    Configuration(#[from] ConfigClientError),
    #[error("the selected runtime refused this client access operation")]
    Refused,
    #[error(
        "the client access request expired before the runtime retained it; request access again"
    )]
    ExpiredUnretained,
    #[error("the runtime returned HTTP {0} for the client access operation")]
    Status(reqwest::StatusCode),
    #[error("sending the client access operation")]
    Http(#[from] reqwest::Error),
    #[error("encoding the client access operation")]
    Json(#[from] serde_json::Error),
    #[error("the client access receipt does not match the signed request")]
    Binding,
}

impl GraphqlEndpoint {
    pub async fn prepare_runtime_access_request(
        &self,
        node_did: &str,
        request_id: String,
        label: String,
    ) -> Result<ClientAccessRequest, RuntimeAccessError> {
        let capability = self.held_client_signing_capability()?;
        let now = Utc::now();
        let mut request = ClientAccessRequest {
            request_id,
            node_did: node_did.to_owned(),
            client_did: capability.did().to_owned(),
            label,
            issued_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
            signature: Vec::new(),
        };
        request.signature = capability.sign(request.signing_payload()?).await?;
        Ok(request)
    }

    pub async fn request_runtime_access(
        &self,
        request: &ClientAccessRequest,
    ) -> Result<ClientAccessReceipt, RuntimeAccessError> {
        if self.held_client_signing_capability()?.did() != request.client_did {
            return Err(RuntimeAccessError::Binding);
        }
        let receipt = self.send_runtime_access(REQUEST_PATH, request).await?;
        if receipt.request_id != request.request_id
            || receipt.node_did != request.node_did
            || receipt.client_did != request.client_did
        {
            return Err(RuntimeAccessError::Binding);
        }
        Ok(receipt)
    }

    pub async fn decide_runtime_access(
        &self,
        decision: &DecideClientAccess,
    ) -> Result<ClientAccessReceipt, RuntimeAccessError> {
        let receipt = self.send_runtime_access(DECIDE_PATH, decision).await?;
        if receipt.request_id != decision.request_id || receipt.node_did != decision.node_did {
            return Err(RuntimeAccessError::Binding);
        }
        Ok(receipt)
    }

    async fn send_runtime_access(
        &self,
        path: &str,
        input: &impl serde::Serialize,
    ) -> Result<ClientAccessReceipt, RuntimeAccessError> {
        let base = self.url().strip_suffix("/api/v0/graphql").ok_or_else(|| {
            ConfigClientError::GraphqlApiBase {
                graphql: self.to_string(),
            }
        })?;
        let client = super::runtime_endpoints::discovery_client().await?;
        let url = format!("{base}{path}");
        let response = self
            .send_resolving(|endpoint| Ok(endpoint.authorize(client.post(&url))?.json(input)))
            .await??;
        match response.status() {
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                Err(RuntimeAccessError::Refused)
            }
            reqwest::StatusCode::GONE if path == REQUEST_PATH => {
                Err(RuntimeAccessError::ExpiredUnretained)
            }
            status if status.is_success() => Ok(response.json().await?),
            status => Err(RuntimeAccessError::Status(status)),
        }
    }
}
