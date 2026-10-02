//! Child of graph_pipeline::run: one derived workspace observation for both
//! preflight and the existing native request publication transaction.
use super::super::StageTarget;
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
            &explicit.workspace_owner_agent_did,
            &source.workspace_owner_agent_did,
        )
        && matches_hint(&explicit.workspace_seal_hash, &source.workspace_seal_hash)
        && present(explicit.workspace_authority.as_deref())
            .is_none_or(|value| Some(value) == authority)
}

fn from_row(row: &AgentRequestRow) -> WorkspaceLineage {
    WorkspaceLineage {
        workspace_id: row.workspace_id.clone(),
        workspace_owner_agent_did: row.workspace_owner_agent_did.clone(),
        workspace_authority: row.workspace_authority.clone(),
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
        workspace_owner_agent_did: None,
        workspace_authority: field("workspace_authority")?,
        workspace_seal_hash: field("workspace_seal_hash")?,
    })
}

async fn stamp_from_workspace_owner(
    executor: &(impl GraphRunQuery + ?Sized),
    lineage: &mut WorkspaceLineage,
    owner: &str,
) -> Result<()> {
    let Some(workspace_id) = lineage.workspace_id.as_deref() else {
        return Ok(());
    };
    let response = executor
        .execute_graph_query(&crate::workspace::isolated_workspace_record_query(
            workspace_id,
            owner,
        ))
        .await?;
    let workspace = crate::workspace::decode_isolated_workspace_record_response(&response)?
        .context("isolated workspace for graph entry is missing")?;
    anyhow::ensure!(
        workspace.owner_agent_did == owner,
        "graph input workspace owner mismatch"
    );
    crate::workspace::apply_workspace_lineage_stamp(lineage, &workspace)
}

struct VerifiedGraphContext {
    run: Value,
    plan: GraphPlan,
    digest: String,
    node_id: String,
}

async fn load_verified_graph_context(
    executor: &(impl GraphRunQuery + ?Sized),
    trigger_id: &str,
    correlation: Option<&str>,
    target_did: &str,
) -> Result<Option<VerifiedGraphContext>> {
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
        .context("graph request trigger is not a pinned route")?
        .clone();
    Ok(Some(VerifiedGraphContext {
        run,
        plan,
        digest,
        node_id,
    }))
}

pub(crate) async fn derive_graph_workspace(
    executor: &(impl GraphRunQuery + ?Sized),
    trigger_id: &str,
    correlation: Option<&str>,
    target_did: &str,
    source_doc_id: Option<&str>,
    explicit: &WorkspaceLineage,
) -> Result<Option<GraphWorkspaceResolution>> {
    let Some(VerifiedGraphContext {
        run,
        plan,
        digest,
        node_id,
    }) = load_verified_graph_context(executor, trigger_id, correlation, target_did).await?
    else {
        return Ok(None);
    };
    let run_id = required_string(&run, "correlation")?;
    let owner = required_string(&run, "owner_did")?;
    let authority = super::super::runtime::planned_workspace_authority(&plan, &node_id);
    let entry = plan
        .entries
        .iter()
        .find(|entry| run.get("entry_name").and_then(Value::as_str) == Some(entry.name.as_str()))
        .context("graph run selected entry is absent from its pinned plan")?;
    let entry_route = crate::graph_pipeline::routes::entry_route_id(&digest, entry)?;
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
        lineage.require_authority_if_workspace_id()?;
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
/// existing workspace eligibility checks. Native publication calls the same finalizer.
pub(crate) async fn finalize_graph_workspace(
    executor: &(impl GraphRunQuery + ?Sized),
    mut resolution: GraphWorkspaceResolution,
) -> Result<GraphWorkspaceResolution> {
    let authority = resolution.authority.as_deref();
    let owner = required_string(&resolution.run, "owner_did")?;
    let mut stamped = WorkspaceLineage {
        workspace_authority: resolution.authority.clone(),
        ..resolution.source.clone()
    };
    if resolution.bootstrap {
        stamp_from_workspace_owner(executor, &mut stamped, owner).await?;
        anyhow::ensure!(
            explicit_matches(&resolution.source, &stamped, authority),
            "graph controller workspace input conflicts with owner stamp or destination"
        );
    } else if authority.is_some() && stamped.workspace_id.is_some() {
        let inherited_seal = stamped.workspace_seal_hash.clone();
        let workspace_owner = stamped
            .workspace_owner_agent_did
            .clone()
            .context("authenticated entry lacks workspace owner scope")?;
        stamp_from_workspace_owner(executor, &mut stamped, &workspace_owner).await?;
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
        workspace_owner_agent_did: request.workspace_owner_agent_did.clone(),
        workspace_authority: request.workspace_authority.clone(),
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
    if let Some(session_id) = resolve_graph_session(
        txn,
        trigger,
        request.caused_by_correlation.as_deref(),
        &request.agent_did,
    )
    .await?
    {
        anyhow::ensure!(
            request.session_id == session_id,
            "signed graph session differs from resolved continuation target"
        );
    }
    // Planned authority without a workspace is projection evidence only;
    // RequestSpec's workspace_ref(None) serializes the unbound physical tuple.
    let expected = if resolved.lineage.workspace_id.is_some() {
        resolved.lineage
    } else {
        WorkspaceLineage::default()
    };
    anyhow::ensure!(
        explicit.workspace_id == expected.workspace_id
            && explicit.workspace_owner_agent_did == expected.workspace_owner_agent_did
            && explicit.workspace_authority == expected.workspace_authority
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTargetRoute {
    SelectedEntry,
    Grouped,
    PerDocument,
}

pub(crate) struct SessionTargetEligibility {
    pub source_is_task: bool,
    pub target_exists: bool,
    pub target_is_task: bool,
    pub route_count: usize,
    pub route_kind: SessionTargetRoute,
}

pub(crate) fn session_target_eligible(eligibility: &SessionTargetEligibility) -> bool {
    eligibility.source_is_task
        && eligibility.target_exists
        && eligibility.target_is_task
        && eligibility.route_count == 1
        && eligibility.route_kind != SessionTargetRoute::PerDocument
}

pub(crate) struct SessionContinuationContext {
    pub owner: String,
    pub firing_node: String,
    pub target_node: String,
    pub correlation: String,
    pub revision: String,
    pub target_route: String,
    pub run_and_plan_verified: bool,
    pub destination_route_verified: bool,
}

pub(crate) struct SessionRootCandidate {
    #[cfg(test)]
    pub root_doc_id: String,
    pub session_id: String,
    pub owner: String,
    pub correlation: String,
    pub revision: String,
    pub target_route: String,
    pub authenticated: bool,
}

pub(crate) struct SessionSelection {
    pub session_id: String,
    #[cfg(test)]
    pub root_doc_id: String,
    #[cfg(test)]
    pub firing_node: String,
}

pub(crate) fn select_graph_session(
    eligibility: &SessionTargetEligibility,
    context: &SessionContinuationContext,
    candidates: &[SessionRootCandidate],
) -> Option<SessionSelection> {
    if !session_target_eligible(eligibility)
        || !context.run_and_plan_verified
        || !context.destination_route_verified
        || context.firing_node == context.target_node
    {
        return None;
    }
    let mut matching = candidates.iter().filter(|candidate| {
        candidate.authenticated
            && candidate.owner == context.owner
            && candidate.correlation == context.correlation
            && candidate.revision == context.revision
            && candidate.target_route == context.target_route
    });
    let candidate = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some(SessionSelection {
        session_id: candidate.session_id.clone(),
        #[cfg(test)]
        root_doc_id: candidate.root_doc_id.clone(),
        #[cfg(test)]
        firing_node: context.firing_node.clone(),
    })
}

pub(crate) async fn resolve_graph_session(
    executor: &(impl GraphRunQuery + ?Sized),
    trigger_id: &str,
    correlation: Option<&str>,
    target_did: &str,
) -> Result<Option<String>> {
    let Some(verified) =
        load_verified_graph_context(executor, trigger_id, correlation, target_did).await?
    else {
        return Ok(None);
    };
    let node = verified
        .plan
        .nodes
        .iter()
        .find(|node| node.node_id == verified.node_id)
        .context("pinned firing node is absent from its graph plan")?;
    let Some(selection) = &node.session else {
        return Ok(None);
    };
    let target_node = verified
        .plan
        .nodes
        .iter()
        .find(|node| node.node_id == selection.continue_node_id)
        .context("graph session target is absent from its pinned plan")?;
    let entry_name = required_string(&verified.run, "entry_name")?;
    let entry = verified
        .plan
        .entries
        .iter()
        .find(|entry| entry.name == entry_name);
    let selected_entry = entry.filter(|entry| entry.to.node_id == target_node.node_id);
    let incoming = verified
        .plan
        .edges
        .iter()
        .enumerate()
        .filter(|(_, edge)| edge.to.node_id == target_node.node_id)
        .collect::<Vec<_>>();
    let target_route = if let Some(entry) = selected_entry {
        anyhow::ensure!(
            incoming.is_empty(),
            "graph session entry target has additional routes"
        );
        crate::graph_pipeline::routes::entry_route_id(&verified.digest, entry)?
    } else {
        let [(index, edge)] = incoming.as_slice() else {
            anyhow::bail!("graph session target is not singleton")
        };
        anyhow::ensure!(
            edge.delivery.is_some(),
            "graph session target is per-document fan-out"
        );
        crate::graph_pipeline::routes::edge_route_id(&verified.digest, *index, edge)?
    };
    let eligibility = SessionTargetEligibility {
        source_is_task: matches!(node.target, StageTarget::Task { .. }),
        target_exists: true,
        target_is_task: matches!(target_node.target, StageTarget::Task { .. }),
        route_count: 1,
        route_kind: if selected_entry.is_some() {
            SessionTargetRoute::SelectedEntry
        } else {
            SessionTargetRoute::Grouped
        },
    };
    let context = SessionContinuationContext {
        owner: target_did.into(),
        firing_node: node.node_id.clone(),
        target_node: target_node.node_id.clone(),
        correlation: required_string(&verified.run, "correlation")?.into(),
        revision: verified.digest,
        target_route,
        run_and_plan_verified: true,
        destination_route_verified: true,
    };
    let response = executor
        .execute_graph_query(&format!(
            r#"{{AgentRequest(filter: {{
        caused_by_correlation: {{_eq: "{}"}}, caused_by_trigger_id: {{_eq: "{}"}}
    }}) {{ {SIGNED_REQUEST_FIELDS} }} }}"#,
            escape_graphql_string(&context.correlation),
            escape_graphql_string(&context.target_route)
        ))
        .await?;
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(Value::Array(rows(&response, "AgentRequest").to_vec()))?;
    let candidates = rows
        .iter()
        .map(|row| SessionRootCandidate {
            #[cfg(test)]
            root_doc_id: row.doc_id.clone().unwrap_or_default(),
            session_id: row.session_id.clone().unwrap_or_default(),
            owner: row.agent_did.clone().unwrap_or_default(),
            correlation: row.caused_by_correlation.clone().unwrap_or_default(),
            revision: row
                .caused_by_trigger_id
                .as_deref()
                .and_then(super::super::runtime::graph_artifact_revision_digest)
                .unwrap_or_default(),
            target_route: row.caused_by_trigger_id.clone().unwrap_or_default(),
            authenticated: super::logical_invocation::authentic_root(row, target_did),
        })
        .collect::<Vec<_>>();
    let selected = select_graph_session(&eligibility, &context, &candidates)
        .context("graph session continuation requires exactly one authenticated singleton root")?;
    Ok(Some(selected.session_id))
}
