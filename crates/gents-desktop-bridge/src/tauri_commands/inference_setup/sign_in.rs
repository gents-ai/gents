use gents::inference_setup::InferenceAuthMethod;
use gents::oauth_credential::SignIn;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio_util::sync::CancellationToken;

use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{current_core, DesktopAppState, DesktopBridge, ProviderLoginWait};

fn slot(
    bridge: &mut DesktopBridge,
    method: InferenceAuthMethod,
) -> Option<&mut Option<ProviderLoginWait>> {
    match method {
        InferenceAuthMethod::ChatGptOauth => Some(&mut bridge.codex_login_cancel),
        InferenceAuthMethod::ClaudeOauth => Some(&mut bridge.claude_login_cancel),
        InferenceAuthMethod::GrokOauth => Some(&mut bridge.grok_login_cancel),
        _ => None,
    }
}

/// Cancellation leaves the slot occupied until its callback has drained, so
/// an older completion cannot clear the next window's cancellation handle.
struct Wait<'a> {
    state: &'a DesktopAppState,
    method: InferenceAuthMethod,
    cancel: CancellationToken,
}
impl Drop for Wait<'_> {
    fn drop(&mut self) {
        self.cancel.cancel();
        let mut bridge = self
            .state
            .bridge
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(slot) = slot(&mut bridge, self.method) {
            *slot = None;
        }
    }
}

pub(super) async fn run<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    window: &str,
    node_did: &str,
    method: InferenceAuthMethod,
    provider: &str,
    label: Option<&str>,
) -> Result<SignIn, BridgeError>
where
    R: Runtime,
{
    super::sign_in_platform::require_supported(method, cfg!(target_os = "ios"))?;
    let core = current_core(state).ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::ClientNotRunning,
            "desktop client is not running",
        )
    })?;
    let cancel = CancellationToken::new();
    {
        let mut bridge = state
            .bridge
            .lock()
            .map_err(|_| BridgeError::untyped("desktop bridge lock poisoned"))?;
        let slot = slot(&mut bridge, method).ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::Unsupported,
                "Choose a browser sign-in method.",
            )
        })?;
        if slot.is_some() {
            return Err(BridgeError::untyped(
                "A sign-in is already waiting for this provider. Finish or cancel it first.",
            ));
        }
        *slot = Some(ProviderLoginWait {
            window: window.to_owned(),
            cancel: cancel.clone(),
        });
    }
    let wait = Wait {
        state,
        method,
        cancel,
    };
    let signed = crate::accounts::login::sign_in(
        &state.pending_oauth_credentials,
        || core.operator_access(node_did),
        node_did,
        method,
        provider,
        label,
        &wait.cancel,
        |url| {
            if wait.cancel.is_cancelled() || app.get_webview_window(window).is_none() {
                return Err(BridgeError::untyped("Sign-in cancelled."));
            }
            match method {
                InferenceAuthMethod::ChatGptOauth => {
                    let _ = app.emit_to(
                        window,
                        "desktop://codex-login-url",
                        super::CodexLoginUrl {
                            url: url.to_owned(),
                        },
                    );
                }
                InferenceAuthMethod::ClaudeOauth => {
                    let _ = app.emit_to(
                        window,
                        crate::contract::CLAUDE_LOGIN_URL_EVENT,
                        super::ClaudeLoginUrl {
                            url: url.to_owned(),
                        },
                    );
                }
                InferenceAuthMethod::GrokOauth => {
                    let _ = app.emit_to(
                        window,
                        crate::contract::GROK_LOGIN_URL_EVENT,
                        super::GrokLoginUrl {
                            url: url.to_owned(),
                        },
                    );
                }
                _ => {}
            }
            if crate::host_browser::open_url(url).is_err() {
                tracing::warn!("Could not open the sign-in page; open its link in Settings");
            }
            Ok(())
        },
    )
    .await?;
    super::notify_config_changed(app, &core).await;
    Ok(signed)
}

pub(super) fn cancel(
    state: &DesktopAppState,
    window: &str,
    method: InferenceAuthMethod,
) -> Result<(), BridgeError> {
    let mut bridge = state
        .bridge
        .lock()
        .map_err(|_| BridgeError::untyped("desktop bridge lock poisoned"))?;
    if let Some(Some(wait)) = slot(&mut bridge, method) {
        if wait.window == window {
            wait.cancel.cancel();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_keeps_the_slot_until_its_owner_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let config = crate::BridgeConfig {
            home: crate::HomePolicy::FixedRoot(temp.path().join("client")),
            bootstrap: crate::BootstrapPolicy::PairedRemoteOnly,
            store_key_custody: gents::store_key::StoreKeyCustodyChoice::File,
            ..Default::default()
        };
        let state = DesktopAppState::new(crate::state::resolve_policy(&config, None).unwrap());
        let token = CancellationToken::new();
        state.bridge.lock().unwrap().codex_login_cancel = Some(ProviderLoginWait {
            window: "first".into(),
            cancel: token.clone(),
        });
        let wait = Wait {
            state: &state,
            method: InferenceAuthMethod::ChatGptOauth,
            cancel: token.clone(),
        };
        cancel(&state, "second", InferenceAuthMethod::ChatGptOauth).unwrap();
        assert!(!token.is_cancelled());
        state.window_event("second", &tauri::WindowEvent::Destroyed);
        assert!(!token.is_cancelled());
        cancel(&state, "first", InferenceAuthMethod::ChatGptOauth).unwrap();
        assert!(token.is_cancelled());
        assert!(state.bridge.lock().unwrap().codex_login_cancel.is_some());
        drop(wait);
        assert!(state.bridge.lock().unwrap().codex_login_cancel.is_none());
    }
}
