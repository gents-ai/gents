use std::future::Future;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use gents::inference::auth::credential::{OAuthCredential, SignIn};
use gents::inference_setup::InferenceAuthMethod;
use tokio_util::sync::CancellationToken;

use super::{
    provider_label, require_reachable_configuration, save_issued_credential, sign_in_label,
    PendingOAuthCredentials,
};
use crate::error::{BridgeError, BridgeErrorCode};

pub(crate) fn provider(method: InferenceAuthMethod) -> Result<&'static str, BridgeError> {
    match method {
        InferenceAuthMethod::ChatGptOauth => Ok(gents::chatgpt_codex::CHATGPT_CODEX_PROVIDER),
        InferenceAuthMethod::ClaudeOauth => {
            Ok(gents::inference::auth::claude::CLAUDE_OAUTH_PROVIDER)
        }
        InferenceAuthMethod::GrokOauth => Ok(gents::xai_grok_oauth::XAI_OAUTH_PROVIDER),
        _ => Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Choose a browser sign-in method.",
        )),
    }
}

struct Shutdown<F>(F)
where
    F: Fn();
impl<F> Drop for Shutdown<F>
where
    F: Fn(),
{
    fn drop(&mut self) {
        (self.0)();
    }
}

/// Shutdown may overlap a token exchange already accepted by the provider.
/// Drain that bounded exchange and retain any issued tokens before saving;
/// cancelling the caller must not detach the callback task and lose its result.
async fn finish<F, T, E, S>(
    login: F,
    cancel: &CancellationToken,
    stop: S,
    opened: Result<(), BridgeError>,
    label: &str,
) -> Result<T, BridgeError>
where
    F: Future<Output = Result<T, E>>,
    S: Fn(),
{
    let shutdown = Shutdown(stop);
    tokio::pin!(login);
    let failure = match opened {
        Err(error) => error,
        Ok(()) => tokio::select! {
            result = &mut login => return result.map_err(|_| BridgeError::untyped(format!("{label} sign-in failed. Start sign-in again."))),
            _ = cancel.cancelled() => BridgeError::untyped("Sign-in cancelled."),
            _ = tokio::time::sleep(Duration::from_secs(300)) => BridgeError::untyped(format!("{label} sign-in timed out waiting for the browser.")),
        },
    };
    (shutdown.0)();
    login.await.map_err(|_| failure)
}

#[expect(
    clippy::too_many_arguments,
    reason = "sign-in inputs from the IPC request plus the host's callbacks"
)]
pub(crate) async fn sign_in<A, O>(
    pending: &PendingOAuthCredentials,
    access: A,
    node_did: &str,
    method: InferenceAuthMethod,
    credential_provider: &str,
    label: Option<&str>,
    cancel: &CancellationToken,
    on_url: O,
) -> Result<SignIn, BridgeError>
where
    A: Fn() -> anyhow::Result<gents::ConfigAccess>,
    O: AsyncFnOnce(&str) -> Result<(), BridgeError>,
{
    provider(method)?;
    if node_did.trim().is_empty() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Choose the runtime that owns this sign-in.",
        ));
    }
    let label = sign_in_label(label)?;
    if cancel.is_cancelled() {
        return Err(BridgeError::untyped("Sign-in cancelled."));
    }
    require_reachable_configuration(access(), node_did, credential_provider).await?;
    if cancel.is_cancelled() {
        return Err(BridgeError::untyped("Sign-in cancelled."));
    }
    let product = provider_label(credential_provider);
    let credential = match method {
        InferenceAuthMethod::ChatGptOauth => {
            let server = gents_chatgpt_login::run_login_server(gents_chatgpt_login::LoginOptions {
                open_browser: false, ..Default::default()
            }).map_err(|_| BridgeError::untyped("Could not start the ChatGPT sign-in callback. Close another sign-in and try again."))?;
            let stop = server.cancel_handle();
            let opened = on_url(&server.auth_url).await;
            let tokens = finish(
                server.block_until_done(),
                cancel,
                || stop.shutdown(),
                opened,
                product,
            )
            .await?;
            OAuthCredential {
                label,
                ..OAuthCredential::from_login_tokens(
                    node_did,
                    credential_provider,
                    &tokens.id_token,
                    tokens.access_token,
                    tokens.refresh_token,
                    chrono::Utc::now(),
                )
            }
        }
        InferenceAuthMethod::ClaudeOauth => {
            let server = gents_claude_login::run_loopback_login(gents_claude_login::LoginOptions {
                open_browser: false, ..Default::default()
            }).map_err(|_| BridgeError::untyped("Could not start the Claude sign-in callback. Close another sign-in and try again."))?;
            let stop = server.cancel_handle();
            let opened = on_url(&server.auth_url).await;
            let tokens = finish(
                server.block_until_done(),
                cancel,
                || stop.shutdown(),
                opened,
                product,
            )
            .await?;
            let tokens = gents::inference::auth::claude::ClaudeLoginTokens {
                access_token: tokens.access_token,
                refresh_token: tokens.refresh_token,
                expires_in: tokens.expires_in,
                scope: tokens.scope,
                account_id: tokens.account_id,
                organization_uuid: tokens.organization_uuid,
                account_uuid: tokens.account_uuid,
            };
            OAuthCredential {
                label,
                ..gents::inference::auth::claude::credential_from_login_tokens(
                    node_did,
                    credential_provider,
                    &tokens,
                    chrono::Utc::now(),
                )
            }
        }
        InferenceAuthMethod::GrokOauth => {
            let stop = Arc::new(AtomicBool::new(false));
            let failed = Arc::new(Mutex::new(None));
            let callback_stop = stop.clone();
            let callback_failed = failed.clone();
            let http = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|_| BridgeError::untyped("Could not prepare the sign-in connection."))?;
            let login = gents::inference::auth::xai_login::run_device_code_login_with_url_callback(
                &http,
                Some(stop.clone()),
                async move |url| {
                    if callback_stop.load(Ordering::SeqCst) {
                        return;
                    }
                    if let Err(error) = on_url(url).await {
                        *callback_failed
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                        callback_stop.store(true, Ordering::SeqCst);
                    }
                },
            );
            let tokens = finish(
                login,
                cancel,
                || stop.store(true, Ordering::SeqCst),
                Ok(()),
                product,
            )
            .await;
            let tokens = tokens.map_err(|error| {
                failed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                    .unwrap_or(error)
            })?;
            OAuthCredential {
                label,
                ..gents::inference::auth::xai_login::credential_from_login_tokens(
                    node_did,
                    credential_provider,
                    &tokens,
                    chrono::Utc::now(),
                )
            }
        }
        _ => unreachable!("browser method checked before starting sign-in"),
    };
    save_issued_credential(pending, access(), pending.issue(credential)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn already_cancelled_sign_in_does_not_probe_or_open_the_browser() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = sign_in(
            &PendingOAuthCredentials::default(),
            || panic!("cancelled sign-in must not read configuration"),
            "did:test:node",
            InferenceAuthMethod::GrokOauth,
            gents::xai_grok_oauth::XAI_OAUTH_PROVIDER,
            None,
            &cancel,
            async |_| panic!("cancelled sign-in must not open a browser"),
        )
        .await;
        assert!(result.unwrap_err().message.contains("cancelled"));
    }

    #[tokio::test]
    async fn cancellation_drains_and_retains_an_exchange_that_issued_tokens() {
        let cancel = CancellationToken::new();
        let released = Arc::new(tokio::sync::Notify::new());
        let work = released.clone();
        let finish = finish(
            async move {
                work.notified().await;
                Ok::<_, ()>("issued")
            },
            &cancel,
            || released.notify_one(),
            Ok(()),
            "Provider",
        );
        tokio::pin!(finish);
        assert!(futures::poll!(&mut finish).is_pending());
        cancel.cancel();
        assert_eq!(finish.await.unwrap(), "issued");
    }

    #[tokio::test]
    async fn callback_failure_is_redacted_when_no_tokens_were_issued() {
        let result = finish(
            async { Err::<(), _>("private callback data") },
            &CancellationToken::new(),
            || {},
            Ok(()),
            "Provider",
        )
        .await
        .unwrap_err();
        assert!(!result.message.contains("private callback data"));
    }
}
