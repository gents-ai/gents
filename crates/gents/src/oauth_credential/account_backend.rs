use anyhow::{Context, Result};

use super::*;

fn account_backend(
    credential: &OAuthCredential,
    account_ref: &str,
) -> Result<crate::InferenceBackend> {
    preset_account_backend(
        &credential.node_did,
        &credential.provider,
        Some(account_ref),
        effective_account_label(credential),
    )
}

pub(crate) fn preset_account_backend(
    node_did: &str,
    provider: &str,
    account_ref: Option<&str>,
    name: String,
) -> Result<crate::InferenceBackend> {
    use crate::inference_setup::{InferenceAuthMethod as Auth, InferenceProviderId as Id};
    let (provider_id, auth) = match provider {
        crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER => (Id::OpenAi, Auth::ChatGptOauth),
        crate::claude_oauth::CLAUDE_OAUTH_PROVIDER => (Id::Anthropic, Auth::ClaudeOauth),
        crate::xai_grok_oauth::XAI_OAUTH_PROVIDER => (Id::Grok, Auth::GrokOauth),
        other => anyhow::bail!("no backend reads sign-ins of provider {other:?}"),
    };
    let spec = crate::inference_setup::connection_spec(provider_id, auth, "")?;
    Ok(crate::InferenceBackend {
        node_did: node_did.to_owned(),
        backend_id: match account_ref {
            Some(account_ref) => format!("{provider}-{account_ref}"),
            None => provider.to_owned(),
        },
        name,
        provider_kind: spec.provider_kind,
        openai_wire_api: spec.openai_wire_api,
        endpoint: spec.endpoint,
        auth: crate::document_config::BackendAuth::NodeOAuth {
            account_ref: account_ref.map(str::to_owned),
        },
        connect_timeout_secs: None,
        discovery_timeout_secs: None,
        max_concurrent: None,
        max_queue_depth: None,
        enabled: true,
        tags: Vec::new(),
    })
}

async fn write_account_backend(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    backend: &crate::InferenceBackend,
) -> Result<()> {
    crate::config_client::write_inference_backend_in_txn(txn, backend)
        .await
        .context(
            "the account's backend could not be stored, so the sign-in was not saved; \
             fix the node's configuration and sign in again",
        )
}

pub(super) async fn sync_account_backend(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    credential: &OAuthCredential,
    old_label: Option<&str>,
) -> Result<()> {
    let Some(account_ref) = credential.account_ref.as_deref() else {
        return Ok(());
    };
    let serving: Vec<_> =
        crate::config_client::list_inference_backends_in_txn(txn, &credential.node_did)
            .await?
            .into_iter()
            .filter(|backend| backend.auth.oauth_account_ref() == Some(account_ref))
            .collect();
    if serving.is_empty() {
        return write_account_backend(txn, &account_backend(credential, account_ref)?).await;
    }
    let label = effective_account_label(credential);
    for mut backend in serving {
        if old_label.is_some_and(|old| old == backend.name && old != label) {
            backend.name = label.clone();
            write_account_backend(txn, &backend).await?;
        }
    }
    Ok(())
}
