use std::sync::Arc;

use defra_node::EmbeddedNode;

use crate::backend_provider::BackendProviderOauthExt;
use crate::backend_registry::InferenceBackend;
use crate::inference::auth::credential::OAuthRefreshKind;

pub struct OAuthProbeContext<'a> {
    pub node: Arc<EmbeddedNode>,
    pub node_did: &'a str,
}

#[derive(Debug, thiserror::Error)]
#[error("{context}: {source:#}")]
pub(super) struct OAuthProbeError {
    context: String,
    #[source]
    pub(super) source: crate::inference::auth::credential::OAuthCredentialError,
}

pub(super) async fn oauth_credential_for_probe(
    context: &OAuthProbeContext<'_>,
    backend: &InferenceBackend,
) -> std::result::Result<crate::inference::auth::credential::OAuthCredential, OAuthProbeError> {
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
    let (bearer, mut credential) = crate::inference::auth::http::bootstrap_oauth_client_typed(
        context.node.clone(),
        context.node_did,
        provider,
        oauth_refresh_kind(backend.provider_kind),
        oauth_product(backend.provider_kind),
        crate::inference::auth::credential::AccountPick::Reference(
            backend.auth.oauth_account_ref(),
        ),
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
    kind: crate::backend_provider::BackendProviderKind,
) -> OAuthRefreshKind {
    match kind {
        crate::backend_provider::BackendProviderKind::ChatGptCodex => OAuthRefreshKind::ChatGpt,
        crate::backend_provider::BackendProviderKind::XaiGrokOAuth => OAuthRefreshKind::Xai,
        crate::backend_provider::BackendProviderKind::ClaudeCliSubscription => {
            OAuthRefreshKind::Claude
        }
        _ => unreachable!("only agent-scoped OAuth kinds reach OAuth probing"),
    }
}

pub(crate) fn oauth_product(
    kind: crate::backend_provider::BackendProviderKind,
) -> crate::inference::auth::credential::OAuthProduct {
    match kind {
        crate::backend_provider::BackendProviderKind::ChatGptCodex => {
            crate::inference::auth::credential::CHATGPT_OAUTH_PRODUCT
        }
        crate::backend_provider::BackendProviderKind::XaiGrokOAuth => {
            crate::inference::auth::credential::XAI_OAUTH_PRODUCT
        }
        crate::backend_provider::BackendProviderKind::ClaudeCliSubscription => {
            crate::inference::auth::claude::CLAUDE_OAUTH_PRODUCT
        }
        _ => unreachable!("only agent-scoped OAuth kinds reach OAuth probing"),
    }
}
