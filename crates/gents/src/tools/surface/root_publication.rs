use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::store::config_client::{read_desired_state_record_in_txn, ConfigAccess, ConfigApplyTxn};
use crate::store::graphql::{created_doc_id, escape_graphql_string, mutation_documents, rows};
use crate::Collection;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceRootWriteError {
    #[error("workspace root publication failed")]
    Publication(#[source] anyhow::Error),
}

async fn selected_node(txn: &ConfigApplyTxn<'_>, node_did: &str) -> Result<String> {
    anyhow::ensure!(
        txn.actor_did().is_some(),
        "workspace root publication requires an authenticated actor"
    );
    identity::Did::new(node_did)?;
    let (doc, _) = read_desired_state_record_in_txn(txn, Collection::Node, node_did, node_did)
        .await?
        .context("selected Node is unavailable")?;
    Ok(doc)
}

async fn root_row(txn: &ConfigApplyTxn<'_>, root_path: &str) -> Result<Option<Value>> {
    let response = txn.execute(&format!(
        "{{ WorkspaceRoot(filter: {{root_path: {{_eq: \"{}\"}}}}) {{ _docID node_did node_doc_id }} }}",
        escape_graphql_string(root_path)
    )).await?;
    let mut rows: Vec<Value> = rows(&response, "WorkspaceRoot")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "multiple visible WorkspaceRoot documents share a path"
    );
    Ok(rows.pop())
}

fn checked_root_doc<'a>(row: &'a Value, node_did: &str, node_doc: &str) -> Result<&'a str> {
    anyhow::ensure!(
        row["node_did"] == node_did && row["node_doc_id"] == node_doc,
        "WorkspaceRoot does not belong to the selected physical Node"
    );
    row["_docID"]
        .as_str()
        .context("WorkspaceRoot is missing its physical ID")
}

/// This write is the native Node authorization check and the shared transaction
/// conflict fence. A failed child write rolls the fence back with the child.
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

/// Publishes an immutable physical Node binding and the native parent relation in
/// one transaction. Existing roots retain their current relationships on update.
pub async fn set_workspace_root(
    access: &ConfigAccess,
    node_did: &str,
    root_path: &str,
    display_name: Option<&str>,
    enabled: bool,
) -> std::result::Result<String, WorkspaceRootWriteError> {
    let result: Result<String> = access.transact_write("workspace_root.set", |txn| Box::pin(async move {
        anyhow::ensure!(std::path::Path::new(root_path).is_absolute(), "workspace root path must be absolute");
        let node_doc = selected_node(txn, node_did).await?;
        let existing = root_row(txn, root_path).await?;
        if let Some(row) = &existing { checked_root_doc(row, node_did, &node_doc)?; }
        fence_node(txn, &node_doc).await?;
        let updated_at = chrono::Utc::now().to_rfc3339();
        if let Some(row) = existing {
            let doc = checked_root_doc(&row, node_did, &node_doc)?;
            let mutation = format!("mutation($input: WorkspaceRootMutationInputArg!) {{ update_WorkspaceRoot(docID: \"{}\", input: $input) {{ _docID }} }}", escape_graphql_string(doc));
            let result = txn.execute_client_signed_update(&mutation, &json!({"input":{"display_name":display_name,"enabled":enabled,"updated_at":updated_at}}), gents_protocol::schemas::WORKSPACE_ROOT).await?;
            let docs = mutation_documents(&result, "update_WorkspaceRoot")?.context("WorkspaceRoot update returned no confirmation")?;
            anyhow::ensure!(docs.len() == 1 && docs[0]["_docID"] == doc, "WorkspaceRoot update was not confirmed");
            Ok(doc.to_owned())
        } else {
            let result = txn.execute_client_signed_create(
                "mutation($input: WorkspaceRootMutationInputArg!) { create_WorkspaceRoot(input: $input) { _docID } }",
                &json!({"input":{"node_did":node_did,"node_doc_id":node_doc,"root_path":root_path,"display_name":display_name,"enabled":enabled,"updated_at":updated_at}}),
                gents_protocol::schemas::WORKSPACE_ROOT).await?;
            let doc = created_doc_id(&result, "WorkspaceRoot")?;
            txn.add_resource_relationship("WorkspaceRoot", &doc, "parent", "nodes", &node_doc).await?;
            Ok(doc)
        }
    })).await;
    result.map_err(WorkspaceRootWriteError::Publication)
}

/// Removes the exact visible root using the native signed-delete adapter. The
/// root's physical Node binding and the Node update fence are checked first.
pub async fn delete_workspace_root(
    access: &ConfigAccess,
    root_path: &str,
) -> std::result::Result<(), WorkspaceRootWriteError> {
    let result: Result<()> = access
        .transact_write("workspace_root.delete", |txn| {
            Box::pin(async move {
                let row = root_row(txn, root_path)
                    .await?
                    .context("workspace root is unavailable")?;
                let node_did = row["node_did"]
                    .as_str()
                    .context("workspace root is missing Node scope")?;
                let node_doc = selected_node(txn, node_did).await?;
                let doc = checked_root_doc(&row, node_did, &node_doc)?;
                fence_node(txn, &node_doc).await?;
                txn.delete_configuration_document("WorkspaceRoot", doc)
                    .await?;
                Ok(())
            })
        })
        .await;
    result.map_err(WorkspaceRootWriteError::Publication)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::config_client::GraphqlEndpoint;
    use crate::store::identity::NodeIdentity;
    use std::sync::Arc;

    fn client(endpoint: &str, key: &crate::KeyIdentity) -> Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(endpoint, key.did())
                .with_client_signing_capability(key.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        ))
    }

    #[tokio::test]
    async fn native_workspace_root_publication_obeys_node_authority() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let user = crate::KeyIdentity::load_or_create(directory.path().join("user.key"), None)?;
        let runtime =
            crate::KeyIdentity::load_or_create(directory.path().join("runtime.key"), None)?;
        let agent = crate::KeyIdentity::load_or_create(directory.path().join("agent.key"), None)?;
        let other = crate::KeyIdentity::load_or_create(directory.path().join("other.key"), None)?;
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
        for key in [&runtime, &agent, &other] {
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
        let path = directory.path().join("one").to_string_lossy().into_owned();
        let root_doc = set_workspace_root(&owner, runtime.did(), &path, Some("one"), true).await?;
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().workspace_root_access_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let key = match case["actor"].as_str().unwrap() {
                "user" => &user,
                "runtime" => &runtime,
                "agent" => &agent,
                "other" => &other,
                actor => panic!("unknown modeled actor {actor}"),
            };
            let permission = match case["permission"].as_str().unwrap() {
                "read" => defra_node::DocumentPermission::Read,
                "update" => defra_node::DocumentPermission::Update,
                "delete" => defra_node::DocumentPermission::Delete,
                permission => panic!("unknown modeled permission {permission}"),
            };
            let allowed = node
                .document_acp()
                .check_doc_access(
                    &defra_node::AcpIdentity::Authenticated(key.did().parse()?),
                    permission,
                    &policy,
                    "workspace_roots",
                    &root_doc,
                )
                .await?;
            assert_eq!(allowed, case["expected"].as_bool().unwrap(), "{case}");
            let may_publish = node
                .document_acp()
                .check_doc_access(
                    &defra_node::AcpIdentity::Authenticated(key.did().parse()?),
                    defra_node::DocumentPermission::Update,
                    &policy,
                    "nodes",
                    &node_doc,
                )
                .await?;
            assert_eq!(
                may_publish,
                case["may_publish"].as_bool().unwrap(),
                "{case}"
            );
            if case["permission"] == "read" {
                let query = format!(
                    "{{ WorkspaceRoot(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
                    escape_graphql_string(&root_doc)
                );
                let response = client(&endpoint, key)?.execute(&query).await?;
                assert_eq!(
                    !rows::<Value>(&response, "WorkspaceRoot")?.is_empty(),
                    allowed,
                    "{case}"
                );
            }
        }
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
        let aborted_path = directory
            .path()
            .join("aborted")
            .to_string_lossy()
            .into_owned();
        let runtime_did = runtime.did().to_owned();
        let aborted: Result<()> = owner.transact_write("test.root.abort", |txn| {
            let node_doc = &node_doc;
            let aborted_path = &aborted_path;
            let runtime_did = &runtime_did;
            Box::pin(async move {
                fence_node(txn, node_doc).await?;
                let response = txn.execute_client_signed_create(
                    "mutation($input: WorkspaceRootMutationInputArg!) { create_WorkspaceRoot(input: $input) { _docID } }",
                    &json!({"input":{"node_did":runtime_did,"node_doc_id":node_doc,"root_path":aborted_path,"enabled":true}}),
                    gents_protocol::schemas::WORKSPACE_ROOT).await?;
                let doc = created_doc_id(&response, "WorkspaceRoot")?;
                txn.add_resource_relationship("WorkspaceRoot", &doc, "parent", "nodes", node_doc).await?;
                anyhow::bail!("intentional abort")
            })
        }).await;
        assert!(aborted.is_err());
        assert_eq!(
            !root_row_in_access(&owner, &aborted_path).await?.is_empty(),
            cases[0]["aborted_document"].as_bool().unwrap()
        );
        owner
            .delete_resource_relationship("WorkspaceRoot", &root_doc, "parent", "nodes", &node_doc)
            .await?;
        assert_eq!(
            set_workspace_root(&owner, runtime.did(), &path, None, false).await?,
            root_doc
        );
        for case in cases {
            let key = match case["actor"].as_str().unwrap() {
                "user" => &user,
                "runtime" => &runtime,
                "agent" => &agent,
                "other" => &other,
                actor => panic!("unknown modeled actor {actor}"),
            };
            let permission = match case["permission"].as_str().unwrap() {
                "read" => defra_node::DocumentPermission::Read,
                "update" => defra_node::DocumentPermission::Update,
                "delete" => defra_node::DocumentPermission::Delete,
                permission => panic!("unknown modeled permission {permission}"),
            };
            let allowed = node
                .document_acp()
                .check_doc_access(
                    &defra_node::AcpIdentity::Authenticated(key.did().parse()?),
                    permission,
                    &policy,
                    "workspace_roots",
                    &root_doc,
                )
                .await?;
            assert_eq!(
                allowed,
                case["after_parent_revocation"].as_bool().unwrap(),
                "{case}"
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
        Ok(rows(&access.execute(&query).await?, "WorkspaceRoot")?)
    }
}
