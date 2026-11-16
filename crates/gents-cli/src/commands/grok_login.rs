use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents_inference::auth::xai_login::{poll_device_token, request_device_code};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::cli::args::GrokLoginArgs;
use crate::commands::login_wait::{connect_first_sign_in, print_sign_in_notes, until_cancelled};
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access};
use gents_service::home_state::resolve_node_did;

pub(crate) struct GrokLoginOptions {
    pub(crate) provider: String,
    pub(crate) label: Option<String>,
}

pub(crate) struct GrokLoginOutcome {
    pub(crate) sign_in: gents_inference::auth::credential::SignIn,
}

pub(crate) async fn grok_login(args: GrokLoginArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let node_did = resolve_node_did(Some(&home_dir), args.node_did.as_deref())?;
    let mut outcome = run_grok_login(
        &access,
        &node_did,
        &GrokLoginOptions {
            provider: args.provider,
            label: args.label,
        },
        print_grok_sign_in_url,
        &CancellationToken::new(),
    )
    .await?;
    print_sign_in_notes(&outcome.sign_in);
    connect_first_sign_in(&access, &node_did, &mut outcome.sign_in).await;
    print_json(&grok_login_result_json(&outcome))?;
    Ok(())
}

pub(crate) fn print_grok_sign_in_url(url: &str, user_code: Option<&str>) {
    eprintln!("Open this URL to sign in with Grok / xAI:");
    eprintln!("{url}");
    if let Some(user_code) = user_code {
        eprintln!("When prompted, enter code: {user_code}");
    }
}

/// `on_url` receives the verification URL and, when xAI returned no complete
/// URL, the code the user must enter there.
pub(crate) async fn run_grok_login<O>(
    access: &ConfigAccess,
    node_did: &str,
    opts: &GrokLoginOptions,
    on_url: O,
    cancel: &CancellationToken,
) -> Result<GrokLoginOutcome>
where
    O: FnOnce(&str, Option<&str>),
{
    let provider = gents_inference::clients::xai_grok::normalize_provider(&opts.provider);
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("preparing the Grok / xAI sign-in connection")?;

    let stop = Arc::new(AtomicBool::new(false));
    let login = async {
        let challenge = request_device_code(&http).await?;
        if !stop.load(Ordering::SeqCst) {
            match challenge.verification_uri_complete.as_deref() {
                Some(url) => on_url(url, None),
                None => on_url(&challenge.verification_uri, Some(&challenge.user_code)),
            }
        }
        poll_device_token(&http, &challenge, Some(stop.clone())).await
    };
    let tokens = until_cancelled(
        async { login.await.context("Grok / xAI device-code login failed") },
        cancel,
        || stop.store(true, Ordering::SeqCst),
    )
    .await?;

    let credential = gents_inference::auth::xai_login::credential_from_login_tokens(
        node_did,
        &provider,
        &tokens,
        chrono::Utc::now(),
    );
    let sign_in =
        gents_inference::auth::credential::store_sign_in(access, credential, opts.label.as_deref())
            .await?;
    Ok(GrokLoginOutcome { sign_in })
}

pub(crate) fn grok_login_result_json(outcome: &GrokLoginOutcome) -> Value {
    let credential = &outcome.sign_in.credential;
    json!({
        "doc_id": outcome.sign_in.doc_id,
        "label": gents_inference::auth::credential::effective_account_label(credential),
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
