use crate::client_runtime::{ClientRuntime, ClientRuntimeOptions};

pub fn file_journal_options() -> ClientRuntimeOptions {
    ClientRuntimeOptions {
        store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
        ..Default::default()
    }
}

pub async fn connect_runtime(
    runtime: &gents::client_test_support::ServedRuntimeHome,
    core: &ClientRuntime,
) {
    core.connect_runtime(gents_protocol::runtime_connection::RuntimeConnection {
        node_did: runtime.node_did().into(),
        label: "Served Runtime".into(),
        graphql: runtime.graphql().into(),
        local_node_home: Some(runtime.home_path().to_string_lossy().into_owned()),
        access_request: None,
        access_state: None,
    })
    .await
    .unwrap();
    assert_eq!(
        core.reconnect_runtime_now(runtime.node_did()).await,
        Some(gents_observe::client_projection::observe::RuntimeConnectionStatus::Connected)
    );
    core.refresh_node(runtime.node_did()).await.unwrap();
}
