use std::sync::Arc;

use anyhow::Result;
use defra_node::Node as EmbeddedNode;
use gents_protocol::event_delivery::EventConsumer;
use gents_protocol::request_admission::AgentRequestCreate;
use gents_protocol::trigger_delivery::TriggerFire;
use serde_json::Value;

use crate::documents::{automation, event_consumer, event_trigger, EventSource, Task, Trigger};
use crate::request::lifecycle::{
    materialize, ExecutionOrigin, ExecutionOwnershipLost, RequestSpec, TaskDeliveryAdmission,
    TriggerLineage, WorkspaceLineage,
};
use crate::store::config_client::ConfigApplyTxn;
use crate::store::identity::NodeIdentity;
use crate::triggers::engine::durable;

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
    pub replace_prior: bool,
    pub event_source_generation: Option<Value>,
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
        replace_prior,
        event_source_generation,
    } = prepared.clone();
    materialize::write_trigger_delivery_with_identity(
        node,
        actor,
        &durable::PreparedFire {
            receipt,
            target_existing,
            replace_prior,
            event_source_generation,
        },
        create,
        identity,
    )
    .await
}
