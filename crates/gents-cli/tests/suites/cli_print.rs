use anyhow::{ensure, Context, Result};
use serde_json::Value;

use super::cli_core_fixture::LiveCli;

async fn request_for(live: &LiveCli, content: &str) -> Result<Value> {
    live.first_row("AgentRequest", &format!(
        r#"{{AgentRequest(filter: {{node_did: {{_eq: "{}"}}, content: {{_eq: "{}"}}}}) {{request_id session_id agent_did}}}}"#,
        crate::support::escape_graphql_string(&live.node), crate::support::escape_graphql_string(content),
    )).await
}

async fn assert_file_read(live: &LiveCli, id: &str, token: &str) -> Result<()> {
    let calls = live.access.execute(&format!(
        r#"{{AgentToolCall(filter: {{request_id: {{_eq: "{}"}}, tool_name: {{_eq: "read_file"}}, lifecycle_state: {{_eq: "completed"}}}}) {{_docID node_did requester_did request_doc_id session_id}}}}"#,
        crate::support::escape_graphql_string(id),
    )).await?;
    let mut found = false;
    for call in calls["data"]["AgentToolCall"]
        .as_array()
        .context("file calls")?
    {
        let output = gents_session::tool_call::query::result::load_tool_call_result(
            &live.access,
            call["_docID"].as_str().context("tool doc")?,
            &live.node,
            call["session_id"].as_str().context("tool session")?,
            call["requester_did"].as_str(),
        )
        .await?;
        found |=
            gents_session::tool_call::query::result::render_tool_result(&output)?.contains(&token);
    }
    ensure!(
        found,
        "the completed file tool did not return the fixture contents"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET and run the live CLI target"]
async fn print_writes_only_the_reply_to_stdout() -> Result<()> {
    let live = LiveCli::start("live-cli-print", false).await?;
    let result = async {
        let token = format!("file-token-{}", uuid::Uuid::new_v4());
        tokio::fs::write(live.home.join("notes.txt"), format!("{token}\n")).await?;
        let prompt = "Call read_file for notes.txt, then acknowledge briefly.";
        let stdout = live.text(&["-p", prompt]).await?;
        let request = request_for(&live, prompt).await?;
        let id = request["request_id"].as_str().context("print request id")?;
        live.terminal(id).await?;
        ensure!(
            stdout.trim_end() == live.answer(id).await?.trim_end(),
            "print stdout differs from the final reply"
        );
        assert_file_read(&live, id, &token).await?;
        Ok(())
    }
    .await;
    live.finish(result).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET and run the live CLI target"]
async fn yolo_makes_one_agent_with_the_default_agents_tools_and_reuses_it() -> Result<()> {
    let live = LiveCli::start("live-cli-yolo", false).await?;
    let result = async {
        ensure!(!live.text(&["--yolo", "-p", "Acknowledge this first turn briefly; do not call tools."]).await?.trim().is_empty(), "first yolo turn has no reply");
        let query = format!(r#"{{Agent(filter: {{node_did: {{_eq: "{}"}}, display_name: {{_eq: "Yolo"}}}}) {{agent_did context_id}} WorkspaceRoot(filter: {{node_did: {{_eq: "{}"}}}}) {{root_path}}}}"#,
            crate::support::escape_graphql_string(&live.node), crate::support::escape_graphql_string(&live.node));
        let first = live.access.execute(&query).await?;
        let first_agent = first["data"]["Agent"][0]["agent_did"].as_str().context("yolo agent")?.to_owned();
        let root = first["data"]["WorkspaceRoot"][0]["root_path"].as_str().context("workspace root")?;
        live.json(&["config", "workspace-root", "revoke-reader", "--graphql", &live.graphql, "--path", root, "--agent-did", &first_agent]).await?;
        let token = format!("reader-token-{}", uuid::Uuid::new_v4());
        tokio::fs::write(live.home.join("reader.txt"), &token).await?;
        let prompt = "Call read_file for reader.txt, then acknowledge briefly.";
        ensure!(!live.text(&["--yolo", "-p", prompt]).await?.trim().is_empty(), "second yolo turn has no reply");
        let second = request_for(&live, prompt).await?;
        assert_file_read(&live, second["request_id"].as_str().context("reader request")?, &token).await?;
        let rows = live.access.execute(&query).await?;
        let agents = rows["data"]["Agent"].as_array().context("yolo agents")?;
        ensure!(agents.len() == 1 && agents[0]["agent_did"] == first_agent, "the yolo Agent was not reused: {rows}");
        ensure!(first_agent != live.agent, "yolo reused the default Agent");
        let contexts = live.access.execute(&format!(r#"{{Agent(filter: {{node_did: {{_eq: "{}"}}}}) {{agent_did context_id}} AgentContext {{context_id tools_id}}}}"#, crate::support::escape_graphql_string(&live.node))).await?;
        let tools_id = |agent: &str| -> Result<&str> {
            let context = contexts["data"]["Agent"].as_array().context("agents")?.iter().find(|row| row["agent_did"] == agent).context("Agent context")?["context_id"].as_str().context("context id")?;
            contexts["data"]["AgentContext"].as_array().context("contexts")?.iter().find(|row| row["context_id"] == context).context("selected context")?["tools_id"].as_str().context("tools id")
        };
        let ordinary = live.json(&["config", "tools", "show", tools_id(&live.agent)?, "--graphql", &live.graphql]).await?;
        let yolo = live.json(&["config", "tools", "show", tools_id(&first_agent)?, "--graphql", &live.graphql]).await?;
        for field in ["host", "self_config", "built_ins", "datastore", "integrations"] {
            ensure!(ordinary.get(field) == yolo.get(field), "yolo changed the default Agent's {field} tools");
        }
        Ok(())
    }.await;
    live.finish(result).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET and run the live CLI target"]
async fn continue_adds_a_turn_to_the_session_with_the_agent_it_had() -> Result<()> {
    let live = LiveCli::start("live-cli-continue", false).await?;
    let result = async {
        let first_prompt =
            "Acknowledge the first continuation test turn briefly; do not call tools.";
        live.text(&["--yolo", "-p", first_prompt]).await?;
        let first = request_for(&live, first_prompt).await?;
        let session = first["session_id"].as_str().context("session")?;
        let second_prompt =
            "Acknowledge the second continuation test turn briefly; do not call tools.";
        ensure!(
            !live
                .text(&["--continue", session, "-p", second_prompt])
                .await?
                .trim()
                .is_empty(),
            "continued turn has no reply"
        );
        let second = request_for(&live, second_prompt).await?;
        ensure!(
            second["session_id"] == first["session_id"]
                && second["agent_did"] == first["agent_did"],
            "continuation changed its session or Agent"
        );
        let error = live
            .failure(&["--continue", "no-such-session", "-p", "hello"])
            .await?;
        ensure!(
            error.contains("no session no-such-session") && error.contains("gents sessions list"),
            "missing-session error has no recovery action: {error}"
        );
        Ok(())
    }
    .await;
    live.finish(result).await
}
