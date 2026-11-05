use serde_json::Value;
use std::time::Duration;

use super::{graphql, ConfigClientError, GraphqlEndpoint, Result};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CreateRoute {
    Native,
    Signed,
    Refuse,
}

pub(super) fn create_route(intent: bool, agent: bool, create: bool, http: bool) -> CreateRoute {
    if !intent || !agent || !create {
        CreateRoute::Native
    } else if http {
        CreateRoute::Signed
    } else {
        CreateRoute::Refuse
    }
}

pub(super) async fn execute(
    endpoint: &GraphqlEndpoint,
    txn_id: &str,
    mutation: &str,
    variables: &Value,
) -> Result<Value> {
    let actor = endpoint
        .principal_did
        .as_deref()
        .filter(|_| endpoint.delegated_authorization.is_none())
        .ok_or(ConfigClientError::ClientAgentSignerRequired)?;
    let signer = defra_core::signing::get_identity(actor)
        .ok_or(ConfigClientError::ClientAgentSignerRequired)?;
    if !signer.has_local_private_key()
        || crate::store::identity::commit_signer_identity_for_did(actor)? != signer.public_key_hex
    {
        return Err(ConfigClientError::ClientAgentSignerRequired);
    }
    let input = variables
        .get("input")
        .cloned()
        .ok_or(ConfigClientError::VariablesNotObject)?;
    let mut graph = tokio::task::spawn_blocking(move || {
        let mut schemas = query::parse_sdl(gents_protocol::schemas::AGENT)?;
        if schemas.len() != 1 || schemas[0].name != gents_protocol::schemas::AGENT_NAME {
            return Err(ConfigClientError::ClientAgentTransportRequired);
        }
        db::block::builder::ClientCreateSignatures::from_input(
            &storage::Crdts::default(),
            &schemas.remove(0),
            &input,
            &signer,
        )
        .map_err(ConfigClientError::ClientAgentGraph)
    })
    .await
    .map_err(ConfigClientError::ClientAgentGraphJoin)??;
    let mut url =
        reqwest::Url::parse(endpoint.url()).map_err(|source| ConfigClientError::ParseEndpoint {
            url: endpoint.url().to_owned(),
            source,
        })?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err(ConfigClientError::ClientAgentTransportRequired),
    };
    if !url.path().ends_with("/graphql") || url.set_scheme(scheme).is_err() {
        return Err(ConfigClientError::ClientAgentTransportRequired);
    }
    url.set_path(&format!("{}/client-sign", url.path()));
    let authorization = endpoint
        .authorization()?
        .ok_or(ConfigClientError::ClientAgentSignerRequired)?;
    let request = defra_http::client_signed_create::websocket_request(url.as_str(), &authorization)
        .map_err(ConfigClientError::ClientAgentSigning)?;
    let query = query::QueryRequest::new(mutation).with_variables(variables.clone());
    let response = tokio::time::timeout(Duration::from_secs(90), async {
        let (socket, _) = tokio_tungstenite::connect_async_with_config(
            request,
            Some(defra_http::client_signed_create::websocket_config()),
            false,
        )
        .await
        .map_err(|error| ConfigClientError::ClientAgentConnection(Box::new(error)))?;
        defra_http::client_signed_create::execute_with_socket(
            socket,
            query,
            Some(txn_id),
            &mut graph,
        )
        .await
        .map_err(ConfigClientError::ClientAgentSigning)
    })
    .await
    .map_err(ConfigClientError::ClientAgentTimeout)??;
    graphql::validate_txn_response(serde_json::to_value(response)?)
}
