//! Child of graph_pipeline::run: one derived workspace observation for both
//! preflight and the existing native request publication transaction.
use super::*;
use crate::lifecycle::WorkspaceLineage;
use crate::request_admission::SIGNED_REQUEST_FIELDS;

pub(crate) struct GraphWorkspaceResolution {
    pub(crate) lineage: WorkspaceLineage,
    source: WorkspaceLineage,
    explicit: WorkspaceLineage,
    authority: Option<String>,
    bootstrap: bool,
    // Existing GraphRun owner consumes this same observation for generation CAS.
    pub(super) run: Value,
    pub(super) digest: String,
}

fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn matches_hint(hint: &Option<String>, expected: &Option<String>) -> bool {
    present(hint.as_deref()).is_none_or(|value| Some(value) == present(expected.as_deref()))
}

fn validate_tuple(lineage: &WorkspaceLineage) -> Result<()> {
    anyhow::ensure!(
        lineage.workspace_id.is_some() == lineage.workspace_owner_deployment_id.is_some(),
        "graph workspace identity requires both workspace and owner"
    );
    anyhow::ensure!(
        lineage.workspace_id.is_some() || lineage.workspace_seal_hash.is_none(),
        "unbound graph workspace cannot carry a seal"
    );
    Ok(())
}

fn explicit_matches(
    explicit: &WorkspaceLineage,
    source: &WorkspaceLineage,
    authority: Option<&str>,
) -> bool {
    matches_hint(&explicit.workspace_id, &source.workspace_id)
        && matches_hint(
            &explicit.workspace_owner_deployment_id,
            &source.workspace_owner_deployment_id,
        )
        && matches_hint(&explicit.workspace_seal_hash, &source.workspace_seal_hash)
        && present(explicit.workspace_authority.as_deref())
            .is_none_or(|value| Some(value) == authority)
}

fn from_row(row: &AgentRequestRow) -> WorkspaceLineage {
    WorkspaceLineage {
        workspace_id: row.workspace_id.clone(),
        workspace_authority: row.workspace_authority.clone(),
        workspace_owner_deployment_id: row.workspace_owner_deployment_id.clone(),
        workspace_seal_hash: row.workspace_seal_hash.clone(),
    }
}

fn from_input(input: &Value) -> Result<WorkspaceLineage> {
    let field = |name: &str| -> Result<Option<String>> {
        match input.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(present(Some(value)).map(str::to_owned)),
            _ => anyhow::bail!("graph input workspace field {name} must be a string"),
        }
    };
    Ok(WorkspaceLineage {
        workspace_id: field("workspace_id")?,
        workspace_authority: field("workspace_authority")?,
        workspace_owner_deployment_id: field("workspace_owner_deployment_id")?,
        workspace_seal_hash: field("workspace_seal_hash")?,
    })
}

async fn stamp_from_workspace_owner(
    executor: &(impl GraphRunQuery + ?Sized),
    lineage: &mut WorkspaceLineage,
) -> Result<()> {
    let Some(workspace_id) = lineage.workspace_id.as_deref() else {
        return Ok(());
    };
    let response = executor
        .execute_graph_query(&crate::workspace::isolated_workspace_record_query(
            workspace_id,
        ))
        .await?;
    let workspace = crate::workspace::decode_isolated_workspace_record_response(&response)?
        .context("isolated workspace for graph entry is missing")?;
    anyhow::ensure!(
        lineage.workspace_owner_deployment_id.as_deref()
            == Some(workspace.owner_deployment_id.as_str()),
        "graph input workspace owner mismatch"
    );
    crate::workspace::apply_workspace_lineage_stamp(lineage, &workspace)
}

pub(crate) async fn derive_graph_workspace(
    executor: &(impl GraphRunQuery + ?Sized),
    trigger_id: &str,
    correlation: Option<&str>,
    target_did: &str,
    source_doc_id: Option<&str>,
    explicit: &WorkspaceLineage,
) -> Result<Option<GraphWorkspaceResolution>> {
    let Some(digest) = super::super::runtime::graph_artifact_revision_digest(trigger_id) else {
        anyhow::ensure!(
            !super::super::runtime::graph_artifact_is_reserved(trigger_id),
            "malformed reserved graph trigger ID"
        );
        return Ok(None);
    };
    let run_id = present(correlation).context("graph workspace requires run correlation")?;
    let runs = query_run_rows(executor, run_id).await?;
    if let Some(reason) = super::super::runtime::graph_publication_denial(&runs, &digest) {
        return Err(crate::trigger_engine::MaterializeSkip {
            reason: reason.to_owned(),
        }
        .into());
    }
    let run = runs
        .into_iter()
        .next()
        .expect("publication policy requires one run");
    anyhow::ensure!(
        required_string(&run, "correlation")? == run_id,
        "graph correlation differs from durable run identity"
    );
    let owner = required_string(&run, "owner_did")?;
    anyhow::ensure!(
        target_did == owner,
        "graph request principal is not its pinned owner"
    );
    let plan = load_plan(executor, &digest, owner).await?;
    anyhow::ensure!(
        required_string(&run, "graph_id")? == plan.graph_id,
        "graph run differs from its pinned plan"
    );
    let routes = planned_trigger_nodes(&plan)?;
    let node_id = routes
        .get(trigger_id)
        .context("graph request trigger is not a pinned route")?;
    let authority = super::super::runtime::planned_workspace_authority(&plan, node_id);
    let entry = plan
        .entries
        .iter()
        .find(|entry| run.get("entry_name").and_then(Value::as_str) == Some(entry.name.as_str()))
        .context("graph run selected entry is absent from its pinned plan")?;
    let entry_route = graph_trigger_id(
        &digest,
        &format!(
            "entry:{}:{}:{}",
            entry.name, entry.to.node_id, entry.to.port
        ),
    )?;
    let source = if trigger_id == entry_route {
        validate_collection_identifier(&entry.collection)?;
        validate_collection_identifier(&entry.correlation_field)?;
        let response = executor
            .execute_graph_query(&format!(
                "{{ {}(filter: {{ {}: {{ _eq: \"{}\" }} }}, limit: 2) {{ _docID }} }}",
                entry.collection,
                entry.correlation_field,
                escape_graphql_string(run_id),
            ))
            .await?;
        let [seed] = rows(&response, &entry.collection) else {
            anyhow::bail!("graph entry seed is absent or ambiguous");
        };
        anyhow::ensure!(
            present(source_doc_id).is_some()
                && seed.get("_docID").and_then(Value::as_str) == present(source_doc_id),
            "graph entry request does not name the pinned seed observation"
        );
        let input: Value = serde_json::from_str(required_string(&run, "input_json")?)?;
        let controller = from_input(&input)?;
        validate_tuple(&controller)?;
        controller
    } else {
        let response = executor.execute_graph_query(&format!(
            "{{ AgentRequest(filter: {{ caused_by_correlation: {{ _eq: \"{}\" }}, caused_by_trigger_id: {{ _eq: \"{}\" }} }}) {{ {SIGNED_REQUEST_FIELDS} }} }}",
            escape_graphql_string(run_id), escape_graphql_string(&entry_route),
        )).await?;
        let candidates: Vec<AgentRequestRow> =
            serde_json::from_value(Value::Array(rows(&response, "AgentRequest").to_vec()))?;
        let roots = candidates
            .iter()
            .filter(|row| super::logical_invocation::authentic_root(row, owner))
            .collect::<Vec<_>>();
        let [root] = roots.as_slice() else {
            anyhow::bail!(
                "graph workspace requires exactly one authenticated selected-entry request"
            );
        };
        let lineage = from_row(root);
        validate_tuple(&lineage)?;
        lineage
    };
    let lineage = if authority.is_none() {
        WorkspaceLineage::default()
    } else {
        WorkspaceLineage {
            workspace_authority: authority.map(str::to_owned),
            ..source.clone()
        }
    };
    Ok(Some(GraphWorkspaceResolution {
        lineage,
        source,
        explicit: explicit.clone(),
        authority: authority.map(str::to_owned),
        bootstrap: trigger_id == entry_route,
        run,
        digest,
    }))
}

/// Complete workspace-owner validation after the materializer has applied the
/// existing locality predicate. Native publication calls the same finalizer.
pub(crate) async fn finalize_graph_workspace(
    executor: &(impl GraphRunQuery + ?Sized),
    mut resolution: GraphWorkspaceResolution,
) -> Result<GraphWorkspaceResolution> {
    let authority = resolution.authority.as_deref();
    let mut stamped = WorkspaceLineage {
        workspace_authority: resolution.authority.clone(),
        ..resolution.source.clone()
    };
    if resolution.bootstrap {
        stamp_from_workspace_owner(executor, &mut stamped).await?;
        anyhow::ensure!(
            explicit_matches(&resolution.source, &stamped, authority),
            "graph controller workspace input conflicts with owner stamp or destination"
        );
    } else if authority.is_some() && stamped.workspace_id.is_some() {
        let inherited_seal = stamped.workspace_seal_hash.clone();
        stamp_from_workspace_owner(executor, &mut stamped).await?;
        anyhow::ensure!(
            stamped.workspace_seal_hash == inherited_seal,
            "current workspace stamp differs from immutable entry seal"
        );
    }
    anyhow::ensure!(
        explicit_matches(&resolution.explicit, &stamped, authority),
        "explicit graph workspace conflicts with authenticated entry or pinned authority"
    );
    resolution.lineage = if authority.is_none() {
        WorkspaceLineage::default()
    } else {
        stamped
    };
    Ok(resolution)
}

/// Public composed resolver for native publication and production consumers.
pub(crate) async fn resolve_graph_workspace(
    executor: &(impl GraphRunQuery + ?Sized),
    trigger_id: &str,
    correlation: Option<&str>,
    target_did: &str,
    source_doc_id: Option<&str>,
    explicit: &WorkspaceLineage,
) -> Result<Option<GraphWorkspaceResolution>> {
    match derive_graph_workspace(
        executor,
        trigger_id,
        correlation,
        target_did,
        source_doc_id,
        explicit,
    )
    .await?
    {
        Some(resolution) => Ok(Some(finalize_graph_workspace(executor, resolution).await?)),
        None => Ok(None),
    }
}

/// Revalidate the already signed tuple and share the already loaded run with
/// its existing generation-write owner. The caller stages request writes in txn.
pub(crate) async fn fence_root_workspace_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<()> {
    let Some(trigger) = request.caused_by_trigger_id.as_deref() else {
        return Ok(());
    };
    let explicit = WorkspaceLineage {
        workspace_id: request.workspace_id.clone(),
        workspace_authority: request.workspace_authority.clone(),
        workspace_owner_deployment_id: request.workspace_owner_deployment_id.clone(),
        workspace_seal_hash: request.workspace_seal_hash.clone(),
    };
    let Some(resolved) = resolve_graph_workspace(
        txn,
        trigger,
        request.caused_by_correlation.as_deref(),
        &request.agent_did,
        request.caused_by_source_doc_id.as_deref(),
        &explicit,
    )
    .await?
    else {
        return Ok(());
    };
    // Planned authority without a workspace is projection evidence only;
    // RequestSpec's workspace_ref(None) serializes the unbound physical tuple.
    let expected = if resolved.lineage.workspace_id.is_some() {
        resolved.lineage
    } else {
        WorkspaceLineage::default()
    };
    anyhow::ensure!(
        explicit.workspace_id == expected.workspace_id
            && explicit.workspace_authority == expected.workspace_authority
            && explicit.workspace_owner_deployment_id == expected.workspace_owner_deployment_id
            && explicit.workspace_seal_hash == expected.workspace_seal_hash,
        "signed graph workspace tuple differs from resolved publication evidence"
    );
    super::super::runtime::fence_observed_graph_publication_in_txn(
        txn,
        &resolved.run,
        &resolved.digest,
    )
    .await
}
