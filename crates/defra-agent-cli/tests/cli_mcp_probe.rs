mod support;
use support::*;

use anyhow::{Context, Result};
use defra_agent::defra_node::{EmbeddedNode, StorageBackend};
use defra_agent::ensure_runtime_schemas;
use serde_json::Value;
use uuid::Uuid;

// Feature matrix tag: mcp-health / operatorCli.
#[tokio::test]
async fn mcp_probe_json_reports_health_snapshot_for_registry_service() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let agent_home = tempdir.path().join("agent-home");
    let data_dir = agent_home.join("data");
    let service_id = format!("fixture-mcp-{}", Uuid::new_v4().simple());

    {
        let node = EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_storage_backend(StorageBackend::RocksDb)
            .build()
            .await
            .context("opening embedded node")?;
        ensure_runtime_schemas(&node).await?;
        seed_unreachable_mcp_service(&node, &service_id).await?;
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

async fn seed_unreachable_mcp_service(node: &EmbeddedNode, service_id: &str) -> Result<()> {
    let service_id = escape_graphql_string(service_id);
    let mutation = format!(
        r#"mutation {{
            create_ToolServiceRegistry(input: {{
                service_id: "{service_id}",
                display_name: "Fixture MCP",
                description: "MCP probe fixture",
                hostname: "fixture-host",
                tailscale_ip: "",
                lan_ip: "",
                mcp_port: 0,
                mcp_path: "/mcp",
                status: "online",
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
