use anyhow::{Context, Result};
use codex_login::{
    run_device_code_login, run_login_server, AuthCredentialsStoreMode, AuthManager, ServerOptions,
    CLIENT_ID,
};
use serde_json::json;
use uuid::Uuid;

use crate::cli::args::CodexLoginArgs;
use crate::{print_json, resolve_agent_did, resolve_config_access};

pub(crate) async fn codex_login(args: CodexLoginArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let agent_did = resolve_agent_did(Some(&home_dir), args.agent_did.as_deref())?;
    let provider = defra_agent::chatgpt_codex::normalize_provider(&args.provider);
    let synthetic_home =
        std::env::temp_dir().join(format!("defra-agent-codex-login-{}", Uuid::new_v4()));
    let mut opts = ServerOptions::new(
        synthetic_home.clone(),
        args.client_id.unwrap_or_else(|| CLIENT_ID.to_string()),
        None,
        AuthCredentialsStoreMode::Ephemeral,
    );
    if let Some(issuer) = args.issuer.filter(|value| !value.trim().is_empty()) {
        opts.issuer = issuer;
    }

    if args.device_auth {
        opts.open_browser = false;
        run_device_code_login(opts)
            .await
            .context("ChatGPT device-code login failed")?;
    } else {
        let server = run_login_server(opts).context("starting ChatGPT login server")?;
        // Human prompt goes to stderr so stdout stays pure JSON (the command emits a single JSON
        // object via print_json on success) for automation.
        eprintln!(
            "Open this URL to sign in with ChatGPT:\n{}",
            server.auth_url
        );
        server
            .block_until_done()
            .await
            .context("ChatGPT browser login failed")?;
    }

    let manager = AuthManager::new(
        synthetic_home,
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::Ephemeral,
        /*chatgpt_base_url*/ None,
    )
    .await;
    let auth = manager
        .auth()
        .await
        .context("ChatGPT login completed but no ephemeral auth was returned")?;
    if !auth.is_chatgpt_auth() {
        anyhow::bail!(
            "ChatGPT login returned {:?}; ChatGPT OAuth credentials are required",
            auth.auth_mode()
        );
    }
    let token_data = auth
        .get_token_data()
        .context("ChatGPT login did not expose token data")?;
    let credential = defra_agent::chatgpt_codex::OAuthCredential::from_login_token_data(
        &agent_did,
        &provider,
        &token_data,
        chrono::Utc::now(),
    );
    let mutation = defra_agent::chatgpt_codex::oauth_credential_upsert_mutation(&credential);
    let response = access.execute(&mutation).await?;
    let doc_id =
        defra_agent_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")?;

    print_json(&json!({
        "doc_id": doc_id,
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
    }))?;
    Ok(())
}
