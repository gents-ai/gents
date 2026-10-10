use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

fn apply_live_file_agent_config(
    home: &std::path::Path,
    root: &std::path::Path,
    graphql: &str,
    node_did: &str,
    tools_id: &str,
    file_root: &std::path::Path,
    system_prompt: &std::path::Path,
    agent_ids: &[&str],
) -> Result<()> {
    run_cli_text(
        home,
        &[
            "config",
            "export",
            "--home",
            home.to_str().context("node home is not UTF-8")?,
            "--root",
            root.to_str().context("config root is not UTF-8")?,
            "--graphql",
            graphql,
            "--node-did",
            node_did,
        ],
    )?;
    let path = root.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    let tools = config["tools"]
        .as_array_mut()
        .context("tools is not an array")?
        .iter_mut()
        .find(|tools| tools["tools_id"] == tools_id)
        .context("default Tools document is missing")?;
    tools["host"] = serde_json::json!({
        "root": file_root.to_str().context("file root is not UTF-8")?,
        "files": {"mode": "ReadOnly"}
    });
    let prompt = fs::read_to_string(system_prompt)?;
    let base_agent = config["agents"][0].clone();
    let base_context = config["contexts"][0].clone();
    for agent_id in agent_ids {
        let existing_context_id = config["agents"]
            .as_array()
            .context("agents is not an array")?
            .iter()
            .find(|agent| agent["agent_id"] == *agent_id)
            .and_then(|agent| agent["context_id"].as_str())
            .map(ToOwned::to_owned);
        let context_id = if let Some(context_id) = existing_context_id {
            context_id
        } else {
            let context_id = format!("{agent_id}:context");
            let mut agent = base_agent.clone();
            agent["agent_id"] = Value::String((*agent_id).to_string());
            agent["context_id"] = Value::String(context_id.clone());
            config["agents"]
                .as_array_mut()
                .context("agents is not an array")?
                .push(agent);
            let mut context = base_context.clone();
            context["context_id"] = Value::String(context_id.clone());
            context["tools_id"] = Value::String(tools_id.to_string());
            config["contexts"]
                .as_array_mut()
                .context("contexts is not an array")?
                .push(context);
            context_id
        };
        let context = config["contexts"]
            .as_array_mut()
            .context("contexts is not an array")?
            .iter_mut()
            .find(|context| context["context_id"] == context_id)
            .context("agent context is missing")?;
        context["system_prompt"] = Value::String(prompt.clone());
        context["tools_id"] = Value::String(tools_id.to_string());
    }
    write_json_file(&path, &config)?;
    run_cli_json(
        home,
        &[
            "config",
            "apply",
            "--home",
            home.to_str().context("node home is not UTF-8")?,
            "--root",
            root.to_str().context("config root is not UTF-8")?,
            "--graphql",
            graphql,
        ],
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a reachable external OpenAI-compatible endpoint"]
async fn standard_onboarding_live_demo_runs_real_conversation_with_filesystem_tools() -> Result<()>
{
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("agent-home");
    let desktop_home = tempdir.path().join("desktop-home");
    fs::create_dir_all(&home_dir)?;
    let home_arg = home_dir
        .to_str()
        .ok_or_else(|| anyhow!("demo home path is not UTF-8"))?;

    let files_dir = home_dir.join("demo-files");
    fs::create_dir_all(&files_dir)?;
    let alpha_token = format!("LIVE_DEMO_ALPHA_{}", Uuid::new_v4().simple());
    let beta_token = format!("LIVE_DEMO_BETA_{}", Uuid::new_v4().simple());
    fs::write(files_dir.join("alpha.txt"), format!("{alpha_token}\n"))?;
    fs::write(files_dir.join("beta.txt"), format!("{beta_token}\n"))?;

    let system_prompt = tempdir.path().join("standard_onboarding_system_prompt.txt");
    fs::write(
        &system_prompt,
        "This is a live onboarding smoke test. When the user asks about files, use list_files and read_file before answering. Do not infer file contents from names. Keep final answers short and include the exact requested file tokens.",
    )?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-live-demo-{}", Uuid::new_v4().simple());
    let model_endpoint = std::env::var("GENTS_CLI_E2E_MODEL_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_MODEL_ENDPOINT.to_string());
    let model_name = std::env::var("GENTS_CLI_E2E_MODEL_NAME")
        .unwrap_or_else(|_| DEFAULT_MODEL_NAME.to_string());

    let init_args = vec![
        "--home".to_string(),
        home_arg.to_string(),
        "--node-name".to_string(),
        agent_name.clone(),
        "--model-name".to_string(),
        model_name,
        "--max-concurrent".to_string(),
        "2".to_string(),
        "--max-queue-depth".to_string(),
        "4".to_string(),
        "--inference-url".to_string(),
        model_endpoint,
    ];
    let init_arg_refs = init_args.iter().map(String::as_str).collect::<Vec<_>>();
    let init = run_init_json(&home_dir, &init_arg_refs)?;
    let node_did = node_did_from_init(&init)?;
    let agent_id = init
        .pointer("/init/default_agent_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing default_agent_id: {init}"))?
        .to_string();
    let tools_id = init
        .pointer("/init/tools_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing tools_id: {init}"))?
        .to_string();

    let (mut serve, readiness) =
        spawn_server_with_ready_json(&home_dir, port, &["--home", home_arg], &[])?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    assert_eq!(
        readiness.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );

    let desktop_init = run_desktop_init_json(&home_dir, &desktop_home, "Standard Onboarding Demo")?;
    assert_eq!(
        desktop_init.get("status").and_then(Value::as_str),
        Some("initialized")
    );
    assert_eq!(
        desktop_init.get("source").and_then(Value::as_str),
        Some("local-standard")
    );
    assert_eq!(
        desktop_init.get("nodeDid").and_then(Value::as_str),
        Some(node_did.as_str())
    );
    assert_eq!(
        desktop_init.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );
    assert_eq!(
        desktop_init.get("p2pTransport").and_then(Value::as_str),
        Some("iroh")
    );
    let desktop_next_steps = desktop_init
        .get("nextSteps")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("desktop init output missing nextSteps: {desktop_init}"))?;
    assert!(
        desktop_next_steps.iter().any(|step| step
            .as_str()
            .is_some_and(|step| step.contains("replication subscriptions armed"))),
        "desktop init should tell the demo to wait for desktop bootstrap before chat replication: {desktop_init}"
    );
    let peer_directory_path = desktop_home.join("peers.json");
    let peer_directory: Value = serde_json::from_slice(
        &fs::read(&peer_directory_path)
            .with_context(|| format!("reading {}", peer_directory_path.display()))?,
    )
    .with_context(|| format!("decoding {}", peer_directory_path.display()))?;
    let peer = peer_directory
        .get("peers")
        .and_then(Value::as_array)
        .and_then(|peers| peers.first())
        .ok_or_else(|| anyhow!("desktop init did not persist a peer: {peer_directory}"))?;
    assert_eq!(
        peer.get("source").and_then(Value::as_str),
        Some("local-standard")
    );
    assert_eq!(
        peer.get("node_did").and_then(Value::as_str),
        Some(node_did.as_str())
    );
    assert_eq!(
        peer.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );

    apply_live_file_agent_config(
        &home_dir,
        &tempdir.path().join("live-config"),
        &graphql,
        &node_did,
        &tools_id,
        &home_dir,
        &system_prompt,
        &[&agent_id],
    )?;
    wait_for_runtime_quiescence(&graphql, &node_did, 2, Duration::from_secs(6)).await?;

    let session_id = Uuid::new_v4().to_string();
    let first_prompt = "Use the filesystem tools. First list demo-files, then read demo-files/alpha.txt, then reply with only the exact token in alpha.txt.";
    let first = run_cli_json(
        &home_dir,
        &[
            "chat",
            "--home",
            home_arg,
            "--session-id",
            &session_id,
            "--output",
            "json",
            "--timeout-secs",
            "240",
            "--poll-secs",
            "1",
            first_prompt,
        ],
    )?;
    assert_eq!(
        first.get("session_id").and_then(Value::as_str),
        Some(session_id.as_str())
    );
    let first_content = first
        .pointer("/output/presentation/body_markdown")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("first live chat output missing response content: {first}"))?;
    assert!(
        first_content.contains(&alpha_token),
        "expected first response to contain {alpha_token}, got content={first_content:?}; full output={first}"
    );

    let second_prompt = "Continue this same conversation. Read demo-files/beta.txt with the filesystem tools, then reply with the alpha token from the previous turn and the exact beta token, separated by a single space.";
    let second = run_cli_json(
        &home_dir,
        &[
            "chat",
            "--home",
            home_arg,
            "--session-id",
            &session_id,
            "--output",
            "json",
            "--timeout-secs",
            "240",
            "--poll-secs",
            "1",
            second_prompt,
        ],
    )?;
    assert_eq!(
        second.get("session_id").and_then(Value::as_str),
        Some(session_id.as_str())
    );
    let second_content = second
        .pointer("/output/presentation/body_markdown")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("second live chat output missing response content: {second}"))?;
    assert!(
        second_content.contains(&alpha_token) && second_content.contains(&beta_token),
        "expected second response to contain {alpha_token} and {beta_token}, got content={second_content:?}; full output={second}"
    );

    wait_for_completed_tool_calls(&graphql, &session_id, "list_files", 1).await?;
    let read_calls = wait_for_completed_tool_calls(&graphql, &session_id, "read_file", 2).await?;
    let mut read_results = Vec::new();
    for call in &read_calls {
        read_results.push(canonical_tool_result_text(&graphql, call).await?);
    }
    let read_results = read_results.join("\n");
    assert!(
        read_results.contains(&alpha_token) && read_results.contains(&beta_token),
        "expected persisted read_file tool results to contain {alpha_token} and {beta_token}: {read_results}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a reachable external OpenAI-compatible endpoint"]
async fn trace_project_exports_live_inference_turn_as_adapter_artifacts() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let files_dir = home_dir.join("live-projection-files");
    fs::create_dir_all(&files_dir)?;
    let token = format!("LIVE_PROJECTION_{}", Uuid::new_v4().simple());
    let relative_fixture = "live-projection-files/source.txt";
    fs::write(home_dir.join(relative_fixture), format!("{token}\n"))?;

    let system_prompt = tempdir.path().join("projection_system_prompt.txt");
    fs::write(
        &system_prompt,
        "When the user asks about local files, use read_file before answering. Keep final answers to the requested file contents only.",
    )?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-live-projection-{}", Uuid::new_v4().simple());
    let model_endpoint = std::env::var("GENTS_CLI_E2E_MODEL_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_MODEL_ENDPOINT.to_string());
    let model_name = std::env::var("GENTS_CLI_E2E_MODEL_NAME")
        .unwrap_or_else(|_| DEFAULT_MODEL_NAME.to_string());
    let mut init_args = vec![
        "--node-name".to_string(),
        agent_name,
        "--model-name".to_string(),
        model_name.clone(),
        "--max-concurrent".to_string(),
        "1".to_string(),
        "--max-queue-depth".to_string(),
        "2".to_string(),
    ];
    if std::env::var_os("GENTS_CLI_E2E_API_KEY").is_some() {
        init_args.push("--api-key-env-var".to_string());
        init_args.push("GENTS_CLI_E2E_API_KEY".to_string());
    }
    init_args.push("--inference-url".to_string());
    init_args.push(model_endpoint);
    let init_arg_refs = init_args.iter().map(String::as_str).collect::<Vec<_>>();
    let init = run_init_json(&home_dir, &init_arg_refs)?;
    let node_did = node_did_from_init(&init)?;
    let agent_id = init
        .pointer("/init/default_agent_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing default_agent_id: {init}"))?
        .to_string();
    let tools_id = init
        .pointer("/init/tools_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing tools_id: {init}"))?
        .to_string();

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    apply_live_file_agent_config(
        &home_dir,
        &tempdir.path().join("projection-config"),
        &graphql,
        &node_did,
        &tools_id,
        &home_dir,
        &system_prompt,
        &[&agent_id],
    )?;
    wait_for_runtime_quiescence(&graphql, &node_did, 2, Duration::from_secs(6)).await?;

    let session_id = format!("live-projection-session-{}", Uuid::new_v4().simple());
    let prompt = format!(
        "Use read_file to read `{relative_fixture}`, then reply with exactly the file contents and no extra words."
    );
    let result = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--node-did",
            &node_did,
            "--session-id",
            &session_id,
            "--agent-id",
            &agent_id,
            "--content",
            &prompt,
            "--timeout-secs",
            "240",
            "--poll-secs",
            "1",
        ],
    )?;
    let request_id = result
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("live request result missing request_id: {result}"))?
        .to_string();
    assert_eq!(
        result.get("session_id").and_then(Value::as_str),
        Some(session_id.as_str())
    );
    let response = result
        .pointer("/output/presentation/body_markdown")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("live request result missing response content: {result}"))?;
    assert!(
        response.contains(&token),
        "expected live response to contain {token}, got {response:?}; full output={result}"
    );
    let read_calls = wait_for_completed_tool_calls(&graphql, &session_id, "read_file", 1).await?;
    let mut tool_results = Vec::new();
    for call in &read_calls {
        tool_results.push(canonical_tool_result_text(&graphql, call).await?);
    }
    let tool_results = tool_results.join("\n");
    assert!(
        tool_results.contains(&token),
        "expected persisted read_file result to contain {token}: {tool_results}"
    );

    let openai = trace_project_json(
        &home_dir,
        &graphql,
        &request_id,
        "openai-codex",
        "training-safe",
    )?;
    assert_eq!(
        openai.get("projection_id").and_then(Value::as_str),
        Some("openai_codex_run_trace")
    );
    assert_eq!(
        openai.get("source_request_id").and_then(Value::as_str),
        Some(request_id.as_str())
    );
    assert_eq!(
        openai.get("source_session_id").and_then(Value::as_str),
        Some(session_id.as_str())
    );
    assert_eq!(
        openai.pointer("/output/adapter").and_then(Value::as_str),
        Some("openai_codex_run_trace")
    );
    assert_eq!(
        openai
            .pointer("/output/projection/run_id")
            .and_then(Value::as_str),
        Some(request_id.as_str())
    );
    assert_eq!(
        openai
            .pointer("/output/projection/thread_id")
            .and_then(Value::as_str),
        Some(session_id.as_str())
    );
    let items = openai
        .pointer("/output/projection/items")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("openai-codex projection missing items: {openai}"))?;
    assert!(
        items.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("request")
                && item.get("id").and_then(Value::as_str) == Some(request_id.as_str())
                && item.get("status").and_then(Value::as_str) == Some("completed")
        }),
        "live openai-codex projection missing completed root request item: {openai:#}"
    );
    let read_file_call_id = items
        .iter()
        .find(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_call")
                && item.get("name").and_then(Value::as_str) == Some("read_file")
                && item.get("status").and_then(Value::as_str) == Some("completed")
        })
        .and_then(|item| item.get("id").and_then(Value::as_str))
        .ok_or_else(|| {
            anyhow!(
                "live openai-codex projection missing completed read_file tool item: {openai:#}"
            )
        })?
        .to_string();
    // The request's terminal_output names its canonical terminal message;
    // projection order is by timestamp, not terminal selection.
    let request_row = graphql_query(
        &graphql,
        &format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{
                _docID terminal_output
            }} }}"#,
            escape_graphql_string(&request_id)
        ),
    )
    .await?;
    let request_row = first_graphql_row(&request_row, "AgentRequest")?.clone();
    let terminal_output = match request_row.get("terminal_output") {
        Some(Value::String(encoded)) => serde_json::from_str::<Value>(encoded)?,
        Some(value) => value.clone(),
        None => Value::Null,
    };
    assert_eq!(
        terminal_output.get("kind").and_then(Value::as_str),
        Some("message"),
        "completed live request must name a terminal message: {request_row}"
    );
    let message_doc_id = terminal_output
        .get("message_doc_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("terminal_output lacks message_doc_id: {terminal_output}"))?;
    let rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentMessage(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{
                    session_id request_doc_id sequence role
                }}
                AgentToolCall(filter: {{ request_id: {{ _eq: "{}" }}, tool_call_id: {{ _eq: "{}" }} }}, limit: 2) {{
                    message_sequence
                }}
            }}"#,
            escape_graphql_string(message_doc_id),
            escape_graphql_string(&request_id),
            escape_graphql_string(&read_file_call_id),
        ),
    )
    .await?;
    let terminal_message = first_graphql_row(&rows, "AgentMessage")?;
    assert_eq!(
        terminal_message.get("role").and_then(Value::as_str),
        Some("assistant")
    );
    assert_eq!(
        terminal_message.get("request_doc_id"),
        request_row.get("_docID"),
        "terminal message must belong to the live request: {terminal_message}"
    );
    let terminal_sequence = terminal_message
        .get("sequence")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("terminal message lacks a sequence: {terminal_message}"))?;
    let read_file_sequence = first_graphql_row(&rows, "AgentToolCall")?
        .get("message_sequence")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("read_file call lacks its message sequence: {rows}"))?;
    assert!(
        terminal_sequence > read_file_sequence,
        "terminal message {terminal_sequence} must follow the read_file call {read_file_sequence}"
    );
    let terminal_item_id = format!("{session_id}:message:{terminal_sequence}");
    assert!(
        items.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && item.get("id").and_then(Value::as_str) == Some(terminal_item_id.as_str())
                && item.get("role").and_then(Value::as_str) == Some("assistant")
                && item.get("request_id").and_then(Value::as_str) == Some(request_id.as_str())
        }),
        "live openai-codex projection missing the terminal message {terminal_item_id}: {openai:#}"
    );
    let serialized_openai = serde_json::to_string(&openai)?;
    assert!(
        serialized_openai.contains("[training_safe_redacted]"),
        "training-safe live projection should redact content-bearing fields: {openai:#}"
    );
    assert!(
        !serialized_openai.contains(&token) && !serialized_openai.contains(&prompt),
        "training-safe live projection leaked prompt or model output: {openai:#}"
    );

    let openai_jsonl = trace_project_jsonl_lines(
        &home_dir,
        &graphql,
        &request_id,
        "openai-codex",
        "training-safe",
        "jsonl",
    )?;
    assert_projection_records(
        &openai_jsonl,
        "openai_codex_run_trace",
        &request_id,
        &session_id,
    );
    assert!(
        openai_jsonl.iter().any(|record| {
            record.get("record_kind").and_then(Value::as_str) == Some("openai_codex_trace_item")
                && record
                    .get("value")
                    .and_then(|value| value.get("type"))
                    .and_then(Value::as_str)
                    == Some("tool_call")
                && record
                    .get("value")
                    .and_then(|value| value.get("name"))
                    .and_then(Value::as_str)
                    == Some("read_file")
        }),
        "live openai-codex JSONL missing read_file record: {openai_jsonl:#?}"
    );

    let openai_eval_jsonl = trace_project_jsonl_lines(
        &home_dir,
        &graphql,
        &request_id,
        "openai-codex",
        "training-safe",
        "eval-jsonl",
    )?;
    assert_projection_records(
        &openai_eval_jsonl,
        "openai_codex_run_trace",
        &request_id,
        &session_id,
    );
    assert!(
        openai_eval_jsonl.iter().any(|record| {
            record.get("sample_kind").and_then(Value::as_str) == Some("tool_call")
                && record.get("tool_name").and_then(Value::as_str) == Some("read_file")
        }),
        "live openai-codex eval JSONL missing read_file sample: {openai_eval_jsonl:#?}"
    );

    let langgraph_eval_jsonl = trace_project_jsonl_lines(
        &home_dir,
        &graphql,
        &request_id,
        "langgraph",
        "training-safe",
        "eval-jsonl",
    )?;
    assert_projection_records(
        &langgraph_eval_jsonl,
        "langgraph_state_history",
        &request_id,
        &session_id,
    );
    assert!(
        langgraph_eval_jsonl.iter().any(|record| {
            record.get("sample_kind").and_then(Value::as_str) == Some("task")
                && record.get("tool_name").and_then(Value::as_str) == Some("read_file")
        }),
        "live LangGraph eval JSONL missing read_file task: {langgraph_eval_jsonl:#?}"
    );

    let multi_agent_eval_jsonl = trace_project_jsonl_lines(
        &home_dir,
        &graphql,
        &request_id,
        "multi-agent",
        "training-safe",
        "eval-jsonl",
    )?;
    assert_projection_records(
        &multi_agent_eval_jsonl,
        "multi_agent_task",
        &request_id,
        &session_id,
    );
    assert!(
        multi_agent_eval_jsonl.iter().any(|record| {
            record.get("sample_kind").and_then(Value::as_str) == Some("participant")
                && record
                    .get("metadata")
                    .and_then(|metadata| metadata.get("node_did"))
                    .and_then(Value::as_str)
                    == Some(node_did.as_str())
        }),
        "live multi-agent eval JSONL missing owner participant: {multi_agent_eval_jsonl:#?}"
    );
    assert!(
        multi_agent_eval_jsonl.iter().any(|record| {
            record.get("sample_kind").and_then(Value::as_str) == Some("tool_call")
                && record.get("tool_name").and_then(Value::as_str) == Some("read_file")
        }),
        "live multi-agent eval JSONL missing read_file tool sample: {multi_agent_eval_jsonl:#?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a reachable external OpenAI-compatible endpoint"]
async fn cli_flow_runs_real_tool_loop_against_live_endpoint() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    struct LiveRequestSpec {
        agent_id: String,
        prompt: String,
        tokens: Vec<String>,
    }

    let files_dir = home_dir.join("live-smoke-files");
    fs::create_dir_all(&files_dir)?;
    let agent_name = format!("cli-live-{}", Uuid::new_v4().simple());
    let mut request_specs = Vec::new();
    for request_index in 0..4 {
        let mut paths = Vec::new();
        let mut tokens = Vec::new();
        for file_index in 0..3 {
            let path = format!("live-smoke-files/request-{request_index}-file-{file_index}.txt");
            let token = format!(
                "LIVE_E2E_REQUEST_{request_index}_FILE_{file_index}_{}",
                Uuid::new_v4().simple()
            );
            fs::write(home_dir.join(&path), format!("{token}\n"))?;
            paths.push(path);
            tokens.push(token);
        }
        let prompt = format!(
            "This is live concurrency request {request_index}. First call list_files for live-smoke-files. Then call read_file separately for each of these files, in this exact order: {}. Reply with only the file tokens in that same order, separated by spaces. Do not guess or reuse contents from another request.",
            paths.join(", ")
        );
        request_specs.push(LiveRequestSpec {
            agent_id: format!("live-{request_index}"),
            prompt,
            tokens,
        });
    }

    let system_prompt = tempdir.path().join("system_prompt.txt");
    fs::write(
        &system_prompt,
        "When the user asks about local files, use the available file tools instead of guessing. For multi-file requests, call read_file separately for every requested path before answering. Keep final answers to the requested file tokens only.",
    )?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let model_endpoint = std::env::var("GENTS_CLI_E2E_MODEL_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_MODEL_ENDPOINT.to_string());
    let model_name = std::env::var("GENTS_CLI_E2E_MODEL_NAME")
        .unwrap_or_else(|_| DEFAULT_MODEL_NAME.to_string());
    let mut init_args = vec![
        "--node-name".to_string(),
        agent_name.clone(),
        "--model-name".to_string(),
        model_name.clone(),
        "--max-concurrent".to_string(),
        "4".to_string(),
        "--max-queue-depth".to_string(),
        "8".to_string(),
    ];
    if std::env::var_os("GENTS_CLI_E2E_API_KEY").is_some() {
        init_args.push("--api-key-env-var".to_string());
        init_args.push("GENTS_CLI_E2E_API_KEY".to_string());
    }
    init_args.push("--inference-url".to_string());
    init_args.push(model_endpoint.clone());
    let init_arg_refs = init_args.iter().map(String::as_str).collect::<Vec<_>>();
    let init = run_init_json(&home_dir, &init_arg_refs)?;
    let node_did = node_did_from_init(&init)?;
    let backend_id = init
        .pointer("/init/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_id: {init}"))?
        .to_string();
    let tools_id = init
        .pointer("/init/tools_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing tools_id: {init}"))?
        .to_string();
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    let agent_ids = request_specs
        .iter()
        .map(|spec| spec.agent_id.as_str())
        .collect::<Vec<_>>();
    apply_live_file_agent_config(
        &home_dir,
        &tempdir.path().join("smoke-config"),
        &graphql,
        &node_did,
        &tools_id,
        &home_dir,
        &system_prompt,
        &agent_ids,
    )?;
    wait_for_runtime_quiescence(&graphql, &node_did, 2, Duration::from_secs(6)).await?;

    let mut children = Vec::new();
    for spec in &request_specs {
        let child = spawn_cli(
            &home_dir,
            &[
                "request",
                "submit",
                "--graphql",
                &graphql,
                "--node-did",
                &node_did,
                "--agent-id",
                &spec.agent_id,
                "--content",
                &spec.prompt,
                "--timeout-secs",
                "240",
                "--poll-secs",
                "1",
            ],
        )?;
        children.push((spec, child));
    }

    let mut outputs = Vec::new();
    let mut wait_errors = Vec::new();
    for (spec, child) in children {
        match child.wait_with_output() {
            Ok(output) => outputs.push((spec, output)),
            Err(error) => wait_errors.push(format!("{}: {error}", spec.agent_id)),
        }
    }
    if !wait_errors.is_empty() {
        bail!(
            "failed waiting for live request child process(es): {}",
            wait_errors.join("; ")
        );
    }

    for (spec, output) in outputs {
        if !output.status.success() {
            let (server_stdout, server_stderr) = serve.captured_output()?;
            bail!(
                "live request {} failed\nstdout:\n{}\nstderr:\n{}\nserver stdout:\n{}\nserver stderr:\n{}",
                spec.agent_id,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                server_stdout,
                server_stderr
            );
        }
        let result: Value = serde_json::from_slice(&output.stdout)
            .with_context(|| format!("parsing live request JSON for {}", spec.agent_id))?;
        assert_eq!(
            result.get("agent_id").and_then(Value::as_str),
            Some(spec.agent_id.as_str())
        );
        let response = result
            .pointer("/output/presentation/body_markdown")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow!("request submit result did not include response content: {result}")
            })?;
        for token in &spec.tokens {
            assert!(
                response.contains(token),
                "expected response for {} to contain token {token}, got {response}",
                spec.agent_id
            );
        }
        let session_id = result
            .get("session_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("live request result missing session_id: {result}"))?;
        let tool_calls =
            wait_for_completed_tool_calls(&graphql, session_id, "read_file", spec.tokens.len())
                .await?;
        let mut tool_results = Vec::new();
        for call in &tool_calls {
            tool_results.push(canonical_tool_result_text(&graphql, call).await?);
        }
        let tool_results = tool_results.join("\n");
        for token in &spec.tokens {
            assert!(
                tool_results.contains(token),
                "expected persisted read_file tool calls for {} to include token {token}: {tool_results}",
                spec.agent_id
            );
        }
    }

    wait_for_completed_inference_agents(
        &graphql,
        &backend_id,
        &request_specs
            .iter()
            .map(|spec| spec.agent_id.as_str())
            .collect::<Vec<_>>(),
    )
    .await?;

    Ok(())
}

fn trace_project_json(
    home_dir: &std::path::Path,
    graphql: &str,
    request_id: &str,
    projection: &str,
    redaction: &str,
) -> Result<Value> {
    let output = run_cli_text(
        home_dir,
        &[
            "trace",
            "project",
            "--graphql",
            graphql,
            "--request-id",
            request_id,
            "--projection",
            projection,
            "--redaction",
            redaction,
            "--actor-did",
            "did:test:live-projection-test",
        ],
    )?;
    serde_json::from_str::<Value>(&output).context("parsing live adapter projection JSON")
}

fn trace_project_jsonl_lines(
    home_dir: &std::path::Path,
    graphql: &str,
    request_id: &str,
    projection: &str,
    redaction: &str,
    format: &str,
) -> Result<Vec<Value>> {
    let output = run_cli_text(
        home_dir,
        &[
            "trace",
            "project",
            "--graphql",
            graphql,
            "--request-id",
            request_id,
            "--projection",
            projection,
            "--redaction",
            redaction,
            "--format",
            format,
            "--actor-did",
            "did:test:live-projection-test",
        ],
    )?;
    output
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).context("parsing live adapter projection JSONL")
        })
        .collect::<Result<Vec<_>>>()
}

fn assert_projection_records(
    records: &[Value],
    projection_id: &str,
    request_id: &str,
    session_id: &str,
) {
    assert!(
        !records.is_empty(),
        "expected {projection_id} records for live request {request_id}"
    );
    assert!(
        records.iter().all(|record| {
            record.get("projection_id").and_then(Value::as_str) == Some(projection_id)
                && record.get("source_request_id").and_then(Value::as_str) == Some(request_id)
                && record.get("source_session_id").and_then(Value::as_str) == Some(session_id)
                && record.get("redaction_mode").and_then(Value::as_str) == Some("training_safe")
        }),
        "live {projection_id} records did not carry consistent provenance/redaction: {records:#?}"
    );
}
