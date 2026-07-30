use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::cli::args::GrokLoginArgs;
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_agent_did, resolve_config_access};

pub(crate) struct GrokLoginOptions {
    pub(crate) provider: String,
}

pub(crate) struct GrokLoginOutcome {
    pub(crate) doc_id: String,
    pub(crate) credential: gents::oauth_credential::OAuthCredential,
}

pub(crate) async fn grok_login(args: GrokLoginArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let agent_did = resolve_agent_did(Some(&home_dir), args.agent_did.as_deref())?;
    let outcome = run_grok_login(
        &access,
        &agent_did,
        &GrokLoginOptions {
            provider: args.provider,
        },
    )
    .await?;
    print_json(&grok_login_result_json(&outcome))?;
    Ok(())
}

pub(crate) async fn run_grok_login(
    access: &ConfigAccess,
    agent_did: &str,
    opts: &GrokLoginOptions,
) -> Result<GrokLoginOutcome> {
    let provider = gents::xai_grok_oauth::normalize_provider(&opts.provider);
    let http = reqwest::Client::new();

    // v1: device-code is the only login path (SSH/VPS safe; no loopback).
    let tokens = gents::xai_oauth_login::run_device_code_login(&http, true)
        .await
        .context("Grok / xAI device-code login failed")?;

    let credential = gents::xai_oauth_login::credential_from_login_tokens(
        agent_did,
        &provider,
        &tokens,
        chrono::Utc::now(),
    );
    let mutation = gents::oauth_credential::oauth_credential_upsert_mutation(&credential);
    let response = access.execute(&mutation).await?;
    let doc_id = gents_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")?;

    Ok(GrokLoginOutcome { doc_id, credential })
}

pub(crate) fn grok_login_result_json(outcome: &GrokLoginOutcome) -> Value {
    let credential = &outcome.credential;
    json!({
        "doc_id": outcome.doc_id,
        "credential_id": credential.credential_id,
        "agent_did": credential.agent_did,
        "provider": credential.provider,
        "account_id": credential.account_id,
        "chatgpt_plan_type": credential.chatgpt_plan_type,
        "is_fedramp": credential.is_fedramp,
        "access_token_expires_at": credential.access_token_expires_at,
        "last_refresh": credential.last_refresh,
        "enabled": credential.enabled,
        "access_token": "<redacted>",
        "refresh_token": "<redacted>",
        "id_token": credential.id_token.as_ref().map(|_| "<redacted>"),
    })
}
