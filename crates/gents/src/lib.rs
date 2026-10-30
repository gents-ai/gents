#[cfg(test)]
extern crate self as gents;

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
pub(crate) mod support;

pub mod adapter_projection;
pub mod agent;
pub mod application_write;
pub mod background_completion;
mod background_completion_diagnostics;
pub mod background_tools;
pub mod blocked_turn;
pub(crate) mod callback;
pub mod codex_shim_binding;
pub(crate) mod completion_factory;
pub mod config_client;
pub mod configuration_discovery;
pub mod defra_query;
pub mod defra_write;
pub mod documents;
pub mod drop_cleanup;
pub mod error;
pub mod eval;
pub mod event_delivery_contract;
pub mod external_adapter_capture;
pub mod failure_text;
pub mod file_inventory;
pub mod file_lock;
pub mod goal;
pub mod graphql;
pub mod health_checker;
pub mod hook;
pub mod inference;
pub mod inference_observation;
pub mod interrupt;
#[cfg(test)]
pub(crate) mod lean_vocab_test;
pub mod native_logging;
mod node_readiness_publisher;
pub mod openai_wire;
pub mod p2p_observability;
pub mod p2p_tool;
pub mod pack;
pub mod pack_archive;
pub mod pack_registry;
pub mod pack_resolve;
pub mod pack_store;
pub mod plugin;
pub mod plugin_job;
pub(crate) mod provider_input;
pub mod ux_data;
pub mod ux_plugin;
pub use gents_loop::provider_input::ProviderInputProfile;
pub mod provider_budget {
    pub use crate::provider_input::budget::{effective_input_budget, threshold_budget};
}
#[cfg(test)]
#[path = "lean_vocab_test/canonical_execution/native_adapter.rs"]
mod canonical_execution_native_adapter;
pub(crate) mod provider_usage;
pub mod starter_recipes;
pub mod startup_readiness;
pub mod startup_recovery;
pub mod store;

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) mod read_observation;

    /// A test's `#[tokio::test]` runtime is its entry point; constructors take
    /// that runtime's handle from here.
    #[allow(
        clippy::disallowed_methods,
        reason = "a test's #[tokio::test] runtime is its entry-point runtime"
    )]
    pub(crate) fn runtime_handle() -> tokio::runtime::Handle {
        tokio::runtime::Handle::current()
    }

    /// Fixtures that reopen a store must retain its original signing identity.
    pub(crate) fn node_builder() -> defra_node::NodeBuilder {
        use crate::NodeIdentity as _;

        let directory = tempfile::tempdir().unwrap();
        let identity =
            crate::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
        defra_node::EmbeddedNode::builder().with_node_identity_did(identity.did())
    }

    const CAPTURED_EVENT_TARGETS: &[&str] = &[
        crate::config_client::write_telemetry::WRITE_ATTEMPT_EVENT_TARGET,
        crate::runtime_status::RECONCILE_PHASE_EVENT_TARGET,
        crate::session::REFUSAL_PROJECTION_EVENT_TARGET,
    ];

    pub(crate) fn enable_scoped_event_capture() {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            tracing::subscriber::set_global_default(CapturedTargetSubscriber)
                .expect("no other global tracing default in this test binary");
        });
    }

    struct CapturedTargetSubscriber;

    impl tracing::Subscriber for CapturedTargetSubscriber {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            CAPTURED_EVENT_TARGETS.contains(&metadata.target())
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::Id {
            tracing::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}

        fn event(&self, _event: &tracing::Event<'_>) {}

        fn enter(&self, _span: &tracing::Id) {}

        fn exit(&self, _span: &tracing::Id) {}
    }

    pub(crate) async fn capture_scripted_provider_request(
        request: &rig::completion::CompletionRequest,
        model: &str,
    ) -> Result<(), rig::completion::CompletionError> {
        use rig::http_client::HttpClientExt;
        let body = rig::providers::openai::completion::CompletionRequest::try_from((
            model.to_string(),
            request.clone(),
        ))
        .map_err(|error| rig::completion::CompletionError::ProviderError(error.to_string()))?;
        let mut body = serde_json::to_value(body)
            .map_err(|error| rig::completion::CompletionError::ProviderError(error.to_string()))?;
        body["stream"] = serde_json::Value::Bool(true);
        body["stream_options"] = serde_json::json!({ "include_usage": true });
        let inner = crate::inference::rendered_request::transport::CountingInner::default();
        let transport =
            crate::inference::rendered_request::transport::RenderedRequestCapturingHttpClient::new(
                inner.clone(),
            );
        let outbound = rig::http_client::Request::builder()
            .method("POST")
            .uri("https://scripted-provider.invalid/v1/chat/completions")
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(serde_json::to_vec(&body).map_err(
                |error| rig::completion::CompletionError::ProviderError(error.to_string()),
            )?))
            .map_err(|error| rig::completion::CompletionError::ProviderError(error.to_string()))?;
        transport
            .send_streaming(outbound)
            .await
            .map_err(|error| rig::completion::CompletionError::ProviderError(error.to_string()))?;
        assert_eq!(
            inner.send_count(),
            1,
            "capture must authorize scripted provider send"
        );
        Ok(())
    }

    pub(crate) struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        pub(crate) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => std::env::set_var(self.key, previous),
                None => std::env::remove_var(self.key),
            }
        }
    }

    pub(crate) async fn install_test_agent(
        node: &std::sync::Arc<defra_node::EmbeddedNode>,
        owner: &str,
        agent_id: &str,
    ) {
        use crate::config_client::{
            ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        use crate::Collection;
        use serde_json::json;
        crate::ensure_node(node, owner).await.unwrap();
        let context = format!("{agent_id}:context");
        let tools = format!("{agent_id}:tools");
        let profile = format!("{agent_id}:inference");
        let backend = format!("{agent_id}:backend");
        let documents = [
            (Collection::Agent, json!({"node_did":owner,"agent_id":agent_id,"context_id":context,"inference_profile_id":profile})),
            (Collection::AgentContext, json!({"node_did":owner,"context_id":context,"tools_id":tools})),
            (Collection::Tools, json!({"node_did":owner,"tools_id":tools})),
            (Collection::InferenceProfile, json!({"node_did":owner,"profile_id":profile,"backend_id":backend,"model_name":"test-model"})),
            (Collection::InferenceBackend, json!({"node_did":owner,"backend_id":backend,"name":"Test inference","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
        ].into_iter().map(|(collection,value)| DesiredStateApplyDocument {collection,add:value.clone(),update:value}).collect();
        let plan = DesiredStateApplyPlan::new(documents).unwrap();
        ConfigAccess::transact_local_write(node, None, "test.install_agent", |txn| {
            let plan = &plan;
            Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
        crate::inference::backends::registry::set_backend_probe_status(
            node, owner, &backend, "healthy",
        )
        .await
        .unwrap();
    }

    pub(crate) fn fixture_pack_copy(
        name: &str,
        plugin_output: &serde_json::Value,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/packs")
            .join(name);
        let manifest: crate::pack::PackManifest = serde_json::from_slice(
            &std::fs::read(source.join("manifest.json"))
                .unwrap_or_else(|error| panic!("fixture {name:?} manifest: {error}")),
        )
        .unwrap_or_else(|error| panic!("fixture {name:?} manifest: {error}"));
        let artifacts: std::collections::BTreeSet<&str> = manifest
            .metadata
            .plugins
            .iter()
            .map(|plugin| plugin.artifact.as_str())
            .collect();
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(name);
        for path in crate::pack::declared_paths(&manifest) {
            let target = root.join(&path);
            std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
            if artifacts.contains(path.as_str()) {
                let wat = crate::plugin::tests::constant_output_wat(
                    &serde_json::to_vec(plugin_output).expect("encode plugin output"),
                );
                std::fs::write(&target, crate::plugin::tests::build_plugin_afb(&wat))
                    .expect("write artifact");
            } else {
                std::fs::copy(source.join(&path), &target)
                    .unwrap_or_else(|error| panic!("fixture {name:?} file {path:?}: {error}"));
            }
        }
        (dir, root)
    }

    pub(crate) fn home_with_fixture_pack(
        name: &str,
    ) -> (
        tempfile::TempDir,
        std::sync::Arc<crate::plugin::executor::PluginExecutor>,
    ) {
        let (_guard, dir) = fixture_pack_copy(name, &serde_json::json!({}));
        home_with_pack_dir(&dir)
    }

    pub(crate) fn home_with_pack_dir(
        dir: &std::path::Path,
    ) -> (
        tempfile::TempDir,
        std::sync::Arc<crate::plugin::executor::PluginExecutor>,
    ) {
        let (bytes, _) = crate::pack_archive::pack_dir(dir)
            .unwrap_or_else(|error| panic!("packing {}: {error:#}", dir.display()));
        let home = tempfile::tempdir().expect("home");
        crate::pack_store::PackStore::new(home.path())
            .import(&bytes[..], None)
            .unwrap_or_else(|error| panic!("storing {}: {error:#}", dir.display()));
        let plugins = std::sync::Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.path().to_path_buf(),
        )));
        (home, plugins)
    }

    pub(crate) fn first_content<T>(items: &[T]) -> &T {
        items.first().expect("non-empty content")
    }

    pub(crate) const CORRUPT_TOOL_ARGS_589: &str = "{\"raw_schema\": false, \
         \"service_id\": \"observability-mcp\", \"tool房\n</think\": \"\n<tool_call>\n\
         <function=describe_tool>\", \"raw_schema\": false, \
         \"service_id\": \"observability-mcp\", \"tool_name\": \"list_hosts\"}";
}
pub mod lifecycle;
pub mod log_rate;
pub mod mailbox;
pub(crate) mod managed_exec;
pub mod mcp_pool;
pub mod meta_tools;
pub mod native_executor_status;
pub mod oneshot;
pub mod optimization;
pub mod periodic_recovery;
pub mod prompt;
pub mod provider_context_reduction;
pub mod registry;
mod request_admission;
#[doc(hidden)]
pub use request_admission::final_claim_admission_disposition;
pub use request_admission::{
    sign_agent_request_create, sign_agent_request_create_as_registered_target,
    verify_request_receipt_signature, SIGNED_REQUEST_FIELDS,
};
pub(crate) mod request_binding;
pub mod retry;
pub mod run_timeline;
pub mod run_timeline_fetch;
pub mod runtime_service;
pub(crate) mod runtime_snapshot;
pub(crate) mod runtime_status;
pub(crate) mod runtime_trace;
pub mod schema_tool;
pub mod self_config;
pub mod session;
pub mod session_message;
pub mod session_origin;
pub mod streaming;
pub(crate) mod task_hooks;
pub mod tool_call_lifecycle;
pub mod tool_control;
pub mod tool_surface;
pub mod toolset;
pub mod trace_export;
pub(crate) mod trigger_engine;
pub mod truncation;
pub mod watcher;
pub mod workspace;

pub use adapter_projection::{
    adapter_projection_eval_jsonl_record_schema, adapter_projection_eval_jsonl_records,
    adapter_projection_json_schema, adapter_projection_jsonl_record_schema,
    adapter_projection_jsonl_records, adapter_projection_native_json,
    adapter_projection_native_json_schema, build_adapter_projection,
    validate_adapter_projection_contract, AdapterProjection, AdapterProjectionContractError,
    AdapterProjectionEnvelope, AdapterProjectionEvalJsonlRecord, AdapterProjectionJsonlRecord,
    AdapterProjectionKind, AtifAgent, AtifFinalMetrics, AtifObservation, AtifObservationResult,
    AtifStep, AtifStepSource, AtifToolCall, AtifTrajectory, ProjectionContext,
    ProjectionRedactionMode, ATIF_SCHEMA_VERSION,
};
pub use agent::{
    AgentBuilder, DocumentRuntimeOptions, Gents, GentsBuilder, ProcessLifecycleObserver,
    ProcessLifecycleState, RuntimeSnapshotObserver,
};
pub(crate) use background_completion_diagnostics::load_background_completion_diagnostics_for_session;
pub use background_completion_diagnostics::{
    load_background_completion_diagnostics, BackgroundCompletionDiagnostics,
    BackgroundCompletionEpochDiagnostic,
};
pub use background_tools::load_caused_request_terminal;
pub use background_tools::status_projection::OutputEvidence;
pub use callback::reject_secret_bearing_callback_fields;
pub use config_client::ConfigAccess;
pub use defra_node;
pub use documents::template::{
    check_template_vocabulary, parse_template_for_validation, render_template, TemplateError,
    TemplateScope, VariableRef,
};
#[cfg(test)]
pub(crate) use documents::upsert_node;
pub use documents::{
    default_agent_id_for_node, default_inference_profile_id_for_agent,
    default_inference_profile_id_for_node, deserialize_dual_shape, ensure_node,
    is_reserved_builtin_tool_name, list_agents, list_datastore_tool_surfaces,
    list_inference_profile_records, load_agent, load_inference_profile, load_node,
    merge_datastore_tool_surfaces, upsert_agent, upsert_inference_profile, Agent as AgentDocument,
    AgentTargetDocument, ConfigReferences, DatastoreToolSurfaceDocument, InferenceProfile,
    MergedSurfaceTools, QueryToolDecl, SurfaceToolDecl, Tools, WriteToolDecl, WriteToolField,
    WriteToolFieldFill, WriteToolOutputObligation, WriteToolOutputObligationScope,
};
pub use external_adapter_capture::{
    import_external_adapter_capture_to_derived_view, ExternalAdapterCapture, ExternalAdapterImport,
    ExternalAdapterMapping, ExternalAdapterSource,
};
pub use gents_protocol::client_protocol;
pub use health_checker::{
    run_health_check_cycle, spawn_health_checker, HealthCheckerOptions, HealthPersistenceContext,
    HealthStatus, MCPServiceHealthSnapshot, McpHealthCheckService, ServiceHealth, ServiceHealthMap,
};
pub use hook::{
    BackgroundExecutionRegistry, BackgroundToolRegistry, DefraSessionHook, FailurePolicy, HookStats,
};
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
    HEALTHY_PROBE_STATUS, UNKNOWN_PROBE_STATUS,
};
pub use inference::compaction::CompactionStrategy;
pub use inference::resolved::{
    ReasoningEffort, ResolvedAgent, SamplingConfig, DEFAULT_COMPACTION_THRESHOLD,
    DEFAULT_CONTEXT_WINDOW, DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS,
    DEFAULT_MAX_TURNS, DEFAULT_MODEL_NAME, DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
    DEFAULT_STREAM_BATCH_MS, DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
};
pub use interrupt::{
    fetch_interrupt_requested_at, fetch_interrupt_requested_at_by_doc_id, interrupt_request,
    interrupt_request_by_doc_id,
};
pub use lifecycle::{
    background_wake_next_retry_at, background_wake_retry_delay,
    build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title,
    build_signed_request, enqueue_local_steering_request, next_request_hop,
    request_hop_within_bound, task_session_title, write_manual_agent_request,
    write_manual_agent_request_with_conversation_title, BackgroundWakeRedriveReport,
    EnqueuedAgentRequest, ParentLink, RecoveryReport, RequestHopCause, RequestIdentity,
    RequestLifecycle, RequestSigner, RequestSpec, RetryLink, TerminalRedriveReport,
    TerminalRepairReport, TERMINAL_REDRIVE_BATCH_LIMIT, TERMINAL_REDRIVE_CAP,
};
pub use mcp_pool::McpPool;
pub use meta_tools::build_meta_tools;
pub use native_executor_status::{active_native_executors, NativeExecutorStatus};
pub use oneshot::{run_openai_oneshot, run_openai_oneshot_with_tools, OneshotRunResult};
pub use openai_wire::OpenAiWireApi;
pub use p2p_observability::{
    JsonP2pSyncStatusAdapter, P2pInboundClassSnapshot, P2pPeerBacklogSnapshot,
    P2pPeerFetchSnapshot, P2pPeerSummarySnapshot, P2pPushBacklogSnapshot,
    P2pPushRetryMarkerSnapshot, P2pRequestDispatchSnapshot, P2pSyncStatusAdapter,
    P2pSyncStatusSnapshot,
};
pub use periodic_recovery::{
    periodic_recovery_sweep_metadata, run_periodic_recovery_sweeps, PeriodicRecoverySweepMetadata,
    PeriodicRecoverySweepOutcome, PeriodicRecoverySweepRun,
};
pub use prompt::{LayeredPromptBuilder, PromptBuilder};
pub use run_timeline::{
    build_run_timeline, RetrySummary, RunTimeline, RunTimelineEvent, RunTimelineRows,
    TimelineGoalParentState, TimelineGoalState, TimelineGoalTransitionEvent,
    TimelineGoalVersionRow, TimelineInferenceCallRow, TimelineMessageRow, TimelineRequestRow,
    TimelineSessionRow, TimelineToolCallRow,
};
pub use runtime_snapshot::{
    ActiveRuntimeSnapshot, ConcurrencyMode, DispatcherMap, EventTriggerFireMode,
    ResolvedEventTrigger, ResolvedSchedule, ResolvedTask, ScheduleCadence,
    MAX_EVENT_TRIGGER_GROUP_DOCS,
};
pub use session::load_history;
pub use session::{fork, fork_via_http, ForkError, ForkOutcome, ForkParams};
pub use store::collection::{Collection, DESIRED_STATE_APPLY_ORDER};
pub use store::identity::{
    load_macos_keychain_identity, load_macos_secure_enclave_identity,
    load_or_create_macos_keychain_identity, load_or_create_macos_secure_enclave_identity,
    KeyIdentity, NodeIdentity, RegisteredIdentity, RuntimeNode, ServiceAccount,
};
#[cfg(feature = "agent-memory")]
pub use store::schema::NODE_MEMORY_SCHEMA;
pub use store::schema::{
    ensure_runtime_schemas, AGENT_MESSAGE_SCHEMA, AGENT_OUTPUT_SEGMENT_SCHEMA,
    AGENT_REQUEST_SCHEMA, AGENT_SCHEMA, AGENT_SESSION_SCHEMA, AGENT_TOOL_CALL_SCHEMA,
    COMPACTION_ENTRY_SCHEMA, GOAL_SCHEMA, INFERENCE_BACKEND_SCHEMA, INFERENCE_CALL_SCHEMA,
    INFERENCE_PROFILE_SCHEMA, MAILBOX_ITEM_SCHEMA, NODE_RUNTIME_SCHEMA, NODE_SCHEMA,
    OAUTH_CREDENTIAL_SCHEMA, SCHEDULE_SCHEMA, TASK_SCHEMA, TOOLS_SCHEMA,
    TOOL_SERVICE_HEALTH_STATE_SCHEMA, TOOL_SERVICE_REGISTRY_SCHEMA,
};
pub use streaming::{DefraStreamWriter, StreamWriter, MAX_LIVE_REASONING_BYTES};
pub use tool_control::{cancel_background_tool_call, CancelBackgroundToolCallOutcome};
pub use tool_surface::{
    cli_tool, AgentToolSurfaceConfig, BashMode, CustomToolFactory, FileToolMode,
    ResolvedToolSelection, ToolCeiling, ToolPolicyVersion, ToolRuntimeContext, ToolSurface,
    TOOL_POLICY_V1,
};
pub use toolset::{
    build_native_tools, enable_self_runner, CliToolConfig, CommandExecutionMode,
    CommandExecutionPolicy, CommandNetworkMode, FileToolLimits, NativeTool, ToolSet,
    ToolSetBuilder,
};
pub use trigger_engine::event_source::EventSource;
pub use trigger_engine::goal_source::GoalSource;
pub use trigger_engine::subscription_source::{
    collection_ids, subscribe_collections, UpdateSubscriptionSource,
};
pub use trigger_engine::{FireIntent, FireResult, TriggerKind, TriggerSource};
pub use truncation::{TruncationLimits, TruncationMode};
pub use watcher::{AgentRequest, DefraWatcher, Watcher};

#[doc(hidden)]
pub mod __test_internals {
    pub use crate::agent::principal_assembly::AgentBuildError;
    pub use crate::config_client::TXN_TRACING_TARGET;
    pub use crate::lifecycle::activate_workspace_bound_request;
    pub use crate::lifecycle::materialize::supersede_trigger_requests_in_txn;
    pub use crate::lifecycle::materialize::EnqueuedAgentRequest;
    pub use crate::lifecycle::queue::{reconcile_coalesced_pending_request, QueueSource};
}

pub(crate) mod tool_output;
