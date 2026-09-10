use crate::support::*;

use std::{fs, time::Duration};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_set_persists_canonical_nested_policy() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("home");
    fs::create_dir_all(&home)?;
    let model = format!("tools-model-{}", Uuid::new_v4().simple());
    let endpoint = MockModelEndpoint::start(&model)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home,
        &[
            "--agent-name",
            "tools-agent",
            "--model-name",
            &model,
            "--inference-url",
            endpoint.endpoint(),
        ],
    )?;
    let owner = agent_did_from_init(&init)?;
    let mut server = spawn_server(&home, port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &owner, Duration::from_secs(30)).await?;

    let path = tempdir.path().join("tools.json");
    write_json_file(
        &path,
        &json!({
            "tools_id":"review-tools",
            "agent_did":owner,
            "host":{
                "files":{"mode":"ReadOnly"},
                "bash":{
                    "mode":"Unrestricted",
                    "execution_mode":"artifact_write",
                    "network_mode":"disabled",
                    "allowed_argv_prefixes":[["git","status"]],
                    "forbidden_argv_prefixes":[["git","commit"]]
                }
            },
            "built_ins":{"enable_goal_tools":true}
        }),
    )?;
    let output = run_cli_json(
        &home,
        &[
            "config",
            "tools",
            "set",
            "--graphql",
            &graphql,
            "--file",
            path.to_str().unwrap(),
        ],
    )?;
    assert_eq!(output["tools_id"], "review-tools");

    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{ Tools(filter: {{ agent_did: {{ _eq: "{}" }}, tools_id: {{ _eq: "review-tools" }} }}, limit: 1) {{ tools_id host built_ins }} }}"#,
            escape_graphql_string(&owner)
        ),
    )
    .await?;
    let row = first_graphql_row(&response, "Tools")?;
    assert_eq!(row["tools_id"], "review-tools");
    assert_eq!(
        nested_json(&row, "host")?["bash"]["execution_mode"],
        "artifact_write"
    );
    assert_eq!(nested_json(&row, "built_ins")?["enable_goal_tools"], true);
    drop(server);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_set_persists_host_root_and_export_round_trips_it() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    fs::create_dir_all(&home)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(&home, &["--agent-name", "rooted-tools-agent"])?;
    let owner = agent_did_from_init(&init)?;
    let mut server = spawn_server(&home, port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &owner, Duration::from_secs(30)).await?;
    let scoped_root = home.join("workspace");
    fs::create_dir_all(&scoped_root)?;
    let path = tempdir.path().join("tools.json");
    write_json_file(
        &path,
        &json!({
            "tools_id":"rooted-tools",
            "agent_did":owner,
            "host":{"root":scoped_root,"files":{"mode":"ReadOnly"}}
        }),
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
            path.to_str().unwrap(),
        ],
    )?;
    let export = tempdir.path().join("export");
    run_cli_text(
        &home,
        &["config", "export", "--root", export.to_str().unwrap()],
    )?;
    let config = read_json_file(&export.join("pack_config.json"))?;
    let rooted = config["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tools| tools["tools_id"] == "rooted-tools")
        .context("exported rooted Tools")?;
    assert_eq!(rooted["host"]["root"], scoped_root.to_str().unwrap());
    drop(server);
    Ok(())
}

fn nested_json(row: &Value, field: &str) -> Result<Value> {
    let value = row.get(field).cloned().unwrap_or(Value::Null);
    Ok(match value.as_str() {
        Some(encoded) => serde_json::from_str(encoded)?,
        None => value,
    })
}
