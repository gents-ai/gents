use gents::inference::backends::setup::{InferenceAuthMethod, InferenceSetupCatalog};

use crate::error::{BridgeError, BridgeErrorCode};

/// The desktop callback servers require a host browser and a loopback listener.
/// iOS sign-in needs a native authentication session before these methods can
/// be offered there. Device-code and API-key methods do not use that callback.
fn supported(method: InferenceAuthMethod, ios: bool) -> bool {
    match method {
        InferenceAuthMethod::ChatGptOauth | InferenceAuthMethod::ClaudeOauth => !ios,
        InferenceAuthMethod::ApiKey
        | InferenceAuthMethod::GrokOauth
        | InferenceAuthMethod::OptionalApiKey => true,
    }
}

pub(super) fn setup_catalog(ios: bool) -> InferenceSetupCatalog {
    let mut catalog = gents::inference::backends::setup::inference_setup_catalog();
    for provider in &mut catalog.providers {
        provider
            .auth_methods
            .retain(|method| supported(*method, ios));
        provider
            .auth_options
            .retain(|option| supported(option.method, ios));
        if !supported(provider.default_auth_method, ios) {
            if let Some(option) = provider.auth_options.first() {
                provider.default_auth_method = option.method;
                provider.default_endpoint = option.default_endpoint.clone();
                provider.description = format!("Use an {}.", option.display_name);
            }
        }
    }
    catalog
        .providers
        .retain(|provider| !provider.auth_options.is_empty());
    catalog
}

pub(super) fn require_supported(method: InferenceAuthMethod, ios: bool) -> Result<(), BridgeError> {
    if supported(method, ios) {
        return Ok(());
    }
    Err(BridgeError::new(
        BridgeErrorCode::Unsupported,
        "Browser sign-in is not available on iOS. Use an API key or connect this account from the desktop app.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::inference::backends::setup::InferenceProviderId;

    #[test]
    fn desktop_retains_the_canonical_catalog() {
        assert_eq!(
            setup_catalog(false),
            gents::inference::backends::setup::inference_setup_catalog()
        );
    }

    #[test]
    fn ios_offers_only_sign_ins_the_bridge_accepts() {
        let catalog = setup_catalog(true);
        for provider in &catalog.providers {
            assert!(!provider.auth_options.is_empty());
            assert!(provider
                .auth_methods
                .contains(&provider.default_auth_method));
            let default = provider
                .auth_options
                .iter()
                .find(|option| option.method == provider.default_auth_method)
                .unwrap();
            assert_eq!(provider.default_endpoint, default.default_endpoint);
            for option in &provider.auth_options {
                assert!(provider.auth_methods.contains(&option.method));
                require_supported(option.method, true).unwrap();
            }
        }
        let openai = catalog
            .providers
            .iter()
            .find(|provider| provider.id == InferenceProviderId::OpenAi)
            .unwrap();
        assert_eq!(openai.default_auth_method, InferenceAuthMethod::ApiKey);
        assert_eq!(
            openai.default_endpoint,
            gents::inference::backends::setup::OPENAI_ENDPOINT
        );
        assert!(catalog.providers.iter().any(|provider| provider
            .auth_methods
            .contains(&InferenceAuthMethod::GrokOauth)));
    }

    #[test]
    fn ios_refuses_callback_sign_ins_without_a_retry() {
        for method in [
            InferenceAuthMethod::ChatGptOauth,
            InferenceAuthMethod::ClaudeOauth,
        ] {
            let error = require_supported(method, true).unwrap_err();
            assert_eq!(error.code, BridgeErrorCode::Unsupported);
            assert!(!error.retryable);
            assert!(error.message.contains("iOS"));
            require_supported(method, false).unwrap();
        }
    }
}
