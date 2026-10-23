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
            "--node-name",
            "tools-agent",
            "--model-name",
            &model,
            "--inference-url",
            endpoint.endpoint(),
        ],
    )?;
    let owner = node_did_from_init(&init)?;
    let mut server = spawn_server(&home, port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &owner, Duration::from_secs(30)).await?;

    let path = tempdir.path().join("tools.json");
    write_json_file(
        &path,
        &json!({
            "tools_id":"review-tools",
            "node_did":owner,
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
            r#"{{ Tools(filter: {{ node_did: {{ _eq: "{}" }}, tools_id: {{ _eq: "review-tools" }} }}, limit: 1) {{ tools_id host built_ins }} }}"#,
            escape_graphql_string(&owner)
        ),
    )
    .await?;
    let row = first_graphql_row(&response, "Tools")?;
    assert_eq!(row["tools_id"], "review-tools");
    assert_eq!(
        nested_json(row, "host")?["bash"]["execution_mode"],
        "artifact_write"
    );
    assert_eq!(nested_json(row, "built_ins")?["enable_goal_tools"], true);
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
    let init = run_init_json(&home, &["--node-name", "rooted-tools-agent"])?;
    let owner = node_did_from_init(&init)?;
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
            "node_did":owner,
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
    server.child.kill()?;
    server.child.wait()?;
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
    Ok(())
}

fn nested_json(row: &Value, field: &str) -> Result<Value> {
    let value = row.get(field).cloned().unwrap_or(Value::Null);
    Ok(match value.as_str() {
        Some(encoded) => serde_json::from_str(encoded)?,
        None => value,
    })
}

fn explained_tool_names(explanation: &Value, agent_id: &str) -> Result<Vec<String>> {
    explanation["agents"]
        .as_array()
        .context("explanation has no agents")?
        .iter()
        .find(|agent| agent["agent_id"] == agent_id)
        .and_then(|agent| agent.pointer("/surface/tool_names"))
        .and_then(Value::as_array)
        .with_context(|| format!("{agent_id} has no explained surface: {explanation}"))
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_explain_reports_the_ceiling_the_running_node_enforces() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let elsewhere = tempdir.path().join("elsewhere");
    let workspace = home.join("workspace");
    for dir in [&home, &elsewhere, &workspace] {
        fs::create_dir_all(dir)?;
    }
    let init = run_init_json(
        &home,
        &[
            "--node-name",
            "explain-live-agent",
            "--tool-package",
            "write",
            "--tool-root",
            workspace.to_str().context("utf-8 tool root")?,
        ],
    )?;
    let owner = node_did_from_init(&init)?;
    let agent_id = gents::default_agent_id_for_node(&owner);
    let tools_path = tempdir.path().join("tools.json");
    write_json_file(
        &tools_path,
        &json!({
            "tools_id": format!("{agent_id}-tools"),
            "node_did": owner,
            "host": {"files": {"mode": "ReadWrite"}, "cli": [{"name": "echo"}]}
        }),
    )?;
    run_cli_json(
        &home,
        &[
            "config",
            "tools",
            "set",
            "--file",
            tools_path.to_str().unwrap(),
        ],
    )?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut server = spawn_server_with_env(
        &home,
        port,
        &[
            "--tool-ceiling",
            "readonly",
            "--tool-root",
            "workspace",
            "--cli-tool",
            "echo=/bin/echo",
        ],
        &[],
    )?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &owner, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home, &graphql, Duration::from_secs(30)).await?;
    let resolved_root = fs::canonicalize(&workspace)?;
    let resolved_root = resolved_root.to_str().context("utf-8 resolved root")?;

    let live = run_cli_json(&home, &["tools", "explain"])?;
    assert_eq!(
        live["host_tool_ceiling"]["tool_ceiling"], "readonly",
        "{live}"
    );
    assert_eq!(
        live["host_tool_ceiling"]["tool_root"], resolved_root,
        "{live}"
    );
    assert_eq!(
        live["host_tool_ceiling"]["cli_tools"],
        json!(["echo"]),
        "{live}"
    );
    assert_eq!(live["host_tool_ceiling"]["live"], true, "{live}");
    let live_tools = explained_tool_names(&live, &agent_id)?;
    assert!(live_tools.iter().any(|name| name == "read_file"), "{live}");
    assert!(live_tools.iter().any(|name| name == "echo"), "{live}");
    assert!(
        !live_tools.iter().any(|name| name == "write_file"),
        "{live}"
    );

    let owner_home = home.join(".gents");
    let owner_home = owner_home.to_str().context("utf-8 owner home")?;
    let remote = run_cli_json(
        &elsewhere,
        &[
            "tools",
            "explain",
            "--home",
            owner_home,
            "--graphql",
            &graphql,
        ],
    )?;
    assert_eq!(remote["node_did"], owner.as_str(), "{remote}");
    assert_eq!(
        remote["host_tool_ceiling"]["tool_ceiling"], "readonly",
        "{remote}"
    );
    assert_eq!(
        remote["host_tool_ceiling"]["tool_root"], resolved_root,
        "{remote}"
    );

    server.child.kill()?;
    server.child.wait()?;
    let offline_text = run_cli_text(&home, &["tools", "explain"])?;
    assert_keys_in_order(
        &offline_text,
        &[
            "\"node_did\"",
            "\"host_tool_ceiling\"",
            "\"tool_ceiling\"",
            "\"tool_root\"",
            "\"live\"",
            "\"agents\"",
            "\"tool_names\"",
            "\"configuration_issues\"",
            "\"next\"",
            "\"ceiling_source\"",
            "\"ceiling_scope\"",
            "\"access_mode\"",
            "\"home\"",
        ],
    );
    let offline: Value = serde_json::from_str(&offline_text)?;
    assert_eq!(
        offline["host_tool_ceiling"]["tool_ceiling"], "readwrite",
        "{offline}"
    );
    assert_eq!(offline["ceiling_source"], "init_json", "{offline}");
    assert_eq!(offline["host_tool_ceiling"]["live"], false, "{offline}");
    let offline_tools = explained_tool_names(&offline, &agent_id)?;
    assert!(
        offline_tools.iter().any(|name| name == "write_file"),
        "{offline}"
    );
    assert!(
        !offline_tools.iter().any(|name| name == "echo"),
        "{offline}"
    );
    Ok(())
}

fn assert_keys_in_order(report: &str, keys: &[&str]) {
    let positions = keys
        .iter()
        .map(|key| {
            report
                .find(&format!("{key}:"))
                .unwrap_or_else(|| panic!("{key} missing from {report}"))
        })
        .collect::<Vec<_>>();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "keys out of reading order {keys:?}: {report}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_explain_refuses_a_live_root_the_server_cannot_resolve() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    fs::create_dir_all(&home)?;
    let dangling = home.join("dangling");
    std::os::unix::fs::symlink(home.join("missing"), &dangling)?;
    run_init_json(
        &home,
        &[
            "--node-name",
            "explain-dangling",
            "--tool-package",
            "minimal",
        ],
    )?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut server = spawn_server_with_env(
        &home,
        port,
        &[
            "--tool-ceiling",
            "readonly",
            "--tool-root",
            dangling.to_str().context("utf-8 dangling root")?,
        ],
        &[],
    )?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_state_graphql(&home, &graphql, Duration::from_secs(30)).await?;

    let stderr = run_cli_failure_stderr(&home, &["tools", "explain"])?;
    assert!(stderr.contains("has no usable root"), "{stderr}");
    assert!(stderr.contains(dangling.to_str().unwrap()), "{stderr}");
    assert!(stderr.contains("restart the server"), "{stderr}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_explain_refuses_a_home_whose_runtime_serves_another_node() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let cwd = tempdir.path().join("cwd");
    let home = tempdir.path().join("my home");
    let other = tempdir.path().join("other");
    for dir in [&cwd, &other] {
        fs::create_dir_all(dir)?;
    }
    let home_arg = home.to_str().context("utf-8 home")?;
    let quoted_home = format!("'{home_arg}'");
    let owner = node_did_from_init(&run_init_json(
        &cwd,
        &["--node-name", "explain-home", "--home", home_arg],
    )?)?;

    let offline = run_cli_json(&cwd, &["tools", "explain", "--home", home_arg])?;
    let next = offline["next"].as_str().unwrap_or_default();
    assert!(
        next.contains(&format!("gents server --home {quoted_home}")),
        "{offline}"
    );

    let other_owner =
        node_did_from_init(&run_init_json(&other, &["--node-name", "explain-other"])?)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut server = spawn_server(&other, port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &other_owner, Duration::from_secs(30)).await?;
    let other_state =
        wait_for_runtime_state_graphql(&other, &graphql, Duration::from_secs(30)).await?;
    write_json_file(&home.join("runtime.json"), &other_state)?;

    let stderr = run_cli_failure_stderr(&cwd, &["tools", "explain", "--home", home_arg])?;
    assert!(
        stderr.contains(&format!("belongs to node {owner}")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("serves node {other_owner}")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("gents server --home {quoted_home}")),
        "{stderr}"
    );
    Ok(())
}
