use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_list_and_show_include_request_count() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-session-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-session-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;

    run_cli_text(
        &home_dir,
        &["config", "export", "--root", &root.to_string_lossy()],
    )?;
    let config = read_json_file(&root.join("pack_config.json"))?;
    let agent_did = config["default_agent_selection"]
        .get("default_agent_did")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing default_agent_did after export"))?
        .to_string();

    let session_id = format!("session-{}", Uuid::new_v4().simple());
    for mutation in [
        format!(
            r#"mutation {{
                create_AgentSession(input: {{
                    session_id: "{}",
                    node_did: "{}",
                    agent_did: "{}",
                    created_at: "2026-06-12T10:00:00Z"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&session_id),
            escape_graphql_string(&node_did),
            escape_graphql_string(&agent_did),
        ),
        format!(
            r#"mutation {{
                create_AgentRequest(input: {{purpose: "normal", 
                    request_id: "{}",
                    node_did: "{}",
                    agent_did: "{}",
                    session_id: "{}",
                    lifecycle_state: "completed",
                    created_at: "2026-06-12T10:01:00Z"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&format!("{session_id}-request-a")),
            escape_graphql_string(&node_did),
            escape_graphql_string(&agent_did),
            escape_graphql_string(&session_id),
        ),
        format!(
            r#"mutation {{
                create_AgentRequest(input: {{purpose: "normal", 
                    request_id: "{}",
                    node_did: "{}",
                    agent_did: "{}",
                    session_id: "{}",
                    lifecycle_state: "completed",
                    created_at: "2026-06-12T10:02:00Z"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&format!("{session_id}-request-b")),
            escape_graphql_string(&node_did),
            escape_graphql_string(&agent_did),
            escape_graphql_string(&session_id),
        ),
    ] {
        graphql_query(&graphql, &mutation).await?;
    }

    let list = run_cli_json(
        &home_dir,
        &[
            "sessions",
            "list",
            "--graphql",
            &graphql,
            "--output",
            "json",
        ],
    )?;
    let list_row = list
        .get("items")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|row| row.get("session_id").and_then(Value::as_str) == Some(&session_id))
        })
        .ok_or_else(|| anyhow!("session list did not include {session_id}: {list}"))?;
    assert_eq!(
        list_row.get("request_count").and_then(Value::as_u64),
        Some(2)
    );

    let show = run_cli_json(
        &home_dir,
        &["sessions", "show", "--graphql", &graphql, &session_id],
    )?;
    assert_eq!(
        show.get("session_id").and_then(Value::as_str),
        Some(session_id.as_str())
    );
    assert_eq!(show.get("request_count").and_then(Value::as_u64), Some(2));

    let show_by_flag = run_cli_json(
        &home_dir,
        &[
            "sessions",
            "show",
            "--graphql",
            &graphql,
            "--id",
            &session_id,
        ],
    )?;
    assert_eq!(
        show_by_flag.get("session_id").and_then(Value::as_str),
        Some(session_id.as_str())
    );

    Ok(())
}

const FORK_OF_AN_UNKNOWN_SESSION: [&str; 5] = [
    "sessions",
    "fork",
    "--from",
    "any-session",
    "--at-user-turn",
];

fn init_fork_home(home_dir: &std::path::Path) -> Result<(MockModelEndpoint, String)> {
    fs::create_dir_all(home_dir)?;
    let model_name = format!("mock-session-fork-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let init = run_init_json(
        home_dir,
        &[
            "--node-name",
            &format!("cli-session-fork-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    Ok((mock_endpoint, node_did_from_init(&init)?))
}

fn fork_an_unknown_session(home_dir: &std::path::Path, node_did: &str) -> Result<String> {
    let mut args = FORK_OF_AN_UNKNOWN_SESSION.to_vec();
    args.extend(["0", "--node-did", node_did]);
    run_cli_failure_stderr(home_dir, &args)
}

#[test]
fn session_fork_against_a_held_store_refuses_with_the_holder_and_the_graphql_escape() -> Result<()>
{
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let (_mock_endpoint, node_did) = init_fork_home(&home_dir)?;

    let _held = gents_store::local::home::lock_home_store(&home_dir.join(".gents"))?;
    let stderr = fork_an_unknown_session(&home_dir, &node_did)?;

    assert!(
        stderr.contains("already using")
            && stderr.contains(&format!("process {}", std::process::id())),
        "a fork against a held store names the holder: {stderr}"
    );
    assert!(
        stderr.contains("--graphql"),
        "a fork against a held store points at the graphql escape: {stderr}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_fork_against_a_ready_server_is_answered_by_that_server() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let (_mock_endpoint, node_did) = init_fork_home(&home_dir)?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;

    let stderr = fork_an_unknown_session(&home_dir, &node_did)?;

    assert!(
        stderr.contains("session any-session was not found"),
        "the serving runtime answers a fork of a session it does not have: {stderr}"
    );
    assert!(
        !stderr.contains("already using"),
        "a fork reaches the serving runtime instead of contending for its store: {stderr}"
    );

    Ok(())
}
