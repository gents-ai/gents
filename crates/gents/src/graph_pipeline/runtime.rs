use std::collections::BTreeSet;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::graphql::{
    graphql_input_literal, graphql_rows_from_response, graphql_string_list_literal,
};
use identity::Did;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::config_client::{write_event_trigger_document, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;

use super::{
    verify_graph_plan_digest, DeliveryConcurrency, DeliveryMode, GraphPlan, GroupCount,
};

const TRIGGER_PREFIX: &str = "graph-trigger-";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublishedGraph {
    pub graph_id: String,
    pub digest: String,
    pub trigger_ids: Vec<String>,
}

fn digest_hex(digest: &str) -> Result<&str> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        anyhow::bail!("graph plan digest must use sha256:<64 lowercase hex>");
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        anyhow::bail!("graph plan digest must use sha256:<64 lowercase hex>");
    }
    Ok(hex)
}

fn trigger_id(digest: &str, route: &str) -> Result<String> {
    let component = format!("{:x}", Sha256::digest(route.as_bytes()));
    Ok(format!(
        "{TRIGGER_PREFIX}{}-{}",
        digest_hex(digest)?,
        &component[..16]
    ))
}

fn publication_receipt(plan: &GraphPlan) -> Result<PublishedGraph> {
    let mut trigger_ids = plan
        .entries
        .iter()
        .map(|entry| {
            trigger_id(
                &plan.digest,
                &format!(
                    "entry:{}:{}:{}",
                    entry.name, entry.to.node_id, entry.to.port
                ),
            )
        })
        .chain(plan.edges.iter().enumerate().map(|(index, edge)| {
            trigger_id(
                &plan.digest,
                &format!(
                    "edge:{index}:{}:{}:{}:{}",
                    edge.from.node_id, edge.from.port, edge.to.node_id, edge.to.port,
                ),
            )
        }))
        .collect::<Result<Vec<_>>>()?;
    trigger_ids.sort();
    Ok(PublishedGraph {
        graph_id: plan.graph_id.clone(),
        digest: plan.digest.clone(),
        trigger_ids,
    })
}

async fn query_graph_definition(txn: &ConfigApplyTxn<'_>, graph_id: &str) -> Result<Option<Value>> {
    let query = format!(
        r#"{{
            GraphDefinition(filter: {{ graph_id: {{ _eq: "{}" }} }}, limit: 2) {{
                _docID graph_id owner_did digest plan_json
            }}
        }}"#,
        escape_graphql_string(graph_id)
    );
    let response = txn.execute(&query).await?;
    let rows = graphql_rows_from_response(&response, "GraphDefinition");
    if rows.len() > 1 {
        anyhow::bail!("multiple GraphDefinition rows share graph_id {graph_id:?}");
    }
    Ok(rows.first().cloned())
}

async fn query_enabled_task_ids(
    txn: &ConfigApplyTxn<'_>,
    task_ids: &[String],
) -> Result<BTreeSet<String>> {
    let task_ids = graphql_string_list_literal(task_ids);
    let response = txn
        .execute(&format!(
            "{{ Task(filter: {{ task_id: {{ _in: {task_ids} }}, enabled: {{ _eq: true }} }}) {{ task_id }} }}"
        ))
        .await?;
    Ok(graphql_rows_from_response(&response, "Task")
        .iter()
        .filter_map(|row| {
            row.get("task_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect())
}

async fn create_graph_definition(
    txn: &ConfigApplyTxn<'_>,
    owner_did: &str,
    plan: &GraphPlan,
    plan_json: &str,
    now: &str,
) -> Result<()> {
    let input = graphql_input_literal(&json!({
        "graph_id": plan.graph_id,
        "owner_did": owner_did,
        "digest": plan.digest,
        "plan_json": plan_json,
        "created_at": now,
    }))?;
    txn.execute(&format!(
        "mutation {{ create_GraphDefinition(input: {input}) {{ _docID }} }}"
    ))
    .await?;
    Ok(())
}

async fn write_trigger(
    txn: &ConfigApplyTxn<'_>,
    id: &str,
    task_id: &str,
    source_collection: &str,
    correlation_field: &str,
    delivery: &DeliveryMode,
    concurrency: &DeliveryConcurrency,
    predicate: Option<&str>,
    now: &str,
) -> Result<()> {
    let (fire_mode, expected_count, expected_count_field, group_timeout_secs) = match delivery {
        DeliveryMode::PerDocument => (
            "per_document",
            Value::Null,
            Value::Null,
            Value::Null,
        ),
        DeliveryMode::PerGroup {
            expected: GroupCount::Static { count },
            timeout_secs,
        } => (
            "per_group",
            json!(count),
            Value::Null,
            timeout_secs.map(Value::from).unwrap_or(Value::Null),
        ),
        DeliveryMode::PerGroup {
            expected: GroupCount::SourceField { field },
            timeout_secs,
        } => (
            "per_group",
            Value::Null,
            json!(field),
            timeout_secs.map(Value::from).unwrap_or(Value::Null),
        ),
    };
    let concurrency = match concurrency {
        DeliveryConcurrency::Parallel => "parallel",
        DeliveryConcurrency::Serial => "serial",
    };
    let group_min_count = if group_timeout_secs.is_null() {
        Value::Null
    } else {
        json!(1)
    };
    let add = json!({
        "trigger_id": id,
        "task_id": task_id,
        "source_collection": source_collection,
        "event_kind": "created",
        "filter": predicate.map(Value::from).unwrap_or(Value::Null),
        "enabled": true,
        "concurrency": concurrency,
        "correlation_field": correlation_field,
        "fire_mode": fire_mode,
        "expected_count": expected_count,
        "expected_count_field": expected_count_field,
        "group_timeout_secs": group_timeout_secs,
        "group_min_count": group_min_count,
        "workspace_authority": Value::Null,
        "created_at": now,
        "updated_at": now,
    });
    let mut update = add.clone();
    update
        .as_object_mut()
        .expect("trigger object")
        .remove("created_at");
    write_event_trigger_document(txn, id, &add, &update).await?;
    Ok(())
}

/// Publish a compiled graph as ordinary EventTrigger documents in one
/// identity-scoped transaction. Stage Tasks already exist and remain the
/// runtime's single source of prompts, behaviors, tools, and model settings.
///
/// A graph ID is immutable in v1. Repeating the same plan is idempotent; a
/// different plan must use a new graph ID. Execution starts separately through
/// existing bounded document writes after normal runtime reconciliation.
pub async fn publish_graph_plan(
    node: &EmbeddedNode,
    identity: Did,
    plan: &GraphPlan,
) -> Result<PublishedGraph> {
    if !verify_graph_plan_digest(plan) {
        anyhow::bail!("refusing to publish a GraphPlan with an invalid digest");
    }
    digest_hex(&plan.digest)?;
    let owner_did = identity.to_string();
    let plan_json = serde_json::to_string(plan)?;
    let now = chrono::Utc::now().to_rfc3339();
    let txn = ConfigApplyTxn::begin_local(node, Some(identity)).await?;
    let result = async {
        if let Some(existing) = query_graph_definition(&txn, &plan.graph_id).await? {
            if existing.get("owner_did").and_then(Value::as_str) != Some(owner_did.as_str()) {
                anyhow::bail!("graph {:?} belongs to a different principal", plan.graph_id);
            }
            if existing.get("digest").and_then(Value::as_str) != Some(plan.digest.as_str())
                || existing.get("plan_json").and_then(Value::as_str) != Some(plan_json.as_str())
            {
                anyhow::bail!(
                    "graph {:?} is immutable; publish a new graph_id for a changed plan",
                    plan.graph_id
                );
            }
        } else {
            create_graph_definition(&txn, &owner_did, plan, &plan_json, &now).await?;
        }

        let task_ids = plan
            .nodes
            .iter()
            .map(|node| node.task_id.clone())
            .collect::<Vec<_>>();
        let enabled_tasks = query_enabled_task_ids(&txn, &task_ids).await?;
        for planned_node in &plan.nodes {
            if !enabled_tasks.contains(&planned_node.task_id) {
                anyhow::bail!(
                    "approved task {:?} is missing or disabled",
                    planned_node.task_id
                );
            }
        }

        for entry in &plan.entries {
            let id = trigger_id(
                &plan.digest,
                &format!(
                    "entry:{}:{}:{}",
                    entry.name, entry.to.node_id, entry.to.port
                ),
            )?;
            write_trigger(
                &txn,
                &id,
                &entry.target_task_id,
                &entry.collection,
                &entry.correlation_field,
                &DeliveryMode::PerDocument,
                &DeliveryConcurrency::Parallel,
                None,
                &now,
            )
            .await?;
        }
        for (index, edge) in plan.edges.iter().enumerate() {
            let id = trigger_id(
                &plan.digest,
                &format!(
                    "edge:{index}:{}:{}:{}:{}",
                    edge.from.node_id, edge.from.port, edge.to.node_id, edge.to.port,
                ),
            )?;
            write_trigger(
                &txn,
                &id,
                &edge.target_task_id,
                &edge.source_collection,
                &edge.correlation_field,
                &edge.delivery,
                &edge.concurrency,
                edge.predicate.as_deref(),
                &now,
            )
            .await?;
        }
        publication_receipt(plan)
    }
    .await;

    match result {
        Ok(receipt) => {
            txn.commit().await.context("commit graph publication")?;
            Ok(receipt)
        }
        Err(error) => {
            if let Err(discard_error) = txn.discard().await {
                tracing::warn!(%discard_error, "graph publication transaction discard failed");
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_pipeline::{
        compile_graph, CompilerPolicy, EntryBinding, GraphIntent, GraphLimits, GraphNode,
        PortCardinality, PortRef, PortSpec, ResultCardinality, ResultContract, StageCapability,
    };

    fn plan() -> GraphPlan {
        let input = PortSpec {
            name: "input".to_owned(),
            collection: "PipelineInput".to_owned(),
            schema: "PipelineInput/v1".to_owned(),
            correlation_field: "run_id".to_owned(),
            cardinality: PortCardinality::One,
            required: true,
        };
        let output = PortSpec {
            name: "result".to_owned(),
            collection: "PipelineOutput".to_owned(),
            schema: "PipelineOutput/v1".to_owned(),
            correlation_field: "run_id".to_owned(),
            cardinality: PortCardinality::One,
            required: false,
        };
        let intent = GraphIntent {
            graph_id: "pipeline".to_owned(),
            nodes: vec![GraphNode {
                node_id: "worker".to_owned(),
                capability_id: "worker".to_owned(),
                capability_revision: "v1".to_owned(),
            }],
            edges: vec![],
            entries: vec![EntryBinding {
                name: "input".to_owned(),
                collection: input.collection.clone(),
                schema: input.schema.clone(),
                input_contract: None,
                to: PortRef {
                    node_id: "worker".to_owned(),
                    port: input.name.clone(),
                },
            }],
            results: vec![ResultContract {
                name: "result".to_owned(),
                from: PortRef {
                    node_id: "worker".to_owned(),
                    port: output.name.clone(),
                },
                cardinality: ResultCardinality::Exactly { count: 1 },
                terminal: true,
            }],
            limits: GraphLimits {
                max_nodes: 2,
                max_edges: 2,
                max_depth: 2,
                max_fan_out: 2,
                max_total_invocations: 2,
                max_runtime_secs: 60,
            },
        };
        compile_graph(
            &intent,
            &[StageCapability {
                capability_id: "worker".to_owned(),
                revision: "v1".to_owned(),
                task_id: "existing-worker-task".to_owned(),
                input_ports: vec![input],
                output_ports: vec![output],
                allowed_callers: vec!["did:key:owner".to_owned()],
            }],
            "did:key:owner",
            &CompilerPolicy::default(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn publication_is_atomic_idempotent_and_reuses_existing_tasks() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        for schema in [
            gents_protocol::schemas::GRAPH_DEFINITION,
            gents_protocol::schemas::TASK,
            gents_protocol::schemas::EVENT_TRIGGER,
        ] {
            node.add_schema(schema).await.unwrap();
        }
        node.execute(
            r#"mutation { create_Task(input: {
                task_id: "existing-worker-task",
                behavior_id: "worker",
                prompt_template: "existing approved prompt",
                enabled: true
            }) { _docID } }"#,
        )
        .await;

        let identity = Did::new("did:key:owner".to_owned()).unwrap();
        let first = publish_graph_plan(&node, identity.clone(), &plan())
            .await
            .unwrap();
        let second = publish_graph_plan(&node, identity.clone(), &plan())
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.trigger_ids.len(), 1);

        let mut changed = plan();
        changed.entries[0].name = "different".to_owned();
        changed.digest = crate::graph_pipeline::graph_plan_digest(&changed);
        let error = publish_graph_plan(&node, identity, &changed)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("immutable"));

        let persisted = node
            .execute("{ GraphDefinition { digest } Task { task_id prompt_template } EventTrigger { task_id source_collection } }")
            .await;
        assert!(!persisted.has_errors(), "{:?}", persisted.errors);
        let data = persisted.data.unwrap();
        assert_eq!(data["GraphDefinition"].as_array().unwrap().len(), 1);
        assert_eq!(data["Task"].as_array().unwrap().len(), 1);
        assert_eq!(
            data["Task"][0]["prompt_template"],
            "existing approved prompt"
        );
        assert_eq!(data["EventTrigger"][0]["task_id"], "existing-worker-task");
        node.shutdown().await;
    }
}
