use anyhow::{Context, Result};
use codex_login::{
    run_device_code_login, run_login_server, AuthCredentialsStoreMode, AuthManager, ServerOptions,
    CLIENT_ID,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::cli::args::CodexLoginArgs;
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_agent_did, resolve_config_access};

/// Inputs to a single ChatGPT/Codex OAuth login, independent of how the caller
/// obtained the target node. Shared by the `codex-login` command and `init`'s
/// inline first-login.
pub(crate) struct CodexLoginOptions {
    pub(crate) provider: String,
    pub(crate) client_id: Option<String>,
    pub(crate) issuer: Option<String>,
    pub(crate) device_auth: bool,
}

/// The persisted result of a successful login.
pub(crate) struct CodexLoginOutcome {
    pub(crate) doc_id: String,
    pub(crate) credential: gents::chatgpt_codex::OAuthCredential,
}

pub(crate) async fn codex_login(args: CodexLoginArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
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

/// Drive the ChatGPT OAuth flow and upsert the resulting `OAuthCredential`
/// through `access`. Emits the sign-in prompt to stderr but never prints the
/// result — the caller owns output so `init` can fold it into its single JSON
/// object.
pub(crate) async fn run_codex_login(
    access: &ConfigAccess,
    agent_did: &str,
    opts: &CodexLoginOptions,
) -> Result<CodexLoginOutcome> {
    let provider = gents::chatgpt_codex::normalize_provider(&opts.provider);
    let synthetic_home = std::env::temp_dir().join(format!("gents-codex-login-{}", Uuid::new_v4()));
    let mut server_opts = ServerOptions::new(
        synthetic_home.clone(),
        opts.client_id
            .clone()
            .unwrap_or_else(|| CLIENT_ID.to_string()),
        None,
        AuthCredentialsStoreMode::Ephemeral,
    );
    if let Some(issuer) = opts
        .issuer
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        server_opts.issuer = issuer.to_string();
    }

    if opts.device_auth {
        server_opts.open_browser = false;
        run_device_code_login(server_opts)
            .await
            .context("ChatGPT device-code login failed")?;
    } else {
        let server = run_login_server(server_opts).context("starting ChatGPT login server")?;
        // Human prompt goes to stderr so stdout stays reserved for the caller's
        // machine-readable JSON.
        eprintln!("Open this URL to sign in with ChatGPT:\n{}", server.auth_url);
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
    let credential = gents::chatgpt_codex::OAuthCredential::from_login_token_data(
        agent_did,
        &provider,
        &token_data,
        chrono::Utc::now(),
    );
    let mutation = gents::chatgpt_codex::oauth_credential_upsert_mutation(&credential);
    let response = access.execute(&mutation).await?;
    let doc_id = gents_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")?;

    Ok(CodexLoginOutcome { doc_id, credential })
}

/// The redacted JSON view of a login result, shared by the standalone command
/// and `init`'s folded output.
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
