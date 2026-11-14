use gents_inference::backends::setup::{
    InferenceAuthMethod, InferenceModelOption, InferenceModelRecommendation, InferenceProviderId,
};
use serde::{Deserialize, Serialize};

use crate::error::{BridgeError, BridgeErrorCode};

const LOG_TARGET: &str = "gents_desktop_bridge::inference_setup";

pub use gents_desktop_core::inference::InferenceSetupState;

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
    pub provider_kind: gents_inference::backends::provider::BackendProviderKind,
    pub openai_wire_api: Option<gents_loop::openai_wire::OpenAiWireApi>,
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
    pub reasoning_efforts: Option<Vec<gents_documents::inference_defaults::ReasoningEffort>>,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct InferenceBackendRecommendationRequest {
    pub provider_kind: gents_inference::backends::provider::BackendProviderKind,
    pub endpoint: String,
    pub model_name: String,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    pub max_context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub reasoning_efforts: Option<Vec<gents_documents::inference_defaults::ReasoningEffort>>,
}

pub fn inference_model_recommendation(
    request: InferenceRecommendationRequest,
) -> Result<InferenceModelRecommendation, BridgeError> {
    let model_name = request.model_name.trim();
    if model_name.is_empty() {
        return Err(BridgeError::untyped("model name is required"));
    }
    gents_inference::backends::setup::recommendation_for_model(
        request.provider,
        request.auth_method,
        &gents_documents::AdvertisedModel {
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
    let (provider, auth_method) = gents_inference::backends::setup::provider_selection_for_backend(
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

pub async fn discover_inference_models_for_core(
    request: InferenceDiscoveryRequest,
    core: Option<&gents_desktop_core::client::ClientCore>,
) -> Result<InferenceDiscoveryResult, BridgeError> {
    use gents_desktop_core::inference::ModelDiscoveryError;
    let node_did = request.node_did.trim();
    let access = core.and_then(|core| {
        let access = core.operator_access(node_did);
        access
            .inspect_err(|error| {
                tracing::warn!(
                    target: LOG_TARGET,
                    node_did,
                    error = %format!("{error:#}"),
                    "resolving node configuration access for model discovery failed"
                );
            })
            .ok()
    });
    let accounts_unchecked = || {
        BridgeError::new(
            BridgeErrorCode::EndpointUnreachable,
            "The node is not running, so connected accounts could not be checked. Start the node and try again.",
        )
    };
    let found = gents_desktop_core::inference::discover_models(
        access.as_ref(),
        node_did,
        request.provider,
        request.auth_method,
        &request.endpoint,
        request.api_key.as_deref(),
        request.account_ref.as_deref(),
    )
    .await
    .map_err(|error| match error {
        ModelDiscoveryError::NoAccess if core.is_none() => {
            BridgeError::untyped("desktop client is not running")
        }
        ModelDiscoveryError::NoAccess => accounts_unchecked(),
        ModelDiscoveryError::Accounts(error) => {
            tracing::warn!(
                target: LOG_TARGET,
                node_did,
                error = %format!("{error:#}"),
                "reading provider accounts for model discovery failed"
            );
            accounts_unchecked()
        }
        error => BridgeError::untyped(error.to_string()),
    })?;
    Ok(InferenceDiscoveryResult {
        request_key: request.request_key,
        contract_version: gents_inference::backends::setup::INFERENCE_SETUP_CONTRACT_VERSION,
        defaults_version: gents_inference::backends::setup::INFERENCE_DEFAULTS_VERSION.into(),
        requested_endpoint: request.endpoint.trim().to_string(),
        effective_endpoint: found.spec.endpoint,
        backend_name: found.spec.backend_name.into(),
        provider_kind: found.spec.provider_kind,
        openai_wire_api: found.spec.openai_wire_api,
        reachable: found.reachable,
        models: found.models,
        manual_entry_allowed: found.manual_entry_allowed,
        failure: found
            .failure
            .map(|(kind, message)| InferenceDiscoveryFailure {
                kind: kind.into(),
                message,
            }),
    })
}
