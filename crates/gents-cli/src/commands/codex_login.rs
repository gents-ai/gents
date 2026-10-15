use anyhow::{Context, Result};
use gents_chatgpt_login::{
    complete_device_code_login, request_device_code, run_login_server, LoginOptions,
};
use gents_protocol::chatgpt_oauth::CLIENT_ID;
use serde_json::{json, Value};

use crate::cli::args::CodexLoginArgs;
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access, resolve_node_did};

pub(crate) struct CodexLoginOptions {
    pub(crate) provider: String,
    pub(crate) label: Option<String>,
    pub(crate) client_id: Option<String>,
    pub(crate) issuer: Option<String>,
    pub(crate) device_auth: bool,
}

pub(crate) struct CodexLoginOutcome {
    pub(crate) sign_in: gents::oauth_credential::SignIn,
}

pub(crate) async fn codex_login(args: CodexLoginArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let node_did = resolve_node_did(Some(&home_dir), args.node_did.as_deref())?;
    let outcome = run_codex_login(
        &access,
        &node_did,
        &CodexLoginOptions {
            provider: args.provider,
            label: args.label,
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
    node_did: &str,
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
        node_did,
        &provider,
        &tokens.id_token,
        tokens.access_token,
        tokens.refresh_token,
        chrono::Utc::now(),
    );
    let sign_in =
        gents::oauth_credential::store_sign_in(access, credential, opts.label.as_deref()).await?;
    if let Some(hint) = sign_in.account_chooser_hint() {
        eprintln!("{hint}");
    }
    if let Some(note) = sign_in.profiles_note() {
        eprintln!("{note}");
    }
    Ok(CodexLoginOutcome { sign_in })
}

pub(crate) fn codex_login_result_json(outcome: &CodexLoginOutcome) -> Value {
    let credential = &outcome.sign_in.credential;
    json!({
        "doc_id": outcome.sign_in.doc_id,
        "label": gents::oauth_credential::effective_account_label(credential),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_json_redacts_tokens_and_reports_the_account() {
        let mut credential = gents::oauth_credential::OAuthCredential::from_login_tokens(
            "did:key:z6MkTest",
            "chatgpt-codex",
            "id-SECRET",
            "access-SECRET".into(),
            "refresh-SECRET".into(),
            chrono::Utc::now(),
        );
        credential.label = Some("Work".into());
        let json = codex_login_result_json(&CodexLoginOutcome {
            sign_in: gents::oauth_credential::SignIn {
                doc_id: "bae-1".into(),
                credential,
                result: gents::oauth_credential::SignInResult::Refreshed,
                identity_matched: true,
                profiles: Vec::new(),
            },
        });
        let text = json.to_string();
        assert!(!text.contains("SECRET"), "{text}");
        assert_eq!(json["label"], "Work");
        assert_eq!(json["result"], "refreshed");
        assert_eq!(json["profiles"], serde_json::json!([]));
    }
}
