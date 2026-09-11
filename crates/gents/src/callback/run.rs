use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use serde_json::Value;

use crate::workspace::journal::{advance, current_state};
use crate::workspace::{
    action_plan_canonical_json, emit_create_workspace_plan, execute_create_workspace_plan,
    parse_action_plan_json, ActionJournalEntry, ActionJournalState, ActionPlan,
    CreateWorkspaceAction, CreationPolicy, HostAction, HostExecuteError, HostExecutorContext,
    IsolatedWorkspaceDoc, MemoryWorkspaceDocuments, WorkspaceAdapterKind, WorkspaceDocuments,
    WorkspacePlacementDoc,
};

use super::claim::{claim_invocation, invocation_is_claimable};
use super::documents::{
    create_callback_result, flush_workspace_docs, load_callback, load_callback_module,
    load_callback_result, load_memory_workspace_docs, load_repository_placement,
    load_trusted_callback_signers, strip_secret_fields, update_invocation, CallbackInvocationDoc,
    CallbackModuleDoc, CallbackResultDoc,
};
use super::wasm::{plan_from_wasm_module, validate_callback_module};
use super::{
    LIFECYCLE_CLAIMED, LIFECYCLE_DENIED, LIFECYCLE_FAILED, LIFECYCLE_RUNNING, LIFECYCLE_SUCCEEDED,
};

/// Action N+1 must not enter Executing until N is ResultDocsWritten.
pub fn can_start_executing(journal: &[ActionJournalEntry], index: u32) -> bool {
    crate::workspace::action_journal_prefix_legal(journal)
        && (index == 0
            || matches!(
                current_state(journal, index - 1),
                Some(ActionJournalState::ResultDocsWritten)
            ))
}

pub fn result_docs_ready(
    journal: &[ActionJournalEntry],
    workspace: Option<&IsolatedWorkspaceDoc>,
    placement: Option<&WorkspacePlacementDoc>,
) -> bool {
    !journal.is_empty()
        && crate::workspace::action_journal_prefix_legal(journal)
        && journal
            .iter()
            .all(|entry| matches!(entry.state, ActionJournalState::ResultDocsWritten))
        && workspace.is_some()
        && placement.is_some()
}

/// CallbackResult is an edge from a succeeded invocation whose result docs are
/// complete. Succeeded-without-result is repaired by recovery.
pub fn can_emit_callback_result(
    state: &str,
    journal: &[ActionJournalEntry],
    workspace: Option<&IsolatedWorkspaceDoc>,
    placement: Option<&WorkspacePlacementDoc>,
) -> bool {
    state == LIFECYCLE_SUCCEEDED && result_docs_ready(journal, workspace, placement)
}

pub fn encode_journal(journal: &[ActionJournalEntry]) -> String {
    serde_json::to_string(journal).unwrap_or_else(|_| "[]".to_string())
}

pub fn decode_journal(raw: Option<&str>) -> Result<Vec<ActionJournalEntry>> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    Ok(serde_json::from_str(raw)?)
}

#[cfg(test)]
pub fn emit_plan_from_source(
    callback: &crate::document_config::Callback,
    source: &Value,
) -> Result<ActionPlan, String> {
    plan_from_callback(callback, source, None)
}

pub fn plan_from_callback(
    callback: &crate::document_config::Callback,
    source: &Value,
    module: Option<&CallbackModuleDoc>,
) -> Result<ActionPlan, String> {
    use crate::document_config::{BuiltInCallback, CallbackHandler};
    let source = strip_secret_fields(source.clone());
    match &callback.handler {
        CallbackHandler::BuiltIn {
            emitter: BuiltInCallback::CreateWorkspace,
        } => emit_create_workspace_from_source(&source),
        CallbackHandler::Module { module_id } => {
            let module = module.ok_or_else(|| {
                format!("CallbackModule {module_id} was not loaded for WASM planner")
            })?;
            if module.module_id != *module_id || module.agent_did != callback.agent_did {
                return Err("callback module owner/reference mismatch".into());
            }
            plan_from_wasm_module(
                module,
                &source,
                &callback.capabilities.iter().cloned().collect(),
            )
        }
    }
}

async fn load_planner_module(
    node: &EmbeddedNode,
    callback: &crate::document_config::Callback,
) -> Result<Option<CallbackModuleDoc>, String> {
    let crate::document_config::CallbackHandler::Module { module_id } = &callback.handler else {
        return Ok(None);
    };
    let module = load_callback_module(node, module_id, &callback.agent_did)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("CallbackModule {module_id} not found for callback owner"))?;
    let trusted = load_trusted_callback_signers(node)
        .await
        .map_err(|e| e.to_string())?;
    validate_callback_module(&module, &trusted)?;
    Ok(Some(module))
}

fn emit_create_workspace_from_source(source: &Value) -> Result<ActionPlan, String> {
    let work_unit_id = required_string(source, "work_unit_id")
        .or_else(|_| required_string(source, "assignment_id"))?;
    let repository_id = required_string(source, "repository_id")?;
    let base_sha = required_string(source, "base_sha")
        .or_else(|_| required_string(source, "base_revision"))?;
    let branch = optional_string(source, "branch")
        .or_else(|| optional_string(source, "suggested_branch"))
        .unwrap_or_else(|| git_branch_from_work_unit(&work_unit_id));
    let workspace_id =
        optional_string(source, "workspace_id").unwrap_or_else(|| work_unit_id.clone());
    let creation_policy = match optional_string(source, "creation_policy").as_deref() {
        None | Some("git_worktree_diff") => CreationPolicy::GitWorktreeDiff,
        Some(other) => {
            return Err(format!(
                "creation_policy `{other}` is not implemented in v1"
            ))
        }
    };
    let adapter = match optional_string(source, "adapter").as_deref() {
        None | Some("make_worktree") => WorkspaceAdapterKind::MakeWorktree,
        Some("git_worktree") => WorkspaceAdapterKind::GitWorktree,
        Some(other) => return Err(format!("unknown workspace adapter `{other}`")),
    };
    let clone_artifacts = source.get("clone_artifacts").and_then(|value| {
        value.as_array().map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
    });
    let path_capability = workspace_path_capability_from_source(source)?;
    Ok(emit_create_workspace_plan(CreateWorkspaceAction {
        workspace_id,
        path_capability,
        work_unit_id,
        repository_id,
        base_sha,
        branch,
        creation_policy,
        adapter,
        clone_artifacts,
    }))
}

fn workspace_path_capability_from_source(
    source: &Value,
) -> Result<crate::workspace::WorkspacePathCapability, String> {
    use crate::workspace::WorkspacePathCapability;
    let decode = |value: &Value| -> Result<Value, String> {
        match value {
            Value::String(raw) => serde_json::from_str(raw)
                .map_err(|error| format!("invalid workspace path manifest JSON: {error}")),
            other => Ok(other.clone()),
        }
    };
    match (source.get("path_capability"), source.get("owned_files")) {
        (Some(_), Some(_)) => {
            Err("workspace source must choose path_capability or owned_files, not both".into())
        }
        (Some(value), None) => {
            let capability: WorkspacePathCapability = serde_json::from_value(decode(value)?)
                .map_err(|error| format!("invalid workspace path capability: {error}"))?;
            match capability {
                WorkspacePathCapability::ExactPaths { paths } => {
                    WorkspacePathCapability::exact_paths(paths).map_err(|error| error.to_string())
                }
                WorkspacePathCapability::UnrestrictedCompatibility => {
                    Err("new callback workspace requires exact paths".into())
                }
            }
        }
        (None, Some(value)) => {
            let paths: Vec<String> = serde_json::from_value(decode(value)?).map_err(|error| {
                format!("owned_files must be a JSON array of relative file paths: {error}")
            })?;
            WorkspacePathCapability::exact_paths(paths).map_err(|error| error.to_string())
        }
        (None, None) => Err(
            "workspace source requires an explicit path_capability or owned_files manifest".into(),
        ),
    }
}

fn required_string(source: &Value, field: &str) -> Result<String, String> {
    optional_string(source, field).ok_or_else(|| format!("source document missing `{field}`"))
}

fn optional_string(source: &Value, field: &str) -> Option<String> {
    source
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn git_branch_from_work_unit(work_unit_id: &str) -> String {
    let mut branch = String::from("gents/");
    for ch in work_unit_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            branch.push(ch);
        } else {
            branch.push('-');
        }
    }
    branch
}

fn correlation_from_source(source: &Value) -> String {
    optional_string(source, "caused_by_correlation")
        .or_else(|| optional_string(source, "correlation"))
        .unwrap_or_default()
}

fn stored_action_plan(invocation: &CallbackInvocationDoc) -> Result<Option<ActionPlan>, String> {
    let Some(raw) = invocation
        .action_plan
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    parse_action_plan_json(raw).map(Some)
}

#[cfg(test)]
pub fn resolve_action_plan(
    invocation: &CallbackInvocationDoc,
    callback: &crate::document_config::Callback,
    source: &Value,
) -> Result<ActionPlan, String> {
    resolve_action_plan_with_module(invocation, callback, source, None)
}

#[cfg(test)]
pub fn resolve_action_plan_with_module(
    invocation: &CallbackInvocationDoc,
    callback: &crate::document_config::Callback,
    source: &Value,
    module: Option<&CallbackModuleDoc>,
) -> Result<ActionPlan, String> {
    if let Some(plan) = stored_action_plan(invocation)? {
        return Ok(plan);
    }
    let journal =
        decode_journal(invocation.action_journal.as_deref()).map_err(|error| error.to_string())?;
    if !journal.is_empty() {
        return Err("missing stored ActionPlan with a non-empty journal".to_string());
    }
    plan_from_callback(callback, source, module)
}

/// Host adapter may already have run; recovery must observe, not wipe.
pub(crate) fn journal_has_started_host_execution(journal: &[ActionJournalEntry]) -> bool {
    journal.iter().any(|entry| {
        matches!(
            entry.state,
            ActionJournalState::Validated
                | ActionJournalState::Executing
                | ActionJournalState::EffectObserved
                | ActionJournalState::ResultDocsWritten
        )
    })
}

/// Denied wipes the journal only before host execution. After Validated the
/// journal is kept and the invocation is Failed so recovery can observe.
pub(crate) fn apply_planner_deny(invocation: &mut CallbackInvocationDoc, reason: &str) {
    let journal = decode_journal(invocation.action_journal.as_deref()).unwrap_or_default();
    if journal_has_started_host_execution(&journal) {
        invocation.lifecycle_state = LIFECYCLE_FAILED.to_string();
        invocation.error = Some(reason.to_string());
        return;
    }
    invocation.action_journal = Some("[]".to_string());
    invocation.lifecycle_state = LIFECYCLE_DENIED.to_string();
    invocation.error = Some(reason.to_string());
}

pub async fn run_owned_invocation(
    node: &EmbeddedNode,
    invocation: &CallbackInvocationDoc,
    callback: &crate::document_config::Callback,
    ceiling: Option<&Path>,
) -> Result<()> {
    if !invocation_is_claimable(&invocation.owner_agent_did, invocation)
        && invocation.lifecycle_state != LIFECYCLE_CLAIMED
        && invocation.lifecycle_state != LIFECYCLE_RUNNING
    {
        return Ok(());
    }
    let Some(mut claimed) = claim_invocation(node, &invocation.owner_agent_did, invocation).await?
    else {
        return Ok(());
    };
    persist_claimed_to_running(node, &mut claimed).await?;

    if finish_succeeded_if_docs_ready(node, &claimed).await? {
        return Ok(());
    }

    let source = claimed.input.clone();
    execute_running_invocation(node, &mut claimed, callback, &source, ceiling).await
}

async fn persist_claimed_to_running(
    node: &EmbeddedNode,
    invocation: &mut CallbackInvocationDoc,
) -> Result<()> {
    if invocation.lifecycle_state == LIFECYCLE_RUNNING {
        return Ok(());
    }
    invocation.lifecycle_state = LIFECYCLE_RUNNING.to_string();
    if update_invocation(node, invocation, Some(LIFECYCLE_CLAIMED)).await? {
        return Ok(());
    }
    let current = super::documents::load_invocation(
        node,
        &invocation.invocation_id,
        &invocation.owner_agent_did,
    )
    .await?;
    match current {
        Some(row) if row.lifecycle_state == LIFECYCLE_RUNNING => {
            *invocation = row;
            Ok(())
        }
        Some(row) => anyhow::bail!(
            "CallbackInvocation {} could not persist claimed→running; state={}",
            invocation.invocation_id,
            row.lifecycle_state
        ),
        None => anyhow::bail!(
            "CallbackInvocation {} disappeared during claimed→running",
            invocation.invocation_id
        ),
    }
}

/// Testable planner + host-executor core. Persists journal/docs via the node.
async fn execute_running_invocation(
    node: &EmbeddedNode,
    invocation: &mut CallbackInvocationDoc,
    callback: &crate::document_config::Callback,
    source: &Value,
    ceiling: Option<&Path>,
) -> Result<()> {
    if callback.agent_did != invocation.owner_agent_did
        || callback.callback_id != invocation.callback_id
        || !callback.enabled
    {
        return deny(
            node,
            invocation,
            "callback is disabled or differs from invocation owner/reference",
        )
        .await;
    }

    let mut journal = decode_journal(invocation.action_journal.as_deref())?;
    if !crate::workspace::action_journal_prefix_legal(&journal) {
        return deny(node, invocation, "illegal action journal prefix").await;
    }

    // Recovery with a stored plan must not reload/re-validate the WASM module.
    // A missing module or empty trusted-signer set would otherwise deny() and
    // wipe an Executing journal the host may already have acted on.
    let stored = match stored_action_plan(invocation) {
        Ok(plan) => plan,
        Err(reason) => return deny(node, invocation, &reason).await,
    };
    let plan = if let Some(plan) = stored {
        plan
    } else if !journal.is_empty() {
        return deny(
            node,
            invocation,
            "missing stored ActionPlan with a non-empty journal",
        )
        .await;
    } else {
        let module = match load_planner_module(node, callback).await {
            Ok(module) => module,
            Err(reason) => return deny(node, invocation, &reason).await,
        };
        match emit_new_plan(callback, source, module).await {
            Ok(plan) => plan,
            Err(reason) => return deny(node, invocation, &reason).await,
        }
    };
    let canonical = match action_plan_canonical_json(&plan) {
        Ok(json) => json,
        Err(reason) => return deny(node, invocation, &reason).await,
    };
    invocation.action_plan = Some(canonical);
    if let Err(error) = plan.validate_against(&callback.capabilities.iter().cloned().collect()) {
        return deny(node, invocation, &error.to_string()).await;
    }

    let action = match plan
        .actions
        .first()
        .ok_or_else(|| anyhow!("ActionPlan has no actions"))?
        .clone()
    {
        HostAction::CreateWorkspace(action) => action,
        HostAction::FreezeWorkspaceBase(_) => {
            return deny(
                node,
                invocation,
                "freeze_workspace_base is an explicit operator action, not a callback plan",
            )
            .await;
        }
        HostAction::SealWorkspace(_) => {
            return deny(
                node,
                invocation,
                "seal_workspace is invoked on writer success, not as a callback plan",
            )
            .await;
        }
        HostAction::IntegrateWorkspace(_) => {
            return deny(
                node,
                invocation,
                "integrate_workspace is invoked on integrator success, not as a callback plan",
            )
            .await;
        }
        HostAction::CleanupWorkspace(_) => {
            return deny(
                node,
                invocation,
                "cleanup_workspace is an explicit host action, not a callback plan",
            )
            .await;
        }
    };
    if !can_start_executing(&journal, 0) {
        return deny(node, invocation, "journal prefix blocks first action").await;
    }

    let Some(repository) =
        load_repository_placement(node, &action.repository_id, &invocation.owner_agent_did).await?
    else {
        return deny(
            node,
            invocation,
            &format!(
                "RepositoryPlacement {} not found on this host",
                action.repository_id
            ),
        )
        .await;
    };

    persist_journal(node, invocation, &journal, LIFECYCLE_RUNNING, None).await?;

    if current_state(&journal, 0).is_none() {
        advance(&mut journal, 0, ActionJournalState::Validated);
        persist_journal(node, invocation, &journal, LIFECYCLE_RUNNING, None).await?;
    }
    if matches!(
        current_state(&journal, 0),
        Some(ActionJournalState::Validated)
    ) {
        // Durable Executing before the adapter so recovery observes rather than re-runs.
        advance(&mut journal, 0, ActionJournalState::Executing);
        persist_journal(node, invocation, &journal, LIFECYCLE_RUNNING, None).await?;
    }

    let mut docs =
        load_memory_workspace_docs(node, &action.workspace_id, &invocation.owner_agent_did).await?;
    let capabilities: BTreeSet<String> = callback.capabilities.iter().cloned().collect();
    let correlation = correlation_from_source(source);
    let execute_result = {
        let mut ctx = HostExecutorContext {
            owner_agent_did: invocation.owner_agent_did.clone(),
            repository,
            ceiling,
            capabilities,
            writer_principal: callback.agent_did.clone(),
            integrator_principal: callback.agent_did.clone(),
            caused_by_invocation_id: invocation.invocation_id.clone(),
            caused_by_correlation: correlation.clone(),
            documents: &mut docs,
        };
        execute_create_workspace_plan(&plan, &mut journal, &mut ctx)
    };
    let outcome = match execute_result {
        Ok(outcome) => outcome,
        Err(HostExecuteError::Denied { reason }) => {
            return deny(node, invocation, &reason).await;
        }
        Err(HostExecuteError::Failed {
            reason, outcome, ..
        }) => {
            if let Some(outcome) = outcome {
                let written = memory_from_outcome(&outcome);
                flush_workspace_docs(node, &written).await?;
            } else {
                flush_workspace_docs(node, &docs).await?;
            }
            persist_journal(node, invocation, &journal, LIFECYCLE_FAILED, Some(&reason)).await?;
            return Ok(());
        }
    };

    let written = memory_from_outcome(&outcome);
    flush_workspace_docs(node, &written).await?;
    persist_journal(node, invocation, &journal, LIFECYCLE_RUNNING, None).await?;
    succeed_then_emit_result(
        node,
        invocation,
        &journal,
        &outcome.workspace,
        &outcome.placement,
        Some(correlation),
    )
    .await
}

fn memory_from_outcome(
    outcome: &crate::workspace::CreateWorkspaceOutcome,
) -> MemoryWorkspaceDocuments {
    let mut docs = MemoryWorkspaceDocuments::default();
    let _ = docs.write_isolated_workspace(outcome.workspace.clone());
    let _ = docs.write_placement(outcome.placement.clone());
    docs
}

async fn persist_journal(
    node: &EmbeddedNode,
    invocation: &mut CallbackInvocationDoc,
    journal: &[ActionJournalEntry],
    state: &str,
    error: Option<&str>,
) -> Result<()> {
    invocation.action_journal = Some(encode_journal(journal));
    invocation.lifecycle_state = state.to_string();
    invocation.error = error.map(str::to_string);
    if !update_invocation(node, invocation, None).await? {
        anyhow::bail!(
            "CallbackInvocation {} persist matched no row (state={state})",
            invocation.invocation_id
        );
    }
    Ok(())
}

async fn emit_new_plan(
    callback: &crate::document_config::Callback,
    source: &Value,
    module: Option<CallbackModuleDoc>,
) -> Result<ActionPlan, String> {
    if module.is_some() {
        let callback = callback.clone();
        let source = source.clone();
        tokio::task::spawn_blocking(move || plan_from_callback(&callback, &source, module.as_ref()))
            .await
            .map_err(|error| format!("WASM planner task failed: {error}"))?
    } else {
        plan_from_callback(callback, source, None)
    }
}

async fn deny(
    node: &EmbeddedNode,
    invocation: &mut CallbackInvocationDoc,
    reason: &str,
) -> Result<()> {
    apply_planner_deny(invocation, reason);
    if !update_invocation(node, invocation, None).await? {
        anyhow::bail!(
            "CallbackInvocation {} deny persist matched no row",
            invocation.invocation_id
        );
    }
    Ok(())
}

async fn succeed_then_emit_result(
    node: &EmbeddedNode,
    invocation: &mut CallbackInvocationDoc,
    journal: &[ActionJournalEntry],
    workspace: &IsolatedWorkspaceDoc,
    placement: &WorkspacePlacementDoc,
    correlation: Option<String>,
) -> Result<()> {
    if invocation.lifecycle_state != LIFECYCLE_SUCCEEDED {
        persist_journal(node, invocation, journal, LIFECYCLE_SUCCEEDED, None).await?;
    }
    if !can_emit_callback_result(
        &invocation.lifecycle_state,
        journal,
        Some(workspace),
        Some(placement),
    ) {
        anyhow::bail!(
            "CallbackInvocation {} result docs are not ready",
            invocation.invocation_id
        );
    }
    let correlation = correlation
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| workspace.caused_by_correlation.clone());
    create_callback_result(
        node,
        &CallbackResultDoc {
            result_id: format!("res-{}", invocation.invocation_id),
            invocation_id: invocation.invocation_id.clone(),
            owner_agent_did: invocation.owner_agent_did.clone(),
            workspace_id: Some(workspace.workspace_id.clone()),
            work_unit_id: Some(workspace.work_unit_id.clone()),
            caused_by_correlation: Some(correlation),
            created_at: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        },
    )
    .await
    .map(|_| ())
}

pub async fn recover_local_invocations(
    node: &EmbeddedNode,
    agent_did: &str,
    ceiling: Option<&Path>,
) -> Result<()> {
    let invocations = super::documents::list_recoverable_invocations(node, agent_did).await?;
    for invocation in invocations {
        if invocation.owner_agent_did != agent_did {
            continue;
        }
        if invocation.lifecycle_state == LIFECYCLE_SUCCEEDED {
            if let Err(error) = finish_succeeded_if_docs_ready(node, &invocation).await {
                tracing::warn!(
                    invocation_id = %invocation.invocation_id,
                    %error,
                    "callback succeeded-without-result repair failed"
                );
            }
            continue;
        }
        if !invocation_is_claimable(agent_did, &invocation) {
            continue;
        }
        let Some(callback) =
            load_callback(node, &invocation.callback_id, &invocation.owner_agent_did).await?
        else {
            tracing::warn!(
                invocation_id = %invocation.invocation_id,
                callback_id = %invocation.callback_id,
                "recovery skipped: Callback missing"
            );
            continue;
        };
        if let Err(error) = run_owned_invocation(node, &invocation, &callback, ceiling).await {
            tracing::warn!(
                invocation_id = %invocation.invocation_id,
                %error,
                "callback recovery run failed"
            );
        }
    }
    Ok(())
}

pub async fn finish_succeeded_if_docs_ready(
    node: &EmbeddedNode,
    invocation: &CallbackInvocationDoc,
) -> Result<bool> {
    if !matches!(
        invocation.lifecycle_state.as_str(),
        LIFECYCLE_RUNNING | LIFECYCLE_SUCCEEDED
    ) {
        return Ok(false);
    }
    let journal = decode_journal(invocation.action_journal.as_deref())?;
    let workspace_id = stored_action_plan(invocation)
        .map_err(anyhow::Error::msg)?
        .and_then(|plan| {
            plan.actions
                .into_iter()
                .next()
                .map(|action| action.workspace_id().to_string())
        });
    let Some(workspace_id) = workspace_id else {
        return Ok(false);
    };
    let docs = load_memory_workspace_docs(node, &workspace_id, &invocation.owner_agent_did).await?;
    let workspace = docs.load_isolated_workspace(&workspace_id)?;
    let placement = docs.load_placement(&workspace_id)?;
    if !result_docs_ready(&journal, workspace.as_ref(), placement.as_ref()) {
        return Ok(false);
    }
    let Some(workspace) = workspace else {
        return Ok(false);
    };
    let Some(placement) = placement else {
        return Ok(false);
    };
    if load_callback_result(node, &invocation.invocation_id, &invocation.owner_agent_did)
        .await?
        .is_some()
        && invocation.lifecycle_state == LIFECYCLE_SUCCEEDED
    {
        return Ok(true);
    }
    let mut current = invocation.clone();
    succeed_then_emit_result(
        node,
        &mut current,
        &journal,
        &workspace,
        &placement,
        Some(workspace.caused_by_correlation.clone()),
    )
    .await?;
    Ok(true)
}

#[cfg(test)]
mod path_capability_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builtin_requires_one_explicit_exact_manifest() {
        for source in [
            json!({}),
            json!({"owned_files": null}),
            json!({"owned_files": "not-json"}),
            json!({"owned_files": ["src/a.rs", 4]}),
            json!({"owned_files": ["../escape"]}),
            json!({"path_capability": {"mode": "unrestrictedCompatibility"}}),
            json!({"owned_files": [], "path_capability": {"mode": "exactPaths", "paths": []}}),
        ] {
            assert!(
                workspace_path_capability_from_source(&source).is_err(),
                "{source}"
            );
        }
        let empty = workspace_path_capability_from_source(&json!({"owned_files": "[]"})).unwrap();
        assert_eq!(
            empty,
            crate::workspace::WorkspacePathCapability::exact_paths(Vec::new()).unwrap()
        );
        let paths = workspace_path_capability_from_source(
            &json!({"owned_files": "[\"src/b.rs\",\"src/a.rs\"]"}),
        )
        .unwrap();
        assert_eq!(
            paths,
            crate::workspace::WorkspacePathCapability::exact_paths(vec![
                "src/a.rs".into(),
                "src/b.rs".into()
            ])
            .unwrap()
        );
    }

    #[test]
    fn builtin_requires_capability_in_frozen_projected_input() {
        let callback: crate::document_config::Callback = serde_json::from_value(json!({
            "callback_id":"path-contract", "agent_did":"did:key:writer", "handler":{"kind":"built_in","emitter":"create_workspace"}
        })).unwrap();
        let mut source = json!({"work_unit_id":"unit","repository_id":"repo","base_sha":"base","branch":"branch"});
        assert!(plan_from_callback(&callback, &source, None).is_err());
        source["owned_files"] = json!(["src/a.rs"]);
        let plan = plan_from_callback(&callback, &source, None).unwrap();
        let HostAction::CreateWorkspace(action) = &plan.actions[0] else {
            panic!("expected workspace");
        };
        assert_eq!(
            action.path_capability,
            crate::workspace::WorkspacePathCapability::exact_paths(vec!["src/a.rs".into()])
                .unwrap()
        );
        assert!(plan_from_callback(&callback, &json!({}), None).is_err());
    }
}
