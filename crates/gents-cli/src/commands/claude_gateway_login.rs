use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::cli::args::ClaudeGatewayLoginArgs;
use crate::{print_json, resolve_config_access, resolve_node_did};

pub(crate) async fn claude_gateway_login(args: ClaudeGatewayLoginArgs) -> Result<()> {
    let base = gents::claude_gateway_login::base_url(&args.url)?;
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let node_did = resolve_node_did(Some(&home_dir), args.node_did.as_deref())?;
    let http = gents::claude_gateway_login::gateway_http()?;
    let endpoints = gents::claude_gateway_login::discover(&http, &base).await;
    let code = gents::claude_gateway_login::request_device_code(&http, &endpoints)
        .await
        .context("Claude gateway sign-in failed")?;
    eprintln!("Open this URL to sign in to the Claude gateway at {base}:");
    eprintln!("{}", code.open_url());
    eprintln!("When prompted, enter code: {}", code.user_code);
    let tokens = gents::claude_gateway_login::poll_tokens(&http, &endpoints, &code)
        .await
        .context("Claude gateway sign-in failed")?;
    let credential = gents::claude_gateway_login::credential_from_tokens(
        &node_did,
        &base,
        &tokens,
        chrono::Utc::now(),
    );
    let sign_in =
        gents::inference::auth::credential::store_sign_in(&access, credential, args.label.as_deref()).await?;
    if let Some(hint) = sign_in.account_chooser_hint() {
        eprintln!("{hint}");
    }
    if let Some(note) = sign_in.profiles_note() {
        eprintln!("{note}");
    }
    print_json(&sign_in_json(&sign_in))?;
    Ok(())
}

fn sign_in_json(sign_in: &gents::inference::auth::credential::SignIn) -> Value {
    let credential = &sign_in.credential;
    json!({
        "doc_id": sign_in.doc_id,
        "label": gents::inference::auth::credential::effective_account_label(credential),
        "result": sign_in.result,
        "profiles": sign_in.profiles,
        "credential_id": credential.credential_id,
        "node_did": credential.node_did,
        "provider": credential.provider,
        "gateway": credential.account_id,
        "access_token_expires_at": credential.access_token_expires_at,
        "enabled": credential.enabled,
        "access_token": "<redacted>",
        "refresh_token": if credential.refresh_token.is_empty() { Value::Null } else { json!("<redacted>") },
    })
}
