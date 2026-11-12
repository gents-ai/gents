use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde_json::Value;

use super::cli_core_fixture::{path_text, LiveCli};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET and run the live CLI target"]
async fn request_submit_waits_for_response_by_default() -> Result<()> {
    let live = LiveCli::start("live-cli-request-wait", false).await?;
    let result = async {
        let response = live
            .json(&[
                "utils",
                "debug",
                "request",
                "submit",
                "--graphql",
                &live.graphql,
                "--node-did",
                &live.node,
                "--content",
                "Acknowledge this request briefly; do not call tools.",
                "--timeout-secs",
                "90",
                "--poll-secs",
                "1",
            ])
            .await?;
        let request = response["request_id"]
            .as_str()
            .context("submitted request id")?;
        ensure!(
            response["output"]["kind"] == "terminal_message",
            "submit returned before its terminal output: {response}"
        );
        live.terminal(request).await?;
        ensure!(
            response["output"]["presentation"]["body_markdown"].as_str()
                == Some(live.answer(request).await?.as_str()),
            "submit output differs from persisted terminal output"
        );
        Ok(())
    }
    .await;
    live.finish(result).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET and run the live CLI target"]
async fn request_submit_supports_content_file_and_output_file() -> Result<()> {
    let live = LiveCli::start("live-cli-request-files", false).await?;
    let result = async {
        let prompt = "Acknowledge this file-supplied request briefly; do not call tools.";
        let input = live.home.join("request.txt");
        let output = live.home.join("response.json");
        tokio::fs::write(&input, prompt).await?;
        let response = live
            .json(&[
                "utils",
                "debug",
                "request",
                "submit",
                "--graphql",
                &live.graphql,
                "--node-did",
                &live.node,
                "--content-file",
                path_text(&input)?,
                "--output-file",
                path_text(&output)?,
                "--timeout-secs",
                "90",
                "--poll-secs",
                "1",
            ])
            .await?;
        let written: Value = serde_json::from_slice(&tokio::fs::read(&output).await?)?;
        ensure!(
            response == written,
            "output file differs from the command's JSON receipt"
        );
        let request = response["request_id"].as_str().context("request id")?;
        live.terminal(request).await?;
        let row = live
            .first_row(
                "AgentRequest",
                &format!(
                    r#"{{AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{content}}}}"#,
                    crate::support::escape_graphql_string(request)
                ),
            )
            .await?;
        ensure!(
            row["content"] == prompt,
            "content file was not submitted verbatim"
        );
        live.answer(request).await?;
        Ok(())
    }
    .await;
    live.finish(result).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET and run the live CLI target"]
async fn request_interrupt_waits_until_running_request_is_terminal() -> Result<()> {
    let live = LiveCli::start("live-cli-request-interrupt", true).await?;
    let result = async {
        let fifo = live.home.join("held-read");
        let status = tokio::process::Command::new("mkfifo").arg(&fifo).status().await?;
        ensure!(status.success(), "could not create held-read FIFO");
        let arguments = serde_json::json!({"command":"cat","args":[path_text(&fifo)?]});
        let prompt = format!("Call bash_unrestricted with exactly these arguments: {arguments}. Wait for its result, then acknowledge briefly.");
        let submitted = live.json(&["utils", "debug", "request", "submit", "--graphql", &live.graphql,
            "--node-did", &live.node, "--content", &prompt, "--no-wait"]).await?;
        let request = submitted["request_id"].as_str().context("request id")?;
        let query = format!(r#"{{AgentToolCall(filter: {{request_id: {{_eq: "{}"}}, tool_name: {{_eq: "bash_unrestricted"}}}}) {{lifecycle_state}}}}"#, crate::support::escape_graphql_string(request));
        gents::support::live_inference::wait_for_rows(&live.access, "AgentToolCall", &query, Duration::from_secs(30), |data| {
            data["AgentToolCall"].as_array().is_some_and(|rows| rows.iter().any(|row| row["lifecycle_state"] == "running"))
        }).await;
        let interrupted = live.json(&["utils", "debug", "request", "interrupt", "--graphql", &live.graphql,
            "--wait", "--timeout", "20s", "--output", "json", request]).await?;
        ensure!(interrupted["request_id"] == request && interrupted["lifecycle_state"] == "interrupted" && interrupted["terminal"] == true, "interrupt returned before the request terminalized: {interrupted}");
        let row = live.first_row("AgentRequest", &format!(r#"{{AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{lifecycle_state interrupt_requested_at}}}}"#, crate::support::escape_graphql_string(request))).await?;
        ensure!(row["lifecycle_state"] == "interrupted" && row["interrupt_requested_at"].as_str().is_some_and(|s| !s.is_empty()), "the durable interrupt was not retained: {row}");
        Ok(())
    }.await;
    live.finish(result).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET and run the live CLI target"]
async fn request_interrupt_does_not_latch_completed_request() -> Result<()> {
    let live = LiveCli::start("live-cli-request-complete", false).await?;
    let result = async {
        let submitted = live.request(&uuid::Uuid::new_v4().to_string(), "Acknowledge briefly without calling tools.").await?;
        let request = submitted["request_id"].as_str().context("request id")?;
        live.terminal(request).await?;
        let error = live.failure(&["utils", "debug", "request", "interrupt", "--graphql", &live.graphql,
            "--output", "json", request]).await?;
        ensure!(error.contains("already completed") && error.contains("gents utils debug request show"), "completed interrupt did not explain the refusal: {error}");
        let row = live.first_row("AgentRequest", &format!(r#"{{AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{lifecycle_state interrupt_requested_at}}}}"#, crate::support::escape_graphql_string(request))).await?;
        ensure!(row["lifecycle_state"] == "completed" && row.get("interrupt_requested_at").is_none_or(Value::is_null), "the completed request acquired interrupt intent: {row}");
        Ok(())
    }.await;
    live.finish(result).await
}
