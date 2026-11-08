use std::sync::Arc;

use defra_node::Node as EmbeddedNode;

use crate::documents::backend_credential::{BackendCredential, BackendCredentialError};

/// The caller must first authorize the initiating actor's metadata selection.
/// This runtime-only lookup never substitutes another credential or destination.
pub(crate) async fn resolve_backend_credential(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    credential_id: &str,
    provider: &str,
    endpoint: &str,
    version: &str,
) -> Result<BackendCredential, BackendCredentialError> {
    let access = super::rows::runtime_credential_access(node, node_did)
        .map_err(BackendCredentialError::Read)?;
    let id = crate::store::graphql::escape_graphql_string(credential_id);
    let query = format!(
        r#"{{ BackendCredential(filter: {{credential_id: {{_eq: "{id}"}}}}, limit: 2) {{ credential_id node_did provider_kind endpoint version secret }} }}"#
    );
    let response = access
        .execute(&query)
        .await
        .map_err(|error| BackendCredentialError::Read(error.into()))?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "BackendCredential");
    if rows.len() != 1 {
        return Err(BackendCredentialError::MissingOrAmbiguous);
    }
    let credential: BackendCredential =
        serde_json::from_value(rows[0].clone()).map_err(|_| BackendCredentialError::Invalid)?;
    if credential.credential_id != credential_id
        || !credential.matches_binding(node_did, provider, endpoint, version)
    {
        return Err(BackendCredentialError::BindingMismatch);
    }
    Ok(credential)
}

/// Resolve only after the caller has authorized the selected metadata.
pub(crate) async fn resolve_runtime_backend_auth(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    provider: crate::inference::backends::provider::BackendProviderKind,
    endpoint: &str,
    auth: &crate::documents::BackendAuth,
) -> anyhow::Result<crate::documents::BackendAuth> {
    let crate::documents::BackendAuth::Credential { id, version } = auth else {
        return Ok(auth.clone());
    };
    let secret =
        resolve_backend_credential(node, node_did, id, provider.as_str(), endpoint, version)
            .await?;
    let resolved = secret.secret.into_backend_auth();
    anyhow::ensure!(
        resolved.suits_provider(provider),
        "backend credential is incompatible with the provider"
    );
    Ok(resolved)
}
