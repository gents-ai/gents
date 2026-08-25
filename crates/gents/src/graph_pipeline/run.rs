use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::graphql::graphql_input_literal;
use identity::Did;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::{escape_graphql_string, validate_collection_identifier};

use super::runtime::graph_trigger_id;
use super::{
    graph_run_terminal_decision, verify_graph_plan_digest, GraphPlan, PlannedResult,
    ResultCardinality, ResultPredicate,
};

const GRAPH_RUN_VIEW_VERSION: u32 = 1;
const MAX_CANCEL_REASON_BYTES: usize = 1_024;

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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphRunRequestView {
    pub request_id: String,
    pub node_id: Option<String>,
    pub behavior_id: String,
    pub status: String,
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
    pub completed_at: Option<String>,
    pub update_generation: i64,
    pub requests: Vec<GraphRunRequestView>,
    pub stages: Vec<GraphRunStageView>,
    pub groups: Vec<GraphRunGroupView>,
    pub results: Vec<GraphRunResultView>,
    pub persisted_result_refs: Vec<GraphResultRef>,
    pub active_request_count: usize,
    pub terminal_request_count: usize,
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

fn request_is_terminal(state: Option<&str>, status: &str) -> bool {
    matches!(
        state.unwrap_or(status),
        "completed" | "failed" | "dead" | "interrupted" | "superseded"
    )
}

fn request_succeeded(state: Option<&str>, status: &str) -> bool {
    state.unwrap_or(status) == "completed" && matches!(status, "complete" | "completed")
}

async fn load_requests(
    executor: &(impl GraphRunQuery + ?Sized),
    correlation: &str,
    plan: &GraphPlan,
) -> Result<Vec<GraphRunRequestView>> {
    let limit = usize::try_from(plan.limits.max_total_invocations)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let response = executor
        .execute_graph_query(&format!(
            r#"{{
                AgentRequest(
                    filter: {{ caused_by_correlation: {{ _eq: "{}" }} }},
                    order: {{ created_at: ASC }}, limit: {limit}
                ) {{
                    request_id behavior_id caused_by_trigger_id status lifecycle_state failure_reason
                }}
            }}"#,
            escape_graphql_string(correlation),
        ))
        .await?;
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
    let mut requests = rows(&response, "AgentRequest")
        .iter()
        .map(|row| {
            let status = row
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let lifecycle_state = row
                .get("lifecycle_state")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let behavior_id = row
                .get("behavior_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let caused_by_trigger_id = row.get("caused_by_trigger_id").and_then(Value::as_str);
            GraphRunRequestView {
                request_id: row
                    .get("request_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                node_id: caused_by_trigger_id
                    .and_then(|trigger_id| nodes_by_trigger.get(trigger_id))
                    .cloned(),
                behavior_id,
                terminal: request_is_terminal(lifecycle_state.as_deref(), &status),
                succeeded: request_succeeded(lifecycle_state.as_deref(), &status),
                status,
                lifecycle_state,
                failure_reason: row
                    .get("failure_reason")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            }
        })
        .collect::<Vec<_>>();
    requests.sort_by(|left, right| left.request_id.cmp(&right.request_id));
    Ok(requests)
}

async fn load_groups(
    executor: &(impl GraphRunQuery + ?Sized),
    correlation: &str,
) -> Result<Vec<GraphRunGroupView>> {
    let response = executor
        .execute_graph_query(&format!(
            r#"{{
                EventTriggerGroupState(
                    filter: {{ correlation: {{ _eq: "{}" }} }},
                    order: {{ first_seen_at: ASC }}
                ) {{ group_key trigger_id first_seen_at quiesced_at quiesced_reason }}
            }}"#,
            escape_graphql_string(correlation),
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

fn result_fields(result: &PlannedResult, all_results: &[PlannedResult]) -> Result<Vec<String>> {
    let mut fields = BTreeSet::new();
    for predicate in &result.predicates {
        match predicate {
            ResultPredicate::Distinct { field }
            | ResultPredicate::AllEqual { field }
            | ResultPredicate::CountEqualsField { field }
            | ResultPredicate::AllMatch { field, .. }
            | ResultPredicate::FieldEqualsResultCount { field, .. } => {
                fields.insert(field.clone());
            }
            ResultPredicate::SameMembers { field, .. }
            | ResultPredicate::SubsetOf { field, .. }
            | ResultPredicate::FieldEqualsField { field, .. } => {
                fields.insert(field.clone());
            }
            ResultPredicate::FieldEqualsSum { field, terms } => {
                fields.insert(field.clone());
                fields.extend(terms.iter().cloned());
            }
        }
    }
    for other in all_results {
        for predicate in &other.predicates {
            match predicate {
                ResultPredicate::SameMembers {
                    result: target,
                    result_field,
                    ..
                }
                | ResultPredicate::SubsetOf {
                    result: target,
                    result_field,
                    ..
                }
                | ResultPredicate::FieldEqualsField {
                    result: target,
                    result_field,
                    ..
                } if target == &result.name => {
                    fields.insert(result_field.clone());
                }
                _ => {}
            }
        }
    }
    for field in &fields {
        crate::graphql::validate_graphql_name(field)?;
    }
    Ok(fields.into_iter().collect())
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

fn scalar_key(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(format!("s:{value}")),
        Value::Number(value) => Some(format!("n:{value}")),
        Value::Bool(value) => Some(format!("b:{value}")),
        _ => None,
    }
}

fn scalar_usize(value: &Value) -> Option<usize> {
    match value {
        Value::String(value) => value.parse().ok(),
        Value::Number(value) => value.as_u64().and_then(|value| usize::try_from(value).ok()),
        _ => None,
    }
}

fn scalar_values(docs: &[Value], field: &str) -> Option<Vec<String>> {
    docs.iter()
        .map(|doc| doc.get(field).and_then(scalar_key))
        .collect()
}

fn member_set(docs: &[Value], field: &str) -> Option<BTreeSet<String>> {
    Some(scalar_values(docs, field)?.into_iter().collect())
}

fn predicate_violation(
    result: &PlannedResult,
    docs: &[Value],
    all_docs: &BTreeMap<&str, &[Value]>,
) -> Option<String> {
    for predicate in &result.predicates {
        let valid = match predicate {
            ResultPredicate::Distinct { field } => scalar_values(docs, field)
                .is_some_and(|values| values.iter().collect::<BTreeSet<_>>().len() == values.len()),
            ResultPredicate::AllEqual { field } => scalar_values(docs, field)
                .is_some_and(|values| values.windows(2).all(|pair| pair[0] == pair[1])),
            ResultPredicate::CountEqualsField { field } => docs
                .iter()
                .all(|doc| doc.get(field).and_then(scalar_usize) == Some(docs.len())),
            ResultPredicate::AllMatch { field, value } => {
                scalar_values(docs, field).is_some_and(|values| {
                    values
                        .iter()
                        .all(|observed| observed == &format!("s:{value}"))
                })
            }
            ResultPredicate::SameMembers {
                field,
                result,
                result_field,
            } => all_docs
                .get(result.as_str())
                .and_then(|other| {
                    Some((member_set(docs, field)?, member_set(other, result_field)?))
                })
                .is_some_and(|(left, right)| left == right),
            ResultPredicate::SubsetOf {
                field,
                result,
                result_field,
            } => all_docs
                .get(result.as_str())
                .and_then(|other| {
                    Some((member_set(docs, field)?, member_set(other, result_field)?))
                })
                .is_some_and(|(left, right)| left.is_subset(&right)),
            ResultPredicate::FieldEqualsResultCount { field, result } => {
                all_docs.get(result.as_str()).is_some_and(|other| {
                    docs.iter()
                        .all(|doc| doc.get(field).and_then(scalar_usize) == Some(other.len()))
                })
            }
            ResultPredicate::FieldEqualsSum { field, terms } => docs.iter().all(|doc| {
                let Some(expected) = doc.get(field).and_then(scalar_usize) else {
                    return false;
                };
                terms
                    .iter()
                    .map(|term| doc.get(term).and_then(scalar_usize))
                    .try_fold(0usize, |sum, value| Some(sum.checked_add(value?)?))
                    == Some(expected)
            }),
            ResultPredicate::FieldEqualsField {
                field,
                result,
                result_field,
            } => {
                let other = all_docs.get(result.as_str()).copied().unwrap_or_default();
                docs.len() == 1
                    && other.len() == 1
                    && docs[0].get(field).and_then(scalar_key)
                        == other[0].get(result_field).and_then(scalar_key)
            }
        };
        if !valid {
            return Some(format!("result predicate {predicate:?} failed"));
        }
    }
    None
}

struct LoadedResult {
    plan: PlannedResult,
    docs: Vec<Value>,
    view: GraphRunResultView,
}

async fn load_result(
    executor: &(impl GraphRunQuery + ?Sized),
    correlation: &str,
    result: &PlannedResult,
    all_results: &[PlannedResult],
) -> Result<LoadedResult> {
    validate_collection_identifier(&result.collection)?;
    crate::graphql::validate_graphql_name(&result.correlation_field)?;
    let fields = result_fields(result, all_results)?;
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
                ) {{ _docID _version {{ cid height fieldName }} {fields} }}
            }}"#,
            collection = result.collection,
            correlation_field = result.correlation_field,
            correlation = escape_graphql_string(correlation),
            fields = fields.join(" "),
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
    Ok(LoadedResult {
        plan: result.clone(),
        docs,
        view: GraphRunResultView {
            name: result.name.clone(),
            terminal: result.terminal,
            satisfied: violation.is_none(),
            observed_count: refs.len(),
            violation,
            refs,
        },
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
    let requests = load_requests(executor, correlation, &plan).await?;
    let groups = load_groups(executor, correlation).await?;
    let mut loaded_results = Vec::with_capacity(plan.results.len());
    for result in &plan.results {
        loaded_results.push(load_result(executor, correlation, result, &plan.results).await?);
    }
    let all_docs = loaded_results
        .iter()
        .map(|result| (result.plan.name.as_str(), result.docs.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let predicate_violations = loaded_results
        .iter()
        .map(|result| predicate_violation(&result.plan, &result.docs, &all_docs))
        .collect::<Vec<_>>();
    drop(all_docs);
    for (result, violation) in loaded_results.iter_mut().zip(predicate_violations) {
        if result.view.violation.is_none() {
            result.view.violation = violation;
        }
        result.view.satisfied = result.view.violation.is_none();
    }
    let mut results = loaded_results
        .into_iter()
        .map(|result| result.view)
        .collect::<Vec<_>>();
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
    let failed_request = requests
        .iter()
        .find(|request| request.terminal && !request.succeeded);
    let over_limit = requests.len() > plan.limits.max_total_invocations as usize;
    let invalid_group = groups.iter().find(|group| group.quiesced_at.is_some());
    let failure_evidence = if let Some(group) = invalid_group {
        Some(json!({
            "version": 1, "code": "group_quiesced",
            "message": group.quiesced_reason.as_deref().unwrap_or("required graph group was quiesced"),
            "group_key": group.group_key, "trigger_id": group.trigger_id,
        }))
    } else if let Some(request) = unknown_request {
        Some(json!({
            "version": 1, "code": "contract_drift",
            "message": "correlated request behavior is not in the pinned graph plan",
            "request_id": request.request_id, "behavior_id": request.behavior_id,
        }))
    } else if over_limit {
        Some(json!({
            "version": 1, "code": "invocation_limit_exceeded",
            "message": "correlated request count exceeds the pinned graph limit",
            "observed": requests.len(), "limit": plan.limits.max_total_invocations,
        }))
    } else {
        failed_request.map(|request| {
            json!({
                "version": 1, "code": "required_request_failed",
                "message": request.failure_reason.as_deref().unwrap_or("required graph request did not complete successfully"),
                "request_id": request.request_id,
                "lifecycle_state": request.lifecycle_state,
            })
        })
    };
    let terminal_results = results
        .iter()
        .filter(|result| result.terminal)
        .collect::<Vec<_>>();
    let result_contract_satisfied =
        !terminal_results.is_empty() && results.iter().all(|result| result.satisfied);
    let persisted_result_refs = run
        .get("result_refs_json")
        .and_then(Value::as_str)
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    let error = run
        .get("error")
        .and_then(Value::as_str)
        .map(serde_json::from_str)
        .transpose()?;

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
        started_at: run
            .get("started_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
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
        result_contract_satisfied,
        failure_evidence,
    })
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
    error: Option<&Value>,
    result_refs: Option<&[GraphResultRef]>,
) -> Result<()> {
    let txn = ConfigApplyTxn::begin_local(node, identity).await?;
    commit_terminal_txn(txn, view, status, error, result_refs).await
}

async fn commit_terminal_with_access(
    access: &ConfigAccess,
    view: &GraphRunView,
    status: &str,
    error: Option<&Value>,
    result_refs: Option<&[GraphResultRef]>,
) -> Result<()> {
    let txn = access.begin_apply_txn().await?;
    commit_terminal_txn(txn, view, status, error, result_refs).await
}

async fn commit_terminal_txn(
    txn: ConfigApplyTxn<'_>,
    view: &GraphRunView,
    status: &str,
    error: Option<&Value>,
    result_refs: Option<&[GraphResultRef]>,
) -> Result<()> {
    let result = async {
        let current = query_run(&txn, &view.run_id).await?;
        let current_status = required_string(&current, "status")?;
        let current_generation = current
            .get("update_generation")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        if current_status != "running" || current_generation != view.update_generation {
            anyhow::bail!("graph run terminal CAS lost; reload the durable run");
        }
        let decision = graph_run_terminal_decision(
            current_status,
            current
                .get("cancel_requested_at")
                .and_then(Value::as_str)
                .is_some(),
            view.result_contract_satisfied,
            view.active_request_count == 0,
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
        let doc_id = required_string(&current, "_docID")?;
        let input = json!({
            "status": status,
            "error": error.map(serde_json::to_string).transpose()?,
            "result_refs_json": result_refs.map(serde_json::to_string).transpose()?,
            "update_generation": current_generation.saturating_add(1),
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
    } else if let Some(error) = view.failure_evidence.as_ref() {
        Some(("failed", Some(error), None))
    } else if !view.requests.is_empty()
        && view.active_request_count == 0
        && view.result_contract_satisfied
    {
        Some(("succeeded", None, Some(view.successful_result_refs())))
    } else {
        None
    }
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
    let view = load_graph_run_view(node, actor_did, run_id).await?;
    if view.is_terminal() {
        return Ok(view);
    }
    let terminal = terminal_projection(&view);
    if let Some((status, error, refs)) = terminal {
        if let Err(commit_error) =
            commit_terminal(node, identity, &view, status, error, refs.as_deref()).await
        {
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
    let view = load_graph_run_view_with_access(access, actor_did, run_id).await?;
    if view.is_terminal() {
        return Ok(view);
    }
    if let Some((status, error, refs)) = terminal_projection(&view) {
        if let Err(commit_error) =
            commit_terminal_with_access(access, &view, status, error, refs.as_deref()).await
        {
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
    for request in &before.requests {
        if !request.terminal && !request.request_id.is_empty() {
            crate::interrupt_request(node, &request.request_id).await?;
        }
    }
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
            "update_generation": generation.saturating_add(1),
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
    use crate::graph_pipeline::PortRef;

    fn result(name: &str, predicates: Vec<ResultPredicate>) -> PlannedResult {
        PlannedResult {
            name: name.to_owned(),
            from: PortRef {
                node_id: "node".to_owned(),
                port: name.to_owned(),
            },
            collection: format!("{name}Collection"),
            schema: format!("{name}/v1"),
            correlation_field: "run_id".to_owned(),
            cardinality: ResultCardinality::AtMost { count: 8 },
            terminal: false,
            predicates,
        }
    }

    #[test]
    fn generic_result_predicates_close_cross_collection_ledgers() {
        let areas = vec![
            json!({"area_id": "a", "expected_total": "2"}),
            json!({"area_id": "b", "expected_total": "2"}),
        ];
        let scans = vec![
            json!({"area_id": "b", "expected_total": "2"}),
            json!({"area_id": "a", "expected_total": "2"}),
        ];
        let candidates = vec![json!({"finding_id": "f1"}), json!({"finding_id": "f2"})];
        let findings = vec![json!({"finding_id": "f1", "verdict": "confirmed"})];
        let summary = vec![json!({
            "candidate_count": "2", "confirmed_count": "1", "refuted_count": "1"
        })];
        let all_docs = BTreeMap::from([
            ("areas", areas.as_slice()),
            ("scans", scans.as_slice()),
            ("candidates", candidates.as_slice()),
            ("findings", findings.as_slice()),
            ("summary", summary.as_slice()),
        ]);

        let scans_contract = result(
            "scans",
            vec![
                ResultPredicate::Distinct {
                    field: "area_id".to_owned(),
                },
                ResultPredicate::CountEqualsField {
                    field: "expected_total".to_owned(),
                },
                ResultPredicate::SameMembers {
                    field: "area_id".to_owned(),
                    result: "areas".to_owned(),
                    result_field: "area_id".to_owned(),
                },
            ],
        );
        assert_eq!(
            predicate_violation(&scans_contract, &scans, &all_docs),
            None
        );

        let summary_contract = result(
            "summary",
            vec![
                ResultPredicate::FieldEqualsResultCount {
                    field: "candidate_count".to_owned(),
                    result: "candidates".to_owned(),
                },
                ResultPredicate::FieldEqualsResultCount {
                    field: "confirmed_count".to_owned(),
                    result: "findings".to_owned(),
                },
                ResultPredicate::FieldEqualsSum {
                    field: "candidate_count".to_owned(),
                    terms: vec!["confirmed_count".to_owned(), "refuted_count".to_owned()],
                },
            ],
        );
        assert_eq!(
            predicate_violation(&summary_contract, &summary, &all_docs),
            None
        );

        let invalid = vec![json!({
            "candidate_count": "2", "confirmed_count": "1", "refuted_count": "0"
        })];
        assert!(predicate_violation(&summary_contract, &invalid, &all_docs).is_some());
    }
}
