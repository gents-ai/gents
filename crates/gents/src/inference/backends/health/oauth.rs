use std::sync::Arc;

use defra_node::EmbeddedNode;

use crate::inference::auth::credential::OAuthRefreshKind;
use crate::inference::backends::provider::BackendProviderOauthExt;
use crate::inference::backends::registry::InferenceBackend;

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
    kind: crate::inference::backends::provider::BackendProviderKind,
) -> OAuthRefreshKind {
    match kind {
        crate::inference::backends::provider::BackendProviderKind::ChatGptCodex => {
            OAuthRefreshKind::ChatGpt
        }
        crate::inference::backends::provider::BackendProviderKind::XaiGrokOAuth => {
            OAuthRefreshKind::Xai
        }
        crate::inference::backends::provider::BackendProviderKind::ClaudeCliSubscription => {
            OAuthRefreshKind::Claude
        }
        _ => unreachable!("only agent-scoped OAuth kinds reach OAuth probing"),
    }
}

pub(crate) fn oauth_product(
    kind: crate::inference::backends::provider::BackendProviderKind,
) -> crate::inference::auth::credential::OAuthProduct {
    match kind {
        crate::inference::backends::provider::BackendProviderKind::ChatGptCodex => {
            crate::inference::auth::credential::CHATGPT_OAUTH_PRODUCT
        }
        crate::inference::backends::provider::BackendProviderKind::XaiGrokOAuth => {
            crate::inference::auth::credential::XAI_OAUTH_PRODUCT
        }
        crate::inference::backends::provider::BackendProviderKind::ClaudeCliSubscription => {
            crate::inference::auth::claude::CLAUDE_OAUTH_PRODUCT
        }
        _ => unreachable!("only agent-scoped OAuth kinds reach OAuth probing"),
    }
}
