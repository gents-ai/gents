//! Versioned provider/model guidance shared by first-run onboarding and later
//! inference editing. Provider responses remain advertised facts; every value
//! Gents recommends lives here and is labelled with this catalog version.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::backend_provider::BackendProviderKind;
use crate::config::ReasoningEffort;
use crate::document_config::AdvertisedModel;
use crate::openai_wire::OpenAiWireApi;

pub const INFERENCE_SETUP_CONTRACT_VERSION: u32 = 1;
pub const INFERENCE_DEFAULTS_VERSION: &str = "2026-09-15.1";

pub const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1";
pub const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1";
pub const LOCAL_DEFAULT_ENDPOINT: &str = "http://127.0.0.1:11434/v1";
pub const GROK_ENDPOINT: &str = "https://cli-chat-proxy.grok.com/v1";
pub const CLAUDE_ENDPOINT: &str = "claude-cli://subscription";
pub const CODEX_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum InferenceProviderId {
    #[serde(rename = "openai")]
    OpenAi,
    Anthropic,
    Grok,
    Local,
    #[serde(rename = "openrouter")]
    OpenRouter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum InferenceAuthMethod {
    ChatGptOauth,
    ApiKey,
    ClaudeOauth,
    GrokOauth,
    OptionalApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceProviderOption {
    pub id: InferenceProviderId,
    pub display_name: String,
    pub description: String,
    pub auth_methods: Vec<InferenceAuthMethod>,
    pub auth_options: Vec<InferenceAuthOption>,
    pub default_auth_method: InferenceAuthMethod,
    pub default_endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceAuthOption {
    pub method: InferenceAuthMethod,
    pub display_name: String,
    pub default_endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceSetupCatalog {
    pub contract_version: u32,
    pub defaults_version: String,
    pub providers: Vec<InferenceProviderOption>,
    pub execution_defaults: std::collections::BTreeMap<String, Option<i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceConnectionSpec {
    pub backend_name: &'static str,
    pub provider_kind: BackendProviderKind,
    pub openai_wire_api: Option<OpenAiWireApi>,
    pub endpoint: String,
    pub oauth_provider: Option<&'static str>,
    pub api_key_required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct RecommendedNumberControl {
    pub recommended: f64,
    pub min: f64,
    pub max: Option<f64>,
    pub step: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct RecommendedIntegerControl {
    pub recommended: i64,
    pub min: i64,
    pub max: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct RecommendedReasoningControl {
    pub recommended: ReasoningEffort,
    pub choices: Vec<ReasoningEffort>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceModelRecommendation {
    pub defaults_version: String,
    pub summary: String,
    pub context_window: Option<RecommendedIntegerControl>,
    pub max_output_tokens: Option<RecommendedIntegerControl>,
    pub temperature: Option<RecommendedNumberControl>,
    pub top_p: Option<RecommendedNumberControl>,
    pub reasoning_effort: Option<RecommendedReasoningControl>,
    pub max_concurrent: RecommendedIntegerControl,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceModelOption {
    /// Facts returned by the provider. Unknown facts stay `None`.
    pub advertised: AdvertisedModel,
    /// Versioned Gents guidance, never presented as provider-advertised data.
    pub recommendation: InferenceModelRecommendation,
}

pub fn inference_setup_catalog() -> InferenceSetupCatalog {
    use InferenceAuthMethod::*;
    use InferenceProviderId::*;

    InferenceSetupCatalog {
        contract_version: INFERENCE_SETUP_CONTRACT_VERSION,
        defaults_version: INFERENCE_DEFAULTS_VERSION.to_string(),
        execution_defaults: [
            ("maxTurns", Some(crate::config::DEFAULT_MAX_TURNS as i64)),
            ("maxTotalTokens", None),
            (
                "streamBatchMs",
                Some(crate::config::DEFAULT_STREAM_BATCH_MS as i64),
            ),
            (
                "streamLivenessSecs",
                Some(crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS as i64),
            ),
            (
                "deadlineSecs",
                Some(crate::config::DEFAULT_DEADLINE_DURATION_SECS as i64),
            ),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect(),
        providers: vec![
            InferenceProviderOption {
                id: OpenAi,
                display_name: "OpenAI".into(),
                description: "Sign in with ChatGPT or use an OpenAI API key.".into(),
                auth_methods: vec![ChatGptOauth, ApiKey],
                auth_options: vec![
                    InferenceAuthOption {
                        method: ChatGptOauth,
                        display_name: "ChatGPT sign-in".into(),
                        default_endpoint: CODEX_ENDPOINT.into(),
                    },
                    InferenceAuthOption {
                        method: ApiKey,
                        display_name: "OpenAI API key".into(),
                        default_endpoint: OPENAI_ENDPOINT.into(),
                    },
                ],
                default_auth_method: ChatGptOauth,
                default_endpoint: CODEX_ENDPOINT.into(),
            },
            InferenceProviderOption {
                id: Anthropic,
                display_name: "Anthropic".into(),
                description: "Use a Claude Pro or Max subscription.".into(),
                auth_methods: vec![ClaudeOauth],
                auth_options: vec![InferenceAuthOption {
                    method: ClaudeOauth,
                    display_name: "Claude sign-in".into(),
                    default_endpoint: CLAUDE_ENDPOINT.into(),
                }],
                default_auth_method: ClaudeOauth,
                default_endpoint: CLAUDE_ENDPOINT.into(),
            },
            InferenceProviderOption {
                id: Grok,
                display_name: "Grok".into(),
                description: "Use SuperGrok or an eligible X Premium+ subscription.".into(),
                auth_methods: vec![GrokOauth],
                auth_options: vec![InferenceAuthOption {
                    method: GrokOauth,
                    display_name: "Grok sign-in".into(),
                    default_endpoint: GROK_ENDPOINT.into(),
                }],
                default_auth_method: GrokOauth,
                default_endpoint: GROK_ENDPOINT.into(),
            },
            InferenceProviderOption {
                id: Local,
                display_name: "Local".into(),
                description: "Ollama, llama.cpp, vLLM, or another OpenAI-compatible server.".into(),
                auth_methods: vec![OptionalApiKey],
                auth_options: vec![InferenceAuthOption {
                    method: OptionalApiKey,
                    display_name: "Endpoint + optional key".into(),
                    default_endpoint: LOCAL_DEFAULT_ENDPOINT.into(),
                }],
                default_auth_method: OptionalApiKey,
                default_endpoint: LOCAL_DEFAULT_ENDPOINT.into(),
            },
            InferenceProviderOption {
                id: OpenRouter,
                display_name: "OpenRouter".into(),
                description: "Use one API key for OpenRouter's advertised models.".into(),
                auth_methods: vec![ApiKey],
                auth_options: vec![InferenceAuthOption {
                    method: ApiKey,
                    display_name: "OpenRouter API key".into(),
                    default_endpoint: OPENROUTER_ENDPOINT.into(),
                }],
                default_auth_method: ApiKey,
                default_endpoint: OPENROUTER_ENDPOINT.into(),
            },
        ],
    }
}

pub fn connection_spec(
    provider: InferenceProviderId,
    auth: InferenceAuthMethod,
    endpoint: &str,
) -> Result<InferenceConnectionSpec> {
    use InferenceAuthMethod::*;
    use InferenceProviderId::*;

    let requested = endpoint.trim();
    let spec = match (provider, auth) {
        (OpenAi, ChatGptOauth) => InferenceConnectionSpec {
            backend_name: "ChatGPT",
            provider_kind: BackendProviderKind::ChatGptCodex,
            openai_wire_api: Some(OpenAiWireApi::Responses),
            endpoint: if requested.is_empty() {
                CODEX_ENDPOINT
            } else {
                requested
            }
            .into(),
            oauth_provider: Some(crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER),
            api_key_required: false,
        },
        (OpenAi, ApiKey) => InferenceConnectionSpec {
            backend_name: "OpenAI",
            provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: Some(OpenAiWireApi::Responses),
            endpoint: if requested.is_empty() {
                OPENAI_ENDPOINT
            } else {
                requested
            }
            .into(),
            oauth_provider: None,
            api_key_required: true,
        },
        (Anthropic, ClaudeOauth) => InferenceConnectionSpec {
            backend_name: "Anthropic",
            provider_kind: BackendProviderKind::ClaudeCliSubscription,
            openai_wire_api: None,
            endpoint: if requested.is_empty() {
                CLAUDE_ENDPOINT
            } else {
                requested
            }
            .into(),
            oauth_provider: Some(crate::claude_oauth::CLAUDE_OAUTH_PROVIDER),
            api_key_required: false,
        },
        (Grok, GrokOauth) => InferenceConnectionSpec {
            backend_name: "Grok",
            provider_kind: BackendProviderKind::XaiGrokOAuth,
            openai_wire_api: Some(OpenAiWireApi::ChatCompletions),
            endpoint: if requested.is_empty() {
                GROK_ENDPOINT
            } else {
                requested
            }
            .into(),
            oauth_provider: Some(crate::xai_grok_oauth::XAI_OAUTH_PROVIDER),
            api_key_required: false,
        },
        (Local, OptionalApiKey) => InferenceConnectionSpec {
            backend_name: "Local server",
            provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: Some(OpenAiWireApi::ChatCompletions),
            endpoint: if requested.is_empty() {
                LOCAL_DEFAULT_ENDPOINT
            } else {
                requested
            }
            .into(),
            oauth_provider: None,
            api_key_required: false,
        },
        (OpenRouter, ApiKey) => InferenceConnectionSpec {
            backend_name: "OpenRouter",
            provider_kind: BackendProviderKind::OpenRouter,
            openai_wire_api: Some(OpenAiWireApi::ChatCompletions),
            endpoint: if requested.is_empty() {
                OPENROUTER_ENDPOINT
            } else {
                requested
            }
            .into(),
            oauth_provider: None,
            api_key_required: true,
        },
        _ => anyhow::bail!("authentication method is incompatible with the selected provider"),
    };
    anyhow::ensure!(!spec.endpoint.trim().is_empty(), "endpoint is required");
    Ok(spec)
}

/// Recover the setup vocabulary for an existing canonical backend. This keeps
/// the post-onboarding editor on the same provider contract as first run.
pub fn provider_selection_for_backend(
    provider_kind: BackendProviderKind,
    endpoint: &str,
) -> (InferenceProviderId, InferenceAuthMethod) {
    match provider_kind {
        BackendProviderKind::ChatGptCodex => (
            InferenceProviderId::OpenAi,
            InferenceAuthMethod::ChatGptOauth,
        ),
        BackendProviderKind::XaiGrokOAuth => {
            (InferenceProviderId::Grok, InferenceAuthMethod::GrokOauth)
        }
        BackendProviderKind::ClaudeCliSubscription => (
            InferenceProviderId::Anthropic,
            InferenceAuthMethod::ClaudeOauth,
        ),
        BackendProviderKind::OpenRouter => {
            (InferenceProviderId::OpenRouter, InferenceAuthMethod::ApiKey)
        }
        BackendProviderKind::OpenAiCompatible
            if endpoint
                .trim_end_matches('/')
                .eq_ignore_ascii_case(OPENAI_ENDPOINT) =>
        {
            (InferenceProviderId::OpenAi, InferenceAuthMethod::ApiKey)
        }
        BackendProviderKind::OpenAiCompatible => (
            InferenceProviderId::Local,
            InferenceAuthMethod::OptionalApiKey,
        ),
    }
}

fn reasoning_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.contains("gpt-5")
        || model.contains("deepseek-r1")
        || model.contains("glm-5.3")
}

/// Reviewed fallback for catalog responses that omit capability metadata.
/// Sources: platform.claude.com/docs/en/models/overview and /build-with-claude/effort.
/// Unknown models do not inherit capabilities from a family-name guess.
pub(crate) fn claude_model_defaults(
    model: &str,
) -> Option<(i64, i64, Vec<ReasoningEffort>, ReasoningEffort)> {
    use ReasoningEffort::{High, Low, Max, Medium, XHigh};
    let model = model.strip_suffix("-20251001").unwrap_or(model);
    match model {
        "claude-fable-5-1" | "claude-fable-5" | "claude-mythos-5-1" | "claude-mythos-5"
        | "claude-opus-5" | "claude-sonnet-5" => Some((
            1_000_000,
            128_000,
            vec![Low, Medium, High, XHigh, Max],
            High,
        )),
        "claude-opus-4-7" | "claude-opus-4-8" => Some((
            1_000_000,
            128_000,
            vec![Low, Medium, High, XHigh, Max],
            XHigh,
        )),
        "claude-opus-4-6" => Some((1_000_000, 128_000, vec![Low, Medium, High, Max], High)),
        "claude-sonnet-4-6" => Some((1_000_000, 128_000, vec![Low, Medium, High, Max], Medium)),
        "claude-haiku-4-5" => Some((200_000, 64_000, vec![], High)),
        _ => None,
    }
}

/// Resolve Claude effort support once from authoritative advertised metadata,
/// falling back only to the reviewed exact-model table when metadata is absent.
/// `Some([])` is authoritative unsupported; unknown models are never guessed.
pub(crate) fn claude_supported_reasoning_efforts(
    model: &str,
    advertised: Option<&[ReasoningEffort]>,
) -> Option<Vec<ReasoningEffort>> {
    advertised
        .map(|values| values.to_vec())
        .or_else(|| claude_model_defaults(model).map(|(_, _, supported, _)| supported))
}

pub fn recommendation_for_model(
    provider: InferenceProviderId,
    auth: InferenceAuthMethod,
    advertised: &AdvertisedModel,
) -> Result<InferenceModelRecommendation> {
    let spec = connection_spec(provider, auth, "")?;
    let is_reasoning = reasoning_model(&advertised.model_name);
    let is_claude = spec.provider_kind == BackendProviderKind::ClaudeCliSubscription;
    let is_codex = spec.provider_kind == BackendProviderKind::ChatGptCodex;
    let is_grok = provider == InferenceProviderId::Grok;
    let claude_defaults = is_claude
        .then(|| claude_model_defaults(&advertised.model_name))
        .flatten();
    let mut advertised = advertised.clone();
    if let Some((context, output, _, _)) = &claude_defaults {
        advertised.context_window = advertised.context_window.or(Some(*context));
        advertised.max_output_tokens = advertised.max_output_tokens.or(Some(*output));
    }

    let reasoning_effort = if is_claude {
        claude_supported_reasoning_efforts(
            &advertised.model_name,
            advertised.reasoning_efforts.as_deref(),
        )
        .map(|choices| {
            let default = claude_defaults
                .as_ref()
                .map(|(_, _, _, default)| *default)
                .unwrap_or(ReasoningEffort::High);
            (choices, default)
        })
        .and_then(|(choices, default)| {
            (!choices.is_empty()).then(|| RecommendedReasoningControl {
                recommended: if choices.contains(&default) {
                    default
                } else {
                    choices[0]
                },
                choices,
            })
        })
    } else if is_codex || is_reasoning {
        let choices = advertised.reasoning_efforts.clone().unwrap_or_else(|| {
            vec![
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
            ]
        });
        (!choices.is_empty()).then(|| RecommendedReasoningControl {
            recommended: if choices.contains(&ReasoningEffort::Medium) {
                ReasoningEffort::Medium
            } else {
                choices[0]
            },
            choices,
        })
    } else {
        None
    };

    // Claude Messages deliberately sends no sampling controls. Reasoning-style
    // OpenAI models also reject or ignore ordinary temperature/top-p settings.
    let supports_sampling = !is_claude && !is_codex && !is_reasoning;
    let fixture = advertised.model_name == "GLM-5.3-Flash-NVFP4";
    let temperature = (supports_sampling || fixture).then(|| RecommendedNumberControl {
        recommended: if fixture { 1.0 } else { 0.7 },
        min: 0.0,
        max: Some(2.0),
        step: 0.05,
    });
    let top_p = (supports_sampling || fixture).then(|| RecommendedNumberControl {
        recommended: if fixture || is_grok { 0.95 } else { 1.0 },
        min: 0.0,
        max: Some(1.0),
        step: 0.05,
    });

    let summary = if fixture {
        "Gents recommends temperature 1 and top-p 0.95 for this workstation model."
    } else if is_claude && reasoning_effort.is_some() {
        "Anthropic model-specific thinking defaults; sampling is provider managed."
    } else if reasoning_effort.is_some() {
        "Gents recommends medium reasoning; sampling controls are hidden for this model."
    } else if is_claude {
        "Claude subscription uses the provider's sampling and thinking defaults."
    } else {
        "Gents recommends a balanced sampling preset; the provider remains authoritative."
    };

    Ok(InferenceModelRecommendation {
        defaults_version: INFERENCE_DEFAULTS_VERSION.into(),
        summary: summary.into(),
        context_window: advertised
            .context_window
            .map(|value| RecommendedIntegerControl {
                recommended: value,
                min: 1,
                max: Some(
                    advertised
                        .max_context_window
                        .filter(|maximum| *maximum >= value)
                        .unwrap_or(value),
                ),
            }),
        max_output_tokens: advertised
            .max_output_tokens
            .map(|value| RecommendedIntegerControl {
                recommended: if is_claude { value.min(64_000) } else { value },
                min: 1,
                max: Some(value),
            }),
        temperature,
        top_p,
        reasoning_effort,
        max_concurrent: RecommendedIntegerControl {
            recommended: if provider == InferenceProviderId::Local {
                1
            } else {
                8
            },
            min: 1,
            max: None,
        },
    })
}

pub fn model_option(
    provider: InferenceProviderId,
    auth: InferenceAuthMethod,
    advertised: AdvertisedModel,
) -> Result<InferenceModelOption> {
    let recommendation = recommendation_for_model(provider, auth, &advertised)?;
    Ok(InferenceModelOption {
        advertised,
        recommendation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advertised(name: &str) -> AdvertisedModel {
        AdvertisedModel {
            model_name: name.into(),
            display_name: None,
            context_window: None,
            max_context_window: None,
            max_output_tokens: None,
            reasoning_efforts: None,
        }
    }

    #[test]
    fn provider_catalog_is_versioned_and_owns_connection_defaults() {
        let catalog = inference_setup_catalog();
        assert_eq!(catalog.contract_version, 1);
        assert_eq!(catalog.defaults_version, INFERENCE_DEFAULTS_VERSION);
        assert_eq!(catalog.providers.len(), 5);
        let local = catalog
            .providers
            .iter()
            .find(|provider| provider.id == InferenceProviderId::Local)
            .unwrap();
        assert_eq!(local.default_endpoint, LOCAL_DEFAULT_ENDPOINT);
        assert_eq!(
            local.auth_methods,
            vec![InferenceAuthMethod::OptionalApiKey]
        );
    }

    #[test]
    fn hosted_providers_default_to_eight_concurrent_requests() {
        for provider in inference_setup_catalog().providers {
            let recommendation = recommendation_for_model(
                provider.id,
                provider.default_auth_method,
                &advertised("test-model"),
            )
            .unwrap();
            assert_eq!(
                recommendation.max_concurrent.recommended,
                if provider.id == InferenceProviderId::Local {
                    1
                } else {
                    8
                }
            );
        }
    }

    #[test]
    fn claude_model_defaults_preserve_advertised_limits_and_supported_choices() {
        let mut model = advertised("claude-sonnet-5");
        model.context_window = Some(200_000);
        model.max_output_tokens = Some(32_000);
        model.reasoning_efforts = Some(vec![ReasoningEffort::Low, ReasoningEffort::High]);
        let value = recommendation_for_model(
            InferenceProviderId::Anthropic,
            InferenceAuthMethod::ClaudeOauth,
            &model,
        )
        .unwrap();
        assert_eq!(value.context_window.unwrap().recommended, 200_000);
        assert_eq!(value.max_output_tokens.unwrap().recommended, 32_000);
        assert_eq!(
            value.reasoning_effort.unwrap().choices,
            model.reasoning_efforts.unwrap()
        );
        assert!(claude_model_defaults("claude-unknown").is_none());
    }

    #[test]
    fn newly_advertised_claude_model_uses_exact_provider_efforts() {
        let mut model = advertised("claude-future-unreviewed");
        model.reasoning_efforts = Some(vec![ReasoningEffort::Low, ReasoningEffort::XHigh]);
        let value = recommendation_for_model(
            InferenceProviderId::Anthropic,
            InferenceAuthMethod::ClaudeOauth,
            &model,
        )
        .unwrap();
        assert_eq!(
            value.reasoning_effort.unwrap().choices,
            vec![ReasoningEffort::Low, ReasoningEffort::XHigh]
        );

        model.reasoning_efforts = None;
        assert!(recommendation_for_model(
            InferenceProviderId::Anthropic,
            InferenceAuthMethod::ClaudeOauth,
            &model,
        )
        .unwrap()
        .reasoning_effort
        .is_none());
    }

    #[test]
    fn workstation_fixture_has_exact_reviewed_sampling_defaults() {
        let recommendation = recommendation_for_model(
            InferenceProviderId::Local,
            InferenceAuthMethod::OptionalApiKey,
            &advertised("GLM-5.3-Flash-NVFP4"),
        )
        .unwrap();
        assert_eq!(recommendation.temperature.unwrap().recommended, 1.0);
        assert_eq!(recommendation.top_p.unwrap().recommended, 0.95);
    }

    #[test]
    fn grok_has_provider_specific_sampling_without_changing_other_providers() {
        let grok = recommendation_for_model(
            InferenceProviderId::Grok,
            InferenceAuthMethod::GrokOauth,
            &advertised("grok-4.6"),
        )
        .unwrap();
        assert_eq!(grok.temperature.unwrap().recommended, 0.7);
        assert_eq!(grok.top_p.unwrap().recommended, 0.95);

        let openai = recommendation_for_model(
            InferenceProviderId::OpenAi,
            InferenceAuthMethod::ApiKey,
            &advertised("gpt-4.1"),
        )
        .unwrap();
        assert_eq!(openai.top_p.unwrap().recommended, 1.0);
    }

    #[test]
    fn codex_context_default_and_override_ceiling_remain_distinct() {
        let mut model = advertised("gpt-5.6-sol");
        model.context_window = Some(272_000);
        model.max_context_window = Some(872_000);
        let control = recommendation_for_model(
            InferenceProviderId::OpenAi,
            InferenceAuthMethod::ChatGptOauth,
            &model,
        )
        .unwrap()
        .context_window
        .unwrap();
        assert_eq!(control.recommended, 272_000);
        assert_eq!(control.max, Some(872_000));

        model.max_context_window = None;
        assert_eq!(
            recommendation_for_model(
                InferenceProviderId::OpenAi,
                InferenceAuthMethod::ChatGptOauth,
                &model,
            )
            .unwrap()
            .context_window
            .unwrap()
            .max,
            Some(272_000)
        );

        model.context_window = None;
        model.max_context_window = Some(872_000);
        assert!(recommendation_for_model(
            InferenceProviderId::OpenAi,
            InferenceAuthMethod::ChatGptOauth,
            &model,
        )
        .unwrap()
        .context_window
        .is_none());
    }

    #[test]
    fn provider_specific_controls_do_not_claim_unsupported_sampling() {
        let claude = recommendation_for_model(
            InferenceProviderId::Anthropic,
            InferenceAuthMethod::ClaudeOauth,
            &advertised("claude-sonnet-5"),
        )
        .unwrap();
        assert!(claude.temperature.is_none());
        assert!(claude.top_p.is_none());
        assert_eq!(
            claude.reasoning_effort.unwrap().recommended,
            ReasoningEffort::High
        );
        assert_eq!(claude.context_window.unwrap().recommended, 1_000_000);
        assert_eq!(claude.max_output_tokens.unwrap().max, Some(128_000));

        let codex = recommendation_for_model(
            InferenceProviderId::OpenAi,
            InferenceAuthMethod::ChatGptOauth,
            &advertised("gpt-5.5"),
        )
        .unwrap();
        assert!(codex.temperature.is_none());
        assert_eq!(
            codex.reasoning_effort.unwrap().recommended,
            ReasoningEffort::Medium
        );
    }

    #[test]
    fn incompatible_provider_auth_pairs_fail_closed() {
        assert!(connection_spec(
            InferenceProviderId::Anthropic,
            InferenceAuthMethod::ApiKey,
            ""
        )
        .is_err());
    }

    #[test]
    fn existing_backends_recover_the_same_provider_contract() {
        assert_eq!(
            provider_selection_for_backend(
                BackendProviderKind::OpenAiCompatible,
                "https://api.openai.com/v1/"
            ),
            (InferenceProviderId::OpenAi, InferenceAuthMethod::ApiKey)
        );
        assert_eq!(
            provider_selection_for_backend(
                BackendProviderKind::OpenAiCompatible,
                "http://workstation-1:8000/v1"
            ),
            (
                InferenceProviderId::Local,
                InferenceAuthMethod::OptionalApiKey
            )
        );
    }
}
