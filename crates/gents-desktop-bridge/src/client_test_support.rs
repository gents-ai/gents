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
pub async fn runtime_key(name: &str) -> RuntimeKey {
    let name = name.to_owned();
    let key = tokio::task::spawn_blocking(move || gents_store::test_support::test_identity(&name))
        .await
        .unwrap();
    let did = gents_store::identity::NodeIdentity::did(&key).to_owned();
    RuntimeKey {
        did,
        sign: std::sync::Arc::new(move |payload| key.sign_now(payload).unwrap()),
    }
}

#[cfg(test)]
type SignNow = std::sync::Arc<dyn Fn(&[u8]) -> Vec<u8> + Send + Sync>;

#[cfg(test)]
#[derive(Clone)]
pub struct RuntimeKey {
    pub did: String,
    pub sign: SignNow,
}

#[cfg(test)]
impl RuntimeKey {
    /// The key held in the file at `path`, which signs without being
    /// registered with DefraDB.
    pub fn of_file(path: &std::path::Path) -> Self {
        use identity::{FullIdentity as _, Identity as _};
        let key = gents_store::identity::load_file_identity(path).unwrap();
        Self {
            did: key.did().unwrap().to_string(),
            sign: std::sync::Arc::new(move |payload| key.sign(payload).unwrap()),
        }
    }

    pub fn of_principal(
        principal: &gents_store::client_journal::principal_identity::PrincipalIdentity,
    ) -> Self {
        let principal = principal.clone();
        Self {
            did: principal.did().to_owned(),
            sign: std::sync::Arc::new(move |payload| principal.sign(payload).unwrap()),
        }
    }
}

#[cfg(test)]
pub async fn runtime_metadata(server: &wiremock::MockServer, node: &RuntimeKey) {
    let node = node.clone();
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/status"))
        .respond_with(move |request: &wiremock::Request| {
            wiremock::ResponseTemplate::new(200).set_body_json(
                gents_store::test_support::local_home_status_now(
                    &node.did,
                    node.sign.as_ref(),
                    request.url.query(),
                ),
            )
        })
        .mount(server)
        .await;
}
