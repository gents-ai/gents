use std::sync::Arc;
use std::time::Duration;

use crate::compaction::CompactionStrategy;
use crate::identity::AgentIdentity;
use crate::tool_surface::BehaviorToolConfig;

pub const DEFAULT_CONTEXT_WINDOW: usize = 131_072;
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 32_768;
pub const DEFAULT_MAX_TURNS: usize = 50;
pub const DEFAULT_STREAM_BATCH_MS: u64 = 1_000;
pub const DEFAULT_COMPACTION_THRESHOLD: f64 = 0.75;
pub const DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS: u64 = 300;
pub const DEFAULT_DEADLINE_DURATION_SECS: u64 = 900;
pub const DEFAULT_MODEL_NAME: &str = "default";

/// Runtime configuration for one loaded behavior executor.
#[derive(Clone)]
pub struct BehaviorConfig {
    pub name: String,
    pub identity: Arc<dyn AgentIdentity>,
    pub backend_id: Option<String>,
    pub backend_endpoint: String,
    pub model_name: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub max_turns: usize,
    pub system_prompt: String,
    pub tools: BehaviorToolConfig,
    pub compaction_threshold: f64,
    pub compaction_strategy: CompactionStrategy,
    pub stream_batch_ms: u64,
    pub deadline_duration: Duration,
}

impl std::fmt::Debug for BehaviorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BehaviorConfig")
            .field("name", &self.name)
            .field("identity_did", &self.identity.did())
            .field("backend_id", &self.backend_id)
            .field("backend_endpoint", &self.backend_endpoint)
            .field("model_name", &self.model_name)
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("max_turns", &self.max_turns)
            .field("system_prompt", &self.system_prompt)
            .field("tools", &self.tools)
            .field("compaction_threshold", &self.compaction_threshold)
            .field("compaction_strategy", &self.compaction_strategy)
            .field("stream_batch_ms", &self.stream_batch_ms)
            .field("deadline_duration", &self.deadline_duration)
            .finish()
    }
}

impl BehaviorConfig {
    pub fn did(&self) -> &str {
        self.identity.did()
    }
}
