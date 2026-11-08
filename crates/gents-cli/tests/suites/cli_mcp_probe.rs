use crate::support::*;

use anyhow::{Context, Result};
use gents::defra_node::Node as EmbeddedNode;
use gents::NodeIdentity;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn mcp_register_upserts_a_host_endpoint() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let node_home = tempdir.path().join("node-home");
    let service_id = format!("registered-mcp-{}", Uuid::new_v4().simple());
    let fixture = McpFixture::new(tempdir.path(), &node_home, "mcp-register-test").await?;
    let output = fixture
        .run(&[
            "mcp",
            "register",
            "--endpoint",
            "http://127.0.0.1:9213/mcp",
            "--display-name",
            "Research Gateway",
            "--send-node-did",
            &service_id,
        ])
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(&service_id));
    let query = format!(
        r#"{{ ToolServiceRegistry(filter: {{ service_id: {{ _eq: "{}" }} }}) {{
        service_id display_name hostname lan_ip mcp_port mcp_path send_node_did enabled
    }} }}"#,
        escape_graphql_string(&service_id)
    );
    let response = fixture.access.execute(&query).await?;
    let row = response
        .get("data")
        .and_then(|data| data.get("ToolServiceRegistry"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .context("registered row")?;
    assert_eq!(row.get("lan_ip").and_then(Value::as_str), Some("127.0.0.1"));
    assert_eq!(row.get("mcp_port").and_then(Value::as_i64), Some(9213));
    assert_eq!(row.get("mcp_path").and_then(Value::as_str), Some("/mcp"));
    assert_eq!(
        row.get("send_node_did").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(row.get("enabled").and_then(Value::as_bool), Some(true));
    fixture.node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn mcp_probe_json_reports_health_snapshot_for_registry_service() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let node_home = tempdir.path().join("node-home");
    let service_id = format!("fixture-mcp-{}", Uuid::new_v4().simple());

    let fixture = McpFixture::new(tempdir.path(), &node_home, "mcp-probe-test").await?;
    fixture
        .seed(&service_id, "fixture-host", "", "", 0, true)
        .await?;
    let result = fixture
        .run(&[
            "mcp",
            "probe",
            "--timeout",
            "1s",
            "--output",
            "json",
            &service_id,
        ])
        .await?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output: Value = serde_json::from_slice(&result.stdout)?;

    assert_eq!(output.get("count").and_then(Value::as_u64), Some(1));
    let items = output
        .get("items")
        .and_then(Value::as_array)
        .context("mcp probe output must include items")?;
    let row = items.first().context("expected one MCP probe row")?;
    assert_eq!(
        row.get("service").and_then(Value::as_str),
        Some(service_id.as_str())
    );
    assert_eq!(
        row.get("health_state").and_then(Value::as_str),
        Some("unreachable")
    );
    assert!(
        row.get("latency_ms").and_then(Value::as_u64).is_some(),
        "expected latency_ms in row: {row}"
    );
    assert!(
        row.get("last_error")
            .and_then(Value::as_str)
            .is_some_and(|error| error.contains("missing mcp_port")),
        "expected missing mcp_port error in row: {row}"
    );

    let table_output = fixture.run(&["mcp", "probe", &service_id]).await?;
    assert!(
        table_output.status.success(),
        "{}",
        String::from_utf8_lossy(&table_output.stderr)
    );
    let table = String::from_utf8(table_output.stdout)?;
    assert!(
        table.contains("SERVICE") && table.contains("HEALTH_STATE") && table.contains(&service_id),
        "text output should include default MCP probe columns:\n{table}"
    );

    fixture.node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn mcp_probe_all_json_lists_each_enabled_service() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let node_home = tempdir.path().join("node-home");
    let missing_port_id = format!("a-fixture-mcp-{}", Uuid::new_v4().simple());
    let missing_address_id = format!("b-fixture-mcp-{}", Uuid::new_v4().simple());
    let offline_id = format!("z-fixture-mcp-{}", Uuid::new_v4().simple());

    let fixture = McpFixture::new(tempdir.path(), &node_home, "mcp-probe-all").await?;
    fixture
        .seed(&missing_port_id, "fixture-host", "", "", 0, true)
        .await?;
    fixture
        .seed(&missing_address_id, "", "", "", 9201, true)
        .await?;
    fixture
        .seed(&offline_id, "offline-host", "", "", 0, false)
        .await?;
    let result = fixture
        .run(&[
            "mcp",
            "probe",
            "--all",
            "--timeout",
            "1s",
            "--output",
            "json",
        ])
        .await?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output: Value = serde_json::from_slice(&result.stdout)?;

    assert_eq!(output.get("count").and_then(Value::as_u64), Some(2));
    let items = output
        .get("items")
        .and_then(Value::as_array)
        .context("mcp probe --all output must include items")?;
    assert_eq!(
        service_ids(items),
        vec![missing_port_id.as_str(), missing_address_id.as_str()]
    );
    assert!(
        items
            .iter()
            .all(|row| row.get("health_state").and_then(Value::as_str) == Some("unreachable")),
        "all fixture rows should be unreachable: {items:?}"
    );
    assert_last_error_contains(&items[0], "missing mcp_port")?;
    assert_last_error_contains(&items[1], "missing address fields")?;

    fixture.node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn mcp_probe_single_missing_service_fails() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let node_home = tempdir.path().join("node-home");

    let fixture = McpFixture::new(tempdir.path(), &node_home, "mcp-probe-missing").await?;
    let output = fixture.run(&["mcp", "probe", "missing-service"]).await?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("no enabled MCP service matched missing-service"),
        "missing service failure should identify the service:\n{stderr}"
    );

    fixture.node.shutdown().await?;
    Ok(())
}

#[test]
fn mcp_probe_rejects_zero_timeout() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let stderr =
        run_cli_failure_stderr(tempdir.path(), &["mcp", "probe", "--all", "--timeout", "0"])?;
    assert!(
        stderr.contains("--timeout must be greater than zero"),
        "zero timeout failure should explain the constraint:\n{stderr}"
    );
    Ok(())
}

#[test]
fn mcp_register_rejects_unsupported_legacy_version() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let stderr = run_cli_failure_stderr(
        tempdir.path(),
        &[
            "mcp",
            "register",
            "service",
            "--endpoint",
            "http://127.0.0.1:9213/mcp",
            "--version",
            "v1",
        ],
    )?;
    assert!(stderr.contains("--version is not supported by the MCP service configuration"));
    Ok(())
}

#[tokio::test]
async fn mcp_register_rejects_an_unavailable_explicit_node() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let node_home = tempdir.path().join("node-home");
    let foreign = gents::KeyIdentity::load_or_create(tempdir.path().join("foreign.key"), None)?;
    let fixture = McpFixture::new(tempdir.path(), &node_home, "mcp-scope").await?;
    let output = fixture
        .run_for_node(
            &[
                "mcp",
                "register",
                "--endpoint",
                "http://127.0.0.1:9213/mcp",
                "foreign-service",
            ],
            foreign.did(),
        )
        .await?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("Node"), "unexpected failure: {stderr}");
    let rows = fixture
        .access
        .execute("{ ToolServiceRegistry(filter:{service_id:{_eq:\"foreign-service\"}}) {_docID} }")
        .await?;
    assert!(rows["data"]["ToolServiceRegistry"]
        .as_array()
        .unwrap()
        .is_empty());
    fixture.node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn mcp_registration_requires_client_signed_transport_when_offline() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let node_home = tempdir.path().join("node-home");
    let fixture = McpFixture::new(tempdir.path(), &node_home, "offline-mcp").await?;
    let desktop = fixture.desktop_home.clone();
    fixture.node.shutdown().await?;
    drop(fixture);
    let cwd = tempdir.path().to_owned();
    let stderr = tokio::task::spawn_blocking(move || {
        let home = node_home.to_str().context("node home utf8")?;
        let desktop = desktop.to_str().context("desktop home utf8")?;
        run_cli_failure_stderr_with_env(
            &cwd,
            &[
                "mcp",
                "register",
                "--home",
                home,
                "--endpoint",
                "http://127.0.0.1:9213/mcp",
                "offline-service",
            ],
            &[("GENTS_DESKTOP_HOME", desktop)],
        )
    })
    .await??;
    assert!(
        stderr.contains("requires a running runtime or --graphql for client-signed publication"),
        "{stderr}"
    );
    Ok(())
}

fn service_ids(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|row| row.get("service").and_then(Value::as_str))
        .collect()
}

fn assert_last_error_contains(row: &Value, needle: &str) -> Result<()> {
    let last_error = row
        .get("last_error")
        .and_then(Value::as_str)
        .context("probe row missing last_error")?;
    assert!(
        last_error.contains(needle),
        "expected last_error to contain {needle:?}, got {last_error:?}"
    );
    Ok(())
}

struct McpFixture {
    node: Arc<EmbeddedNode>,
    access: gents::ConfigAccess,
    node_did: String,
    graphql: String,
    cwd: std::path::PathBuf,
    desktop_home: std::path::PathBuf,
    _store_claim: gents::store::local::home::StoreLock,
    _runtime: gents::KeyIdentity,
    _client: gents_desktop_core::client::PrincipalIdentity,
}

impl McpFixture {
    async fn new(cwd: &std::path::Path, home: &std::path::Path, name: &str) -> Result<Self> {
        let cwd = cwd.to_owned();
        let home = home.to_owned();
        let desktop_home = cwd.join("desktop");
        let init_cwd = cwd.clone();
        let init_home = home.clone();
        let init_desktop = desktop_home.clone();
        let name = name.to_owned();
        let init = tokio::task::spawn_blocking(move || {
            let home = init_home.to_str().context("home utf8")?;
            let desktop = init_desktop.to_str().context("desktop home utf8")?;
            run_cli_json_with_env(
                &init_cwd,
                &[
                    "init",
                    "--store-key-custody",
                    "file",
                    "--home",
                    home,
                    "--node-name",
                    &name,
                ],
                &[("GENTS_DESKTOP_HOME", desktop)],
            )
        })
        .await??;
        let node_did = node_did_from_init(&init)?;
        let runtime = identity_from_init(&init)?;
        let client = gents_desktop_core::client::PrincipalIdentity::load_existing(
            &gents_desktop_core::client::DesktopPaths::from_root(desktop_home.join("cli")),
        )?;
        let data = gents::store::local::home::default_data_dir(&home);
        let claim = gents::store::local::home::lock_store(&home, &data)?;
        let store_key = gents::store::local::store_key::open_home_store_key(&home, &data).await?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let graphql = format!("http://{}/api/v0/graphql", listener.local_addr()?);
        let node = Arc::new(
            gents::store::local::store_key::persistent_builder(&data, &store_key)?
                .with_node_identity_did(node_did.clone())
                .with_node_acp_enabled()
                .with_http(gents::defra_node::HttpConfig::with_listener(listener)?)
                .build()
                .await?,
        );
        let capability = gents::store::config_client::ClientSigningCapability::from_ed25519_key(
            client.private_key_bytes(),
        )?;
        let access = gents::ConfigAccess::Graphql(
            gents::store::config_client::GraphqlEndpoint::as_principal(&graphql, client.did())
                .with_client_signing_capability(capability)?
                .with_client_signed_agent_creates(),
        );
        Ok(Self {
            node,
            access,
            node_did,
            graphql,
            cwd,
            desktop_home,
            _store_claim: claim,
            _runtime: runtime,
            _client: client,
        })
    }
    async fn run(&self, args: &[&str]) -> Result<std::process::Output> {
        self.run_for_node(args, &self.node_did).await
    }
    async fn run_for_node(&self, args: &[&str], node_did: &str) -> Result<std::process::Output> {
        let mut args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        args.extend([
            "--graphql".to_owned(),
            self.graphql.clone(),
            "--node-did".to_owned(),
            node_did.to_owned(),
        ]);
        let cwd = self.cwd.clone();
        let desktop = self.desktop_home.clone();
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(cli_bin())
                .env("HOME", &cwd)
                .env("GENTS_DESKTOP_HOME", desktop)
                .env("RUST_LOG", "error")
                .args(args)
                .output()
                .context("running MCP CLI")
        })
        .await?
    }
    async fn seed(
        &self,
        service_id: &str,
        hostname: &str,
        tailscale_ip: &str,
        lan_ip: &str,
        mcp_port: u16,
        enabled: bool,
    ) -> Result<()> {
        use gents::store::config_client::{
            apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        let value = serde_json::json!({"node_did":self.node_did,"service_id":service_id,"display_name":"Fixture MCP","description":"MCP probe fixture","hostname":hostname,"tailscale_ip":tailscale_ip,"lan_ip":lan_ip,"mcp_port":mcp_port,"mcp_path":"/mcp","enabled":enabled});
        if mcp_port == 0 || [hostname, tailscale_ip, lan_ip].iter().all(|address| address.is_empty()) {
            return self.seed_unvalidated(&value).await;
        }
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: gents::Collection::ToolServiceRegistry,
            add: value.clone(),
            update: value,
        }])?;
        self.access
            .transact_write("test.mcp.service", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await?;
        Ok(())
    }

    /// Desired-state writes refuse a registry entry without a port or an
    /// address, but the probe must still report one that reached the store
    /// another way, so these rows are created directly as the User.
    async fn seed_unvalidated(&self, value: &Value) -> Result<()> {
        let capability = gents::store::config_client::ClientSigningCapability::from_ed25519_key(
            self._client.private_key_bytes(),
        )?;
        let access = gents::ConfigAccess::Graphql(
            gents::store::config_client::GraphqlEndpoint::as_principal(
                &self.graphql,
                self._client.did(),
            )
            .with_client_signing_capability(capability)?,
        );
        let response = crate::support::graphql::graphql_mutation_with_variables(
            &access,
            "mutation($input: ToolServiceRegistryMutationInputArg!) { create_ToolServiceRegistry(input: $input) { _docID } }",
            &serde_json::json!({"input": value}),
        )
        .await?;
        gents::store::graphql::created_doc_id(&response, "ToolServiceRegistry")?;
        Ok(())
    }
}

#[test]
fn mcp_probe_rejects_service_and_all_together() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let stderr = run_cli_failure_stderr(
        tempdir.path(),
        &["mcp", "probe", "--all", "some-service", "--timeout", "1s"],
    )?;
    assert!(
        stderr.contains("provide either <service> or --all, not both"),
        "service + --all failure should name the mutual exclusion:\n{stderr}"
    );
    Ok(())
}

#[test]
fn mcp_probe_rejects_missing_target() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let stderr = run_cli_failure_stderr(tempdir.path(), &["mcp", "probe"])?;
    assert!(
        stderr.contains("provide a service id or --all"),
        "missing-target failure should ask for a service id or --all:\n{stderr}"
    );
    Ok(())
}
