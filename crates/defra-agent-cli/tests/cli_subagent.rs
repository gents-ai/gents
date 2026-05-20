mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_list_shows_two_level_dispatch_lineage() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-subagent-list-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            "cli-subagent-list",
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let runtime_agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &runtime_agent_did, Duration::from_secs(30)).await?;

    let test_agent_did = format!("did:key:zSubagentList{}", Uuid::new_v4().simple());
    let root_request_id = format!("root-{}", Uuid::new_v4().simple());
    let child_request_id = format!("child-{}", Uuid::new_v4().simple());
    let grandchild_request_id = format!("grandchild-{}", Uuid::new_v4().simple());

    seed_request(
        &graphql,
        &test_agent_did,
        &root_request_id,
        "parent-behavior",
        None,
        0,
        "2026-05-20T12:00:00Z",
    )
    .await?;
    seed_request(
        &graphql,
        &test_agent_did,
        &child_request_id,
        "child-behavior",
        Some(&root_request_id),
        1,
        "2026-05-20T12:00:01Z",
    )
    .await?;
    seed_request(
        &graphql,
        &test_agent_did,
        &grandchild_request_id,
        "grandchild-behavior",
        Some(&child_request_id),
        2,
        "2026-05-20T12:00:02Z",
    )
    .await?;

    let output = run_cli_json(
        &home_dir,
        &[
            "subagent",
            "list",
            "--graphql",
            &graphql,
            "--root",
            &root_request_id,
            "--output",
            "json",
        ],
    )?;
    let rows = output
        .get("rows")
        .and_then(Value::as_array)
        .context("subagent list JSON output missing rows array")?;
    assert_eq!(
        rows.len(),
        3,
        "expected root, child, and grandchild: {output}"
    );
    assert_lineage_row(rows, &root_request_id, None, 0, "parent-behavior")?;
    assert_lineage_row(
        rows,
        &child_request_id,
        Some(&root_request_id),
        1,
        "child-behavior",
    )?;
    assert_lineage_row(
        rows,
        &grandchild_request_id,
        Some(&child_request_id),
        2,
        "grandchild-behavior",
    )?;

    let text = run_cli_text(
        &home_dir,
        &[
            "subagent",
            "list",
            "--graphql",
            &graphql,
            "--root",
            &root_request_id,
        ],
    )?;
    assert!(
        text.contains("CHILD_REQUEST_ID"),
        "missing tree header: {text}"
    );
    assert!(text.contains(&root_request_id), "missing root row: {text}");
    assert!(
        text.contains(&format!("  {child_request_id}")),
        "default tree output must indent the child row: {text}"
    );
    assert!(
        text.contains(&format!("    {grandchild_request_id}")),
        "default tree output must indent the grandchild row: {text}"
    );

    let table = run_cli_text(
        &home_dir,
        &[
            "subagent",
            "list",
            "--graphql",
            &graphql,
            "--root",
            &root_request_id,
            "--output",
            "table",
        ],
    )?;
    assert!(
        table.contains("PARENT_REQUEST_ID"),
        "missing table header: {table}"
    );
    assert!(
        table
            .lines()
            .any(|line| line.starts_with(&child_request_id)),
        "flat table output must include child row without leading indentation: {table}"
    );
    assert!(
        !table
            .lines()
            .any(|line| line.starts_with(&format!("  {child_request_id}"))),
        "flat table output must not indent child rows: {table}"
    );

    let depth_limited = run_cli_json(
        &home_dir,
        &[
            "subagent",
            "list",
            "--graphql",
            &graphql,
            "--root",
            &root_request_id,
            "--depth",
            "1",
            "--output",
            "json",
        ],
    )?;
    let depth_rows = depth_limited
        .get("rows")
        .and_then(Value::as_array)
        .context("depth-limited output missing rows array")?;
    assert_eq!(
        depth_rows.len(),
        2,
        "--depth 1 should include root and direct child only: {depth_limited}"
    );
    assert!(
        !depth_rows.iter().any(|row| {
            row.get("child_request_id").and_then(Value::as_str)
                == Some(grandchild_request_id.as_str())
        }),
        "--depth 1 must exclude grandchild: {depth_limited}"
    );

    Ok(())
}

async fn seed_request(
    graphql: &str,
    agent_did: &str,
    request_id: &str,
    behavior_id: &str,
    parent_request_id: Option<&str>,
    subagent_depth: i64,
    created_at: &str,
) -> Result<()> {
    let session_id = format!("session-{request_id}");
    let parent_fields = parent_request_id
        .map(|parent| {
            format!(
                r#"
                    ,
                    caused_by_parent_request_id: "{}",
                    caused_by_parent_tool_call_id: "spawn-{}",
                    caused_by_trigger_id: "spawn-{}",
                    caused_by_trigger_kind: "subagent","#,
                escape_graphql_string(parent),
                escape_graphql_string(request_id),
                escape_graphql_string(request_id),
            )
        })
        .unwrap_or_default();
    graphql_query(
        graphql,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}",
                    session_id: "{session_id}",
                    retry_parent_request: "",
                    retry_root_request: "{request_id}",
                    superseded_by_request: "",
                    content: "seeded subagent list row",
                    status: "pending",
                    lifecycle_state: "pending",
                    backend_id: "",
                    execution_origin: "interactive",
                    failure_reason: "",
                    created_at: "{created_at}",
                    retry_count: 0,
                    max_retries: 3,
                    subagent_depth: {subagent_depth}{parent_fields}
                }}) {{ _docID }}
            }}"#,
            request_id = escape_graphql_string(request_id),
            agent_did = escape_graphql_string(agent_did),
            behavior_id = escape_graphql_string(behavior_id),
            session_id = escape_graphql_string(&session_id),
            created_at = escape_graphql_string(created_at),
        ),
    )
    .await?;
    Ok(())
}

fn assert_lineage_row(
    rows: &[Value],
    request_id: &str,
    parent_request_id: Option<&str>,
    depth: i64,
    behavior_id: &str,
) -> Result<()> {
    let row = rows
        .iter()
        .find(|row| row.get("child_request_id").and_then(Value::as_str) == Some(request_id))
        .with_context(|| format!("missing row for {request_id}: {rows:?}"))?;
    assert_eq!(
        row.get("parent_request_id").and_then(Value::as_str),
        parent_request_id
    );
    assert_eq!(row.get("depth").and_then(Value::as_i64), Some(depth));
    assert_eq!(
        row.get("behavior_id").and_then(Value::as_str),
        Some(behavior_id)
    );
    assert_eq!(row.get("state").and_then(Value::as_str), Some("pending"));
    assert!(
        row.get("started_at")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "row must expose started_at: {row}"
    );
    Ok(())
}
