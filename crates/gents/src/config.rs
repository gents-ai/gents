use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::agent::completion_retry::CompletionRetryProfileFields;
use crate::backend_provider::BackendProviderKind;
use crate::compaction::CompactionStrategy;
use crate::identity::{AgentIdentity, RuntimePrincipal};
use crate::openai_wire::OpenAiWireApi;
use crate::tool_surface::BehaviorToolConfig;

pub const DEFAULT_CONTEXT_WINDOW: usize = 131_072;
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 32_768;
pub const DEFAULT_MAX_TURNS: usize = 1_000;
pub const DEFAULT_STREAM_BATCH_MS: u64 = 1_000;
pub const DEFAULT_COMPACTION_THRESHOLD: f64 = 0.75;
// Moved to gents-loop (G-1): compaction's own default/clamp constants.
pub use gents_loop::compaction::{
    DEFAULT_COMPACTION_SUMMARY_FILE_LIST_MAX, DEFAULT_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS,
    MAX_COMPACTION_SUMMARY_FILE_LIST_MAX, MAX_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS,
};
/// Default execution lease duration, exposed by InferenceExecution's
/// stream_liveness_timeout_secs field. The owned renewal task keeps live work
/// current independently of provider output.
pub const DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS: u64 = 120;
/// Default for InferenceExecution's provider_idle_timeout_secs: the longest a
/// provider attempt's transport may stay silent (header wait included) before
/// the owned loop fails the attempt through the completion retry owner.
/// Independent of the execution lease. Sized for keepalive-less first-token
/// and reasoning silence (Responses reasoning without summaries, long vLLM
/// prefill); Codex CLI uses the same 300s stream idle bound.
pub const DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS: u64 = 300;
/// Overall wall-clock budget for a claimed request. Long-running goals may
/// legitimately work for many hours while continuing to emit model/tool data.
pub const DEFAULT_DEADLINE_DURATION_SECS: u64 = 86_400;
pub const DEFAULT_MODEL_NAME: &str = "default";

/// Fully resolved runtime configuration for one behavior executor. Holds an
/// `Arc<RuntimePrincipal>` back-reference; the principal owns the
/// signing identity used for all DefraDB ops issued for this
/// behavior. The resolved behavior therefore carries the validated owner and
/// signing handle selected by its principal-and-behavior key.
#[derive(Clone)]
pub struct ResolvedBehavior {
    pub behavior_id: String,
    pub principal: Arc<RuntimePrincipal>,
    pub backend_id: Option<String>,
    pub backend_provider_kind: BackendProviderKind,
    pub openai_wire_api: OpenAiWireApi,
    pub backend_endpoint: String,
    pub backend_auth: crate::document_config::BackendAuth,
    pub model_name: String,
    /// Provider-advertised effort choices for the selected model. `None` is
    /// unknown and must not be inferred from the model name at dispatch.
    pub resolved_reasoning_efforts: Option<Vec<ReasoningEffort>>,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub max_turns: usize,
    pub max_turns_provenance: MaxTurnsProvenance,
    pub system_prompt: String,
    pub tools: BehaviorToolConfig,
    /// Canonical compaction selection; absence uses runtime defaults.
    pub compaction: Option<crate::document_config::CompactionConfig>,
    /// Optional summary inference resolved through the same profile chain.
    pub compaction_inference: Option<ResolvedInference>,
    /// Aggregate budget pinned by the physical request execution owner.
    pub max_total_tokens: Option<u64>,
    pub stream_batch_ms: u64,
    pub stream_liveness_timeout: Duration,
    pub provider_idle_timeout: Duration,
    pub deadline_duration: Duration,
    pub completion_retry: CompletionRetryProfileFields,
    pub sampling: SamplingConfig,
    /// Effective skill set for this behavior (decision D5), resolved at
    /// snapshot-build time. Their instructions compose into the prompt
    /// preamble; their tool deps are intersected with the tool ceiling and
    /// never widen it (decision D3). See `crate::skills`.
    pub skills: Vec<crate::skills::Skill>,
}

/// Selected canonical documents after one owner-scoped reference resolution.
/// Shared by user inference and optional compaction inference; no alternate
/// model selection or independently authored provider configuration exists.
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
        if self.backend.provider_kind == BackendProviderKind::ClaudeCliSubscription {
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

/// Which owner set the effective `max_turns`. A behavior resolved from
/// documents may have no `InferenceExecution` document at all, and a behavior
/// built programmatically has none by construction, so the three cases name
/// different knobs.
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
                "no max_turns is configured for this behavior; this is the built-in default, raised by setting max_turns on an InferenceExecution document bound to the behavior's inference profile"
            }
            MaxTurnsProvenance::ExecutionProfile => {
                "max_turns is set explicitly by the InferenceExecution document bound to this behavior's inference profile"
            }
            MaxTurnsProvenance::BuilderOverride => {
                "max_turns was set programmatically through BehaviorBuilder::max_turns; no InferenceExecution document controls it"
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
    /// Sampling knobs the provider takes as extra body params (#649). rig's
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

    /// The sampling knobs that must travel as provider body params.
    ///
    /// `temperature` and `max_tokens` are modeled fields on rig's request, and
    /// `reasoning_effort` needs a provider-specific wire shape in
    /// `completion_factory`; everything else is emitted here and deep-merged
    /// into `additional_params` at the request boundary. A `None` knob emits
    /// nothing at all — the served model's own default stands, which is the
    /// pre-#649 behavior for every profile that does not pin a value.
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

impl std::fmt::Debug for ResolvedBehavior {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedBehavior")
            .field("behavior_id", &self.behavior_id)
            .field("principal_did", &self.principal.agent_did)
            .field("backend_id", &self.backend_id)
            .field("backend_provider_kind", &self.backend_provider_kind)
            .field("openai_wire_api", &self.openai_wire_api)
            .field("backend_endpoint", &self.backend_endpoint)
            .field("backend_auth", &self.backend_auth)
            .field("model_name", &self.model_name)
            .field(
                "resolved_reasoning_efforts",
                &self.resolved_reasoning_efforts,
            )
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("max_turns", &self.max_turns)
            // Included for the same reason as `skills`: reconcile and slot
            // selection compare `{behavior:?}`, so a provenance-only edit
            // (unset <-> an explicit value equal to the default) would
            // otherwise fingerprint identically and leave the running slot
            // reporting the previous provenance.
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
            // Included so the runtime configuration fingerprint (which hashes
            // `{behavior:?}`) changes when a behavior's effective skills change,
            // letting the control watcher reconcile live skill updates (#340).
            .field("skills", &self.skills)
            .finish()
    }
}

impl ResolvedBehavior {
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

    /// Returns the principal's agent_did.
    pub fn agent_did(&self) -> &str {
        &self.principal.agent_did
    }

    /// Returns the principal's signing identity.
    ///
    /// This is the only way to obtain an `Arc<dyn AgentIdentity>` for
    /// a behavior; the behavior itself does not hold one. Two
    /// behaviors sharing an `Arc<RuntimePrincipal>` return identical
    /// clones, so DefraDB ACP receives the same actor for both —
    /// satisfying Lean's `RespectsPrincipal` predicate.
    pub fn principal_identity(&self) -> &Arc<dyn AgentIdentity> {
        &self.principal.identity
    }

    /// Resolve the explicitly selected shared credential. Principal OAuth is
    /// handled by its existing credential owner, never an unauthenticated fallback.
    pub fn resolve_backend_api_key(&self) -> Result<Option<String>> {
        self.backend_auth.resolve_api_key().map_err(|error| {
            anyhow::anyhow!(
                "backend {} for behavior {}: {error:#}",
                self.backend_id.as_deref().unwrap_or("<unbound>"),
                self.behavior_id
            )
        })
    }

    pub fn completion_client_api_key(&self) -> Result<String> {
        Ok(self
            .resolve_backend_api_key()?
            .unwrap_or_else(|| "no-key".to_string()))
    }
}

/// Resolve a backend's explicitly selected shared credential, retaining backend
/// identity in configuration errors. OAuth uses the principal credential owner.
pub fn resolve_backend_api_key(
    backend: &crate::backend_registry::InferenceBackend,
) -> Result<Option<String>> {
    backend
        .auth
        .resolve_api_key()
        .map_err(|error| anyhow::anyhow!("backend {}: {error:#}", backend.backend_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::KeyIdentity;

    fn stub_principal() -> Arc<RuntimePrincipal> {
        let identity = Arc::new(
            KeyIdentity::load_or_create(
                std::env::temp_dir().join(format!("config-behavior-{}.key", uuid::Uuid::new_v4())),
                None,
            )
            .unwrap(),
        );
        Arc::new(RuntimePrincipal {
            agent_did: identity.did().to_string(),
            identity,
            default_behavior_id: String::new(),
            display_name: None,
            enabled: true,
        })
    }

    fn behavior_with_wire(openai_wire_api: OpenAiWireApi) -> ResolvedBehavior {
        ResolvedBehavior {
            behavior_id: "general".to_string(),
            principal: stub_principal(),
            backend_id: Some("backend-general".to_string()),
            backend_provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api,
            backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
            backend_auth: crate::document_config::BackendAuth::Unauthenticated,
            model_name: DEFAULT_MODEL_NAME.to_string(),
            resolved_reasoning_efforts: None,
            context_window: DEFAULT_CONTEXT_WINDOW,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            max_turns: DEFAULT_MAX_TURNS,
            max_turns_provenance: MaxTurnsProvenance::Default,
            system_prompt: "system".to_string(),
            tools: BehaviorToolConfig::meta_only(),
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

    /// A behavior bound to a backend whose environment authentication names an
    /// environment variable that isn't actually set in the process must fail
    /// loudly, naming both the backend and the behavior — never silently
    /// build/run with no key (#1338).
    #[test]
    fn resolve_backend_api_key_errors_when_env_var_named_but_unset() {
        let mut behavior = behavior_with_wire(OpenAiWireApi::ChatCompletions);
        // Deliberately not a substring of `backend-general` (the fixture's
        // backend id) so the two assertions below can't pass vacuously off
        // one shared match.
        behavior.behavior_id = "behavior-alpha".to_string();
        behavior.backend_auth = crate::document_config::BackendAuth::Environment {
            variable: "GENTS_CONFIG_TEST_KEY_MISSING_1338_NEVER_SET".into(),
        };

        let error = behavior
            .resolve_backend_api_key()
            .expect_err("a named-but-unset env var must hard error");
        let message = error.to_string();
        assert!(
            message.contains("backend-general"),
            "error must name the backend: {message}"
        );
        assert!(
            message.contains("behavior-alpha"),
            "error must name the behavior: {message}"
        );
        assert!(
            message.contains("GENTS_CONFIG_TEST_KEY_MISSING_1338_NEVER_SET"),
            "error must name the environment variable: {message}"
        );
    }

    #[test]
    fn resolve_backend_api_key_none_when_no_env_var_configured() {
        let behavior = behavior_with_wire(OpenAiWireApi::ChatCompletions);
        assert_eq!(
            behavior
                .resolve_backend_api_key()
                .expect("no key configured is not an error"),
            None
        );
    }

    #[test]
    fn default_max_turns_supports_long_running_agents() {
        assert_eq!(DEFAULT_MAX_TURNS, 1_000);
    }

    #[test]
    fn max_turns_resolves_default_provenance_when_unset() {
        let resolved = resolve_max_turns(None).expect("default resolves");
        assert_eq!(resolved.value, DEFAULT_MAX_TURNS);
        assert_eq!(resolved.provenance, MaxTurnsProvenance::Default);
    }

    #[test]
    fn max_turns_resolves_explicit_provenance_when_set() {
        let resolved = resolve_max_turns(Some(40)).expect("explicit value resolves");
        assert_eq!(resolved.value, 40);
        assert_eq!(resolved.provenance, MaxTurnsProvenance::ExecutionProfile);
    }

    #[test]
    fn max_turns_keeps_a_persisted_explicit_value_below_the_new_default() {
        let resolved = resolve_max_turns(Some(250)).expect("persisted value resolves");
        assert_eq!(resolved.value, 250);
        assert_eq!(resolved.provenance, MaxTurnsProvenance::ExecutionProfile);
    }

    #[test]
    fn max_turns_explicitly_set_to_the_default_value_is_still_explicit() {
        let configured =
            i64::try_from(DEFAULT_MAX_TURNS).expect("default fits in the authored field");
        let resolved = resolve_max_turns(Some(configured)).expect("explicit value resolves");
        assert_eq!(resolved.value, DEFAULT_MAX_TURNS);
        assert_eq!(resolved.provenance, MaxTurnsProvenance::ExecutionProfile);
    }

    #[test]
    fn max_turns_rejects_non_positive_explicit_value_regardless_of_provenance() {
        let error = resolve_max_turns(Some(0)).expect_err("zero must still be rejected");
        assert!(error.to_string().contains("max_turns must be positive"));
    }

    #[test]
    fn max_turns_provenance_descriptions_name_a_reachable_knob() {
        assert!(MaxTurnsProvenance::Default
            .describe()
            .contains("built-in default"));
        assert!(MaxTurnsProvenance::ExecutionProfile
            .describe()
            .contains("InferenceExecution document"));
        let builder = MaxTurnsProvenance::BuilderOverride.describe();
        assert!(builder.contains("BehaviorBuilder::max_turns"));
        assert!(
            builder.contains("no InferenceExecution document"),
            "a builder-supplied limit must not point the operator at a document: {builder}"
        );
    }

    #[test]
    fn default_request_deadline_supports_long_running_agents() {
        assert_eq!(DEFAULT_DEADLINE_DURATION_SECS, 86_400);
    }

    #[test]
    fn default_execution_lease_is_two_minutes() {
        assert_eq!(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS, 120);
        assert_eq!(
            behavior_with_wire(OpenAiWireApi::ChatCompletions).stream_liveness_timeout,
            Duration::from_secs(120),
        );
    }

    #[test]
    fn reasoning_effort_accepts_provider_vocabulary() {
        for value in [
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ] {
            let effort = ReasoningEffort::parse(value).expect("known effort must parse");
            assert_eq!(effort.as_str(), value);
        }
        assert!(ReasoningEffort::parse("extreme").is_err());
    }

    /// TA-1 (#566 review): `openai_wire_api` must appear in the manual `Debug`
    /// impl, because the runtime configuration fingerprint hashes
    /// `format!("{behavior:?}")` (see `runtime_snapshot::configuration_fingerprint`).
    /// Without the Debug field, switching a backend's wire API would not change the
    /// fingerprint, so the control watcher would never reconcile the change into a new
    /// generation. Deleting the `.field("openai_wire_api", …)` line makes these equal.
    #[test]
    fn debug_distinguishes_openai_wire_api_for_reconcile_fingerprint() {
        let chat = format!("{:?}", behavior_with_wire(OpenAiWireApi::ChatCompletions));
        let responses = format!("{:?}", behavior_with_wire(OpenAiWireApi::Responses));
        assert_ne!(
            chat, responses,
            "openai_wire_api must be in ResolvedBehavior Debug so the runtime fingerprint \
             changes when the wire API changes"
        );
        assert!(chat.contains("ChatCompletions"));
        assert!(responses.contains("Responses"));
    }
}

/// Whether a profile's context window is admissible for its advertised model:
/// a selected window may not exceed a well-formed advertised maximum (a
/// positive maximum no smaller than the advertised default). Runtime inference
/// resolution and any client presenting the effective window share this check.
pub fn validate_advertised_context_override(
    profile: &crate::document_config::InferenceProfile,
    model: &crate::document_config::AdvertisedModel,
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

/// The model catalog a backend's observation advertises in the backend's
/// credential scope: the principal's own catalog for principal OAuth, the
/// shared catalog otherwise. `None` when nothing has been observed; an error
/// when the observation holds more than one catalog for that scope.
pub fn backend_catalog<'a>(
    backend: &crate::document_config::InferenceBackend,
    observation: Option<&'a crate::document_config::InferenceBackendObservation>,
) -> anyhow::Result<Option<&'a crate::document_config::BackendModelCatalog>> {
    let credential_scope = matches!(
        backend.auth,
        crate::document_config::BackendAuth::PrincipalOAuth
    )
    .then_some(backend.agent_did.as_str());
    Ok(observation
        .filter(|observation| observation.backend_id == backend.backend_id)
        .map(|observation| observation.catalog_for(credential_scope))
        .transpose()?
        .flatten())
}

/// The advertised model a profile selects on its backend, admitted against
/// that advertisement: the model must be advertised exactly once, support the
/// selected reasoning effort, and accept the profile's context window
/// ([`validate_advertised_context_override`]). `None` when the backend's
/// catalog has not been observed, in which case nothing is known about the
/// model and nothing is admitted or rejected.
pub fn advertised_model_for_profile(
    backend: &crate::document_config::InferenceBackend,
    profile: &crate::document_config::InferenceProfile,
    observation: Option<&crate::document_config::InferenceBackendObservation>,
) -> anyhow::Result<Option<crate::document_config::AdvertisedModel>> {
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
            supported.contains(&effort),
            "model {} does not advertise selected reasoning effort {effort:?}",
            profile.model_name
        );
    }
    validate_advertised_context_override(profile, model)?;
    Ok(Some(model.clone()))
}
