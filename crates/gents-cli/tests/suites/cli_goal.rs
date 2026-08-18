use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn goal_set_get_pause_resume_and_clear_are_durable() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-goal-cli-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-goal-{}", Uuid::new_v4().simple());
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
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let session_id = format!("goal-session-{}", Uuid::new_v4().simple());
    let objective = "Finish the CLI durable goal";
    let set = run_cli_json(
        &home_dir,
        &[
            "goal",
            "set",
            "--graphql",
            &graphql,
            "--session",
            &session_id,
            "--objective",
            objective,
            "--status",
            "active",
            "--token-budget",
            "5000",
        ],
    )?;
    assert_eq!(
        set.get("objective").and_then(Value::as_str),
        Some(objective)
    );
    assert_eq!(set.get("status").and_then(Value::as_str), Some("active"));
    assert_eq!(set.get("token_budget").and_then(Value::as_i64), Some(5000));

    let paused = run_cli_json(
        &home_dir,
        &[
            "goal",
            "set",
            "--graphql",
            &graphql,
            "--session",
            &session_id,
            "--status",
            "paused",
        ],
    )?;
    assert_eq!(paused.get("status").and_then(Value::as_str), Some("paused"));
    assert_eq!(
        paused.get("objective").and_then(Value::as_str),
        Some(objective)
    );

    let resumed = run_cli_json(
        &home_dir,
        &[
            "goal",
            "set",
            "--graphql",
            &graphql,
            "--session",
            &session_id,
            "--status",
            "active",
        ],
    )?;
    assert_eq!(
        resumed.get("status").and_then(Value::as_str),
        Some("active")
    );

    let shown = run_cli_json(
        &home_dir,
        &[
            "goal",
            "show",
            "--graphql",
            &graphql,
            "--session",
            &session_id,
        ],
    )?;
    assert_eq!(
        shown.get("objective").and_then(Value::as_str),
        Some(objective)
    );

    let unbudgeted = run_cli_json(
        &home_dir,
        &[
            "goal",
            "set",
            "--graphql",
            &graphql,
            "--session",
            &session_id,
            "--clear-token-budget",
        ],
    )?;
    assert!(unbudgeted.get("token_budget").is_some_and(Value::is_null));

    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_Goal(input: {{
                    goal_id: "duplicate-{}",
                    session_id: "{}",
                    agent_did: "{}",
                    objective: "replicated twin",
                    status: "paused",
                    created_at: "2026-07-16T00:00:00Z"
                }}) {{ _docID }}
            }}"#,
            Uuid::new_v4().simple(),
            escape_graphql_string(&session_id),
            escape_graphql_string(&agent_did),
        ),
    )
    .await?;

    let cleared = run_cli_json(
        &home_dir,
        &[
            "goal",
            "clear",
            "--graphql",
            &graphql,
            "--session",
            &session_id,
        ],
    )?;
    assert_eq!(cleared.get("deleted").and_then(Value::as_bool), Some(true));

    let query = graphql_query(
        &graphql,
        &format!(
            r#"{{ Goal(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ goal_id }} }}"#,
            escape_graphql_string(&session_id)
        ),
    )
    .await?;
    assert_eq!(
        query
            .pointer("/data/Goal")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    Ok(())
}
