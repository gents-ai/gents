#[cfg(any(test, feature = "test-fixtures"))]
extern crate self as gents;

#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
#[path = "../tests/support/mod.rs"]
pub mod support;

pub mod claude_gateway_login;
mod claude_request_context;
mod collection_use;
pub mod documents;
pub mod error;
pub mod eval;
pub mod file_inventory;
pub mod goal;
pub mod inference;
pub mod observe;
pub mod openai_wire;
pub mod operations;
pub mod pack;
pub mod plugin;
pub use gents_loop::provider_input::ProviderInputProfile;
pub mod provider_budget {
    pub use crate::turn::provider_input::budget::{effective_input_budget, threshold_budget};
}
pub(crate) mod provider_usage;
pub mod runtime;
pub mod store;

pub mod request;
#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
#[allow(dead_code)]
pub mod test_support;
pub use gents_protocol::request_admission::SIGNED_REQUEST_FIELDS;
#[doc(hidden)]
pub use request::admission::final_claim_admission_disposition;
pub use request::admission::{
    sign_agent_request_create, sign_agent_request_create_as_registered_target,
};
pub mod exec;
pub mod file_contents;
pub mod session;
pub mod tools;
pub mod triggers;
pub mod truncation;
pub mod turn;
pub mod workspace;

pub use defra_node;
pub use documents::automation::{
    EventTriggerFireMode, ResolvedEventTrigger, ResolvedSchedule, ResolvedTask,
    MAX_EVENT_TRIGGER_GROUP_DOCS,
};
pub use documents::callback_secret_fields::reject_secret_bearing_callback_fields;
pub use documents::inference_defaults::{
    ReasoningEffort, DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
};
pub use documents::task_delivery::TriggerKind;
pub use documents::template::{
    check_template_vocabulary, parse_template_for_validation, render_manual_template,
    render_template, TemplateError, TemplateScope, VariableRef,
};
pub use documents::{
    default_inference_profile_id_for_agent, default_inference_profile_id_for_node,
    deserialize_dual_shape, ensure_node, is_reserved_builtin_tool_name, list_agents,
    list_datastore_tool_surfaces, list_inference_profile_records, load_agent,
    load_inference_profile, load_node, merge_datastore_tool_surfaces, Agent as AgentDocument,
    AgentTargetDocument, ConfigReferences, DatastoreToolSurfaceDocument, InferenceProfile,
    MergedSurfaceTools, QueryToolDecl, SurfaceToolDecl, Tools, WriteToolDecl, WriteToolField,
    WriteToolFieldFill, WriteToolOutputObligation, WriteToolOutputObligationScope,
};
pub use documents::{BashMode, FileToolMode};
pub use documents::{ConcurrencyMode, ScheduleCadence};
pub use gents_protocol::client_protocol;
pub use gents_protocol::request_lifecycle::{
    next_request_hop, request_hop_within_bound, EnqueuedAgentRequest, RequestHopCapObservation,
    RequestHopCause,
};
pub use gents_protocol::row::AgentRequest;
pub use inference::admission::call_state_holds_backend_slot;
pub use inference::admission::BackendAdmissionConfig;
pub use inference::admission::{
    document_configured_from_fields, InferenceCall, InferenceCallRecoveryReport,
};
pub use inference::backends::health::{
    probe_backends_cycle, run_backend_probe_cycle, spawn_backend_prober, BackendHealthMap,
    BackendHealthSnapshot, BackendHealthState, BackendProberOptions, ProbeCycleOutcome,
};
pub use inference::backends::provider::{
    discover_models as discover_backend_models, BackendProviderKind,
};
pub use inference::backends::registry::{
    record_discovered_catalog_on, record_model_catalog_in_txn, InferenceBackend,
    HEALTHY_PROBE_STATUS,
};
pub use inference::compaction::CompactionStrategy;
pub use inference::resolved::{
    ResolvedAgent, SamplingConfig, DEFAULT_COMPACTION_THRESHOLD, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS, DEFAULT_MODEL_NAME,
    DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS, DEFAULT_STREAM_BATCH_MS,
};
pub use observe::adapter_projection::{
    adapter_projection_eval_jsonl_record_schema, adapter_projection_eval_jsonl_records,
    adapter_projection_json_schema, adapter_projection_jsonl_record_schema,
    adapter_projection_jsonl_records, adapter_projection_native_json,
    adapter_projection_native_json_schema, build_adapter_projection,
    validate_adapter_projection_contract, AdapterProjection, AdapterProjectionContractError,
    AdapterProjectionEnvelope, AdapterProjectionEvalJsonlRecord, AdapterProjectionJsonlRecord,
    AtifAgent, AtifFinalMetrics, AtifObservation, AtifObservationResult, AtifStep, AtifStepSource,
    AtifToolCall, AtifTrajectory, ProjectionContext, ProjectionRedactionMode, ATIF_SCHEMA_VERSION,
};
pub use observe::external_adapter_capture::{
    import_external_adapter_capture_to_derived_view, ExternalAdapterCapture, ExternalAdapterImport,
    ExternalAdapterMapping, ExternalAdapterSource,
};
pub use observe::run_timeline::{
    build_run_timeline, RetrySummary, RunTimeline, RunTimelineEvent, RunTimelineRows,
    TimelineGoalParentState, TimelineGoalState, TimelineGoalTransitionEvent,
    TimelineGoalVersionRow, TimelineInferenceCallRow, TimelineMessageRow, TimelineRequestRow,
    TimelineSessionRow, TimelineToolCallRow,
};
pub use openai_wire::OpenAiWireApi;
pub use request::interrupt::{
    fetch_interrupt_requested_at, fetch_interrupt_requested_at_by_doc_id, interrupt_request,
    interrupt_request_by_doc_id,
};
pub use request::lifecycle::{
    background_wake_next_retry_at, background_wake_retry_delay,
    build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title,
    build_signed_request, enqueue_local_steering_request, task_session_title,
    write_manual_agent_request, write_manual_agent_request_with_conversation_title,
    BackgroundWakeRedriveReport, ParentLink, RecoveryReport, RequestIdentity, RequestLifecycle,
    RequestSigner, RequestSpec, RetryLink, TerminalRepairReport,
};
pub use request::streaming::{DefraStreamWriter, StreamWriter, MAX_LIVE_REASONING_BYTES};
pub use request::watcher::{DefraWatcher, Watcher};
pub use runtime::agent::{
    AgentBuilder, DocumentRuntimeOptions, Gents, GentsBuilder, ProcessLifecycleObserver,
    ProcessLifecycleState,
};
pub use runtime::oneshot::{
    run_openai_oneshot_admitted, run_openai_oneshot_with_tools_admitted, OneshotRunResult,
};
pub use runtime::p2p::observability::{
    JsonP2pSyncStatusAdapter, P2pInboundClassSnapshot, P2pPeerBacklogSnapshot,
    P2pPeerFetchSnapshot, P2pPeerSummarySnapshot, P2pPendingPeerOccupancySnapshot,
    P2pPushBacklogSnapshot, P2pPushRetryMarkerSnapshot, P2pRequestDispatchSnapshot,
    P2pSyncStatusAdapter, P2pSyncStatusSnapshot,
};
pub use tools::call::periodic_recovery::{
    periodic_recovery_sweep_metadata, run_periodic_recovery_sweeps_with_identities,
    PeriodicRecoverySweepMetadata, PeriodicRecoverySweepOutcome, PeriodicRecoverySweepRun,
};
pub use session::load_history;
pub use session::{ForkError, ForkOutcome, ForkParams};
pub use store::collection::{Collection, DESIRED_STATE_APPLY_ORDER};
pub use store::config_client::ConfigAccess;
pub use store::identity::request_receipt::verify_request_receipt_signature;
pub use store::identity::{
    load_macos_keychain_identity, load_macos_secure_enclave_identity,
    load_or_create_macos_keychain_identity, load_or_create_macos_secure_enclave_identity,
    KeyIdentity, NodeIdentity, RegisteredIdentity, RuntimeNode, ServiceAccount,
};
pub use store::schema::{
    ensure_runtime_schemas, AGENT_MESSAGE_SCHEMA, AGENT_OUTPUT_SEGMENT_SCHEMA,
    AGENT_REQUEST_SCHEMA, AGENT_SCHEMA, AGENT_SESSION_SCHEMA, AGENT_TOOL_CALL_SCHEMA,
    COMPACTION_ENTRY_SCHEMA, GOAL_SCHEMA, INFERENCE_BACKEND_SCHEMA, INFERENCE_CALL_SCHEMA,
    INFERENCE_PROFILE_SCHEMA, MAILBOX_ITEM_SCHEMA, NODE_RUNTIME_SCHEMA, NODE_SCHEMA,
    OAUTH_CREDENTIAL_SCHEMA, SCHEDULE_SCHEMA, TASK_SCHEMA, TOOLS_SCHEMA,
    TOOL_SERVICE_HEALTH_STATE_SCHEMA, TOOL_SERVICE_REGISTRY_SCHEMA,
};
pub(crate) use tools::background::diagnostics::load_background_completion_diagnostics_for_session;
pub use tools::background::diagnostics::{
    load_background_completion_diagnostics, BackgroundCompletionDiagnostics,
    BackgroundCompletionEpochDiagnostic,
};
pub use tools::background::status::status_projection::OutputEvidence;
pub use tools::call::control::{cancel_background_tool_call, CancelBackgroundToolCallOutcome};
pub use tools::call::hook::{
    BackgroundExecutionRegistry, BackgroundToolRegistry, DefraSessionHook, FailurePolicy, HookStats,
};
pub use exec::status::{active_native_executors, NativeExecutorStatus};
pub use tools::mcp::health::{
    run_health_check_cycle, run_health_check_cycle_for_actor, spawn_health_checker,
    HealthCheckerOptions, HealthPersistenceContext, HealthStatus, MCPServiceHealthSnapshot,
    McpHealthCheckService, ServiceHealth, ServiceHealthMap,
};
pub use tools::mcp::meta::build_meta_tools;
pub use tools::mcp::pool::McpPool;
pub use tools::native::{
    build_native_tools, enable_self_runner, run_plugin_build, CliToolConfig, CommandExecutionMode,
    CommandExecutionPolicy, CommandNetworkMode, FileToolLimits, NativeTool, PluginBuildOutcome,
    ToolSet, ToolSetBuilder, PLUGIN_BUILD_COMMAND,
};
pub use tools::surface::{
    cli_tool, AgentToolSurfaceConfig, CustomToolFactory, ResolvedToolSelection, ToolCeiling,
    ToolPolicyVersion, ToolRuntimeContext, ToolSurface, TOOL_POLICY_V1,
};
pub use triggers::engine::event_source::EventSource;
pub use triggers::engine::goal_source::GoalSource;
pub use triggers::engine::{run_trigger_engine, FireIntent, FireResult, TriggerSource};
pub use triggers::runtime_snapshot::{ActiveRuntimeSnapshot, DispatcherMap};
pub use truncation::{TruncationLimits, TruncationMode};
pub use turn::prompt::{LayeredPromptBuilder, PromptBuilder};

#[doc(hidden)]
pub mod __test_internals {
    pub async fn park_backend(
        lifecycle: &mut crate::request::lifecycle::RequestLifecycle,
    ) -> anyhow::Result<bool> {
        lifecycle.park_for_backend().await
    }

    pub async fn wake_backend(
        lifecycle: &mut crate::request::lifecycle::RequestLifecycle,
    ) -> anyhow::Result<bool> {
        Ok(lifecycle
            .wake_for_backend(Some(chrono::Utc::now()))
            .await?
            .resumed())
    }

    pub fn backend_generation(
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
    ) -> anyhow::Result<&str> {
        lifecycle.execution_generation()
    }

    pub fn backend_deadline(
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        lifecycle.claimed_deadline_at()
    }

    pub async fn settle_parked_backend(
        access: &crate::ConfigAccess,
        request_doc_id: &str,
        generation: &str,
        interrupted: bool,
    ) -> anyhow::Result<crate::request::lifecycle::TerminalizeResult> {
        crate::request::lifecycle::terminalize_parked_request(
            access,
            request_doc_id,
            generation,
            interrupted,
        )
        .await
    }
    pub use crate::request::lifecycle::materialize::activate_workspace_bound_request_with_identity;
    pub use crate::request::lifecycle::queue::{
        reconcile_coalesced_pending_request, reconcile_coalesced_pending_request_for_with_access,
        QueueSource,
    };
    pub use crate::runtime::agent::principal_assembly::AgentBuildError;
    pub use crate::triggers::engine::durable::supersede_trigger_requests_in_txn;
    pub use gents_protocol::request_lifecycle::EnqueuedAgentRequest;
}
