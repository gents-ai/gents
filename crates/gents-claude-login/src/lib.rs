use rapidhash::RapidHashMap;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::thread;

use gents_claude::oauth::{self, AuthorizeRequest, ClientOverride, ExchangeRequest, Pkce};
use gents_login_ui::CallbackPage;
use rand::RngCore;
use tiny_http::Server;
use tokio::sync::{mpsc, Notify};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginOptions {
    pub client_id: String,
    pub token_url: String,
    pub open_browser: bool,
    pub force_state: Option<String>,
}

impl Default for LoginOptions {
    fn default() -> Self {
        Self {
            client_id: oauth::CLIENT_ID.to_string(),
            token_url: oauth::TOKEN_URL.to_string(),
            open_browser: true,
            force_state: None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LoginTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: Option<i64>,
    pub scope: Option<String>,
    pub account_id: Option<String>,
    pub organization_uuid: Option<String>,
    pub account_uuid: Option<String>,
}

impl fmt::Debug for LoginTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginTokens")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct ShutdownHandle {
    notify: Arc<Notify>,
}

impl ShutdownHandle {
    pub fn shutdown(&self) {
        self.notify.notify_one();
    }
}

pub struct LoginServer {
    pub auth_url: String,
    pub actual_port: u16,
    task: tokio::task::JoinHandle<io::Result<LoginTokens>>,
    shutdown: ShutdownHandle,
}

impl LoginServer {
    pub async fn block_until_done(self) -> io::Result<LoginTokens> {
        self.task
            .await
            .map_err(|error| io::Error::other(format!("login callback task failed: {error}")))?
    }

    pub fn cancel_handle(&self) -> ShutdownHandle {
        self.shutdown.clone()
    }
}

pub fn run_loopback_login(options: LoginOptions) -> io::Result<LoginServer> {
    let pkce = generate_pkce();
    let state = options.force_state.clone().unwrap_or_else(generate_state);
    let server = Server::http("127.0.0.1:0").map_err(io::Error::other)?;
    let actual_port = server
        .server_addr()
        .to_ip()
        .map(|address| address.port())
        .ok_or_else(|| io::Error::other("login callback server did not expose an IP port"))?;
    let server = Arc::new(server);
    let redirect = Redirect::Loopback(actual_port);
    let auth_url = build_authorize_url(&options, redirect, &pkce, &state)?;

    if options.open_browser {
        if let Err(error) = webbrowser::open(&auth_url) {
            tracing::warn!(%error, "could not open the Claude login URL in a browser");
        }
    }

    let (sender, mut receiver) = mpsc::channel(8);
    let receive_server = server.clone();
    thread::spawn(move || {
        while let Ok(request) = receive_server.recv() {
            if sender.blocking_send(request).is_err() {
                break;
            }
        }
    });

    let notify = Arc::new(Notify::new());
    let task_notify = notify.clone();
    let task_server = server.clone();
    let task = tokio::spawn(async move {
        let result = loop {
            tokio::select! {
                _ = task_notify.notified() => break Err(io::Error::other("Claude login was cancelled")),
                request = receiver.recv() => {
                    let Some(request) = request else {
                        break Err(io::Error::other("Claude login callback server stopped"));
                    };
                    let outcome =
                        handle_callback_request(request.url(), &options, redirect, &pkce, &state)
                            .await;
                    let (page, completed) = outcome.into_parts();
                    let response = gents_login_ui::response(page);
                    let _ = tokio::task::spawn_blocking(move || request.respond(response)).await;
                    if let Some(result) = completed {
                        break result;
                    }
                }
            }
        };
        task_server.unblock();
        result
    });

    Ok(LoginServer {
        auth_url,
        actual_port,
        task,
        shutdown: ShutdownHandle { notify },
    })
}

pub async fn run_manual_login(
    options: LoginOptions,
    read_pasted: impl FnOnce(&str) -> io::Result<String>,
) -> io::Result<LoginTokens> {
    let pkce = generate_pkce();
    let state = options.force_state.clone().unwrap_or_else(generate_state);
    let auth_url = build_authorize_url(&options, Redirect::Manual, &pkce, &state)?;
    let pasted = read_pasted(&auth_url)?;
    let code = parse_manual_code(pasted.trim(), &state)?;
    exchange_code(&options, Redirect::Manual, &pkce, &code, &state).await
}

pub(crate) fn parse_manual_code(pasted: &str, expected_state: &str) -> io::Result<String> {
    let (code, state) = pasted.split_once('#').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected the pasted value to look like code#state",
        )
    })?;
    if state != expected_state {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "OAuth state mismatch; restart sign-in",
        ));
    }
    if code.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "authorization code was empty",
        ));
    }
    Ok(code.to_string())
}

enum CallbackOutcome {
    Continue(CallbackPage),
    Complete {
        page: CallbackPage,
        result: io::Result<LoginTokens>,
    },
}

impl CallbackOutcome {
    fn into_parts(self) -> (CallbackPage, Option<io::Result<LoginTokens>>) {
        match self {
            Self::Continue(page) => (page, None),
            Self::Complete { page, result } => (page, Some(result)),
        }
    }
}

async fn handle_callback_request(
    request_target: &str,
    options: &LoginOptions,
    redirect: Redirect,
    pkce: &Pkce,
    expected_state: &str,
) -> CallbackOutcome {
    let Ok(parsed) = Url::parse(&format!("http://localhost{request_target}")) else {
        return CallbackOutcome::Continue(CallbackPage::Incomplete);
    };
    if parsed.path() != "/callback" {
        return CallbackOutcome::Continue(CallbackPage::NotFound);
    }
    let params: RapidHashMap<String, String> = parsed.query_pairs().into_owned().collect();
    if params.get("state").map(String::as_str) != Some(expected_state) {
        tracing::warn!("rejected Claude OAuth callback with mismatched state");
        return CallbackOutcome::Continue(CallbackPage::StaleLink);
    }
    if let Some(error) = params.get("error") {
        let message = params
            .get("error_description")
            .map(String::as_str)
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(error);
        return CallbackOutcome::Complete {
            page: CallbackPage::NotCompleted,
            result: Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Claude authorization failed: {message}"),
            )),
        };
    }
    let Some(code) = params
        .get("code")
        .map(String::as_str)
        .filter(|v| !v.is_empty())
    else {
        return CallbackOutcome::Continue(CallbackPage::Incomplete);
    };
    match exchange_code(options, redirect, pkce, code, expected_state).await {
        Ok(tokens) => CallbackOutcome::Complete {
            page: CallbackPage::SignedIn,
            result: Ok(tokens),
        },
        Err(error) => CallbackOutcome::Complete {
            page: CallbackPage::ExchangeFailed,
            result: Err(error),
        },
    }
}

/// Where the authorization server sends the browser back to.
#[derive(Clone, Copy)]
pub(crate) enum Redirect {
    Loopback(u16),
    Manual,
}

impl Redirect {
    fn port(self) -> u16 {
        match self {
            Self::Loopback(port) => port,
            Self::Manual => 0,
        }
    }

    fn is_manual(self) -> bool {
        matches!(self, Self::Manual)
    }
}

fn random_bytes() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

pub(crate) fn generate_pkce() -> Pkce {
    oauth::pkce(random_bytes())
}

fn generate_state() -> String {
    oauth::state(random_bytes())
}

pub(crate) fn build_authorize_url(
    options: &LoginOptions,
    redirect: Redirect,
    pkce: &Pkce,
    state: &str,
) -> io::Result<String> {
    let login_scopes: Vec<String> = oauth::LOGIN_SCOPES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let client = (options.client_id != oauth::CLIENT_ID).then_some(ClientOverride {
        client_id: Some(&options.client_id),
        scopes: &login_scopes,
    });
    oauth::authorize_url(&AuthorizeRequest {
        code_challenge: &pkce.challenge,
        state,
        port: redirect.port(),
        manual: redirect.is_manual(),
        login_with_claude_ai: true,
        inference_only: false,
        org_uuid: None,
        login_hint: None,
        login_method: None,
        client,
        also_request_scope: None,
    })
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn text_field(value: &str) -> Option<String> {
    Some(value.trim())
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

fn now_ms() -> io::Result<u64> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?;
    u64::try_from(elapsed.as_millis()).map_err(io::Error::other)
}

/// Maps the owner's parsed response onto the login shape. Claude Code issues
/// a refresh token on every login, so its absence is a failed sign-in.
fn login_tokens(body: &[u8], now_ms: u64) -> io::Result<LoginTokens> {
    let tokens = oauth::parse_token_response(body, now_ms, None)
        .map_err(|error| io::Error::other(format!("Claude sign-in failed: {error}")))?;
    let refresh_token = tokens.refresh_token.ok_or_else(|| {
        io::Error::other("Claude sign-in failed: the token response has no refresh token")
    })?;
    let lifetime_secs = (tokens.expires_at_ms - now_ms) / 1000;
    Ok(LoginTokens {
        access_token: tokens.access_token,
        refresh_token,
        expires_in: i64::try_from(lifetime_secs).ok(),
        scope: Some(tokens.scopes.join(" ")).filter(|scope| !scope.is_empty()),
        account_id: tokens.account.as_ref().and_then(|account| {
            text_field(&account.email_address).or_else(|| text_field(&account.uuid))
        }),
        organization_uuid: tokens.organization_uuid.as_deref().and_then(text_field),
        account_uuid: tokens
            .account
            .as_ref()
            .and_then(|account| text_field(&account.uuid)),
    })
}

pub(crate) async fn exchange_code(
    options: &LoginOptions,
    redirect: Redirect,
    pkce: &Pkce,
    code: &str,
    state: &str,
) -> io::Result<LoginTokens> {
    let post = oauth::exchange_request(&ExchangeRequest {
        code,
        state,
        code_verifier: &pkce.verifier,
        port: redirect.port(),
        manual: redirect.is_manual(),
        expires_in: None,
        client_id: Some(&options.client_id),
    })
    .map_err(io::Error::other)?;
    let response = reqwest::Client::new()
        .post(&options.token_url)
        .timeout(std::time::Duration::from_millis(post.timeout_ms))
        .header("Content-Type", "application/json")
        .body(post.body)
        .send()
        .await
        .map_err(redacted_transport_error)?;
    let status = response.status();
    let body = response.bytes().await.map_err(redacted_transport_error)?;
    if !status.is_success() {
        let detail = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| "no error code".to_string());
        return Err(io::Error::other(format!(
            "Claude token endpoint returned HTTP {status}: {detail}"
        )));
    }
    login_tokens(&body, now_ms()?)
}

fn redacted_transport_error(error: reqwest::Error) -> io::Error {
    let kind = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "could not connect"
    } else {
        "failed"
    };
    io::Error::other(format!("Claude token exchange {kind}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> LoginOptions {
        LoginOptions {
            open_browser: false,
            force_state: Some("state-1".into()),
            ..LoginOptions::default()
        }
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let tokens = LoginTokens {
            access_token: "access-SECRET".into(),
            refresh_token: "refresh-SECRET".into(),
            expires_in: Some(60),
            scope: Some("user:inference".into()),
            account_id: None,
            organization_uuid: None,
            account_uuid: None,
        };
        let rendered = format!("{tokens:?}");
        assert!(!rendered.contains("SECRET"), "{rendered}");
        assert!(rendered.contains("expires_in: Some(60)"), "{rendered}");
        let pkce = generate_pkce();
        let rendered = format!("{pkce:?}");
        assert!(!rendered.contains(&pkce.verifier), "{rendered}");
    }

    #[tokio::test]
    async fn exchange_error_never_echoes_the_code() {
        let (url, _handle) = one_shot_server(401, r#"{"error":"invalid_grant"}"#).await;
        let opts = LoginOptions {
            token_url: url,
            ..options()
        };
        let pkce = generate_pkce();
        let err = exchange_code(
            &opts,
            Redirect::Loopback(1),
            &pkce,
            "code-SECRET",
            "state-1",
        )
        .await
        .unwrap_err();
        assert!(!err.to_string().contains("code-SECRET"), "{err}");
        assert!(err.to_string().contains("401"), "{err}");
    }

    const FULL_RESPONSE: &str = r#"{"access_token":"access-1","refresh_token":"refresh-1","expires_in":28800,"scope":"user:profile user:inference","account":{"uuid":"account-1","email_address":" person@example.test "},"organization":{"uuid":"org-1"}}"#;

    #[test]
    fn token_response_maps_account_and_lifetime() {
        let tokens = login_tokens(FULL_RESPONSE.as_bytes(), 1_000).expect("tokens");
        assert_eq!(tokens.access_token, "access-1");
        assert_eq!(tokens.refresh_token, "refresh-1");
        assert_eq!(tokens.expires_in, Some(28800));
        assert_eq!(tokens.scope.as_deref(), Some("user:profile user:inference"));
        assert_eq!(tokens.account_id.as_deref(), Some("person@example.test"));
        assert_eq!(tokens.account_uuid.as_deref(), Some("account-1"));
        assert_eq!(tokens.organization_uuid.as_deref(), Some("org-1"));
    }

    #[test]
    fn token_response_without_refresh_token_or_lifetime_fails_the_sign_in() {
        let no_refresh = br#"{"access_token":"a","expires_in":60}"#;
        let err = login_tokens(no_refresh, 0).unwrap_err();
        assert!(err.to_string().contains("refresh token"), "{err}");
        let no_expiry = br#"{"access_token":"a","refresh_token":"r"}"#;
        let err = login_tokens(no_expiry, 0).unwrap_err();
        assert!(err.to_string().contains("malformed"), "{err}");
    }

    #[test]
    fn authorize_url_is_built_by_the_owner() {
        let pkce = generate_pkce();
        let manual = build_authorize_url(&options(), Redirect::Manual, &pkce, "state-1").unwrap();
        let url = Url::parse(&manual).unwrap();
        assert_eq!(
            format!("{}{}", url.origin().ascii_serialization(), url.path()),
            "https://claude.com/cai/oauth/authorize"
        );
        let params: RapidHashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(params["client_id"], oauth::CLIENT_ID);
        assert_eq!(params["redirect_uri"], oauth::MANUAL_REDIRECT_URL);
        assert_eq!(params["scope"], oauth::LOGIN_SCOPES.join(" "));
        assert_eq!(params["code_challenge"], pkce.challenge);
        assert_eq!(params["state"], "state-1");

        let custom = LoginOptions {
            client_id: "custom-client".into(),
            ..options()
        };
        let loopback =
            build_authorize_url(&custom, Redirect::Loopback(4242), &pkce, "state-1").unwrap();
        let params: RapidHashMap<String, String> = Url::parse(&loopback)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(params["client_id"], "custom-client");
        assert_eq!(params["redirect_uri"], "http://localhost:4242/callback");
        assert_eq!(params["scope"], oauth::LOGIN_SCOPES.join(" "));
    }

    #[tokio::test]
    async fn exchange_posts_the_owner_body_and_parses_the_response() {
        let (url, handle) = one_shot_server(200, FULL_RESPONSE).await;
        let opts = LoginOptions {
            token_url: url,
            ..options()
        };
        let pkce = generate_pkce();
        let tokens = exchange_code(&opts, Redirect::Manual, &pkce, "code-1", "state-1")
            .await
            .expect("tokens");
        assert_eq!(tokens.refresh_token, "refresh-1");
        let request = handle.await.expect("server");
        let body: serde_json::Value =
            serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["grant_type"], "authorization_code");
        assert_eq!(body["code"], "code-1");
        assert_eq!(body["state"], "state-1");
        assert_eq!(body["redirect_uri"], oauth::MANUAL_REDIRECT_URL);
        assert_eq!(body["client_id"], oauth::CLIENT_ID);
        assert_eq!(body["code_verifier"], pkce.verifier);
    }

    async fn one_shot_server(
        status: u16,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}/v1/oauth/token", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.expect("read");
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(idx) = text.find("\r\n\r\n") {
                    let content_length: usize = text[..idx]
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .or_else(|| line.strip_prefix("Content-Length: "))
                        })
                        .and_then(|value| value.trim().parse().ok())
                        .unwrap_or(0);
                    if buf.len() >= idx + 4 + content_length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&buf).into_owned();
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.expect("write");
            socket.shutdown().await.ok();
            request
        });
        (url, handle)
    }
}
