//! DefraDB-backed agent framework extracted from `agent-daemon`.
//!
//! This crate preserves the current agent runtime pieces while
//! `agent-daemon` remains the first consumer during the extraction phase.

pub(crate) mod admission;
pub mod agent;
pub mod apply_model;
pub mod backend_provider;
pub mod backend_registry;
pub mod collection;
pub mod compaction;
pub(crate) mod completion_factory;
pub mod config;
pub mod desired_fields;
pub mod document_config;
pub mod error;
pub mod graphql;
pub mod health_checker;
pub mod hook;
pub mod identity;
pub mod interrupt;
pub mod lifecycle;
pub mod mcp_pool;
pub mod meta_tools;
pub mod oneshot;
pub mod prompt;
pub(crate) mod registry;
pub mod retry;
pub(crate) mod runtime_snapshot;
pub(crate) mod runtime_status;
pub mod schema;
pub mod session;
pub mod streaming;
pub mod template;
pub mod tool_surface;
pub mod toolset;
pub mod trace_export;
pub(crate) mod trigger_engine;
pub mod truncation;
pub mod watcher;

pub use collection::Collection;

pub use agent::{
    BehaviorBuilder, DefraAgent, DefraAgentBuilder, DocumentRuntimeOptions,
    ProcessLifecycleObserver, ProcessLifecycleState,
};
pub use backend_provider::{discover_models as discover_backend_models, BackendProviderKind};
pub use backend_registry::InferenceBackend;
pub use compaction::CompactionStrategy;
pub use config::BehaviorConfig;
pub use defra_agent_protocol::client_protocol;
pub use defra_node;
pub use desired_fields::{DesiredFields, LiveFields};
pub use document_config::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    default_tool_selection_id_for_behavior, ensure_agent_principal, list_agent_behaviors,
    load_agent_behavior, load_agent_principal, load_inference_profile, load_tool_selection,
    upsert_agent_behavior, upsert_agent_principal, upsert_inference_profile, upsert_tool_selection,
    AgentBehavior, AgentPrincipal, InferenceProfile, PrincipalBootstrap, ToolSelectionDocument,
};
pub use health_checker::{spawn_health_checker, HealthStatus, ServiceHealth, ServiceHealthMap};
pub use hook::{DefraSessionHook, FailurePolicy, HookStats};
pub use identity::{
    load_macos_keychain_identity, load_macos_secure_enclave_identity,
    load_or_create_macos_keychain_identity, load_or_create_macos_secure_enclave_identity,
    AgentIdentity, KeyIdentity, RegisteredIdentity, ServiceAccount,
};
pub use interrupt::{fetch_interrupt_requested_at, interrupt_request};
pub use lifecycle::{
    task_run_conversation_title, write_manual_agent_request,
    write_manual_agent_request_with_conversation_title, RecoveryReport, RequestLifecycle,
};
pub use mcp_pool::McpPool;
pub use meta_tools::build_meta_tools;
pub use oneshot::{run_openai_oneshot, run_openai_oneshot_with_tools, OneshotRunResult};
pub use prompt::{LayeredPromptBuilder, PromptBuilder};
pub use schema::{
    ensure_config_bootstrap_schemas, ensure_runtime_schemas, ensure_schemas, AGENT_BEHAVIOR_SCHEMA,
    AGENT_CONVERSATION_SCHEMA, AGENT_MESSAGE_SCHEMA, AGENT_PRINCIPAL_SCHEMA, AGENT_REQUEST_SCHEMA,
    AGENT_RESPONSE_SCHEMA, AGENT_RUNTIME_SCHEMA, AGENT_SESSION_SCHEMA, AGENT_TOOL_CALL_SCHEMA,
    AGENT_TOOL_RESULT_SCHEMA, COMPACTION_ENTRY_SCHEMA, INFERENCE_BACKEND_SCHEMA,
    INFERENCE_CALL_SCHEMA, INFERENCE_PROFILE_SCHEMA, SCHEDULE_SCHEMA, TASK_SCHEMA,
    TOOL_SELECTION_SCHEMA, TOOL_SERVICE_REGISTRY_SCHEMA,
};
pub use session::load_history;
pub use session::{fork, ForkError, ForkOutcome, ForkParams};
pub use streaming::{DefraStreamWriter, StreamWriter};
pub use template::{
    parse_template_for_validation, render_template, TemplateError, TemplateScope, VariableRef,
};
pub use tool_surface::{
    cli_tool, BashMode, BehaviorToolConfig, CustomToolFactory, FileToolMode, ToolCeiling,
    ToolRuntimeContext, ToolSelection, ToolSurface,
};
pub use toolset::{
    build_delegate_tool, build_native_tools, CliToolConfig, NativeTool, ToolSet, ToolSetBuilder,
    DELEGATE_TOOL_NAME,
};
pub use truncation::{DefraSpillTruncator, TruncationLimits, TruncationMode, Truncator};
pub use watcher::{AgentRequest, DefraWatcher, Watcher};

// Inline test module preserved: single-test smoke check, deliberately not extracted to keep it co-located with the narrow code it tests.
#[cfg(test)]
mod public_api_tests {
    use super::*;

    #[test]
    fn downstream_oneshot_analysis_surface_is_available_from_crate_root() {
        let _strategy = CompactionStrategy::StripThenSummarize;
        let _ensure = ensure_schemas;
        let _history = load_history;
        let _oneshot = run_openai_oneshot_with_tools;
    }
}
