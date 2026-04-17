mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduled_task_set_persists_concrete_default_behavior_id() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-task-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-task-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);
    let task_id = format!("task-{}", Uuid::new_v4().simple());
    let default_behavior_id = format!("{agent_did}:default");

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "task",
            "set",
            "--task-id",
            &task_id,
            "--name",
            "daily-check",
            "--prompt",
            "Check the repo health.",
            "--interval-secs",
            "600",
        ],
    )?;
    assert_eq!(
        output.get("behavior_id").and_then(Value::as_str),
        Some(default_behavior_id.as_str())
    );

    let query = format!(
        r#"{{
            ScheduledTask(filter: {{ task_id: {{ _eq: "{}" }} }}, limit: 1) {{
                task_id
                agent_did
                behavior_id
                name
                prompt
                interval_secs
                enabled
                next_run_at
            }}
        }}"#,
        escape_graphql_string(&task_id),
    );
    let response = graphql_query(&graphql, &query).await?;
    let row = first_graphql_row(&response, "ScheduledTask")?;
    assert_eq!(
        row.get("task_id").and_then(Value::as_str),
        Some(task_id.as_str())
    );
    assert_eq!(
        row.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        row.get("behavior_id").and_then(Value::as_str),
        Some(default_behavior_id.as_str())
    );
    assert_eq!(row.get("name").and_then(Value::as_str), Some("daily-check"));
    assert_eq!(
        row.get("prompt").and_then(Value::as_str),
        Some("Check the repo health.")
    );
    assert_eq!(row.get("interval_secs").and_then(Value::as_i64), Some(600));
    assert_eq!(row.get("enabled").and_then(Value::as_bool), Some(true));
    assert!(row.get("next_run_at").is_some());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduled_task_set_recreates_deleted_task_with_same_task_id() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-task-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-task-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);
    let task_id = format!("task-{}", Uuid::new_v4().simple());

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    run_cli_json(
        &home_dir,
        &[
            "config",
            "task",
            "set",
            "--task-id",
            &task_id,
            "--name",
            "ops-sweep",
            "--prompt",
            "Run the ops sweep.",
            "--interval-secs",
            "600",
        ],
    )?;

    let initial = graphql_query(
        &graphql,
        &format!(
            r#"{{
                ScheduledTask(filter: {{ task_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                    _deleted
                    prompt
                    interval_secs
                }}
            }}"#,
            escape_graphql_string(&task_id),
        ),
    )
    .await?;
    let initial_row = first_graphql_row(&initial, "ScheduledTask")?;
    let initial_doc_id = initial_row
        .get("_docID")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("scheduled task row missing _docID: {initial_row}"))?
        .to_string();

    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{ delete_ScheduledTask(docID: "{}") {{ _docID }} }}"#,
            escape_graphql_string(&initial_doc_id),
        ),
    )
    .await?;

    run_cli_json(
        &home_dir,
        &[
            "config",
            "task",
            "set",
            "--task-id",
            &task_id,
            "--name",
            "ops-sweep",
            "--prompt",
            "Run the ops sweep again.",
            "--interval-secs",
            "1200",
        ],
    )?;
    run_cli_json(
        &home_dir,
        &[
            "config",
            "task",
            "set",
            "--task-id",
            &task_id,
            "--name",
            "ops-sweep",
            "--prompt",
            "Run the ops sweep again.",
            "--interval-secs",
            "1200",
        ],
    )?;

    let recreated = graphql_query(
        &graphql,
        &format!(
            r#"{{
                ScheduledTask(showDeleted: true, filter: {{ task_id: {{ _eq: "{}" }} }}, limit: 4) {{
                    _docID
                    _deleted
                    prompt
                    interval_secs
                }}
            }}"#,
            escape_graphql_string(&task_id),
        ),
    )
    .await?;
    let rows = recreated
        .get("data")
        .and_then(|data| data.get("ScheduledTask"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("scheduled task rows missing from response: {recreated}"))?;
    let live_rows = rows
        .iter()
        .filter(|row| row.get("_deleted").and_then(Value::as_bool) == Some(false))
        .collect::<Vec<_>>();
    assert_eq!(
        live_rows.len(),
        1,
        "expected exactly one live task row after recreate"
    );
    let row = live_rows[0];
    assert_eq!(
        row.get("_deleted").and_then(Value::as_bool),
        Some(false),
        "recreated task should be live"
    );
    assert_eq!(
        row.get("prompt").and_then(Value::as_str),
        Some("Run the ops sweep again.")
    );
    assert_eq!(row.get("interval_secs").and_then(Value::as_i64), Some(1200));

    Ok(())
}
