use std::sync::Arc;

use defra_node::Node as EmbeddedNode;

use crate::auth::credential::OAuthRefreshKind;
use crate::backends::provider::BackendProviderOauthExt;
use gents_documents::InferenceBackend;

pub struct OAuthProbeContext<'a> {
    pub node: Arc<EmbeddedNode>,
    pub node_did: &'a str,
}

#[derive(Debug, thiserror::Error)]
#[error("{context}: {source:#}")]
pub(crate) struct OAuthProbeError {
    context: String,
    #[source]
    pub(super) source: crate::auth::credential::OAuthCredentialError,
}

pub(crate) async fn oauth_credential_for_probe(
    context: &OAuthProbeContext<'_>,
    backend: &InferenceBackend,
) -> std::result::Result<crate::auth::credential::OAuthCredential, OAuthProbeError> {
    let provider = backend
        .provider_kind
        .oauth_provider()
        .ok_or_else(|| OAuthProbeError {
            context: format!(
                "resolving OAuth credential for backend {}",
                backend.backend_id
            ),
            source: anyhow::anyhow!("backend kind has no OAuth provider").into(),
        })?;
    let (bearer, mut credential) = crate::auth::http::bootstrap_oauth_client_typed(
        context.node.clone(),
        context.node_did,
        provider,
        oauth_refresh_kind(backend.provider_kind),
        oauth_product(backend.provider_kind),
        crate::auth::credential::account_pick(&backend.auth)
            .unwrap_or(crate::auth::credential::AccountPick::Reference(None)),
    )
    .await
    .map_err(|source| OAuthProbeError {
        context: format!(
            "resolving OAuth credential for backend {} provider {provider}",
            backend.backend_id
        ),
        source,
    })?;
    credential.access_token =
        bearer
            .current_bearer_typed()
            .await
            .map_err(|source| OAuthProbeError {
                context: format!(
                    "resolving current bearer for backend {} provider {provider}",
                    backend.backend_id
                ),
                source,
            })?;
    tracing::debug!(
        backend_id = %backend.backend_id,
        provider,
        "oauth credential probe ok: bearer current through the credential owner"
    );
    Ok(credential)
}

pub(crate) fn oauth_refresh_kind(
    kind: crate::backends::provider::BackendProviderKind,
) -> OAuthRefreshKind {
    match kind {
        crate::backends::provider::BackendProviderKind::ChatGptCodex => OAuthRefreshKind::ChatGpt,
        crate::backends::provider::BackendProviderKind::XaiGrokOAuth => OAuthRefreshKind::Xai,
        crate::backends::provider::BackendProviderKind::ClaudeCliSubscription => {
            OAuthRefreshKind::Claude
        }
        crate::backends::provider::BackendProviderKind::ClaudeGateway => OAuthRefreshKind::Gateway,
        _ => unreachable!("only agent-scoped OAuth kinds reach OAuth probing"),
    }
}

pub(crate) fn oauth_product(
    kind: crate::backends::provider::BackendProviderKind,
) -> crate::auth::credential::OAuthProduct {
    match kind {
        crate::backends::provider::BackendProviderKind::ChatGptCodex => {
            crate::auth::credential::CHATGPT_OAUTH_PRODUCT
        }
        crate::backends::provider::BackendProviderKind::XaiGrokOAuth => {
            crate::auth::credential::XAI_OAUTH_PRODUCT
        }
        crate::backends::provider::BackendProviderKind::ClaudeCliSubscription => {
            crate::auth::claude::CLAUDE_OAUTH_PRODUCT
        }
        crate::backends::provider::BackendProviderKind::ClaudeGateway => {
            crate::claude_gateway_login::CLAUDE_GATEWAY_PRODUCT
        }
        _ => unreachable!("only agent-scoped OAuth kinds reach OAuth probing"),
    }
}
