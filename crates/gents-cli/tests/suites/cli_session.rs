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
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;

    run_cli_text(
        &home_dir,
        &["config", "export", "--root", &root.to_string_lossy()],
    )?;
    let principal = read_json_file(&root.join("agent-principal.json"))?;
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing default_behavior_id after export"))?
        .to_string();

    let session_id = format!("session-{}", Uuid::new_v4().simple());
    for mutation in [
        format!(
            r#"mutation {{
                create_AgentSession(input: {{
                    session_id: "{}",
                    agent_name: "{}",
                    behavior_id: "{}",
                    started: "2026-06-12T10:00:00Z",
                    status: "active"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&session_id),
            escape_graphql_string(&agent_name),
            escape_graphql_string(&behavior_id),
        ),
        format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{}",
                    agent_did: "{}",
                    behavior_id: "{}",
                    session_id: "{}",
                    status: "completed",
                    lifecycle_state: "completed",
                    created_at: "2026-06-12T10:01:00Z"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&format!("{session_id}-request-a")),
            escape_graphql_string(&agent_did),
            escape_graphql_string(&behavior_id),
            escape_graphql_string(&session_id),
        ),
        format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{}",
                    agent_did: "{}",
                    behavior_id: "{}",
                    session_id: "{}",
                    status: "completed",
                    lifecycle_state: "completed",
                    created_at: "2026-06-12T10:02:00Z"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&format!("{session_id}-request-b")),
            escape_graphql_string(&agent_did),
            escape_graphql_string(&behavior_id),
            escape_graphql_string(&session_id),
        ),
    ] {
        graphql_query(&graphql, &mutation).await?;
    }

    let list = run_cli_json(
        &home_dir,
        &["session", "list", "--graphql", &graphql, "--output", "json"],
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
        &["session", "show", "--graphql", &graphql, &session_id],
    )?;
    assert_eq!(
        show.get("session_id").and_then(Value::as_str),
        Some(session_id.as_str())
    );
    assert_eq!(show.get("request_count").and_then(Value::as_u64), Some(2));

    let show_by_flag = run_cli_json(
        &home_dir,
        &[
            "session",
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
