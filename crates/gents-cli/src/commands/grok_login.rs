use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::cli::args::GrokLoginArgs;
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access, resolve_node_did};

pub(crate) struct GrokLoginOptions {
    pub(crate) provider: String,
    pub(crate) label: Option<String>,
}

pub(crate) struct GrokLoginOutcome {
    pub(crate) sign_in: gents::inference::auth::credential::SignIn,
}

pub(crate) async fn grok_login(args: GrokLoginArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let node_did = resolve_node_did(Some(&home_dir), args.node_did.as_deref())?;
    let outcome = run_grok_login(
        &access,
        &node_did,
        &GrokLoginOptions {
            provider: args.provider,
            label: args.label,
        },
    )
    .await?;
    print_json(&grok_login_result_json(&outcome))?;
    Ok(())
}

pub(crate) async fn run_grok_login(
    access: &ConfigAccess,
    node_did: &str,
    opts: &GrokLoginOptions,
) -> Result<GrokLoginOutcome> {
    let provider = gents::inference::clients::xai_grok::normalize_provider(&opts.provider);
    let http = reqwest::Client::new();

    let tokens = gents::inference::auth::xai_login::run_device_code_login(&http, true)
        .await
        .context("Grok / xAI device-code login failed")?;

    let credential = gents::inference::auth::xai_login::credential_from_login_tokens(
        node_did,
        &provider,
        &tokens,
        chrono::Utc::now(),
    );
    let sign_in = gents::inference::auth::credential::store_sign_in(
        access,
        credential,
        opts.label.as_deref(),
    )
    .await?;
    if let Some(hint) = sign_in.account_chooser_hint() {
        eprintln!("{hint}");
    }
    if let Some(note) = sign_in.profiles_note() {
        eprintln!("{note}");
    }
    Ok(GrokLoginOutcome { sign_in })
}

pub(crate) fn grok_login_result_json(outcome: &GrokLoginOutcome) -> Value {
    let credential = &outcome.sign_in.credential;
    json!({
        "doc_id": outcome.sign_in.doc_id,
        "label": gents::inference::auth::credential::effective_account_label(credential),
        "result": outcome.sign_in.result,
        "profiles": outcome.sign_in.profiles,
        "credential_id": credential.credential_id,
        "node_did": credential.node_did,
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
