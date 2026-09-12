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
pub const DEFAULT_MAX_TURNS: usize = 250;
pub const DEFAULT_STREAM_BATCH_MS: u64 = 100;
pub const DEFAULT_COMPACTION_THRESHOLD: f64 = 0.75;
// Moved to gents-loop (G-1): compaction's own default/clamp constants.
pub use gents_loop::compaction::{
    DEFAULT_COMPACTION_SUMMARY_FILE_LIST_MAX, DEFAULT_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS,
    MAX_COMPACTION_SUMMARY_FILE_LIST_MAX, MAX_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS,
};
/// Maximum provider-stream silence before treating the connection as dead.
pub const DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS: u64 = 1_800;
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
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub max_turns: usize,
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

    pub fn max_turns(&self) -> Result<usize> {
        positive_inference_limit(
            self.execution
                .as_ref()
                .and_then(|execution| execution.max_turns),
            DEFAULT_MAX_TURNS,
            "max_turns",
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
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("max_turns", &self.max_turns)
            .field("system_prompt", &self.system_prompt)
            .field("tools", &self.tools)
            .field("compaction", &self.compaction)
            .field("compaction_inference", &self.compaction_inference)
            .field("max_total_tokens", &self.max_total_tokens)
            .field("stream_batch_ms", &self.stream_batch_ms)
            .field("stream_liveness_timeout", &self.stream_liveness_timeout)
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
            context_window: DEFAULT_CONTEXT_WINDOW,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            max_turns: DEFAULT_MAX_TURNS,
            system_prompt: "system".to_string(),
            tools: BehaviorToolConfig::meta_only(),
            compaction: None,
            compaction_inference: None,
            max_total_tokens: None,
            stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
            stream_liveness_timeout: Duration::from_secs(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS),
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
        assert_eq!(DEFAULT_MAX_TURNS, 250);
    }

    #[test]
    fn default_request_deadline_supports_long_running_agents() {
        assert_eq!(DEFAULT_DEADLINE_DURATION_SECS, 86_400);
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
