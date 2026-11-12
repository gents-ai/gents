//! Desktop folder selection for an agent's host tool root.
//!
//! The Node owner publishes the WorkspaceRoot and grants the agent reader
//! access, then the agent's Tools document names the folder in `host.root`.
//! Both writes are signed by the operator, so they run through the owners the
//! CLI uses. The caller restarts the runtime with the reviewed ceiling after
//! this returns: `gents serve` refuses a configured root that is not published.

use anyhow::{Context, Result};
use gents_documents::views::config::ContextsUsingTools;
use gents_documents::write_tools_document;
use gents_documents::{load_agent_context_and_tools_in_txn, Tools};
use gents_protocol::operations::workspace_root::{
    EnabledUpdate, WorkspaceRootError, WorkspaceRootPublish, WorkspaceRootVerb,
};
use gents_protocol::operations::NextCall;
use gents_protocol::operations::StringUpdate;
use gents_store::config_client::ConfigAccess;
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
/// points that agent's Tools `host.root` at it. Publication happens first so a
/// later runtime start admits the root.
pub async fn select_agent_workspace_root(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
    root_path: &str,
) -> Result<AgentWorkspaceRoot> {
    let published = gents_tools::surface::publish_workspace_root(
        access,
        &WorkspaceRootPublish {
            node_did: node_did.to_owned(),
            root_path: root_path.to_owned(),
            display_name: StringUpdate::Omitted,
            enabled: EnabledUpdate::Enable,
            reader_agents: vec![agent_did.to_owned()],
        },
    )
    .await
    .map_err(refusal)?;
    let root_path = published.root_path.as_str();
    let mut tools = read_unshared_agent_tools(access, node_did, agent_did).await?;
    let tools_id = tools.tools_id.clone();
    if retarget_host_root(&mut tools, root_path) {
        write_tools_document(
            access,
            &tools,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await?;
    }
    Ok(AgentWorkspaceRoot {
        root_path: root_path.to_owned(),
        workspace_root_doc_id: published.doc_id.clone(),
        tools_id,
    })
}

/// A refused publication as the message the UI shows: what happened, then
/// the next step in desktop terms for the call the operation names.
fn refusal(error: WorkspaceRootError) -> anyhow::Error {
    let next = match error.recovery(WorkspaceRootVerb::Publish) {
        NextCall::List { resource: "agent" } => "Choose an enabled agent of this node.",
        NextCall::List { .. } => "Choose a folder that no other node has published.",
        NextCall::Inspect { .. } => {
            "Ask the node's owner or an administrator to choose the folder."
        }
        NextCall::VerbHelp { .. }
        | NextCall::ResourceHelp { .. }
        | NextCall::Preview { .. }
        | NextCall::Verb { .. } => "Choose the folder by its full path.",
    };
    let message = format!(
        "{}. {next}",
        gents_protocol::error_chain::ErrorChain(&error)
            .to_string()
            .trim_end_matches('.')
    );
    anyhow::Error::new(error).context(message)
}

async fn read_unshared_agent_tools(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
) -> Result<Tools> {
    access
        .transact_write("workspace_root.agent_tools", |txn| {
            Box::pin(async move {
                let (context, tools) =
                    load_agent_context_and_tools_in_txn(txn, node_did, agent_did).await?;
                context.context("the agent has no Context document")?;
                let tools = tools.context("the agent has no Tools document")?;
                let selecting = txn
                    .read_view(&ContextsUsingTools {
                        node_did: node_did.to_owned(),
                        tools_id: tools.tools_id.clone(),
                    })
                    .await?
                    .len();
                ensure_unshared(&tools.tools_id, selecting)?;
                Ok::<Tools, anyhow::Error>(tools)
            })
        })
        .await
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
}
