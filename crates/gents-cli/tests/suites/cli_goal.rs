use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use super::cli_core_fixture::LiveCli;

async fn start_paused_goal(
    live: &LiveCli,
    session: &str,
    objective: &str,
    budget: Option<&str>,
) -> Result<(Value, String)> {
    let fifo = live.home.join("goal-release");
    let status = tokio::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .await?;
    ensure!(status.success(), "creating goal release FIFO");
    let mut writer = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .await?;
    let args = serde_json::json!({
        "command": format!("IFS= read -r line < '{}'; printf 'goal fixture released'", fifo.display()),
        "args": [],
    });
    let hold = format!("Call bash_unrestricted directly, exactly once, with these arguments: {args}. Do not call spawn_process or any other tool. Wait for the result, then acknowledge briefly.");
    let started = live
        .json(&[
            "automation",
            "goal",
            "start",
            "--graphql",
            &live.graphql,
            "--session",
            session,
            "--objective",
            &hold,
        ])
        .await?;
    let predecessor = started["request_id"]
        .as_str()
        .context("goal start request")?
        .to_owned();
    let query = format!(
        r#"{{AgentToolCall(filter: {{request_id: {{_eq: "{}"}}, tool_name: {{_eq: "bash_unrestricted"}}}}) {{lifecycle_state}}}}"#,
        crate::support::escape_graphql_string(&predecessor)
    );
    gents::support::live_inference::wait_for_rows(
        &live.access,
        "AgentToolCall",
        &query,
        Duration::from_secs(30),
        |data| {
            data["AgentToolCall"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["lifecycle_state"] == "running"))
        },
    )
    .await;
    let mut args = vec![
        "automation",
        "goal",
        "set",
        "--graphql",
        &live.graphql,
        "--session",
        session,
        "--objective",
        objective,
        "--status",
        "paused",
    ];
    if let Some(budget) = budget {
        args.extend(["--token-budget", budget]);
    }
    let paused = live.json(&args).await?;
    writer.write_all(b"released\n").await?;
    live.terminal(&predecessor).await?;
    Ok((paused, predecessor))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET and run the live CLI target"]
async fn goal_configuration_rejects_implicit_resume_and_clear_is_durable() -> Result<()> {
    let live = LiveCli::start("live-cli-goal-config", true).await?;
    let result = async {
        let session = uuid::Uuid::new_v4().to_string();
        live.request(
            &session,
            "Acknowledge the new session briefly; do not call tools.",
        )
        .await?;
        let objective = "Finish the CLI durable goal after an explicit resumption";
        let (paused, _) = start_paused_goal(&live, &session, objective, Some("5000")).await?;
        ensure!(
            paused["objective"] == objective
                && paused["status"] == "paused"
                && paused["token_budget"] == 5000,
            "goal configuration did not persist: {paused}"
        );
        let rejected = live
            .failure(&[
                "automation",
                "goal",
                "set",
                "--graphql",
                &live.graphql,
                "--session",
                &session,
                "--status",
                "active",
            ])
            .await?;
        ensure!(
            rejected.contains("resume-request"),
            "implicit resume did not name the explicit action: {rejected}"
        );
        let shown = live
            .json(&[
                "automation",
                "goal",
                "show",
                "--graphql",
                &live.graphql,
                "--session",
                &session,
            ])
            .await?;
        ensure!(
            shown["objective"] == objective
                && shown["status"] == "paused"
                && shown["continuation_sequence"] == paused["continuation_sequence"],
            "rejected resume changed the Goal: {shown}"
        );
        let unbudgeted = live
            .json(&[
                "automation",
                "goal",
                "set",
                "--graphql",
                &live.graphql,
                "--session",
                &session,
                "--clear-token-budget",
            ])
            .await?;
        ensure!(
            unbudgeted.get("token_budget").is_some_and(Value::is_null),
            "budget was not cleared: {unbudgeted}"
        );
        let cleared = live
            .json(&[
                "automation",
                "goal",
                "clear",
                "--graphql",
                &live.graphql,
                "--session",
                &session,
            ])
            .await?;
        ensure!(
            cleared["deleted"] == true,
            "goal clear did not delete: {cleared}"
        );
        let rows = live
            .access
            .execute(&format!(
                r#"{{Goal(filter: {{session_id: {{_eq: "{}"}}}}) {{goal_id}}}}"#,
                crate::support::escape_graphql_string(&session)
            ))
            .await?;
        ensure!(
            rows["data"]["Goal"].as_array().is_some_and(Vec::is_empty),
            "Goal remained after clear: {rows}"
        );
        Ok(())
    }
    .await;
    live.finish(result).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET and run the live CLI target"]
async fn goal_resume_request_reuses_signed_predecessor_and_returns_same_child() -> Result<()> {
    let live = LiveCli::start("live-cli-goal-resume", true).await?;
    let result = async {
        let session = uuid::Uuid::new_v4().to_string();
        live.request(&session, "Acknowledge this session briefly; do not call tools.").await?;
        let (_, predecessor) = start_paused_goal(&live, &session,
            "Call update_goal with status complete, then acknowledge briefly.", None).await?;
        let args = ["automation", "goal", "resume-request", "--graphql", &live.graphql, "--session", &session, "--from", &predecessor];
        let first = live.json(&args).await?;
        let second = live.json(&args).await?;
        ensure!(first["created"] == true && second["created"] == false, "resume did not replay the original receipt: {first} {second}");
        for field in ["request_id", "doc_id", "goal_id"] { ensure!(first[field] == second[field], "resume changed {field}"); }
        let child = first["request_id"].as_str().context("resume child")?;
        live.terminal(child).await?;
        let rows = live.access.execute(&format!(r#"{{AgentRequest(filter: {{caused_by_parent_request_id: {{_eq: "{}"}}, caused_by_trigger_kind: {{_eq: "goal"}}}}) {{request_id session_id node_did caused_by_parent_request_doc_id}}}}"#, crate::support::escape_graphql_string(&predecessor))).await?;
        let children = rows["data"]["AgentRequest"].as_array().context("resume children")?;
        ensure!(children.len() == 1 && children[0]["request_id"] == child && children[0]["session_id"] == session && children[0]["node_did"] == live.node, "resume did not retain one exact child: {rows}");
        ensure!(children[0]["caused_by_parent_request_doc_id"].as_str().is_some_and(|s| !s.is_empty()), "resume omitted its physical predecessor");
        Ok(())
    }.await;
    live.finish(result).await
}
