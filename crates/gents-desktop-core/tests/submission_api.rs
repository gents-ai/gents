use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::store::config_client::ConfigAccess;
use gents_desktop_core::client::{
    ClientCore, ClientCoreOptions, DesktopPaths, SubmitRequestOptions,
};
use gents_desktop_core::test_fixtures::ServedRuntimeHome;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn submit_request_does_not_create_runtime_projections() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let (runtime, core, node_did, agent_did) = start_core_with_local_route(tempdir.path()).await?;

    let session_id = Uuid::new_v4().to_string();
    core.submit_request(&session_id, &node_did, "hello", Some(&agent_did))
        .await?;
    let response = runtime
        .user_access()
        .execute(&format!(
            r#"{{
                AgentSession(filter: {{
                    session_id: {{ _eq: "{}" }},
                    node_did: {{ _eq: "{}" }},
                    requester_did: {{ _eq: "{}" }}
                }}) {{ _docID }}
            }}"#,
            gents::store::graphql::escape_graphql_string(&session_id),
            gents::store::graphql::escape_graphql_string(&node_did),
            gents::store::graphql::escape_graphql_string(&node_did),
        ))
        .await?;
    let data = &response["data"];
    {
        let collection = "AgentSession";
        assert!(
            data.get(collection)
                .and_then(serde_json::Value::as_array)
                .is_some_and(Vec::is_empty),
            "client submission must not create {collection}"
        );
    }
    Arc::new(core).shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn submit_request_writes_request_as_the_only_durable_input() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let (runtime, core, node_did, agent_did) = start_core_with_local_route(tempdir.path()).await?;

    let session_id = Uuid::new_v4().to_string();
    let submitted = core
        .submit_request(
            &session_id,
            &node_did,
            "  hello   there\noperator  ",
            Some(&agent_did),
        )
        .await?;

    let request: AgentRequestRow = query_single(
        &runtime.user_access(),
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    request_id
                    node_did
                    requester_did
                    admission_signer_did
                    agent_did
                    session_id
                    content
                    lifecycle_state
                    backend_id
                    execution_origin
                    retry_root_request
                    retry_parent_request
                    retry_count
                    max_retries
                }}
            }}"#,
            submitted.request_id
        ),
        "AgentRequest",
    )
    .await?;
    assert_eq!(request.request_id, submitted.request_id);
    assert_eq!(request.node_did.as_deref(), Some(node_did.as_str()));
    assert_ne!(core.node_identity().did(), node_did);
    assert_eq!(
        request.requester_did.as_deref(),
        Some(core.node_identity().did())
    );
    assert_eq!(
        request.admission_signer_did.as_deref(),
        Some(core.node_identity().did())
    );
    assert_eq!(request.agent_did.as_deref(), Some(agent_did.as_str()));
    assert_eq!(request.session_id.as_deref(), Some(session_id.as_str()));
    assert_eq!(request.content.as_deref(), Some("hello   there\noperator"));
    assert_eq!(
        request.lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );
    assert!(request.backend_id.is_none());
    assert_eq!(request.execution_origin.as_deref(), Some("interactive"));
    assert_eq!(
        request.retry_root_request.as_deref(),
        Some(submitted.request_id.as_str())
    );
    assert!(request.retry_parent_request.is_none());
    assert_eq!(request.retry_count, Some(0));
    assert_eq!(request.max_retries, Some(3));

    assert_eq!(
        core.store().focused_request_id(),
        Some(submitted.request_id.clone())
    );
    assert_eq!(core.store().snapshot().requests.len(), 1);
    Arc::new(core).shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resend_preserves_request_lineage_without_caller_inference_overrides() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let (runtime, core, node_did, agent_did) = start_core_with_local_route(tempdir.path()).await?;

    let session_id = Uuid::new_v4().to_string();

    let options = SubmitRequestOptions::default();
    let original = core
        .submit_request_with_options(
            &session_id,
            &node_did,
            "please preserve this request",
            Some(&agent_did),
            options,
        )
        .await?;

    let force_stale = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{ lifecycle_state: "dead", failure_reason: "Stale" }}
            ) {{ _docID }}
        }}"#,
        request_id = original.request_id,
    );
    runtime
        .user_graphql_access()
        .write("test.submission_api.force_stale", &force_stale)
        .await
        .context("forcing stale state")?;
    core.refresh_store().await?;

    let resent = core.resend_request(&original.request_id).await?;

    let new_row: AgentRequestRow = query_single(
        &runtime.user_access(),
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    request_id
                    session_id
                    retry_parent_request
                    retry_root_request
                }}
            }}"#,
            resent.request_id
        ),
        "AgentRequest",
    )
    .await?;
    assert_eq!(new_row.request_id, resent.request_id);
    assert_eq!(
        new_row.session_id.as_deref(),
        Some(resent.session_id.as_str())
    );
    assert_ne!(
        new_row.session_id.as_deref(),
        Some(original.session_id.as_str())
    );
    assert_eq!(
        new_row.retry_parent_request.as_deref(),
        Some(original.request_id.as_str())
    );
    assert_eq!(
        new_row.retry_root_request.as_deref(),
        Some(original.request_id.as_str())
    );

    Arc::new(core).shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}

/// A desktop whose client key is the User of a served runtime, routed to it
/// as its co-hosted local runtime, with one published chat Agent.
async fn start_core_with_local_route(
    root: &Path,
) -> Result<(ServedRuntimeHome, ClientCore, String, String)> {
    let client = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(root.join("client")),
        ClientCoreOptions::local_simulated_route(),
    )
    .await?;
    let runtime =
        ServedRuntimeHome::start_unpublished_for(Arc::new(client.node_identity().clone())).await;
    runtime.publish_node(serde_json::json!({})).await;
    let agent = runtime.publish_chat_agent().await;
    runtime.connect(&client).await;
    let node_did = runtime.node_did().to_owned();
    Ok((runtime, client, node_did, agent.agent_did))
}

async fn query_single<T>(access: &ConfigAccess, query: &str, root: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let response = access
        .execute(query)
        .await
        .with_context(|| format!("query {root}"))?;
    let row = response["data"][root]
        .as_array()
        .and_then(|rows| rows.first())
        .cloned()
        .with_context(|| format!("missing row for {root}"))?;
    Ok(serde_json::from_value(row)?)
}
