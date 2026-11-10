//! Desktop folder selection for an agent's host tool root.
//!
//! The Node owner publishes the WorkspaceRoot and grants the agent reader
//! access, then the agent's Tools document names the folder in `host.root`.
//! Publication owns its own signed transaction, so the retarget commits
//! separately; both steps are idempotent and a repeated selection finishes a
//! selection whose retarget failed. The caller restarts the runtime with the
//! reviewed ceiling after this returns: `gents serve` refuses a configured
//! root that is not published.

use anyhow::{Context, Result};
use gents::documents::{load_agent_context_and_tools_in_txn, write_tools_document_in_txn, Tools};
use gents::store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents::store::graphql::escape_graphql_string;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkspaceRoot {
    pub root_path: String,
    pub workspace_root_doc_id: String,
    pub tools_id: String,
}

/// Publishes `root_path` for `node_did`, grants `agent_did` reader access, and
/// points that agent's Tools `host.root` at it. A Tools document that cannot
/// be retargeted is refused before anything is published; publication comes
/// before the retarget so a later runtime start admits the root.
pub async fn select_agent_workspace_root(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
    root_path: &str,
) -> Result<AgentWorkspaceRoot> {
    ensure_absolute(root_path)?;
    access
        .transact_readonly("workspace_root.agent_tools", |txn| {
            Box::pin(async move { read_unshared_agent_tools(txn, node_did, agent_did).await })
        })
        .await?;
    let workspace_root_doc_id = gents::tools::surface::set_workspace_root_with_readers(
        access,
        node_did,
        root_path,
        None,
        true,
        &[agent_did.to_owned()],
    )
    .await?;
    let tools_id = retarget_agent_tools(access, node_did, agent_did, root_path)
        .await
        .map_err(|error| published_but_not_retargeted(root_path, error))?;
    Ok(AgentWorkspaceRoot {
        root_path: root_path.to_owned(),
        workspace_root_doc_id,
        tools_id,
    })
}

fn published_but_not_retargeted(root_path: &str, error: anyhow::Error) -> anyhow::Error {
    error.context(format!(
        "{root_path} was published, but the agent still uses its previous folder. Choose the same folder again to finish."
    ))
}

/// Re-reads and re-checks the Tools document in the transaction that replaces
/// it, so a Context that started sharing it after the first check is refused.
async fn retarget_agent_tools(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
    root_path: &str,
) -> Result<String> {
    access
        .transact_write("workspace_root.retarget_tools", |txn| {
            Box::pin(async move {
                let mut tools = read_unshared_agent_tools(txn, node_did, agent_did).await?;
                if retarget_host_root(&mut tools, root_path) {
                    write_tools_document_in_txn(txn, &tools).await?;
                }
                Ok::<String, anyhow::Error>(tools.tools_id)
            })
        })
        .await
}

fn ensure_absolute(root_path: &str) -> Result<()> {
    anyhow::ensure!(
        std::path::Path::new(root_path).is_absolute(),
        "workspace root path must be absolute, got {root_path}"
    );
    Ok(())
}

async fn read_unshared_agent_tools(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent_did: &str,
) -> Result<Tools> {
    let (context, tools) = load_agent_context_and_tools_in_txn(txn, node_did, agent_did).await?;
    context.context("the agent has no Context document")?;
    let tools = tools.context("the agent has no Tools document")?;
    let response = txn
        .execute(&format!(
            "{{ AgentContext(filter: {{tools_id: {{_eq: \"{}\"}}}}) {{ context_id }} }}",
            escape_graphql_string(&tools.tools_id)
        ))
        .await?;
    let selecting = response["data"]["AgentContext"]
        .as_array()
        .map_or(0, Vec::len);
    ensure_unshared(&tools.tools_id, selecting)?;
    Ok(tools)
}

/// Editing `host.root` of a Tools document that another Context selects would
/// change that other agent's workspace too, so such a document is refused.
fn ensure_unshared(tools_id: &str, selecting_contexts: usize) -> Result<()> {
    anyhow::ensure!(
        selecting_contexts <= 1,
        "Tools {tools_id} is selected by {selecting_contexts} Contexts; changing its host root would change the other agents that use it. Give this agent its own Tools document first."
    );
    Ok(())
}

/// Sets `host.root` and returns whether the document changed. Sibling host
/// settings are preserved; a missing `host` group is created.
pub(crate) fn retarget_host_root(tools: &mut Tools, root_path: &str) -> bool {
    let host = tools.host.get_or_insert_with(Default::default);
    if host.root.as_deref() == Some(root_path) {
        return false;
    }
    host.root = Some(root_path.to_owned());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::store::config_client::GraphqlEndpoint;
    use serde_json::json;

    fn tools(host: serde_json::Value) -> Tools {
        serde_json::from_value(json!({
            "tools_id": "engineer-tools",
            "node_did": "did:key:node",
            "host": host,
        }))
        .expect("tools fixture")
    }

    #[test]
    fn retarget_sets_root_and_preserves_sibling_host_settings() {
        let mut document = tools(json!({"files": {"mode": "ReadOnly"}}));
        assert!(retarget_host_root(&mut document, "/work/one"));
        let host = document.host.as_ref().expect("host group");
        assert_eq!(host.root.as_deref(), Some("/work/one"));
        assert!(host.files.is_some(), "files must survive the retarget");
        assert!(!retarget_host_root(&mut document, "/work/one"));
    }

    #[test]
    fn retarget_creates_the_host_group_when_absent() {
        let mut document: Tools = serde_json::from_value(json!({
            "tools_id": "engineer-tools",
            "node_did": "did:key:node",
        }))
        .expect("tools fixture");
        assert!(retarget_host_root(&mut document, "/work/two"));
        assert_eq!(
            document.host.and_then(|host| host.root).as_deref(),
            Some("/work/two")
        );
    }

    #[test]
    fn a_tools_document_shared_by_another_context_is_refused() {
        assert!(ensure_unshared("engineer-tools", 0).is_ok());
        assert!(ensure_unshared("engineer-tools", 1).is_ok());
        let error = ensure_unshared("engineer-tools", 2).expect_err("shared tools refused");
        assert!(error.to_string().contains("2 Contexts"), "{error}");
    }

    #[test]
    fn only_absolute_roots_are_selected() {
        assert!(ensure_absolute("/work/three").is_ok());
        let error = ensure_absolute("work/three").expect_err("relative root refused");
        assert!(error.to_string().contains("must be absolute"), "{error}");
    }

    #[tokio::test]
    async fn a_relative_root_is_refused_before_any_request_is_sent() {
        let access = ConfigAccess::Graphql(GraphqlEndpoint::anonymous(
            "http://127.0.0.1:9/api/v0/graphql",
        ));
        let error = select_agent_workspace_root(
            &access,
            "did:key:node",
            "did:key:agent",
            "relative/folder",
        )
        .await
        .expect_err("relative root refused");
        assert!(error.to_string().contains("must be absolute"), "{error:#}");
    }

    #[test]
    fn a_failed_retarget_names_the_published_folder_and_the_retry() {
        let error = published_but_not_retargeted("/work/four", anyhow::anyhow!("write refused"));
        let shown = format!("{error:#}");
        assert!(
            shown.starts_with("/work/four was published, but the agent still uses its previous folder. Choose the same folder again to finish."),
            "{shown}"
        );
        assert!(shown.ends_with("write refused"), "{shown}");
    }

    #[tokio::test]
    async fn a_refused_retarget_publishes_nothing_and_a_retry_finishes_a_partial_selection(
    ) -> Result<()> {
        use gents::store::collection::Collection;
        use gents::store::config_client::read_desired_state_record_in_txn;

        let runtime = crate::test_fixtures::ServedRuntimeHome::start().await;
        let access = runtime.user_access();
        let node_did = runtime.node_did();
        let agent = runtime.provision_agent().await;
        let mut agent_value = runtime.agent_value(&agent, "profile");
        agent_value["context_id"] = json!("context");
        let context = |context_id: &str, tools_id: &str| json!({"node_did": node_did, "context_id": context_id, "tools_id": tools_id});
        runtime
            .apply_as_user(
                "test.workspace_root.seed",
                vec![
                    (
                        Collection::InferenceBackend,
                        json!({"node_did": node_did, "backend_id": "backend", "name": "Fixture",
                            "provider_kind": "OpenAiCompatible", "endpoint": "http://localhost:8000/v1",
                            "auth": {"kind": "unauthenticated"}}),
                    ),
                    (
                        Collection::InferenceProfile,
                        json!({"node_did": node_did, "profile_id": "profile",
                            "backend_id": "backend", "model_name": "model"}),
                    ),
                    (
                        Collection::Tools,
                        json!({"node_did": node_did, "tools_id": "shared-tools"}),
                    ),
                    (
                        Collection::Tools,
                        json!({"node_did": node_did, "tools_id": "other-tools"}),
                    ),
                    (Collection::AgentContext, context("context", "shared-tools")),
                    (
                        Collection::AgentContext,
                        context("other-context", "shared-tools"),
                    ),
                    (Collection::Agent, agent_value),
                ],
            )
            .await;
        let folder = tempfile::tempdir()?;
        let root_path = std::fs::canonicalize(folder.path())?
            .to_string_lossy()
            .into_owned();
        let published_roots = || async {
            let response = access
                .execute(&format!(
                    "{{ WorkspaceRoot(filter: {{root_path: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
                    escape_graphql_string(&root_path)
                ))
                .await?;
            anyhow::Ok(
                response["data"]["WorkspaceRoot"]
                    .as_array()
                    .map_or(0, Vec::len),
            )
        };
        let host_root = || {
            access.transact_readonly("test.workspace_root.tools", |txn| {
                Box::pin(async move {
                    let (_, tools) = read_desired_state_record_in_txn(
                        txn,
                        Collection::Tools,
                        node_did,
                        "shared-tools",
                    )
                    .await?
                    .context("shared Tools missing")?;
                    anyhow::Ok(tools["host"]["root"].clone())
                })
            })
        };

        let error = select_agent_workspace_root(&access, node_did, &agent.agent_did, &root_path)
            .await
            .expect_err("a Tools document shared by another Context is refused");
        assert!(format!("{error:#}").contains("2 Contexts"), "{error:#}");
        assert_eq!(published_roots().await?, 0, "a refusal publishes nothing");
        assert_eq!(host_root().await?, serde_json::Value::Null);

        // The state a failed retarget leaves: the root published, Tools stale.
        let partial = gents::tools::surface::set_workspace_root_with_readers(
            &access,
            node_did,
            &root_path,
            None,
            true,
            std::slice::from_ref(&agent.agent_did),
        )
        .await?;
        assert_eq!(published_roots().await?, 1);
        assert_eq!(host_root().await?, serde_json::Value::Null);

        runtime
            .apply_as_user(
                "test.workspace_root.unshare",
                vec![(
                    Collection::AgentContext,
                    context("other-context", "other-tools"),
                )],
            )
            .await;
        for _ in 0..2 {
            let selected =
                select_agent_workspace_root(&access, node_did, &agent.agent_did, &root_path)
                    .await?;
            assert_eq!(selected.workspace_root_doc_id, partial);
            assert_eq!(selected.tools_id, "shared-tools");
            assert_eq!(published_roots().await?, 1);
            assert_eq!(host_root().await?, json!(root_path));
        }
        runtime.shutdown().await;
        Ok(())
    }
}
