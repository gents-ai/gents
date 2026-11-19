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
    core.refresh_node(runtime.node_did()).await.unwrap();
    if let Some(status) = core.reconnect_runtime_now(runtime.node_did()).await {
        assert_eq!(
            status,
            gents_observe::client_projection::observe::RuntimeConnectionStatus::Connected
        );
    }
}

#[cfg(test)]
pub async fn runtime_metadata(server: &wiremock::MockServer, node: &str) {
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/status"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "node_did": node,
            (gents_protocol::client_authentication::STATUS_FIELD): gents_protocol::client_authentication::ClientAuthentication::Local,
        })))
        .mount(server).await;
}
