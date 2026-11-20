use std::sync::Arc;

use defra_node::Node as EmbeddedNode;

use gents_documents::backend_credential::{
    backend_credential_in_txn, BackendCredential, BackendCredentialError,
};

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
    let credential = backend_credential_in_txn(&access, credential_id).await?;
    if !credential.matches_binding(node_did, provider, endpoint, version) {
        return Err(BackendCredentialError::BindingMismatch);
    }
    Ok(credential)
}

/// Resolve only after the caller has authorized the selected metadata.
pub async fn resolve_runtime_backend_auth(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    provider: crate::backends::provider::BackendProviderKind,
    endpoint: &str,
    auth: &gents_documents::BackendAuth,
) -> anyhow::Result<gents_documents::BackendAuth> {
    let gents_documents::BackendAuth::Credential { id, version } = auth else {
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
