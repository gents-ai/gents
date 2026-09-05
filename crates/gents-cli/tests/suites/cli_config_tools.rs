use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_selection_upsert_defaults_enabled_modes_and_persists_command_policy() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-config-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-config-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let selection_id = format!("{agent_name}-tools");

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "tools",
            "set",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--selection-id",
            &selection_id,
            "--enable-file-tools",
            "--enable-bash",
            "--allowed-mcp-service-id",
            "x-data",
        ],
    )?;
    let doc_id = output
        .get("doc_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("tool-selection output missing doc_id: {output}"))?;
    assert_eq!(
        output.get("file_tools_mode").and_then(Value::as_str),
        Some("ReadOnly")
    );
    assert_eq!(
        output.get("bash_mode").and_then(Value::as_str),
        Some("ReadOnly")
    );
    assert_eq!(
        output
            .pointer("/allowed_mcp_service_ids/0")
            .and_then(Value::as_str),
        Some("x-data")
    );

    let query = format!(
        r#"{{
            ToolSelection(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                selection_id
                enable_file_tools
                file_tools_mode
                file_tool_root
                enable_bash
                bash_mode
                allowed_mcp_service_ids
            }}
        }}"#,
        escape_graphql_string(doc_id),
    );
    let response = graphql_query(&graphql, &query).await?;
    let row = first_graphql_row(&response, "ToolSelection")?;
    assert_eq!(
        row.get("selection_id").and_then(Value::as_str),
        Some(selection_id.as_str())
    );
    assert_eq!(
        row.get("enable_file_tools").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        row.get("file_tools_mode").and_then(Value::as_str),
        Some("ReadOnly")
    );
    assert_eq!(row.get("file_tool_root"), Some(&Value::Null));
    assert_eq!(row.get("enable_bash").and_then(Value::as_bool), Some(true));
    assert_eq!(
        row.get("bash_mode").and_then(Value::as_str),
        Some("ReadOnly")
    );
    assert_eq!(
        row.pointer("/allowed_mcp_service_ids/0")
            .and_then(Value::as_str),
        Some("x-data")
    );

    for (execution_policy, network_mode) in
        [("unrestricted", "enabled"), ("artifact_write", "disabled")]
    {
        let output = run_cli_json(
            &home_dir,
            &[
                "config",
                "tools",
                "set",
                "--graphql",
                &graphql,
                "--agent-did",
                &agent_did,
                "--selection-id",
                &selection_id,
                "--enable-bash",
                "--bash-mode",
                "Unrestricted",
                "--command-execution-policy",
                execution_policy,
                "--command-network-mode",
                network_mode,
                "--command-allowed-argv-prefix",
                "ps",
                "--command-forbidden-argv-prefix",
                "rm -rf",
            ],
        )?;
        assert_eq!(
            output
                .get("command_execution_policy")
                .and_then(Value::as_str),
            Some(execution_policy)
        );
        assert_eq!(
            output.get("command_network_mode").and_then(Value::as_str),
            Some(network_mode)
        );

        let query = format!(
            r#"{{
                ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    bash_mode
                    command_execution_policy
                    command_network_mode
                    command_allowed_argv_prefixes
                    command_forbidden_argv_prefixes
                }}
            }}"#,
            escape_graphql_string(&selection_id),
        );
        let response = graphql_query(&graphql, &query).await?;
        let row = first_graphql_row(&response, "ToolSelection")?;
        assert_eq!(
            row.get("bash_mode").and_then(Value::as_str),
            Some("Unrestricted")
        );
        assert_eq!(
            row.get("command_execution_policy").and_then(Value::as_str),
            Some(execution_policy)
        );
        assert_eq!(
            row.get("command_network_mode").and_then(Value::as_str),
            Some(network_mode)
        );
        assert_eq!(
            row.pointer("/command_allowed_argv_prefixes/0")
                .and_then(Value::as_str),
            Some("ps")
        );
        assert_eq!(
            row.pointer("/command_forbidden_argv_prefixes/0")
                .and_then(Value::as_str),
            Some("rm -rf")
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_selection_upsert_persists_file_tool_root() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-config-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-config-rooted-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let scoped_root = home_dir.join("bench").join("workspace");

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let selection_id = init
        .pointer("/init/tool_selection_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing tool_selection_id: {init}"))?
        .to_string();
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "tools",
            "set",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--selection-id",
            &selection_id,
            "--enable-file-tools",
            "--file-tool-root",
            scoped_root.to_str().expect("utf-8 scoped root"),
        ],
    )?;
    assert_eq!(
        output.get("file_tool_root").and_then(Value::as_str),
        Some(scoped_root.to_str().expect("utf-8 scoped root"))
    );

    let query = format!(
        r#"{{
            ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}, limit: 1) {{
                selection_id
                file_tool_root
            }}
        }}"#,
        escape_graphql_string(&selection_id),
    );
    let response = graphql_query(&graphql, &query).await?;
    let row = first_graphql_row(&response, "ToolSelection")?;
    assert_eq!(
        row.get("file_tool_root").and_then(Value::as_str),
        Some(scoped_root.to_str().expect("utf-8 scoped root"))
    );

    let export_root = tempdir.path().join("export");
    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            export_root.to_str().expect("utf-8 export root"),
        ],
    )?;
    let selection_doc = read_json_file(
        &export_root
            .join("tool_selections")
            .join(selection_id.replace('-', "_"))
            .join("object.json"),
    )?;
    assert_eq!(
        selection_doc.get("file_tool_root").and_then(Value::as_str),
        Some(scoped_root.to_str().expect("utf-8 scoped root"))
    );

    Ok(())
}
