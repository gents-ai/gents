use super::{
    isolated_workspace_upsert_mutation, workspace_placement_upsert_mutation,
    MemoryWorkspaceDocuments,
};
use crate::store::graphql::escape_graphql_string;
use anyhow::{Context, Result};

pub(crate) async fn flush_workspace_docs(
    access: &crate::ConfigAccess,
    docs: &MemoryWorkspaceDocuments,
    agent_did: &str,
    action: &crate::tools::workspace::CreateWorkspaceAction,
) -> Result<()> {
    anyhow::ensure!(
        !agent_did.is_empty() && access.actor_did().as_deref() == Some(agent_did),
        "callback workspace actor mismatch"
    );
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    access.transact_write("callback.publish_workspace", |txn| {
        let now = now.clone();
        Box::pin(async move {
        let mut published = None;
        for workspace in docs.workspaces.values() {
            anyhow::ensure!(workspace.workspace_id == action.workspace_id
                && Some(workspace.writer_principal.as_str()) == action.writer_agent_did.as_deref()
                && Some(workspace.integrator_principal.as_str()) == action.integrator_agent_did.as_deref(),
                "workspace publication differs from the frozen Agent selection");
            let existing = workspace_document_id_in_txn(txn, "IsolatedWorkspace", &workspace.workspace_id, &workspace.owner_node_did).await?;
            if let Some(existing_doc) = &existing {
                let query = format!(r#"{{IsolatedWorkspace(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{lifecycle_state seal_hash}}}}"#, escape_graphql_string(existing_doc));
                let response = txn.execute(&query).await?;
                let rows = response["data"]["IsolatedWorkspace"].as_array().context("workspace state query omitted rows")?;
                let [row] = rows.as_slice() else { anyhow::bail!("workspace state unavailable or ambiguous") };
                let previous = row["lifecycle_state"].as_str().and_then(crate::tools::native::normalize_workspace_lifecycle_state).context("workspace lifecycle state invalid")?;
                let desired = crate::tools::native::normalize_workspace_lifecycle_state(&workspace.lifecycle_state).context("workspace publication state invalid")?;
                anyhow::ensure!(previous == desired || (previous == "provisioning" && matches!(desired, "ready" | "provisionFailed")), "stale callback workspace publication cannot replace an advanced lifecycle");
                if previous == desired {
                    anyhow::ensure!(row["seal_hash"].as_str() == workspace.seal_hash.as_deref(), "workspace publication seal differs from the retained state");
                }
            }
            let parent = if existing.is_none() {
                for (did, doc) in [(&action.writer_agent_did, &action.writer_agent_doc_id), (&action.integrator_agent_did, &action.integrator_agent_doc_id)] {
                    let did = did.as_deref().context("workspace Agent DID missing")?;
                    let doc = doc.as_deref().context("workspace Agent document missing")?;
                    let query = format!(r#"{{ Agent(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{ _docID node_did agent_did }} }}"#, escape_graphql_string(doc));
                    let response = txn.execute(&query).await?;
                    let rows = response["data"]["Agent"].as_array().context("workspace Agent query omitted rows")?;
                    anyhow::ensure!(matches!(rows.as_slice(), [row] if row["_docID"] == doc && row["node_did"] == workspace.owner_node_did && row["agent_did"] == did), "frozen workspace Agent is unavailable or changed");
                }
                for reader in &action.readers {
                    let doc = reader.agent_doc_id.as_deref().context("workspace reader physical Agent missing")?;
                    let query = format!(r#"{{ Agent(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{ _docID node_did agent_did enabled }} }}"#, escape_graphql_string(doc));
                    let response = txn.execute(&query).await?;
                    let rows = response["data"]["Agent"].as_array().context("workspace reader Agent query omitted rows")?;
                    anyhow::ensure!(matches!(rows.as_slice(), [row] if row["_docID"] == doc
                        && row["node_did"] == workspace.owner_node_did
                        && row["agent_did"] == reader.agent_did && row["enabled"] != false),
                        "frozen workspace reader Agent is unavailable or changed");
                }
                Some(workspace_agent_parent_in_txn(txn, &workspace.owner_node_did, agent_did).await?)
            } else { None };
            let response = txn.execute(&isolated_workspace_upsert_mutation(workspace)).await?;
            let id = workspace_mutation_document_id(&response, "upsert_IsolatedWorkspace")?;
            if let Some(parent) = parent {
                txn.add_resource_relationship("IsolatedWorkspace", &id, "parent", "agents", &parent).await?;
                txn.add_actor_relationship("IsolatedWorkspace", &id, "writer", &workspace.writer_principal.parse()?).await?;
                txn.add_actor_relationship("IsolatedWorkspace", &id, "integrator", &workspace.integrator_principal.parse()?).await?;
                for reader in &action.readers {
                    txn.add_actor_relationship("IsolatedWorkspace", &id, "reader", &reader.agent_did.parse()?).await?;
                }
            } else {
                anyhow::ensure!(existing.as_deref() == Some(id.as_str()), "workspace upsert changed physical identity");
            }
            published = Some(id);
        }
        for placement in docs.placements.values() {
            anyhow::ensure!(placement.workspace_id == action.workspace_id, "workspace placement differs from frozen plan");
            let parent = match &published { Some(id) => id.clone(), None => workspace_document_id_in_txn(txn, "IsolatedWorkspace", &placement.workspace_id, &placement.owner_node_did).await?.context("workspace parent unavailable")? };
            let existing = workspace_document_id_in_txn(txn, "WorkspacePlacement", &placement.workspace_id, &placement.owner_node_did).await?;
            let response = txn.execute(&workspace_placement_upsert_mutation(placement, &now)).await?;
            let id = workspace_mutation_document_id(&response, "upsert_WorkspacePlacement")?;
            if existing.is_none() {
                txn.add_resource_relationship("WorkspacePlacement", &id, "parent", "workspaces", &parent).await?;
            } else {
                anyhow::ensure!(existing.as_deref() == Some(id.as_str()), "placement upsert changed physical identity");
            }
        }
        Ok::<_, anyhow::Error>(())
    })}).await
}

async fn workspace_document_id_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    collection: &str,
    workspace: &str,
    owner: &str,
) -> Result<Option<String>> {
    anyhow::ensure!(
        matches!(collection, "IsolatedWorkspace" | "WorkspacePlacement"),
        "invalid workspace publication collection"
    );
    let query = format!(
        r#"{{ {collection}(filter: {{workspace_id: {{_eq: "{}"}}, owner_node_did: {{_eq: "{}"}}}}, limit: 2) {{_docID}} }}"#,
        escape_graphql_string(workspace),
        escape_graphql_string(owner)
    );
    let response = txn.execute(&query).await?;
    let rows = response["data"][collection]
        .as_array()
        .context("workspace publication query omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "workspace publication is ambiguous");
    rows.first()
        .map(|row| {
            row["_docID"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .context("workspace document ID missing")
        })
        .transpose()
}

fn workspace_mutation_document_id(response: &serde_json::Value, field: &str) -> Result<String> {
    let rows = crate::store::graphql::mutation_documents(response, field)?
        .context("workspace publication mutation omitted rows")?;
    let [row] = rows else {
        anyhow::bail!("workspace publication mutation did not affect exactly one row")
    };
    Ok(row["_docID"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("workspace document ID missing")?
        .to_owned())
}

async fn workspace_agent_parent_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    agent_did: &str,
) -> Result<String> {
    anyhow::ensure!(
        txn.actor_did() == Some(agent_did),
        "workspace publication requires its selected Agent actor"
    );
    let query = format!(
        r#"{{Agent(filter: {{node_did: {{_eq: "{}"}}, agent_did: {{_eq: "{}"}}}}, limit: 2) {{_docID}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(agent_did)
    );
    let response = txn.execute(&query).await?;
    let rows = response["data"]["Agent"]
        .as_array()
        .context("workspace Agent parent query omitted rows")?;
    let [row] = rows.as_slice() else {
        anyhow::bail!("workspace Agent parent unavailable or ambiguous")
    };
    Ok(row["_docID"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("workspace Agent parent lacks physical ID")?
        .to_owned())
}

pub(super) async fn flush_frozen_workspace(
    access: &crate::ConfigAccess,
    documents: &MemoryWorkspaceDocuments,
    workspace_id: &str,
    owner: &str,
) -> Result<()> {
    use super::WorkspaceDocuments as _;
    let workspace = documents
        .load_isolated_workspace(workspace_id)?
        .context("frozen workspace missing")?;
    let placement = documents
        .load_placement(workspace_id)?
        .context("frozen placement missing")?;
    anyhow::ensure!(
        workspace.owner_node_did == owner
            && placement.owner_node_did == owner
            && workspace.lifecycle_state == "sealed",
        "frozen workspace does not match its owner or state"
    );
    let seal = workspace
        .seal_hash
        .as_deref()
        .filter(|hash| !hash.is_empty())
        .context("frozen workspace seal missing")?;
    let now = chrono::Utc::now().to_rfc3339();
    access.transact_write("workspace.quickstart.freeze", |txn| {
        let placement = &placement;
        let now = &now;
        Box::pin(async move {
            let doc = workspace_document_id_in_txn(txn, "IsolatedWorkspace", workspace_id, owner).await?.context("frozen workspace unavailable")?;
            let placement_doc = workspace_document_id_in_txn(txn, "WorkspacePlacement", workspace_id, owner).await?;
            let query = format!(r#"{{IsolatedWorkspace(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{lifecycle_state seal_hash}}}}"#, escape_graphql_string(&doc));
            let response = txn.execute(&query).await?;
            let rows = response["data"]["IsolatedWorkspace"].as_array().context("frozen state query omitted rows")?;
            let [row] = rows.as_slice() else { anyhow::bail!("frozen state unavailable or ambiguous") };
            let state = row["lifecycle_state"].as_str().and_then(crate::tools::native::normalize_workspace_lifecycle_state).context("workspace state invalid")?;
            if state == "sealed" {
                anyhow::ensure!(row["seal_hash"].as_str() == Some(seal), "existing workspace seal differs");
                return Ok(());
            }
            anyhow::ensure!(state == "provisioning" && row["seal_hash"].as_str() == Some(seal),
                "workspace cannot freeze from state {state} or a different reserved seal");
            let mutation = format!(r#"mutation {{update_IsolatedWorkspace(docID: "{}", input: {{lifecycle_state: "sealed", seal_hash: "{}", binding_revision: "{}"}}) {{_docID}}}}"#,
                escape_graphql_string(&doc), escape_graphql_string(seal), uuid::Uuid::new_v4());
            let response = txn.execute(&mutation).await?;
            anyhow::ensure!(crate::store::graphql::mutation_documents(&response, "update_IsolatedWorkspace")?.is_some_and(|rows| rows.len() == 1), "workspace freeze was not authorized");
            let response = txn.execute(&workspace_placement_upsert_mutation(placement, now)).await?;
            let updated = workspace_mutation_document_id(&response, "upsert_WorkspacePlacement")?;
            if let Some(placement_doc) = placement_doc {
                anyhow::ensure!(updated == placement_doc, "workspace placement physical identity changed");
            } else {
                txn.add_resource_relationship("WorkspacePlacement", &updated, "parent", "workspaces", &doc).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
    }).await
}
