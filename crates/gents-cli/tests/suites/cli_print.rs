use crate::support::*;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

/// A home initialized against `endpoint` and served, as `(server, graphql, node_did)`.
async fn served_home(
    home_dir: &Path,
    model_name: &str,
    endpoint: &str,
) -> Result<(ServeProcess, String, String)> {
    let init = run_init_json(
        home_dir,
        &[
            "--node-name",
            &format!("cli-print-{}", Uuid::new_v4().simple()),
            "--model-name",
            model_name,
            "--inference-url",
            endpoint,
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let (serve, port, _readiness) =
        spawn_server_with_ready_json_recovering(home_dir, allocate_port()?, &[], &[])?;
    let graphql = graphql_url(port);
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(home_dir, &graphql, Duration::from_secs(30)).await?;
    Ok((serve, graphql, node_did))
}

/// `gents <args>` with no terminal, as a script runs it; returns stdout.
fn gents_without_a_terminal(home_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("running gents {args:?}"))?;
    if !output.status.success() {
        bail!(
            "gents {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn print_writes_only_the_reply_to_stdout() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    fs::write(home_dir.join("notes.txt"), "print-tool-token\n")?;
    let model_name = format!("mock-print-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockOpenAIEndpoint::start(&model_name, "print-tool-token")?;
    let (_serve, _graphql, _node_did) =
        served_home(&home_dir, &model_name, mock_endpoint.endpoint()).await?;

    let stdout = gents_without_a_terminal(
        &home_dir,
        &["-p", "Read notes.txt and reply with its token."],
    )?;

    assert_eq!(
        stdout, "print-tool-token\n",
        "stdout carries the reply alone, with no tool lines"
    );
    assert!(
        mock_endpoint
            .captured_chat_requests()
            .iter()
            .any(|request| request_contains_role_text(request, "tool", "print-tool-token")),
        "the agent read the file through its tool before replying"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn yolo_makes_one_agent_with_the_default_agents_tools_and_reuses_it() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let reply = format!("yolo-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-yolo-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &reply)?;
    let (_serve, graphql, node_did) =
        served_home(&home_dir, &model_name, mock_endpoint.endpoint()).await?;

    for _ in 0..2 {
        let stdout = gents_without_a_terminal(
            &home_dir,
            &["--yolo", "-p", "Reply with exactly the configured token."],
        )?;
        assert_eq!(stdout.trim(), reply);
    }

    let rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                Agent(filter: {{node_did: {{_eq: "{node_did}"}}}}) {{ agent_id display_name context_id }}
                AgentContext(filter: {{node_did: {{_eq: "{node_did}"}}}}) {{ context_id tools_id system_prompt }}
                AgentRequest(filter: {{node_did: {{_eq: "{node_did}"}}}}) {{ agent_id session_id }}
            }}"#
        ),
    )
    .await?;
    let agents = rows["data"]["Agent"].as_array().context("Agent rows")?;
    let yolo_id = format!("{node_did}:yolo");
    let yolo: Vec<&Value> = agents
        .iter()
        .filter(|agent| agent["display_name"] == "Yolo")
        .collect();
    assert_eq!(yolo.len(), 1, "one yolo agent across both runs: {agents:?}");
    assert_eq!(yolo[0]["agent_id"], yolo_id.as_str());

    let context_of = |agent_id: &str| -> Result<&Value> {
        let context_id = agents
            .iter()
            .find(|agent| agent["agent_id"] == agent_id)
            .and_then(|agent| agent["context_id"].as_str())
            .ok_or_else(|| anyhow!("{agent_id} has no context"))?;
        rows["data"]["AgentContext"]
            .as_array()
            .and_then(|contexts| contexts.iter().find(|c| c["context_id"] == context_id))
            .ok_or_else(|| anyhow!("context {context_id} is missing"))
    };
    let default_context = context_of(&format!("{node_did}:default"))?;
    let yolo_context = context_of(&yolo_id)?;
    let tools_of = |context: &Value| -> Result<Value> {
        let tools_id = context["tools_id"]
            .as_str()
            .context("context has no tools")?;
        let mut tools = run_cli_json(
            &home_dir,
            &["config", "tools", "show", tools_id, "--graphql", &graphql],
        )?;
        let tools = tools.as_object_mut().context("Tools is an object")?;
        for identity in ["tools_id", "_docID", "doc_id"] {
            tools.remove(identity);
        }
        Ok(Value::Object(tools.clone()))
    };
    assert_eq!(
        tools_of(yolo_context)?,
        tools_of(default_context)?,
        "the yolo agent runs with a copy of the default agent's tools"
    );
    assert_ne!(
        yolo_context["system_prompt"],
        default_context["system_prompt"]
    );

    let sessions: std::collections::BTreeSet<&str> = rows["data"]["AgentRequest"]
        .as_array()
        .context("AgentRequest rows")?
        .iter()
        .filter(|request| request["agent_id"] == yolo_id.as_str())
        .filter_map(|request| request["session_id"].as_str())
        .collect();
    assert_eq!(sessions.len(), 2, "both runs went to the same yolo agent");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn continue_adds_a_turn_to_the_session_with_the_agent_it_had() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let reply = format!("continue-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-continue-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &reply)?;
    let (_serve, graphql, node_did) =
        served_home(&home_dir, &model_name, mock_endpoint.endpoint()).await?;
    let turn = |content: &str| {
        let graphql = graphql.clone();
        let query = format!(
            r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{node_did}"}}, content: {{_eq: "{content}"}}}}) {{ session_id agent_id }} }}"#
        );
        async move {
            let rows = graphql_query(&graphql, &query).await?;
            rows["data"]["AgentRequest"]
                .as_array()
                .and_then(|rows| rows.first().cloned())
                .context("the turn has a request")
        }
    };

    gents_without_a_terminal(&home_dir, &["--yolo", "-p", "First turn."])?;
    let first = turn("First turn.").await?;
    let session = first["session_id"]
        .as_str()
        .context("a session")?
        .to_owned();
    let stdout =
        gents_without_a_terminal(&home_dir, &["--continue", &session, "-p", "Second turn."])?;
    assert_eq!(stdout.trim(), reply);
    let second = turn("Second turn.").await?;
    assert_eq!(second["session_id"], session.as_str(), "the same session");
    assert_eq!(
        second["agent_id"],
        format!("{node_did}:yolo").as_str(),
        "the agent the session had, with no --agent"
    );

    let unknown = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .args(["--continue", "no-such-session", "-p", "hello"])
        .stdin(Stdio::null())
        .output()?;
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(!unknown.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains("no session no-such-session") && stderr.contains("gents sessions list"),
        "stderr:\n{stderr}"
    );
    Ok(())
}
