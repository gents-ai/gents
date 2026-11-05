use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use defra_node::Node as EmbeddedNode;
use serde_json::Value;

use crate::tools::workspace::journal::{advance, current_state};
use crate::tools::workspace::{
    action_plan_canonical_json, emit_create_workspace_plan, execute_create_workspace_plan,
    parse_action_plan_json, ActionJournalEntry, ActionJournalState, ActionPlan,
    CreateWorkspaceAction, CreationPolicy, HostAction, HostExecuteError, HostExecutorContext,
    IsolatedWorkspaceDoc, MemoryWorkspaceDocuments, WorkspaceAdapterKind, WorkspaceDocuments,
    WorkspacePlacementDoc,
};

use super::claim::{claim_invocation, invocation_is_claimable};
use super::documents::{
    create_callback_result, flush_workspace_docs, load_callback, load_callback_module_as,
    load_callback_result, load_isolated_workspace, load_memory_workspace_docs, load_repository_placement,
    load_trusted_callback_signers, strip_secret_fields, update_invocation, CallbackInvocationDoc,
    CallbackModuleDoc, CallbackResultDoc,
};
use super::planner::{plan_from_module, validate_callback_module};
use super::{
    LIFECYCLE_CLAIMED, LIFECYCLE_DENIED, LIFECYCLE_FAILED, LIFECYCLE_PENDING, LIFECYCLE_RUNNING,
    LIFECYCLE_SUCCEEDED,
};
use crate::plugin::executor::PluginExecutor;

/// Action N+1 must not enter Executing until N is ResultDocsWritten.
pub fn can_start_executing(journal: &[ActionJournalEntry], index: u32) -> bool {
    crate::tools::workspace::action_journal_prefix_legal(journal)
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
        && crate::tools::workspace::action_journal_prefix_legal(journal)
        && journal
            .iter()
            .all(|entry| matches!(entry.state, ActionJournalState::ResultDocsWritten))
        && workspace.is_some()
        && placement.is_some()
}

/// CallbackResult is an edge from a succeeded invocation whose result docs are
/// complete; recovery repairs succeeded-without-result.
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
    callback: &crate::documents::Callback,
    source: &Value,
) -> Result<ActionPlan, String> {
    plan_from_callback(callback, source, None)
}

pub fn plan_from_callback(
    callback: &crate::documents::Callback,
    source: &Value,
    module: Option<&CallbackModuleDoc>,
) -> Result<ActionPlan, String> {
    use crate::documents::{BuiltInCallback, CallbackHandler};
    let source = strip_secret_fields(source.clone());
    let (mut plan, selection) = match &callback.handler {
        CallbackHandler::BuiltIn {
            emitter: BuiltInCallback::CreateWorkspace,
            workspace_agents,
        } => (
            emit_create_workspace_from_source(&source)?,
            workspace_agents.as_ref(),
        ),
        CallbackHandler::Module {
            module_id,
            workspace_agents,
        } => {
            let module = module.ok_or_else(|| {
                format!("CallbackModule {module_id} was not loaded for callback planner")
            })?;
            if module.module_id != *module_id || module.node_did != callback.node_did {
                return Err("callback module owner/reference mismatch".into());
            }
            (
                plan_from_module(
                    module,
                    &source,
                    &callback.capabilities.iter().cloned().collect(),
                )?,
                workspace_agents.as_ref(),
            )
        }
        CallbackHandler::Plugin { plugin, .. } => {
            return Err(format!(
                "callback plugin {plugin} runs the plugin itself and has no action plan"
            ))
        }
    };
    for action in &mut plan.actions {
        if let HostAction::CreateWorkspace(action) = action {
            let selected = selection
                .ok_or("workspace callback requires configured writer and integrator Agents")?;
            for (role, did) in [
                ("writer", &selected.writer_agent_did),
                ("integrator", &selected.integrator_agent_did),
            ] {
                if did.is_empty() || did.trim() != did || did == &callback.node_did {
                    return Err(format!("workspace {role} must name an explicit Agent DID"));
                }
                identity::Did::new(did).map_err(|error| error.to_string())?;
            }
            if action.writer_agent_did.is_some()
                || action.writer_agent_doc_id.is_some()
                || action.integrator_agent_did.is_some()
                || action.integrator_agent_doc_id.is_some()
            {
                return Err("workspace planner cannot select its own Agent authorities".into());
            }
            action.writer_agent_did = Some(selected.writer_agent_did.clone());
            action.integrator_agent_did = Some(selected.integrator_agent_did.clone());
        }
    }
    Ok(plan)
}

pub(super) async fn bind_workspace_agent_documents(
    access: &crate::ConfigAccess,
    node_did: &str,
    plan: &mut ActionPlan,
) -> Result<()> {
    for action in &mut plan.actions {
        let HostAction::CreateWorkspace(action) = action else {
            continue;
        };
        for (role, did, target) in [
            (
                "writer",
                action.writer_agent_did.as_deref(),
                &mut action.writer_agent_doc_id,
            ),
            (
                "integrator",
                action.integrator_agent_did.as_deref(),
                &mut action.integrator_agent_doc_id,
            ),
        ] {
            let did = did.context("workspace selection omitted an Agent DID")?;
            let query = format!(
                r#"{{ Agent(filter: {{node_did: {{_eq: "{}"}}, agent_did: {{_eq: "{}"}}}}, limit: 2) {{ _docID node_did agent_did enabled }} }}"#,
                crate::store::graphql::escape_graphql_string(node_did),
                crate::store::graphql::escape_graphql_string(did)
            );
            let response = access.execute(&query).await?;
            let rows = response
                .pointer("/data/Agent")
                .and_then(Value::as_array)
                .context("workspace Agent lookup omitted rows")?;
            anyhow::ensure!(
                matches!(rows.as_slice(), [row] if row["node_did"] == node_did
                && row["agent_did"] == did && row["enabled"] != false),
                "configured workspace {role} Agent is unavailable or ambiguous"
            );
            *target = Some(
                rows[0]["_docID"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .context("workspace Agent lacks a physical document ID")?
                    .to_owned(),
            );
        }
    }
    Ok(())
}

async fn load_planner_module(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    callback: &crate::documents::Callback,
) -> Result<Option<(String, CallbackModuleDoc)>, String> {
    let crate::documents::CallbackHandler::Module { module_id, .. } = &callback.handler else {
        return Ok(None);
    };
    let (doc_id, module) = load_callback_module_as(access, module_id, &callback.node_did)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("CallbackModule {module_id} not found for callback owner"))?;
    let trusted = load_trusted_callback_signers(node)
        .await
        .map_err(|e| e.to_string())?;
    validate_callback_module(&module, &trusted)?;
    Ok(Some((doc_id, module)))
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
            ));
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
        writer_agent_did: None,
        writer_agent_doc_id: None,
        integrator_agent_did: None,
        integrator_agent_doc_id: None,
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
) -> Result<crate::tools::workspace::WorkspacePathCapability, String> {
    use crate::tools::workspace::WorkspacePathCapability;
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
    callback: &crate::documents::Callback,
    source: &Value,
) -> Result<ActionPlan, String> {
    resolve_action_plan_with_module(invocation, callback, source, None)
}

#[cfg(test)]
pub fn resolve_action_plan_with_module(
    invocation: &CallbackInvocationDoc,
    callback: &crate::documents::Callback,
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

/// Denied wipes the journal only before host execution; after it started, the
/// denial fails the invocation so an action the host may already have run is
/// marked interrupted and never retried.
pub(crate) fn apply_planner_deny(invocation: &mut CallbackInvocationDoc, reason: &str) {
    let journal = decode_journal(invocation.action_journal.as_deref()).unwrap_or_default();
    if let Some(failed) = crate::tools::workspace::recover_running(&journal) {
        invocation.action_journal = Some(encode_journal(&failed));
        invocation.lifecycle_state = LIFECYCLE_FAILED.to_string();
        invocation.error = Some(reason.to_string());
        return;
    }
    invocation.action_journal = Some("[]".to_string());
    invocation.lifecycle_state = LIFECYCLE_DENIED.to_string();
    invocation.error = Some(reason.to_string());
}

pub async fn run_owned_invocation(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    invocation: &CallbackInvocationDoc,
    callback: &crate::documents::Callback,
    ceiling: Option<&Path>,
    plugins: &PluginExecutor,
    data_node: Option<&std::sync::Arc<EmbeddedNode>>,
) -> Result<()> {
    if !invocation_is_claimable(&invocation.owner_node_did, invocation)
        && invocation.lifecycle_state != LIFECYCLE_CLAIMED
        && invocation.lifecycle_state != LIFECYCLE_RUNNING
    {
        return Ok(());
    }
    let module = if invocation.lifecycle_state == LIFECYCLE_PENDING
        && matches!(
            &callback.handler,
            crate::documents::CallbackHandler::Module { .. }
        ) {
        Some(
            load_planner_module(node, access, callback)
                .await
                .map_err(anyhow::Error::msg)?
                .ok_or_else(|| anyhow!("callback module unavailable before claim"))?,
        )
    } else {
        None
    };
    let prepared_plan = if let Some((_, module)) = &module {
        let plan = emit_new_plan(callback, &invocation.input, Some(module.clone()))
            .await
            .map_err(anyhow::Error::msg)?;
        Some(action_plan_canonical_json(&plan).map_err(anyhow::Error::msg)?)
    } else {
        None
    };
    let Some(mut claimed) = claim_invocation(
        node,
        access,
        &invocation.owner_node_did,
        invocation,
        callback,
        module.as_ref().map(|(doc, module)| (doc.as_str(), module)),
        prepared_plan.as_deref(),
    )
    .await?
    else {
        return Ok(());
    };
    let admitted_callback: crate::documents::Callback = serde_json::from_str(
        claimed
            .claimed_callback
            .as_deref()
            .ok_or_else(|| anyhow!("admitted callback configuration missing"))?,
    )?;
    let callback = &admitted_callback;
    persist_claimed_to_running(node, access, &mut claimed).await?;

    if finish_succeeded_if_docs_ready(node, access, &claimed).await? {
        return Ok(());
    }

    let source = claimed.input.clone();
    execute_running_invocation(
        node,
        access,
        &mut claimed,
        callback,
        &source,
        ceiling,
        plugins,
        data_node,
    )
    .await
}

async fn persist_claimed_to_running(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    invocation: &mut CallbackInvocationDoc,
) -> Result<()> {
    if invocation.lifecycle_state == LIFECYCLE_RUNNING {
        return Ok(());
    }
    invocation.lifecycle_state = LIFECYCLE_RUNNING.to_string();
    if update_invocation(access, invocation, Some(LIFECYCLE_CLAIMED)).await? {
        return Ok(());
    }
    let current = super::documents::load_invocation(
        node,
        &invocation.invocation_id,
        &invocation.owner_node_did,
        &invocation.agent_did,
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

async fn execute_running_invocation(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    invocation: &mut CallbackInvocationDoc,
    callback: &crate::documents::Callback,
    source: &Value,
    ceiling: Option<&Path>,
    plugins: &PluginExecutor,
    data_node: Option<&std::sync::Arc<EmbeddedNode>>,
) -> Result<()> {
    if callback.node_did != invocation.owner_node_did
        || callback.callback_id != invocation.callback_id
        || !callback.enabled
    {
        return deny(
            node,
            access,
            invocation,
            "callback is disabled or differs from invocation owner/reference",
        )
        .await;
    }

    let mut journal = decode_journal(invocation.action_journal.as_deref())?;
    if !crate::tools::workspace::action_journal_prefix_legal(&journal) {
        return deny(node, access, invocation, "illegal action journal prefix").await;
    }
    if let crate::documents::CallbackHandler::Plugin {
        plugin,
        digest,
        correlation_field,
        outputs,
        ..
    } = &callback.handler
    {
        let handler = super::plugin::PluginHandler {
            plugin,
            digest,
            correlation_field: correlation_field.as_deref(),
            outputs,
        };
        return super::plugin::execute(
            node, access, invocation, handler, source, plugins, journal, data_node,
        )
        .await;
    }

    // Recovery with a stored plan must not reload/re-validate the WASM module;
    // a missing module or empty trusted-signer set would otherwise deny() and
    // wipe an Executing journal the host may already have acted on.
    let stored = match stored_action_plan(invocation) {
        Ok(plan) => plan,
        Err(reason) => return deny(node, access, invocation, &reason).await,
    };
    let is_new_plan = stored.is_none();
    let mut plan = if let Some(plan) = stored {
        plan
    } else if !journal.is_empty() {
        return deny(
            node,
            access,
            invocation,
            "missing stored ActionPlan with a non-empty journal",
        )
        .await;
    } else {
        if matches!(
            &callback.handler,
            crate::documents::CallbackHandler::Module { .. }
        ) {
            return deny(
                node,
                access,
                invocation,
                "admitted callback module plan is unavailable",
            )
            .await;
        }
        match emit_new_plan(callback, source, None).await {
            Ok(plan) => plan,
            Err(reason) => return deny(node, access, invocation, &reason).await,
        }
    };
    if is_new_plan {
        bind_workspace_agent_documents(access, &invocation.owner_node_did, &mut plan).await?;
    }
    let canonical = match action_plan_canonical_json(&plan) {
        Ok(json) => json,
        Err(reason) => return deny(node, access, invocation, &reason).await,
    };
    invocation.action_plan = Some(canonical);
    if let Err(error) = plan.validate_against(&callback.capabilities.iter().cloned().collect()) {
        return deny(node, access, invocation, &error.to_string()).await;
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
                access,
                invocation,
                "freeze_workspace_base is an explicit operator action, not a callback plan",
            )
            .await;
        }
        HostAction::SealWorkspace(_) => {
            return deny(
                node,
                access,
                invocation,
                "seal_workspace is invoked on writer success, not as a callback plan",
            )
            .await;
        }
        HostAction::IntegrateWorkspace(_) => {
            return deny(
                node,
                access,
                invocation,
                "integrate_workspace is invoked on integrator success, not as a callback plan",
            )
            .await;
        }
        HostAction::CleanupWorkspace(_) => {
            return deny(
                node,
                access,
                invocation,
                "cleanup_workspace is an explicit host action, not a callback plan",
            )
            .await;
        }
    };
    let Some(writer_agent) = action
        .writer_agent_did
        .as_deref()
        .filter(|did| !did.is_empty() && *did != invocation.owner_node_did.as_str())
    else {
        return deny(
            node,
            access,
            invocation,
            "workspace plan has no selected writer Agent",
        )
        .await;
    };
    let Some(integrator_agent) = action
        .integrator_agent_did
        .as_deref()
        .filter(|did| !did.is_empty() && *did != invocation.owner_node_did.as_str())
    else {
        return deny(
            node,
            access,
            invocation,
            "workspace plan has no selected integrator Agent",
        )
        .await;
    };
    if action
        .writer_agent_doc_id
        .as_deref()
        .is_none_or(str::is_empty)
        || action
            .integrator_agent_doc_id
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return deny(
            node,
            access,
            invocation,
            "workspace plan lacks physical Agent targets",
        )
        .await;
    }
    if !can_start_executing(&journal, 0) {
        return deny(
            node,
            access,
            invocation,
            "journal prefix blocks first action",
        )
        .await;
    }

    let Some(repository) = load_repository_placement(
        access,
        &action.repository_id,
        &invocation.owner_node_did,
        &invocation.agent_did,
    )
    .await?
    else {
        return deny(
            node,
            access,
            invocation,
            &format!(
                "RepositoryPlacement {} not found on this host",
                action.repository_id
            ),
        )
        .await;
    };

    persist_journal(node, access, invocation, &journal, LIFECYCLE_RUNNING, None).await?;

    if current_state(&journal, 0).is_none() {
        advance(&mut journal, 0, ActionJournalState::Validated);
        persist_journal(node, access, invocation, &journal, LIFECYCLE_RUNNING, None).await?;
    }
    let existing_workspace = load_isolated_workspace(
        access,
        &action.workspace_id,
        &invocation.owner_node_did,
        &invocation.agent_did,
    )
    .await?;
    if let Some(existing) = &existing_workspace {
        anyhow::ensure!(
            existing.writer_principal == writer_agent
                && existing.integrator_principal == integrator_agent
                && existing.caused_by_invocation_id == invocation.invocation_id,
            "existing workspace differs from the frozen callback provenance"
        );
    }
    if existing_workspace.is_none() {
        let action_for_reservation = action.clone();
        let repository_for_reservation = repository.clone();
        let ceiling_for_reservation = ceiling.map(Path::to_path_buf);
        let owner = invocation.owner_node_did.clone();
        let writer = writer_agent.to_owned();
        let integrator = integrator_agent.to_owned();
        let invocation_id = invocation.invocation_id.clone();
        let correlation = correlation_from_source(source);
        let reservation = tokio::task::spawn_blocking(move || {
            crate::tools::workspace::prepare_workspace_reservation(
                &action_for_reservation,
                &repository_for_reservation,
                ceiling_for_reservation.as_deref(),
                &owner,
                &writer,
                &integrator,
                &invocation_id,
                &correlation,
            )
        })
        .await
        .context("joining workspace reservation preparation")??;
        let mut reserved = MemoryWorkspaceDocuments::default();
        reserved.write_isolated_workspace(reservation)?;
        flush_workspace_docs(access, &reserved, &invocation.agent_did, &action).await?;
    }
    if matches!(
        current_state(&journal, 0),
        Some(ActionJournalState::Validated)
    ) {
        // Durable Executing before the adapter so recovery observes rather
        // than re-runs.
        advance(&mut journal, 0, ActionJournalState::Executing);
        persist_journal(node, access, invocation, &journal, LIFECYCLE_RUNNING, None).await?;
    }

    let mut docs = load_memory_workspace_docs(
        access,
        &action.workspace_id,
        &invocation.owner_node_did,
        &invocation.agent_did,
    )
    .await?;
    let capabilities: BTreeSet<String> = callback.capabilities.iter().cloned().collect();
    let correlation = correlation_from_source(source);
    let execute_result = {
        let mut ctx = HostExecutorContext {
            owner_node_did: invocation.owner_node_did.clone(),
            repository,
            ceiling,
            capabilities,
            writer_principal: writer_agent.to_owned(),
            integrator_principal: integrator_agent.to_owned(),
            caused_by_invocation_id: invocation.invocation_id.clone(),
            caused_by_correlation: correlation.clone(),
            documents: &mut docs,
        };
        execute_create_workspace_plan(&plan, &mut journal, &mut ctx)
    };
    let outcome = match execute_result {
        Ok(outcome) => outcome,
        Err(HostExecuteError::Denied { reason }) => {
            return deny(node, access, invocation, &reason).await;
        }
        Err(HostExecuteError::Failed {
            reason, outcome, ..
        }) => {
            if let Some(outcome) = outcome {
                let written = memory_from_outcome(&outcome);
                flush_workspace_docs(access, &written, &invocation.agent_did, &action).await?;
            } else {
                flush_workspace_docs(access, &docs, &invocation.agent_did, &action).await?;
            }
            persist_journal(
                node,
                access,
                invocation,
                &journal,
                LIFECYCLE_FAILED,
                Some(&reason),
            )
            .await?;
            return Ok(());
        }
    };

    let written = memory_from_outcome(&outcome);
    flush_workspace_docs(access, &written, &invocation.agent_did, &action).await?;
    persist_journal(node, access, invocation, &journal, LIFECYCLE_RUNNING, None).await?;
    succeed_then_emit_result(
        node,
        access,
        invocation,
        &journal,
        &outcome.workspace,
        &outcome.placement,
        Some(correlation),
    )
    .await
}

fn memory_from_outcome(
    outcome: &crate::tools::workspace::CreateWorkspaceOutcome,
) -> MemoryWorkspaceDocuments {
    let mut docs = MemoryWorkspaceDocuments::default();
    let _ = docs.write_isolated_workspace(outcome.workspace.clone());
    let _ = docs.write_placement(outcome.placement.clone());
    docs
}

pub(super) async fn persist_journal(
    _node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    invocation: &mut CallbackInvocationDoc,
    journal: &[ActionJournalEntry],
    state: &str,
    error: Option<&str>,
) -> Result<()> {
    invocation.action_journal = Some(encode_journal(journal));
    invocation.lifecycle_state = state.to_string();
    invocation.error = error.map(str::to_string);
    if !update_invocation(access, invocation, None).await? {
        anyhow::bail!(
            "CallbackInvocation {} persist matched no row (state={state})",
            invocation.invocation_id
        );
    }
    Ok(())
}

async fn emit_new_plan(
    callback: &crate::documents::Callback,
    source: &Value,
    module: Option<CallbackModuleDoc>,
) -> Result<ActionPlan, String> {
    if module.is_some() {
        let callback = callback.clone();
        let source = source.clone();
        tokio::task::spawn_blocking(move || plan_from_callback(&callback, &source, module.as_ref()))
            .await
            .map_err(|error| format!("callback planner task failed: {error}"))?
    } else {
        plan_from_callback(callback, source, None)
    }
}

pub(super) async fn deny(
    _node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    invocation: &mut CallbackInvocationDoc,
    reason: &str,
) -> Result<()> {
    tracing::warn!(
        invocation_id = %invocation.invocation_id,
        callback_id = %invocation.callback_id,
        %reason,
        "callback invocation denied"
    );
    apply_planner_deny(invocation, reason);
    if !update_invocation(access, invocation, None).await? {
        anyhow::bail!(
            "CallbackInvocation {} deny persist matched no row",
            invocation.invocation_id
        );
    }
    Ok(())
}

async fn succeed_then_emit_result(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    invocation: &mut CallbackInvocationDoc,
    journal: &[ActionJournalEntry],
    workspace: &IsolatedWorkspaceDoc,
    placement: &WorkspacePlacementDoc,
    correlation: Option<String>,
) -> Result<()> {
    if invocation.lifecycle_state != LIFECYCLE_SUCCEEDED {
        persist_journal(node, access, invocation, journal, LIFECYCLE_SUCCEEDED, None).await?;
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
        access,
        &CallbackResultDoc {
            result_id: format!("res-{}", invocation.invocation_id),
            invocation_id: invocation.invocation_id.clone(),
            binding_id: Some(invocation.origin.binding_id().to_string()),
            owner_node_did: invocation.owner_node_did.clone(),
            agent_did: invocation.agent_did.clone(),
            workspace_id: Some(workspace.workspace_id.clone()),
            work_unit_id: Some(workspace.work_unit_id.clone()),
            caused_by_correlation: Some(correlation),
            created_at: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        },
    )
    .await
    .map(|_| ())
}

pub(crate) async fn recover_local_invocations(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    ceiling: Option<&Path>,
    plugins: &PluginExecutor,
    engine: &super::CallbackEngine,
) -> Result<()> {
    let mut invocations =
        super::documents::list_recoverable_invocations(node, node_did, engine).await?;
    if let Some(workers) = engine.workers.as_ref() {
        workers.prioritize(&mut invocations);
    }
    for invocation in invocations {
        let access = match engine.access_for_agent(&invocation.agent_did) {
            Ok(access) => access,
            Err(error) => {
                tracing::warn!(agent_did = %invocation.agent_did, %error, "callback recovery signer unavailable");
                continue;
            }
        };
        if let Err(error) = super::documents::ensure_callback_parent(
            node,
            &access,
            "CallbackInvocation",
            &invocation.invocation_id,
            &invocation.owner_node_did,
            &invocation.agent_did,
        )
        .await
        {
            tracing::warn!(invocation_id = %invocation.invocation_id, %error, "callback parent repair failed");
            continue;
        }
        if engine
            .workers
            .as_ref()
            .is_some_and(|workers| workers.contains(&invocation.invocation_id))
        {
            continue;
        }
        if invocation.owner_node_did != node_did {
            continue;
        }
        if invocation.lifecycle_state == LIFECYCLE_SUCCEEDED {
            if let Err(error) = finish_succeeded_if_docs_ready(node, &access, &invocation).await {
                tracing::warn!(
                    invocation_id = %invocation.invocation_id,
                    %error,
                    "callback succeeded-without-result repair failed"
                );
            }
            continue;
        }
        if !invocation_is_claimable(node_did, &invocation) {
            continue;
        }
        let callback = match callback_for_recovery(node, &invocation).await {
            Ok(callback) => callback,
            Err(error) => {
                tracing::warn!(invocation_id = %invocation.invocation_id, %error, "callback recovery configuration unavailable");
                continue;
            }
        };
        let Some(callback) = callback else {
            tracing::warn!(
                invocation_id = %invocation.invocation_id,
                callback_id = %invocation.callback_id,
                "recovery skipped: Callback missing"
            );
            continue;
        };
        if let Err(error) =
            execute_recovered(node, &invocation, &callback, ceiling, plugins, engine).await
        {
            tracing::warn!(
                invocation_id = %invocation.invocation_id,
                %error,
                "callback recovery run failed"
            );
        }
    }
    retry_failed_plugin_invocations(node, node_did, ceiling, plugins, engine).await
}

pub(super) async fn callback_for_recovery(
    node: &std::sync::Arc<EmbeddedNode>,
    invocation: &CallbackInvocationDoc,
) -> Result<Option<crate::documents::Callback>> {
    if matches!(
        invocation.lifecycle_state.as_str(),
        LIFECYCLE_CLAIMED | LIFECYCLE_RUNNING
    ) {
        let raw = invocation
            .claimed_callback
            .as_deref()
            .ok_or_else(|| anyhow!("admitted callback configuration missing"))?;
        Ok(Some(serde_json::from_str(raw)?))
    } else {
        load_callback(node, &invocation.callback_id, &invocation.owner_node_did).await
    }
}

async fn retry_failed_plugin_invocations(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    ceiling: Option<&Path>,
    plugins: &PluginExecutor,
    engine: &super::CallbackEngine,
) -> Result<()> {
    let now = chrono::Utc::now();
    for failed in super::documents::list_recent_failed(node, node_did, engine).await? {
        let Some(callback) =
            load_callback(node, &failed.callback_id, &failed.owner_node_did).await?
        else {
            continue;
        };
        let crate::documents::CallbackHandler::Plugin { max_attempts, .. } = &callback.handler
        else {
            continue;
        };
        let attempts = u32::try_from(failed.attempts.unwrap_or(0)).unwrap_or(0);
        let journal = decode_journal(failed.action_journal.as_deref())?;
        if !crate::tools::workspace::retry_allowed(
            &failed.lifecycle_state,
            &journal,
            attempts,
            crate::plugin::attempts_allowed(*max_attempts),
        ) {
            continue;
        }
        let last_claim = failed
            .claimed_at
            .as_deref()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok());
        let due = last_claim.is_none_or(|at| {
            now.signed_duration_since(at).to_std().unwrap_or_default()
                >= crate::plugin::retry_backoff(attempts)
        });
        if !due {
            continue;
        }
        let mut pending = failed.clone();
        pending.lifecycle_state = super::LIFECYCLE_PENDING.to_owned();
        pending.action_journal = Some("[]".to_owned());
        pending.error = None;
        let access = engine.access_for_agent(&pending.agent_did)?;
        if !update_invocation(&access, &pending, Some(LIFECYCLE_FAILED)).await? {
            continue;
        }
        tracing::info!(
            invocation_id = %pending.invocation_id,
            attempt = attempts + 1,
            "retrying a failed plugin invocation"
        );
        if let Err(error) =
            execute_recovered(node, &pending, &callback, ceiling, plugins, engine).await
        {
            tracing::warn!(
                invocation_id = %pending.invocation_id,
                %error,
                "callback plugin retry failed"
            );
        }
    }
    Ok(())
}

async fn execute_recovered(
    node: &std::sync::Arc<EmbeddedNode>,
    invocation: &CallbackInvocationDoc,
    callback: &crate::documents::Callback,
    ceiling: Option<&Path>,
    plugins: &PluginExecutor,
    engine: &super::CallbackEngine,
) -> Result<()> {
    let _ = (node, ceiling, plugins);
    engine.execute_invocation(invocation, callback).await
}

pub async fn finish_succeeded_if_docs_ready(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
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
    let docs = load_memory_workspace_docs(
        access,
        &workspace_id,
        &invocation.owner_node_did,
        &invocation.agent_did,
    )
    .await?;
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
    if load_callback_result(
        node,
        &invocation.invocation_id,
        &invocation.owner_node_did,
        &invocation.agent_did,
    )
    .await?
    .is_some()
        && invocation.lifecycle_state == LIFECYCLE_SUCCEEDED
    {
        return Ok(true);
    }
    let mut current = invocation.clone();
    succeed_then_emit_result(
        node,
        access,
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
            crate::tools::workspace::WorkspacePathCapability::exact_paths(Vec::new()).unwrap()
        );
        let paths = workspace_path_capability_from_source(
            &json!({"owned_files": "[\"src/b.rs\",\"src/a.rs\"]"}),
        )
        .unwrap();
        assert_eq!(
            paths,
            crate::tools::workspace::WorkspacePathCapability::exact_paths(vec![
                "src/a.rs".into(),
                "src/b.rs".into()
            ])
            .unwrap()
        );
    }

    #[test]
    fn builtin_requires_capability_in_frozen_projected_input() {
        let callback: crate::documents::Callback = serde_json::from_value(json!({
            "callback_id":"path-contract", "node_did":"did:key:writer", "handler":{"kind":"built_in","emitter":"create_workspace",
                "workspace_agents":{"writer_agent_did":crate::triggers::callback::test_agent_did(), "integrator_agent_did":crate::triggers::callback::test_agent_did()}}
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
            crate::tools::workspace::WorkspacePathCapability::exact_paths(vec!["src/a.rs".into()])
                .unwrap()
        );
        assert!(plan_from_callback(&callback, &json!({}), None).is_err());
    }
}
