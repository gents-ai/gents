use std::sync::Arc;

use anyhow::Result;
use gents_desktop_bridge::client_runtime::ClientRuntime;
use gents_desktop_bridge::client_test_support::file_journal_options;
use gents_protocol::runtime_connection::RuntimeConnection;
use gents_store::client_journal::paths::DesktopPaths;

#[tokio::test]
async fn runtime_directory_and_client_identity_survive_reopen_without_a_peer() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(directory.path());
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(paths.clone(), file_journal_options()).await?,
    );
    let identity = core.node_identity().did().to_owned();
    let mut updates = core.runtime_connection_updates();
    core.connect_runtime(RuntimeConnection {
        node_did: "did:key:remote-runtime".into(),
        label: "Remote runtime".into(),
        graphql: "http://127.0.0.1:1/api/v0/graphql".into(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    })
    .await?;
    updates.changed().await?;
    let saved = updates.borrow_and_update().clone();
    core.shutdown().await?;
    drop(core);
    let core =
        Arc::new(ClientRuntime::start_with_paths_and_options(paths, file_journal_options()).await?);
    assert_eq!(core.node_identity().did(), identity);
    assert_eq!(core.runtime_connections(), saved);
    assert!(core
        .node()
        .get_all_collection_versions()
        .await?
        .iter()
        .all(|collection| {
            collection.policy.as_ref().is_some_and(|policy| {
                policy.resource_name == gents_protocol::schemas::CLIENT_JOURNAL_POLICY_RESOURCE
            })
        }));
    core.shutdown().await?;
    Ok(())
}
