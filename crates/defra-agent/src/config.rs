use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::backend_provider::BackendProviderKind;
use crate::compaction::CompactionStrategy;
use crate::identity::{AgentIdentity, AgentPrincipal};
use crate::tool_surface::BehaviorToolConfig;

pub const DEFAULT_CONTEXT_WINDOW: usize = 131_072;
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 32_768;
pub const DEFAULT_MAX_TURNS: usize = 50;
pub const DEFAULT_STREAM_BATCH_MS: u64 = 1_000;
pub const DEFAULT_COMPACTION_THRESHOLD: f64 = 0.75;
pub const DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS: u64 = 1_800;
pub const DEFAULT_DEADLINE_DURATION_SECS: u64 = 1_800;
pub const DEFAULT_MODEL_NAME: &str = "default";

/// Runtime configuration for one loaded behavior executor.
///
/// Mirrors the Lean `Identity.Behavior` record. Holds an
/// `Arc<AgentPrincipal>` back-reference; the principal owns the
/// signing identity used for all DefraDB ops issued for this
/// behavior. Two behaviors sharing the same principal Arc share the
/// same actor DID (Lean's `behavior_id_determines_principal` is
/// structural at the type level here).
#[derive(Clone)]
pub struct AgentBehavior {
    pub behavior_id: String,
    pub principal: Arc<AgentPrincipal>,
    pub backend_id: Option<String>,
    pub backend_provider_kind: BackendProviderKind,
    pub backend_endpoint: String,
    pub backend_api_key: Option<String>,
    pub backend_api_key_env_var: Option<String>,
    pub model_name: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub max_turns: usize,
    pub system_prompt: String,
    pub request_context_template: Option<String>,
    pub tools: BehaviorToolConfig,
    pub compaction_threshold: f64,
    pub compaction_strategy: CompactionStrategy,
    pub stream_batch_ms: u64,
    pub stream_liveness_timeout: Duration,
    pub deadline_duration: Duration,
    pub sampling: SamplingConfig,
    /// Effective skill set for this behavior (decision D5), resolved at
    /// snapshot-build time. Their instructions compose into the prompt
    /// preamble; their tool deps are intersected with the tool ceiling and
    /// never widen it (decision D3). See `crate::skills`.
    pub skills: Vec<crate::skills::Skill>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SamplingConfig {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub max_tokens: Option<u64>,
}

impl SamplingConfig {
    pub fn is_empty(self) -> bool {
        self.temperature.is_none()
            && self.top_p.is_none()
            && self.top_k.is_none()
            && self.max_tokens.is_none()
    }

    pub fn additional_params(self) -> Option<serde_json::Value> {
        let mut params = serde_json::Map::new();
        if let Some(top_p) = self.top_p {
            params.insert("top_p".to_string(), serde_json::json!(top_p));
        }
        if let Some(top_k) = self.top_k {
            params.insert("top_k".to_string(), serde_json::json!(top_k));
        }

        (!params.is_empty()).then_some(serde_json::Value::Object(params))
    }
}

impl std::fmt::Debug for AgentBehavior {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentBehavior")
            .field("behavior_id", &self.behavior_id)
            .field("principal_did", &self.principal.agent_did)
            .field("backend_id", &self.backend_id)
            .field("backend_provider_kind", &self.backend_provider_kind)
            .field("backend_endpoint", &self.backend_endpoint)
            .field(
                "backend_api_key",
                &self.backend_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("backend_api_key_env_var", &self.backend_api_key_env_var)
            .field("model_name", &self.model_name)
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("max_turns", &self.max_turns)
            .field("system_prompt", &self.system_prompt)
            .field("request_context_template", &self.request_context_template)
            .field("tools", &self.tools)
            .field("compaction_threshold", &self.compaction_threshold)
            .field("compaction_strategy", &self.compaction_strategy)
            .field("stream_batch_ms", &self.stream_batch_ms)
            .field("stream_liveness_timeout", &self.stream_liveness_timeout)
            .field("deadline_duration", &self.deadline_duration)
            .field("sampling", &self.sampling)
            // Included so the runtime configuration fingerprint (which hashes
            // `{behavior:?}`) changes when a behavior's effective skills change,
            // letting the control watcher reconcile live skill updates (#340).
            .field("skills", &self.skills)
            .finish()
    }
}

impl AgentBehavior {
    /// Returns the principal's agent_did.
    pub fn agent_did(&self) -> &str {
        &self.principal.agent_did
    }

    /// Returns the principal's signing identity.
    ///
    /// This is the only way to obtain an `Arc<dyn AgentIdentity>` for
    /// a behavior; the behavior itself does not hold one. Two
    /// behaviors sharing an `Arc<AgentPrincipal>` return identical
    /// clones, so DefraDB ACP receives the same actor for both —
    /// satisfying Lean's `RespectsPrincipal` predicate.
    pub fn principal_identity(&self) -> &Arc<dyn AgentIdentity> {
        &self.principal.identity
    }

    pub fn resolve_backend_api_key(&self) -> Result<Option<String>> {
        if let Some(api_key) = normalize_optional_secret(self.backend_api_key.as_deref()) {
            return Ok(Some(api_key.to_string()));
        }

        if let Some(env_var) = normalize_optional_env_var(self.backend_api_key_env_var.as_deref()) {
            let value = std::env::var(env_var).with_context(|| {
                format!(
                    "backend {} for behavior {} requires environment variable {}",
                    self.backend_id.as_deref().unwrap_or("<unbound>"),
                    self.behavior_id,
                    env_var
                )
            })?;
            let value = value.trim();
            if value.is_empty() {
                anyhow::bail!(
                    "backend {} for behavior {} resolved empty API key from environment variable {}",
                    self.backend_id.as_deref().unwrap_or("<unbound>"),
                    self.behavior_id,
                    env_var
                );
            }
            return Ok(Some(value.to_string()));
        }

        Ok(None)
    }

    pub fn completion_client_api_key(&self) -> Result<String> {
        Ok(self
            .resolve_backend_api_key()?
            .unwrap_or_else(|| "no-key".to_string()))
    }
}

fn normalize_optional_env_var(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn normalize_optional_secret(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}
