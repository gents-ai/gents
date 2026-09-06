use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::graphql::{graphql_input_literal, graphql_string_list_literal};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use identity::Did;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::{
    document_composite_version, escape_graphql_string, validate_collection_identifier,
};

use super::runtime::graph_trigger_id;
use super::{
    graph_run_terminal_decision, verify_graph_plan_digest, GraphPlan, PlannedResult,
    ResultCardinality,
};

const GRAPH_RUN_VIEW_VERSION: u32 = 1;

#[path = "logical_invocation.rs"]
mod logical_invocation;
const MAX_CANCEL_REASON_BYTES: usize = 1_024;

#[cfg(test)]
#[path = "attribution_contract_tests.rs"]
mod attribution_contract_tests;

#[cfg(test)]
#[path = "publication_contract_tests.rs"]
mod publication_contract_tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphResultRef {
    pub name: String,
    pub collection: String,
    pub document_id: String,
    pub commit_cid: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphRunResultView {
    pub name: String,
    pub terminal: bool,
    pub satisfied: bool,
    pub observed_count: usize,
    pub violation: Option<String>,
    pub refs: Vec<GraphResultRef>,
    /// Exact documents used to evaluate this named result contract. The
    /// durable run projection exposes values as well as immutable references
    /// so every observer can render useful output without a second UI model.
    pub documents: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphRunRequestView {
    pub request_id: String,
    pub session_id: Option<String>,
    pub node_id: Option<String>,
    pub behavior_id: String,
    pub lifecycle_state: Option<String>,
    pub failure_reason: Option<String>,
    pub terminal: bool,
    pub succeeded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphRunStageView {
    pub node_id: String,
    pub total: usize,
    pub active: usize,
    pub succeeded: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphRunGroupView {
    pub group_key: String,
    pub trigger_id: String,
    pub first_seen_at: String,
    pub quiesced_at: Option<String>,
    pub quiesced_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphRunView {
    pub view_version: u32,
    pub run_id: String,
    pub graph_id: String,
    pub revision_digest: String,
    pub owner_did: String,
    pub caller_did: String,
    pub entry_name: String,
    pub correlation: String,
    pub status: String,
    pub input: Value,
    pub cancellation_requested_at: Option<String>,
    pub cancellation_requested_by: Option<String>,
    pub cancellation_reason: Option<String>,
    pub error: Option<Value>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub deadline_at: Option<String>,
    pub completed_at: Option<String>,
    pub update_generation: i64,
    pub requests: Vec<GraphRunRequestView>,
    pub stages: Vec<GraphRunStageView>,
    pub groups: Vec<GraphRunGroupView>,
    pub results: Vec<GraphRunResultView>,
    pub persisted_result_refs: Vec<GraphResultRef>,
    pub active_request_count: usize,
    pub terminal_request_count: usize,
    /// Derived from physical ancestry and the canonical Goal; never persisted.
    #[serde(default)]
    pub outstanding_invocation_count: usize,
    #[serde(default)]
    pub terminal_stages_completed: bool,
    pub result_contract_satisfied: bool,
    pub failure_evidence: Option<Value>,
}

impl GraphRunView {
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "succeeded" | "failed" | "cancelled")
    }

    fn successful_result_refs(&self) -> Vec<GraphResultRef> {
        let mut refs = self
            .results
            .iter()
            .filter(|result| result.terminal)
            .flat_map(|result| result.refs.clone())
            .collect::<Vec<_>>();
        refs.sort_by(|left, right| {
            (&left.name, &left.document_id, &left.commit_cid).cmp(&(
                &right.name,
                &right.document_id,
                &right.commit_cid,
            ))
        });
        refs
    }
}

fn rows<'a>(response: &'a Value, name: &str) -> &'a [Value] {
    response
        .get("data")
        .and_then(|data| data.get(name))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn required_string<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("GraphRun is missing {field}"))
}

#[async_trait::async_trait]
trait GraphRunQuery: Sync {
    async fn execute_graph_query(&self, query: &str) -> Result<Value>;
}

#[async_trait::async_trait]
impl GraphRunQuery for EmbeddedNode {
    async fn execute_graph_query(&self, query: &str) -> Result<Value> {
        let response = self.execute(query).await;
        if response.has_errors() {
            anyhow::bail!("query graph run view failed: {:?}", response.errors);
        }
        Ok(json!({ "data": response.data.unwrap_or(Value::Null) }))
    }
}

#[async_trait::async_trait]
impl GraphRunQuery for ConfigAccess {
    async fn execute_graph_query(&self, query: &str) -> Result<Value> {
        self.execute(query).await
    }
}

#[async_trait::async_trait]
impl GraphRunQuery for ConfigApplyTxn<'_> {
    async fn execute_graph_query(&self, query: &str) -> Result<Value> {
        self.execute(query).await
    }
}

async fn query_run(executor: &(impl GraphRunQuery + ?Sized), run_id: &str) -> Result<Value> {
    let response = executor
        .execute_graph_query(&format!(
            r#"{{
            GraphRun(filter: {{ run_id: {{ _eq: "{}" }} }}, limit: 2) {{
                _docID run_id graph_id revision_digest owner_did caller_did entry_name correlation
                status input_json cancel_requested_at cancel_requested_by cancel_reason
                result_refs_json update_generation error created_at started_at completed_at
            }}
        }}"#,
            escape_graphql_string(run_id),
        ))
        .await?;
    let found = rows(&response, "GraphRun");
    if found.len() > 1 {
        anyhow::bail!("multiple GraphRun rows share run_id {run_id:?}");
    }
    found
        .first()
        .cloned()
        .with_context(|| format!("GraphRun {run_id:?} does not exist"))
}

async fn load_plan(executor: &(impl GraphRunQuery + ?Sized), digest: &str) -> Result<GraphPlan> {
    let response = executor.execute_graph_query(&format!(
        r#"{{ GraphRevision(filter: {{ digest: {{ _eq: "{}" }} }}, limit: 2) {{ plan_json }} }}"#,
        escape_graphql_string(digest),
    ))
    .await?;
    let found = rows(&response, "GraphRevision");
    if found.len() != 1 {
        anyhow::bail!("pinned GraphRevision {digest:?} is missing or ambiguous");
    }
    let plan: GraphPlan = serde_json::from_str(
        found[0]
            .get("plan_json")
            .and_then(Value::as_str)
            .context("pinned GraphRevision is missing plan_json")?,
    )?;
    if plan.digest != digest || !verify_graph_plan_digest(&plan) {
        anyhow::bail!("pinned GraphRevision plan failed immutable identity verification");
    }
    Ok(plan)
}

fn planned_trigger_nodes(plan: &GraphPlan) -> Result<BTreeMap<String, String>> {
    let mut nodes_by_trigger = BTreeMap::new();
    for entry in &plan.entries {
        let route = format!(
            "entry:{}:{}:{}",
            entry.name, entry.to.node_id, entry.to.port
        );
        nodes_by_trigger.insert(
            graph_trigger_id(&plan.digest, &route)?,
            entry.to.node_id.clone(),
        );
    }
    for (index, edge) in plan.edges.iter().enumerate() {
        let route = format!(
            "edge:{index}:{}:{}:{}:{}",
            edge.from.node_id, edge.from.port, edge.to.node_id, edge.to.port
        );
        nodes_by_trigger.insert(
            graph_trigger_id(&plan.digest, &route)?,
            edge.to.node_id.clone(),
        );
    }
    if nodes_by_trigger.is_empty() {
        anyhow::bail!("pinned graph plan has no materialized trigger routes");
    }
    Ok(nodes_by_trigger)
}

async fn load_groups(
    executor: &(impl GraphRunQuery + ?Sized),
    correlation: &str,
    plan: &GraphPlan,
) -> Result<Vec<GraphRunGroupView>> {
    let trigger_ids = planned_trigger_nodes(plan)?.into_keys().collect::<Vec<_>>();
    let limit = usize::try_from(plan.limits.max_total_invocations)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let response = executor
        .execute_graph_query(&format!(
            r#"{{
                EventTriggerGroupState(
                    filter: {{
                        correlation: {{ _eq: "{}" }},
                        trigger_id: {{ _in: {} }}
                    }},
                    order: {{ first_seen_at: ASC }}, limit: {limit}
                ) {{ group_key trigger_id first_seen_at quiesced_at quiesced_reason }}
            }}"#,
            escape_graphql_string(correlation),
            graphql_string_list_literal(&trigger_ids),
        ))
        .await?;
    rows(&response, "EventTriggerGroupState")
        .iter()
        .map(|row| {
            Ok(GraphRunGroupView {
                group_key: row
                    .get("group_key")
                    .and_then(Value::as_str)
                    .context("EventTriggerGroupState is missing group_key")?
                    .to_owned(),
                trigger_id: row
                    .get("trigger_id")
                    .and_then(Value::as_str)
                    .context("EventTriggerGroupState is missing trigger_id")?
                    .to_owned(),
                first_seen_at: row
                    .get("first_seen_at")
                    .and_then(Value::as_str)
                    .context("EventTriggerGroupState is missing first_seen_at")?
                    .to_owned(),
                quiesced_at: row
                    .get("quiesced_at")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                quiesced_reason: row
                    .get("quiesced_reason")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })
        })
        .collect()
}

fn current_commit_cid(row: &Value) -> Option<String> {
    row.get("_version")?
        .as_array()?
        .iter()
        .filter(|version| version.get("fieldName").and_then(Value::as_str) == Some("_C"))
        .filter_map(|version| {
            Some((
                version.get("height")?.as_i64()?,
                version.get("cid")?.as_str()?.to_owned(),
            ))
        })
        .max()
        .map(|(_, cid)| cid)
}

fn graph_run_deadline(
    started_at: Option<&str>,
    max_runtime_secs: u64,
) -> Result<Option<chrono::DateTime<chrono::FixedOffset>>> {
    started_at
        .map(|started_at| {
            let started_at = chrono::DateTime::parse_from_rfc3339(started_at)
                .context("GraphRun started_at is not RFC3339")?;
            let seconds = i64::try_from(max_runtime_secs)
                .context("graph max_runtime_secs exceeds the supported clock range")?;
            started_at
                .checked_add_signed(chrono::TimeDelta::seconds(seconds))
                .context("graph run deadline exceeds the supported clock range")
        })
        .transpose()
}

async fn load_result(
    executor: &(impl GraphRunQuery + ?Sized),
    correlation: &str,
    result: &PlannedResult,
) -> Result<GraphRunResultView> {
    validate_collection_identifier(&result.collection)?;
    crate::graphql::validate_graphql_name(&result.correlation_field)?;
    let limit = match result.cardinality {
        ResultCardinality::Exactly { count } | ResultCardinality::AtMost { count } => {
            usize::try_from(count)
                .unwrap_or(usize::MAX)
                .saturating_add(1)
        }
    };
    let response = executor
        .execute_graph_query(&format!(
            r#"{{
                {collection}(
                    filter: {{ {correlation_field}: {{ _eq: "{correlation}" }} }},
                    limit: {limit}
                ) {{ _docID _version {{ cid height fieldName }} }}
            }}"#,
            collection = result.collection,
            correlation_field = result.correlation_field,
            correlation = escape_graphql_string(correlation),
        ))
        .await?;
    let docs = rows(&response, &result.collection).to_vec();
    let cardinality_valid = match result.cardinality {
        ResultCardinality::Exactly { count } => docs.len() == count as usize,
        ResultCardinality::AtMost { count } => docs.len() <= count as usize,
    };
    let violation = (!cardinality_valid).then(|| {
        format!(
            "result cardinality {:?} observed {} documents",
            result.cardinality,
            docs.len()
        )
    });
    let mut refs = Vec::with_capacity(docs.len());
    for doc in &docs {
        let document_id = doc
            .get("_docID")
            .and_then(Value::as_str)
            .context("result document is missing _docID")?;
        let commit_cid = current_commit_cid(doc)
            .with_context(|| format!("result document {document_id} has no current commit CID"))?;
        refs.push(GraphResultRef {
            name: result.name.clone(),
            collection: result.collection.clone(),
            document_id: document_id.to_owned(),
            commit_cid,
        });
    }
    refs.sort_by(|left, right| left.document_id.cmp(&right.document_id));
    let documents = docs
        .iter()
        .cloned()
        .map(|mut document| {
            if let Some(object) = document.as_object_mut() {
                object.remove("_version");
            }
            document
        })
        .collect();
    Ok(GraphRunResultView {
        name: result.name.clone(),
        terminal: result.terminal,
        satisfied: violation.is_none(),
        observed_count: refs.len(),
        violation,
        refs,
        documents,
    })
}

async fn load_graph_run_view_with(
    executor: &(impl GraphRunQuery + ?Sized),
    actor_did: &str,
    run_id: &str,
) -> Result<GraphRunView> {
    let run = query_run(executor, run_id).await?;
    let owner_did = required_string(&run, "owner_did")?;
    let caller_did = required_string(&run, "caller_did")?;
    if actor_did != owner_did && actor_did != caller_did {
        anyhow::bail!("actor is not authorized to observe this graph run");
    }
    let revision_digest = required_string(&run, "revision_digest")?;
    let plan = load_plan(executor, revision_digest).await?;
    let correlation = required_string(&run, "correlation")?;
    let logical = logical_invocation::load(executor, correlation, &plan).await?;
    let requests = logical.requests;
    let outstanding_invocation_count = logical
        .invocations
        .iter()
        .filter(|invocation| invocation.outstanding)
        .count();
    let invalid_invocation = logical
        .invocations
        .iter()
        .find(|invocation| invocation.invalid);
    let groups = load_groups(executor, correlation, &plan).await?;
    let mut results = Vec::with_capacity(plan.results.len());
    for result in &plan.results {
        results.push(load_result(executor, correlation, result).await?);
    }
    results.sort_by(|left, right| left.name.cmp(&right.name));

    let mut stages = plan
        .nodes
        .iter()
        .map(|node| GraphRunStageView {
            node_id: node.node_id.clone(),
            total: 0,
            active: 0,
            succeeded: 0,
            failed: 0,
        })
        .collect::<Vec<_>>();
    for stage in &mut stages {
        for request in requests
            .iter()
            .filter(|request| request.node_id.as_deref() == Some(stage.node_id.as_str()))
        {
            stage.total += 1;
            if !request.terminal {
                stage.active += 1;
            } else if request.succeeded {
                stage.succeeded += 1;
            } else {
                stage.failed += 1;
            }
        }
    }
    let active_request_count = requests.iter().filter(|request| !request.terminal).count();
    let terminal_request_count = requests.len() - active_request_count;
    let unknown_request = requests.iter().find(|request| request.node_id.is_none());
    let failed_invocation = logical.invocations.iter().find(|invocation| {
        !invocation.outstanding
            && !invocation.invalid
            && invocation
                .tip
                .as_ref()
                .is_some_and(|tip| tip.terminal && !tip.succeeded)
    });
    let over_limit = requests.len() > plan.limits.max_total_invocations as usize;
    let invalid_group = groups.iter().find(|group| group.quiesced_at.is_some());
    let started_at = run
        .get("started_at")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let deadline = graph_run_deadline(started_at.as_deref(), plan.limits.max_runtime_secs)?;
    let deadline_exceeded = deadline
        .as_ref()
        .is_some_and(|deadline| chrono::Utc::now() >= *deadline);
    let terminal_results = results
        .iter()
        .filter(|result| result.terminal)
        .collect::<Vec<_>>();
    let result_contract_satisfied =
        !terminal_results.is_empty() && results.iter().all(|result| result.satisfied);
    let terminal_nodes = plan
        .results
        .iter()
        .filter(|result| result.terminal)
        .map(|result| result.from.node_id.as_str())
        .collect::<BTreeSet<_>>();
    let terminal_stages_completed = !terminal_nodes.is_empty()
        && terminal_nodes.iter().all(|node_id| {
            logical.invocations.iter().any(|invocation| {
                invocation.node_id == *node_id
                    && !invocation.outstanding
                    && !invocation.invalid
                    && invocation
                        .tip
                        .as_ref()
                        .is_some_and(|tip| tip.terminal && tip.succeeded)
            })
        });
    let failure_evidence = if let Some(group) = invalid_group {
        Some(json!({
            "version": 1, "code": "group_quiesced",
            "message": group.quiesced_reason.as_deref().unwrap_or("required graph group was quiesced"),
            "group_key": group.group_key, "trigger_id": group.trigger_id,
        }))
    } else if let Some(request) = unknown_request {
        Some(json!({
            "version": 1, "code": "contract_drift",
            "message": "pinned graph request lacks authenticated route binding",
            "request_id": request.request_id, "behavior_id": request.behavior_id,
        }))
    } else if let Some(invocation) = invalid_invocation {
        Some(json!({
            "version": 1, "code": "contract_drift",
            "message": "graph invocation has ambiguous or cyclic authenticated ancestry",
            "root_request_id": invocation.root_request_id,
        }))
    } else if over_limit {
        Some(json!({
            "version": 1, "code": "invocation_limit_exceeded",
            "message": "correlated request count exceeds the pinned graph limit",
            "observed": requests.len(), "limit": plan.limits.max_total_invocations,
        }))
    } else if deadline_exceeded {
        Some(json!({
            "version": 1,
            "code": "run_deadline_exceeded",
            "message": "graph run exceeded its pinned maximum runtime",
            "max_runtime_secs": plan.limits.max_runtime_secs,
            "deadline_at": deadline.map(|value| value.to_rfc3339()),
        }))
    } else if let Some(invocation) = failed_invocation {
        let request = invocation
            .tip
            .as_ref()
            .expect("failed invocation has a tip");
        Some(json!({
            "version": 1, "code": "required_request_failed",
            "message": request.failure_reason.as_deref().unwrap_or("required graph request did not complete successfully"),
            "request_id": request.request_id,
            "root_request_id": invocation.root_request_id,
            "lifecycle_state": request.lifecycle_state,
        }))
    } else if active_request_count == 0
        && outstanding_invocation_count == 0
        && terminal_stages_completed
        && !result_contract_satisfied
    {
        Some(json!({
            "version": 1,
            "code": "result_contract_unsatisfied",
            "message": "terminal graph stages completed but the declared result contract is unsatisfied",
            "violations": results.iter().filter_map(|result| {
                result.violation.as_ref().map(|violation| json!({
                    "name": result.name,
                    "violation": violation,
                }))
            }).collect::<Vec<_>>(),
        }))
    } else {
        None
    };
    let persisted_result_refs = run
        .get("result_refs_json")
        .and_then(Value::as_str)
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    let error: Option<Value> = run
        .get("error")
        .and_then(Value::as_str)
        .map(serde_json::from_str)
        .transpose()?;
    if let Some(primary) = &error {
        anyhow::ensure!(
            primary.get("version").and_then(Value::as_u64) == Some(1)
                && primary.get("message").and_then(Value::as_str).is_some()
                && matches!(
                    primary.get("code").and_then(Value::as_str),
                    Some(
                        "group_quiesced"
                            | "contract_drift"
                            | "invocation_limit_exceeded"
                            | "run_deadline_exceeded"
                            | "required_request_failed"
                            | "result_contract_unsatisfied"
                    )
                ),
            "invalid persisted graph failure evidence"
        );
    }
    // The first committed failure is durable even while siblings drain.
    // Later observations cannot replace it or turn the run into a success.
    let failure_evidence = error.clone().or(failure_evidence);

    Ok(GraphRunView {
        view_version: GRAPH_RUN_VIEW_VERSION,
        run_id: required_string(&run, "run_id")?.to_owned(),
        graph_id: required_string(&run, "graph_id")?.to_owned(),
        revision_digest: revision_digest.to_owned(),
        owner_did: owner_did.to_owned(),
        caller_did: caller_did.to_owned(),
        entry_name: required_string(&run, "entry_name")?.to_owned(),
        correlation: correlation.to_owned(),
        status: required_string(&run, "status")?.to_owned(),
        input: serde_json::from_str(required_string(&run, "input_json")?)?,
        cancellation_requested_at: run
            .get("cancel_requested_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        cancellation_requested_by: run
            .get("cancel_requested_by")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        cancellation_reason: run
            .get("cancel_reason")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        error,
        created_at: required_string(&run, "created_at")?.to_owned(),
        started_at,
        deadline_at: deadline.map(|value| value.to_rfc3339()),
        completed_at: run
            .get("completed_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        update_generation: run
            .get("update_generation")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        requests,
        stages,
        groups,
        results,
        persisted_result_refs,
        active_request_count,
        terminal_request_count,
        outstanding_invocation_count,
        terminal_stages_completed,
        result_contract_satisfied,
        failure_evidence,
    })
}

/// Authenticated derived association used by the existing publication transaction.
/// This is not stored and does not authorize a new request by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphInvocationBinding {
    pub run_id: String,
    pub revision_digest: String,
}

pub(crate) async fn graph_binding_for_request_in_txn(
    txn: &ConfigApplyTxn<'_>,
    parent_doc_id: &str,
) -> Result<Option<GraphInvocationBinding>> {
    // Correlation is only a discovery hint. Walk original physical ancestry so
    // a historical child with omitted optional correlation remains discoverable.
    let mut next = Some(parent_doc_id.to_owned());
    let mut seen = BTreeSet::new();
    let mut correlations = BTreeSet::new();
    while let Some(doc_id) = next {
        if !seen.insert(doc_id.clone()) {
            break;
        }
        let response = txn
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
                _docID caused_by_parent_request_doc_id caused_by_correlation caused_by_trigger_id
            }} }}"#,
                escape_graphql_string(&doc_id),
            ))
            .await?;
        let Some(row) = rows(&response, "AgentRequest").first() else {
            break;
        };
        let graph_route_hint = row
            .get("caused_by_trigger_id")
            .and_then(Value::as_str)
            .is_some_and(super::runtime::graph_artifact_is_reserved);
        if let Some(correlation) = row
            .get("caused_by_correlation")
            .and_then(Value::as_str)
            .filter(|s| graph_route_hint && !s.is_empty())
        {
            correlations.insert(correlation.to_owned());
        }
        next = row
            .get("caused_by_parent_request_doc_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }
    if correlations.is_empty() {
        return Ok(None);
    }
    let response = txn.execute(&format!(
        r#"{{ GraphRun(filter: {{ correlation: {{ _in: {} }} }}) {{ run_id revision_digest correlation }} }}"#,
        graphql_string_list_literal(&correlations.into_iter().collect::<Vec<_>>()),
    )).await?;
    let mut bindings = Vec::new();
    for run in rows(&response, "GraphRun") {
        let digest = required_string(run, "revision_digest")?;
        let plan = load_plan(txn, digest).await?;
        let projection =
            logical_invocation::load(txn, required_string(run, "correlation")?, &plan).await?;
        for invocation in projection.invocations {
            if invocation.member_doc_ids.contains(parent_doc_id) {
                anyhow::ensure!(
                    !invocation.invalid,
                    "cannot publish into ambiguous graph invocation ancestry"
                );
                bindings.push(GraphInvocationBinding {
                    run_id: required_string(run, "run_id")?.to_owned(),
                    revision_digest: digest.to_owned(),
                });
            }
        }
    }
    anyhow::ensure!(
        bindings.len() <= 1,
        "request belongs to multiple authenticated graph invocations"
    );
    Ok(bindings.pop())
}

/// Reconstruct one durable graph execution from its pinned revision and the
/// ordinary request/output documents. No progress cache or UI execution model
/// is involved.
pub async fn load_graph_run_view(
    node: &EmbeddedNode,
    actor_did: &str,
    run_id: &str,
) -> Result<GraphRunView> {
    load_graph_run_view_with(node, actor_did, run_id).await
}

/// The same durable projection through the repository's shared client access.
pub async fn load_graph_run_view_with_access(
    access: &ConfigAccess,
    actor_did: &str,
    run_id: &str,
) -> Result<GraphRunView> {
    load_graph_run_view_with(access, actor_did, run_id).await
}

fn collection_result_projection_fields(version: &Value, collection: &str) -> Result<Vec<String>> {
    let fields = version
        .get("Fields")
        .and_then(Value::as_array)
        .with_context(|| format!("collection {collection} version has no Fields array"))?;
    let mut names = fields
        .iter()
        // Relation fields need a nested GraphQL selection. Result hydration
        // deliberately projects every scalar/array value and leaves relation
        // traversal to an explicitly declared result document instead of
        // inventing an unbounded recursive projection.
        .filter(|field| field.get("RelationName").is_none_or(Value::is_null))
        .filter_map(|field| field.get("Name").and_then(Value::as_str))
        .filter(|name| *name != "_docID" && *name != "_version")
        .map(|name| {
            crate::graphql::validate_graphql_name(name)?;
            Ok(name.to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    names.sort();
    names.dedup();
    Ok(names)
}

async fn hydrate_terminal_result_documents(
    access: &ConfigAccess,
    view: &mut GraphRunView,
) -> Result<()> {
    let mut fields_by_collection = BTreeMap::<String, Vec<String>>::new();
    for result in view.results.iter_mut().filter(|result| result.terminal) {
        let persisted_refs = view
            .persisted_result_refs
            .iter()
            .filter(|reference| reference.name == result.name)
            .cloned()
            .collect::<Vec<_>>();
        let mut refs = if persisted_refs.is_empty() {
            result.refs.clone()
        } else {
            persisted_refs
        };
        refs.sort_by(|left, right| left.document_id.cmp(&right.document_id));

        let mut documents = Vec::with_capacity(refs.len());
        for reference in &refs {
            validate_collection_identifier(&reference.collection)?;
            let fields = if let Some(fields) = fields_by_collection.get(&reference.collection) {
                fields.clone()
            } else {
                let version = access
                    .collection_version(&reference.collection)
                    .await?
                    .with_context(|| {
                        format!("result collection {} is unavailable", reference.collection)
                    })?;
                let fields = collection_result_projection_fields(&version, &reference.collection)?;
                fields_by_collection.insert(reference.collection.clone(), fields.clone());
                fields
            };
            let selected_fields = if fields.is_empty() {
                String::new()
            } else {
                format!(" {}", fields.join(" "))
            };
            let cids = graphql_string_list_literal(std::slice::from_ref(&reference.commit_cid));
            let response = access
                .execute(&format!(
                    r#"{{
                        {collection}(
                            cid: {cids},
                            docID: "{document_id}",
                            showDeleted: true
                        ) {{ _docID _version {{ cid height fieldName }}{selected_fields} }}
                    }}"#,
                    collection = reference.collection,
                    document_id = escape_graphql_string(&reference.document_id),
                ))
                .await
                .with_context(|| {
                    format!(
                        "reconstructing result {} document {} at {}",
                        result.name, reference.document_id, reference.commit_cid
                    )
                })?;
            let rows = rows(&response, &reference.collection);
            if rows.len() != 1 {
                anyhow::bail!(
                    "result {} document {} at {} reconstructed {} rows",
                    result.name,
                    reference.document_id,
                    reference.commit_cid,
                    rows.len()
                );
            }
            let mut document = rows[0].clone();
            if document.get("_docID").and_then(Value::as_str)
                != Some(reference.document_id.as_str())
            {
                anyhow::bail!(
                    "result {} commit {} reconstructed the wrong document",
                    result.name,
                    reference.commit_cid
                );
            }
            let reconstructed = document_composite_version(
                &document,
                &format!("hydrate graph result {}", result.name),
            )?
            .context("historical result document has no composite commit")?;
            if reconstructed.cid != reference.commit_cid {
                anyhow::bail!(
                    "result {} document {} reconstructed commit {}, expected {}",
                    result.name,
                    reference.document_id,
                    reconstructed.cid,
                    reference.commit_cid
                );
            }
            if let Some(object) = document.as_object_mut() {
                object.remove("_version");
            }
            documents.push(document);
        }
        result.refs = refs;
        result.observed_count = documents.len();
        result.documents = documents;
    }
    Ok(())
}

/// Reconstruct a graph run and hydrate each terminal output from the exact
/// document commits pinned when the run succeeded. This is the shared result
/// projection for CLI and bridge consumers; it does not create a UI-only
/// execution or result model.
pub async fn load_graph_run_result_view_with_access(
    access: &ConfigAccess,
    actor_did: &str,
    run_id: &str,
) -> Result<GraphRunView> {
    let mut view = load_graph_run_view_with(access, actor_did, run_id).await?;
    hydrate_terminal_result_documents(access, &mut view).await?;
    Ok(view)
}

/// Reconcile every running graph owned by one principal. The durable run and
/// its pinned revision remain the only source of truth; this sweep merely
/// applies the already-modeled terminal transition when evidence is ready.
pub async fn reconcile_owned_graph_runs(node: &EmbeddedNode, owner_did: &str) -> Result<usize> {
    let response = node
        .execute(&format!(
            r#"{{ GraphRun(filter: {{ owner_did: {{ _eq: "{}" }}, status: {{ _eq: "running" }} }}, limit: 1001) {{ run_id }} }}"#,
            escape_graphql_string(owner_did),
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!("query running graph runs failed: {:?}", response.errors);
    }
    let data = json!({ "data": response.data.unwrap_or(Value::Null) });
    let running = rows(&data, "GraphRun");
    if running.len() > 1_000 {
        anyhow::bail!("running graph run sweep exceeds the bounded 1000-run limit");
    }

    let mut terminalized = 0;
    for row in running {
        let Some(run_id) = row.get("run_id").and_then(Value::as_str) else {
            tracing::warn!("running GraphRun row is missing run_id");
            continue;
        };
        match reconcile_graph_run(node, None, owner_did, run_id).await {
            Ok(view) if view.is_terminal() => terminalized += 1,
            Ok(_) => {}
            Err(error) => tracing::warn!(
                run_id,
                %error,
                "graph run reconciliation failed; retrying on the next daemon tick"
            ),
        }
    }
    Ok(terminalized)
}

/// Own graph completion in the daemon rather than in a CLI/UI observer.
pub async fn run_graph_run_reconciler(
    node: Arc<EmbeddedNode>,
    owner_did: String,
    cancel: CancellationToken,
) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => {
                match reconcile_owned_graph_runs(node.as_ref(), &owner_did).await {
                    Ok(terminalized) if terminalized > 0 => tracing::info!(
                        owner_did,
                        terminalized,
                        "materialized durable graph run completion"
                    ),
                    Ok(_) => {}
                    Err(error) => tracing::warn!(
                        owner_did,
                        %error,
                        "graph run reconciliation sweep failed; retrying"
                    ),
                }
            }
        }
    }
}

async fn commit_terminal(
    node: &EmbeddedNode,
    identity: Option<Did>,
    view: &GraphRunView,
    status: &str,
) -> Result<()> {
    let txn = ConfigApplyTxn::begin_local(node, identity).await?;
    commit_terminal_txn(txn, view, status).await
}

async fn commit_terminal_with_access(
    access: &ConfigAccess,
    view: &GraphRunView,
    status: &str,
) -> Result<()> {
    let txn = access.begin_apply_txn().await?;
    commit_terminal_txn(txn, view, status).await
}

/// Capture inside the existing GraphRun transaction before causing sibling
/// interruptions. A losing generation writes nothing and emits no interrupts.
async fn capture_failure_txn(txn: ConfigApplyTxn<'_>, view: &GraphRunView) -> Result<()> {
    let result = async {
        let fresh = load_graph_run_view_with(&txn, &view.owner_did, &view.run_id).await?;
        if fresh.status != "running" || fresh.update_generation != view.update_generation {
            anyhow::bail!("graph failure capture CAS lost; reload the durable run");
        }
        if fresh.cancellation_requested_at.is_some() || fresh.error.is_some() {
            return Ok(());
        }
        let Some(primary) = fresh.failure_evidence else {
            return Ok(());
        };
        let current = query_run(&txn, &view.run_id).await?;
        let input = json!({
            "error": serde_json::to_string(&primary)?,
            "update_generation": fresh.update_generation.checked_add(1).context("graph run generation exhausted")?,
        });
        txn.execute(&format!(
            "mutation {{ update_GraphRun(docID: \"{}\", input: {}) {{ _docID }} }}",
            escape_graphql_string(required_string(&current, "_docID")?),
            graphql_input_literal(&input)?,
        ))
        .await?;
        Result::<()>::Ok(())
    }
    .await;
    match result {
        Ok(()) => txn.commit().await.context("commit graph failure cause"),
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

async fn commit_terminal_txn(
    txn: ConfigApplyTxn<'_>,
    view: &GraphRunView,
    status: &str,
) -> Result<()> {
    let result = async {
        // The terminal predicate depends on AgentRequest, group-state, and
        // result documents as well as GraphRun. Rebuild that complete view in
        // the same transaction that writes terminal state so a concurrent
        // graph materialization conflicts instead of committing stale success.
        let fresh = load_graph_run_view_with(&txn, &view.owner_did, &view.run_id).await?;
        if fresh.status != "running" || fresh.update_generation != view.update_generation {
            anyhow::bail!("graph run terminal CAS lost; reload the durable run");
        }
        let decision = graph_run_terminal_decision(
            &fresh.status,
            fresh.cancellation_requested_at.is_some(),
            fresh.result_contract_satisfied
                && fresh.terminal_stages_completed
                && fresh.outstanding_invocation_count == 0,
            fresh.active_request_count == 0,
            fresh.failure_evidence.is_some(),
        );
        let allowed = match status {
            "succeeded" => decision.may_succeed,
            "failed" => decision.may_fail,
            "cancelled" => decision.may_cancel,
            _ => false,
        };
        if !allowed {
            anyhow::bail!("graph run terminal transition is not legal");
        }
        let current = query_run(&txn, &view.run_id).await?;
        let current_generation = fresh.update_generation;
        let doc_id = required_string(&current, "_docID")?;
        let result_refs = (status == "succeeded").then(|| fresh.successful_result_refs());
        let error = (status == "failed")
            .then_some(fresh.failure_evidence.as_ref())
            .flatten();
        let input = json!({
            "status": status,
            "error": error.map(serde_json::to_string).transpose()?,
            "result_refs_json": result_refs.as_ref().map(serde_json::to_string).transpose()?,
            "update_generation": current_generation.checked_add(1).context("graph run generation exhausted")?,
            "completed_at": chrono::Utc::now().to_rfc3339(),
        });
        let mutation = format!(
            "mutation {{ update_GraphRun(docID: \"{}\", input: {}) {{ _docID }} }}",
            escape_graphql_string(doc_id),
            graphql_input_literal(&input)?,
        );
        txn.execute(&mutation).await?;
        Result::<()>::Ok(())
    }
    .await;
    match result {
        Ok(()) => txn.commit().await.context("commit GraphRun terminal CAS"),
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

fn terminal_projection(
    view: &GraphRunView,
) -> Option<(&'static str, Option<&Value>, Option<Vec<GraphResultRef>>)> {
    if view.cancellation_requested_at.is_some() && view.active_request_count == 0 {
        Some(("cancelled", None, None))
    } else if let Some(error) = view
        .failure_evidence
        .as_ref()
        .filter(|_| view.active_request_count == 0)
    {
        Some(("failed", Some(error), None))
    } else if !view.requests.is_empty()
        && view.active_request_count == 0
        && view.result_contract_satisfied
        && view.terminal_stages_completed
        && view.outstanding_invocation_count == 0
    {
        Some(("succeeded", None, Some(view.successful_result_refs())))
    } else {
        None
    }
}

async fn interrupt_active_requests(
    node: &EmbeddedNode,
    requests: &[GraphRunRequestView],
) -> Result<()> {
    for request in requests {
        if !request.terminal && !request.request_id.is_empty() {
            crate::interrupt_request(node, &request.request_id).await?;
        }
    }
    Ok(())
}

fn should_interrupt_active_work(view: &GraphRunView) -> bool {
    view.active_request_count > 0
        && (view.cancellation_requested_at.is_some() || view.error.is_some())
}

fn needs_failure_capture(view: &GraphRunView) -> bool {
    view.active_request_count > 0
        && view.cancellation_requested_at.is_none()
        && view.error.is_none()
        && view.failure_evidence.is_some()
}

/// Re-evaluate durable run state and, when a terminal predicate holds, race
/// the single terminal transaction. A losing reconciler reloads on its next
/// pass; no completion-owner lock exists.
pub async fn reconcile_graph_run(
    node: &EmbeddedNode,
    identity: Option<Did>,
    actor_did: &str,
    run_id: &str,
) -> Result<GraphRunView> {
    let mut view = load_graph_run_view(node, actor_did, run_id).await?;
    if view.is_terminal() {
        return Ok(view);
    }
    if needs_failure_capture(&view) {
        let txn = ConfigApplyTxn::begin_local(node, identity.clone()).await?;
        capture_failure_txn(txn, &view).await?;
        view = load_graph_run_view(node, actor_did, run_id).await?;
        if view.is_terminal() {
            return Ok(view);
        }
    }
    if should_interrupt_active_work(&view) {
        interrupt_active_requests(node, &view.requests).await?;
    }
    let terminal = terminal_projection(&view);
    if let Some((status, _, _)) = terminal {
        if let Err(commit_error) = commit_terminal(node, identity, &view, status).await {
            let reloaded = load_graph_run_view(node, actor_did, run_id).await?;
            if reloaded.is_terminal() {
                return Ok(reloaded);
            }
            return Err(commit_error);
        }
        return load_graph_run_view(node, actor_did, run_id).await;
    }
    Ok(view)
}

/// Reconcile through the same local-or-GraphQL access used by operator
/// clients. Terminal state is still committed by the GraphRun CAS above.
pub async fn reconcile_graph_run_with_access(
    access: &ConfigAccess,
    actor_did: &str,
    run_id: &str,
) -> Result<GraphRunView> {
    let mut view = load_graph_run_view_with_access(access, actor_did, run_id).await?;
    if view.is_terminal() {
        return Ok(view);
    }
    if needs_failure_capture(&view) {
        capture_failure_txn(access.begin_apply_txn().await?, &view).await?;
        view = load_graph_run_view_with_access(access, actor_did, run_id).await?;
        if view.is_terminal() {
            return Ok(view);
        }
    }
    if should_interrupt_active_work(&view) {
        for request in &view.requests {
            if !request.terminal && !request.request_id.is_empty() {
                interrupt_request_with_access(access, &request.request_id).await?;
            }
        }
    }
    if let Some((status, _, _)) = terminal_projection(&view) {
        if let Err(commit_error) = commit_terminal_with_access(access, &view, status).await {
            let reloaded = load_graph_run_view_with_access(access, actor_did, run_id).await?;
            if reloaded.is_terminal() {
                return Ok(reloaded);
            }
            return Err(commit_error);
        }
        return load_graph_run_view_with_access(access, actor_did, run_id).await;
    }
    Ok(view)
}

/// Persist cancellation intent, then reuse the ordinary request interrupt
/// path for every currently correlated request. Recovery can repeat this
/// operation until the run has no active work.
pub async fn request_graph_run_cancellation(
    node: &EmbeddedNode,
    identity: Option<Did>,
    actor_did: &str,
    run_id: &str,
    reason: Option<&str>,
) -> Result<GraphRunView> {
    if reason.is_some_and(|reason| reason.len() > MAX_CANCEL_REASON_BYTES) {
        anyhow::bail!("graph cancellation reason exceeds {MAX_CANCEL_REASON_BYTES} bytes");
    }
    let before = load_graph_run_view(node, actor_did, run_id).await?;
    if before.is_terminal() {
        return Ok(before);
    }
    let txn = ConfigApplyTxn::begin_local(node, identity).await?;
    persist_cancellation_intent(txn, actor_did, run_id, reason).await?;
    interrupt_active_requests(node, &before.requests).await?;
    reconcile_graph_run(node, None, actor_did, run_id).await
}

async fn persist_cancellation_intent(
    txn: ConfigApplyTxn<'_>,
    actor_did: &str,
    run_id: &str,
    reason: Option<&str>,
) -> Result<()> {
    let result = async {
        let current = query_run(&txn, run_id).await?;
        if required_string(&current, "status")? != "running" {
            return Ok(());
        }
        if current
            .get("cancel_requested_at")
            .and_then(Value::as_str)
            .is_some()
        {
            return Ok(());
        }
        let doc_id = required_string(&current, "_docID")?;
        let generation = current
            .get("update_generation")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let input = json!({
            "cancel_requested_at": chrono::Utc::now().to_rfc3339(),
            "cancel_requested_by": actor_did,
            "cancel_reason": reason,
            "update_generation": generation.checked_add(1).context("graph run generation exhausted")?,
        });
        txn.execute(&format!(
            "mutation {{ update_GraphRun(docID: \"{}\", input: {}) {{ _docID }} }}",
            escape_graphql_string(doc_id),
            graphql_input_literal(&input)?,
        ))
        .await?;
        Result::<()>::Ok(())
    }
    .await;
    match result {
        Ok(()) => txn
            .commit()
            .await
            .context("commit graph cancellation intent")?,
        Err(error) => {
            let _ = txn.discard().await;
            return Err(error);
        }
    }
    Ok(())
}

async fn interrupt_request_with_access(access: &ConfigAccess, request_id: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{}" }} }},
                input: {{ interrupt_requested_at: "{}" }}
            ) {{ _docID }}
        }}"#,
        escape_graphql_string(request_id),
        escape_graphql_string(&now),
    );
    access.execute_committed(&mutation).await?;
    Ok(())
}

/// Persist cancellation and interrupt correlated work through the shared
/// client access. A running runtime observes the ordinary request latch.
pub async fn request_graph_run_cancellation_with_access(
    access: &ConfigAccess,
    actor_did: &str,
    run_id: &str,
    reason: Option<&str>,
) -> Result<GraphRunView> {
    if reason.is_some_and(|reason| reason.len() > MAX_CANCEL_REASON_BYTES) {
        anyhow::bail!("graph cancellation reason exceeds {MAX_CANCEL_REASON_BYTES} bytes");
    }
    let before = load_graph_run_view_with_access(access, actor_did, run_id).await?;
    if before.is_terminal() {
        return Ok(before);
    }
    let txn = access.begin_apply_txn().await?;
    persist_cancellation_intent(txn, actor_did, run_id, reason).await?;
    for request in &before.requests {
        if !request.terminal && !request.request_id.is_empty() {
            interrupt_request_with_access(access, &request.request_id).await?;
        }
    }
    reconcile_graph_run_with_access(access, actor_did, run_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_view(failure_evidence: Option<Value>) -> GraphRunView {
        GraphRunView {
            view_version: GRAPH_RUN_VIEW_VERSION,
            run_id: "run".to_owned(),
            graph_id: "graph".to_owned(),
            revision_digest: "sha256:revision".to_owned(),
            owner_did: "did:key:owner".to_owned(),
            caller_did: "did:key:owner".to_owned(),
            entry_name: "review".to_owned(),
            correlation: "run".to_owned(),
            status: "running".to_owned(),
            input: json!({}),
            cancellation_requested_at: None,
            cancellation_requested_by: None,
            cancellation_reason: None,
            error: None,
            created_at: "2026-08-25T00:00:00Z".to_owned(),
            started_at: Some("2026-08-25T00:00:00Z".to_owned()),
            deadline_at: Some("2026-08-25T02:00:00+00:00".to_owned()),
            completed_at: None,
            update_generation: 0,
            requests: vec![GraphRunRequestView {
                request_id: "request".to_owned(),
                session_id: Some("session".to_owned()),
                node_id: Some("terminal".to_owned()),
                behavior_id: "behavior".to_owned(),
                lifecycle_state: Some("completed".to_owned()),
                failure_reason: None,
                terminal: true,
                succeeded: true,
            }],
            stages: vec![],
            groups: vec![],
            results: vec![],
            persisted_result_refs: vec![],
            active_request_count: 0,
            terminal_request_count: 1,
            outstanding_invocation_count: 0,
            terminal_stages_completed: true,
            result_contract_satisfied: false,
            failure_evidence,
        }
    }

    #[test]
    fn terminal_projection_requires_and_commits_proven_failure_evidence() {
        let pending = terminal_view(None);
        assert!(terminal_projection(&pending).is_none());

        let evidence = json!({
            "version": 1,
            "code": "result_contract_unsatisfied",
            "message": "terminal graph stages completed but the declared result contract is unsatisfied",
        });
        let failed = terminal_view(Some(evidence.clone()));
        let projected = terminal_projection(&failed).expect("proven failure must terminalize");
        assert_eq!(projected.0, "failed");
        assert_eq!(projected.1, Some(&evidence));
        assert!(projected.2.is_none());

        let mut active = failed;
        active.active_request_count = 1;
        assert!(
            terminal_projection(&active).is_none(),
            "failure must wait for correlated work to quiesce"
        );
        assert!(needs_failure_capture(&active));
        assert!(!should_interrupt_active_work(&active));
        active.error = Some(evidence);
        assert!(!needs_failure_capture(&active));
        assert!(should_interrupt_active_work(&active));
    }

    #[test]
    fn durable_started_at_derives_a_restart_stable_run_deadline() {
        let deadline = graph_run_deadline(Some("2026-08-25T00:00:00Z"), 7_200)
            .unwrap()
            .expect("deadline");
        assert_eq!(deadline.to_rfc3339(), "2026-08-25T02:00:00+00:00");
        assert!(graph_run_deadline(Some("not-a-time"), 7_200).is_err());
    }
}
