//! Core DefraDB-backed agent runtime for Gents.
//!
//! This crate owns request execution, lifecycle enforcement, persistence,
//! identity, tools, networking, and provider-input assembly.

#[cfg(test)]
extern crate self as gents;

// The same fixture owners back external and crate-private conformance tests.
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
pub(crate) mod support;

pub mod adapter_projection;
pub(crate) mod admission;
pub mod agent;
pub mod backend_health;
pub mod backend_provider;
pub mod backend_registry;
pub mod background_completion;
mod background_completion_diagnostics;
pub(crate) mod background_tools;
mod behavior_readiness_publisher;
pub(crate) mod callback;
pub mod chatgpt_codex;
pub mod chatgpt_oauth_refresh;
pub mod claude_messages;
pub mod claude_oauth;
pub mod claude_oauth_refresh;
pub mod claude_subscription;
pub mod codex_shim_binding;
pub mod collection;
pub mod compaction;
pub(crate) mod completion_factory;
pub mod config;
pub mod config_client;
pub mod configuration_discovery;
pub mod defra_query;
pub mod defra_write;
pub mod document_config;
pub mod error;
pub mod eth;
pub mod eval;
pub mod event_delivery_contract;
pub mod external_adapter_capture;
pub mod file_lock;
pub mod goal;
pub mod graph_package;
pub mod graph_pipeline;
pub mod graphql;
pub mod health_checker;
pub mod home;
pub mod hook;
pub mod identity;
pub mod inference_http;
pub mod inference_setup;
pub mod interrupt;
#[cfg(test)]
pub(crate) mod lean_vocab_test;
pub mod native_logging;
pub mod oauth_credential;
pub(crate) mod oauth_http;
pub mod openai_wire;
pub mod p2p_observability;
pub mod pack;
pub mod pack_archive;
pub mod pack_registry;
pub mod pack_resolve;
pub mod pack_store;
pub mod plugin;
pub mod provider_http;
pub(crate) mod provider_input;
pub use gents_loop::provider_input::ProviderInputProfile;
/// Exact provider context-window budget policy shared by compaction,
/// diagnostics, and the final dispatch gate.
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
pub mod storage_backend;
pub mod store_key;
pub mod xai_grok_oauth;
pub mod xai_oauth_login;
pub mod xai_oauth_refresh;

/// Shared in-crate test utilities.
#[cfg(test)]
pub(crate) mod test_support {
    /// Event targets a scoped subscriber in this crate's tests reads back.
    ///
    /// A target left out of this list stays subject to the interest cache
    /// described on [`enable_scoped_event_capture`], so a test that captures a
    /// new target must add it here. Enabling every callsite instead would leave
    /// the whole suite dispatching the runtime's per-operation logging, which
    /// costs it an order of magnitude.
    const CAPTURED_EVENT_TARGETS: &[&str] = &[
        crate::config_client::write_telemetry::WRITE_ATTEMPT_EVENT_TARGET,
        crate::runtime_status::RECONCILE_PHASE_EVENT_TARGET,
    ];

    /// Keeps the captured targets enabled so a scoped subscriber observes them.
    ///
    /// `tracing` caches one interest per callsite for the whole process,
    /// computed from the default subscriber of whichever thread reaches that
    /// callsite first. Tests share the process and run concurrently, so a
    /// callsite first reached by a thread with no subscriber is cached as
    /// disabled, and a scoped subscriber installed before that first reach then
    /// receives nothing. A global default that admits the captured targets
    /// keeps their callsites enabled; each scoped subscriber still sees only the
    /// events raised while it is the default, and this one discards the rest.
    /// Call this before installing a scoped subscriber a test reads from.
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

    /// Scripted providers have no HTTP transport. Persist their actual request
    /// through the capture owner before returning synthetic provider output,
    /// rather than bypassing the owned loop's armed-capture requirement.
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
        let inner = crate::rendered_request::transport::CountingInner::default();
        let transport = crate::rendered_request::transport::RenderedRequestCapturingHttpClient::new(
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

    /// Sets a process environment variable and restores its previous value on
    /// drop, including when the test panics.
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

    /// Install an explicit, inert inference/context/tools chain for a named test behavior.
    /// Schemas must already be registered. The principal's default is never changed.
    pub(crate) async fn install_test_behavior(
        node: &defra_node::EmbeddedNode,
        owner: &str,
        behavior_id: &str,
    ) {
        use crate::config_client::{
            ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        use crate::Collection;
        use serde_json::json;
        crate::ensure_agent_principal(node, owner).await.unwrap();
        let context = format!("{behavior_id}:context");
        let tools = format!("{behavior_id}:tools");
        let profile = format!("{behavior_id}:inference");
        let backend = format!("{behavior_id}:backend");
        let documents = [
            (Collection::AgentBehavior, json!({"agent_did":owner,"behavior_id":behavior_id,"context_id":context,"inference_profile_id":profile})),
            (Collection::AgentContext, json!({"agent_did":owner,"context_id":context,"tools_id":tools})),
            (Collection::Tools, json!({"agent_did":owner,"tools_id":tools})),
            (Collection::InferenceProfile, json!({"agent_did":owner,"profile_id":profile,"backend_id":backend,"model_name":"test-model"})),
            (Collection::InferenceBackend, json!({"agent_did":owner,"backend_id":backend,"name":"Test inference","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
        ].into_iter().map(|(collection,value)| DesiredStateApplyDocument {collection,add:value.clone(),update:value}).collect();
        let plan = DesiredStateApplyPlan::new(documents).unwrap();
        ConfigAccess::transact_local(node, None, "test.install_behavior", |txn| {
            let plan = &plan;
            Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
        crate::backend_registry::set_backend_probe_status(node, owner, &backend, "healthy")
            .await
            .unwrap();
    }

    /// Copies the declared files of the fixture pack `tests/fixtures/packs/<name>`
    /// into a temp directory. A plugin's `.afb` is not checked in, so each
    /// declared artifact is written as a module that ignores stdin and
    /// prints `plugin_output`; every other file is copied as is.
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

    /// A gents home whose pack store holds the fixture pack
    /// `tests/fixtures/packs/<name>` (indexed by name, as any import is), with
    /// a plugin executor over it: what a runtime that resolves
    /// `fixture/<name>` without a network call looks like.
    pub(crate) fn home_with_fixture_pack(
        name: &str,
    ) -> (
        tempfile::TempDir,
        std::sync::Arc<crate::plugin::executor::PluginExecutor>,
    ) {
        let (_guard, dir) = fixture_pack_copy(name, &serde_json::json!({}));
        home_with_pack_dir(&dir)
    }

    /// [`home_with_fixture_pack`] for a pack directory a test has edited.
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

    /// Reads a graph pack fixture from `tests/fixtures/packs/<name>` through
    /// the same archive path an install takes: `pack_dir` packs the
    /// directory, `PackArchive::from_bytes` reads it back, and the graph
    /// loader loads it.
    pub(crate) fn load_test_graph_package(
        name: &str,
        options: &crate::graph_package::GraphPackageInstallBindings,
    ) -> crate::graph_package::LoadedGraphPackage {
        load_test_graph_package_with_plugin_output(name, options, &serde_json::json!({}))
    }

    /// [`load_test_graph_package`] for a fixture whose plugin runs: its
    /// artifact prints `plugin_output` (see [`fixture_pack_copy`]).
    pub(crate) fn load_test_graph_package_with_plugin_output(
        name: &str,
        options: &crate::graph_package::GraphPackageInstallBindings,
        plugin_output: &serde_json::Value,
    ) -> crate::graph_package::LoadedGraphPackage {
        let scope = crate::pack::PackInstallOptions {
            agent_did: options.agent_did.clone(),
        };
        let (_guard, dir) = fixture_pack_copy(name, plugin_output);
        let (bytes, _) = crate::pack_archive::pack_dir(&dir)
            .unwrap_or_else(|error| panic!("packing fixture {name:?}: {error:#}"));
        let archive = crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
        crate::graph_package::load_archive_graph_package_with_environment(
            &archive,
            &scope,
            &|name| (name == "GENTS_REVIEW_MODEL").then(|| "test-model".to_owned()),
        )
        .unwrap()
    }

    pub(crate) async fn install_test_graph_package(
        access: &crate::ConfigAccess,
        actor: &str,
        name: &str,
        options: &crate::graph_package::GraphPackageInstallBindings,
    ) -> anyhow::Result<crate::graph_package::GraphPackageInstallReceipt> {
        install_test_graph_package_explicit(access, actor, name, options, true).await
    }

    /// [`install_test_graph_package`] with control over whether the record
    /// this writes reads as an explicit install; a dependency-install test
    /// wants `false`.
    pub(crate) async fn install_test_graph_package_explicit(
        access: &crate::ConfigAccess,
        actor: &str,
        name: &str,
        options: &crate::graph_package::GraphPackageInstallBindings,
        explicit: bool,
    ) -> anyhow::Result<crate::graph_package::GraphPackageInstallReceipt> {
        let package = load_test_graph_package(name, options);
        crate::graph_package::install_loaded_graph_package(
            access,
            actor,
            &package,
            options,
            None,
            &crate::graph_package::GraphInstallRecord {
                plugins: Vec::new(),
                explicit,
            },
        )
        .await
    }

    /// `OneOrMany::first_ref` stand-in for native `Vec` content: non-empty by
    /// convention in every shape the tests build.
    pub(crate) fn first_content<T>(items: &[T]) -> &T {
        items.first().expect("non-empty content")
    }

    /// The #589 production poison, byte-faithful to Amy's persisted
    /// `AgentToolCall` row `Rrt-HmhWfFSmkh1HSUmHt`: a model tool-call
    /// `arguments` string contaminated by out-of-channel tokens — a stray CJK
    /// `房` and a leaked `</think` reasoning boundary inside a key, a nested
    /// Hermes `<tool_call>`/`<function=...>` fragment as its value, duplicated
    /// keys, and LITERAL newlines inside the strings (the control characters
    /// `serde_json` rejects at "line 2 column 0"). The intended call survives
    /// as the final `tool_name: list_hosts`.
    pub(crate) const CORRUPT_TOOL_ARGS_589: &str = "{\"raw_schema\": false, \
         \"service_id\": \"observability-mcp\", \"tool房\n</think\": \"\n<tool_call>\n\
         <function=describe_tool>\", \"raw_schema\": false, \
         \"service_id\": \"observability-mcp\", \"tool_name\": \"list_hosts\"}";
}
pub mod lifecycle;
pub mod llm;
pub mod log_rate;
pub mod mailbox;
pub(crate) mod managed_exec;
pub mod mcp_pool;
pub mod meta_tools;
pub mod migration;
pub mod native_executor_status;
pub mod oneshot;
pub mod optimization;
pub mod periodic_recovery;
pub mod prompt;
pub mod provider_context_reduction;
pub(crate) mod registry;
pub mod rendered_request;
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
pub(crate) mod runtime_snapshot;
pub(crate) mod runtime_status;
pub(crate) mod runtime_trace;
pub mod schedule_cron;
pub mod schema;
pub mod schema_tool;
pub mod self_config;
pub mod session;
pub mod session_message;
pub mod session_origin;
pub mod skills;
pub mod streaming;
pub(crate) mod task_hooks;
pub mod template;
pub mod tool_call_lifecycle;
pub mod tool_control;
pub mod tool_surface;
pub mod toolset;
pub mod trace_export;
pub(crate) mod trigger_engine;
pub mod truncation;
pub mod watcher;
pub mod workspace;

pub use background_tools::load_caused_request_terminal;
pub use callback::reject_secret_bearing_callback_fields;
pub use collection::{Collection, DESIRED_STATE_APPLY_ORDER};
pub use eth::{
    address_from_secret, attestation_payload, binding_storage_key, encode_attestation,
    generate_secp256k1_secret, method_permitted, validate_eth_call_declarations,
    validate_query_methods, ChainKeyMaterialStore, HttpEthRpc, KeyringChainKeyStore,
    BUILTIN_QUERY_METHODS, ETH_USER_AGENT, KEYRING_SERVICE, KEY_BACKEND_KEYRING,
};

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
pub use admission::call_state_holds_backend_slot;
pub use admission::BackendAdmissionConfig;
pub use admission::{document_configured_from_fields, InferenceCall, InferenceCallRecoveryReport};
pub use agent::{
    BehaviorBuilder, DocumentRuntimeOptions, Gents, GentsBuilder, ProcessLifecycleObserver,
    ProcessLifecycleState, RuntimeSnapshotObserver,
};
pub use backend_health::{
    probe_backends_cycle, run_backend_probe_cycle, spawn_backend_prober, BackendHealthMap,
    BackendHealthSnapshot, BackendHealthState, BackendProberOptions, ProbeCycleOutcome,
};
pub use backend_provider::{discover_models as discover_backend_models, BackendProviderKind};
pub use backend_registry::{
    record_discovered_catalog_on, record_model_catalog_in_txn, InferenceBackend,
    HEALTHY_PROBE_STATUS, UNKNOWN_PROBE_STATUS,
};
pub use background_completion_diagnostics::{
    load_background_completion_diagnostics, BackgroundCompletionDiagnostics,
    BackgroundCompletionEpochDiagnostic,
};
pub use compaction::CompactionStrategy;
pub use config::{
    ReasoningEffort, ResolvedBehavior, SamplingConfig, DEFAULT_COMPACTION_THRESHOLD,
    DEFAULT_CONTEXT_WINDOW, DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS,
    DEFAULT_MAX_TURNS, DEFAULT_MODEL_NAME, DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
    DEFAULT_STREAM_BATCH_MS, DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
};
pub use config_client::ConfigAccess;
pub use defra_node;
#[cfg(test)]
pub(crate) use document_config::upsert_agent_principal;
pub use document_config::{
    chain_key_binding_by_id_query, create_chain_key_binding_mutation,
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    delete_chain_key_binding_mutation, deserialize_dual_shape, ensure_agent_principal,
    eth_tool_by_id_query, is_reserved_builtin_tool_name, list_agent_behaviors,
    list_chain_key_bindings_query, list_datastore_tool_surfaces, list_eth_tools,
    list_inference_profile_records, load_agent_behavior, load_agent_principal,
    load_inference_profile, merge_datastore_tool_surfaces, upsert_agent_behavior,
    upsert_chain_key_binding, upsert_chain_key_binding_mutation, upsert_inference_profile,
    AgentBehavior as AgentBehaviorDocument, ChainKeyBindingDocument, ConfigReferences,
    DatastoreToolSurfaceDocument, EthToolDocument, InferenceProfile, MergedSurfaceTools,
    QueryToolDecl, SubagentTargetDocument, SurfaceToolDecl, Tools, WriteToolDecl, WriteToolField,
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
pub use identity::{
    load_macos_keychain_identity, load_macos_secure_enclave_identity,
    load_or_create_macos_keychain_identity, load_or_create_macos_secure_enclave_identity,
    AgentIdentity, KeyIdentity, RegisteredIdentity, RuntimePrincipal, ServiceAccount,
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
    JsonP2pSyncStatusAdapter, P2pPeerBacklogSnapshot, P2pPushBacklogSnapshot,
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
#[cfg(feature = "agent-memory")]
pub use schema::AGENT_MEMORY_SCHEMA;
pub use schema::{
    ensure_runtime_schemas, AGENT_BEHAVIOR_SCHEMA, AGENT_MESSAGE_SCHEMA,
    AGENT_OUTPUT_SEGMENT_SCHEMA, AGENT_PRINCIPAL_SCHEMA, AGENT_REQUEST_SCHEMA,
    AGENT_RUNTIME_SCHEMA, AGENT_SESSION_SCHEMA, AGENT_TOOL_CALL_SCHEMA, COMPACTION_ENTRY_SCHEMA,
    GOAL_SCHEMA, INFERENCE_BACKEND_SCHEMA, INFERENCE_CALL_SCHEMA, INFERENCE_PROFILE_SCHEMA,
    MAILBOX_ITEM_SCHEMA, OAUTH_CREDENTIAL_SCHEMA, SCHEDULE_SCHEMA, TASK_SCHEMA, TOOLS_SCHEMA,
    TOOL_SERVICE_HEALTH_STATE_SCHEMA, TOOL_SERVICE_REGISTRY_SCHEMA,
};
pub use session::load_history;
pub use session::{fork, fork_via_http, ForkError, ForkOutcome, ForkParams};
pub use streaming::{DefraStreamWriter, StreamWriter, MAX_LIVE_REASONING_BYTES};
pub use template::{
    check_template_vocabulary, parse_template_for_validation, render_template, TemplateError,
    TemplateScope, VariableRef,
};
pub use tool_control::{cancel_background_tool_call, CancelBackgroundToolCallOutcome};
pub use tool_surface::{
    cli_tool, BashMode, BehaviorToolConfig, CustomToolFactory, FileToolMode, ResolvedToolSelection,
    ToolCeiling, ToolPolicyVersion, ToolRuntimeContext, ToolSurface, TOOL_POLICY_V1,
};
pub use toolset::{
    build_native_tools, enable_self_runner, CliToolConfig, CommandExecutionMode,
    CommandExecutionPolicy, CommandNetworkMode, FileToolLimits, NativeTool, ToolSet,
    ToolSetBuilder,
};
pub use trigger_engine::event_source::EventSource;
pub use trigger_engine::goal_source::GoalSource;
pub use trigger_engine::subscription_source::UpdateSubscriptionSource;
pub use trigger_engine::{FireIntent, FireResult, TriggerKind, TriggerSource};
pub use truncation::{TruncationLimits, TruncationMode};
pub use watcher::{AgentRequest, DefraWatcher, Watcher};

#[doc(hidden)]
pub mod __test_internals {
    pub use crate::agent::principal_assembly::BehaviorBuildError;
    pub use crate::lifecycle::activate_workspace_bound_request;
    pub use crate::lifecycle::materialize::EnqueuedAgentRequest;
    pub use crate::lifecycle::queue::{reconcile_coalesced_pending_request, QueueSource};
}

#[cfg(test)]
mod public_api_tests {
    use super::*;

    #[test]
    fn downstream_oneshot_analysis_surface_is_available_from_crate_root() {
        let _strategy = CompactionStrategy::StripThenSummarize;
        let _ensure = ensure_runtime_schemas;
        let _history = load_history;
        let _oneshot = run_openai_oneshot_with_tools;
    }
}
