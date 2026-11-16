use anyhow::{Context, Result};

use super::*;

fn account_backend(
    credential: &OAuthCredential,
    account_ref: &str,
) -> Result<gents_documents::InferenceBackend> {
    preset_account_backend(
        &credential.node_did,
        &credential.provider,
        Some(account_ref),
        effective_account_label(credential),
    )
}

pub fn preset_account_backend(
    node_did: &str,
    provider: &str,
    account_ref: Option<&str>,
    name: String,
) -> Result<gents_documents::InferenceBackend> {
    use crate::backends::setup::{InferenceAuthMethod as Auth, InferenceProviderId as Id};
    let (provider_id, auth) = match provider {
        crate::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER => (Id::OpenAi, Auth::ChatGptOauth),
        crate::auth::claude::CLAUDE_OAUTH_PROVIDER => (Id::Anthropic, Auth::ClaudeOauth),
        crate::clients::xai_grok::XAI_OAUTH_PROVIDER => (Id::Grok, Auth::GrokOauth),
        other => anyhow::bail!("no backend reads sign-ins of provider {other:?}"),
    };
    let spec = crate::backends::setup::connection_spec(provider_id, auth, "")?;
    Ok(gents_documents::InferenceBackend {
        physical_doc_id: None,
        node_did: node_did.to_owned(),
        backend_id: match account_ref {
            Some(account_ref) => format!("{provider}-{account_ref}"),
            None => provider.to_owned(),
        },
        name,
        provider_kind: spec.provider_kind,
        openai_wire_api: spec.openai_wire_api,
        endpoint: spec.endpoint,
        claude_platform: None,
        auth: gents_documents::BackendAuth::NodeOAuth {
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
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    backend: &gents_documents::InferenceBackend,
) -> Result<()> {
    gents_documents::write_inference_backend_in_txn(
        txn,
        backend,
        &crate::resolved::ProviderProfilePolicy,
    )
    .await
    .context(
        "the account's backend could not be stored, so the sign-in was not saved; \
             fix the node's configuration and sign in again",
    )
}

pub(super) async fn sync_account_backend(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    credential: &OAuthCredential,
    old_label: Option<&str>,
) -> Result<()> {
    let Some(account_ref) = credential.account_ref.as_deref() else {
        return Ok(());
    };
    let serving: Vec<_> =
        gents_documents::list_inference_backends_in_txn(txn, &credential.node_did)
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
