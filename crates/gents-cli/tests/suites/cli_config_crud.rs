use crate::support::*;

use std::{fs, time::Duration};

use anyhow::{Context, Result};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_tools_list_show_and_remove_canonical_document() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("home");
    fs::create_dir_all(&home)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(&home, &["--agent-name", "crud-agent"])?;
    let owner = agent_did_from_init(&init)?;
    let mut server = spawn_server(&home, port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &owner, Duration::from_secs(30)).await?;

    let file = tempdir.path().join("tools.json");
    write_json_file(
        &file,
        &json!({"agent_did":owner,"tools_id":"extra-tools","built_ins":{"enable_goal_tools":true}}),
    )?;
    run_cli_json(
        &home,
        &[
            "config",
            "tools",
            "set",
            "--graphql",
            &graphql,
            "--file",
            file.to_str().unwrap(),
        ],
    )?;
    assert_list_show_rm(&home, &graphql, "tools", "Tools", "tools_id", "extra-tools").await?;
    drop(server);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn behavior_set_rejects_missing_context() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    fs::create_dir_all(&home)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(&home, &["--agent-name", "behavior-crud-agent"])?;
    let owner = agent_did_from_init(&init)?;
    let profile = init["init"]["inference_profile_id"]
        .as_str()
        .context("profile")?;
    let mut server = spawn_server(&home, port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &owner, Duration::from_secs(30)).await?;

    let error = run_cli_failure_stderr(
        &home,
        &[
            "config",
            "behavior",
            "set",
            "--graphql",
            &graphql,
            "--agent-did",
            &owner,
            "--behavior-id",
            "broken",
            "--context-id",
            "missing-context",
            "--inference-profile-id",
            profile,
        ],
    )?;
    assert!(
        error.contains("field context_id references missing AgentContext \"missing-context\""),
        "{error}"
    );
    drop(server);
    Ok(())
}

async fn assert_list_show_rm(
    home: &std::path::Path,
    graphql: &str,
    command: &str,
    collection: &str,
    unique_field: &str,
    id: &str,
) -> Result<()> {
    let list = run_cli_json(
        home,
        &[
            "config",
            command,
            "list",
            "--graphql",
            graphql,
            "--output",
            "json",
        ],
    )?;
    assert!(list["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row[unique_field] == id));
    let shown = run_cli_json(home, &["config", command, "show", "--graphql", graphql, id])?;
    assert_eq!(shown[unique_field], id);
    assert_eq!(
        run_cli_json(home, &["config", command, "rm", "--graphql", graphql, id])?["deleted"],
        1
    );
    let response = graphql_query(
        graphql,
        &format!(r#"{{ {collection}(filter: {{ {unique_field}: {{ _eq: "{}" }} }}) {{ {unique_field} }} }}"#, escape_graphql_string(id)),
    ).await?;
    assert!(response["data"][collection]
        .as_array()
        .is_some_and(Vec::is_empty));
    Ok(())
}
