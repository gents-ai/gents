use anyhow::Result;
use gents_protocol::operations::workspace_root::{
    AgentWorkspaceRoot, AgentWorkspaceRootError, WorkspaceRootError, WorkspaceRootVerb,
};
use gents_protocol::operations::NextCall;
use gents_store::config_client::ConfigAccess;

pub async fn select_agent_workspace_root(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
    root_path: &str,
) -> Result<AgentWorkspaceRoot> {
    gents_tools::surface::root_selection::select_agent_workspace_root(
        access, node_did, agent_did, root_path,
    )
    .await
    .map_err(selection_failure)
}

fn selection_failure(error: AgentWorkspaceRootError) -> anyhow::Error {
    match error {
        AgentWorkspaceRootError::Publication(error) => refusal(error),
        error @ AgentWorkspaceRootError::Retarget { .. } => {
            let message = gents_protocol::error_chain::ErrorChain(&error).to_string();
            let mut classified = crate::error::BridgeError::classify_transport_error(error.into());
            classified.message = message;
            classified.into()
        }
        error => error.into(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use gents_store::graphql::escape_graphql_string;
    use serde_json::json;

    #[test]
    fn a_failed_retarget_keeps_the_published_folder_message_and_permission_code() {
        let error = selection_failure(AgentWorkspaceRootError::Retarget {
            root_path: "/work/four".into(),
            source: Box::new(crate::error::BridgeError::new(
                crate::error::BridgeErrorCode::PermissionDenied,
                "access denied",
            )),
        });
        let classified = crate::error::BridgeError::classify_transport_error(error);
        assert_eq!(
            classified.code,
            crate::error::BridgeErrorCode::PermissionDenied
        );
        assert!(classified.message.starts_with("/work/four was published, but the agent still uses its previous folder. Choose the same folder again to finish."));
        assert!(classified.message.ends_with("access denied"));
    }

    #[tokio::test]
    async fn a_refused_retarget_publishes_nothing_and_a_retry_finishes_a_partial_selection(
    ) -> Result<()> {
        use gents_documents::read_desired_state_record_in_txn;
        use gents_store::collection::Collection;

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

        let partial = gents_tools::surface::set_workspace_root_with_readers(
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
