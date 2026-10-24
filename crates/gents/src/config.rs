use std::sync::Arc;
use std::time::Duration;

use gents_protocol::error_chain::ErrorChain;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{field} must be positive")]
    NotPositive { field: &'static str },
    #[error(transparent)]
    OutOfRange(#[from] std::num::TryFromIntError),
    #[error(
        "reasoning_effort must be one of: none, minimal, low, medium, high, xhigh, max, ultra"
    )]
    UnknownReasoningEffort,
    #[error("sampling seed must be non-negative")]
    NegativeSeed,
    #[error(
        "sampling seed is unsupported by provider {provider_kind} on the {openai_wire_api} wire"
    )]
    SeedUnsupported {
        provider_kind: BackendProviderKind,
        openai_wire_api: OpenAiWireApi,
    },
    #[error("{provider} does not support selecting a service tier")]
    ServiceTierUnsupported { provider: BackendProviderKind },
    #[error("backend {backend_id} for agent {agent_id}: {error}")]
    AgentBackendApiKey {
        backend_id: String,
        agent_id: String,
        error: String,
    },
    #[error("backend {backend_id}: {error}")]
    BackendApiKey { backend_id: String, error: String },
    #[error("profile {profile_id} context window {selected} exceeds model {model_name} advertised maximum {maximum}")]
    ContextWindowExceeded {
        profile_id: String,
        selected: i64,
        model_name: String,
        maximum: i64,
    },
    #[error("model {model_name} is not advertised by backend {backend_id} in the selected credential scope")]
    ModelNotAdvertised {
        model_name: String,
        backend_id: String,
    },
    #[error("ambiguous advertised model {model_name}")]
    AmbiguousAdvertisedModel { model_name: String },
    #[error("model {model_name} does not advertise selected reasoning effort {effort:?}")]
    ReasoningEffortNotAdvertised {
        model_name: String,
        effort: ReasoningEffort,
    },
    #[error(transparent)]
    BackendCatalog(#[from] crate::backend_registry::BackendRegistryError),
}

type Result<T, E = ConfigError> = std::result::Result<T, E>;

use crate::agent::completion_retry::CompletionRetryProfileFields;
use crate::backend_provider::BackendProviderKind;
use crate::compaction::CompactionStrategy;
use crate::identity::{NodeIdentity, RuntimeNode};
use crate::openai_wire::OpenAiWireApi;
use crate::tool_surface::AgentToolSurfaceConfig;

pub const DEFAULT_CONTEXT_WINDOW: usize = 131_072;
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 32_768;
pub const DEFAULT_MAX_TURNS: usize = 1_000;
pub const DEFAULT_STREAM_BATCH_MS: u64 = 1_000;
pub const DEFAULT_COMPACTION_THRESHOLD: f64 = 0.75;
pub use gents_loop::compaction::{
    DEFAULT_COMPACTION_SUMMARY_FILE_LIST_MAX, DEFAULT_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS,
    MAX_COMPACTION_SUMMARY_FILE_LIST_MAX, MAX_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS,
};
pub const DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS: u64 = 120;
pub const DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS: u64 = 300;
pub const DEFAULT_DEADLINE_DURATION_SECS: u64 = 86_400;
pub const DEFAULT_MODEL_NAME: &str = "default";

#[derive(Clone)]
pub struct ResolvedAgent {
    pub agent_id: String,
    pub node: Arc<RuntimeNode>,
    pub backend_id: Option<String>,
    pub backend_provider_kind: BackendProviderKind,
    pub openai_wire_api: OpenAiWireApi,
    pub backend_endpoint: String,
    pub backend_auth: crate::document_config::BackendAuth,
    pub model_name: String,
    pub service_tier: Option<ServiceTier>,
    /// Provider-advertised effort choices for the selected model. `None` is
    /// unknown and must not be inferred from the model name at dispatch.
    pub resolved_reasoning_efforts: Option<Vec<ReasoningEffort>>,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub max_turns: usize,
    pub max_turns_provenance: MaxTurnsProvenance,
    pub system_prompt: String,
    pub tools: AgentToolSurfaceConfig,
    pub compaction: Option<crate::document_config::CompactionConfig>,
    pub compaction_inference: Option<ResolvedInference>,
    pub max_total_tokens: Option<u64>,
    pub stream_batch_ms: u64,
    pub stream_liveness_timeout: Duration,
    pub provider_idle_timeout: Duration,
    pub deadline_duration: Duration,
    pub completion_retry: CompletionRetryProfileFields,
    pub sampling: SamplingConfig,
    /// Effective skill set for this agent. Their tool deps are intersected with
    /// the tool ceiling and never widen it. See `crate::skills`.
    pub skills: Vec<crate::skills::Skill>,
}

#[derive(Debug, Clone)]
pub struct ResolvedInference {
    pub backend: crate::document_config::InferenceBackend,
    pub profile: crate::document_config::InferenceProfile,
    pub sampling: Option<crate::document_config::InferenceSampling>,
    pub execution: Option<crate::document_config::InferenceExecution>,
    pub retry_policy: Option<crate::document_config::InferenceRetryPolicy>,
    pub advertised_model: Option<crate::document_config::AdvertisedModel>,
}

impl ResolvedInference {
    pub fn resolved_reasoning_efforts(&self) -> Option<Vec<ReasoningEffort>> {
        let advertised = self
            .advertised_model
            .as_ref()
            .and_then(|model| model.reasoning_efforts.as_deref());
        if self.backend.provider_kind.uses_messages_wire() {
            crate::inference_setup::claude_supported_reasoning_efforts(
                &self.profile.model_name,
                advertised,
            )
        } else {
            advertised.map(|values| values.to_vec())
        }
    }

    pub fn context_window(&self) -> Result<usize> {
        positive_inference_limit(
            self.profile.context_window.or_else(|| {
                self.advertised_model
                    .as_ref()
                    .and_then(|model| model.context_window)
            }),
            DEFAULT_CONTEXT_WINDOW,
            "context_window",
        )
    }

    pub fn max_output_tokens(&self) -> Result<usize> {
        positive_inference_limit(
            self.profile.max_output_tokens.or_else(|| {
                self.advertised_model
                    .as_ref()
                    .and_then(|model| model.max_output_tokens)
            }),
            DEFAULT_MAX_OUTPUT_TOKENS,
            "max_output_tokens",
        )
    }

    pub fn max_turns(&self) -> Result<ResolvedMaxTurns> {
        resolve_max_turns(
            self.execution
                .as_ref()
                .and_then(|execution| execution.max_turns),
        )
    }

    pub fn sampling_config(&self) -> Result<SamplingConfig> {
        validate_service_tier(self.backend.provider_kind, self.profile.service_tier)?;
        let sampling = self.sampling.clone().unwrap_or_default();
        let result = SamplingConfig {
            temperature: sampling.temperature,
            top_p: sampling.top_p,
            top_k: sampling.top_k,
            seed: sampling.seed,
            min_p: sampling.min_p,
            frequency_penalty: sampling.frequency_penalty,
            presence_penalty: sampling.presence_penalty,
            repetition_penalty: sampling.repetition_penalty,
            reasoning_effort: self.profile.reasoning_effort,
            max_tokens: Some(u64::try_from(self.max_output_tokens()?)?),
        };
        let backend = self.backend.backend_fields();
        result.validate_for_provider(backend.backend_provider_kind, backend.openai_wire_api)?;
        Ok(result)
    }
}

fn positive_inference_limit(
    value: Option<i64>,
    default: usize,
    field: &'static str,
) -> Result<usize> {
    match value {
        None => Ok(default),
        Some(value) if value > 0 => Ok(usize::try_from(value)?),
        Some(_) => Err(ConfigError::NotPositive { field }),
    }
}

pub use gents_loop::provider_input::ServiceTier;

pub fn supports_service_tier(provider: BackendProviderKind) -> bool {
    matches!(
        provider,
        BackendProviderKind::OpenAiCompatible | BackendProviderKind::ChatGptCodex
    )
}

pub fn validate_service_tier(
    provider: BackendProviderKind,
    tier: Option<ServiceTier>,
) -> Result<()> {
    if tier.is_some() && !supports_service_tier(provider) {
        return Err(ConfigError::ServiceTierUnsupported { provider });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxTurnsProvenance {
    Default,
    ExecutionProfile,
    BuilderOverride,
}

impl MaxTurnsProvenance {
    pub fn describe(self) -> &'static str {
        match self {
            MaxTurnsProvenance::Default => {
                "no max_turns is configured for this agent; this is the built-in default, raised by setting max_turns on an InferenceExecution document bound to the agent's inference profile, or through AgentBuilder::max_turns for an agent built programmatically"
            }
            MaxTurnsProvenance::ExecutionProfile => {
                "max_turns is set explicitly by the InferenceExecution document bound to this agent's inference profile"
            }
            MaxTurnsProvenance::BuilderOverride => {
                "max_turns was set programmatically through AgentBuilder::max_turns; no InferenceExecution document controls it"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedMaxTurns {
    pub value: usize,
    pub provenance: MaxTurnsProvenance,
}

fn resolve_max_turns(configured: Option<i64>) -> Result<ResolvedMaxTurns> {
    let provenance = if configured.is_some() {
        MaxTurnsProvenance::ExecutionProfile
    } else {
        MaxTurnsProvenance::Default
    };
    let value = positive_inference_limit(configured, DEFAULT_MAX_TURNS, "max_turns")?;
    Ok(ResolvedMaxTurns { value, provenance })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Ultra,
}

impl ReasoningEffort {
    pub const ALL: [Self; 8] = [
        Self::None,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
        Self::Ultra,
    ];

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            "ultra" => Ok(Self::Ultra),
            _ => Err(ConfigError::UnknownReasoningEffort),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SamplingConfig {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub seed: Option<i64>,
    /// Sampling knobs the provider takes as extra body params (#649): rig's
    /// `CompletionRequest` models only `temperature`, so everything else rides
    /// `additional_params` — see [`SamplingConfig::additional_params`].
    pub min_p: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub repetition_penalty: Option<f64>,
    pub max_tokens: Option<u64>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl SamplingConfig {
    pub fn validate_for_provider(
        self,
        provider_kind: BackendProviderKind,
        openai_wire_api: OpenAiWireApi,
    ) -> Result<()> {
        let Some(seed) = self.seed else {
            return Ok(());
        };
        if seed < 0 {
            return Err(ConfigError::NegativeSeed);
        }
        if !matches!(
            (provider_kind, openai_wire_api),
            (
                BackendProviderKind::OpenAiCompatible,
                OpenAiWireApi::ChatCompletions
            ) | (BackendProviderKind::OpenRouter, _)
        ) {
            return Err(ConfigError::SeedUnsupported {
                provider_kind,
                openai_wire_api,
            });
        }
        Ok(())
    }

    pub fn is_empty(self) -> bool {
        self.temperature.is_none()
            && self.top_p.is_none()
            && self.top_k.is_none()
            && self.seed.is_none()
            && self.min_p.is_none()
            && self.frequency_penalty.is_none()
            && self.presence_penalty.is_none()
            && self.repetition_penalty.is_none()
            && self.max_tokens.is_none()
            && self.reasoning_effort.is_none()
    }

    pub fn additional_params(self) -> Option<serde_json::Value> {
        let mut params = serde_json::Map::new();
        if let Some(top_p) = self.top_p {
            params.insert("top_p".to_string(), serde_json::json!(top_p));
        }
        if let Some(top_k) = self.top_k {
            params.insert("top_k".to_string(), serde_json::json!(top_k));
        }
        if let Some(seed) = self.seed {
            params.insert("seed".to_string(), serde_json::json!(seed));
        }
        if let Some(min_p) = self.min_p {
            params.insert("min_p".to_string(), serde_json::json!(min_p));
        }
        if let Some(frequency_penalty) = self.frequency_penalty {
            params.insert(
                "frequency_penalty".to_string(),
                serde_json::json!(frequency_penalty),
            );
        }
        if let Some(presence_penalty) = self.presence_penalty {
            params.insert(
                "presence_penalty".to_string(),
                serde_json::json!(presence_penalty),
            );
        }
        if let Some(repetition_penalty) = self.repetition_penalty {
            params.insert(
                "repetition_penalty".to_string(),
                serde_json::json!(repetition_penalty),
            );
        }

        (!params.is_empty()).then_some(serde_json::Value::Object(params))
    }
}

impl std::fmt::Debug for ResolvedAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedAgent")
            .field("agent_id", &self.agent_id)
            .field("node_did", &self.node.node_did)
            .field("backend_id", &self.backend_id)
            .field("backend_provider_kind", &self.backend_provider_kind)
            .field("openai_wire_api", &self.openai_wire_api)
            .field("backend_endpoint", &self.backend_endpoint)
            .field("backend_auth", &self.backend_auth)
            .field("model_name", &self.model_name)
            .field("service_tier", &self.service_tier)
            .field(
                "resolved_reasoning_efforts",
                &self.resolved_reasoning_efforts,
            )
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("max_turns", &self.max_turns)
            .field("max_turns_provenance", &self.max_turns_provenance)
            .field("system_prompt", &self.system_prompt)
            .field("tools", &self.tools)
            .field("compaction", &self.compaction)
            .field("compaction_inference", &self.compaction_inference)
            .field("max_total_tokens", &self.max_total_tokens)
            .field("stream_batch_ms", &self.stream_batch_ms)
            .field("stream_liveness_timeout", &self.stream_liveness_timeout)
            .field("provider_idle_timeout", &self.provider_idle_timeout)
            .field("deadline_duration", &self.deadline_duration)
            .field("completion_retry", &self.completion_retry)
            .field("sampling", &self.sampling)
            .field("skills", &self.skills)
            .finish()
    }
}

impl ResolvedAgent {
    pub fn compaction_threshold(&self) -> f64 {
        self.compaction
            .as_ref()
            .and_then(|config| config.threshold)
            .unwrap_or(DEFAULT_COMPACTION_THRESHOLD)
    }

    pub fn compaction_strategy(&self) -> CompactionStrategy {
        self.compaction
            .as_ref()
            .map(|config| config.strategy.clone())
            .unwrap_or(CompactionStrategy::StripThenSummarize)
    }

    pub fn node_did(&self) -> &str {
        &self.node.node_did
    }

    pub fn node_identity(&self) -> &Arc<dyn NodeIdentity> {
        &self.node.identity
    }

    pub fn resolve_backend_api_key(&self) -> Result<Option<String>> {
        self.backend_auth
            .resolve_api_key()
            .map_err(|error| ConfigError::AgentBackendApiKey {
                backend_id: self.backend_id.as_deref().unwrap_or("<unbound>").to_owned(),
                agent_id: self.agent_id.to_string(),
                error: ErrorChain(&error).to_string(),
            })
    }

    pub fn completion_client_api_key(&self) -> Result<String> {
        Ok(self
            .resolve_backend_api_key()?
            .unwrap_or_else(|| "no-key".to_string()))
    }
}

pub fn resolve_backend_api_key(
    backend: &crate::backend_registry::InferenceBackend,
) -> Result<Option<String>> {
    backend
        .auth
        .resolve_api_key()
        .map_err(|error| ConfigError::BackendApiKey {
            backend_id: backend.backend_id.to_string(),
            error: ErrorChain(&error).to_string(),
        })
}

pub fn validate_advertised_context_override(
    profile: &crate::document_config::InferenceProfile,
    model: &crate::document_config::AdvertisedModel,
) -> Result<()> {
    let Some(selected) = profile.context_window else {
        return Ok(());
    };
    let Some(maximum) = model.max_context_window.filter(|maximum| {
        *maximum > 0
            && model
                .context_window
                .is_none_or(|default| default > 0 && default <= *maximum)
    }) else {
        return Ok(());
    };
    if selected > maximum {
        return Err(ConfigError::ContextWindowExceeded {
            profile_id: profile.profile_id.clone(),
            selected,
            model_name: profile.model_name.clone(),
            maximum,
        });
    }
    Ok(())
}

pub fn backend_catalog<'a>(
    backend: &crate::document_config::InferenceBackend,
    observation: Option<&'a crate::document_config::InferenceBackendObservation>,
) -> Result<Option<&'a crate::document_config::BackendModelCatalog>> {
    let credential_scope = matches!(
        backend.auth,
        crate::document_config::BackendAuth::NodeOAuth { .. }
    )
    .then_some(backend.node_did.as_str());
    Ok(observation
        .filter(|observation| observation.backend_id == backend.backend_id)
        .map(|observation| observation.catalog_for(credential_scope))
        .transpose()?
        .flatten())
}

pub fn admits_reasoning_effort(
    backend: &crate::document_config::InferenceBackend,
    effort: ReasoningEffort,
    advertised: &[ReasoningEffort],
) -> bool {
    advertised.contains(&effort)
        || (backend.provider_kind == BackendProviderKind::OpenAiCompatible
            && crate::inference_setup::is_xai_api_endpoint(&backend.endpoint))
}

pub fn unsent_reasoning_effort(
    backend: &crate::document_config::InferenceBackend,
    profile: &crate::document_config::InferenceProfile,
    observation: Option<&crate::document_config::InferenceBackendObservation>,
) -> Option<String> {
    let effort = profile.reasoning_effort?;
    let fields = backend.backend_fields();
    let model = backend_catalog(backend, observation)
        .ok()
        .flatten()
        .and_then(|catalog| {
            catalog
                .models
                .iter()
                .find(|model| model.model_name == profile.model_name)
        });
    let advertised = model.and_then(|model| model.reasoning_efforts.as_deref());
    crate::inference_setup::sent_reasoning_effort(
        fields.backend_provider_kind,
        fields.openai_wire_api,
        &fields.backend_endpoint,
        advertised,
        Some(effort),
    )
    .is_none()
    .then(|| {
        let reason = match (model, advertised) {
            (None, _) => "has no discovered catalog; run `gents config backend discover-models`"
                .to_string(),
            (Some(_), None) => {
                "catalog predates effort discovery; run `gents config backend discover-models`"
                    .to_string()
            }
            (Some(_), Some([])) => "advertises no reasoning effort".to_string(),
            (Some(_), Some(list)) => format!(
                "advertises {}",
                list.iter()
                    .map(|effort| effort.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        format!(
            "profile {} reasoning effort {} is not sent: model {} {reason}; the model default applies",
            profile.profile_id,
            effort.as_str(),
            profile.model_name
        )
    })
}

pub fn advertised_model_for_profile(
    backend: &crate::document_config::InferenceBackend,
    profile: &crate::document_config::InferenceProfile,
    observation: Option<&crate::document_config::InferenceBackendObservation>,
) -> Result<Option<crate::document_config::AdvertisedModel>> {
    validate_service_tier(backend.provider_kind, profile.service_tier)?;
    let Some(catalog) = backend_catalog(backend, observation)? else {
        return Ok(None);
    };
    let mut models = catalog
        .models
        .iter()
        .filter(|model| model.model_name == profile.model_name);
    let model = models
        .next()
        .ok_or_else(|| ConfigError::ModelNotAdvertised {
            model_name: profile.model_name.clone(),
            backend_id: backend.backend_id.clone(),
        })?;
    if models.next().is_some() {
        return Err(ConfigError::AmbiguousAdvertisedModel {
            model_name: profile.model_name.clone(),
        });
    }
    if let (Some(effort), Some(supported)) =
        (profile.reasoning_effort, model.reasoning_efforts.as_ref())
    {
        if !admits_reasoning_effort(backend, effort, supported) {
            return Err(ConfigError::ReasoningEffortNotAdvertised {
                model_name: profile.model_name.clone(),
                effort,
            });
        }
    }
    validate_advertised_context_override(profile, model)?;
    Ok(Some(model.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::KeyIdentity;

    fn stub_node() -> Arc<RuntimeNode> {
        let identity = Arc::new(
            KeyIdentity::load_or_create(
                std::env::temp_dir().join(format!("config-agent-{}.key", uuid::Uuid::new_v4())),
                None,
            )
            .unwrap(),
        );
        Arc::new(RuntimeNode {
            node_did: identity.did().to_string(),
            identity,
            default_agent_id: String::new(),
            display_name: None,
            enabled: true,
        })
    }

    fn agent_with_wire(openai_wire_api: OpenAiWireApi) -> ResolvedAgent {
        ResolvedAgent {
            agent_id: "general".to_string(),
            node: stub_node(),
            backend_id: Some("backend-general".to_string()),
            backend_provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api,
            backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
            backend_auth: crate::document_config::BackendAuth::Unauthenticated,
            model_name: DEFAULT_MODEL_NAME.to_string(),
            service_tier: None,
            resolved_reasoning_efforts: None,
            context_window: DEFAULT_CONTEXT_WINDOW,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            max_turns: DEFAULT_MAX_TURNS,
            max_turns_provenance: MaxTurnsProvenance::Default,
            system_prompt: "system".to_string(),
            tools: AgentToolSurfaceConfig::meta_only(),
            compaction: None,
            compaction_inference: None,
            max_total_tokens: None,
            stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
            stream_liveness_timeout: Duration::from_secs(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS),
            provider_idle_timeout: Duration::from_secs(DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS),
            deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
            completion_retry: CompletionRetryProfileFields::default(),
            sampling: SamplingConfig::default(),
            skills: Vec::new(),
        }
    }

    #[test]
    fn resolve_backend_api_key_errors_when_env_var_named_but_unset() {
        let mut agent = agent_with_wire(OpenAiWireApi::ChatCompletions);
        agent.agent_id = "agent-alpha".to_string();
        agent.backend_auth = crate::document_config::BackendAuth::Environment {
            variable: "GENTS_CONFIG_TEST_KEY_MISSING_1338_NEVER_SET".into(),
        };

        let error = agent
            .resolve_backend_api_key()
            .expect_err("a named-but-unset env var must hard error");
        let message = error.to_string();
        assert!(
            message.contains("backend-general"),
            "error must name the backend: {message}"
        );
        assert!(
            message.contains("agent-alpha"),
            "error must name the agent: {message}"
        );
        assert!(
            message.contains("GENTS_CONFIG_TEST_KEY_MISSING_1338_NEVER_SET"),
            "error must name the environment variable: {message}"
        );
    }

    #[test]
    fn resolve_backend_api_key_none_when_no_env_var_configured() {
        let agent = agent_with_wire(OpenAiWireApi::ChatCompletions);
        assert_eq!(
            agent
                .resolve_backend_api_key()
                .expect("no key configured is not an error"),
            None
        );
    }

    #[test]
    fn debug_distinguishes_openai_wire_api_for_reconcile_fingerprint() {
        let chat = format!("{:?}", agent_with_wire(OpenAiWireApi::ChatCompletions));
        let responses = format!("{:?}", agent_with_wire(OpenAiWireApi::Responses));
        assert_ne!(
            chat, responses,
            "openai_wire_api must be in ResolvedAgent Debug so the runtime fingerprint \
             changes when the wire API changes"
        );
        assert!(chat.contains("ChatCompletions"));
        assert!(responses.contains("Responses"));
    }
}
