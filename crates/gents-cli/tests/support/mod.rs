#![allow(dead_code, unused_imports)]

#[path = "../../src/desired_state/document_handle.rs"]
mod document_handle;
pub mod fs;
pub(crate) use document_handle::document_handle;
pub mod graphql;
#[path = "../../../gents/tests/support/inference_target.rs"]
pub mod inference_target;
pub mod mocks;
pub mod ports;
pub mod process;
pub mod waits;

pub use fs::{
    assert_json_schema_valid, assert_manifest_node_dids, assert_runtime_init_state, copy_dir_all,
    manifest_contains, parse_jsonl, project_object_fields, read_captured_log, read_json_file,
    read_runtime_state_json, read_workspace_json, rewrite_manifest_node_dids, workspace_root,
    write_json_file, write_manifest_root_from_export,
};
pub use graphql::{
    doc_id_for_tools, doc_id_from_create, escape_graphql_string, exec, first_graphql_row,
    graphql_query,
};
pub use mocks::{
    completion_text_sse, request_contains_role_text, request_has_tool_result_message,
    request_system_message, request_tool_names, request_tool_result_text, tool_call_sse,
    tool_call_sse_with_id, MockChatEndpoint, MockModelEndpoint, MockOpenAIEndpoint,
};
pub use ports::{allocate_port, graphql_url};
pub use process::{
    cli_bin, desktop_bin, run_cli_failure_stderr, run_cli_failure_stderr_with_env,
    run_cli_failure_stdout_json, run_cli_json, run_cli_json_with_env, run_cli_text,
    run_desktop_init_json, run_init_json, spawn_cli, spawn_server, spawn_server_with_env,
    spawn_server_with_ready_json, spawn_server_with_ready_json_recovering, wait_for_port,
    ServeProcess,
};
pub use waits::{
    canonical_tool_result_text, wait_for_completed_inference_agents, wait_for_completed_tool_calls,
    wait_for_connected_peer, wait_for_request, wait_for_request_lifecycle_state,
    wait_for_runtime_quiescence, wait_for_runtime_ready, wait_for_runtime_state_graphql,
    wait_for_tool_call,
};

pub fn fixture_pack_dir(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gents/tests/fixtures/packs")
        .join(name)
}

pub const DEFAULT_MODEL_ENDPOINT: &str = "http://workstation-1:8000/v1";
pub const DEFAULT_MODEL_NAME: &str = "GLM-5.2";

pub async fn initialized_node(
    cwd: &std::path::Path,
    node_home: &std::path::Path,
    node_name: &str,
) -> anyhow::Result<gents::defra_node::Node> {
    use anyhow::Context as _;

    let home = node_home.to_str().context("node home utf8")?;
    let init = run_init_json(
        cwd,
        &["--identity-only", "--node-name", node_name, "--home", home],
    )?;
    let node_did = node_did_from_init(&init)?;
    let key_path = init
        .get("key_path")
        .and_then(serde_json::Value::as_str)
        .context("identity-only init output missing key_path")?;
    let _identity = gents::KeyIdentity::load_or_create(key_path, None)
        .context("loading initialized node identity")?;

    let data = node_home.join("data");
    gents::store::local::store_key::open_home_store_key(node_home, &data)
        .await?
        .encrypt(
            gents::defra_node::Node::builder()
                .data_path(&data)
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith),
        )
        .with_node_identity_did(&node_did)
        .build()
        .await
        .context("opening initialized embedded node")
}

pub fn node_did_from_init(init: &serde_json::Value) -> anyhow::Result<String> {
    let node_did = init
        .get("node_did")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("init output missing node_did: {init}"))?;
    anyhow::ensure!(
        !node_did.starts_with("did:test:"),
        "init returned a name-derived DID placeholder: {node_did}"
    );
    Ok(node_did.to_string())
}

/// The default Agent's inference profile `gents init` created.
pub fn inference_profile_id_from_init(init: &serde_json::Value) -> anyhow::Result<String> {
    init.pointer("/init/inference_profile_id")
        .or_else(|| init.get("inference_profile_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("init output missing inference_profile_id: {init}"))
}

pub fn agent_did_from_init(init: &serde_json::Value) -> anyhow::Result<String> {
    init.pointer("/init/default_agent_did")
        .and_then(serde_json::Value::as_str)
        .filter(|did| did.starts_with("did:key:"))
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("init output missing provisioned agent DID: {init}"))
}

pub fn identity_from_init(init: &serde_json::Value) -> anyhow::Result<gents::KeyIdentity> {
    use anyhow::Context as _;

    let key_path = init
        .get("key_path")
        .and_then(serde_json::Value::as_str)
        .context("init output missing key_path")?;
    gents::KeyIdentity::load_or_create(key_path, None).context("loading initialized node identity")
}

/// A GraphQL response from [`UserSeededHome`], shaped like the embedded
/// node's so fixtures read `data` and `errors` the same way.
pub struct SeedResponse {
    pub data: Option<serde_json::Value>,
    pub errors: Vec<String>,
}

impl SeedResponse {
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    fn from_result(result: anyhow::Result<serde_json::Value>) -> Self {
        match result {
            Ok(mut value) => Self {
                errors: value
                    .get("errors")
                    .and_then(serde_json::Value::as_array)
                    .map(|errors| errors.iter().map(ToString::to_string).collect())
                    .unwrap_or_default(),
                data: value.get_mut("data").map(serde_json::Value::take),
            },
            Err(error) => Self {
                data: None,
                errors: vec![format!("{error:#}")],
            },
        }
    }
}

/// An identity-only home whose protected store is open with a loopback
/// endpoint, so fixture rows are written and read as the CLI's User — the
/// actor every later offline CLI command in that `HOME` acts as.
pub struct UserSeededHome {
    node: std::sync::Arc<gents::defra_node::Node>,
    access: gents::ConfigAccess,
    _claim: gents::store::local::home::StoreLock,
    _runtime: gents::KeyIdentity,
}

impl UserSeededHome {
    pub async fn open(
        cwd: &std::path::Path,
        node_home: &std::path::Path,
        node_name: &str,
    ) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        let home = node_home.to_str().context("node home utf8")?;
        let init = run_init_json(
            cwd,
            &["--identity-only", "--node-name", node_name, "--home", home],
        )?;
        let node_did = node_did_from_init(&init)?;
        let runtime = gents::KeyIdentity::load_existing(
            init["key_path"]
                .as_str()
                .context("identity-only init output missing key_path")?,
            None,
        )
        .context("loading the home's Node key")?;
        let client = gents_desktop_core::client::PrincipalIdentity::load_existing(
            &graphql::cli_client_paths(cwd, &[]),
        )
        .context("loading the CLI User key")?;
        let data = gents::store::local::home::default_data_dir(node_home);
        let claim = gents::store::local::home::lock_store(node_home, &data)?;
        let key = gents::store::local::store_key::open_home_store_key(node_home, &data).await?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}/api/v0/graphql", listener.local_addr()?);
        let node = std::sync::Arc::new(
            gents::store::local::store_key::persistent_builder(&data, &key)?
                .with_node_identity_did(node_did)
                .with_node_acp_enabled()
                .with_http(gents::defra_node::HttpConfig::with_listener(listener)?)
                .build()
                .await
                .context("opening the identity-only home")?,
        );
        let access = gents::ConfigAccess::Graphql(
            gents::store::config_client::GraphqlEndpoint::as_principal(&url, client.did())
                .with_client_signing_capability(
                    gents::store::config_client::ClientSigningCapability::from_ed25519_key(
                        client.private_key_bytes(),
                    )?,
                )?,
        );
        Ok(Self {
            node,
            access,
            _claim: claim,
            _runtime: runtime,
        })
    }

    pub fn node(&self) -> &std::sync::Arc<gents::defra_node::Node> {
        &self.node
    }

    pub async fn execute(&self, query: &str) -> SeedResponse {
        let result = if query.trim_start().starts_with("mutation") {
            self.access.write("test.seed", query).await
        } else {
            self.access.execute(query).await
        };
        SeedResponse::from_result(result.map_err(Into::into))
    }

    pub async fn execute_with_variables(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> SeedResponse {
        SeedResponse::from_result(
            graphql::graphql_mutation_with_variables(&self.access, query, &variables).await,
        )
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.node.shutdown().await?;
        Ok(())
    }
}

/// Seeds a mutation as the home's User, failing on any GraphQL error.
pub async fn user_exec(home: &UserSeededHome, query: &str) -> anyhow::Result<()> {
    let response = home.execute(query).await;
    if response.has_errors() {
        anyhow::bail!("GraphQL mutation failed: {:?}", response.errors);
    }
    Ok(())
}
