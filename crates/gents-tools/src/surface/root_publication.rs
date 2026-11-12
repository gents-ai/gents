use anyhow::{Context, Result};
use serde_json::{json, Value};

use gents_documents::read_desired_state_record_in_txn;
use gents_protocol::operations::workspace_root::{
    enabled_after, normalize_root_path, reader_decision, writes_display_name, EnabledUpdate,
    NameUpdate, ReaderChange, ReaderDecision, ReaderSubject, StoredRoot, WorkspaceRootError,
    WorkspaceRootPublish, WorkspaceRootReaderInput, WorkspaceRootReaderReceipt,
    WorkspaceRootReceipt, WorkspaceRootRemoved, WorkspaceRootView,
};
use gents_protocol::operations::StringUpdate;
use gents_store::collection::Collection;
use gents_store::config_client::{
    ConfigAccess, ConfigApplyTxn, ConfigClientError, TransactionFailure,
};
use gents_store::graphql::{
    created_doc_id, escape_graphql_string, mutation_documents, required_rows,
};

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceRootWriteError {
    #[error("workspace root publication failed")]
    Publication(#[source] anyhow::Error),
}

enum RootApplyError {
    Operation(WorkspaceRootError),
    Other(anyhow::Error),
}

impl From<WorkspaceRootError> for RootApplyError {
    fn from(error: WorkspaceRootError) -> Self {
        Self::Operation(error)
    }
}

impl From<anyhow::Error> for RootApplyError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl From<ConfigClientError> for RootApplyError {
    fn from(error: ConfigClientError) -> Self {
        Self::Other(error.into())
    }
}

impl From<RootApplyError> for WorkspaceRootError {
    fn from(error: RootApplyError) -> Self {
        match error {
            RootApplyError::Operation(error) => error,
            RootApplyError::Other(error) => {
                Self::Passthrough(error.reallocate_into_boxed_dyn_error_without_backtrace())
            }
        }
    }
}

impl From<RootApplyError> for anyhow::Error {
    fn from(error: RootApplyError) -> Self {
        match error {
            RootApplyError::Operation(error) => error.into(),
            RootApplyError::Other(error) => error,
        }
    }
}

impl TransactionFailure for RootApplyError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Operation(_) => None,
            Self::Other(error) => error.config_client_error(),
        }
    }
}

type Applied<T> = std::result::Result<T, RootApplyError>;

async fn selected_node(txn: &ConfigApplyTxn<'_>, node_did: &str) -> Applied<String> {
    if txn.actor_did().is_none() {
        return Err(
            anyhow::anyhow!("workspace root publication requires an authenticated actor").into(),
        );
    }
    identity::Did::new(node_did).map_err(anyhow::Error::from)?;
    let (doc, _) = read_desired_state_record_in_txn(txn, Collection::Node, node_did, node_did)
        .await?
        .ok_or_else(|| WorkspaceRootError::NodeUnavailable(node_did.to_owned()))?;
    Ok(doc)
}

fn value_rows(response: &Value, field: &str) -> Result<Vec<Value>> {
    let response: query::QueryResponse = serde_json::from_value(response.clone())?;
    Ok(required_rows(&response, field)?)
}

type RootDoc = gents_protocol::document_query::fields::workspace_root::Doc;

fn root_selection() -> gents_protocol::document_query::Selection<RootDoc> {
    use gents_protocol::document_query::{fields::workspace_root as fields, Selection};
    Selection::doc_id()
        .field(fields::NODE_DID)
        .field(fields::NODE_DOC_ID)
        .field(fields::ROOT_PATH)
        .field(fields::DISPLAY_NAME)
        .field(fields::ENABLED)
        .field(fields::UPDATED_AT)
}

async fn root_row(txn: &ConfigApplyTxn<'_>, root_path: &str) -> Result<Option<Value>> {
    use gents_protocol::document_query::{fields::workspace_root as fields, Filter, Query};
    let rendered = Query::select(root_selection())
        .filter(Filter::eq(fields::ROOT_PATH, root_path))
        .render();
    let response = txn.run(&rendered).await?;
    let mut rows: Vec<Value> = value_rows(&response, "WorkspaceRoot")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "multiple visible WorkspaceRoot documents share a path"
    );
    Ok(rows.pop())
}

async fn existing_root(txn: &ConfigApplyTxn<'_>, root_path: &str) -> Applied<Value> {
    Ok(root_row(txn, root_path)
        .await?
        .ok_or_else(|| WorkspaceRootError::RootMissing(root_path.to_owned()))?)
}

fn checked_root_doc<'a>(row: &'a Value, node_did: &str, node_doc: &str) -> Applied<&'a str> {
    if row["node_did"] != node_did || row["node_doc_id"] != node_doc {
        return Err(WorkspaceRootError::ForeignRoot(
            row["root_path"].as_str().unwrap_or_default().to_owned(),
        )
        .into());
    }
    Ok(row["_docID"]
        .as_str()
        .context("WorkspaceRoot is missing its physical ID")?)
}

/// This write is the native Node authorization check and the shared transaction
/// conflict fence. A failed child write rolls the fence back with the child.
/// Root publications read every input and write every effect inside their
/// transaction, so a confirmed conflict with the runtime's own Node writes is
/// rebuilt from fresh reads and signed again rather than surfaced.
async fn fence_node(txn: &ConfigApplyTxn<'_>, node_doc: &str) -> Result<()> {
    let mutation = format!("mutation($input: NodeMutationInputArg!) {{ update_Node(docID: \"{}\", input: $input) {{ _docID }} }}", escape_graphql_string(node_doc));
    let response = txn
        .execute_client_signed_update(
            &mutation,
            &json!({"input":{"workspace_root_revision":uuid::Uuid::new_v4().to_string()}}),
            gents_protocol::schemas::NODE,
        )
        .await?;
    let docs = mutation_documents(&response, "update_Node")?
        .context("Node update returned no confirmation")?;
    anyhow::ensure!(
        docs.len() == 1 && docs[0]["_docID"] == node_doc,
        "Node update did not confirm the selected document"
    );
    Ok(())
}

async fn reader_subject(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent_did: &str,
    change: ReaderChange,
) -> Result<ReaderSubject> {
    if agent_did == node_did {
        return Ok(ReaderSubject::NodeActor);
    }
    if change == ReaderChange::Revoke {
        return Ok(ReaderSubject::UnknownActor);
    }
    let Some((_, row)) =
        read_desired_state_record_in_txn(txn, Collection::Agent, node_did, agent_did).await?
    else {
        return Ok(ReaderSubject::UnknownActor);
    };
    Ok(
        if row["agent_did"] != agent_did || row["node_did"] != node_did {
            ReaderSubject::UnknownActor
        } else if row["enabled"] == false {
            ReaderSubject::DisabledAgent
        } else {
            ReaderSubject::EnabledAgent
        },
    )
}

async fn admitted_reader(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent_did: &str,
    change: ReaderChange,
) -> Applied<identity::Did> {
    let did = identity::Did::new(agent_did).map_err(anyhow::Error::from)?;
    match reader_decision(
        change,
        reader_subject(txn, node_did, agent_did, change).await?,
    ) {
        ReaderDecision::Admitted => Ok(did),
        ReaderDecision::AgentUnavailable => {
            Err(WorkspaceRootError::AgentUnavailable(agent_did.to_owned()).into())
        }
        ReaderDecision::NodeActor => Err(WorkspaceRootError::NodeActor.into()),
    }
}

async fn publish_in_txn(
    txn: &ConfigApplyTxn<'_>,
    input: &WorkspaceRootPublish,
) -> Applied<WorkspaceRootReceipt> {
    let (node_did, root_path) = (input.node_did.as_str(), input.root_path.as_str());
    if !std::path::Path::new(root_path).is_absolute() {
        return Err(WorkspaceRootError::PathNotAbsolute(root_path.to_owned()).into());
    }
    let node_doc = selected_node(txn, node_did).await?;
    let existing = root_row(txn, root_path).await?;
    if let Some(row) = &existing {
        checked_root_doc(row, node_did, &node_doc)?;
    }
    let mut readers = Vec::new();
    for agent_did in input
        .reader_agents
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
    {
        readers.push((
            agent_did.clone(),
            admitted_reader(txn, node_did, agent_did, ReaderChange::Grant).await?,
        ));
    }
    fence_node(txn, &node_doc).await?;
    let stored = match &existing {
        None => StoredRoot::Absent,
        Some(row) if row["enabled"] == false => StoredRoot::Disabled,
        Some(_) => StoredRoot::Enabled,
    };
    let enabled = enabled_after(stored, input.enabled);
    let display_name = if writes_display_name(stored, NameUpdate::from(&input.display_name)) {
        input.display_name.owned_value()
    } else {
        existing
            .as_ref()
            .and_then(|row| row["display_name"].as_str())
            .map(str::to_owned)
    };
    let updated_at = chrono::Utc::now().to_rfc3339();
    let created = existing.is_none();
    let doc = if let Some(row) = existing {
        let doc = checked_root_doc(&row, node_did, &node_doc)?;
        let mutation = format!("mutation($input: WorkspaceRootMutationInputArg!) {{ update_WorkspaceRoot(docID: \"{}\", input: $input) {{ _docID }} }}", escape_graphql_string(doc));
        let result = txn.execute_client_signed_update(&mutation, &json!({"input":{"display_name":display_name,"enabled":enabled,"updated_at":updated_at}}), gents_protocol::schemas::WORKSPACE_ROOT).await?;
        let docs = mutation_documents(&result, "update_WorkspaceRoot")
            .map_err(anyhow::Error::from)?
            .context("WorkspaceRoot update returned no confirmation")?;
        if docs.len() != 1 || docs[0]["_docID"] != doc {
            return Err(anyhow::anyhow!("WorkspaceRoot update was not confirmed").into());
        }
        doc.to_owned()
    } else {
        let result = txn.execute_client_signed_create(
            "mutation($input: WorkspaceRootMutationInputArg!) { create_WorkspaceRoot(input: $input) { _docID } }",
            &json!({"input":{"node_did":node_did,"node_doc_id":node_doc,"root_path":root_path,"display_name":display_name,"enabled":enabled,"updated_at":updated_at}}),
            gents_protocol::schemas::WORKSPACE_ROOT).await?;
        let doc = created_doc_id(&result, "WorkspaceRoot").map_err(anyhow::Error::from)?;
        txn.add_resource_relationship("WorkspaceRoot", &doc, "parent", "nodes", &node_doc)
            .await?;
        gents_store::config_client::publish_node_managers_in_txn(
            txn,
            "WorkspaceRoot",
            &doc,
            "reader_manager",
            &node_doc,
        )
        .await?;
        doc
    };
    let mut granted_agents = Vec::new();
    for (agent_did, reader) in readers {
        txn.add_actor_relationship("WorkspaceRoot", &doc, "reader", &reader)
            .await?;
        granted_agents.push(agent_did);
    }
    Ok(WorkspaceRootReceipt {
        doc_id: doc,
        root_path: root_path.to_owned(),
        display_name,
        enabled,
        created,
        granted_agents,
    })
}

async fn publish_spelled(
    access: &ConfigAccess,
    input: &WorkspaceRootPublish,
) -> Applied<WorkspaceRootReceipt> {
    access
        .transact_signed_conflict_rebuild("workspace_root.set", |txn| {
            Box::pin(publish_in_txn(txn, input))
        })
        .await
}

/// Publishes a root for a Node, or patches the root already published at the
/// path, in one transaction with its Node binding, parent relation and the
/// selected readers. Fields the caller omits keep their stored value, and no
/// publication revokes a reader.
pub async fn publish_workspace_root(
    access: &ConfigAccess,
    input: &WorkspaceRootPublish,
) -> std::result::Result<WorkspaceRootReceipt, WorkspaceRootError> {
    let input = WorkspaceRootPublish {
        root_path: normalize_root_path(&input.root_path)?,
        ..input.clone()
    };
    Ok(publish_spelled(access, &input).await?)
}

/// Grants or revokes one Agent's reader relation on the root at the path,
/// without changing the root.
pub async fn change_workspace_root_reader(
    access: &ConfigAccess,
    input: &WorkspaceRootReaderInput,
) -> std::result::Result<WorkspaceRootReaderReceipt, WorkspaceRootError> {
    let root_path = normalize_root_path(&input.root_path)?;
    change_reader_spelled(access, &root_path, &input.agent_did, input.change).await?;
    Ok(WorkspaceRootReaderReceipt {
        root_path,
        agent_did: input.agent_did.clone(),
        change: input.change,
    })
}

/// Removes the root at the path. Only an actor who may update the root's Node
/// removes it, whichever Node that is.
pub async fn remove_workspace_root(
    access: &ConfigAccess,
    root_path: &str,
) -> std::result::Result<WorkspaceRootRemoved, WorkspaceRootError> {
    let root_path = normalize_root_path(root_path)?;
    let doc_id = delete_spelled(access, &root_path).await?;
    Ok(WorkspaceRootRemoved { root_path, doc_id })
}

/// The published roots this actor can read, ordered by path.
pub async fn list_workspace_roots(
    access: &ConfigAccess,
) -> std::result::Result<Vec<WorkspaceRootView>, WorkspaceRootError> {
    use gents_protocol::document_query::Query;
    let mut roots = read_roots(access, Query::select(root_selection())).await?;
    roots.sort_by(|left, right| left.root_path.cmp(&right.root_path));
    Ok(roots)
}

/// The published root at the path.
pub async fn show_workspace_root(
    access: &ConfigAccess,
    root_path: &str,
) -> std::result::Result<WorkspaceRootView, WorkspaceRootError> {
    use gents_protocol::document_query::{fields::workspace_root as fields, Filter, Query};
    let root_path = normalize_root_path(root_path)?;
    let query = Query::select(root_selection()).filter(Filter::eq(fields::ROOT_PATH, &root_path));
    read_roots(access, query)
        .await?
        .into_iter()
        .next()
        .ok_or(WorkspaceRootError::RootMissing(root_path))
}

async fn read_roots(
    access: &ConfigAccess,
    query: gents_protocol::document_query::Query<RootDoc>,
) -> std::result::Result<Vec<WorkspaceRootView>, WorkspaceRootError> {
    let passthrough = |error: anyhow::Error| {
        WorkspaceRootError::Passthrough(error.reallocate_into_boxed_dyn_error_without_backtrace())
    };
    let response = access
        .read(&query.render())
        .await
        .map_err(|error| passthrough(error.into()))?;
    gents_protocol::graphql::graphql_rows_from_response(&response, "WorkspaceRoot")
        .into_iter()
        .map(|row| serde_json::from_value(row).map_err(|error| passthrough(error.into())))
        .collect()
}

/// Publishes an immutable physical Node binding and the native parent relation in
/// one transaction. Existing roots retain their current relationships on update.
pub async fn set_workspace_root(
    access: &ConfigAccess,
    node_did: &str,
    root_path: &str,
    display_name: Option<&str>,
    enabled: bool,
) -> std::result::Result<String, WorkspaceRootWriteError> {
    set_workspace_root_with_readers(access, node_did, root_path, display_name, enabled, &[]).await
}

/// The runtime's and the fixtures' whole-value publication of a path they
/// already spell canonically: every field is named, so nothing is preserved.
/// Each reader is an explicit operator selection; an ordinary update with no
/// selections never restores an Agent reader that the operator revoked.
pub async fn set_workspace_root_with_readers(
    access: &ConfigAccess,
    node_did: &str,
    root_path: &str,
    display_name: Option<&str>,
    enabled: bool,
    reader_agents: &[String],
) -> std::result::Result<String, WorkspaceRootWriteError> {
    let input = WorkspaceRootPublish {
        node_did: node_did.to_owned(),
        root_path: root_path.to_owned(),
        display_name: display_name.map_or(StringUpdate::Clear, |name| {
            StringUpdate::Set(name.to_owned())
        }),
        enabled: EnabledUpdate::from(Some(enabled)),
        reader_agents: reader_agents.to_vec(),
    };
    publish_spelled(access, &input)
        .await
        .map(|receipt| receipt.doc_id)
        .map_err(|error| WorkspaceRootWriteError::Publication(error.into()))
}

/// Grants a selected Agent read access without changing the root's state.
pub async fn grant_workspace_root_reader(
    access: &ConfigAccess,
    root_path: &str,
    agent_did: &str,
) -> std::result::Result<(), WorkspaceRootWriteError> {
    change_reader_spelled(access, root_path, agent_did, ReaderChange::Grant)
        .await
        .map_err(|error| WorkspaceRootWriteError::Publication(error.into()))
}

/// Revocation acts on the exact visible root and actor in the same
/// Node-authorized transaction, even if the Agent document has been deleted.
pub async fn revoke_workspace_root_reader(
    access: &ConfigAccess,
    root_path: &str,
    agent_did: &str,
) -> std::result::Result<(), WorkspaceRootWriteError> {
    change_reader_spelled(access, root_path, agent_did, ReaderChange::Revoke)
        .await
        .map_err(|error| WorkspaceRootWriteError::Publication(error.into()))
}

async fn change_reader_spelled(
    access: &ConfigAccess,
    root_path: &str,
    agent_did: &str,
    change: ReaderChange,
) -> Applied<()> {
    access
        .transact_signed_conflict_rebuild("workspace_root.reader", |txn| {
            Box::pin(async move {
                let row = existing_root(txn, root_path).await?;
                let node_did = row["node_did"]
                    .as_str()
                    .context("workspace root is missing Node scope")?;
                let node_doc = selected_node(txn, node_did).await?;
                let doc = checked_root_doc(&row, node_did, &node_doc)?;
                let reader = admitted_reader(txn, node_did, agent_did, change).await?;
                fence_node(txn, &node_doc).await?;
                match change {
                    ReaderChange::Grant => {
                        txn.add_actor_relationship("WorkspaceRoot", doc, "reader", &reader)
                            .await?
                    }
                    ReaderChange::Revoke => {
                        txn.delete_actor_relationship("WorkspaceRoot", doc, "reader", &reader)
                            .await?
                    }
                }
                Ok(())
            })
        })
        .await
}

/// Removes the exact visible root using the native signed-delete adapter. The
/// root's physical Node binding and the Node update fence are checked first.
pub async fn delete_workspace_root(
    access: &ConfigAccess,
    root_path: &str,
) -> std::result::Result<(), WorkspaceRootWriteError> {
    delete_spelled(access, root_path)
        .await
        .map(drop)
        .map_err(|error| WorkspaceRootWriteError::Publication(error.into()))
}

async fn delete_spelled(access: &ConfigAccess, root_path: &str) -> Applied<String> {
    access
        .transact_signed_conflict_rebuild("workspace_root.delete", |txn| {
            Box::pin(async move {
                let row = existing_root(txn, root_path).await?;
                let node_did = row["node_did"]
                    .as_str()
                    .context("workspace root is missing Node scope")?;
                let node_doc = selected_node(txn, node_did).await?;
                let doc = checked_root_doc(&row, node_did, &node_doc)?;
                fence_node(txn, &node_doc).await?;
                txn.delete_configuration_document("WorkspaceRoot", doc)
                    .await?;
                Ok(doc.to_owned())
            })
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_store::config_client::GraphqlEndpoint;
    use gents_store::identity::NodeIdentity;
    use std::sync::Arc;

    fn client(endpoint: &str, key: &dyn NodeIdentity) -> Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(endpoint, key.did())
                .with_client_signing_capability(key.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        ))
    }

    #[test]
    fn publication_and_reader_decisions_match_lean() {
        use gents_lean_vocab::families;
        use gents_protocol::operations::workspace_root::WorkspaceRootRefusal;
        families::workspace_root_operations_enabled_after::replay(|i| {
            enabled_after(i.stored, i.toggle)
        });
        families::workspace_root_operations_writes_display_name::replay(|i| {
            writes_display_name(i.stored, i.name)
        });
        families::workspace_root_operations_reader_decision::replay(|i| {
            reader_decision(i.change, i.subject)
        });
        families::workspace_root_operations_refusal_recovery::replay(|i| {
            WorkspaceRootRefusal::recovery(i.refusal)
        });
    }

    #[tokio::test]
    async fn native_workspace_root_publication_obeys_node_authority() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let user = gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("user.key"),
            None,
        )?;
        let runtime = gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("runtime.key"),
            None,
        )?;
        let writer_binding = gents_store::identity::provision_agent_identity(&runtime)?;
        let integrator_binding = gents_store::identity::provision_agent_identity(&runtime)?;
        let writer = gents_store::identity::resolve_agent_identity(&runtime, &writer_binding)?;
        let integrator =
            gents_store::identity::resolve_agent_identity(&runtime, &integrator_binding)?;
        let other = gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("other.key"),
            None,
        )?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let node = Arc::new(
            defra_node::Node::builder()
                .enable_signing()
                .with_node_identity_did(runtime.did())
                .with_node_acp_owner(user.did().to_owned())
                .with_http(defra_node::HttpConfig::with_listener(listener)?)
                .build()
                .await?,
        );
        let context =
            defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
        let policy = node
            .add_dac_policy_with_counter_as(
                &context,
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
                0,
            )
            .await?;
        assert_eq!(policy, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID);
        for key in [
            &runtime as &dyn NodeIdentity,
            writer.as_ref(),
            integrator.as_ref(),
            &other,
        ] {
            for permission in [
                acp::nac::NodePermission::DocumentRead,
                acp::nac::NodePermission::DocumentUpdate,
                acp::nac::NodePermission::CollectionGet,
            ] {
                node.add_nac_actor_relationship_as(&context, permission.as_str(), key.did())
                    .await?;
            }
        }
        node.add_collection_as(&context, gents_protocol::schemas::NODE)
            .await?;
        node.add_collection_as(&context, gents_protocol::schemas::AGENT)
            .await?;
        let binding = format!(
            "type WorkspaceRoot @policy(id: \"{policy}\", resource: \"workspace_roots\") {{"
        );
        let schema =
            gents_protocol::schemas::WORKSPACE_ROOT.replacen("type WorkspaceRoot {", &binding, 1);
        node.add_collection_as(&context, &schema).await?;
        let endpoint = format!("http://{address}/api/v0/graphql");
        let owner = client(&endpoint, &user)?;
        let node_doc = owner
            .transact_write("test.root.node", |txn| {
                Box::pin(async {
                    let response = txn.execute_client_signed_create(
                "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                &json!({"input":{"node_did":runtime.did()}}), gents_protocol::schemas::NODE).await?;
                    let doc = created_doc_id(&response, "Node")?;
                    txn.add_actor_relationship(
                        "Node",
                        &doc,
                        "runtime_reader",
                        &identity::Did::new(runtime.did())?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(doc)
                })
            })
            .await?;
        for (agent, nonce) in [
            (writer.as_ref(), &writer_binding.creation_nonce),
            (integrator.as_ref(), &integrator_binding.creation_nonce),
        ] {
            owner.transact_write("test.root.agent", |txn| Box::pin(async {
                txn.execute_client_signed_create(
                    "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                    &json!({"input":{"node_did":runtime.did(),"agent_did":agent.did(),"creation_nonce":nonce,"context_id":format!("{}:context", agent.did()),"inference_profile_id":format!("{}:inference", agent.did()),"enabled":true}}),
                    gents_protocol::schemas::AGENT).await?;
                Ok::<_, anyhow::Error>(())
            })).await?;
        }
        // The execution guard admits a persisted root only when it is already
        // its own canonical resolution (macOS temp paths are symlinked).
        let path = std::fs::canonicalize(directory.path())?.join("one");
        std::fs::create_dir(&path)?;
        let path = path.to_string_lossy().into_owned();
        let readers = vec![writer.did().to_owned(), integrator.did().to_owned()];
        let root_doc = set_workspace_root_with_readers(
            &owner,
            runtime.did(),
            &path,
            Some("one"),
            true,
            &readers,
        )
        .await?;
        let query = format!("{{ WorkspaceRoot(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID _version {{ cid height fieldName }} }} }}", escape_graphql_string(&root_doc));
        let signed = owner.execute(&query).await?;
        let signed_rows: Vec<Value> = value_rows(&signed, "WorkspaceRoot")?;
        let [signed_root] = signed_rows.as_slice() else {
            anyhow::bail!("published root was not visible")
        };
        let version =
            gents_store::graphql::document_composite_version(signed_root, "workspace root")?
                .context("published root is missing a composite version")?;
        assert_eq!(
            node.verified_block_signer_did(&version.cid).await?,
            user.did()
        );
        let cases = gents_lean_vocab::families::workspace_root_access_root_access::cases();
        let key_of = |actor: &str| -> &dyn NodeIdentity {
            match actor {
                "user" => &user,
                "runtime" => &runtime,
                "writer" => writer.as_ref(),
                "integrator" => integrator.as_ref(),
                "other" => &other,
                actor => panic!("unknown modeled actor {actor}"),
            }
        };
        let permission_of = |permission: &str| match permission {
            "read" => defra_node::DocumentPermission::Read,
            "update" => defra_node::DocumentPermission::Update,
            "delete" => defra_node::DocumentPermission::Delete,
            permission => panic!("unknown modeled permission {permission}"),
        };
        let guard = crate::surface::RootExecutionGuard {
            agent_did: writer.did().to_owned(),
            selected_root: Some(std::path::PathBuf::from(&path)),
            ceiling_root: None,
        };
        let mut expected = Vec::new();
        let mut may_publish = Vec::new();
        let mut writer_execution = Vec::new();
        for case in &cases {
            let key = key_of(&case.input.actor);
            let allowed = node
                .document_acp()
                .check_doc_access(
                    &defra_node::AcpIdentity::Authenticated(key.did().parse()?),
                    permission_of(&case.input.permission),
                    &policy,
                    "workspace_roots",
                    &root_doc,
                )
                .await?;
            expected.push(allowed);
            may_publish.push(
                node.document_acp()
                    .check_doc_access(
                        &defra_node::AcpIdentity::Authenticated(key.did().parse()?),
                        defra_node::DocumentPermission::Update,
                        &policy,
                        "nodes",
                        &node_doc,
                    )
                    .await?,
            );
            writer_execution.push(guard.validate(&node, key).await.is_ok());
            if case.input.permission == "read" {
                let query = format!(
                    "{{ WorkspaceRoot(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
                    escape_graphql_string(&root_doc)
                );
                let response = client(&endpoint, key)?.execute(&query).await?;
                assert_eq!(
                    !value_rows(&response, "WorkspaceRoot")?.is_empty(),
                    allowed,
                    "{:?}",
                    case.input
                );
            }
        }
        let writer_access = client(&endpoint, writer.as_ref())?;
        let update = format!("mutation($input: WorkspaceRootMutationInputArg!) {{ update_WorkspaceRoot(docID: \"{}\", input: $input) {{ _docID }} }}", escape_graphql_string(&root_doc));
        let denied_update = writer_access
            .transact_write("test.root.reader_update_denied", |txn| {
                let update = &update;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        update,
                        &json!({"input":{"enabled":false}}),
                        gents_protocol::schemas::WORKSPACE_ROOT,
                    )
                    .await
                })
            })
            .await;
        let changed = match denied_update {
            Ok(response) => !mutation_documents(&response, "update_WorkspaceRoot")?
                .unwrap_or_default()
                .is_empty(),
            Err(_) => false,
        };
        let writer_update = cases
            .iter()
            .position(|case| case.input.actor == "writer" && case.input.permission == "update")
            .context("missing modeled writer update")?;
        assert_eq!(changed, expected[writer_update]);
        let observed = owner
            .execute(&format!(
                "{{ WorkspaceRoot(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ enabled }} }}",
                escape_graphql_string(&root_doc)
            ))
            .await?;
        assert_eq!(observed["data"]["WorkspaceRoot"][0]["enabled"], true);
        let runtime_access = client(&endpoint, &runtime)?;
        let before = owner
            .execute(&format!(
                "{{ Node(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ workspace_root_revision }} }}",
                escape_graphql_string(&node_doc)
            ))
            .await?;
        let denied_path = directory
            .path()
            .join("denied")
            .to_string_lossy()
            .into_owned();
        assert!(
            set_workspace_root(&runtime_access, runtime.did(), &denied_path, None, true)
                .await
                .is_err()
        );
        let after = owner
            .execute(&format!(
                "{{ Node(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ workspace_root_revision }} }}",
                escape_graphql_string(&node_doc)
            ))
            .await?;
        assert_eq!(before, after);
        assert!(root_row_in_access(&owner, &denied_path).await?.is_empty());
        assert!(set_workspace_root_with_readers(
            &owner,
            runtime.did(),
            &denied_path,
            None,
            true,
            &[other.did().to_owned()]
        )
        .await
        .is_err());
        assert!(root_row_in_access(&owner, &denied_path).await?.is_empty());
        let aborted_path = directory
            .path()
            .join("aborted")
            .to_string_lossy()
            .into_owned();
        let runtime_did = runtime.did().to_owned();
        let writer_did = writer.did().to_owned();
        let aborted: Result<()> = owner.transact_write("test.root.abort", |txn| {
            let node_doc = &node_doc;
            let aborted_path = &aborted_path;
            let runtime_did = &runtime_did;
            let writer_did = &writer_did;
            Box::pin(async move {
                fence_node(txn, node_doc).await?;
                let response = txn.execute_client_signed_create(
                    "mutation($input: WorkspaceRootMutationInputArg!) { create_WorkspaceRoot(input: $input) { _docID } }",
                    &json!({"input":{"node_did":runtime_did,"node_doc_id":node_doc,"root_path":aborted_path,"enabled":true}}),
                    gents_protocol::schemas::WORKSPACE_ROOT).await?;
                let doc = created_doc_id(&response, "WorkspaceRoot")?;
                txn.add_resource_relationship("WorkspaceRoot", &doc, "parent", "nodes", node_doc).await?;
                txn.add_actor_relationship("WorkspaceRoot", &doc, "reader", &identity::Did::new(writer_did)?).await?;
                anyhow::bail!("intentional abort")
            })
        }).await;
        assert!(aborted.is_err());
        let aborted_document = !root_row_in_access(&owner, &aborted_path).await?.is_empty();
        revoke_workspace_root_reader(&owner, &path, writer.did()).await?;
        let mut writer_execution_after_revocation = Vec::new();
        for case in &cases {
            writer_execution_after_revocation.push(
                guard
                    .validate(&node, key_of(&case.input.actor))
                    .await
                    .is_ok(),
            );
        }
        assert_eq!(
            set_workspace_root(&owner, runtime.did(), &path, None, true).await?,
            root_doc
        );
        let mut after_writer_revocation = Vec::new();
        for case in &cases {
            after_writer_revocation.push(
                node.document_acp()
                    .check_doc_access(
                        &defra_node::AcpIdentity::Authenticated(
                            key_of(&case.input.actor).did().parse()?,
                        ),
                        permission_of(&case.input.permission),
                        &policy,
                        "workspace_roots",
                        &root_doc,
                    )
                    .await?,
            );
        }
        grant_workspace_root_reader(&owner, &path, writer.did()).await?;
        owner
            .delete_resource_relationship("WorkspaceRoot", &root_doc, "parent", "nodes", &node_doc)
            .await?;
        assert_eq!(
            set_workspace_root(&owner, runtime.did(), &path, None, false).await?,
            root_doc
        );
        let mut after_parent_revocation = Vec::new();
        for case in &cases {
            after_parent_revocation.push(
                node.document_acp()
                    .check_doc_access(
                        &defra_node::AcpIdentity::Authenticated(
                            key_of(&case.input.actor).did().parse()?,
                        ),
                        permission_of(&case.input.permission),
                        &policy,
                        "workspace_roots",
                        &root_doc,
                    )
                    .await?,
            );
        }
        let query = format!(
            "{{ Agent(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
            escape_graphql_string(integrator.did())
        );
        let agent_rows = owner.execute(&query).await?;
        let agent_doc = agent_rows["data"]["Agent"][0]["_docID"]
            .as_str()
            .context("integrator physical ID missing")?
            .to_owned();
        owner
            .transact_write("test.root.remove_agent", |txn| {
                let agent_doc = &agent_doc;
                Box::pin(async move { txn.delete_configuration_document("Agent", agent_doc).await })
            })
            .await?;
        revoke_workspace_root_reader(&owner, &path, integrator.did()).await?;
        let mut after_integrator_revocation = Vec::new();
        for case in &cases {
            after_integrator_revocation.push(
                node.document_acp()
                    .check_doc_access(
                        &defra_node::AcpIdentity::Authenticated(
                            key_of(&case.input.actor).did().parse()?,
                        ),
                        permission_of(&case.input.permission),
                        &policy,
                        "workspace_roots",
                        &root_doc,
                    )
                    .await?,
            );
        }
        for (index, case) in cases.iter().enumerate() {
            case.check(
                gents_lean_vocab::families::workspace_root_access_root_access::AccessOutput {
                    expected: expected[index],
                    may_publish: may_publish[index],
                    writer_execution: writer_execution[index],
                    aborted_document,
                    writer_execution_after_revocation: writer_execution_after_revocation[index],
                    after_writer_revocation: after_writer_revocation[index],
                    after_parent_revocation: after_parent_revocation[index],
                    after_integrator_revocation: after_integrator_revocation[index],
                },
            );
        }
        delete_workspace_root(&owner, &path).await?;
        assert!(root_row_in_access(&owner, &path).await?.is_empty());
        node.shutdown().await?;
        Ok(())
    }

    async fn root_row_in_access(access: &ConfigAccess, path: &str) -> Result<Vec<Value>> {
        let query = format!(
            "{{ WorkspaceRoot(filter: {{root_path: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
            escape_graphql_string(path)
        );
        value_rows(&access.execute(&query).await?, "WorkspaceRoot")
    }
}
