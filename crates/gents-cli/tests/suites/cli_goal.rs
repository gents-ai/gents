use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn goal_configuration_rejects_implicit_resume_and_clear_is_durable() -> Result<()> {
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
            "--inference-url",
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

    let rejected = run_cli_failure_stderr(
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
    assert!(rejected.contains("resume-request"), "{rejected}");

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
    assert_eq!(shown.get("status").and_then(Value::as_str), Some("paused"));
    assert_eq!(
        shown.get("continuation_sequence"),
        paused.get("continuation_sequence")
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn goal_resume_request_reuses_signed_predecessor_and_returns_same_child() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-resume-cli-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "Durable predecessor complete.")?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-resume-{}", Uuid::new_v4().simple());
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
    let session_id = format!("resume-session-{}", Uuid::new_v4().simple());

    // The real CLI signs and submits the predecessor; the running daemon owns
    // its completion. No unsigned request or forged terminal row is seeded.
    let submitted = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--session-id",
            &session_id,
            "--content",
            "Finish this predecessor before operator recovery.",
            "--timeout-secs",
            "30",
            "--poll-secs",
            "1",
        ],
    )?;
    assert_eq!(
        submitted
            .pointer("/response/status")
            .and_then(Value::as_str),
        Some("complete")
    );
    let predecessor = submitted
        .get("request_id")
        .and_then(Value::as_str)
        .context("signed submission request ID")?;
    let paused = run_cli_json(
        &home_dir,
        &[
            "goal",
            "set",
            "--graphql",
            &graphql,
            "--session",
            &session_id,
            "--objective",
            "Resume from this completed signed predecessor.",
            "--status",
            "paused",
            "--token-budget",
            "1",
        ],
    )?;
    assert_eq!(paused.get("status").and_then(Value::as_str), Some("paused"));
    let resume_args = [
        "goal",
        "resume-request",
        "--graphql",
        &graphql,
        "--session",
        &session_id,
        "--from",
        predecessor,
    ];
    let first = run_cli_json(&home_dir, &resume_args)?;
    assert_eq!(first.get("created").and_then(Value::as_bool), Some(true));
    let child_id = first
        .get("request_id")
        .and_then(Value::as_str)
        .context("resume receipt request ID")?;
    let second = run_cli_json(&home_dir, &resume_args)?;
    assert_eq!(second.get("created").and_then(Value::as_bool), Some(false));
    assert_eq!(second.get("request_id"), first.get("request_id"));
    assert_eq!(second.get("doc_id"), first.get("doc_id"));
    assert_eq!(second.get("goal_id"), first.get("goal_id"));

    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
        AgentRequest(filter: {{ caused_by_parent_request_id: {{ _eq: "{}" }} }}) {{
            _docID request_id session_id agent_did caused_by_trigger_kind
            caused_by_parent_request_id caused_by_parent_request_doc_id
        }}
    }}"#,
            escape_graphql_string(predecessor)
        ),
    )
    .await?;
    let children = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .context("physical resume children")?;
    assert_eq!(
        children.len(),
        1,
        "one child of this predecessor: {response}"
    );
    let child = &children[0];
    assert_eq!(
        child.get("request_id").and_then(Value::as_str),
        Some(child_id)
    );
    assert_eq!(
        child.get("session_id").and_then(Value::as_str),
        Some(session_id.as_str())
    );
    assert_eq!(
        child.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        child.get("caused_by_trigger_kind").and_then(Value::as_str),
        Some("goal")
    );
    assert!(child
        .get("caused_by_parent_request_doc_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty()));
    Ok(())
}
