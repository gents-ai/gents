use crate::support::*;

use anyhow::{Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::ensure_runtime_schemas;
use serde_json::Value;
use uuid::Uuid;

#[tokio::test]
async fn mcp_register_upserts_a_host_endpoint() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let agent_home = tempdir.path().join("agent-home");
    let service_id = format!("registered-mcp-{}", Uuid::new_v4().simple());
    {
        let node = initialized_agent_node(tempdir.path(), &agent_home, "mcp-register-test").await?;
        ensure_runtime_schemas(&node).await?;
    }
    let agent_home_text = agent_home.to_str().context("agent home utf8")?;
    let output = run_cli_text(
        tempdir.path(),
        &[
            "mcp",
            "register",
            "--home",
            agent_home_text,
            "--endpoint",
            "http://127.0.0.1:9213/mcp",
            "--display-name",
            "Research Gateway",
            "--send-agent-did",
            &service_id,
        ],
    )?;
    assert!(output.contains(&service_id));

    let node = initialized_agent_node(tempdir.path(), &agent_home, "mcp-register-read").await?;
    let query = format!(
        r#"{{ ToolServiceRegistry(filter: {{ service_id: {{ _eq: "{service_id}" }} }}) {{
            service_id display_name hostname lan_ip mcp_port mcp_path send_agent_did status
        }} }}"#
    );
    let response = node.execute(&query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("ToolServiceRegistry"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .context("registered row")?;
    assert_eq!(row.get("lan_ip").and_then(Value::as_str), Some("127.0.0.1"));
    assert_eq!(row.get("mcp_port").and_then(Value::as_i64), Some(9213));
    assert_eq!(row.get("mcp_path").and_then(Value::as_str), Some("/mcp"));
    assert_eq!(
        row.get("send_agent_did").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(row.get("status").and_then(Value::as_str), Some("online"));
    Ok(())
}

#[tokio::test]
async fn mcp_probe_json_reports_health_snapshot_for_registry_service() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let agent_home = tempdir.path().join("agent-home");
    let service_id = format!("fixture-mcp-{}", Uuid::new_v4().simple());

    {
        let node = initialized_agent_node(tempdir.path(), &agent_home, "mcp-probe-test").await?;
        ensure_runtime_schemas(&node).await?;
        seed_mcp_service(&node, &service_id, "fixture-host", "", "", 0, "online").await?;
    }

    let agent_home = agent_home.to_str().context("agent home utf8")?;
    let output = run_cli_json(
        tempdir.path(),
        &[
            "mcp",
            "probe",
            "--home",
            agent_home,
            "--timeout",
            "1s",
            "--output",
            "json",
            &service_id,
        ],
    )?;

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

    let table = run_cli_text(
        tempdir.path(),
        &["mcp", "probe", "--home", agent_home, &service_id],
    )?;
    assert!(
        table.contains("SERVICE") && table.contains("HEALTH_STATE") && table.contains(&service_id),
        "text output should include default MCP probe columns:\n{table}"
    );

    Ok(())
}

#[tokio::test]
async fn mcp_probe_all_json_lists_each_online_service() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let agent_home = tempdir.path().join("agent-home");
    let missing_port_id = format!("a-fixture-mcp-{}", Uuid::new_v4().simple());
    let missing_address_id = format!("b-fixture-mcp-{}", Uuid::new_v4().simple());
    let offline_id = format!("z-fixture-mcp-{}", Uuid::new_v4().simple());

    {
        let node = initialized_agent_node(tempdir.path(), &agent_home, "mcp-probe-all").await?;
        ensure_runtime_schemas(&node).await?;
        seed_mcp_service(&node, &missing_port_id, "fixture-host", "", "", 0, "online").await?;
        seed_mcp_service(&node, &missing_address_id, "", "", "", 9201, "online").await?;
        seed_mcp_service(&node, &offline_id, "offline-host", "", "", 0, "offline").await?;
    }

    let agent_home = agent_home.to_str().context("agent home utf8")?;
    let output = run_cli_json(
        tempdir.path(),
        &[
            "mcp",
            "probe",
            "--home",
            agent_home,
            "--all",
            "--timeout",
            "1s",
            "--output",
            "json",
        ],
    )?;

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

    Ok(())
}

#[tokio::test]
async fn mcp_probe_single_missing_service_fails() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let agent_home = tempdir.path().join("agent-home");

    {
        let node = initialized_agent_node(tempdir.path(), &agent_home, "mcp-probe-missing").await?;
        ensure_runtime_schemas(&node).await?;
    }

    let agent_home = agent_home.to_str().context("agent home utf8")?;
    let stderr = run_cli_failure_stderr(
        tempdir.path(),
        &["mcp", "probe", "--home", agent_home, "missing-service"],
    )?;
    assert!(
        stderr.contains("no online MCP service matched missing-service"),
        "missing service failure should identify the service:\n{stderr}"
    );

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

async fn seed_mcp_service(
    node: &EmbeddedNode,
    service_id: &str,
    hostname: &str,
    tailscale_ip: &str,
    lan_ip: &str,
    mcp_port: u16,
    status: &str,
) -> Result<()> {
    let service_id = escape_graphql_string(service_id);
    let hostname = escape_graphql_string(hostname);
    let tailscale_ip = escape_graphql_string(tailscale_ip);
    let lan_ip = escape_graphql_string(lan_ip);
    let status = escape_graphql_string(status);
    let mutation = format!(
        r#"mutation {{
            create_ToolServiceRegistry(input: {{
                service_id: "{service_id}",
                display_name: "Fixture MCP",
                description: "MCP probe fixture",
                hostname: "{hostname}",
                tailscale_ip: "{tailscale_ip}",
                lan_ip: "{lan_ip}",
                mcp_port: {mcp_port},
                mcp_path: "/mcp",
                status: "{status}",
                version: "test",
                updated_at: "2026-05-20T12:00:00Z"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!("seed ToolServiceRegistry failed: {:?}", response.errors);
    }
    Ok(())
}
