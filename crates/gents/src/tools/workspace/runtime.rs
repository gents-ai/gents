use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::request::lifecycle::WorkspaceLineage;
use crate::request::watcher::AgentRequest;
use crate::tools::native::WorkspaceAuthority;

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

pub async fn stamp_workspace_lineage(
    node: &std::sync::Arc<EmbeddedNode>,
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
    let workspace = super::overlay::load_isolated_workspace_record(node, workspace_id, owner)
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

    let mut docs = load_docs(
        node,
        workspace_id,
        super::overlay::request_workspace_owner(request)?,
    )
    .await?;
    let workspace = docs
        .load_isolated_workspace(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("isolated workspace {workspace_id} not found"))?;
    anyhow::ensure!(
        workspace.writer_principal.trim() == request.node_did.trim(),
        "request node no longer holds the workspace writer_principal grant"
    );
    docs.load_placement(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("workspace placement {workspace_id} not found"))?;
    let repository =
        load_repository(node, &workspace.repository_id, &workspace.owner_node_did).await?;

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
    flush_seal_outcome(node, &docs, &outcome).await
}

pub async fn integrate_on_integrator_success(
    node: &std::sync::Arc<EmbeddedNode>,
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

    let mut docs = load_docs(
        node,
        workspace_id,
        super::overlay::request_workspace_owner(request)?,
    )
    .await?;
    let workspace = docs
        .load_isolated_workspace(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("isolated workspace {workspace_id} not found"))?;
    anyhow::ensure!(
        workspace.integrator_principal.trim() == request.node_did.trim(),
        "request node no longer holds the workspace integrator_principal grant"
    );
    docs.load_placement(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("workspace placement {workspace_id} not found"))?;
    let repository =
        load_repository(node, &workspace.repository_id, &workspace.owner_node_did).await?;

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
    flush_integrate_outcome(node, &outcome).await?;
    finalize_integrate_trunk(&trunk, outcome.pending_head_sha.as_deref())?;
    release_integrate_binding(node, &docs, &outcome).await?;
    clear_integrate_journal(&trunk, workspace_id);
    Ok(())
}

fn ctx_repository_path(repository: &RepositoryPlacementRef) -> PathBuf {
    repository.host_path.clone()
}

/// Explicit operator/ack cleanup. Never invoked from request terminal.
pub async fn cleanup_workspace(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    workspace_id: &str,
    operator_tool_root: Option<&Path>,
) -> Result<()> {
    let workspace_id = optional_id(Some(workspace_id))
        .ok_or_else(|| anyhow::anyhow!("cleanup_workspace requires a workspace_id"))?;
    let mut docs = load_docs(node, workspace_id, node_did).await?;
    let workspace = docs
        .load_isolated_workspace(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("isolated workspace {workspace_id} not found"))?;
    docs.load_placement(workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("workspace placement {workspace_id} not found"))?;
    let repository =
        load_repository(node, &workspace.repository_id, &workspace.owner_node_did).await?;

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
    flush_cleanup_outcome(node, &docs, &outcome).await
}

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
    principal_did: &str,
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
    lineage.require_authority_if_workspace_id()?;
    let workspace_owner = lineage
        .workspace_owner_node_did
        .as_deref()
        .context("workspace owner is missing")?;
    let authority = match lineage.workspace_authority.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => WorkspaceAuthority::parse(value)?,
        _ => anyhow::bail!("workspace-bound request {workspace_id} is missing workspace_authority"),
    };
    let response = graphql_with_transaction_retry(node, &format!(
        r#"{{ AgentRequest(filter: {{_docID: {{_eq:"{}"}}}},limit:1) {{request_id node_did workspace_id workspace_owner_node_did workspace_authority workspace_seal_hash}} }}"#,
        escape_graphql_string(request_doc_id)), "bind exact workspace request").await?;
    let actual = crate::graphql::first_row::<gents_protocol::row::AgentRequestRow>(
        &response,
        "AgentRequest",
    )?
    .context("workspace binding request document missing")?;
    anyhow::ensure!(
        actual.request_id == request_id
            && actual.node_did.as_deref() == Some(principal_did)
            && actual.workspace_id.as_deref() == Some(workspace_id)
            && actual.workspace_owner_node_did == lineage.workspace_owner_node_did
            && actual.workspace_authority.as_deref() == Some(authority.as_str())
            && actual.workspace_seal_hash == lineage.workspace_seal_hash,
        "workspace binding must match exact physical request node and lineage"
    );
    let workspace =
        super::overlay::load_isolated_workspace_record(node, workspace_id, workspace_owner)
            .await?
            .ok_or_else(|| anyhow::anyhow!("isolated workspace {workspace_id} not found"))?;
    super::overlay::require_workspace_principal(&workspace, principal_did, authority)?;
    let existing =
        super::overlay::load_workspace_bindings_for(node, workspace_id, workspace_owner).await?;
    let candidate = new_binding(
        workspace_id,
        request_id,
        request_doc_id,
        authority,
        workspace_owner,
        optional_id(lineage.workspace_seal_hash.as_deref())
            .or(optional_id(workspace.seal_hash.as_deref())),
    );
    let release_previous = matches!(
        authority,
        WorkspaceAuthority::ReadWrite | WorkspaceAuthority::Integrate
    ) && super::overlay::previous_exclusive_is_stale(
        node,
        workspace_id,
        &existing,
        request_id,
        matches!(authority, WorkspaceAuthority::Integrate),
    )
    .await?;
    match admit_workspace_binding(
        workspace_id,
        &workspace.lifecycle_state,
        optional_id(workspace.seal_hash.as_deref()),
        &existing,
        candidate,
        release_previous,
    )? {
        AdmitBinding::Reuse(_) => Ok(()),
        AdmitBinding::Create { binding, release } => {
            let mut docs = release;
            docs.push(binding);
            super::overlay::persist_workspace_binding_docs(node, &docs).await
        }
    }
}

pub async fn release_writer_binding(
    node: &std::sync::Arc<EmbeddedNode>,
    request: &AgentRequest,
) -> Result<()> {
    let Some(workspace_id) = optional_id(request.workspace_id.as_deref()) else {
        return Ok(());
    };
    let owner = super::overlay::request_workspace_owner(request)?;
    crate::config_client::ConfigAccess::transact_local_idempotent(
        node,
        None,
        crate::config_client::IdempotentTransactionRetry::Standard,
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
    txn: &crate::config_client::ConfigApplyTxn<'_>,
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

async fn release_writer_binding_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
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

async fn load_docs(
    node: &EmbeddedNode,
    workspace_id: &str,
    node_did: &str,
) -> Result<MemoryWorkspaceDocuments> {
    let mut docs = MemoryWorkspaceDocuments::default();
    if let Some(workspace) = load_isolated_workspace_doc(node, workspace_id, node_did).await? {
        docs.write_isolated_workspace(workspace)?;
    }
    if let Some(placement) = load_placement_doc(node, workspace_id, node_did).await? {
        docs.write_placement(placement)?;
    }
    for binding in super::overlay::load_workspace_bindings_for(node, workspace_id, node_did).await?
    {
        docs.write_binding(binding)?;
    }
    for receipt in load_receipts(node, workspace_id).await? {
        docs.write_receipt(receipt)?;
    }
    Ok(docs)
}

async fn load_receipts(
    node: &EmbeddedNode,
    workspace_id: &str,
) -> Result<Vec<WorkspaceReceiptDoc>> {
    let query = format!(
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
    );
    let response =
        graphql_with_transaction_retry(node, &query, "load WorkspaceReceipt for seal").await?;
    Ok(crate::graphql::rows(&response, "WorkspaceReceipt")?)
}

async fn load_isolated_workspace_doc(
    node: &EmbeddedNode,
    workspace_id: &str,
    node_did: &str,
) -> Result<Option<IsolatedWorkspaceDoc>> {
    let query = format!(
        r#"{{
            IsolatedWorkspace(
                filter: {{ workspace_id: {{ _eq: "{id}" }}, owner_node_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{ {ISOLATED_WORKSPACE_FIELDS} }}
        }}"#,
        id = escape_graphql_string(workspace_id),
        owner = escape_graphql_string(node_did),
    );
    let response =
        graphql_with_transaction_retry(node, &query, "load IsolatedWorkspace for seal").await?;
    {
        let mut found =
            crate::graphql::rows::<IsolatedWorkspaceDoc>(&response, "IsolatedWorkspace")?;
        anyhow::ensure!(found.len() <= 1, "ambiguous node-scoped IsolatedWorkspace");
        Ok(found.pop())
    }
}

async fn load_placement_doc(
    node: &EmbeddedNode,
    workspace_id: &str,
    node_did: &str,
) -> Result<Option<WorkspacePlacementDoc>> {
    let query = format!(
        r#"{{
            WorkspacePlacement(
                filter: {{ workspace_id: {{ _eq: "{id}" }}, owner_node_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{ {PLACEMENT_FIELDS} }}
        }}"#,
        id = escape_graphql_string(workspace_id),
        owner = escape_graphql_string(node_did),
    );
    let response =
        graphql_with_transaction_retry(node, &query, "load WorkspacePlacement for seal").await?;
    {
        let mut found =
            crate::graphql::rows::<WorkspacePlacementDoc>(&response, "WorkspacePlacement")?;
        anyhow::ensure!(found.len() <= 1, "ambiguous node-scoped WorkspacePlacement");
        Ok(found.pop())
    }
}

async fn load_repository(
    node: &EmbeddedNode,
    repository_id: &str,
    node_did: &str,
) -> Result<RepositoryPlacementRef> {
    let response = graphql_with_transaction_retry(node, &format!(
        r#"{{ RepositoryPlacement(filter: {{repository_id: {{_eq:"{}"}},node_did: {{_eq:"{}"}}}},limit:2) {{repository_id node_did host_path enabled}} }}"#,
        escape_graphql_string(repository_id), escape_graphql_string(node_did)), "load scoped RepositoryPlacement").await?;
    let mut found = crate::graphql::rows::<crate::documents::RepositoryPlacement>(
        &response,
        "RepositoryPlacement",
    )?;
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
    node: &std::sync::Arc<EmbeddedNode>,
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
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "workspace.seal_documents",
        &mutation,
    )
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
    node: &std::sync::Arc<EmbeddedNode>,
    outcome: &IntegrateWorkspaceOutcome,
) -> Result<()> {
    let mutation = workspace_integrate_docs_mutation(&[], &outcome.receipt);
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "workspace.integrate_documents",
        &mutation,
    )
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
    node: &std::sync::Arc<EmbeddedNode>,
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
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "workspace.release_integrate_binding",
        &mutation,
    )
    .await
    .with_context(|| {
        format!(
            "release Integrate binding for receipt {}",
            outcome.receipt.receipt_id
        )
    })?;
    Ok(())
}

async fn flush_cleanup_outcome(
    node: &std::sync::Arc<EmbeddedNode>,
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
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "workspace.cleanup_documents",
        &mutation,
    )
    .await
    .with_context(|| {
        format!(
            "persist cleaned workspace {}",
            outcome.workspace.workspace_id
        )
    })?;
    Ok(())
}
