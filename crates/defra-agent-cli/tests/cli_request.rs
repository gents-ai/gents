mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_submit_waits_for_response_by_default() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-submit-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-submit-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let request_agent_did = format!("did:key:z{}", Uuid::new_v4().simple());
    let request_content = format!("CLI wait test {}", Uuid::new_v4());
    let expected_content = format!("wait-ok-{}", Uuid::new_v4().simple());

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

    let submit = spawn_cli(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &request_agent_did,
            "--content",
            &request_content,
            "--timeout-secs",
            "20",
            "--poll-secs",
            "1",
        ],
    )?;

    let (request_id, session_id, behavior_id) =
        wait_for_request(&graphql, &request_agent_did, &request_content).await?;
    insert_terminal_response(
        &graphql,
        &request_id,
        &request_agent_did,
        &behavior_id,
        &session_id,
        &expected_content,
    )
    .await?;

    let output = submit
        .wait_with_output()
        .context("waiting for request submit child")?;
    if !output.status.success() {
        bail!(
            "request submit failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let parsed: Value =
        serde_json::from_slice(&output.stdout).context("parsing request submit JSON")?;
    assert_eq!(
        parsed.get("request_id").and_then(Value::as_str),
        Some(request_id.as_str())
    );
    assert_eq!(
        parsed.pointer("/response/status").and_then(Value::as_str),
        Some("complete")
    );
    assert_eq!(
        parsed.pointer("/response/content").and_then(Value::as_str),
        Some(expected_content.as_str())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_submit_supports_content_file_and_output_file() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-submit-file-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-submit-file-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let request_agent_did = format!("did:key:z{}", Uuid::new_v4().simple());
    let request_content = format!("CLI file request {}", Uuid::new_v4());
    let expected_content = format!("wait-file-ok-{}", Uuid::new_v4().simple());
    let content_path = tempdir.path().join("request.txt");
    let output_path = tempdir.path().join("request-output.json");
    fs::write(&content_path, &request_content)?;

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

    let submit = spawn_cli(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &request_agent_did,
            "--content-file",
            content_path
                .to_str()
                .ok_or_else(|| anyhow!("content path is not utf-8"))?,
            "--output-file",
            output_path
                .to_str()
                .ok_or_else(|| anyhow!("output path is not utf-8"))?,
            "--timeout-secs",
            "20",
            "--poll-secs",
            "1",
        ],
    )?;

    let (request_id, session_id, behavior_id) =
        wait_for_request(&graphql, &request_agent_did, &request_content).await?;
    insert_terminal_response(
        &graphql,
        &request_id,
        &request_agent_did,
        &behavior_id,
        &session_id,
        &expected_content,
    )
    .await?;

    let output = submit
        .wait_with_output()
        .context("waiting for request submit child")?;
    if !output.status.success() {
        bail!(
            "request submit failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout_json: Value =
        serde_json::from_slice(&output.stdout).context("parsing request submit JSON")?;
    let file_json = read_json_file(&output_path)?;
    assert_eq!(stdout_json, file_json);
    assert_eq!(
        stdout_json
            .pointer("/response/content")
            .and_then(Value::as_str),
        Some(expected_content.as_str())
    );

    Ok(())
}
