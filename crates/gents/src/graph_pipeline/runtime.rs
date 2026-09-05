use std::collections::BTreeSet;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::graphql::{
    graphql_input_literal, graphql_rows_from_response, graphql_string_list_literal,
};
use identity::Did;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::config_client::{write_event_trigger_document, ConfigAccess, ConfigApplyTxn};
use crate::graphql::{escape_graphql_string, validate_collection_identifier};

use super::{
    verify_graph_plan_digest, DeliveryConcurrency, DeliveryMode, GraphPlan, GroupCount,
    PackageArtifactKind, PackagePlan, WorkspaceAuthorityCeiling,
};

const TRIGGER_PREFIX: &str = "graph-trigger-";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct MaterializedRevision {
    pub graph_id: String,
    pub digest: String,
    pub task_ids: Vec<String>,
    pub trigger_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PublishedGraph {
    pub graph_id: String,
    pub digest: String,
    pub trigger_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ActivationReceipt {
    pub graph_id: String,
    pub previous_digest: Option<String>,
    pub active_digest: String,
    pub generation: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct GraphRunReceipt {
    pub run_id: String,
    pub graph_id: String,
    pub revision_digest: String,
    pub entry_name: String,
    pub correlation: String,
    pub seed_doc_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevisionGateDecision {
    pub may_activate: bool,
    pub may_start: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphRunTerminalDecision {
    pub may_succeed: bool,
    pub may_fail: bool,
    pub may_cancel: bool,
}

/// Executable refinement of the Lean GraphRun terminal transition guards.
/// Persisted terminal writes still use a transaction/CAS; this pure decision
/// keeps every caller on the same legal-transition contract.
pub fn graph_run_terminal_decision(
    status: &str,
    cancellation_requested: bool,
    result_contract_satisfied: bool,
    active_work_terminal: bool,
    failure_proven: bool,
) -> GraphRunTerminalDecision {
    let running = status == "running";
    GraphRunTerminalDecision {
        may_succeed: running && result_contract_satisfied && active_work_terminal,
        may_fail: running && failure_proven && active_work_terminal,
        may_cancel: running && cancellation_requested && active_work_terminal,
    }
}

/// Executable refinement of the Lean publication gate for a single revision.
/// `activation_precondition_met` means the active pointer matched the
/// caller's compare-and-swap expectation;
/// `pointer_matches` means the active pointer selects this exact revision.
pub fn revision_gate_decision(
    status: &str,
    artifacts_complete: bool,
    activation_precondition_met: bool,
    pointer_matches: bool,
) -> RevisionGateDecision {
    RevisionGateDecision {
        may_activate: status == "validated" && artifacts_complete && activation_precondition_met,
        may_start: status == "active" && artifacts_complete && pointer_matches,
    }
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

fn component_hash(value: &str) -> String {
    let hex = format!("{:x}", Sha256::digest(value.as_bytes()));
    hex[..16].to_owned()
}

pub(crate) fn graph_trigger_id(digest: &str, route: &str) -> Result<String> {
    Ok(format!(
        "{TRIGGER_PREFIX}{}-{}",
        digest_hex(digest)?,
        component_hash(route)
    ))
}

/// Extract the immutable revision identity from the reserved artifact ID
/// namespace. Ordinary operator-authored Task/EventTrigger IDs return `None`.
pub(crate) fn graph_artifact_revision_digest(id: &str) -> Option<String> {
    let tail = id.strip_prefix(TRIGGER_PREFIX)?;
    let (digest, component) = tail.split_once('-')?;
    if digest.len() != 64
        || component.len() != 16
        || !digest
            .bytes()
            .chain(component.bytes())
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    Some(format!("sha256:{digest}"))
}

pub(crate) fn graph_artifact_is_reserved(id: &str) -> bool {
    id.starts_with(TRIGGER_PREFIX)
}

pub(crate) fn graph_artifact_is_visible(
    id: &str,
    active_digests: &std::collections::BTreeSet<String>,
) -> bool {
    if !graph_artifact_is_reserved(id) {
        return true;
    }
    graph_artifact_revision_digest(id).is_some_and(|digest| active_digests.contains(&digest))
}

fn verify_package_role_bindings(package: &PackagePlan, owner_did: &str) -> Result<()> {
    if package
        .roles
        .values()
        .any(|role| role.principal_did != owner_did)
    {
        anyhow::bail!("graph package v1 requires every logical role to bind the revision owner");
    }
    Ok(())
}

fn revision_visibility_authorized(status: &str, active_pointer: bool, run_pin: bool) -> bool {
    (active_pointer && status == "active") || (run_pin && matches!(status, "active" | "retired"))
}

/// Resolve package-owned ordinary configuration resources through the same
/// active-revision/nonterminal-run pin set used for Task/EventTrigger
/// visibility. The plan remains the only ownership ledger; no package catalog
/// or mutable install state is consulted at runtime.
pub(crate) async fn load_visible_package_artifact_ids(
    node: &EmbeddedNode,
    agent_did: &str,
    active_revision_digests: &BTreeSet<String>,
    pinned_revision_digests: &BTreeSet<String>,
) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let revision_digests = active_revision_digests
        .union(pinned_revision_digests)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut artifact_ids = BTreeSet::new();
    let mut ready_revision_digests = BTreeSet::new();
    for digest in &revision_digests {
        let active_pointer = active_revision_digests.contains(digest);
        let run_pin = pinned_revision_digests.contains(digest);
        match load_visible_package_artifact_ids_for_revision(
            node,
            agent_did,
            digest,
            active_pointer,
            run_pin,
        )
        .await
        {
            Ok(ids) => {
                ready_revision_digests.insert(digest.clone());
                artifact_ids.extend(ids);
            }
            Err(error) => {
                tracing::warn!(
                    revision_digest = %digest,
                    agent_did = %agent_did,
                    error = %error,
                    "graph package revision is not ready; excluding its artifacts from the runtime view"
                );
            }
        }
    }
    Ok((artifact_ids, ready_revision_digests))
}

async fn load_visible_package_artifact_ids_for_revision(
    node: &EmbeddedNode,
    agent_did: &str,
    digest: &str,
    active_pointer: bool,
    run_pin: bool,
) -> Result<BTreeSet<String>> {
    let response = node
        .execute(&format!(
            r#"{{
                    GraphRevision(filter: {{ digest: {{ _eq: "{}" }} }}, limit: 2) {{
                        owner_did status artifacts_complete plan_json
                    }}
                }}"#,
            escape_graphql_string(digest),
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "load graph package artifact visibility failed: {:?}",
            response.errors
        );
    }
    let data = json!({ "data": response.data.unwrap_or(Value::Null) });
    let revisions = rows(&data, "GraphRevision");
    if revisions.len() != 1 {
        anyhow::bail!("visible GraphRevision {digest:?} is missing or ambiguous");
    }
    let revision = &revisions[0];
    if revision.get("owner_did").and_then(Value::as_str) != Some(agent_did) {
        anyhow::bail!("visible GraphRevision {digest:?} is owned by another principal");
    }
    let status = revision
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !revision_visibility_authorized(status, active_pointer, run_pin) {
        anyhow::bail!(
            "visible GraphRevision {digest:?} status {status:?} is inconsistent with its active pointer or nonterminal run pin"
        );
    }
    if revision.get("artifacts_complete").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!("visible GraphRevision {digest:?} has incomplete artifacts");
    }
    let plan: GraphPlan = serde_json::from_str(
        revision
            .get("plan_json")
            .and_then(Value::as_str)
            .context("visible GraphRevision is missing plan_json")?,
    )?;
    if plan.digest != digest || !verify_graph_plan_digest(&plan) {
        anyhow::bail!("visible GraphRevision {digest:?} failed immutable identity verification");
    }
    let Some(package) = plan.package else {
        return Ok(BTreeSet::new());
    };
    verify_package_role_bindings(&package, agent_did)?;
    let txn = ConfigApplyTxn::begin_local(node, None).await?;
    let readiness = async {
        verify_package_schemas_in_txn(&txn, &package).await?;
        verify_package_artifacts_in_txn(&txn, &package).await
    }
    .await;
    let _ = txn.discard().await;
    readiness?;
    Ok(package
        .artifacts
        .into_iter()
        .map(|artifact| artifact.physical_id)
        .collect())
}

async fn verify_package_schemas_in_txn(
    txn: &ConfigApplyTxn<'_>,
    package: &PackagePlan,
) -> Result<()> {
    for schema in &package.required_schema_digests {
        if schema.collection_contract_digests.is_empty() {
            anyhow::bail!(
                "package schema {:?} has no pinned runtime contract",
                schema.namespace
            );
        }
        for (collection, expected_digest) in &schema.collection_contract_digests {
            let live = txn.collection_version(collection).await?.with_context(|| {
                format!(
                    "package schema {:?} collection {collection:?} is not locally ready",
                    schema.namespace
                )
            })?;
            let live_digest = crate::config_client::collection_schema_contract_digest(&live)?;
            if &live_digest != expected_digest {
                anyhow::bail!(
                    "package schema {:?} collection {collection:?} does not match its pinned contract: expected {expected_digest}, observed {live_digest}",
                    schema.namespace,
                );
            }
        }
    }
    Ok(())
}

async fn verify_package_artifacts_in_txn(
    txn: &ConfigApplyTxn<'_>,
    package: &PackagePlan,
) -> Result<()> {
    for artifact in &package.artifacts {
        let collection = match artifact.kind {
            PackageArtifactKind::Behavior => crate::Collection::AgentBehavior,
            PackageArtifactKind::ToolSelection => crate::Collection::ToolSelection,
            PackageArtifactKind::ToolSurface => crate::Collection::DatastoreToolSurface,
            PackageArtifactKind::Task => crate::Collection::Task,
            PackageArtifactKind::Trigger => {
                anyhow::bail!(
                    "package artifact ledger contains a revision-derived trigger in the desired-state resource set"
                )
            }
        };
        let live = crate::config_client::read_desired_state_document_in_txn(
            txn,
            collection,
            &artifact.physical_id,
        )
        .await?
        .with_context(|| {
            format!(
                "package artifact {:?} {:?} is missing",
                artifact.kind, artifact.physical_id
            )
        })?;
        let observed = crate::config_client::desired_state_document_digest(&live)?;
        if observed != artifact.content_digest {
            anyhow::bail!(
                "package artifact {:?} {:?} drifted: expected {}, observed {}",
                artifact.kind,
                artifact.physical_id,
                artifact.content_digest,
                observed
            );
        }
    }
    Ok(())
}

/// Guard the real trigger-to-AgentRequest materialization path for derived
/// graph triggers. Ordinary operator triggers return `Ok(None)`. A graph
/// trigger must carry a correlation resolving to a running, non-cancelled run
/// pinned to the trigger's exact revision; otherwise the returned reason is a
/// fail-closed skip.
pub(crate) async fn graph_materialization_denial(
    node: &EmbeddedNode,
    trigger_id: &str,
    correlation: Option<&str>,
) -> Result<Option<String>> {
    let Some(trigger_digest) = graph_artifact_revision_digest(trigger_id) else {
        return Ok(graph_artifact_is_reserved(trigger_id)
            .then(|| "malformed reserved graph trigger ID".to_owned()));
    };
    let Some(correlation) = correlation.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Some(
            "graph trigger materialization is missing controller-authored correlation".to_owned(),
        ));
    };
    let response = node
        .execute(&format!(
            r#"{{
                GraphRun(filter: {{ run_id: {{ _eq: "{}" }} }}, limit: 2) {{
                    revision_digest status cancel_requested_at error
                }}
            }}"#,
            escape_graphql_string(correlation),
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "query graph correlation guard failed: {:?}",
            response.errors
        );
    }
    let data = json!({ "data": response.data.unwrap_or(Value::Null) });
    Ok(graph_publication_denial(rows(&data, "GraphRun"), &trigger_digest).map(str::to_owned))
}

/// Preflight and transactional publication consume the same admission policy.
/// Only the transaction's read plus generation write authorizes publication.
fn graph_publication_denial(runs: &[Value], revision_digest: &str) -> Option<&'static str> {
    let [run] = runs else {
        return Some("graph publication requires exactly one durable run");
    };
    if run.get("revision_digest").and_then(Value::as_str) != Some(revision_digest) {
        return Some("graph publication revision does not match the pinned run");
    }
    if run.get("status").and_then(Value::as_str) != Some("running") {
        return Some("graph run is terminal");
    }
    if run
        .get("cancel_requested_at")
        .is_some_and(|value| !value.is_null())
    {
        return Some("graph run cancellation is requested");
    }
    if run.get("error").is_some_and(|value| !value.is_null()) {
        return Some("graph run has a definitive failure latch");
    }
    None
}

/// Root publication knows its immutable graph route before the request exists.
/// Continuations instead resolve their authenticated physical predecessor.
pub(crate) async fn fence_graph_root_request_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<()> {
    let Some(trigger_id) = request.caused_by_trigger_id.as_deref() else {
        return Ok(());
    };
    let Some(digest) = graph_artifact_revision_digest(trigger_id) else {
        anyhow::ensure!(
            !graph_artifact_is_reserved(trigger_id),
            "malformed reserved graph trigger ID"
        );
        return Ok(());
    };
    let run_id = request
        .caused_by_correlation
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context("graph root publication requires a durable run correlation")?;
    super::run::validate_graph_root_owner_in_txn(txn, request, run_id, &digest).await?;
    fence_graph_publication_in_txn(txn, run_id, &digest).await
}

/// Serialize publication with the existing GraphRun completion owner. The caller
/// stages its request/Goal writes in this same native transaction; rollback
/// therefore removes both publication and this generation change.
pub(crate) async fn fence_graph_publication_in_txn(
    txn: &ConfigApplyTxn<'_>,
    run_id: &str,
    revision_digest: &str,
) -> Result<()> {
    let response = txn
        .execute(&format!(
            r#"{{ GraphRun(filter: {{ run_id: {{ _eq: "{}" }} }}, limit: 2) {{
            _docID revision_digest status cancel_requested_at error update_generation
        }} }}"#,
            escape_graphql_string(run_id),
        ))
        .await?;
    let found = rows(&response, "GraphRun");
    if let Some(reason) = graph_publication_denial(found, revision_digest) {
        anyhow::bail!(reason);
    }
    let run = &found[0];
    let generation = run
        .get("update_generation")
        .and_then(Value::as_i64)
        .context("graph run is missing its publication generation")?;
    let next = generation
        .checked_add(1)
        .context("graph run publication generation exhausted")?;
    let doc_id = run
        .get("_docID")
        .and_then(Value::as_str)
        .context("graph run is missing its document ID")?;
    txn.execute(&format!(
        "mutation {{ update_GraphRun(docID: \"{}\", input: {}) {{ _docID }} }}",
        escape_graphql_string(doc_id),
        graphql_input_literal(&json!({ "update_generation": next }))?,
    ))
    .await?;
    Ok(())
}

fn revision_id(plan: &GraphPlan) -> String {
    format!("{}:{}", plan.graph_id, plan.digest)
}

fn planned_workspace_authority(plan: &GraphPlan, node_id: &str) -> Option<&'static str> {
    let node = plan.nodes.iter().find(|node| node.node_id == node_id)?;
    let ceiling = plan
        .package
        .as_ref()?
        .workspace_authority
        .get(&node.capability_id)?;
    match ceiling {
        WorkspaceAuthorityCeiling::None => None,
        WorkspaceAuthorityCeiling::ReadOnly => Some("readOnly"),
        WorkspaceAuthorityCeiling::ReadWrite => Some("readWrite"),
    }
}

fn materialization_receipt(plan: &GraphPlan) -> Result<MaterializedRevision> {
    let mut task_ids = plan
        .nodes
        .iter()
        .map(|node| node.task_id.clone())
        .collect::<Vec<_>>();
    let mut trigger_ids = plan
        .entries
        .iter()
        .map(|entry| {
            graph_trigger_id(
                &plan.digest,
                &format!(
                    "entry:{}:{}:{}",
                    entry.name, entry.to.node_id, entry.to.port
                ),
            )
        })
        .chain(plan.edges.iter().enumerate().map(|(index, edge)| {
            graph_trigger_id(
                &plan.digest,
                &format!(
                    "edge:{index}:{}:{}:{}:{}",
                    edge.from.node_id, edge.from.port, edge.to.node_id, edge.to.port,
                ),
            )
        }))
        .collect::<Result<Vec<_>>>()?;
    task_ids.sort();
    trigger_ids.sort();
    Ok(MaterializedRevision {
        graph_id: plan.graph_id.clone(),
        digest: plan.digest.clone(),
        task_ids,
        trigger_ids,
    })
}

fn rows<'a>(response: &'a Value, collection: &str) -> &'a [Value] {
    response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

async fn query_graph_definition(txn: &ConfigApplyTxn<'_>, graph_id: &str) -> Result<Option<Value>> {
    let query = format!(
        r#"{{
            GraphDefinition(filter: {{ graph_id: {{ _eq: "{}" }} }}, limit: 2) {{
                _docID graph_id owner_did enabled active_revision_digest generation created_at updated_at
            }}
        }}"#,
        escape_graphql_string(graph_id)
    );
    let response = txn.execute(&query).await?;
    let rows = rows(&response, "GraphDefinition");
    if rows.len() > 1 {
        anyhow::bail!("multiple GraphDefinition rows share graph_id {graph_id:?}");
    }
    Ok(rows.first().cloned())
}

async fn query_graph_revision(txn: &ConfigApplyTxn<'_>, digest: &str) -> Result<Option<Value>> {
    let query = format!(
        r#"{{
            GraphRevision(filter: {{ digest: {{ _eq: "{}" }} }}, limit: 2) {{
                _docID revision_id graph_id digest owner_did status compiler_version plan_json
                artifacts_complete materialization_error materialization_failed_at
                created_at materialized_at activated_at retired_at
            }}
        }}"#,
        escape_graphql_string(digest)
    );
    let response = txn.execute(&query).await?;
    let rows = rows(&response, "GraphRevision");
    if rows.len() > 1 {
        anyhow::bail!("multiple GraphRevision rows share digest {digest:?}");
    }
    Ok(rows.first().cloned())
}

fn verified_revision_plan(revision: &Value, graph_id: &str, owner_did: &str) -> Result<GraphPlan> {
    if revision.get("graph_id").and_then(Value::as_str) != Some(graph_id)
        || revision.get("owner_did").and_then(Value::as_str) != Some(owner_did)
    {
        anyhow::bail!("graph revision does not belong to this graph and owner");
    }
    let plan: GraphPlan = serde_json::from_str(
        revision
            .get("plan_json")
            .and_then(Value::as_str)
            .context("graph revision is missing plan_json")?,
    )?;
    let digest = revision
        .get("digest")
        .and_then(Value::as_str)
        .context("graph revision is missing digest")?;
    if plan.graph_id != graph_id || plan.digest != digest || !verify_graph_plan_digest(&plan) {
        anyhow::bail!("graph revision plan failed immutable identity verification");
    }
    Ok(plan)
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

async fn create_document(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    document: &Value,
) -> Result<String> {
    validate_collection_identifier(collection)?;
    let input = graphql_input_literal(document)?;
    let mutation = format!("mutation {{ create_{collection}(input: {input}) {{ _docID }} }}");
    let response = txn.execute(&mutation).await?;
    gents_protocol::graphql::extract_mutation_doc_id(&response, collection)
        .map_err(anyhow::Error::from)
}

async fn update_document(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    doc_id: &str,
    document: &Value,
) -> Result<()> {
    validate_collection_identifier(collection)?;
    let input = graphql_input_literal(document)?;
    let mutation = format!(
        "mutation {{ update_{collection}(docID: \"{}\", input: {input}) {{ _docID }} }}",
        escape_graphql_string(doc_id)
    );
    txn.execute(&mutation).await?;
    Ok(())
}

async fn discard_with_context(txn: ConfigApplyTxn<'_>, operation: &str) {
    if let Err(error) = txn.discard().await {
        tracing::warn!(operation, error = %error, "failed to discard graph pipeline transaction");
    }
}

/// Persist a compiled revision and its revision-derived EventTriggers in one
/// transaction. Stage Tasks are ordinary package/operator configuration and
/// must already exist and be enabled. The triggers remain invisible to runtime
/// reconciliation until activation advances the graph definition pointer.
pub async fn materialize_graph_revision(
    node: &EmbeddedNode,
    identity: Option<Did>,
    owner_did: &str,
    plan: &GraphPlan,
) -> Result<MaterializedRevision> {
    if !verify_graph_plan_digest(plan) {
        anyhow::bail!("refusing to materialize a GraphPlan with an invalid digest");
    }
    digest_hex(&plan.digest)?;
    let plan_json = serde_json::to_string(plan)?;
    let now = chrono::Utc::now().to_rfc3339();
    let txn = ConfigApplyTxn::begin_local(node, identity).await?;
    let result = materialize_in_txn(&txn, owner_did, plan, &plan_json, &now).await;
    match result {
        Ok(receipt) => {
            txn.commit().await.context("commit graph materialization")?;
            Ok(receipt)
        }
        Err(error) => {
            discard_with_context(txn, "materialize").await;
            Err(error)
        }
    }
}

/// Publish through the immutable revision machinery while preserving the
/// foundation tool's compact receipt. Publication never starts execution.
pub async fn publish_graph_plan(
    node: &EmbeddedNode,
    identity: Did,
    plan: &GraphPlan,
) -> Result<PublishedGraph> {
    let owner_did = identity.to_string();
    let materialized =
        materialize_graph_revision(node, Some(identity.clone()), &owner_did, plan).await?;
    activate_graph_revision(
        node,
        Some(identity),
        &owner_did,
        &plan.graph_id,
        &plan.digest,
        None,
    )
    .await?;
    Ok(PublishedGraph {
        graph_id: materialized.graph_id,
        digest: materialized.digest,
        trigger_ids: materialized.trigger_ids,
    })
}

pub(crate) async fn materialize_graph_revision_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner_did: &str,
    plan: &GraphPlan,
) -> Result<MaterializedRevision> {
    if !verify_graph_plan_digest(plan) {
        anyhow::bail!("refusing to materialize a GraphPlan with an invalid digest");
    }
    let plan_json = serde_json::to_string(plan)?;
    let now = chrono::Utc::now().to_rfc3339();
    materialize_in_txn(txn, owner_did, plan, &plan_json, &now).await
}

async fn materialize_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner_did: &str,
    plan: &GraphPlan,
    plan_json: &str,
    now: &str,
) -> Result<MaterializedRevision> {
    match query_graph_definition(txn, &plan.graph_id).await? {
        Some(definition) => {
            if definition.get("owner_did").and_then(Value::as_str) != Some(owner_did) {
                anyhow::bail!("graph {:?} belongs to a different principal", plan.graph_id);
            }
        }
        None => {
            create_document(
                txn,
                "GraphDefinition",
                &json!({
                    "graph_id": plan.graph_id,
                    "owner_did": owner_did,
                    "enabled": true,
                    "active_revision_digest": Value::Null,
                    "generation": 0,
                    "created_at": now,
                    "updated_at": now,
                }),
            )
            .await?;
        }
    }

    let existing_revision = query_graph_revision(txn, &plan.digest).await?;
    if let Some(revision) = existing_revision.as_ref() {
        if revision.get("graph_id").and_then(Value::as_str) != Some(plan.graph_id.as_str())
            || revision.get("owner_did").and_then(Value::as_str) != Some(owner_did)
            || revision.get("plan_json").and_then(Value::as_str) != Some(plan_json)
        {
            anyhow::bail!("stored graph revision does not match compiled plan identity");
        }
        if revision.get("artifacts_complete").and_then(Value::as_bool) == Some(true) {
            return materialization_receipt(plan);
        }
        if revision.get("status").and_then(Value::as_str) != Some("validated") {
            anyhow::bail!("incomplete graph revision is not in validated state");
        }
    } else {
        create_document(
            txn,
            "GraphRevision",
            &json!({
                "revision_id": revision_id(plan),
                "graph_id": plan.graph_id,
                "digest": plan.digest,
                "owner_did": owner_did,
                "status": "validated",
                "compiler_version": plan.compiler_version,
                "plan_json": plan_json,
                "artifacts_complete": false,
                "materialization_error": Value::Null,
                "materialization_failed_at": Value::Null,
                "created_at": now,
                "materialized_at": Value::Null,
                "activated_at": Value::Null,
                "retired_at": Value::Null,
            }),
        )
        .await?;
    }

    let task_ids = plan
        .nodes
        .iter()
        .map(|node| node.task_id.clone())
        .collect::<Vec<_>>();
    let enabled_tasks = query_enabled_task_ids(txn, &task_ids).await?;
    for planned_node in &plan.nodes {
        if !enabled_tasks.contains(&planned_node.task_id) {
            anyhow::bail!(
                "approved task {:?} is missing or disabled",
                planned_node.task_id
            );
        }
    }

    for entry in &plan.entries {
        let route = format!(
            "entry:{}:{}:{}",
            entry.name, entry.to.node_id, entry.to.port
        );
        let id = graph_trigger_id(&plan.digest, &route)?;
        write_trigger(
            txn,
            &id,
            &entry.target_task_id,
            &entry.collection,
            &entry.correlation_field,
            &DeliveryMode::PerDocument,
            &DeliveryConcurrency::Parallel,
            None,
            planned_workspace_authority(plan, &entry.to.node_id),
            now,
        )
        .await?;
    }
    for (index, edge) in plan.edges.iter().enumerate() {
        let route = format!(
            "edge:{index}:{}:{}:{}:{}",
            edge.from.node_id, edge.from.port, edge.to.node_id, edge.to.port,
        );
        let id = graph_trigger_id(&plan.digest, &route)?;
        write_trigger(
            txn,
            &id,
            &edge.target_task_id,
            &edge.source_collection,
            &edge.correlation_field,
            &edge.delivery,
            &edge.concurrency,
            edge.predicate.as_deref(),
            planned_workspace_authority(plan, &edge.to.node_id),
            now,
        )
        .await?;
    }

    let revision = query_graph_revision(txn, &plan.digest)
        .await?
        .context("materialized revision disappeared inside transaction")?;
    let revision_doc_id = revision
        .get("_docID")
        .and_then(Value::as_str)
        .context("GraphRevision is missing _docID")?;
    update_document(
        txn,
        "GraphRevision",
        revision_doc_id,
        &json!({
            "status": "validated",
            "artifacts_complete": true,
            "materialization_error": Value::Null,
            "materialization_failed_at": Value::Null,
            "materialized_at": now,
        }),
    )
    .await?;

    materialization_receipt(plan)
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
    workspace_authority: Option<&str>,
    now: &str,
) -> Result<()> {
    validate_collection_identifier(source_collection)?;
    let (fire_mode, expected_count, expected_count_field, group_timeout_secs) = match delivery {
        DeliveryMode::PerDocument => ("per_document", Value::Null, Value::Null, Value::Null),
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
        "workspace_authority": workspace_authority.map(Value::from).unwrap_or(Value::Null),
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

/// Compare-and-swap the one mutable pointer that makes a complete immutable
/// revision visible to runtime reconciliation.
pub async fn activate_graph_revision(
    node: &EmbeddedNode,
    identity: Option<Did>,
    owner_did: &str,
    graph_id: &str,
    digest: &str,
    expected_previous: Option<&str>,
) -> Result<ActivationReceipt> {
    digest_hex(digest)?;
    let now = chrono::Utc::now().to_rfc3339();
    let txn = ConfigApplyTxn::begin_local(node, identity).await?;
    let result = activate_in_txn(&txn, owner_did, graph_id, digest, expected_previous, &now).await;
    match result {
        Ok(receipt) => {
            txn.commit().await.context("commit graph activation")?;
            Ok(receipt)
        }
        Err(error) => {
            discard_with_context(txn, "activate").await;
            Err(error)
        }
    }
}

/// Activate through the existing local-or-GraphQL control-plane seam used by
/// CLI and desktop consumers.
pub async fn activate_graph_revision_with_access(
    access: &ConfigAccess,
    owner_did: &str,
    graph_id: &str,
    digest: &str,
    expected_previous: Option<&str>,
) -> Result<ActivationReceipt> {
    digest_hex(digest)?;
    let now = chrono::Utc::now().to_rfc3339();
    let txn = access.begin_apply_txn().await?;
    let result = activate_in_txn(&txn, owner_did, graph_id, digest, expected_previous, &now).await;
    match result {
        Ok(receipt) => {
            txn.commit().await.context("commit graph activation")?;
            Ok(receipt)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

async fn activate_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner_did: &str,
    graph_id: &str,
    digest: &str,
    expected_previous: Option<&str>,
    now: &str,
) -> Result<ActivationReceipt> {
    let definition = query_graph_definition(txn, graph_id)
        .await?
        .context("graph must be materialized before activation")?;
    if definition.get("owner_did").and_then(Value::as_str) != Some(owner_did) {
        anyhow::bail!("graph {graph_id:?} belongs to a different principal");
    }
    let current = definition
        .get("active_revision_digest")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let generation = definition
        .get("generation")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let revision = query_graph_revision(txn, digest)
        .await?
        .context("candidate graph revision does not exist")?;
    if revision.get("graph_id").and_then(Value::as_str) != Some(graph_id)
        || revision.get("owner_did").and_then(Value::as_str) != Some(owner_did)
        || revision.get("artifacts_complete").and_then(Value::as_bool) != Some(true)
    {
        anyhow::bail!("candidate revision is not complete for this graph and owner");
    }
    let plan = verified_revision_plan(&revision, graph_id, owner_did)?;
    if let Some(package) = plan.package.as_ref() {
        verify_package_role_bindings(package, owner_did)?;
    }
    if current.as_deref() == Some(digest) {
        let gate = revision_gate_decision(
            revision
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            revision
                .get("artifacts_complete")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            false,
            true,
        );
        if !gate.may_start {
            anyhow::bail!("active pointer selects a revision that is not runnable");
        }
        return Ok(ActivationReceipt {
            graph_id: graph_id.to_owned(),
            previous_digest: current.clone(),
            active_digest: digest.to_owned(),
            generation,
        });
    }
    if current.as_deref() != expected_previous {
        anyhow::bail!(
            "graph activation conflict: expected previous {:?}, found {:?}",
            expected_previous,
            current
        );
    }
    let gate = revision_gate_decision(
        revision
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        revision
            .get("artifacts_complete")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        true,
        false,
    );
    if !gate.may_activate {
        anyhow::bail!("candidate revision is not validated and ready for activation");
    }

    if let Some(previous) = current.as_deref() {
        if let Some(previous_revision) = query_graph_revision(txn, previous).await? {
            if let Some(doc_id) = previous_revision.get("_docID").and_then(Value::as_str) {
                update_document(
                    txn,
                    "GraphRevision",
                    doc_id,
                    &json!({ "status": "retired", "retired_at": now }),
                )
                .await?;
            }
        }
    }
    let revision_doc_id = revision
        .get("_docID")
        .and_then(Value::as_str)
        .context("candidate revision is missing _docID")?;
    update_document(
        txn,
        "GraphRevision",
        revision_doc_id,
        &json!({ "status": "active", "activated_at": now, "retired_at": Value::Null }),
    )
    .await?;
    let definition_doc_id = definition
        .get("_docID")
        .and_then(Value::as_str)
        .context("GraphDefinition is missing _docID")?;
    let next_generation = generation.saturating_add(1);
    update_document(
        txn,
        "GraphDefinition",
        definition_doc_id,
        &json!({
            "active_revision_digest": digest,
            "generation": next_generation,
            "updated_at": now,
        }),
    )
    .await?;

    Ok(ActivationReceipt {
        graph_id: graph_id.to_owned(),
        previous_digest: current,
        active_digest: digest.to_owned(),
        generation: next_generation,
    })
}

async fn load_active_graph_plan_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner_did: &str,
    graph_id: &str,
) -> Result<Option<GraphPlan>> {
    let definition = query_graph_definition(txn, graph_id)
        .await?
        .context("graph does not exist")?;
    if definition.get("owner_did").and_then(Value::as_str) != Some(owner_did) {
        anyhow::bail!("graph {graph_id:?} belongs to a different principal");
    }
    let Some(digest) = definition
        .get("active_revision_digest")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let revision = query_graph_revision(txn, digest)
        .await?
        .context("active graph revision is missing")?;
    Ok(Some(verified_revision_plan(
        &revision, graph_id, owner_did,
    )?))
}

/// Load the active immutable plan through the same ownership and identity
/// checks used by activation and run start.
pub async fn load_active_graph_plan_with_access(
    access: &ConfigAccess,
    owner_did: &str,
    graph_id: &str,
) -> Result<Option<GraphPlan>> {
    let txn = access.begin_apply_txn().await?;
    let result = load_active_graph_plan_in_txn(&txn, owner_did, graph_id).await;
    let _ = txn.discard().await;
    result
}

async fn set_graph_enabled_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner_did: &str,
    graph_id: &str,
    enabled: bool,
    now: &str,
) -> Result<()> {
    let definition = query_graph_definition(txn, graph_id)
        .await?
        .context("graph does not exist")?;
    if definition.get("owner_did").and_then(Value::as_str) != Some(owner_did) {
        anyhow::bail!("graph {graph_id:?} belongs to a different principal");
    }
    if enabled {
        let digest = definition
            .get("active_revision_digest")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .context("cannot enable a graph without an active revision")?;
        let revision = query_graph_revision(txn, digest)
            .await?
            .context("active graph revision is missing")?;
        verified_revision_plan(&revision, graph_id, owner_did)?;
        if !revision_gate_decision(
            revision
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            revision
                .get("artifacts_complete")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            false,
            true,
        )
        .may_start
        {
            anyhow::bail!("active graph revision is not runnable");
        }
    }
    let doc_id = definition
        .get("_docID")
        .and_then(Value::as_str)
        .context("GraphDefinition is missing _docID")?;
    update_document(
        txn,
        "GraphDefinition",
        doc_id,
        &json!({ "enabled": enabled, "updated_at": now }),
    )
    .await
}

/// Enable or disable a graph through the runtime-owned document control plane.
pub async fn set_graph_enabled_with_access(
    access: &ConfigAccess,
    owner_did: &str,
    graph_id: &str,
    enabled: bool,
) -> Result<()> {
    let txn = access.begin_apply_txn().await?;
    let result = set_graph_enabled_in_txn(
        &txn,
        owner_did,
        graph_id,
        enabled,
        &chrono::Utc::now().to_rfc3339(),
    )
    .await;
    match result {
        Ok(()) => txn.commit().await.context("commit graph enabled state"),
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

/// Pin a run to the active immutable manifest and seed exactly one compiled
/// entry collection in the same transaction as the GraphRun record.
pub async fn start_graph_run(
    node: &EmbeddedNode,
    identity: Option<Did>,
    caller_did: &str,
    graph_id: &str,
    expected_revision_digest: Option<&str>,
    entry_name: &str,
    input: Value,
) -> Result<GraphRunReceipt> {
    let txn = ConfigApplyTxn::begin_local(node, identity).await?;
    let result = start_run_in_txn(
        &txn,
        caller_did,
        graph_id,
        expected_revision_digest,
        entry_name,
        input,
    )
    .await;
    match result {
        Ok(receipt) => {
            txn.commit().await.context("commit graph run start")?;
            Ok(receipt)
        }
        Err(error) => {
            discard_with_context(txn, "start_run").await;
            Err(error)
        }
    }
}

/// Start through the ordinary local-or-GraphQL transaction seam.
pub async fn start_graph_run_with_access(
    access: &ConfigAccess,
    caller_did: &str,
    graph_id: &str,
    expected_revision_digest: Option<&str>,
    entry_name: &str,
    input: Value,
) -> Result<GraphRunReceipt> {
    let txn = access.begin_apply_txn().await?;
    let result = start_run_in_txn(
        &txn,
        caller_did,
        graph_id,
        expected_revision_digest,
        entry_name,
        input,
    )
    .await;
    match result {
        Ok(receipt) => {
            txn.commit().await.context("commit graph run start")?;
            Ok(receipt)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

async fn start_run_in_txn(
    txn: &ConfigApplyTxn<'_>,
    caller_did: &str,
    graph_id: &str,
    expected_revision_digest: Option<&str>,
    entry_name: &str,
    input: Value,
) -> Result<GraphRunReceipt> {
    let definition = query_graph_definition(txn, graph_id)
        .await?
        .context("graph does not exist")?;
    let owner_did = definition
        .get("owner_did")
        .and_then(Value::as_str)
        .context("GraphDefinition is missing owner_did")?;
    if owner_did != caller_did {
        anyhow::bail!("v1 graph runs may only be started by the graph owner");
    }
    if definition.get("enabled").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!("graph is disabled");
    }
    let digest = definition
        .get("active_revision_digest")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .context("graph has no active revision")?;
    if let Some(expected) = expected_revision_digest {
        if expected != digest {
            anyhow::bail!(
                "active graph revision changed after preflight: expected {expected:?}, observed {digest:?}"
            );
        }
    }
    let revision = query_graph_revision(txn, digest)
        .await?
        .context("active graph revision is missing")?;
    let plan = verified_revision_plan(&revision, graph_id, owner_did)?;
    if !revision_gate_decision(
        revision
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        revision
            .get("artifacts_complete")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        false,
        true,
    )
    .may_start
    {
        anyhow::bail!("active graph revision is not runnable");
    }
    if let Some(package) = plan.package.as_ref() {
        verify_package_role_bindings(package, owner_did)?;
        verify_package_schemas_in_txn(txn, package).await?;
        verify_package_artifacts_in_txn(txn, package).await?;
    }
    let entry = plan
        .entries
        .iter()
        .find(|entry| entry.name == entry_name)
        .context("unknown graph entry")?;
    validate_collection_identifier(&entry.collection)?;

    let mut input = match input {
        Value::Object(object) => object,
        _ => anyhow::bail!("graph entry input must be a JSON object"),
    };
    let run_id = uuid::Uuid::new_v4().to_string();
    if let Some(existing) = input.get(&entry.correlation_field) {
        if existing.as_str() != Some(run_id.as_str()) {
            anyhow::bail!("entry input may not override the run correlation field");
        }
    }
    input.insert(
        entry.correlation_field.clone(),
        Value::String(run_id.clone()),
    );
    let canonical_input = Value::Object(input.clone());
    let now = chrono::Utc::now().to_rfc3339();
    create_document(
        txn,
        "GraphRun",
        &json!({
            "run_id": run_id,
            "graph_id": graph_id,
            "revision_digest": digest,
            "owner_did": owner_did,
            "caller_did": caller_did,
            "entry_name": entry_name,
            "correlation": run_id,
            "status": "running",
            "input_json": serde_json::to_string(&canonical_input)?,
            "semantic_manifest_json": serde_json::to_string(&plan.capability_manifest)?,
            "limits_json": serde_json::to_string(&plan.limits)?,
            "cancel_requested_at": Value::Null,
            "cancel_requested_by": Value::Null,
            "cancel_reason": Value::Null,
            "result_refs_json": Value::Null,
            "update_generation": 0,
            "error": Value::Null,
            "created_at": now,
            "started_at": now,
            "completed_at": Value::Null,
        }),
    )
    .await?;
    let seed_doc_id = create_document(txn, &entry.collection, &canonical_input).await?;

    Ok(GraphRunReceipt {
        run_id: run_id.clone(),
        graph_id: graph_id.to_owned(),
        revision_digest: digest.to_owned(),
        entry_name: entry_name.to_owned(),
        correlation: run_id,
        seed_doc_id,
    })
}

#[cfg(test)]
pub(super) use tests::{
    attribution_test_fixture, graph_test_identity, graph_test_owner, seed_signed_graph_request,
};

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use super::*;
    use crate::graph_pipeline::{
        compile_graph, BundledProvenance, CompilerPolicy, EntryBinding, GraphIntent, GraphLimits,
        GraphNode, PortCardinality, PortRef, PortSpec, RequiredSchemaDigest, ResultCardinality,
        ResultContract, StageCapability,
    };

    #[tokio::test]
    async fn unavailable_package_revisions_fail_closed_without_failing_the_runtime_view() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        node.add_schema(gents_protocol::schemas::GRAPH_REVISION)
            .await
            .unwrap();
        let created_at = chrono::Utc::now().to_rfc3339();
        for (revision_id, digest, complete) in [
            ("incomplete", "sha256:incomplete", false),
            ("malformed", "sha256:malformed", true),
        ] {
            let response = node
                .execute(&format!(
                    r#"mutation {{ create_GraphRevision(input: {{
                        revision_id: "{revision_id}", graph_id: "graph", digest: "{digest}",
                        owner_did: "{owner}", status: "validated",
                        compiler_version: "test", plan_json: "{{}}",
                        artifacts_complete: {complete}, created_at: "{created_at}"
                    }}) {{ _docID }} }}"#,
                    owner = graph_test_owner(),
                    revision_id = escape_graphql_string(revision_id),
                    digest = escape_graphql_string(digest),
                    created_at = escape_graphql_string(&created_at),
                ))
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }

        let digests = BTreeSet::from([
            "sha256:missing".to_owned(),
            "sha256:incomplete".to_owned(),
            "sha256:malformed".to_owned(),
        ]);
        let visible = load_visible_package_artifact_ids(
            &node,
            graph_test_owner(),
            &digests,
            &BTreeSet::new(),
        )
        .await
        .unwrap();
        assert!(visible.0.is_empty());
        assert!(visible.1.is_empty());

        node.shutdown().await;
    }

    #[tokio::test]
    async fn package_schema_readiness_reuses_live_schema_introspection_and_fails_closed() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        node.add_schema("type ReadyInput { graph_run_id: String }")
            .await
            .unwrap();
        let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
        let ready_contract = crate::config_client::collection_schema_contract_digest(
            &txn.collection_version("ReadyInput").await.unwrap().unwrap(),
        )
        .unwrap();
        let mut package = PackagePlan {
            name: "test-package".to_owned(),
            version: "1.0.0".to_owned(),
            package_digest: format!("sha256:{}", "1".repeat(64)),
            bundled_provenance: BundledProvenance {
                binary_version: "test".to_owned(),
                build_commit: "test".to_owned(),
            },
            roles: Default::default(),
            workspace_authority: Default::default(),
            predecessor_revision_digest: None,
            artifacts: vec![],
            required_schema_digests: vec![RequiredSchemaDigest {
                namespace: "test.graphql".to_owned(),
                digest: format!("sha256:{}", "3".repeat(64)),
                collection_contract_digests: std::collections::BTreeMap::from([(
                    "ReadyInput".to_owned(),
                    ready_contract,
                )]),
            }],
        };

        verify_package_schemas_in_txn(&txn, &package).await.unwrap();
        package.required_schema_digests[0]
            .collection_contract_digests
            .get_mut("ReadyInput")
            .unwrap()
            .clone_from(&format!("sha256:{}", "f".repeat(64)));
        assert!(verify_package_schemas_in_txn(&txn, &package)
            .await
            .unwrap_err()
            .to_string()
            .contains("does not match its pinned contract"));
        package.required_schema_digests[0].collection_contract_digests =
            std::collections::BTreeMap::from([(
                "MissingInput".to_owned(),
                format!("sha256:{}", "f".repeat(64)),
            )]);
        assert!(verify_package_schemas_in_txn(&txn, &package)
            .await
            .unwrap_err()
            .to_string()
            .contains("is not locally ready"));

        txn.discard().await.unwrap();
        node.shutdown().await;
    }

    fn test_plan(configuration: &str) -> GraphPlan {
        let input = PortSpec {
            name: "input".to_owned(),
            collection: "PipelineInput".to_owned(),
            schema: "PipelineInput/v1".to_owned(),
            correlation_field: "graph_run_id".to_owned(),
            cardinality: PortCardinality::One,
            required: true,
        };
        let output = PortSpec {
            name: "result".to_owned(),
            collection: "PipelineResult".to_owned(),
            schema: "PipelineResult/v1".to_owned(),
            correlation_field: "graph_run_id".to_owned(),
            cardinality: PortCardinality::One,
            required: false,
        };
        compile_graph(
            &GraphIntent {
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
            },
            &[StageCapability {
                capability_id: "worker".to_owned(),
                revision: "v1".to_owned(),
                task_id: format!("worker-{configuration}"),
                input_ports: vec![input],
                output_ports: vec![output],
                allowed_callers: vec![graph_test_owner().to_owned()],
            }],
            graph_test_owner(),
            &CompilerPolicy::default(),
        )
        .unwrap()
    }

    fn grouped_test_plan() -> GraphPlan {
        let input = PortSpec {
            name: "input".to_owned(),
            collection: "PipelineInput".to_owned(),
            schema: "PipelineInput/v1".to_owned(),
            correlation_field: "graph_run_id".to_owned(),
            cardinality: PortCardinality::One,
            required: true,
        };
        let output = PortSpec {
            name: "items".to_owned(),
            collection: "PipelineBatch".to_owned(),
            schema: "PipelineBatch/v1".to_owned(),
            correlation_field: "graph_run_id".to_owned(),
            cardinality: PortCardinality::One,
            required: false,
        };
        let grouped_input = PortSpec {
            cardinality: PortCardinality::Many,
            required: true,
            ..output.clone()
        };
        compile_graph(
            &GraphIntent {
                graph_id: "grouped-pipeline".to_owned(),
                nodes: vec![
                    GraphNode {
                        node_id: "producer".to_owned(),
                        capability_id: "producer".to_owned(),
                        capability_revision: "v1".to_owned(),
                    },
                    GraphNode {
                        node_id: "consumer".to_owned(),
                        capability_id: "consumer".to_owned(),
                        capability_revision: "v1".to_owned(),
                    },
                ],
                edges: vec![super::super::GraphEdge {
                    from: super::super::PortRef {
                        node_id: "producer".to_owned(),
                        port: "items".to_owned(),
                    },
                    to: super::super::PortRef {
                        node_id: "consumer".to_owned(),
                        port: "items".to_owned(),
                    },
                    delivery: DeliveryMode::PerGroup {
                        expected: GroupCount::SourceField {
                            field: "expected_total".to_owned(),
                        },
                        timeout_secs: Some(60),
                    },
                    concurrency: DeliveryConcurrency::Serial,
                    predicate: None,
                }],
                entries: vec![EntryBinding {
                    name: "input".to_owned(),
                    collection: input.collection.clone(),
                    schema: input.schema.clone(),
                    input_contract: None,
                    to: super::super::PortRef {
                        node_id: "producer".to_owned(),
                        port: "input".to_owned(),
                    },
                }],
                results: vec![ResultContract {
                    name: "items".to_owned(),
                    from: PortRef {
                        node_id: "producer".to_owned(),
                        port: "items".to_owned(),
                    },
                    cardinality: ResultCardinality::AtMost { count: 2 },
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
            },
            &[
                StageCapability {
                    capability_id: "producer".to_owned(),
                    revision: "v1".to_owned(),
                    task_id: "producer-task".to_owned(),
                    input_ports: vec![input],
                    output_ports: vec![output],
                    allowed_callers: vec![graph_test_owner().to_owned()],
                },
                StageCapability {
                    capability_id: "consumer".to_owned(),
                    revision: "v1".to_owned(),
                    task_id: "consumer-task".to_owned(),
                    input_ports: vec![grouped_input],
                    output_ports: vec![],
                    allowed_callers: vec![graph_test_owner().to_owned()],
                },
            ],
            graph_test_owner(),
            &CompilerPolicy::default(),
        )
        .unwrap()
    }

    fn result_test_plan() -> GraphPlan {
        let input = PortSpec {
            name: "input".to_owned(),
            collection: "PipelineInput".to_owned(),
            schema: "PipelineInput/v1".to_owned(),
            correlation_field: "graph_run_id".to_owned(),
            cardinality: PortCardinality::One,
            required: true,
        };
        let output = PortSpec {
            name: "report".to_owned(),
            collection: "PipelineResult".to_owned(),
            schema: "PipelineResult/v1".to_owned(),
            correlation_field: "graph_run_id".to_owned(),
            cardinality: PortCardinality::One,
            required: false,
        };
        compile_graph(
            &GraphIntent {
                graph_id: "result-pipeline".to_owned(),
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
                        port: "input".to_owned(),
                    },
                }],
                results: vec![ResultContract {
                    name: "report".to_owned(),
                    from: PortRef {
                        node_id: "worker".to_owned(),
                        port: "report".to_owned(),
                    },
                    cardinality: ResultCardinality::Exactly { count: 1 },
                    terminal: true,
                }],
                limits: GraphLimits {
                    max_nodes: 1,
                    max_edges: 1,
                    max_depth: 1,
                    max_fan_out: 1,
                    max_total_invocations: 1,
                    max_runtime_secs: 60,
                },
            },
            &[StageCapability {
                capability_id: "worker".to_owned(),
                revision: "v1".to_owned(),
                task_id: "worker-result".to_owned(),
                input_ports: vec![input],
                output_ports: vec![output],
                allowed_callers: vec![graph_test_owner().to_owned()],
            }],
            graph_test_owner(),
            &CompilerPolicy::default(),
        )
        .unwrap()
    }

    #[test]
    fn reserved_artifact_ids_round_trip_only_in_strict_shape() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let id = graph_trigger_id(&digest, "entry:input:node:input").unwrap();
        assert_eq!(graph_artifact_revision_digest(&id), Some(digest.clone()));
        assert_eq!(graph_artifact_revision_digest("operator-task"), None);
        assert_eq!(
            graph_artifact_revision_digest("graph-trigger-not-a-digest-x"),
            None
        );
        let active = BTreeSet::from([digest.clone()]);
        assert!(graph_artifact_is_reserved(&id));
        assert!(graph_artifact_is_reserved("graph-trigger-not-a-digest-x"));
        assert!(!graph_artifact_is_reserved("operator-task"));
        assert!(graph_artifact_is_visible(&id, &active));
        assert!(graph_artifact_is_visible("operator-task", &BTreeSet::new()));
        assert!(!graph_artifact_is_visible(&id, &BTreeSet::new()));
        assert!(!graph_artifact_is_visible(
            "graph-trigger-not-a-digest-x",
            &BTreeSet::new()
        ));
    }

    #[test]
    fn package_visibility_distinguishes_active_pointers_from_run_pins() {
        assert!(revision_visibility_authorized("active", true, false));
        assert!(!revision_visibility_authorized("retired", true, false));
        assert!(revision_visibility_authorized("active", false, true));
        assert!(revision_visibility_authorized("retired", false, true));
        assert!(!revision_visibility_authorized("validated", true, true));
        assert!(!revision_visibility_authorized("draft", true, true));
    }

    async fn install_plan_tasks(node: &EmbeddedNode, plan: &GraphPlan) {
        use crate::identity::AgentIdentity;
        let identity = graph_test_identity();
        let behavior = node.execute(r#"{ AgentBehavior(filter: { behavior_id: { _eq: "test-behavior" } }) { _docID } }"#).await;
        assert!(!behavior.has_errors(), "{:?}", behavior.errors);
        if behavior.data.unwrap()["AgentBehavior"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            let result = node.execute(&format!(r#"mutation {{ create_AgentBehavior(input: {{ behavior_id: "test-behavior", agent_did: "{}", enabled: true }}) {{ _docID }} }}"#, escape_graphql_string(identity.did()))).await;
            assert!(!result.has_errors(), "{:?}", result.errors);
        }
        let now = chrono::Utc::now().to_rfc3339();
        for task_id in plan
            .nodes
            .iter()
            .map(|planned| planned.task_id.as_str())
            .collect::<BTreeSet<_>>()
        {
            let response = node
                .execute(&format!(
                    r#"mutation {{ create_Task(input: {{
                        task_id: "{}", name: "{}", description: "test task",
                        behavior_id: "test-behavior", prompt_template: "test",
                        enabled: true, created_at: "{}", updated_at: "{}"
                    }}) {{ _docID }} }}"#,
                    escape_graphql_string(task_id),
                    escape_graphql_string(task_id),
                    escape_graphql_string(&now),
                    escape_graphql_string(&now),
                ))
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
    }

    #[tokio::test]
    async fn revision_publication_and_run_start_are_transactional_and_pinned() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        for schema in [
            gents_protocol::schemas::GRAPH_DEFINITION,
            gents_protocol::schemas::GRAPH_REVISION,
            gents_protocol::schemas::GRAPH_RUN,
            gents_protocol::schemas::AGENT_REQUEST,
            gents_protocol::schemas::GOAL,
            gents_protocol::schemas::GOAL_CREATION_CLAIM,
            gents_protocol::schemas::EVENT_TRIGGER_GROUP_STATE,
            gents_protocol::schemas::TASK,
            gents_protocol::schemas::AGENT_BEHAVIOR,
            gents_protocol::schemas::EVENT_TRIGGER,
            r#"type PipelineInput {
                graph_run_id: String @index(unique: true)
                payload: String
            }"#,
            r#"type PipelineResult {
                graph_run_id: String @index
                report: String
            }"#,
        ] {
            node.add_schema(schema).await.unwrap();
        }

        let first = test_plan("first");
        install_plan_tasks(&node, &first).await;
        let materialized = materialize_graph_revision(&node, None, graph_test_owner(), &first)
            .await
            .unwrap();
        assert_eq!(materialized.task_ids.len(), 1);
        assert_eq!(materialized.trigger_ids.len(), 1);
        assert_eq!(
            materialize_graph_revision(&node, None, graph_test_owner(), &first)
                .await
                .unwrap(),
            materialized
        );

        let before = node
            .execute("{ GraphDefinition { active_revision_digest generation } }")
            .await;
        assert!(!before.has_errors(), "{:?}", before.errors);
        let before_row = before.data.unwrap()["GraphDefinition"][0].clone();
        assert!(before_row["active_revision_digest"].is_null());
        assert_eq!(before_row["generation"], 0);

        let activation = activate_graph_revision(
            &node,
            None,
            graph_test_owner(),
            "pipeline",
            &first.digest,
            None,
        )
        .await
        .unwrap();
        assert_eq!(activation.generation, 1);
        assert_eq!(activation.previous_digest, None);
        assert_eq!(
            materialize_graph_revision(&node, None, graph_test_owner(), &first)
                .await
                .unwrap(),
            materialized
        );
        let still_active = node
            .execute("{ GraphRevision { digest status artifacts_complete } }")
            .await;
        assert!(!still_active.has_errors(), "{:?}", still_active.errors);
        let active_row = still_active.data.unwrap()["GraphRevision"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["digest"].as_str() == Some(first.digest.as_str()))
            .unwrap()
            .clone();
        assert_eq!(active_row["status"], "active");
        assert_eq!(active_row["artifacts_complete"], true);

        let read_txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
        let loaded = load_active_graph_plan_in_txn(&read_txn, graph_test_owner(), "pipeline")
            .await
            .unwrap()
            .expect("active plan");
        assert_eq!(loaded.digest, first.digest);
        read_txn.discard().await.unwrap();

        let disable_txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
        set_graph_enabled_in_txn(
            &disable_txn,
            graph_test_owner(),
            "pipeline",
            false,
            "2026-08-25T00:00:00Z",
        )
        .await
        .unwrap();
        disable_txn.commit().await.unwrap();
        assert!(start_graph_run(
            &node,
            None,
            graph_test_owner(),
            "pipeline",
            Some(&first.digest),
            "input",
            json!({ "payload": "disabled" }),
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("graph is disabled"));
        let enable_txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
        set_graph_enabled_in_txn(
            &enable_txn,
            graph_test_owner(),
            "pipeline",
            true,
            "2026-08-25T00:00:01Z",
        )
        .await
        .unwrap();
        enable_txn.commit().await.unwrap();

        let stale_digest = format!("sha256:{}", "f".repeat(64));
        let stale_start = start_graph_run(
            &node,
            None,
            graph_test_owner(),
            "pipeline",
            Some(&stale_digest),
            "input",
            json!({ "payload": "stale" }),
        )
        .await
        .unwrap_err();
        assert!(stale_start
            .to_string()
            .contains("active graph revision changed after preflight"));
        let no_stale_run = node.execute("{ GraphRun { run_id } }").await;
        assert!(!no_stale_run.has_errors(), "{:?}", no_stale_run.errors);
        assert!(no_stale_run.data.unwrap()["GraphRun"]
            .as_array()
            .unwrap()
            .is_empty());

        let run = start_graph_run(
            &node,
            None,
            graph_test_owner(),
            "pipeline",
            None,
            "input",
            json!({ "payload": "hello" }),
        )
        .await
        .unwrap();
        assert_eq!(run.revision_digest, first.digest);
        assert_eq!(run.correlation, run.run_id);
        assert!(graph_materialization_denial(
            &node,
            &materialized.trigger_ids[0],
            Some(&run.correlation),
        )
        .await
        .unwrap()
        .is_none());
        assert!(
            graph_materialization_denial(&node, "operator-trigger", None)
                .await
                .unwrap()
                .is_none()
        );
        assert!(graph_materialization_denial(
            &node,
            &materialized.trigger_ids[0],
            Some("unknown-run"),
        )
        .await
        .unwrap()
        .is_some());

        let persisted = node
            .execute("{ GraphRun { run_id revision_digest status input_json } PipelineInput { graph_run_id payload } }")
            .await;
        assert!(!persisted.has_errors(), "{:?}", persisted.errors);
        let data = persisted.data.unwrap();
        assert_eq!(data["GraphRun"][0]["status"], "running");
        assert_eq!(data["GraphRun"][0]["revision_digest"], first.digest);
        assert_eq!(data["PipelineInput"][0]["graph_run_id"], run.run_id);
        assert_eq!(data["PipelineInput"][0]["payload"], "hello");

        let second = test_plan("second");
        install_plan_tasks(&node, &second).await;
        materialize_graph_revision(&node, None, graph_test_owner(), &second)
            .await
            .unwrap();
        let conflict = activate_graph_revision(
            &node,
            None,
            graph_test_owner(),
            "pipeline",
            &second.digest,
            None,
        )
        .await
        .unwrap_err();
        assert!(conflict.to_string().contains("activation conflict"));
        let switched = activate_graph_revision(
            &node,
            None,
            graph_test_owner(),
            "pipeline",
            &second.digest,
            Some(&first.digest),
        )
        .await
        .unwrap();
        assert_eq!(
            switched.previous_digest.as_deref(),
            Some(first.digest.as_str())
        );
        assert_eq!(switched.generation, 2);

        let now = chrono::Utc::now().to_rfc3339();
        for request_id in [
            "completed-request-1",
            "completed-request-2",
            "completed-request-3",
        ] {
            seed_signed_graph_request(
                &node,
                &run,
                &materialized.trigger_ids[0],
                request_id,
                "completed",
                "",
            )
            .await;
        }
        seed_signed_graph_request(
            &node,
            &run,
            &materialized.trigger_ids[0],
            "cancel-recovery-request",
            "processing",
            "",
        )
        .await;
        let cancel_intent = node
            .execute(&format!(
                r#"mutation {{ update_GraphRun(
                    filter: {{ run_id: {{ _eq: "{}" }} }},
                    input: {{ cancel_requested_at: "{}", cancel_requested_by: "did:key:owner",
                        cancel_reason: "operator stopped the run", update_generation: 1 }}
                ) {{ _docID }} }}"#,
                escape_graphql_string(&run.run_id),
                escape_graphql_string(&now),
            ))
            .await;
        assert!(!cancel_intent.has_errors(), "{:?}", cancel_intent.errors);

        let recovering =
            super::super::reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
                .await
                .unwrap();
        assert_eq!(recovering.status, "running");
        assert_eq!(recovering.active_request_count, 1);
        let interrupted = node
            .execute(
                r#"{ AgentRequest(filter: { request_id: { _eq: "cancel-recovery-request" } }) {
                    interrupt_requested_at
                } }"#,
            )
            .await;
        assert!(!interrupted.has_errors(), "{:?}", interrupted.errors);
        assert!(
            interrupted.data.unwrap()["AgentRequest"][0]["interrupt_requested_at"]
                .as_str()
                .is_some()
        );
        let terminal_request = node
            .execute(
                r#"mutation { update_AgentRequest(
                    filter: { request_id: { _eq: "cancel-recovery-request" } },
                    input: { lifecycle_state: "interrupted" }
                ) { _docID } }"#,
            )
            .await;
        assert!(
            !terminal_request.has_errors(),
            "{:?}",
            terminal_request.errors
        );
        let cancelled =
            super::super::reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
                .await
                .unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert_eq!(cancelled.update_generation, 2);
        assert_eq!(
            cancelled.cancellation_reason.as_deref(),
            Some("operator stopped the run")
        );
        assert!(graph_materialization_denial(
            &node,
            &materialized.trigger_ids[0],
            Some(&run.correlation),
        )
        .await
        .unwrap()
        .is_some());

        node.shutdown().await;
    }

    #[tokio::test]
    async fn graph_run_view_commits_exact_terminal_result_refs_and_reloads_them() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        for schema in [
            gents_protocol::schemas::GRAPH_DEFINITION,
            gents_protocol::schemas::GRAPH_REVISION,
            gents_protocol::schemas::GRAPH_RUN,
            gents_protocol::schemas::AGENT_REQUEST,
            gents_protocol::schemas::GOAL,
            gents_protocol::schemas::GOAL_CREATION_CLAIM,
            gents_protocol::schemas::EVENT_TRIGGER_GROUP_STATE,
            gents_protocol::schemas::TASK,
            gents_protocol::schemas::AGENT_BEHAVIOR,
            gents_protocol::schemas::EVENT_TRIGGER,
            r#"type PipelineInput {
                graph_run_id: String @index(unique: true)
                payload: String
            }"#,
            r#"type PipelineResult {
                graph_run_id: String @index
                report: String
            }"#,
        ] {
            node.add_schema(schema).await.unwrap();
        }
        let plan = result_test_plan();
        install_plan_tasks(&node, &plan).await;
        materialize_graph_revision(&node, None, graph_test_owner(), &plan)
            .await
            .unwrap();
        activate_graph_revision(
            &node,
            None,
            graph_test_owner(),
            "result-pipeline",
            &plan.digest,
            None,
        )
        .await
        .unwrap();
        let run = start_graph_run(
            &node,
            None,
            graph_test_owner(),
            "result-pipeline",
            None,
            "input",
            json!({ "payload": "review" }),
        )
        .await
        .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let entry_trigger_id = graph_trigger_id(&plan.digest, "entry:input:worker:input").unwrap();
        seed_signed_graph_request(&node, &run, &entry_trigger_id, "request-1", "completed", "")
            .await;
        let unsatisfied = super::super::load_graph_run_view(&node, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        assert_eq!(
            unsatisfied
                .failure_evidence
                .as_ref()
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str),
            Some("result_contract_unsatisfied")
        );
        let result = node
            .execute(&format!(
                r#"mutation {{ create_PipelineResult(input: {{
                    graph_run_id: "{}", report: "looks good"
                }}) {{ _docID }} }}"#,
                escape_graphql_string(&run.correlation),
            ))
            .await;
        assert!(!result.has_errors(), "{:?}", result.errors);

        // Ordinary automation may deliberately reuse the run UUID as a
        // correlation value. Only requests/groups owned by this pinned
        // revision's trigger IDs contribute to graph completion.
        let unrelated = node
            .execute(&format!(
                r#"mutation {{
                    request: create_AgentRequest(input: {{
                        request_id: "unrelated-request", agent_did: "did:key:worker",
                        requester_did: "did:key:owner", behavior_id: "operator-behavior",
                        lifecycle_state: "processing",
                        caused_by_trigger_id: "operator-trigger",
                        caused_by_correlation: "{}", created_at: "{}"
                    }}) {{ _docID }}
                    group: create_EventTriggerGroupState(input: {{
                        group_key: "unrelated-group", trigger_id: "operator-trigger",
                        trigger_config_key: "operator-trigger-v1", correlation: "{}",
                        first_seen_at: "{}", quiesced_at: "{}",
                        quiesced_reason: "operator timeout"
                    }}) {{ _docID }}
                }}"#,
                escape_graphql_string(&run.correlation),
                escape_graphql_string(&now),
                escape_graphql_string(&run.correlation),
                escape_graphql_string(&now),
                escape_graphql_string(&now),
            ))
            .await;
        assert!(!unrelated.has_errors(), "{:?}", unrelated.errors);
        let scoped = super::super::load_graph_run_view(&node, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        assert_eq!(scoped.requests.len(), 1);
        assert!(scoped.groups.is_empty());
        assert_eq!(scoped.active_request_count, 0);
        assert!(scoped.failure_evidence.is_none());

        assert!(
            super::super::load_graph_run_view(&node, "did:key:intruder", &run.run_id)
                .await
                .unwrap_err()
                .to_string()
                .contains("not authorized")
        );
        assert_eq!(
            super::super::reconcile_owned_graph_runs(&node, graph_test_owner())
                .await
                .unwrap(),
            1
        );
        let terminal = super::super::load_graph_run_view(&node, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        assert_eq!(terminal.status, "succeeded");
        assert_eq!(terminal.persisted_result_refs.len(), 1);
        assert_eq!(terminal.persisted_result_refs[0].name, "report");
        assert!(!terminal.persisted_result_refs[0].commit_cid.is_empty());

        let reloaded = super::super::load_graph_run_view(&node, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        assert_eq!(
            reloaded.persisted_result_refs,
            terminal.persisted_result_refs
        );
        assert_eq!(
            super::super::reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id,)
                .await
                .unwrap()
                .status,
            "succeeded"
        );
        assert_eq!(
            super::super::reconcile_owned_graph_runs(&node, graph_test_owner())
                .await
                .unwrap(),
            0
        );
        let result_document = node
            .execute("{ PipelineResult(limit: 1) { _docID report } }")
            .await;
        assert!(
            !result_document.has_errors(),
            "{:?}",
            result_document.errors
        );
        let result_document = result_document.data.unwrap()["PipelineResult"][0].clone();
        let result_doc_id = result_document["_docID"].as_str().unwrap();
        let changed = node
            .execute(&format!(
                r#"mutation {{ update_PipelineResult(
                    docID: "{}", input: {{ report: "changed after completion" }}
                ) {{ _docID }} }}"#,
                escape_graphql_string(result_doc_id),
            ))
            .await;
        assert!(!changed.has_errors(), "{:?}", changed.errors);
        let access = ConfigAccess::Local(Arc::clone(&node));
        let hydrated = super::super::load_graph_run_result_view_with_access(
            &access,
            graph_test_owner(),
            &run.run_id,
        )
        .await
        .unwrap();
        let report = hydrated
            .results
            .iter()
            .find(|result| result.name == "report")
            .unwrap();
        assert_eq!(report.refs, terminal.persisted_result_refs);
        assert_eq!(report.documents[0]["report"], "looks good");
        node.shutdown().await;
    }

    #[tokio::test]
    async fn grouped_delivery_lowers_to_the_existing_event_trigger_contract() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        for schema in [
            gents_protocol::schemas::GRAPH_DEFINITION,
            gents_protocol::schemas::GRAPH_REVISION,
            gents_protocol::schemas::TASK,
            gents_protocol::schemas::AGENT_BEHAVIOR,
            gents_protocol::schemas::EVENT_TRIGGER,
        ] {
            node.add_schema(schema).await.unwrap();
        }

        let plan = grouped_test_plan();
        install_plan_tasks(&node, &plan).await;
        materialize_graph_revision(&node, None, graph_test_owner(), &plan)
            .await
            .unwrap();
        let response = node
            .execute("{ EventTrigger { fire_mode concurrency expected_count expected_count_field group_timeout_secs group_min_count } }")
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let rows = response.data.unwrap()["EventTrigger"]
            .as_array()
            .unwrap()
            .clone();
        let grouped = rows
            .iter()
            .find(|row| row["fire_mode"] == "per_group")
            .expect("group trigger");
        assert_eq!(grouped["concurrency"], "serial");
        assert!(grouped["expected_count"].is_null());
        assert_eq!(grouped["expected_count_field"], "expected_total");
        assert_eq!(grouped["group_timeout_secs"], 60);
        assert_eq!(grouped["group_min_count"], 1);

        node.shutdown().await;
    }
    async fn reconcile_failure_fixture(
        node: &Arc<EmbeddedNode>,
        run_id: &str,
        via_access: bool,
    ) -> crate::graph_pipeline::GraphRunView {
        if via_access {
            let access = ConfigAccess::Local(Arc::clone(node));
            super::super::reconcile_graph_run_with_access(&access, graph_test_owner(), run_id)
                .await
                .unwrap()
        } else {
            super::super::reconcile_graph_run(node, None, graph_test_owner(), run_id)
                .await
                .unwrap()
        }
    }

    // Drives the real GraphRun transaction/interrupt owners. Request fixture
    // state changes emulate executor terminal observations, not a second graph
    // coordinator. No provider or wall-clock sleep is needed.
    /// Seed a real authenticated route receipt, then set its mutable lifecycle.
    pub(in crate::graph_pipeline) fn graph_test_owner() -> &'static str {
        use crate::identity::AgentIdentity;
        static OWNER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        OWNER.get_or_init(|| graph_test_identity().did().to_owned())
    }

    pub(in crate::graph_pipeline) fn graph_test_identity() -> crate::identity::KeyIdentity {
        use crate::identity::KeyIdentity;
        static KEY: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        let key = KEY.get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("worker.key");
            KeyIdentity::load_or_create(&path, None).unwrap();
            std::fs::read(path).unwrap()
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worker.key");
        std::fs::write(&path, key).unwrap();
        KeyIdentity::load_or_create(path, None).unwrap()
    }

    pub(in crate::graph_pipeline) async fn seed_signed_graph_request(
        node: &EmbeddedNode,
        run: &GraphRunReceipt,
        trigger: &str,
        request_id: &str,
        lifecycle: &str,
        reason: &str,
    ) {
        use crate::identity::AgentIdentity;
        use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
        let identity = graph_test_identity();
        let mut create = AgentRequestCreate::base(
            request_id,
            identity.did(),
            identity.did(),
            "test-behavior",
            &format!("session-{request_id}"),
            "Execute the pinned graph stage",
            "scheduled",
            "2026-08-25T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_automated_trigger(identity.did(), trigger),
        );
        let triggers = node
            .execute(&format!(
                r#"{{ EventTrigger(filter: {{ trigger_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(trigger),
            ))
            .await;
        assert!(!triggers.has_errors(), "{:?}", triggers.errors);
        create.caused_by_trigger_doc_id = Some(
            triggers.data.unwrap()["EventTrigger"][0]["_docID"]
                .as_str()
                .unwrap()
                .into(),
        );
        create.caused_by_trigger_id = Some(trigger.into());
        create.caused_by_trigger_kind = Some("event".into());
        create.caused_by_correlation = Some(run.correlation.clone());
        create.caused_by_source_doc_id = Some(run.seed_doc_id.clone());
        crate::sign_agent_request_create(&identity, &mut create)
            .await
            .unwrap();
        let created = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(!created.has_errors(), "{:?}", created.errors);
        let updated = node.execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "{}", failure_reason: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(request_id), escape_graphql_string(lifecycle), escape_graphql_string(reason),
        )).await;
        assert!(!updated.has_errors(), "{:?}", updated.errors);
    }

    pub(in crate::graph_pipeline) async fn attribution_test_fixture(
        max_invocations: u32,
    ) -> (Arc<EmbeddedNode>, GraphRunReceipt, String) {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        for schema in [
            gents_protocol::schemas::GRAPH_DEFINITION,
            gents_protocol::schemas::GRAPH_REVISION,
            gents_protocol::schemas::GRAPH_RUN,
            gents_protocol::schemas::AGENT_REQUEST,
            gents_protocol::schemas::GOAL,
            gents_protocol::schemas::GOAL_CREATION_CLAIM,
            gents_protocol::schemas::EVENT_TRIGGER_GROUP_STATE,
            gents_protocol::schemas::TASK,
            gents_protocol::schemas::AGENT_BEHAVIOR,
            gents_protocol::schemas::EVENT_TRIGGER,
            "type PipelineInput { graph_run_id: String @index(unique: true) payload: String }",
            "type PipelineResult { graph_run_id: String @index report: String }",
        ] {
            node.add_schema(schema).await.unwrap();
        }
        let mut plan = result_test_plan();
        // The physical requests must fit the run limit, or the fixture would
        // prove invocation-limit failure instead of request-cause attribution.
        plan.limits.max_total_invocations = max_invocations;
        plan.digest = crate::graph_pipeline::graph_plan_digest(&plan);
        install_plan_tasks(&node, &plan).await;
        materialize_graph_revision(&node, None, graph_test_owner(), &plan)
            .await
            .unwrap();
        activate_graph_revision(
            &node,
            None,
            graph_test_owner(),
            "result-pipeline",
            &plan.digest,
            None,
        )
        .await
        .unwrap();
        let run = start_graph_run(
            &node,
            None,
            graph_test_owner(),
            "result-pipeline",
            None,
            "input",
            json!({ "payload": "fail-fast cause regression" }),
        )
        .await
        .unwrap();
        let trigger = graph_trigger_id(&plan.digest, "entry:input:worker:input").unwrap();
        (node, run, trigger)
    }

    async fn assert_fail_fast_keeps_committed_primary_cause(
        cause_id: &str,
        sibling_id: &str,
        sibling_outcome: &str,
        via_access: bool,
        cancel_after_latch: bool,
    ) {
        let (node, run, trigger) = attribution_test_fixture(2).await;
        let original_reason = "MaxTurnError: maximum 250 turns exceeded";
        seed_signed_graph_request(&node, &run, &trigger, cause_id, "failed", original_reason).await;
        seed_signed_graph_request(&node, &run, &trigger, sibling_id, "processing", "").await;

        let first = reconcile_failure_fixture(&node, &run.run_id, via_access).await;
        assert_eq!(first.status, "running");
        assert_eq!(first.active_request_count, 1);
        // Reload from DB: asserting only failure_evidence would pass before
        // the fix because that is recomputed and is not a durable latch.
        let latched = super::super::load_graph_run_view(&node, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        let primary = latched
            .error
            .clone()
            .expect("primary cause must be durable before fail-fast drains siblings");
        assert_eq!(primary["code"], "required_request_failed");
        assert_eq!(primary["request_id"], cause_id);
        assert_eq!(primary["message"], original_reason);
        assert_eq!(primary["lifecycle_state"], "failed");
        assert!(latched.update_generation > 0);
        let interrupt = node.execute(&format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }} ) {{ interrupt_requested_at }} }}"#,
            escape_graphql_string(sibling_id))).await;
        assert!(!interrupt.has_errors(), "{:?}", interrupt.errors);
        assert!(
            interrupt.data.unwrap()["AgentRequest"][0]["interrupt_requested_at"]
                .as_str()
                .is_some()
        );

        // A fresh reconciliation object has only DB state, like restart after
        // the committed cause and initial interrupt. It cannot replace cause.
        let repeated = reconcile_failure_fixture(&node, &run.run_id, via_access).await;
        assert_eq!(repeated.status, "running");
        assert_eq!(repeated.error.as_ref(), Some(&primary));
        assert_eq!(repeated.update_generation, latched.update_generation);
        if cancel_after_latch {
            let reason = Some("operator cancelled after primary cause was committed");
            let cancelled = if via_access {
                let access = ConfigAccess::Local(Arc::clone(&node));
                super::super::request_graph_run_cancellation_with_access(
                    &access,
                    graph_test_owner(),
                    &run.run_id,
                    reason,
                )
                .await
                .unwrap()
            } else {
                super::super::request_graph_run_cancellation(
                    &node,
                    None,
                    graph_test_owner(),
                    &run.run_id,
                    reason,
                )
                .await
                .unwrap()
            };
            assert_eq!(cancelled.status, "running");
            assert!(cancelled.cancellation_requested_at.is_some());
            assert_eq!(cancelled.cancellation_reason.as_deref(), reason);
            assert!(cancelled.update_generation > latched.update_generation);
            // Error may remain diagnostic during drain. Cancellation must win
            // final status and clear error once the sibling is terminal.
        }
        let drained = node
            .execute(&format!(
                r#"mutation {{ update_AgentRequest(
            filter: {{ request_id: {{ _eq: "{}" }} }},
            input: {{ lifecycle_state: "{}", failure_reason: "later sibling outcome" }}
        ) {{ _docID }} }}"#,
                escape_graphql_string(sibling_id),
                escape_graphql_string(sibling_outcome)
            ))
            .await;
        assert!(!drained.has_errors(), "{:?}", drained.errors);
        let terminal = reconcile_failure_fixture(&node, &run.run_id, via_access).await;
        assert_eq!(
            terminal.status,
            if cancel_after_latch {
                "cancelled"
            } else {
                "failed"
            }
        );
        assert_eq!(terminal.active_request_count, 0);
        if cancel_after_latch {
            assert!(terminal.error.is_none());
            assert_eq!(
                terminal.cancellation_reason.as_deref(),
                Some("operator cancelled after primary cause was committed")
            );
        } else {
            assert_eq!(terminal.error.as_ref(), Some(&primary));
        }
        let again = reconcile_failure_fixture(&node, &run.run_id, via_access).await;
        assert_eq!(again.error, terminal.error);
        assert_eq!(again.update_generation, terminal.update_generation);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn graph_fail_fast_latches_cause_before_interrupted_earlier_sibling() {
        assert_fail_fast_keeps_committed_primary_cause(
            "z-cause",
            "a-sibling",
            "interrupted",
            false,
            false,
        )
        .await;
    }

    #[tokio::test]
    async fn graph_fail_fast_keeps_cause_when_earlier_sibling_later_fails() {
        assert_fail_fast_keeps_committed_primary_cause(
            "z-cause",
            "a-sibling",
            "failed",
            false,
            false,
        )
        .await;
    }

    #[tokio::test]
    async fn graph_fail_fast_latch_does_not_depend_on_request_lexical_order() {
        assert_fail_fast_keeps_committed_primary_cause(
            "a-cause",
            "z-sibling",
            "interrupted",
            false,
            false,
        )
        .await;
    }

    #[tokio::test]
    async fn graph_fail_fast_config_access_preserves_durable_primary_cause() {
        assert_fail_fast_keeps_committed_primary_cause(
            "z-cause",
            "a-sibling",
            "interrupted",
            true,
            false,
        )
        .await;
    }

    #[tokio::test]
    async fn graph_fail_fast_cancel_after_latch_wins_over_primary_failure() {
        assert_fail_fast_keeps_committed_primary_cause(
            "z-cause",
            "a-sibling",
            "interrupted",
            false,
            true,
        )
        .await;
    }

    #[tokio::test]
    async fn graph_fail_fast_config_access_cancel_after_latch_clears_error() {
        assert_fail_fast_keeps_committed_primary_cause(
            "z-cause",
            "a-sibling",
            "interrupted",
            true,
            true,
        )
        .await;
    }
}
