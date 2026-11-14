pub mod goal_hook;

use std::sync::Arc;

use anyhow::Result;
use defra_node::Node as EmbeddedNode;
use gents_protocol::event_delivery::EventConsumer;
use gents_protocol::request_admission::AgentRequestCreate;
use gents_protocol::trigger_delivery::TriggerFire;
use serde_json::Value;

use crate::triggers::engine::durable;
use crate::triggers::TaskDeliveryAdmission;
use gents_documents::{automation, event_consumer, event_trigger, EventSource, Task, Trigger};
use gents_session::request::lifecycle::{
    materialize, ExecutionOrigin, ExecutionOwnershipLost, RequestSpec, TriggerLineage,
    WorkspaceLineage,
};
use gents_store::config_client::ConfigApplyTxn;
use gents_store::identity::NodeIdentity;

/// How many generated cases, each on its own node, run at once.
pub const CASES_AT_ONCE: usize = 8;

/// Drives every generated case to completion as its own task, `CASES_AT_ONCE`
/// at a time, and re-raises the first case panic in the caller. Most of a
/// case's work runs on the task that drives it, so the caller's runtime needs
/// several workers for the cases to overlap.
pub async fn drive_cases<C, F, Fut>(cases: C, drive: F)
where
    C: IntoIterator,
    F: FnMut(C::Item) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    use futures::StreamExt;
    let mut finished = futures::stream::iter(cases)
        .map(drive)
        .map(tokio::spawn)
        .buffer_unordered(CASES_AT_ONCE);
    while let Some(result) = finished.next().await {
        if let Err(error) = result {
            std::panic::resume_unwind(error.into_panic());
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct TriggerRuntimeUpdate {
    pub next_run_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_fired_source_doc_id: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub fire_count_delta: Option<i64>,
}

pub async fn update_trigger_runtime_fields(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    trigger_id: &str,
    updates: TriggerRuntimeUpdate,
) -> Result<()> {
    let TriggerRuntimeUpdate {
        next_run_at,
        last_attempt_at,
        last_fired_source_doc_id,
        last_status,
        last_error,
        fire_count_delta,
    } = updates;
    event_trigger::update_trigger_runtime_fields(
        node,
        node_did,
        trigger_id,
        event_trigger::TriggerRuntimeUpdate {
            next_run_at,
            last_attempt_at,
            last_fired_source_doc_id,
            last_status,
            last_error,
            fire_count_delta,
        },
    )
    .await
}

pub struct EventConfigDocIds<'a> {
    pub trigger: &'a str,
    pub source: &'a str,
    pub task: &'a str,
}

pub fn event_admission_generation(
    source: &EventSource,
    trigger: &Trigger,
    task: &Task,
    ids: &EventConfigDocIds<'_>,
) -> Result<Value> {
    automation::event_admission_generation(
        source,
        trigger,
        task,
        &automation::EventConfigDocIds {
            trigger: ids.trigger,
            source: ids.source,
            task: ids.task,
        },
    )
}

pub async fn checkpoint_prefix(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    collection: &str,
    through: &str,
    legacy_serial_busy: bool,
) -> Result<bool> {
    event_consumer::checkpoint_prefix(
        txn,
        owner,
        consumer,
        collection,
        through,
        legacy_serial_busy,
    )
    .await
}

pub fn execution_ownership_lost_reason() -> String {
    ExecutionOwnershipLost.to_string()
}

#[allow(clippy::too_many_arguments)]
pub fn pending_agent_request_spec(
    node_did: &str,
    agent_did: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    trigger_lineage: TriggerLineage,
    conversation_title: Option<&str>,
    workspace_lineage: Option<&WorkspaceLineage>,
    request_id: &str,
    session_id: &str,
    retry_key: Option<&str>,
    requester_did: Option<&str>,
    trigger_doc_id: Option<&str>,
) -> Result<RequestSpec> {
    materialize::pending_agent_request_spec(
        node_did,
        agent_did,
        content,
        execution_origin,
        trigger_lineage,
        conversation_title,
        workspace_lineage,
        request_id,
        session_id,
        retry_key,
        requester_did,
        trigger_doc_id,
    )
}

#[derive(Clone)]
pub struct PreparedFire {
    pub receipt: TriggerFire,
    pub target_existing: bool,
    pub create_session: bool,
    pub replace_prior: bool,
    pub event_source_generation: Option<Value>,
    pub max_fires_per_hour: u32,
}

pub async fn write_trigger_delivery_with_identity(
    node: &Arc<EmbeddedNode>,
    actor: ::identity::Did,
    prepared: &PreparedFire,
    create: &AgentRequestCreate,
    identity: Option<&dyn NodeIdentity>,
) -> Result<TaskDeliveryAdmission> {
    let PreparedFire {
        receipt,
        target_existing,
        create_session,
        replace_prior,
        event_source_generation,
        max_fires_per_hour,
    } = prepared.clone();
    durable::write_trigger_delivery_with_identity(
        node,
        actor,
        &durable::PreparedFire {
            receipt,
            target_existing,
            create_session,
            replace_prior,
            event_source_generation,
            max_fires_per_hour,
        },
        create,
        identity,
    )
    .await
}

pub async fn signed_test_node(
    name: &str,
) -> (
    gents_inference::test_support::TestDb,
    gents_store::identity::KeyIdentity,
    gents_store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    gents_inference::test_support::signed_test_node(name, crate::lifecycle_hooks::composition())
        .await
}

pub mod self_config;
pub mod self_config_text;

pub async fn self_config_fixture(
    name: &str,
) -> gents_toolcalls::test_support::self_config::SelfConfigFixture {
    let db = gents_inference::test_support::test_db_with_tool_policy(
        name,
        crate::lifecycle_hooks::composition(),
        std::sync::Arc::new(gents_tools::surface::ToolCeiling::meta_only()),
    )
    .await;
    gents_toolcalls::test_support::self_config::SelfConfigFixture::from_db(db, name).await
}

pub mod loop_stream;
