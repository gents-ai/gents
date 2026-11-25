mod durable_contract;

use rapidhash::{HashMapExt, RapidHashMap};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use tokio::sync::{mpsc, watch, Notify};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::test_support::doubles::SpyMaterializer;
use crate::triggers::engine::event_source::EventSource;
use crate::triggers::engine::production_materializer::{
    execution_origin_for_trigger_kind, ProductionMaterializer,
};
use crate::triggers::engine::schedule_source::ScheduleSource;
use crate::triggers::runtime_snapshot::{ActiveRuntimeSnapshot, ResolvedRuntimeSnapshot};
use gents_documents::automation::{ResolvedEventTrigger, ResolvedSchedule, ResolvedTask};
use gents_documents::{ConcurrencyMode, ScheduleCadence};
use gents_inference::backends::provider::BackendProviderKind;
use gents_inference::resolved::{ResolvedAgent, SamplingConfig};
use gents_lean_vocab::{assert_lean_to_defradb_vocabulary_matches, LeanVocabulary};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::{KeyIdentity, RuntimeNode};
use gents_store::schema::ensure_runtime_schemas;
use gents_tools::surface::AgentToolSurfaceConfig;

fn stub_principal() -> Arc<gents_store::identity::RuntimeNode> {
    let identity: Arc<dyn gents_store::identity::NodeIdentity> = Arc::new(
        KeyIdentity::load_or_create(
            std::env::temp_dir().join(format!("stub-principal-{}.key", uuid::Uuid::new_v4())),
            None,
        )
        .unwrap(),
    );
    Arc::new(gents_store::identity::RuntimeNode {
        node_did: identity.did().to_string(),
        identity,
        default_agent_did: String::new(),
        display_name: None,
        enabled: true,
    })
}

const LEAN_TRIGGER_TYPES_MODEL: &str =
    include_str!("../../../../../gents/proofs/Proofs/Automation/Triggers/Types.lean");
const LEAN_TRIGGER_TYPES_FILE: &str = "crates/gents/proofs/Proofs/Automation/Triggers/Types.lean";

#[test]
fn durable_fire_keys_are_unambiguous_across_component_delimiters() {
    let left =
        gents_protocol::trigger_delivery::durable_fire_key("event-document", &["trigger:a", "doc"]);
    let right =
        gents_protocol::trigger_delivery::durable_fire_key("event-document", &["trigger", "a:doc"]);
    assert_ne!(left, right);
    assert_ne!(
        left,
        gents_protocol::trigger_delivery::durable_fire_key("event-group", &["trigger:a", "doc"])
    );
}

#[test]
fn rust_trigger_kind_vocabulary_matches_lean_model() {
    let rust_kinds = vec![
        TriggerKind::Schedule.as_str(),
        TriggerKind::Event.as_str(),
        TriggerKind::Manual.as_str(),
    ];
    assert_lean_to_defradb_vocabulary_matches(LeanVocabulary {
        lean_file: LEAN_TRIGGER_TYPES_FILE,
        model: LEAN_TRIGGER_TYPES_MODEL,
        namespace: "TriggerKind",
        rust_source: "TriggerKind::{Schedule, Event, Manual}",
        rust_values: &rust_kinds,
    });
}

fn snapshot_with_schedules(
    schedules: RapidHashMap<String, ResolvedSchedule>,
) -> Arc<ActiveRuntimeSnapshot> {
    snapshot_with_agent_and_schedules(spy_test_agent(), schedules)
}

fn spy_test_agent() -> Arc<ResolvedAgent> {
    static AGENT: std::sync::OnceLock<Arc<ResolvedAgent>> = std::sync::OnceLock::new();
    AGENT
        .get_or_init(|| integration_test_agent("trigger-spy"))
        .clone()
}

fn spy_resolved_task(prompt_template: &str) -> ResolvedTask {
    let mut task = resolved_task(prompt_template);
    task.agent_did = spy_test_agent().agent_did.clone();
    task
}

fn resolved_task(prompt_template: &str) -> ResolvedTask {
    ResolvedTask {
        emit_outcome: false,
        task_id: "t1".to_string(),
        name: None,
        agent_did: spy_test_agent().agent_did.clone(),
        prompt_template: prompt_template.to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        goal_max_infrastructure_retries: None,
        output_schema_ref: None,
        hooks: Vec::new(),
        command: None,
    }
}

fn resolved_schedule(schedule_id: &str, task: ResolvedTask) -> ResolvedSchedule {
    ResolvedSchedule {
        session_id_template: None,
        create_session: false,
        max_fires_per_hour: gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
        trigger_doc_id: format!("{schedule_id}-doc"),
        schedule_doc_id: format!("{schedule_id}-schedule-doc"),
        schedule_id: schedule_id.to_string(),
        task,
        cadence: ScheduleCadence::Interval { interval_secs: 60 },
        concurrency: ConcurrencyMode::Serial,
    }
}

fn resolved_schedule_with_concurrency(
    schedule_id: &str,
    task: ResolvedTask,
    concurrency: ConcurrencyMode,
) -> ResolvedSchedule {
    ResolvedSchedule {
        session_id_template: None,
        create_session: false,
        max_fires_per_hour: gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
        trigger_doc_id: format!("{schedule_id}-doc"),
        schedule_doc_id: format!("{schedule_id}-schedule-doc"),
        schedule_id: schedule_id.to_string(),
        task,
        cadence: ScheduleCadence::Interval { interval_secs: 60 },
        concurrency,
    }
}

fn resolved_event_trigger_with_concurrency(
    trigger_id: &str,
    task: ResolvedTask,
    concurrency: ConcurrencyMode,
) -> ResolvedEventTrigger {
    ResolvedEventTrigger {
        event_source_id: "source".into(),
        event_source_doc_id: "source-doc".into(),
        task_doc_id: "task-doc".into(),
        session_id_template: None,
        create_session: false,
        max_fires_per_hour: gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
        trigger_doc_id: format!("{trigger_id}-doc"),
        trigger_id: trigger_id.to_string(),
        task,
        source_collection: "WebhookEvent".to_string(),
        event_kind: "created".to_string(),
        filter: None,
        concurrency,
        fire_mode: gents_documents::automation::EventTriggerFireMode::PerDocument,
        correlation_field: None,
        expected_count: None,
        expected_count_field: None,
        group_timeout_secs: None,
        group_min_count: 1,
        workspace_authority: None,
    }
}

fn trigger_kind_from_lean(value: &str) -> TriggerKind {
    match value {
        "schedule" => TriggerKind::Schedule,
        "event" => TriggerKind::Event,
        "manual" => TriggerKind::Manual,
        other => panic!("unknown Lean trigger kind {other:?}"),
    }
}

fn concurrency_from_lean(value: &str) -> ConcurrencyMode {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .unwrap_or_else(|error| panic!("unknown Lean concurrency mode {value:?}: {error}"))
}

fn snapshot_from_trigger_contract(
    active_schedule_ids: &[String],
    active_event_trigger_ids: &[String],
    task: &ResolvedTask,
    concurrency: ConcurrencyMode,
) -> Arc<ActiveRuntimeSnapshot> {
    let active_schedules = active_schedule_ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                resolved_schedule_with_concurrency(id, task.clone(), concurrency),
            )
        })
        .collect();
    let active_event_triggers = active_event_trigger_ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                resolved_event_trigger_with_concurrency(id, task.clone(), concurrency),
            )
        })
        .collect();
    let agent = spy_test_agent();
    let resolved = ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        agent.agent_did.clone(),
        vec![with_default_tools(agent.clone())],
        RapidHashMap::new(),
        RapidHashMap::new(),
        RapidHashMap::new(),
    )
    .with_automation(gents_documents::automation::ResolvedAutomation {
        schedules: active_schedules,
        event_triggers: active_event_triggers,
        ..Default::default()
    })
    .with_node(agent.node.clone())
    .with_local_did(agent.node_did().to_string());
    Arc::new(resolved.activate(1, RapidHashMap::new()))
}

/// Pairs a fixture agent with the default tool surface configuration, as the snapshot carries them side by side.
pub(super) fn with_default_tools(
    agent: Arc<ResolvedAgent>,
) -> gents_tools::surface::AgentWithTools {
    gents_tools::surface::AgentWithTools {
        agent,
        tools: Arc::new(AgentToolSurfaceConfig::default()),
    }
}

pub(super) fn integration_test_agent(behavior_name: &str) -> Arc<ResolvedAgent> {
    let identity: Arc<dyn gents_store::identity::NodeIdentity> = Arc::new(
        KeyIdentity::load_or_create(
            std::env::temp_dir().join(format!("{behavior_name}-{}.key", uuid::Uuid::new_v4())),
            None,
        )
        .unwrap(),
    );
    let principal = Arc::new(RuntimeNode {
        node_did: identity.did().to_string(),
        identity,
        default_agent_did: String::new(),
        display_name: None,
        enabled: true,
    });
    let binding =
        gents_store::identity::provision_agent_identity(principal.identity.as_ref()).unwrap();
    let agent_identity =
        gents_store::identity::resolve_agent_identity(principal.identity.as_ref(), &binding)
            .unwrap();
    let agent_did = binding.agent_did;
    Arc::new(ResolvedAgent {
        configuration: gents_inference::resolved::ResolvedConfiguration::Static,
        profile_id: None,
        skills: Vec::new(),
        oauth_pool: Vec::new(),
        agent_did,
        agent_identity,
        node: principal,
        backend_id: Some("backend-it".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://localhost:0/v1".to_string(),
        claude_platform: None,
        client_version: None,
        backend_auth: gents_documents::BackendAuth::Unauthenticated,
        model_name: gents_inference::resolved::DEFAULT_MODEL_NAME.to_string(),
        service_tier: None,
        resolved_reasoning_efforts: None,
        context_window: gents_inference::resolved::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: gents_inference::resolved::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: gents_inference::resolved::DEFAULT_MAX_TURNS,
        max_turns_provenance: gents_inference::resolved::MaxTurnsProvenance::Default,
        system_prompt: String::new(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        max_invalid_tool_calls: gents_loop::loop_stream::DEFAULT_MAX_INVALID_TOOL_CALLS,
        repeated_tool_failure_limit: gents_loop::loop_stream::DEFAULT_REPEATED_TOOL_FAILURE_LIMIT,
        max_request_retries: gents_documents::inference_defaults::DEFAULT_REQUEST_MAX_RETRIES,
        stream_batch_ms: gents_inference::resolved::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(
            gents_documents::inference_defaults::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: Duration::from_secs(
            gents_documents::inference_defaults::DEFAULT_DEADLINE_DURATION_SECS,
        ),
        title_timeout: Duration::from_secs(gents_inference::resolved::DEFAULT_TITLE_TIMEOUT_SECS),
        provider_idle_timeout: Duration::from_secs(
            gents_inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
        ),
        completion_retry: gents_loop::completion_retry::CompletionRetryProfileFields::default(),
        sampling: SamplingConfig::default(),
    })
}

/// A signed fixture Node whose default Agent is derived from the node's own
/// runtime key, so the materializer can act as that Agent's managed signer.
pub(super) async fn signed_materializer_agent(
    name: &str,
) -> (
    gents_store::test_support::TestStore,
    gents_store::identity::KeyIdentity,
    gents_store::config_client::ConfigAccess,
    Arc<ResolvedAgent>,
) {
    let signed_home = gents_documents::test_support::SignedHome::start(
        name,
        crate::lifecycle_hooks::composition(),
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, user, access) = signed_home.into_parts();
    let template = tokio::task::spawn_blocking(|| integration_test_agent("materializer-template"))
        .await
        .expect("build test agent template");
    let identity =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
            .expect("derive the fixture's held Agent from its runtime key");
    let mut agent = (*template).clone();
    agent.agent_did = binding.agent_did.clone();
    agent.agent_identity = identity;
    agent.node = Arc::new(gents_store::identity::RuntimeNode {
        node_did: db.node_identity.did().to_owned(),
        identity: db.node_identity.clone(),
        default_agent_did: binding.agent_did,
        display_name: None,
        enabled: true,
    });
    (db, user, access, Arc::new(agent))
}

/// The fixture Agent's managed access, which owns and reads its rows.
pub(super) fn agent_access(
    node: &Arc<defra_node::Node>,
    agent: &ResolvedAgent,
) -> gents_store::config_client::ConfigAccess {
    gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.agent_did.parse().unwrap(),
        agent.agent_identity.as_ref(),
    )
    .unwrap()
}

/// Applies `config` as the operator User, who owns configuration, and returns
/// the document identity of `trigger_id`.
pub(super) async fn apply_user_config(
    client: &gents_store::config_client::ConfigAccess,
    config: &gents_documents::PackConfig,
    trigger_id: &str,
) -> String {
    let owner = config.node.node_did.clone();
    let plan = gents_documents::DesiredStateApplyPlan::from_pack_config(config).unwrap();
    client
        .transact_write("test.trigger_config", |txn| {
            let plan = &plan;
            let owner = &owner;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await?;
                let (id, _) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Trigger,
                    owner,
                    trigger_id,
                )
                .await?
                .expect("created trigger");
                Ok::<_, anyhow::Error>(id)
            })
        })
        .await
        .unwrap()
}

pub(super) fn snapshot_with_agent_and_schedules(
    behavior: Arc<ResolvedAgent>,
    schedules: RapidHashMap<String, ResolvedSchedule>,
) -> Arc<ActiveRuntimeSnapshot> {
    let principal = behavior.node.clone();
    let local_did = behavior.node_did().to_string();
    let resolved = ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        behavior.agent_did.clone(),
        vec![with_default_tools(behavior)],
        RapidHashMap::new(),
        RapidHashMap::new(),
        RapidHashMap::new(),
    )
    .with_automation(gents_documents::automation::ResolvedAutomation {
        schedules,
        ..Default::default()
    })
    .with_node(principal)
    .with_local_did(local_did);
    Arc::new(resolved.activate(1, RapidHashMap::new()))
}

mod command_task;
mod dispatch;
mod dispatch_contract;
mod event_source;
mod manual_source;
mod production_materializer;
mod schedule_source;

#[tokio::test]
async fn trigger_engine_dispatch_matches_lean_generated_contract_cases() {
    dispatch_contract::trigger_engine_dispatch_matches_lean_generated_contract_cases().await;
}

#[tokio::test]
async fn event_group_eligibility_matches_lean_generated_contract_cases() {
    dispatch_contract::event_group_eligibility_matches_lean_generated_contract_cases().await;
}
