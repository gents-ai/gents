use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;

use crate::request::lifecycle::WorkspaceLineage;
use crate::store::graphql::escape_graphql_string;
#[cfg(test)]
use crate::store::graphql::graphql_with_transaction_retry;
use crate::tools::native::WorkspaceAuthority;
use gents_protocol::row::AgentRequest;

use super::action_plan::{
    emit_cleanup_workspace_plan, emit_integrate_workspace_plan, emit_seal_workspace_plan,
    CleanupWorkspaceAction, IntegrateWorkspaceAction, SealWorkspaceAction, CAP_CLEANUP_WORKSPACE,
    CAP_INTEGRATE_WORKSPACE, CAP_SEAL_WORKSPACE,
};
use super::binding::{admit_workspace_binding, new_binding, release_binding, AdmitBinding};
use super::documents::{
    workspace_cleanup_docs_mutation, workspace_integrate_docs_mutation,
    workspace_seal_docs_mutation, IsolatedWorkspaceDoc, MemoryWorkspaceDocuments,
    WorkspaceBindingDoc, WorkspaceDocuments, WorkspacePlacementDoc, WorkspaceReceiptDoc,
    LIFECYCLE_SEALED, RECEIPT_KIND_WRITER,
};
use super::executor::cleanup::{execute_cleanup_workspace_plan, CleanupWorkspaceOutcome};
use super::executor::integrate::{
    clear_integrate_journal, execute_integrate_workspace_plan, finalize_integrate_trunk,
    IntegrateWorkspaceOutcome,
};
use super::executor::seal::{execute_seal_workspace_plan, SealWorkspaceOutcome};
use super::executor::{HostExecutorContext, RepositoryPlacementRef};
use super::overlay::{optional_id, IsolatedWorkspaceRecord};

const ISOLATED_WORKSPACE_FIELDS: &str = r#"
    workspace_id
    work_unit_id
    repository_id
    base_sha
    branch
    creation_policy
    adapter
    owner_node_did
    writer_principal
    integrator_principal
    instruction_manifest
    path_capability
    seal_hash
    lifecycle_state
    caused_by_invocation_id
    caused_by_correlation
"#;

const PLACEMENT_FIELDS: &str = r#"
    workspace_id
    owner_node_did
    host_path
    repository_placement_id
    adapter
    adapter_version
    dirty_base
    dirty_base_summary
    provisioning_state
    observed_tree_hash
"#;

pub(super) fn completion_access(
    node: &std::sync::Arc<EmbeddedNode>,
    identity: &dyn crate::store::identity::NodeIdentity,
    selected_agent: &str,
) -> Result<crate::ConfigAccess> {
    anyhow::ensure!(
        !selected_agent.is_empty() && identity.did() == selected_agent,
        "workspace completion requires the selected Agent identity"
    );
    Ok(crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(selected_agent)?,
        identity,
    )?)
}

pub async fn stamp_workspace_lineage(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_identity: &dyn crate::store::identity::NodeIdentity,
    node_did: &str,
    lineage: &mut WorkspaceLineage,
) -> Result<()> {
    let Some(workspace_id) = lineage
        .workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let owner = lineage
        .workspace_owner_node_did
        .as_deref()
        .unwrap_or(node_did);
    let access = completion_access(node, agent_identity, agent_identity.did())?;
    let workspace =
        super::overlay::load_isolated_workspace_record_with_access(&access, workspace_id, owner)
            .await?
            .ok_or_else(|| anyhow::anyhow!("isolated workspace {workspace_id} not found"))?;
    apply_workspace_lineage_stamp(lineage, &workspace)
}

pub(crate) fn apply_workspace_lineage_stamp(
    lineage: &mut WorkspaceLineage,
    workspace: &IsolatedWorkspaceRecord,
) -> Result<()> {
    anyhow::ensure!(
        lineage.workspace_id.as_deref() == Some(workspace.workspace_id.as_str()),
        "workspace stamp identity differs from resolved workspace"
    );
    anyhow::ensure!(
        lineage
            .workspace_owner_node_did
            .as_deref()
            .is_none_or(|owner| owner == workspace.owner_node_did),
        "workspace stamp owner differs from resolved workspace"
    );
    lineage.workspace_owner_node_did = Some(workspace.owner_node_did.clone());
    let state =
        crate::tools::native::normalize_workspace_lifecycle_state(&workspace.lifecycle_state);
    let authority = lineage
        .workspace_authority
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(WorkspaceAuthority::parse)
        .transpose()?;
    match (state, authority) {
        (Some("sealed"), Some(WorkspaceAuthority::ReadWrite)) => anyhow::bail!(
            "isolated workspace {} is sealed; ReadWrite bindings are illegal",
            workspace.workspace_id
        ),
        (Some("sealed"), _) => {
            let Some(hash) = optional_id(workspace.seal_hash.as_deref()) else {
                anyhow::bail!(
                    "sealed workspace {} is missing seal_hash",
                    workspace.workspace_id
                );
            };
            match lineage
                .workspace_seal_hash
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                None => lineage.workspace_seal_hash = Some(hash.to_string()),
                Some(existing) if existing == hash => {}
                Some(existing) => anyhow::bail!(
                    "request workspace_seal_hash {existing} does not match workspace seal_hash {hash}"
                ),
            }
        }
        (Some("ready"), Some(WorkspaceAuthority::Integrate)) => anyhow::bail!(
            "isolated workspace {} is ready; Integrate bindings require Sealed",
            workspace.workspace_id
        ),
        (Some("ready"), _) => {}
        (other, Some(authority)) => anyhow::bail!(
            "isolated workspace {} in state {} is not bindable for authority {}",
            workspace.workspace_id,
            other.unwrap_or(workspace.lifecycle_state.as_str()),
            authority.as_str()
        ),
        _ => {}
    }
    Ok(())
}

pub async fn seal_on_writer_success(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_identity: &dyn crate::store::identity::NodeIdentity,
    request: &AgentRequest,
    operator_tool_root: Option<&Path>,
) -> Result<()> {
    let Some(workspace_id) = optional_id(request.workspace_id.as_deref()) else {
        return Ok(());
    };
    let authority = match request.workspace_authority.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => WorkspaceAuthority::parse(value)?,
        _ => return Ok(()),
    };
    if !matches!(authority, WorkspaceAuthority::ReadWrite) {
        return Ok(());
    }
    let access = completion_access(node, agent_identity, &request.agent_did)?;

    let mut docs = load_docs_with_access(
        &access,
        workspace_id,
        super::overlay::request_workspace_owner(request)?,
    )
    .await?;
    let workspace = docs
        .load_isolated_workspace(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("isolated workspace {workspace_id} not found"))?;
    anyhow::ensure!(
        workspace.writer_principal.trim() == request.agent_did.trim(),
        "selected Agent is not the workspace writer"
    );
    docs.load_placement(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("workspace placement {workspace_id} not found"))?;
    let repository =
        load_repository_with_access(&access, &workspace.repository_id, &workspace.owner_node_did)
            .await?;

    let plan = emit_seal_workspace_plan(SealWorkspaceAction {
        workspace_id: workspace_id.to_string(),
        produced_by_request_id: request.request_id.clone(),
        produced_by_request_doc_id: request.doc_id.clone(),
    });
    let mut journal = Vec::new();
    let mut capabilities = BTreeSet::new();
    capabilities.insert(CAP_SEAL_WORKSPACE.to_string());
    let outcome = {
        let mut ctx = HostExecutorContext {
            owner_node_did: workspace.owner_node_did.clone(),
            repository,
            ceiling: operator_tool_root,
            capabilities,
            writer_principal: workspace.writer_principal.clone(),
            integrator_principal: workspace.integrator_principal.clone(),
            caused_by_invocation_id: workspace.caused_by_invocation_id.clone(),
            caused_by_correlation: workspace.caused_by_correlation.clone(),
            documents: &mut docs,
        };
        execute_seal_workspace_plan(&plan, &mut journal, &mut ctx)
            .map_err(|err| anyhow::anyhow!("{err}"))?
    };
    flush_seal_outcome(&access, &docs, &outcome).await
}

pub async fn integrate_on_integrator_success(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_identity: &dyn crate::store::identity::NodeIdentity,
    request: &AgentRequest,
    operator_tool_root: Option<&Path>,
) -> Result<()> {
    let Some(workspace_id) = optional_id(request.workspace_id.as_deref()) else {
        return Ok(());
    };
    let authority = match request.workspace_authority.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => WorkspaceAuthority::parse(value)?,
        _ => return Ok(()),
    };
    if !matches!(authority, WorkspaceAuthority::Integrate) {
        return Ok(());
    }
    let access = completion_access(node, agent_identity, &request.agent_did)?;

    let mut docs = load_docs_with_access(
        &access,
        workspace_id,
        super::overlay::request_workspace_owner(request)?,
    )
    .await?;
    let workspace = docs
        .load_isolated_workspace(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("isolated workspace {workspace_id} not found"))?;
    anyhow::ensure!(
        workspace.integrator_principal.trim() == request.agent_did.trim(),
        "selected Agent is not the workspace integrator"
    );
    docs.load_placement(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("workspace placement {workspace_id} not found"))?;
    let repository =
        load_repository_with_access(&access, &workspace.repository_id, &workspace.owner_node_did)
            .await?;

    let plan = emit_integrate_workspace_plan(IntegrateWorkspaceAction {
        workspace_id: workspace_id.to_string(),
        produced_by_request_id: request.request_id.clone(),
        produced_by_request_doc_id: request.doc_id.clone(),
        mode: super::IntegrateMode::ApplyDiff,
    });
    let mut journal = Vec::new();
    let mut capabilities = BTreeSet::new();
    capabilities.insert(CAP_INTEGRATE_WORKSPACE.to_string());
    let trunk = PathBuf::from(&ctx_repository_path(&repository));
    let outcome = {
        let mut ctx = HostExecutorContext {
            owner_node_did: workspace.owner_node_did.clone(),
            repository,
            ceiling: operator_tool_root,
            capabilities,
            writer_principal: workspace.writer_principal.clone(),
            integrator_principal: workspace.integrator_principal.clone(),
            caused_by_invocation_id: workspace.caused_by_invocation_id.clone(),
            caused_by_correlation: workspace.caused_by_correlation.clone(),
            documents: &mut docs,
        };
        execute_integrate_workspace_plan(&plan, &mut journal, &mut ctx)
            .map_err(|err| anyhow::anyhow!("{err}"))?
    };
    flush_integrate_outcome(&access, &outcome).await?;
    finalize_integrate_trunk(&trunk, outcome.pending_head_sha.as_deref())?;
    release_integrate_binding(&access, &docs, &outcome).await?;
    clear_integrate_journal(&trunk, workspace_id);
    Ok(())
}

fn ctx_repository_path(repository: &RepositoryPlacementRef) -> PathBuf {
    repository.host_path.clone()
}

/// Explicit operator/ack cleanup. Never invoked from request terminal.
pub async fn cleanup_workspace(
    access: &crate::ConfigAccess,
    node_did: &str,
    workspace_id: &str,
    operator_tool_root: Option<&Path>,
) -> Result<()> {
    let workspace_id = optional_id(Some(workspace_id))
        .ok_or_else(|| anyhow::anyhow!("cleanup_workspace requires a workspace_id"))?;
    claim_workspace_cleanup(access, workspace_id, node_did).await?;
    let mut docs = load_docs_with_access(access, workspace_id, node_did).await?;
    let workspace = docs
        .load_isolated_workspace(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("isolated workspace {workspace_id} not found"))?;
    docs.load_placement(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("workspace placement {workspace_id} not found"))?;
    let repository =
        load_repository_with_access(access, &workspace.repository_id, &workspace.owner_node_did)
            .await?;

    let plan = emit_cleanup_workspace_plan(CleanupWorkspaceAction {
        workspace_id: workspace_id.to_string(),
    });
    let mut journal = Vec::new();
    let mut capabilities = BTreeSet::new();
    capabilities.insert(CAP_CLEANUP_WORKSPACE.to_string());
    let outcome = {
        let mut ctx = HostExecutorContext {
            owner_node_did: workspace.owner_node_did.clone(),
            repository,
            ceiling: operator_tool_root,
            capabilities,
            writer_principal: workspace.writer_principal.clone(),
            integrator_principal: workspace.integrator_principal.clone(),
            caused_by_invocation_id: workspace.caused_by_invocation_id.clone(),
            caused_by_correlation: workspace.caused_by_correlation.clone(),
            documents: &mut docs,
        };
        execute_cleanup_workspace_plan(&plan, &mut journal, &mut ctx)
            .map_err(|err| anyhow::anyhow!("{err}"))?
    };
    flush_cleanup_outcome(access, &docs, &outcome).await
}

pub(super) async fn claim_workspace_cleanup(
    access: &crate::ConfigAccess,
    workspace_id: &str,
    owner: &str,
) -> Result<()> {
    access.transact_write("workspace.claim_cleanup", |txn| Box::pin(async move {
        let response = txn.execute(&super::overlay::isolated_workspace_record_query(workspace_id, owner)).await?;
        let rows = response["data"]["IsolatedWorkspace"].as_array().context("workspace cleanup query omitted rows")?;
        let [row] = rows.as_slice() else { anyhow::bail!("cleanup workspace unavailable or ambiguous") };
        let doc = row["_docID"].as_str().filter(|id| !id.is_empty()).context("cleanup workspace physical ID missing")?;
        let state = row["lifecycle_state"].as_str().context("cleanup workspace state missing")?;
        let normalized = crate::tools::native::normalize_workspace_lifecycle_state(state);
        anyhow::ensure!(matches!(normalized, Some("sealed" | "cleaning" | "cleaned" | "provisionFailed")),
            "workspace in state {state} cannot be cleaned");
        let response = txn.execute(&super::overlay::workspace_bindings_for_parent_query(doc)).await?;
        let values = response.pointer("/data/WorkspaceBinding").context("cleanup binding query omitted rows")?;
        let bindings = super::overlay::validate_workspace_bindings(serde_json::from_value(values.clone())?, workspace_id)?;
        anyhow::ensure!(bindings.iter().all(|binding| binding.workspace_doc_id.as_deref() == Some(doc) && binding.owner_node_did == owner), "cleanup binding parent mismatch");
        anyhow::ensure!(!bindings.iter().any(WorkspaceBindingDoc::is_active), "workspace has Active binding(s); cleanup requires them Released first");
        let next = if normalized == Some("cleaned") { "cleaned" } else { "cleaning" };
        let response = txn.execute(&format!(r#"mutation {{update_IsolatedWorkspace(docID: "{}", input: {{lifecycle_state: "{next}", binding_revision: "{}"}}) {{_docID}}}}"#, escape_graphql_string(doc), uuid::Uuid::new_v4())).await?;
        anyhow::ensure!(crate::store::graphql::mutation_documents(&response, "update_IsolatedWorkspace")?.is_some_and(|rows| rows.len() == 1), "workspace cleanup claim was not authorized");
        Ok::<_, anyhow::Error>(())
    })).await
}

#[cfg(test)]
pub async fn writer_request_already_sealed(
    node: &std::sync::Arc<EmbeddedNode>,
    request: &AgentRequest,
) -> Result<bool> {
    let Some(workspace_id) = optional_id(request.workspace_id.as_deref()) else {
        return Ok(false);
    };
    let authority = match request.workspace_authority.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => WorkspaceAuthority::parse(value)?,
        _ => return Ok(false),
    };
    if !matches!(authority, WorkspaceAuthority::ReadWrite) {
        return Ok(false);
    }
    let Some(workspace) = super::overlay::load_isolated_workspace_record(
        node,
        workspace_id,
        super::overlay::request_workspace_owner(request)?,
    )
    .await?
    else {
        return Ok(false);
    };
    let receipts = load_receipts(node, workspace_id).await?;
    let bindings = super::overlay::load_workspace_bindings_for(
        node,
        workspace_id,
        super::overlay::request_workspace_owner(request)?,
    )
    .await?;
    Ok(writer_already_sealed(
        &workspace.lifecycle_state,
        &receipts,
        &bindings,
        &request.request_id,
        &request.doc_id,
    ))
}

pub(crate) async fn writer_request_already_sealed_with_identity(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_identity: &dyn crate::store::identity::NodeIdentity,
    request: &AgentRequest,
) -> Result<bool> {
    let Some(workspace_id) = optional_id(request.workspace_id.as_deref()) else {
        return Ok(false);
    };
    if request
        .workspace_authority
        .as_deref()
        .and_then(|value| WorkspaceAuthority::parse(value).ok())
        != Some(WorkspaceAuthority::ReadWrite)
    {
        return Ok(false);
    }
    let access = completion_access(node, agent_identity, &request.agent_did)?;
    let docs = load_docs_with_access(
        &access,
        workspace_id,
        super::overlay::request_workspace_owner(request)?,
    )
    .await?;
    let workspace = docs
        .load_isolated_workspace(workspace_id)?
        .context("writer workspace is unavailable")?;
    Ok(writer_already_sealed(
        &workspace.lifecycle_state,
        &docs.load_receipts(workspace_id)?,
        &docs.load_bindings(workspace_id)?,
        &request.request_id,
        &request.doc_id,
    ))
}

fn writer_already_sealed(
    lifecycle_state: &str,
    receipts: &[WorkspaceReceiptDoc],
    bindings: &[WorkspaceBindingDoc],
    request_id: &str,
    request_doc_id: &str,
) -> bool {
    crate::tools::native::normalize_workspace_lifecycle_state(lifecycle_state)
        == Some(LIFECYCLE_SEALED)
        && (receipts.iter().any(|receipt| {
            receipt.kind == RECEIPT_KIND_WRITER
                && receipt.produced_by_request_id == request_id
                && receipt.produced_by_request_doc_id == request_doc_id
        }) || bindings.iter().any(|binding| {
            binding.request_id == request_id
                && binding.request_doc_id == request_doc_id
                && binding.authority == WorkspaceAuthority::ReadWrite.as_str()
        }))
}

pub async fn materialize_workspace_binding(
    node: &std::sync::Arc<EmbeddedNode>,
    request_id: &str,
    request_doc_id: &str,
    agent_identity: &dyn crate::store::identity::NodeIdentity,
    lineage: &WorkspaceLineage,
) -> Result<()> {
    let Some(workspace_id) = lineage
        .workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let principal_did = agent_identity.did();
    let access = completion_access(node, agent_identity, principal_did)?;
    lineage.require_authority_if_workspace_id()?;
    let workspace_owner = lineage
        .workspace_owner_node_did
        .as_deref()
        .context("workspace owner is missing")?;
    let authority = match lineage.workspace_authority.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => WorkspaceAuthority::parse(value)?,
        _ => anyhow::bail!("workspace-bound request {workspace_id} is missing workspace_authority"),
    };
    access.transact_write("workspace.admit_binding", |txn| Box::pin(async move {
        let response = txn.execute(&format!(
            r#"{{ AgentRequest(filter: {{_docID: {{_eq:"{}"}}}},limit:1) {{request_id agent_did node_did workspace_id workspace_owner_node_did workspace_authority workspace_seal_hash}} }}"#,
            escape_graphql_string(request_doc_id))).await?;
        let values = response.pointer("/data/AgentRequest").context("workspace binding query omitted rows")?;
        let mut found: Vec<gents_protocol::row::AgentRequestRow> = serde_json::from_value(values.clone())?;
        anyhow::ensure!(found.len() == 1, "workspace binding request unavailable or ambiguous");
        let actual = found.remove(0);
        anyhow::ensure!(actual.request_id == request_id && actual.agent_did.as_deref() == Some(principal_did)
            && node.node_identity_did().is_some_and(|did| actual.node_did.as_deref() == Some(did))
            && actual.workspace_id.as_deref() == Some(workspace_id)
            && actual.workspace_owner_node_did == lineage.workspace_owner_node_did
            && actual.workspace_authority.as_deref() == Some(authority.as_str())
            && actual.workspace_seal_hash == lineage.workspace_seal_hash,
            "workspace binding must match the exact physical request Agent and lineage");
        let response = txn.execute(&super::overlay::isolated_workspace_record_query(workspace_id, workspace_owner)).await?;
        let rows = response.pointer("/data/IsolatedWorkspace").and_then(serde_json::Value::as_array).context("workspace query omitted rows")?;
        let [row] = rows.as_slice() else { anyhow::bail!("workspace is unavailable or ambiguous") };
        let workspace_doc = row["_docID"].as_str().filter(|id| !id.is_empty()).context("workspace physical ID missing")?;
        let workspace = super::overlay::decode_isolated_workspace_record(serde_json::from_value(row.clone())?)?;
        super::overlay::require_workspace_principal(&workspace, principal_did, authority)?;
        let response = txn.execute(&super::overlay::workspace_bindings_for_parent_query(workspace_doc)).await?;
        let values = response.pointer("/data/WorkspaceBinding").context("workspace binding query omitted rows")?;
        let existing = super::overlay::validate_workspace_bindings(serde_json::from_value(values.clone())?, workspace_id)?;
        anyhow::ensure!(existing.iter().all(|binding| binding.workspace_doc_id.as_deref() == Some(workspace_doc)
            && binding.owner_node_did == workspace_owner), "workspace binding has a different physical parent");
        let mut candidate = new_binding(workspace_id, request_id, request_doc_id, authority, workspace_owner,
            optional_id(lineage.workspace_seal_hash.as_deref()).or(optional_id(workspace.seal_hash.as_deref())));
        candidate.workspace_doc_id = Some(workspace_doc.to_owned());
        let release_previous = if matches!(authority, WorkspaceAuthority::ReadWrite | WorkspaceAuthority::Integrate) {
            let prior = super::overlay::previous_exclusive_binding(workspace_id, &existing, request_id,
                matches!(authority, WorkspaceAuthority::Integrate))?;
            if let Some(prior) = prior {
                let response = txn.execute(&super::overlay::request_liveness_query(&prior.request_doc_id)?).await?;
                let values = response.pointer("/data/AgentRequest").context("workspace request query omitted rows")?;
                let mut rows: Vec<gents_protocol::row::AgentRequestRow> = serde_json::from_value(values.clone())?;
                anyhow::ensure!(rows.len() == 1, "workspace binding request is unavailable or ambiguous");
                !super::overlay::validate_request_liveness(rows.remove(0), &prior.request_id, workspace_id, workspace_owner)?
            } else { false }
        } else { false };
        match admit_workspace_binding(workspace_id, &workspace.lifecycle_state,
            optional_id(workspace.seal_hash.as_deref()), &existing, candidate, release_previous)? {
            AdmitBinding::Reuse(_) => Ok(()),
            AdmitBinding::Create { binding, release } => {
                if matches!(authority, WorkspaceAuthority::ReadWrite | WorkspaceAuthority::Integrate) {
                    let revision = uuid::Uuid::new_v4().to_string();
                    let fence = format!(r#"mutation {{ update_IsolatedWorkspace(docID: "{}", input: {{binding_revision: "{}"}}) {{_docID}} }}"#,
                        escape_graphql_string(workspace_doc), revision);
                    let response = txn.execute(&fence).await?;
                    anyhow::ensure!(crate::store::graphql::mutation_documents(&response, "update_IsolatedWorkspace")?.is_some_and(|rows| rows.len() == 1), "workspace admission fence unavailable");
                }
                for released in &release {
                    txn.execute(&super::documents::workspace_binding_upsert_mutation(released)).await?;
                }
                let already_exists = existing.iter().any(|old| old.binding_id == binding.binding_id);
                let response = txn.execute(&super::documents::workspace_binding_upsert_mutation(&binding)).await?;
                let rows = crate::store::graphql::mutation_documents(&response, "upsert_WorkspaceBinding")?.context("workspace binding write omitted rows")?;
                let [row] = rows else { anyhow::bail!("workspace binding write did not affect exactly one row") };
                let doc = row["_docID"].as_str().filter(|id| !id.is_empty()).context("workspace binding physical ID missing")?;
                if !already_exists {
                    txn.add_resource_relationship("WorkspaceBinding", doc, "parent", "workspaces", workspace_doc).await?;
                }
                Ok(())
            }
        }
    })).await
}

pub async fn release_writer_binding(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_identity: &dyn crate::store::identity::NodeIdentity,
    request: &AgentRequest,
) -> Result<()> {
    let Some(workspace_id) = optional_id(request.workspace_id.as_deref()) else {
        return Ok(());
    };
    let owner = super::overlay::request_workspace_owner(request)?;
    let access = completion_access(node, agent_identity, &request.agent_did)?;
    access
        .transact_idempotent(
            crate::store::config_client::IdempotentTransactionRetry::Standard,
            "workspace.release_writer_binding",
            |txn| {
                Box::pin(release_writer_binding_in_txn(
                    txn,
                    &request.doc_id,
                    &request.request_id,
                    workspace_id,
                    owner,
                ))
            },
        )
        .await
}

/// Pending terminal paths can encounter malformed request rows. Cleanup
/// requires the complete binding tuple from the winning update, but absent
/// provenance must not prevent rejection by physical document ID.
pub(crate) async fn release_terminal_writer_binding(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request: &serde_json::Value,
) -> Result<()> {
    let string = |field: &str| {
        request[field]
            .as_str()
            .filter(|value| !value.trim().is_empty())
    };
    let (Some(doc_id), Some(request_id), Some(workspace_id), Some(owner)) = (
        string("_docID"),
        string("request_id"),
        string("workspace_id"),
        string("workspace_owner_node_did"),
    ) else {
        return Ok(());
    };
    release_writer_binding_in_txn(txn, doc_id, request_id, workspace_id, owner).await
}

pub(super) async fn release_writer_binding_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request_doc_id: &str,
    request_id: &str,
    workspace_id: &str,
    owner: &str,
) -> Result<()> {
    let request_doc_id = escape_graphql_string(request_doc_id);
    let request_id = escape_graphql_string(request_id);
    let workspace_id = escape_graphql_string(workspace_id);
    let owner = escape_graphql_string(owner);
    txn.execute(&format!(
        r#"mutation {{ update_WorkspaceBinding(
            filter: {{
                workspace_id: {{ _eq: "{workspace_id}" }},
                owner_node_did: {{ _eq: "{owner}" }},
                request_id: {{ _eq: "{request_id}" }},
                request_doc_id: {{ _eq: "{request_doc_id}" }},
                lifecycle_state: {{ _eq: "active" }}
            }},
            input: {{ lifecycle_state: "released" }}
        ) {{ _docID }} }}"#,
    ))
    .await?;
    Ok(())
}

pub(super) async fn load_docs_with_access(
    access: &crate::ConfigAccess,
    workspace_id: &str,
    node_did: &str,
) -> Result<MemoryWorkspaceDocuments> {
    let mut docs = MemoryWorkspaceDocuments::default();
    let query = isolated_workspace_doc_query(workspace_id, node_did);
    let response = access.execute(&query).await?;
    let mut workspaces: Vec<IsolatedWorkspaceDoc> =
        decode_access_rows(&response, "IsolatedWorkspace")?;
    anyhow::ensure!(
        workspaces.len() <= 1,
        "ambiguous node-scoped IsolatedWorkspace"
    );
    if let Some(workspace) = workspaces.pop() {
        docs.write_isolated_workspace(workspace)?;
    }
    let response = access
        .execute(&placement_doc_query(workspace_id, node_did))
        .await?;
    let mut placements: Vec<WorkspacePlacementDoc> =
        decode_access_rows(&response, "WorkspacePlacement")?;
    anyhow::ensure!(
        placements.len() <= 1,
        "ambiguous node-scoped WorkspacePlacement"
    );
    if let Some(placement) = placements.pop() {
        docs.write_placement(placement)?;
    }
    for binding in
        super::overlay::load_workspace_bindings_for_access(access, workspace_id, node_did).await?
    {
        docs.write_binding(binding)?;
    }
    let response = access
        .execute(&workspace_receipts_query(workspace_id))
        .await?;
    for receipt in decode_access_rows::<WorkspaceReceiptDoc>(&response, "WorkspaceReceipt")? {
        docs.write_receipt(receipt)?;
    }
    Ok(docs)
}

fn decode_access_rows<T>(response: &serde_json::Value, collection: &str) -> Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    let values = response
        .get("data")
        .and_then(|data| data.get(collection))
        .with_context(|| format!("{collection} query omitted rows"))?;
    Ok(serde_json::from_value(values.clone())?)
}

#[cfg(test)]
async fn load_receipts(
    node: &EmbeddedNode,
    workspace_id: &str,
) -> Result<Vec<WorkspaceReceiptDoc>> {
    let query = workspace_receipts_query(workspace_id);
    let response =
        graphql_with_transaction_retry(node, &query, "load WorkspaceReceipt for seal").await?;
    Ok(crate::store::graphql::rows(&response, "WorkspaceReceipt")?)
}

fn workspace_receipts_query(workspace_id: &str) -> String {
    format!(
        r#"{{
            WorkspaceReceipt(
                filter: {{ workspace_id: {{ _eq: "{id}" }} }}
            ) {{
                receipt_id
                workspace_id
                produced_by_request_id
                produced_by_request_doc_id
                kind
                path_capability_digest
                base_sha
                seal_hash
                head_sha
                changed_files
                diff_artifact
                unresolved_conflicts
                integration_instructions
            }}
        }}"#,
        id = escape_graphql_string(workspace_id),
    )
}

fn isolated_workspace_doc_query(workspace_id: &str, node_did: &str) -> String {
    format!(
        r#"{{
            IsolatedWorkspace(
                filter: {{ workspace_id: {{ _eq: "{id}" }}, owner_node_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{ {ISOLATED_WORKSPACE_FIELDS} }}
        }}"#,
        id = escape_graphql_string(workspace_id),
        owner = escape_graphql_string(node_did),
    )
}

fn placement_doc_query(workspace_id: &str, node_did: &str) -> String {
    format!(
        r#"{{
            WorkspacePlacement(
                filter: {{ workspace_id: {{ _eq: "{id}" }}, owner_node_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{ {PLACEMENT_FIELDS} }}
        }}"#,
        id = escape_graphql_string(workspace_id),
        owner = escape_graphql_string(node_did),
    )
}

pub(super) async fn load_repository_with_access(
    access: &crate::ConfigAccess,
    repository_id: &str,
    node_did: &str,
) -> Result<RepositoryPlacementRef> {
    let response = access
        .execute(&repository_query(repository_id, node_did))
        .await?;
    selected_repository(decode_access_rows(&response, "RepositoryPlacement")?)
}

fn repository_query(repository_id: &str, node_did: &str) -> String {
    format!(
        r#"{{ RepositoryPlacement(filter: {{repository_id: {{_eq:"{}"}},node_did: {{_eq:"{}"}}}},limit:2) {{repository_id node_did host_path enabled}} }}"#,
        escape_graphql_string(repository_id),
        escape_graphql_string(node_did)
    )
}

fn selected_repository(
    mut found: Vec<crate::documents::RepositoryPlacement>,
) -> Result<RepositoryPlacementRef> {
    anyhow::ensure!(
        found.len() == 1,
        "repository placement missing or ambiguous for node"
    );
    let row = found.pop().unwrap();
    anyhow::ensure!(row.enabled, "repository placement is disabled");
    Ok(RepositoryPlacementRef {
        repository_id: row.repository_id,
        owner_node_did: row.node_did,
        host_path: PathBuf::from(row.host_path),
        enabled: row.enabled,
    })
}

async fn flush_seal_outcome(
    access: &crate::ConfigAccess,
    docs: &MemoryWorkspaceDocuments,
    outcome: &SealWorkspaceOutcome,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let bindings: Vec<WorkspaceBindingDoc> = docs
        .bindings
        .values()
        .filter(|binding| binding.workspace_id == outcome.workspace.workspace_id)
        .cloned()
        .collect();
    let mutation = workspace_seal_docs_mutation(
        &outcome.workspace,
        &outcome.placement,
        &bindings,
        &outcome.receipt,
        &now,
    );
    write_workspace_receipt(access, &mutation, &outcome.workspace, &outcome.receipt)
        .await
        .with_context(|| {
            format!(
                "persist sealed workspace {} receipt {}",
                outcome.workspace.workspace_id, outcome.receipt.receipt_id
            )
        })?;
    Ok(())
}

async fn flush_integrate_outcome(
    access: &crate::ConfigAccess,
    outcome: &IntegrateWorkspaceOutcome,
) -> Result<()> {
    let mutation = workspace_integrate_docs_mutation(&[], &outcome.receipt);
    write_workspace_receipt(access, &mutation, &outcome.workspace, &outcome.receipt)
        .await
        .with_context(|| {
            format!(
                "persist integrator receipt {} for workspace {}",
                outcome.receipt.receipt_id, outcome.workspace.workspace_id
            )
        })?;
    Ok(())
}

async fn release_integrate_binding(
    access: &crate::ConfigAccess,
    docs: &MemoryWorkspaceDocuments,
    outcome: &IntegrateWorkspaceOutcome,
) -> Result<()> {
    let released: Vec<WorkspaceBindingDoc> = docs
        .bindings
        .values()
        .filter(|binding| {
            binding.workspace_id == outcome.workspace.workspace_id
                && binding.is_active_integrate()
                && binding.request_id == outcome.receipt.produced_by_request_id
        })
        .cloned()
        .map(release_binding)
        .collect();
    if released.is_empty() {
        return Ok(());
    }
    let mutation = workspace_integrate_docs_mutation(&released, &outcome.receipt);
    write_workspace_receipt(access, &mutation, &outcome.workspace, &outcome.receipt)
        .await
        .with_context(|| {
            format!(
                "release Integrate binding for receipt {}",
                outcome.receipt.receipt_id
            )
        })?;
    Ok(())
}

async fn write_workspace_receipt(
    access: &crate::ConfigAccess,
    mutation: &str,
    workspace: &IsolatedWorkspaceDoc,
    receipt: &WorkspaceReceiptDoc,
) -> Result<()> {
    anyhow::ensure!(
        receipt.workspace_id == workspace.workspace_id,
        "receipt workspace mismatch"
    );
    access.transact_write("workspace.publish_receipt", |txn| Box::pin(async move {
        let response = txn.execute(&super::overlay::isolated_workspace_record_query(&workspace.workspace_id, &workspace.owner_node_did)).await?;
        let rows = response["data"]["IsolatedWorkspace"].as_array().context("workspace parent query omitted rows")?;
        let [parent] = rows.as_slice() else { anyhow::bail!("workspace receipt parent unavailable or ambiguous") };
        let parent = parent["_docID"].as_str().filter(|id| !id.is_empty()).context("workspace parent lacks physical ID")?;
        let query = format!(r#"{{ WorkspaceReceipt(filter: {{receipt_id: {{_eq: "{}"}}}}, limit: 2) {{_docID workspace_id}} }}"#, escape_graphql_string(&receipt.receipt_id));
        let response = txn.execute(&query).await?;
        let rows = response["data"]["WorkspaceReceipt"].as_array().context("workspace receipt query omitted rows")?;
        anyhow::ensure!(rows.len() <= 1, "workspace receipt is ambiguous");
        let existing = rows.first().map(|row| {
            anyhow::ensure!(row["workspace_id"] == workspace.workspace_id, "workspace receipt belongs to another workspace");
            Ok::<_, anyhow::Error>(row["_docID"].as_str().filter(|id| !id.is_empty()).context("workspace receipt physical ID missing")?.to_owned())
        }).transpose()?;
        let response = txn.execute(mutation).await?;
        let rows = crate::store::graphql::mutation_documents(&response, "receipt")?.context("workspace receipt write omitted rows")?;
        let [row] = rows else { anyhow::bail!("workspace receipt write did not affect exactly one row") };
        let doc = row["_docID"].as_str().filter(|id| !id.is_empty()).context("workspace receipt physical ID missing")?;
        if let Some(existing) = existing {
            anyhow::ensure!(existing == doc, "workspace receipt changed physical identity");
        } else {
            txn.add_resource_relationship("WorkspaceReceipt", doc, "parent", "workspaces", parent).await?;
        }
        Ok::<_, anyhow::Error>(())
    })).await
}

async fn flush_cleanup_outcome(
    access: &crate::ConfigAccess,
    docs: &MemoryWorkspaceDocuments,
    outcome: &CleanupWorkspaceOutcome,
) -> Result<()> {
    let bindings: Vec<WorkspaceBindingDoc> = docs
        .bindings
        .values()
        .filter(|binding| binding.workspace_id == outcome.workspace.workspace_id)
        .cloned()
        .collect();
    let mutation = workspace_cleanup_docs_mutation(&outcome.workspace, &bindings);
    access
        .write("workspace.cleanup_documents", &mutation)
        .await
        .with_context(|| {
            format!(
                "persist cleaned workspace {}",
                outcome.workspace.workspace_id
            )
        })?;
    Ok(())
}
