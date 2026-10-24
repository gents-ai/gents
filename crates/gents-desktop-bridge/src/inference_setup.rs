use gents_protocol::error_chain::ErrorChain;
use std::time::Duration;

use gents::inference_setup::{
    InferenceAuthMethod, InferenceModelOption, InferenceModelRecommendation, InferenceProviderId,
};
use gents::oauth_credential::OAuthCredential;
use serde::{Deserialize, Serialize};

use crate::error::{BridgeError, BridgeErrorCode};

const LOG_TARGET: &str = "gents_desktop_bridge::inference_setup";

pub use gents_desktop_core::inference::InferenceSetupState;

impl From<crate::types::DeploymentView> for InferenceSetupState {
    fn from(node: crate::types::DeploymentView) -> Self {
        let fast_mode_backend_ids =
            gents_desktop_core::inference::fast_mode_backend_ids(&node.inference_backends);
        Self {
            fast_mode_backend_ids,
            node_did: node.node_did,
            node: node.node,
            agents: node.agents,
            agent_configs: node.agent_configs,
            inference_backends: node.inference_backends,
            inference_profiles: node.inference_profiles,
        }
    }
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct InferenceDiscoveryFailure {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct InferenceDiscoveryRequest {
    pub request_key: String,
    pub node_did: String,
    pub provider: InferenceProviderId,
    pub auth_method: InferenceAuthMethod,
    pub endpoint: String,
    pub api_key: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub account_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct InferenceDiscoveryResult {
    pub request_key: String,
    pub contract_version: u32,
    pub defaults_version: String,
    pub requested_endpoint: String,
    pub effective_endpoint: String,
    pub backend_name: String,
    pub provider_kind: gents::BackendProviderKind,
    pub openai_wire_api: Option<gents::OpenAiWireApi>,
    pub reachable: bool,
    pub models: Vec<InferenceModelOption>,
    pub failure: Option<InferenceDiscoveryFailure>,
    pub manual_entry_allowed: bool,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct InferenceRecommendationRequest {
    pub provider: InferenceProviderId,
    pub auth_method: InferenceAuthMethod,
    pub model_name: String,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    pub max_context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub reasoning_efforts: Option<Vec<gents::config::ReasoningEffort>>,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct InferenceBackendRecommendationRequest {
    pub provider_kind: gents::BackendProviderKind,
    pub endpoint: String,
    pub model_name: String,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    pub max_context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub reasoning_efforts: Option<Vec<gents::config::ReasoningEffort>>,
}

pub fn inference_model_recommendation(
    request: InferenceRecommendationRequest,
) -> Result<InferenceModelRecommendation, BridgeError> {
    let model_name = request.model_name.trim();
    if model_name.is_empty() {
        return Err(BridgeError::untyped("model name is required"));
    }
    gents::inference_setup::recommendation_for_model(
        request.provider,
        request.auth_method,
        &gents::document_config::AdvertisedModel {
            model_name: model_name.to_string(),
            display_name: request.display_name,
            context_window: request.context_window,
            max_context_window: request.max_context_window,
            max_output_tokens: request.max_output_tokens,
            reasoning_efforts: request.reasoning_efforts,
        },
    )
    .map_err(|error| BridgeError::untyped(error.to_string()))
}

pub fn inference_backend_recommendation(
    request: InferenceBackendRecommendationRequest,
) -> Result<InferenceModelRecommendation, BridgeError> {
    let (provider, auth_method) = gents::inference_setup::provider_selection_for_backend(
        request.provider_kind,
        &request.endpoint,
    );
    inference_model_recommendation(InferenceRecommendationRequest {
        provider,
        auth_method,
        model_name: request.model_name,
        display_name: request.display_name,
        context_window: request.context_window,
        max_context_window: request.max_context_window,
        max_output_tokens: request.max_output_tokens,
        reasoning_efforts: request.reasoning_efforts,
    })
}

fn classify_discovery_failure(
    error: &gents::backend_provider::BackendProviderError,
) -> (&'static str, bool, bool) {
    let http_error = error.model_discovery_http();
    let is_decode = gents_protocol::error_chain::find_source::<serde_json::Error>(error).is_some();
    if let Some(error) = http_error {
        if error.is_auth() {
            ("authentication", true, false)
        } else {
            ("http", true, true)
        }
    } else if is_decode {
        ("decode", true, true)
    } else {
        ("connection", false, false)
    }
}

pub async fn discover_inference_models_for_core(
    request: InferenceDiscoveryRequest,
    core: Option<&gents_desktop_core::client::ClientCore>,
) -> Result<InferenceDiscoveryResult, BridgeError> {
    let spec = gents::inference_setup::connection_spec(
        request.provider,
        request.auth_method,
        &request.endpoint,
    )
    .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let api_key = request
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if spec.api_key_required && api_key.is_none() {
        return Err(BridgeError::untyped("API key is required"));
    }

    let (credential, oauth_auth) = if let Some(provider) = spec.oauth_provider {
        let core = core.ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
        let access = core.operator_access(request.node_did.trim()).map_err(|error| {
            tracing::warn!(
                target: LOG_TARGET,
                node_did = %request.node_did.trim(),
                error = %ErrorChain(error.as_ref()),
                "resolving node configuration access for model discovery failed"
            );
            BridgeError::new(
                BridgeErrorCode::EndpointUnreachable,
                "The node is not running, so connected accounts could not be checked. Start the node and try again.",
            )
        })?;
        let (credential, auth) = discovery_account(
            &access,
            request.node_did.trim(),
            provider,
            request.account_ref.as_deref(),
        )
        .await
        .map_err(|error| {
            tracing::warn!(
                target: LOG_TARGET,
                node_did = %request.node_did.trim(),
                error = %ErrorChain(&error),
                "reading provider accounts for model discovery failed"
            );
            BridgeError::new(
                BridgeErrorCode::EndpointUnreachable,
                "The node is not running, so connected accounts could not be checked. Start the node and try again.",
            )
        })?;
        (credential, Some(auth))
    } else {
        (None, None)
    };
    if spec.oauth_provider.is_some() && credential.is_none() {
        return Err(BridgeError::untyped(
            "the selected provider account is not connected",
        ));
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let requested_endpoint = request.endpoint.trim().to_string();
    let discovered = gents::discover_backend_models(
        &client,
        spec.provider_kind,
        &spec.endpoint,
        api_key,
        credential.as_ref(),
    )
    .await;

    if let (Ok(models), Some(core)) = (&discovered, core) {
        publish_discovered_catalog(core, &request, &spec, api_key, oauth_auth, models.clone())
            .await;
    }

    let (reachable, models, failure, manual_entry_allowed) = match discovered {
        Ok(models) if models.is_empty() => (
            true,
            Vec::new(),
            Some(InferenceDiscoveryFailure {
                kind: "empty_catalog".into(),
                message: "The connection succeeded, but the provider advertised no models.".into(),
            }),
            true,
        ),
        Ok(models) => {
            let models = models
                .into_iter()
                .map(|model| {
                    gents::inference_setup::model_option(
                        request.provider,
                        request.auth_method,
                        model,
                    )
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| BridgeError::untyped(error.to_string()))?;
            (true, models, None, false)
        }
        Err(error) => {
            let (kind, reachable, manual_entry_allowed) = classify_discovery_failure(&error);
            (
                reachable,
                Vec::new(),
                Some(InferenceDiscoveryFailure {
                    kind: kind.into(),
                    message: gents_protocol::error_chain::ErrorChain(&error).to_string(),
                }),
                manual_entry_allowed,
            )
        }
    };

    Ok(InferenceDiscoveryResult {
        request_key: request.request_key,
        contract_version: gents::inference_setup::INFERENCE_SETUP_CONTRACT_VERSION,
        defaults_version: gents::inference_setup::INFERENCE_DEFAULTS_VERSION.into(),
        requested_endpoint,
        effective_endpoint: spec.endpoint,
        backend_name: spec.backend_name.into(),
        provider_kind: spec.provider_kind,
        openai_wire_api: spec.openai_wire_api,
        reachable,
        models,
        manual_entry_allowed,
        failure,
    })
}

pub(crate) async fn discovery_account(
    access: &gents::ConfigAccess,
    node_did: &str,
    provider: &str,
    account_ref: Option<&str>,
) -> Result<
    (Option<OAuthCredential>, gents::document_config::BackendAuth),
    gents::oauth_credential::OAuthCredentialError,
> {
    let credential = gents::oauth_credential::resolve_oauth_credential(
        access,
        node_did,
        provider,
        gents::oauth_credential::AccountPick::Reference(account_ref),
    )
    .await?;
    Ok((
        credential,
        gents::document_config::BackendAuth::NodeOAuth {
            account_ref: account_ref.map(str::to_owned),
        },
    ))
}

async fn publish_discovered_catalog(
    core: &gents_desktop_core::client::ClientCore,
    request: &InferenceDiscoveryRequest,
    spec: &gents::inference_setup::InferenceConnectionSpec,
    api_key: Option<&str>,
    oauth_auth: Option<gents::document_config::BackendAuth>,
    models: Vec<gents::document_config::AdvertisedModel>,
) {
    use gents::document_config::BackendAuth;
    let node_did = request.node_did.trim();
    let auth = match (oauth_auth, api_key) {
        (Some(auth), _) => auth,
        (None, Some(key)) => BackendAuth::ApiKey {
            key: key.to_string(),
        },
        (None, None) => BackendAuth::Unauthenticated,
    };
    let result = match core.operator_access(node_did) {
        Ok(access) => {
            gents::backend_registry::record_connection_catalog_on(
                &access,
                node_did,
                spec.provider_kind,
                &spec.endpoint,
                &auth,
                models,
            )
            .await
        }
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        tracing::warn!(
            target: LOG_TARGET,
            node_did,
            error = %ErrorChain(error.as_ref()),
            "publishing the discovered model catalog failed; the previous catalog is kept"
        );
    }
}
