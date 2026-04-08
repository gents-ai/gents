//! Agent daemon configuration — data rooms, model endpoints, tools.
//!
//! Each agent daemon is configured with a "data room" that defines its
//! system prompt, available tools, model endpoint, and context window size.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::compaction::CompactionStrategy;
use crate::identity::AgentIdentity;
use crate::toolset::ToolSet;

pub const DEFAULT_CONTEXT_WINDOW: usize = 131_072;
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 32_768;
pub const DEFAULT_MAX_TURNS: usize = 50;
pub const DEFAULT_STREAM_BATCH_MS: u64 = 1_000;
pub const DEFAULT_COMPACTION_THRESHOLD: f64 = 0.75;
pub const DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS: u64 = 120;
pub const DEFAULT_DEADLINE_DURATION_SECS: u64 = 900;
pub const DEFAULT_MODEL_ENDPOINT: &str = "http://localhost:8000/v1";
pub const DEFAULT_MODEL_NAME: &str = "default";

/// Configuration for an agent daemon instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Data room name (e.g., "general", "research", "code").
    pub data_room: String,
    /// Static system prompt for this agent.
    pub system_prompt: String,
    /// Model inference endpoint URL (e.g., vLLM-MLX local endpoint).
    pub model_endpoint: String,
    /// Backend binding for requests executed by this daemon.
    pub backend_id: String,
    /// Model name to request from the endpoint.
    pub model_name: String,
    /// Context window size in tokens.
    pub context_window: usize,
    /// Maximum output tokens per response.
    pub max_output_tokens: usize,
    /// Maximum model turns per request, including tool round-trips.
    pub max_turns: usize,
    /// Streaming batch interval in milliseconds.
    pub stream_batch_ms: u64,
    /// Compaction threshold (fraction of context window).
    pub compaction_threshold: f64,
    /// Stream liveness timeout in seconds. If no stream item (token,
    /// tool call, tool result) arrives within this window, the stream
    /// is considered dead. This is NOT a wall-clock limit — the timer
    /// resets on every item received. Default: 120s (generous for slow
    /// local inference).
    pub stream_liveness_timeout_secs: u64,
    /// Deadline duration in seconds for request processing. When the
    /// daemon claims a request, it sets deadline = now + this value.
    /// Clients detect stalls by checking if the deadline has passed.
    pub deadline_duration_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            data_room: "general".to_string(),
            system_prompt: String::new(),
            model_endpoint: DEFAULT_MODEL_ENDPOINT.to_string(),
            backend_id: String::new(),
            model_name: DEFAULT_MODEL_NAME.to_string(),
            context_window: DEFAULT_CONTEXT_WINDOW,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            max_turns: DEFAULT_MAX_TURNS,
            stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
            compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
            stream_liveness_timeout_secs: DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
            deadline_duration_secs: DEFAULT_DEADLINE_DURATION_SECS,
        }
    }
}

impl DaemonConfig {
    pub fn load(path: Option<&Path>, data_room_override: Option<&str>) -> Result<Self> {
        let mut config = match path {
            Some(path) => {
                let contents = std::fs::read_to_string(path)
                    .with_context(|| format!("reading config file {}", path.display()))?;
                toml::from_str(&contents)
                    .with_context(|| format!("parsing config file {}", path.display()))?
            }
            None => Self::default(),
        };

        config.apply_env_overrides();

        if let Some(data_room) = data_room_override {
            config.data_room = data_room.to_string();
        }

        Ok(config)
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(value) = std::env::var("AGENT_DAEMON_DATA_ROOM") {
            self.data_room = value;
        }
        if let Ok(value) = std::env::var("AGENT_DAEMON_SYSTEM_PROMPT") {
            self.system_prompt = value;
        }
        if let Ok(value) = std::env::var("AGENT_DAEMON_MODEL_ENDPOINT") {
            self.model_endpoint = value;
        }
        if let Ok(value) = std::env::var("AGENT_DAEMON_BACKEND_ID") {
            self.backend_id = value;
        }
        if let Ok(value) = std::env::var("AGENT_DAEMON_MODEL_NAME") {
            self.model_name = value;
        }
        parse_env_override("AGENT_DAEMON_CONTEXT_WINDOW", &mut self.context_window);
        parse_env_override(
            "AGENT_DAEMON_MAX_OUTPUT_TOKENS",
            &mut self.max_output_tokens,
        );
        parse_env_override("AGENT_DAEMON_MAX_TURNS", &mut self.max_turns);
        parse_env_override("AGENT_DAEMON_STREAM_BATCH_MS", &mut self.stream_batch_ms);
        parse_env_override(
            "AGENT_DAEMON_COMPACTION_THRESHOLD",
            &mut self.compaction_threshold,
        );
        parse_env_override(
            "AGENT_DAEMON_STREAM_LIVENESS_TIMEOUT_SECS",
            &mut self.stream_liveness_timeout_secs,
        );
        parse_env_override(
            "AGENT_DAEMON_DEADLINE_DURATION_SECS",
            &mut self.deadline_duration_secs,
        );
    }
}

fn parse_env_override<T: std::str::FromStr + std::fmt::Display>(var: &str, target: &mut T)
where
    T::Err: std::fmt::Display,
{
    if let Ok(value) = std::env::var(var) {
        match value.parse() {
            Ok(parsed) => *target = parsed,
            Err(e) => {
                tracing::warn!(
                    var = %var,
                    value = %value,
                    error = %e,
                    "ignoring unparseable env var, using default"
                );
            }
        }
    }
}

/// Multi-profile runtime configuration for a single Defra agent profile.
#[derive(Clone)]
pub struct ProfileConfig {
    pub name: String,
    pub identity: Arc<dyn AgentIdentity>,
    pub backend_id: Option<String>,
    pub model_endpoint: String,
    pub model_name: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub max_turns: usize,
    pub system_prompt: String,
    pub native_tools: ToolSet,
    pub compaction_threshold: f64,
    pub compaction_strategy: CompactionStrategy,
    pub stream_batch_ms: u64,
    pub deadline_duration: Duration,
}

impl std::fmt::Debug for ProfileConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileConfig")
            .field("name", &self.name)
            .field("identity_did", &self.identity.did())
            .field("backend_id", &self.backend_id)
            .field("model_endpoint", &self.model_endpoint)
            .field("model_name", &self.model_name)
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("max_turns", &self.max_turns)
            .field("native_tools", &self.native_tools)
            .field("compaction_threshold", &self.compaction_threshold)
            .field("compaction_strategy", &self.compaction_strategy)
            .field("stream_batch_ms", &self.stream_batch_ms)
            .field("deadline_duration", &self.deadline_duration)
            .finish()
    }
}

impl ProfileConfig {
    pub fn did(&self) -> &str {
        self.identity.did()
    }
}
