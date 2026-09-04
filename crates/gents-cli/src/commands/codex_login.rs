use anyhow::{Context, Result};
use gents_chatgpt_login::{
    complete_device_code_login, request_device_code, run_login_server, LoginOptions,
};
use gents_protocol::chatgpt_oauth::CLIENT_ID;
use serde_json::{json, Value};

use crate::cli::args::CodexLoginArgs;
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_agent_did, resolve_config_access};

pub(crate) struct CodexLoginOptions {
    pub(crate) provider: String,
    pub(crate) client_id: Option<String>,
    pub(crate) issuer: Option<String>,
    pub(crate) device_auth: bool,
}

pub(crate) struct CodexLoginOutcome {
    pub(crate) doc_id: String,
    pub(crate) credential: gents::oauth_credential::OAuthCredential,
}

pub(crate) async fn codex_login(args: CodexLoginArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let agent_did = resolve_agent_did(Some(&home_dir), args.agent_did.as_deref())?;
    let outcome = run_codex_login(
        &access,
        &agent_did,
        &CodexLoginOptions {
            provider: args.provider,
            client_id: args.client_id,
            issuer: args.issuer,
            device_auth: args.device_auth,
        },
    )
    .await?;
    print_json(&codex_login_result_json(&outcome))?;
    Ok(())
}

pub(crate) async fn run_codex_login(
    access: &ConfigAccess,
    agent_did: &str,
    opts: &CodexLoginOptions,
) -> Result<CodexLoginOutcome> {
    let provider = gents::chatgpt_codex::normalize_provider(&opts.provider);
    let mut login_options = LoginOptions {
        client_id: opts
            .client_id
            .clone()
            .unwrap_or_else(|| CLIENT_ID.to_string()),
        ..LoginOptions::default()
    };
    if let Some(issuer) = opts
        .issuer
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        login_options.issuer = issuer.to_string();
    }

    let tokens = if opts.device_auth {
        login_options.open_browser = false;
        let device_code = request_device_code(&login_options)
            .await
            .context("requesting ChatGPT device code")?;
        eprintln!(
            "Open {} and enter code {} (expires in 15 minutes).",
            device_code.verification_url, device_code.user_code
        );
        complete_device_code_login(&login_options, device_code)
            .await
            .context("ChatGPT device-code login failed")?
    } else {
        let server = run_login_server(login_options).context("starting ChatGPT login server")?;
        eprintln!(
            "Open this URL to sign in with ChatGPT:\n{}",
            server.auth_url
        );
        server
            .block_until_done()
            .await
            .context("ChatGPT browser login failed")?
    };

    let credential = gents::oauth_credential::OAuthCredential::from_login_tokens(
        agent_did,
        &provider,
        &tokens.id_token,
        tokens.access_token,
        tokens.refresh_token,
        chrono::Utc::now(),
    );
    let mutation = gents::oauth_credential::oauth_credential_upsert_mutation(&credential);
    let response = access.execute(&mutation).await?;
    let doc_id = gents_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")?;

    Ok(CodexLoginOutcome { doc_id, credential })
}

pub(crate) fn codex_login_result_json(outcome: &CodexLoginOutcome) -> Value {
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
