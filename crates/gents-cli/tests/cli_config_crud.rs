mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_core_documents_list_show_and_rm() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-config-crud-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-config-crud-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_backend_id = init
        .pointer("/init/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_id: {init}"))?
        .to_string();
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let referenced_delete = run_cli_failure_stderr(
        &home_dir,
        &[
            "config",
            "backend",
            "rm",
            "--graphql",
            &graphql,
            &default_backend_id,
        ],
    )?;
    assert!(
        referenced_delete.contains("still referenced"),
        "{referenced_delete}"
    );

    let backend_id = format!("{agent_name}-extra-backend");
    run_cli_json(
        &home_dir,
        &[
            "config",
            "backend",
            "set",
            "--graphql",
            &graphql,
            "--backend-id",
            &backend_id,
            "--name",
            "Extra Backend",
            "--provider-kind",
            "OpenAiCompatible",
            "--endpoint",
            mock_endpoint.endpoint(),
            "--max-concurrent",
            "1",
        ],
    )?;
    assert_list_show_rm(
        &home_dir,
        &graphql,
        &["config", "backend"],
        "InferenceBackend",
        "backend_id",
        &backend_id,
    )
    .await?;

    let behavior_id = format!("{agent_name}-extra-behavior");
    run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "set",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--behavior-id",
            &behavior_id,
            "--display-name",
            "Extra Behavior",
            "--backend-id",
            &default_backend_id,
        ],
    )?;
    assert_list_show_rm(
        &home_dir,
        &graphql,
        &["config", "behavior"],
        "AgentBehavior",
        "behavior_id",
        &behavior_id,
    )
    .await?;

    let selection_id = format!("{agent_name}-extra-tools");
    run_cli_json(
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
            "false",
            "--enable-bash",
            "false",
        ],
    )?;
    assert_list_show_rm(
        &home_dir,
        &graphql,
        &["config", "tools"],
        "ToolSelection",
        "selection_id",
        &selection_id,
    )
    .await?;

    let profile_id = format!("{agent_name}-extra-profile");
    run_cli_json(
        &home_dir,
        &[
            "config",
            "profile",
            "set",
            "--graphql",
            &graphql,
            "--profile-id",
            &profile_id,
            "--display-name",
            "Extra Profile",
            "--context-window",
            "4096",
        ],
    )?;
    assert_list_show_rm(
        &home_dir,
        &graphql,
        &["config", "profile"],
        "InferenceProfile",
        "profile_id",
        &profile_id,
    )
    .await?;

    Ok(())
}

async fn assert_list_show_rm(
    home_dir: &std::path::Path,
    graphql: &str,
    command_prefix: &[&str],
    collection: &str,
    unique_field: &str,
    id: &str,
) -> Result<()> {
    let mut list_args = command_prefix.to_vec();
    list_args.extend(["list", "--graphql", graphql, "--output", "json"]);
    let list = run_cli_json(home_dir, &list_args)?;
    let contains_id = list
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .any(|row| row.get(unique_field).and_then(Value::as_str) == Some(id))
        })
        .unwrap_or(false);
    assert!(
        contains_id,
        "{collection} list did not contain {id}: {list}"
    );

    let mut show_args = command_prefix.to_vec();
    show_args.extend(["show", "--graphql", graphql, id]);
    let show = run_cli_json(home_dir, &show_args)?;
    assert_eq!(show.get(unique_field).and_then(Value::as_str), Some(id));

    let mut rm_args = command_prefix.to_vec();
    rm_args.extend(["rm", "--graphql", graphql, id]);
    let deleted = run_cli_json(home_dir, &rm_args)?;
    assert_eq!(deleted.get("deleted").and_then(Value::as_u64), Some(1));

    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                {collection}(filter: {{ {unique_field}: {{ _eq: "{}" }} }}, limit: 1) {{
                    {unique_field}
                }}
            }}"#,
            escape_graphql_string(id)
        ),
    )
    .await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(rows.is_empty(), "{collection} {id} still exists: {rows:?}");

    let mut second_rm_args = command_prefix.to_vec();
    second_rm_args.extend(["rm", "--graphql", graphql, id]);
    let second_rm = run_cli_failure_stderr(home_dir, &second_rm_args)?;
    assert!(second_rm.contains("not found"), "{second_rm}");

    Ok(())
}
