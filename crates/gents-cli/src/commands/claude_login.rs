use std::io::{self, BufRead, Write};

use anyhow::{Context, Result};
use gents_claude_login::{run_loopback_login, run_manual_login, LoginOptions};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::cli::args::ClaudeLoginArgs;
use crate::commands::login_wait::{connect_first_sign_in, print_sign_in_notes, until_cancelled};
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access};
use gents_service::home_state::resolve_node_did;

pub(crate) struct ClaudeLoginOptions {
    pub(crate) provider: String,
    pub(crate) label: Option<String>,
    pub(crate) manual: bool,
    pub(crate) open_browser: bool,
    pub(crate) client_id: Option<String>,
    pub(crate) token_url: Option<String>,
}

pub(crate) struct ClaudeLoginOutcome {
    pub(crate) sign_in: gents_inference::auth::credential::SignIn,
}

pub(crate) async fn claude_login(args: ClaudeLoginArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let node_did = resolve_node_did(Some(&home_dir), args.node_did.as_deref())?;
    let mut outcome = run_claude_login(
        &access,
        &node_did,
        &ClaudeLoginOptions {
            provider: args.provider,
            label: args.label,
            manual: args.manual,
            open_browser: !args.no_browser,
            client_id: args.client_id,
            token_url: args.token_url,
        },
        |url| eprintln!("Open this URL to sign in with Claude:\n{url}"),
        &CancellationToken::new(),
    )
    .await?;
    print_sign_in_notes(&outcome.sign_in);
    connect_first_sign_in(&access, &node_did, &mut outcome.sign_in).await;
    print_json(&claude_login_result_json(&outcome))?;
    Ok(())
}

pub(crate) async fn run_claude_login<O>(
    access: &ConfigAccess,
    node_did: &str,
    opts: &ClaudeLoginOptions,
    on_url: O,
    cancel: &CancellationToken,
) -> Result<ClaudeLoginOutcome>
where
    O: FnOnce(&str),
{
    let provider = gents_inference::auth::claude::normalize_provider(&opts.provider);
    let mut login_options = LoginOptions {
        open_browser: opts.open_browser,
        ..LoginOptions::default()
    };
    if let Some(client_id) = opts
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        login_options.client_id = client_id.to_string();
    }
    if let Some(token_url) = opts
        .token_url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        login_options.token_url = token_url.to_string();
    }

    let tokens = if opts.manual {
        run_manual_login(login_options, |auth_url| {
            eprintln!(
                "Open this URL to sign in with Claude:\n{auth_url}\n\nPaste the code shown on the success page (code#state) and press Enter:"
            );
            io::stderr().flush()?;
            let mut line = String::new();
            io::stdin().lock().read_line(&mut line)?;
            Ok(line)
        })
        .await
        .context("Claude manual login failed")?
    } else {
        let server =
            run_loopback_login(login_options).context("starting Claude login callback server")?;
        let stop = server.cancel_handle();
        on_url(&server.auth_url);
        until_cancelled(
            async {
                server
                    .block_until_done()
                    .await
                    .context("Claude browser login failed")
            },
            cancel,
            || stop.shutdown(),
        )
        .await?
    };

    let login_tokens = gents_inference::auth::claude::ClaudeLoginTokens {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_in: tokens.expires_in,
        scope: tokens.scope,
        account_id: tokens.account_id,
        organization_uuid: tokens.organization_uuid,
        account_uuid: tokens.account_uuid,
    };
    let credential = gents_inference::auth::claude::credential_from_login_tokens(
        node_did,
        &provider,
        &login_tokens,
        chrono::Utc::now(),
    );
    let sign_in =
        gents_inference::auth::credential::store_sign_in(access, credential, opts.label.as_deref())
            .await?;
    Ok(ClaudeLoginOutcome { sign_in })
}

pub(crate) fn claude_login_result_json(outcome: &ClaudeLoginOutcome) -> Value {
    let credential = &outcome.sign_in.credential;
    json!({
        "login": "completed",
        "doc_id": outcome.sign_in.doc_id,
        "label": gents_inference::auth::credential::effective_account_label(credential),
        "result": outcome.sign_in.result,
        "profiles": outcome.sign_in.profiles,
        "credential_id": credential.credential_id,
        "node_did": credential.node_did,
        "provider": credential.provider,
        "access_token_expires_at": credential.access_token_expires_at,
        "last_refresh": credential.last_refresh,
        "enabled": credential.enabled,
        "access_token": "<redacted>",
        "refresh_token": "<redacted>",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_json_redacts_tokens() {
        let credential = gents_inference::auth::claude::credential_from_login_tokens(
            "did:key:z6MkTest",
            "claude-subscription",
            &gents_inference::auth::claude::ClaudeLoginTokens {
                access_token: "access-SECRET".into(),
                refresh_token: "refresh-SECRET".into(),
                expires_in: Some(60),
                scope: None,
                account_id: None,
                organization_uuid: None,
                account_uuid: None,
            },
            chrono::Utc::now(),
        );
        let json = claude_login_result_json(&ClaudeLoginOutcome {
            sign_in: gents_inference::auth::credential::SignIn {
                doc_id: "bae-1".into(),
                credential,
                result: gents_inference::auth::credential::SignInResult::Added,
                identity_matched: false,
                profiles: vec!["default-profile".into()],
            },
        });
        let text = json.to_string();
        assert!(!text.contains("SECRET"), "{text}");
        assert_eq!(json["access_token"], "<redacted>");
        assert_eq!(json["login"], "completed");
        assert_eq!(json["label"], "Claude");
        assert_eq!(json["result"], "added");
        assert_eq!(json["profiles"], serde_json::json!(["default-profile"]));
    }

    fn browser_free_login() -> (ConfigAccess, ClaudeLoginOptions) {
        let access = ConfigAccess::graphql("http://127.0.0.1:9/api/v0/graphql");
        let opts = ClaudeLoginOptions {
            provider: gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER.into(),
            label: None,
            manual: false,
            open_browser: false,
            client_id: None,
            token_url: None,
        };
        (access, opts)
    }

    fn callback_port(auth_url: &str) -> u16 {
        let auth_url = url::Url::parse(auth_url).expect("auth URL");
        let (_, redirect) = auth_url
            .query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .expect("redirect_uri");
        url::Url::parse(&redirect)
            .expect("redirect URL")
            .port()
            .expect("loopback port")
    }

    /// tiny_http closes its listener on its own accept thread after the last
    /// server handle drops, so the port frees shortly after the login ends.
    async fn port_frees(port: u16) -> bool {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok()
    }

    #[tokio::test]
    async fn a_cancelled_sign_in_frees_its_callback_port() {
        let (access, opts) = browser_free_login();
        let cancel = CancellationToken::new();
        let mut auth_url = String::new();
        let result = run_claude_login(
            &access,
            "did:key:z6MkTest",
            &opts,
            |url| {
                auth_url = url.to_string();
                cancel.cancel();
            },
            &cancel,
        )
        .await;
        let Err(error) = result else {
            panic!("a cancelled sign-in stored a credential");
        };
        assert_eq!(error.to_string(), "Sign-in cancelled.");
        assert!(port_frees(callback_port(&auth_url)).await);
    }

    #[tokio::test]
    async fn dropping_a_waiting_sign_in_frees_its_callback_port() {
        let (access, opts) = browser_free_login();
        let cancel = CancellationToken::new();
        let (opened, auth_url) = tokio::sync::oneshot::channel();
        let login = run_claude_login(
            &access,
            "did:key:z6MkTest",
            &opts,
            |url| {
                let _ = opened.send(url.to_string());
            },
            &cancel,
        );
        let auth_url = tokio::select! {
            _ = login => panic!("the sign-in finished without a browser"),
            auth_url = auth_url => auth_url.expect("auth URL"),
        };
        assert!(port_frees(callback_port(&auth_url)).await);
    }
}
