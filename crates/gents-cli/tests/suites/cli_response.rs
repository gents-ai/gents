use crate::support::*;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

struct ResponseTestRuntime {
    _tempdir: tempfile::TempDir,
    home_dir: PathBuf,
    _mock_endpoint: MockModelEndpoint,
    _serve: ServeProcess,
    graphql: String,
    agent_did: String,
}

async fn start_response_runtime(label: &str) -> Result<ResponseTestRuntime> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-response-{label}-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-response-{label}-{}", Uuid::new_v4().simple());
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
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    Ok(ResponseTestRuntime {
        _tempdir: tempdir,
        home_dir,
        _mock_endpoint: mock_endpoint,
        _serve: serve,
        graphql,
        agent_did,
    })
}

struct MaterializedResponse<'a> {
    request_id: &'a str,
    session_id: &'a str,
    response_content: &'a str,
    response_reasoning: &'a str,
    message_content: Option<&'a str>,
    message_reasoning: &'a str,
    sequence: i64,
}

async fn insert_materialized_response(
    runtime: &ResponseTestRuntime,
    fixture: MaterializedResponse<'_>,
) -> Result<()> {
    let MaterializedResponse {
        request_id,
        session_id,
        response_content,
        response_reasoning,
        message_content,
        message_reasoning,
        sequence,
    } = fixture;
    let now = chrono::Utc::now().to_rfc3339();
    let response_key = format!("response-{request_id}");
    let message_mutation = message_content.map_or_else(String::new, |content| {
        let message_key = format!("{session_id}:{sequence}");
        format!(
            r#"create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{session_id}",
                agent_did: "{agent_did}",
                request_id: "{request_id}",
                sequence: {sequence},
                role: "assistant",
                content: "{content}",
                reasoning: "{message_reasoning}",
                timestamp: "{now}"
            }}) {{ _docID }}"#,
            message_key = escape_graphql_string(&message_key),
            session_id = escape_graphql_string(session_id),
            agent_did = escape_graphql_string(&runtime.agent_did),
            request_id = escape_graphql_string(request_id),
            content = escape_graphql_string(content),
            message_reasoning = escape_graphql_string(message_reasoning),
            now = escape_graphql_string(&now),
        )
    });
    let mutation = format!(
        r#"mutation {{
            {message_mutation}
            create_AgentResponse(input: {{
                response_key: "{response_key}",
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "response-test",
                session_id: "{session_id}",
                content: "{response_content}",
                reasoning: "{response_reasoning}",
                status: "complete",
                error_message: "",
                token_count: 1241,
                progress_seq: 31,
                reasoning_progress_seq: 17,
                materialized_message_sequence: {sequence},
                materialized_at: "{now}",
                created_at: "{now}",
                completed_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        response_key = escape_graphql_string(&response_key),
        request_id = escape_graphql_string(request_id),
        agent_did = escape_graphql_string(&runtime.agent_did),
        session_id = escape_graphql_string(session_id),
        response_content = escape_graphql_string(response_content),
        response_reasoning = escape_graphql_string(response_reasoning),
        now = escape_graphql_string(&now),
    );
    graphql_query(&runtime.graphql, &mutation).await?;
    Ok(())
}

fn response_show(runtime: &ResponseTestRuntime, request_id: &str) -> Result<Value> {
    run_cli_json(
        &runtime.home_dir,
        &[
            "response",
            "show",
            "--graphql",
            &runtime.graphql,
            request_id,
        ],
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_show_hydrates_materialized_content_like_response_wait() -> Result<()> {
    let runtime = start_response_runtime("hydrate").await?;
    let request_id = format!("response-hydrate-{}", Uuid::new_v4().simple());
    let session_id = format!("session-hydrate-{}", Uuid::new_v4().simple());
    let durable_content = format!("durable answer {}", Uuid::new_v4().simple());
    let durable_reasoning = format!("durable reasoning {}", Uuid::new_v4().simple());
    let persisted_message = serde_json::json!({
        "role": "assistant",
        "id": null,
        "content": [{ "text": durable_content }]
    })
    .to_string();

    insert_materialized_response(
        &runtime,
        MaterializedResponse {
            request_id: &request_id,
            session_id: &session_id,
            response_content: "",
            response_reasoning: "",
            message_content: Some(&persisted_message),
            message_reasoning: &durable_reasoning,
            sequence: 409,
        },
    )
    .await?;

    let shown = response_show(&runtime, &request_id)?;
    let waited = run_cli_json(
        &runtime.home_dir,
        &[
            "response",
            "wait",
            "--graphql",
            &runtime.graphql,
            "--timeout-secs",
            "5",
            "--poll-secs",
            "1",
            &request_id,
        ],
    )?;
    let shown_row = shown
        .pointer("/data/AgentResponse/0")
        .context("response show output missing GraphQL envelope row")?;

    assert_eq!(shown_row.get("content"), waited.get("content"));
    assert_eq!(shown_row.get("reasoning"), waited.get("reasoning"));
    assert_eq!(
        shown_row.get("content").and_then(Value::as_str),
        Some(durable_content.as_str())
    );
    assert_eq!(
        shown_row.get("reasoning").and_then(Value::as_str),
        Some(durable_reasoning.as_str())
    );
    assert_eq!(
        shown_row.get("status").and_then(Value::as_str),
        Some("complete")
    );
    assert_eq!(
        shown_row.get("token_count").and_then(Value::as_i64),
        Some(1241)
    );
    assert_eq!(
        shown_row.get("progress_seq").and_then(Value::as_i64),
        Some(31)
    );
    assert_eq!(
        shown_row
            .get("reasoning_progress_seq")
            .and_then(Value::as_i64),
        Some(17)
    );
    assert_eq!(
        shown_row
            .get("materialized_message_sequence")
            .and_then(Value::as_i64),
        Some(409)
    );
    assert!(shown_row
        .get("materialized_at")
        .is_some_and(Value::is_string));
    assert!(shown_row.get("completed_at").is_some_and(Value::is_string));

    let preserved_request_id = format!("response-preserved-{}", Uuid::new_v4().simple());
    let preserved_session_id = format!("session-preserved-{}", Uuid::new_v4().simple());
    insert_materialized_response(
        &runtime,
        MaterializedResponse {
            request_id: &preserved_request_id,
            session_id: &preserved_session_id,
            response_content: "already present",
            response_reasoning: "already reasoned",
            message_content: Some("durable replacement that must not win"),
            message_reasoning: "replacement reasoning that must not win",
            sequence: 7,
        },
    )
    .await?;
    let preserved = response_show(&runtime, &preserved_request_id)?;
    let preserved_row = preserved
        .pointer("/data/AgentResponse/0")
        .context("preserved response show output missing GraphQL envelope row")?;
    assert_eq!(
        preserved_row.get("content").and_then(Value::as_str),
        Some("already present")
    );
    assert_eq!(
        preserved_row.get("reasoning").and_then(Value::as_str),
        Some("already reasoned")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_show_diagnoses_missing_materialized_message() -> Result<()> {
    let runtime = start_response_runtime("missing").await?;
    let request_id = format!("response-missing-{}", Uuid::new_v4().simple());
    let session_id = format!("session-missing-{}", Uuid::new_v4().simple());
    insert_materialized_response(
        &runtime,
        MaterializedResponse {
            request_id: &request_id,
            session_id: &session_id,
            response_content: "",
            response_reasoning: "",
            message_content: None,
            message_reasoning: "",
            sequence: 73,
        },
    )
    .await?;

    let stderr = run_cli_failure_stderr(
        &runtime.home_dir,
        &[
            "response",
            "show",
            "--graphql",
            &runtime.graphql,
            &request_id,
        ],
    )?;
    assert!(stderr.contains("could not hydrate materialized AgentMessage"));
    assert!(stderr.contains(&format!("request {request_id}")));
    assert!(stderr.contains(&format!("session_id={session_id}")));
    assert!(stderr.contains("sequence=73"));
    assert!(stderr.contains("missing or invalid"));

    Ok(())
}
