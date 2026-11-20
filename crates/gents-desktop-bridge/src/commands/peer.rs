use super::util::require_trimmed;
use crate::client_runtime::ClientRuntime;
use crate::types::ClientAccessRequestView;
use crate::types::RuntimeAccessConnectRequest;
use gents_protocol::runtime_connection::RuntimeConnection;
use gents_store::client_error::BridgeError;

pub async fn forget_runtime(core: &ClientRuntime, node_did: String) -> anyhow::Result<bool> {
    core.forget_runtime(require_trimmed("node_did", node_did)?.as_str())
        .await
}

pub async fn rename_runtime(
    core: &ClientRuntime,
    node_did: String,
    label: String,
) -> anyhow::Result<()> {
    core.rename_runtime(
        &require_trimmed("node_did", node_did)?,
        &require_trimmed("label", label)?,
    )
    .await
}

pub async fn request_runtime_access(
    core: &ClientRuntime,
    request: RuntimeAccessConnectRequest,
) -> Result<ClientAccessRequestView, BridgeError> {
    let address = require_trimmed("server_address", request.server_address)
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let status = gents::client_runtime::fetch_runtime_connection_payload(&address)
        .await
        .map_err(BridgeError::classify_transport_error)?;
    let node_did = status
        .get("node_did")
        .and_then(serde_json::Value::as_str)
        .filter(|did| !did.is_empty())
        .ok_or_else(|| BridgeError::untyped("the runtime did not identify its Node"))?;
    let label = status
        .get("node_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(node_did);
    let graphql = gents::client_runtime::graphql_endpoint_for_desktop_access(
        &gents::client_runtime::runtime_status_url(&address)
            .map_err(|error| BridgeError::untyped(error.to_string()))?,
    )
    .ok_or_else(|| BridgeError::untyped("the runtime address is invalid"))?;
    core.request_runtime_access(RuntimeConnection {
        node_did: node_did.to_owned(),
        label: label.to_owned(),
        graphql,
        local_node_home: None,
        access_request: None,
        access_state: None,
    })
    .await
    .map(Into::into)
    .map_err(|error| BridgeError::untyped(format!("{error:#}")))
}
