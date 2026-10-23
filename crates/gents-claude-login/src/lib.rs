use rapidhash::RapidHashMap;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::thread;

use base64::Engine;
use gents_login_ui::CallbackPage;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tiny_http::Server;
use tokio::sync::{mpsc, Notify};
use url::Url;

pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const MANUAL_REDIRECT_URL: &str = "https://platform.claude.com/oauth/code/callback";
pub const SCOPES: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginOptions {
    pub client_id: String,
    pub authorize_url: String,
    pub token_url: String,
    pub open_browser: bool,
    pub force_state: Option<String>,
}

impl Default for LoginOptions {
    fn default() -> Self {
        Self {
            client_id: CLIENT_ID.to_string(),
            authorize_url: AUTHORIZE_URL.to_string(),
            token_url: TOKEN_URL.to_string(),
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
    let redirect_uri = format!("http://localhost:{actual_port}/callback");
    let auth_url = build_authorize_url(&options, &redirect_uri, &pkce, &state)?;

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
                        handle_callback_request(request.url(), &options, &redirect_uri, &pkce, &state)
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
    let auth_url = build_authorize_url(&options, MANUAL_REDIRECT_URL, &pkce, &state)?;
    let pasted = read_pasted(&auth_url)?;
    let code = parse_manual_code(pasted.trim(), &state)?;
    exchange_code(&options, MANUAL_REDIRECT_URL, &pkce, &code, &state).await
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
    redirect_uri: &str,
    pkce: &PkceCodes,
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
    match exchange_code(options, redirect_uri, pkce, code, expected_state).await {
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

#[derive(Clone)]
pub(crate) struct PkceCodes {
    pub(crate) verifier: String,
    pub(crate) challenge: String,
}

impl fmt::Debug for PkceCodes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PkceCodes")
            .field("verifier", &"<redacted>")
            .field("challenge", &self.challenge)
            .finish()
    }
}

pub(crate) fn generate_pkce() -> PkceCodes {
    let mut bytes = [0u8; 64];
    rand::rng().fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    PkceCodes {
        verifier,
        challenge,
    }
}

fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn build_authorize_url(
    options: &LoginOptions,
    redirect_uri: &str,
    pkce: &PkceCodes,
    state: &str,
) -> io::Result<String> {
    let mut url = Url::parse(&options.authorize_url)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    url.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", &options.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPES)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    Ok(url.into())
}

#[derive(Serialize)]
struct ExchangeRequest<'a> {
    grant_type: &'static str,
    code: &'a str,
    redirect_uri: &'a str,
    client_id: &'a str,
    code_verifier: &'a str,
    state: &'a str,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    account: Option<serde_json::Value>,
    /// A `Value` so a malformed block leaves the ids empty instead of failing
    /// the sign-in.
    #[serde(default)]
    organization: Option<serde_json::Value>,
}

fn text_field(block: &serde_json::Value, key: &str) -> Option<String> {
    block
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn account_label(account: &serde_json::Value) -> Option<String> {
    ["email_address", "uuid"]
        .into_iter()
        .find_map(|key| text_field(account, key))
}

fn uuid_of(block: Option<&serde_json::Value>) -> Option<String> {
    text_field(block?, "uuid")
}

pub(crate) async fn exchange_code(
    options: &LoginOptions,
    redirect_uri: &str,
    pkce: &PkceCodes,
    code: &str,
    state: &str,
) -> io::Result<LoginTokens> {
    let request = ExchangeRequest {
        grant_type: "authorization_code",
        code,
        redirect_uri,
        client_id: &options.client_id,
        code_verifier: &pkce.verifier,
        state,
    };
    let response = reqwest::Client::new()
        .post(&options.token_url)
        .timeout(std::time::Duration::from_secs(30))
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
        .map_err(redacted_transport_error)?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| "no error code".to_string());
        return Err(io::Error::other(format!(
            "Claude token endpoint returned HTTP {status}: {detail}"
        )));
    }
    let tokens = response
        .json::<TokenResponse>()
        .await
        .map_err(|error| io::Error::other(format!("decoding Claude OAuth tokens: {error}")))?;
    Ok(LoginTokens {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_in: tokens.expires_in,
        scope: tokens.scope,
        account_id: tokens.account.as_ref().and_then(account_label),
        organization_uuid: uuid_of(tokens.organization.as_ref()),
        account_uuid: uuid_of(tokens.account.as_ref()),
    })
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
            "http://localhost:1/callback",
            &pkce,
            "code-SECRET",
            "state-1",
        )
        .await
        .unwrap_err();
        assert!(!err.to_string().contains("code-SECRET"), "{err}");
        assert!(err.to_string().contains("401"), "{err}");
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
