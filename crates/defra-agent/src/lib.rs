//! DefraDB-backed agent framework extracted from `agent-daemon`.
//!
//! This crate preserves the current agent runtime pieces while
//! `agent-daemon` remains the first consumer during the extraction phase.

pub(crate) mod admission;
pub mod agent;
pub mod apply_model;
pub mod backend_provider;
pub mod backend_registry;
pub mod background_completion;
pub(crate) mod background_tools;
pub mod chatgpt_codex;
pub mod collection;
pub mod compaction;
pub(crate) mod completion_factory;
pub mod config;
pub mod defra_query;
pub mod desired_fields;
pub mod document_config;
pub mod error;
pub mod event_delivery_contract;
pub mod graphql;
pub mod health_checker;
pub mod hook;
pub mod identity;
pub mod interrupt;
#[cfg(test)]
pub(crate) mod lean_vocab_test;
pub mod lifecycle;
pub(crate) mod managed_exec;
pub mod mcp_pool;
pub mod meta_tools;
pub mod migration;
pub mod native_executor_status;
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
pub mod tool_call_lifecycle;
pub mod tool_control;
pub mod tool_surface;
pub mod toolset;
pub mod trace_export;
pub(crate) mod trigger_engine;
pub mod truncation;
pub mod watcher;

pub use collection::Collection;

pub use admission::BackendAdmissionConfig;
pub use admission::{InferenceCall, InferenceCallRecoveryReport};
pub use agent::{
    BehaviorBuilder, DefraAgent, DefraAgentBuilder, DocumentRuntimeOptions,
    ProcessLifecycleObserver, ProcessLifecycleState,
};
pub use backend_provider::{discover_models as discover_backend_models, BackendProviderKind};
pub use backend_registry::{InferenceBackend, HEALTHY_PROBE_STATUS, UNKNOWN_PROBE_STATUS};
pub use compaction::CompactionStrategy;
pub use config::{
    AgentBehavior, SamplingConfig, DEFAULT_COMPACTION_THRESHOLD, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS,
    DEFAULT_MODEL_NAME, DEFAULT_STREAM_BATCH_MS,
};
pub use defra_agent_protocol::client_protocol;
pub use defra_node;
pub use desired_fields::{DesiredFields, LiveFields};
pub use document_config::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    default_tool_selection_id_for_behavior, ensure_agent_principal, list_agent_behaviors,
    list_inference_profile_records, load_agent_behavior, load_agent_principal,
    load_inference_profile, load_tool_selection, upsert_agent_behavior, upsert_agent_principal,
    upsert_inference_profile, upsert_tool_selection, AgentBehavior as AgentBehaviorDocument,
    InferenceProfile, PrincipalBootstrap, ToolSelectionDocument,
};
pub use health_checker::{
    run_health_check_cycle, spawn_health_checker, HealthCheckerOptions, HealthPersistenceContext,
    HealthStatus, MCPServiceHealthSnapshot, McpHealthCheckService, ServiceHealth, ServiceHealthMap,
};
pub use hook::{
    BackgroundExecutionRegistry, BackgroundToolRegistry, DefraSessionHook, FailurePolicy, HookStats,
};
pub use identity::{
    load_macos_keychain_identity, load_macos_secure_enclave_identity,
    load_or_create_macos_keychain_identity, load_or_create_macos_secure_enclave_identity,
    AgentIdentity, AgentPrincipal, KeyIdentity, RegisteredIdentity, ServiceAccount,
};
pub use interrupt::{fetch_interrupt_requested_at, interrupt_request};
pub use lifecycle::{
    task_run_conversation_title, write_manual_agent_request,
    write_manual_agent_request_with_conversation_title, RecoveryReport, RequestLifecycle,
};
pub use mcp_pool::McpPool;
pub use meta_tools::build_meta_tools;
pub use native_executor_status::{active_native_executors, NativeExecutorStatus};
pub use oneshot::{run_openai_oneshot, run_openai_oneshot_with_tools, OneshotRunResult};
pub use prompt::{LayeredPromptBuilder, PromptBuilder};
pub use runtime_snapshot::{
    ActiveRuntimeSnapshot, ConcurrencyMode, DispatcherMap, ResolvedEventTrigger, ResolvedSchedule,
    ResolvedTask,
};
pub use schema::{
    ensure_config_bootstrap_schemas, ensure_runtime_schemas, ensure_schemas, AGENT_BEHAVIOR_SCHEMA,
    AGENT_CONVERSATION_SCHEMA, AGENT_MESSAGE_SCHEMA, AGENT_PRINCIPAL_SCHEMA, AGENT_REQUEST_SCHEMA,
    AGENT_RESPONSE_SCHEMA, AGENT_RUNTIME_SCHEMA, AGENT_SESSION_SCHEMA, AGENT_TOOL_CALL_SCHEMA,
    AGENT_TOOL_RESULT_SCHEMA, CODEX_THREAD_PROJECTION_SCHEMA, COMPACTION_ENTRY_SCHEMA,
    INFERENCE_BACKEND_SCHEMA, INFERENCE_CALL_SCHEMA, INFERENCE_PROFILE_SCHEMA, SCHEDULE_SCHEMA,
    TASK_SCHEMA, TOOL_SELECTION_SCHEMA, TOOL_SERVICE_HEALTH_STATE_SCHEMA,
    TOOL_SERVICE_REGISTRY_SCHEMA,
};
pub use session::load_history;
pub use session::{fork, ForkError, ForkOutcome, ForkParams};
pub use streaming::{DefraStreamWriter, StreamWriter};
pub use template::{
    parse_template_for_validation, render_template, TemplateError, TemplateScope, VariableRef,
};
pub use tool_control::{cancel_background_tool_call, CancelBackgroundToolCallOutcome};
pub use tool_surface::{
    cli_tool, BashMode, BehaviorToolConfig, CustomToolFactory, FileToolMode, ToolCeiling,
    ToolRuntimeContext, ToolSelection, ToolSurface,
};
pub use toolset::{
    build_delegate_tool, build_native_tools, CliToolConfig, CommandExecutionMode,
    CommandExecutionPolicy, CommandNetworkMode, NativeTool, ToolSet, ToolSetBuilder,
    DELEGATE_TOOL_NAME,
};
pub use trigger_engine::event_source::EventSource;
pub use trigger_engine::subagent_source::SubagentSource;
pub use trigger_engine::subscription_source::UpdateSubscriptionSource;
pub use trigger_engine::{FireIntent, FireResult, TriggerKind, TriggerSource};
pub use truncation::{DefraSpillTruncator, TruncationLimits, TruncationMode, Truncator};
pub use watcher::{AgentRequest, DefraWatcher, Watcher};

/// Test-internal surface for driving production helpers directly from
/// integration tests.
///
/// `assemble_principal_and_behaviors` is `pub(crate)` in production.
/// Exposing it here (under `#[doc(hidden)]`) lets the loader-dedup
/// proptest (`tests/identity_conformance_proptest.rs`) call the same
/// helper that both production snapshot paths funnel through, without
/// widening the public API.
///
/// `handle_list_subagents` and its arg/response types are exposed here so
/// the `subagent_enablement_e2e` integration test can call the handler
/// directly and assert C2 state (running-subagent listing) end-to-end.
#[doc(hidden)]
pub mod __test_internals {
    pub use crate::agent::principal_assembly::{
        assemble_principal_and_behaviors, BehaviorBuildError,
    };
    pub use crate::background_tools::handle_list_subagents;
    pub use crate::background_tools::r4c_args::{
        ListSubagentsArgs, ListSubagentsEntry, ListSubagentsResponse,
    };
    pub use crate::trigger_engine::run_subagent_source_for_test;
}

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
