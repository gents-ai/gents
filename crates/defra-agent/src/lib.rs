//! DefraDB-backed agent framework extracted from `agent-daemon`.
//!
//! This crate preserves the current single-profile agent loop pieces while
//! `agent-daemon` remains the first consumer during the extraction phase.

pub mod agent;
pub mod backend_registry;
pub mod compaction;
pub mod config;
pub mod error;
pub mod graphql;
pub mod health_checker;
pub mod hook;
pub mod identity;
pub mod lifecycle;
pub mod mcp_pool;
pub mod meta_tools;
pub mod oneshot;
pub mod prompt;
pub mod retry;
pub mod scheduler;
pub mod schema;
pub mod session;
pub mod streaming;
pub mod toolset;
pub mod truncation;
pub mod watcher;

pub use agent::{
    DefraAgent, DefraAgentBuilder, ProcessLifecycleObserver, ProcessLifecycleState, ProfileBuilder,
};
pub use backend_registry::{BackendTracker, InferenceBackend};
pub use config::{DaemonConfig, ProfileConfig};
pub use defra_node;
pub use health_checker::{spawn_health_checker, HealthStatus, ServiceHealth, ServiceHealthMap};
pub use hook::{DefraSessionHook, FailurePolicy, HookStats};
pub use identity::{AgentIdentity, ServiceAccount, SimpleIdentity};
pub use lifecycle::{RecoveryReport, RequestLifecycle};
pub use mcp_pool::McpPool;
pub use meta_tools::build_meta_tools;
pub use oneshot::{run_openai_oneshot, OneshotRunResult};
pub use prompt::{LayeredPromptBuilder, PromptBuilder};
pub use schema::{
    ensure_runtime_schemas, ensure_schemas, AGENT_CONVERSATION_SCHEMA, AGENT_MESSAGE_SCHEMA,
    AGENT_REQUEST_SCHEMA, AGENT_RESPONSE_SCHEMA, AGENT_SESSION_SCHEMA, AGENT_TOOL_CALL_SCHEMA,
    AGENT_TOOL_RESULT_SCHEMA, COMPACTION_ENTRY_SCHEMA, INFERENCE_BACKEND_SCHEMA,
    TOOL_SERVICE_REGISTRY_SCHEMA,
};
pub use streaming::{DefraStreamWriter, StreamWriter};
pub use toolset::{build_delegate_tool, build_native_tools, NativeTool, ToolSet, ToolSetBuilder};
pub use truncation::{DefraSpillTruncator, TruncationLimits, TruncationMode, Truncator};
pub use watcher::{AgentRequest, DefraWatcher, Watcher};
