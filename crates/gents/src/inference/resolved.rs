use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::inference::backends::provider::BackendProviderKind;
use crate::inference::compaction::CompactionStrategy;
use crate::openai_wire::OpenAiWireApi;
use crate::runtime::agent::completion_retry::CompletionRetryProfileFields;
use crate::store::identity::{NodeIdentity, RuntimeNode};
use crate::tools::surface::AgentToolSurfaceConfig;

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

/// A static builder has no persisted configuration documents. Managed agents
/// retain the physical documents of the resolved generation for native reads
/// at each provider-input assembly.
#[derive(Clone, Debug)]
pub enum ResolvedConfiguration {
    Static,
    Documents(SelectedConfiguration),
}

#[derive(Clone, Debug)]
pub struct SelectedConfigurationDocument {
    pub collection: crate::Collection,
    pub doc_id: String,
    pub logical_id: Option<String>,
}

#[derive(Clone)]
pub struct SelectedConfiguration {
    pub(crate) node: Arc<defra_node::Node>,
    pub(crate) actor: identity::Did,
    pub(crate) documents: Vec<SelectedConfigurationDocument>,
}

impl std::fmt::Debug for SelectedConfiguration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SelectedConfiguration")
            .field("actor", &self.actor)
            .field("documents", &self.documents)
            .finish_non_exhaustive()
    }
}

impl SelectedConfiguration {
    pub(crate) fn new(
        node: Arc<defra_node::Node>,
        actor: identity::Did,
        documents: Vec<SelectedConfigurationDocument>,
    ) -> Self {
        Self {
            node,
            actor,
            documents,
        }
    }
}

#[derive(Clone)]
pub struct ResolvedAgent {
    pub agent_did: String,
    pub node: Arc<RuntimeNode>,
    pub agent_identity: Arc<dyn NodeIdentity>,
    pub configuration: ResolvedConfiguration,
    pub backend_id: Option<String>,
    pub backend_provider_kind: BackendProviderKind,
    pub openai_wire_api: OpenAiWireApi,
    pub backend_endpoint: String,
    pub backend_auth: crate::documents::BackendAuth,
    pub claude_platform: Option<crate::documents::ClaudePlatform>,
    pub model_name: String,
    pub profile_id: Option<String>,
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
    pub compaction: Option<crate::documents::CompactionConfig>,
    pub compaction_inference: Option<ResolvedInference>,
    pub max_total_tokens: Option<u64>,
    pub stream_batch_ms: u64,
    pub stream_liveness_timeout: Duration,
    pub provider_idle_timeout: Duration,
    pub deadline_duration: Duration,
    pub completion_retry: CompletionRetryProfileFields,
    pub sampling: SamplingConfig,
    /// Effective skill set for this agent. Their tool deps are intersected with
    /// the tool ceiling and never widen it. See `crate::documents::skills`.
    pub skills: Vec<crate::documents::skills::Skill>,
    /// Credential ids of the backend's account pool in rotation order; empty
    /// unless the backend rotates a pool. Part of the slot fingerprint, so an
    /// account signing in, out or being disabled rebuilds the live client.
    pub oauth_pool: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedInference {
    pub backend: crate::documents::InferenceBackend,
    pub profile: crate::documents::InferenceProfile,
    pub sampling: Option<crate::documents::InferenceSampling>,
    pub execution: Option<crate::documents::InferenceExecution>,
    pub retry_policy: Option<crate::documents::InferenceRetryPolicy>,
    pub advertised_model: Option<crate::documents::AdvertisedModel>,
}

impl ResolvedInference {
    pub fn resolved_reasoning_efforts(&self) -> Option<Vec<ReasoningEffort>> {
        let advertised = self
            .advertised_model
            .as_ref()
            .and_then(|model| model.reasoning_efforts.as_deref());
        if self.backend.provider_kind.uses_messages_wire() {
            crate::inference::backends::setup::claude_supported_reasoning_efforts(
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

fn positive_inference_limit(value: Option<i64>, default: usize, field: &str) -> Result<usize> {
    match value {
        None => Ok(default),
        Some(value) if value > 0 => Ok(usize::try_from(value)?),
        Some(_) => anyhow::bail!("{field} must be positive"),
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
    anyhow::ensure!(
        tier.is_none() || supports_service_tier(provider),
        "{provider} does not support selecting a service tier"
    );
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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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

    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            "ultra" => Ok(Self::Ultra),
            _ => anyhow::bail!(
                "reasoning_effort must be one of: none, minimal, low, medium, high, xhigh, max, ultra"
            ),
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
            anyhow::bail!("sampling seed must be non-negative");
        }
        if !matches!(
            (provider_kind, openai_wire_api),
            (
                BackendProviderKind::OpenAiCompatible,
                OpenAiWireApi::ChatCompletions
            ) | (BackendProviderKind::OpenRouter, _)
        ) {
            anyhow::bail!(
                "sampling seed is unsupported by provider {} on the {} wire",
                provider_kind,
                openai_wire_api
            );
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
            .field("agent_did", &self.agent_did)
            .field("configuration", &self.configuration)
            .field("node_did", &self.node.node_did)
            .field("backend_id", &self.backend_id)
            .field("backend_provider_kind", &self.backend_provider_kind)
            .field("openai_wire_api", &self.openai_wire_api)
            .field("backend_endpoint", &self.backend_endpoint)
            .field("backend_auth", &self.backend_auth)
            .field("claude_platform", &self.claude_platform)
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
            .field("oauth_pool", &self.oauth_pool)
            .finish()
    }
}

impl ResolvedAgent {
    /// The settings this agent's slot binds to any request dispatched to it
    /// while this generation is active.
    ///
    /// The slot holds one immutable `ResolvedAgent` for its whole life and the
    /// router re-checks the active snapshot immediately before handing a
    /// request to a dispatcher, so this is exactly what a request dispatched
    /// now will use. A request already dispatched keeps the settings of the
    /// generation that dispatched it, which its claim recorded; this
    /// projection says nothing about it.
    pub fn effective_inference(&self) -> gents_protocol::row::EffectiveInferenceSettings {
        let configured = self.sampling.reasoning_effort;
        let sent = crate::inference::backends::setup::sent_reasoning_effort(
            self.backend_provider_kind,
            self.openai_wire_api,
            &self.backend_endpoint,
            self.resolved_reasoning_efforts.as_deref(),
            configured,
        );
        let reasoning_effort_dispatch = match (configured, sent) {
            (None, _) => gents_protocol::row::ReasoningEffortBinding::Unset,
            (Some(_), Some(sent)) => gents_protocol::row::ReasoningEffortBinding::Sent {
                effort: sent.as_str().to_string(),
            },
            (Some(configured), None) => {
                gents_protocol::row::ReasoningEffortBinding::DroppedUnsupported {
                    effort: configured.as_str().to_string(),
                    advertised: self.resolved_reasoning_efforts.as_ref().map(|efforts| {
                        efforts
                            .iter()
                            .map(|effort| effort.as_str().to_string())
                            .collect()
                    }),
                }
            }
        };
        // A resolved agent exists only when `validate_service_tier` accepted
        // its selection, so an unsupported selection is reported against the
        // agent as an unavailable reason and never reaches this projection.
        // What this does report is whether the provider has a tier at all.
        let service_tier = match self.service_tier {
            Some(tier) => gents_protocol::row::ServiceTierBinding::Selected {
                tier: tier.as_str().to_string(),
            },
            None if supports_service_tier(self.backend_provider_kind) => {
                gents_protocol::row::ServiceTierBinding::Unset
            }
            None => gents_protocol::row::ServiceTierBinding::NotSupportedByProvider,
        };
        gents_protocol::row::EffectiveInferenceSettings {
            profile_id: self.profile_id.clone(),
            backend_id: self.backend_id.clone(),
            model_name: self.model_name.clone(),
            reasoning_effort: configured.map(|effort| effort.as_str().to_string()),
            service_tier,
            reasoning_effort_dispatch,
        }
    }

    /// The binding a claim records for one execution dispatched to this
    /// slot under `runtime_generation`. It is the same resolved configuration
    /// the slot will use for every provider call of that request, because the
    /// loop configuration and provider model handle are built once per
    /// request and reused for every turn, retry and tool round trip.
    pub fn executed_inference(
        &self,
        runtime_generation: u64,
    ) -> gents_protocol::row::ExecutedInferenceSettings {
        gents_protocol::row::ExecutedInferenceSettings {
            model_name: self.model_name.clone(),
            reasoning_effort: self
                .sampling
                .reasoning_effort
                .map(|effort| effort.as_str().to_string()),
            service_tier: self.service_tier.map(|tier| tier.as_str().to_string()),
            inference_profile_id: self.profile_id.clone(),
            runtime_generation: Some(runtime_generation),
        }
    }

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

    pub fn agent_did(&self) -> &str {
        self.agent_identity.did()
    }

    pub fn agent_identity(&self) -> &Arc<dyn NodeIdentity> {
        &self.agent_identity
    }

    pub fn resolve_backend_api_key(&self) -> Result<Option<String>> {
        self.backend_auth.resolve_api_key().map_err(|error| {
            anyhow::anyhow!(
                "backend {} for agent {}: {error:#}",
                self.backend_id.as_deref().unwrap_or("<unbound>"),
                self.agent_did
            )
        })
    }

    pub fn completion_client_api_key(&self) -> Result<String> {
        Ok(self
            .resolve_backend_api_key()?
            .unwrap_or_else(|| "no-key".to_string()))
    }
}

pub fn resolve_backend_api_key(
    backend: &crate::inference::backends::registry::InferenceBackend,
) -> Result<Option<String>> {
    backend
        .auth
        .resolve_api_key()
        .map_err(|error| anyhow::anyhow!("backend {}: {error:#}", backend.backend_id))
}

pub fn validate_advertised_context_override(
    profile: &crate::documents::InferenceProfile,
    model: &crate::documents::AdvertisedModel,
) -> anyhow::Result<()> {
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
    anyhow::ensure!(
        selected <= maximum,
        "profile {} context window {} exceeds model {} advertised maximum {}",
        profile.profile_id,
        selected,
        profile.model_name,
        maximum
    );
    Ok(())
}

pub fn backend_catalog<'a>(
    backend: &crate::documents::InferenceBackend,
    observation: Option<&'a crate::documents::InferenceBackendObservation>,
) -> anyhow::Result<Option<&'a crate::documents::BackendModelCatalog>> {
    let credential_scope = backend
        .auth
        .uses_node_oauth()
        .then_some(backend.node_did.as_str());
    Ok(observation
        .filter(|observation| observation.backend_id == backend.backend_id)
        .map(|observation| observation.catalog_for(credential_scope))
        .transpose()?
        .flatten())
}

pub fn admits_reasoning_effort(
    backend: &crate::documents::InferenceBackend,
    effort: ReasoningEffort,
    advertised: &[ReasoningEffort],
) -> bool {
    advertised.contains(&effort)
        || (backend.provider_kind == BackendProviderKind::OpenAiCompatible
            && crate::inference::backends::setup::is_xai_api_endpoint(&backend.endpoint))
}

/// Why a configured reasoning effort will not reach the provider.
///
/// The fallback is silent on the wire, so the typed reason is the only way a
/// client can say that the model default applied rather than the configured
/// effort. `NoCatalog` and `AdvertisesNone` stay distinct: an undiscovered
/// catalog is unknown support, not an absence of support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsentReasoningEffortReason {
    NoCatalog,
    CatalogPredatesEffortDiscovery,
    AdvertisesNone,
    Advertises(Vec<ReasoningEffort>),
}

/// A configured effort the selected backend will drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsentReasoningEffort {
    pub profile_id: String,
    pub model_name: String,
    pub effort: ReasoningEffort,
    pub reason: UnsentReasoningEffortReason,
}

impl UnsentReasoningEffort {
    pub fn advertised(&self) -> Option<&[ReasoningEffort]> {
        match &self.reason {
            UnsentReasoningEffortReason::NoCatalog
            | UnsentReasoningEffortReason::CatalogPredatesEffortDiscovery => None,
            UnsentReasoningEffortReason::AdvertisesNone => Some(&[]),
            UnsentReasoningEffortReason::Advertises(list) => Some(list),
        }
    }
}

impl std::fmt::Display for UnsentReasoningEffort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match &self.reason {
            UnsentReasoningEffortReason::NoCatalog => {
                "has no discovered catalog; run `gents config backend discover-models`".to_string()
            }
            UnsentReasoningEffortReason::CatalogPredatesEffortDiscovery => {
                "catalog predates effort discovery; run `gents config backend discover-models`"
                    .to_string()
            }
            UnsentReasoningEffortReason::AdvertisesNone => {
                "advertises no reasoning effort".to_string()
            }
            UnsentReasoningEffortReason::Advertises(list) => format!(
                "advertises {}",
                list.iter()
                    .map(|effort| effort.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        write!(
            formatter,
            "profile {} reasoning effort {} is not sent: model {} {reason}; the model default applies",
            self.profile_id,
            self.effort.as_str(),
            self.model_name
        )
    }
}

pub fn unsent_reasoning_effort(
    backend: &crate::documents::InferenceBackend,
    profile: &crate::documents::InferenceProfile,
    observation: Option<&crate::documents::InferenceBackendObservation>,
) -> Option<UnsentReasoningEffort> {
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
    crate::inference::backends::setup::sent_reasoning_effort(
        fields.backend_provider_kind,
        fields.openai_wire_api,
        &fields.backend_endpoint,
        advertised,
        Some(effort),
    )
    .is_none()
    .then(|| UnsentReasoningEffort {
        profile_id: profile.profile_id.clone(),
        model_name: profile.model_name.clone(),
        effort,
        reason: unsent_reasoning_effort_reason(model.is_some(), advertised),
    })
}

/// The structured reason for a dropped effort, from the same catalog facts
/// `sent_reasoning_effort` consulted.
pub fn unsent_reasoning_effort_reason(
    model_discovered: bool,
    advertised: Option<&[ReasoningEffort]>,
) -> UnsentReasoningEffortReason {
    match (model_discovered, advertised) {
        (false, _) => UnsentReasoningEffortReason::NoCatalog,
        (true, None) => UnsentReasoningEffortReason::CatalogPredatesEffortDiscovery,
        (true, Some([])) => UnsentReasoningEffortReason::AdvertisesNone,
        (true, Some(list)) => UnsentReasoningEffortReason::Advertises(list.to_vec()),
    }
}

pub fn advertised_model_for_profile(
    backend: &crate::documents::InferenceBackend,
    profile: &crate::documents::InferenceProfile,
    observation: Option<&crate::documents::InferenceBackendObservation>,
) -> anyhow::Result<Option<crate::documents::AdvertisedModel>> {
    validate_service_tier(backend.provider_kind, profile.service_tier)?;
    let Some(catalog) = backend_catalog(backend, observation)? else {
        return Ok(None);
    };
    let mut models = catalog
        .models
        .iter()
        .filter(|model| model.model_name == profile.model_name);
    let model = models.next().ok_or_else(|| {
        anyhow::anyhow!(
            "model {} is not advertised by backend {} in the selected credential scope",
            profile.model_name,
            backend.backend_id
        )
    })?;
    anyhow::ensure!(
        models.next().is_none(),
        "ambiguous advertised model {}",
        profile.model_name
    );
    if let (Some(effort), Some(supported)) =
        (profile.reasoning_effort, model.reasoning_efforts.as_ref())
    {
        anyhow::ensure!(
            admits_reasoning_effort(backend, effort, supported),
            "model {} does not advertise selected reasoning effort {effort:?}",
            profile.model_name
        );
    }
    validate_advertised_context_override(profile, model)?;
    Ok(Some(model.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::identity::KeyIdentity;

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
            default_agent_did: String::new(),
            display_name: None,
            enabled: true,
        })
    }

    #[test]
    fn dropped_effort_reasons_distinguish_unknown_from_absent_support() {
        assert_eq!(
            unsent_reasoning_effort_reason(false, None),
            UnsentReasoningEffortReason::NoCatalog
        );
        assert_eq!(
            unsent_reasoning_effort_reason(false, Some(&[ReasoningEffort::Low])),
            UnsentReasoningEffortReason::NoCatalog
        );
        assert_eq!(
            unsent_reasoning_effort_reason(true, None),
            UnsentReasoningEffortReason::CatalogPredatesEffortDiscovery
        );
        assert_eq!(
            unsent_reasoning_effort_reason(true, Some(&[])),
            UnsentReasoningEffortReason::AdvertisesNone
        );
        assert_eq!(
            unsent_reasoning_effort_reason(true, Some(&[ReasoningEffort::Low])),
            UnsentReasoningEffortReason::Advertises(vec![ReasoningEffort::Low])
        );
    }

    /// The owner itself returns the structured reason, not prose a consumer
    /// would have to parse. Without a discovered catalog the xAI Responses
    /// path drops the effort and says why.
    #[test]
    fn the_owner_returns_a_structured_dropped_effort_reason() {
        let backend: crate::documents::InferenceBackend =
            serde_json::from_value(serde_json::json!({
                "node_did": "did:test:node",
                "backend_id": "xai",
                "name": "xai",
                "provider_kind": "OpenAiCompatible",
                "openai_wire_api": "responses",
                "endpoint": crate::inference::backends::setup::XAI_API_ENDPOINT,
                "auth": {"kind": "unauthenticated"},
            }))
            .expect("canonical backend fixture");
        let profile = crate::documents::InferenceProfile {
            node_did: "did:test:node".to_string(),
            profile_id: "grok".to_string(),
            backend_id: "xai".to_string(),
            model_name: "grok-4".to_string(),
            reasoning_effort: Some(ReasoningEffort::Medium),
            ..Default::default()
        };
        let unsent =
            unsent_reasoning_effort(&backend, &profile, None).expect("the effort is not sent");
        assert_eq!(unsent.effort, ReasoningEffort::Medium);
        assert_eq!(unsent.profile_id, "grok");
        assert_eq!(unsent.model_name, "grok-4");
        assert_eq!(unsent.reason, UnsentReasoningEffortReason::NoCatalog);
        assert_eq!(unsent.advertised(), None);

        let sent = crate::documents::InferenceProfile {
            reasoning_effort: None,
            ..profile
        };
        assert!(unsent_reasoning_effort(&backend, &sent, None).is_none());
    }

    #[test]
    fn a_dropped_effort_keeps_its_operator_text() {
        let unsent = UnsentReasoningEffort {
            profile_id: "grok".to_string(),
            model_name: "grok-4".to_string(),
            effort: ReasoningEffort::Medium,
            reason: UnsentReasoningEffortReason::Advertises(vec![
                ReasoningEffort::Low,
                ReasoningEffort::High,
            ]),
        };
        assert_eq!(
            unsent.to_string(),
            "profile grok reasoning effort medium is not sent: model grok-4 advertises low, high; the model default applies"
        );
        assert_eq!(
            unsent.advertised(),
            Some([ReasoningEffort::Low, ReasoningEffort::High].as_slice())
        );
    }

    #[test]
    fn effective_inference_reports_the_sent_effort_and_selected_tier() {
        let mut agent_config = agent_with_wire(OpenAiWireApi::ChatCompletions);
        agent_config.model_name = "model-a".to_string();
        agent_config.sampling.reasoning_effort = Some(ReasoningEffort::Low);
        agent_config.service_tier = Some(ServiceTier::Fast);
        let effective = agent_config.effective_inference();
        assert_eq!(effective.model_name, "model-a");
        assert_eq!(effective.reasoning_effort.as_deref(), Some("low"));
        assert_eq!(
            effective.service_tier,
            gents_protocol::row::ServiceTierBinding::Selected {
                tier: "fast".to_string()
            }
        );
        assert_eq!(
            effective.reasoning_effort_dispatch,
            gents_protocol::row::ReasoningEffortBinding::Sent {
                effort: "low".to_string()
            }
        );
    }

    #[test]
    fn an_unset_selection_is_reported_as_unset_not_as_a_default() {
        let agent_config = agent_with_wire(OpenAiWireApi::ChatCompletions);
        let effective = agent_config.effective_inference();
        assert_eq!(effective.reasoning_effort, None);
        assert_eq!(
            effective.service_tier,
            gents_protocol::row::ServiceTierBinding::Unset
        );
        assert_eq!(
            effective.reasoning_effort_dispatch,
            gents_protocol::row::ReasoningEffortBinding::Unset
        );
    }

    /// The xAI Responses path drops an effort the catalog does not advertise.
    /// The binding must say so rather than report the configured effort.
    #[test]
    fn a_dropped_effort_is_reported_as_dropped_with_the_advertised_list() {
        let mut agent_config = agent_with_wire(OpenAiWireApi::Responses);
        agent_config.backend_endpoint =
            crate::inference::backends::setup::XAI_API_ENDPOINT.to_string();
        agent_config.sampling.reasoning_effort = Some(ReasoningEffort::Medium);
        agent_config.resolved_reasoning_efforts = Some(vec![ReasoningEffort::Low]);
        let effective = agent_config.effective_inference();
        assert_eq!(effective.reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(
            effective.reasoning_effort_dispatch,
            gents_protocol::row::ReasoningEffortBinding::DroppedUnsupported {
                effort: "medium".to_string(),
                advertised: Some(vec!["low".to_string()]),
            }
        );
    }

    /// An unsupported tier is refused at resolution, so it never becomes a
    /// binding; the agent is reported unavailable instead.
    #[test]
    fn an_unsupported_tier_is_refused_rather_than_bound() {
        assert!(validate_service_tier(
            BackendProviderKind::AnthropicApiKey,
            Some(ServiceTier::Fast)
        )
        .is_err());
        assert!(validate_service_tier(BackendProviderKind::AnthropicApiKey, None).is_ok());
        assert!(validate_service_tier(
            BackendProviderKind::OpenAiCompatible,
            Some(ServiceTier::Fast)
        )
        .is_ok());
    }

    fn agent_with_wire(openai_wire_api: OpenAiWireApi) -> ResolvedAgent {
        let node = stub_node();
        let binding =
            crate::store::identity::provision_agent_identity(node.identity.as_ref()).unwrap();
        let agent_identity =
            crate::store::identity::resolve_agent_identity(node.identity.as_ref(), &binding)
                .unwrap();
        let agent_did = binding.agent_did;
        ResolvedAgent {
            configuration: crate::inference::resolved::ResolvedConfiguration::Static,
            profile_id: None,
            agent_did,
            agent_identity,
            node,
            backend_id: Some("backend-general".to_string()),
            backend_provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api,
            backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
            backend_auth: crate::documents::BackendAuth::Unauthenticated,
            claude_platform: None,
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
            oauth_pool: Vec::new(),
        }
    }

    #[test]
    fn an_account_pool_change_recreates_the_agent_slot() {
        let agent = agent_with_wire(OpenAiWireApi::ChatCompletions);
        let mut grown = agent.clone();
        grown.oauth_pool = vec!["claude-a".to_string(), "claude-b".to_string()];
        let mut shrunk = grown.clone();
        shrunk.oauth_pool.pop();
        let fingerprint = crate::turn::completion_factory::agent_slot_fingerprint;
        assert_ne!(fingerprint(&agent), fingerprint(&grown));
        assert_ne!(fingerprint(&grown), fingerprint(&shrunk));
        assert_eq!(fingerprint(&grown), fingerprint(&grown.clone()));
    }

    #[test]
    fn resolve_backend_api_key_errors_when_env_var_named_but_unset() {
        let mut agent = agent_with_wire(OpenAiWireApi::ChatCompletions);
        agent.agent_did = "agent-alpha".to_string();
        agent.backend_auth = crate::documents::BackendAuth::Environment {
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
