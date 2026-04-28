mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_state_reset_is_explicit() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("reset-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-reset-{}", Uuid::new_v4().simple());

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

    let runtime_home = home_dir.join(".defra-agent");
    let runtime_state = runtime_home.join("runtime.json");
    fs::write(&runtime_state, r#"{"status":"stale"}"#).context("writing stale runtime state")?;

    let rerun = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    assert_eq!(
        rerun.get("runtime_state_reset").and_then(Value::as_bool),
        Some(false)
    );
    assert!(
        runtime_state.exists(),
        "init without --reset should leave runtime.json in place"
    );

    let reset_init = run_init_json(
        &home_dir,
        &[
            "--reset",
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    assert_eq!(
        reset_init
            .get("runtime_state_reset")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        !runtime_state.exists(),
        "init --reset should remove runtime.json"
    );

    fs::write(&runtime_state, r#"{"status":"stale-again"}"#)
        .context("rewriting stale runtime state")?;
    let reset = run_cli_json(&home_dir, &["reset"])?;
    assert_eq!(reset.get("status").and_then(Value::as_str), Some("reset"));
    assert_eq!(reset.get("cleared").and_then(Value::as_bool), Some(true));
    assert!(
        !runtime_state.exists(),
        "reset command should remove runtime.json"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconciled_runtime_sends_generation_two_tools_and_completes_tool_loop() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let token = format!("E2E_TOKEN_{}", Uuid::new_v4().simple());
    fs::write(home_dir.join("notes.txt"), format!("{token}\n"))?;

    let system_prompt = tempdir.path().join("system_prompt.txt");
    fs::write(
        &system_prompt,
        "When the user asks you to read a local file, call read_file and respond with only the token from that file.",
    )?;

    let model_name = format!("mock-tool-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockOpenAIEndpoint::start(&model_name, &token)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-tool-loop-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);
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
    let backend_id = init
        .pointer("/init/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_id: {init}"))?
        .to_string();
    let selection_id = init
        .pointer("/init/tool_selection_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing tool_selection_id: {init}"))?
        .to_string();
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    let behavior = run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "set",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--display-name",
            "Default",
            "--system-prompt-file",
            system_prompt
                .to_str()
                .context("system prompt path is not UTF-8")?,
            "--backend-id",
            &backend_id,
            "--model-name",
            &model_name,
            "--tool-selection-id",
            &selection_id,
        ],
    )?;
    let behavior_doc_id = behavior
        .get("doc_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("behavior output missing doc_id: {behavior}"))?;
    let selection_doc_id = doc_id_for_selection(&graphql, &selection_id).await?;
    let config_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentBehavior(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                    behavior_id
                    tool_selection_id
                    backend_id
                }}
                ToolSelection(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                    selection_id
                    enable_file_tools
                    file_tools_mode
                }}
            }}"#,
            escape_graphql_string(behavior_doc_id),
            escape_graphql_string(&selection_doc_id),
        ),
    )
    .await?;
    let behavior_row = first_graphql_row(&config_rows, "AgentBehavior")?;
    assert_eq!(
        behavior_row
            .get("tool_selection_id")
            .and_then(Value::as_str),
        Some(selection_id.as_str())
    );
    assert_eq!(
        behavior_row.get("backend_id").and_then(Value::as_str),
        Some(backend_id.as_str())
    );
    let selection_row = first_graphql_row(&config_rows, "ToolSelection")?;
    assert_eq!(
        selection_row
            .get("enable_file_tools")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        selection_row.get("file_tools_mode").and_then(Value::as_str),
        Some("ReadOnly")
    );
    wait_for_runtime_quiescence(&graphql, &agent_did, 2, Duration::from_secs(6)).await?;

    let prompt =
        "Use the read_file tool to read notes.txt. Reply with only the token from that file.";
    let result = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--content",
            prompt,
            "--timeout-secs",
            "60",
            "--poll-secs",
            "1",
        ],
    )?;
    let response = result
        .pointer("/response/content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!("request submit result did not include response content: {result}")
        })?;
    assert_eq!(response, token);

    let captured_requests = mock_endpoint.captured_chat_requests();
    assert!(
        captured_requests.len() >= 2,
        "expected at least two chat completion requests, got {}",
        captured_requests.len()
    );

    let initial_request = captured_requests
        .iter()
        .find(|request| {
            !request_has_tool_result_message(request)
                && request_system_message(request).is_some_and(|system| {
                    system.contains("You have access to these tools")
                        && system.contains("read_file")
                })
        })
        .ok_or_else(|| anyhow!("missing initial chat completion request"))?;
    let tool_result_request = captured_requests
        .iter()
        .find(|request| request_has_tool_result_message(request))
        .ok_or_else(|| anyhow!("missing follow-up chat completion request with tool result"))?;

    assert_eq!(
        initial_request.get("model").and_then(Value::as_str),
        Some(model_name.as_str())
    );
    let initial_tool_names = request_tool_names(initial_request);
    assert!(
        initial_tool_names.contains(&"read_file".to_string()),
        "expected initial request to include read_file, got tools {:?} in request {initial_request}",
        initial_tool_names
    );
    assert!(
        initial_tool_names.contains(&"list_files".to_string()),
        "expected initial request to include list_files, got tools {:?} in request {initial_request}",
        initial_tool_names
    );
    assert!(
        request_system_message(initial_request)
            .is_some_and(|system| system.contains("You have access to these tools")
                && system.contains("read_file")),
        "expected initial system message to advertise direct tools: {initial_request}"
    );

    let followup_tool_names = request_tool_names(tool_result_request);
    assert!(followup_tool_names.contains(&"read_file".to_string()));
    assert!(
        request_tool_result_text(tool_result_request)
            .is_some_and(|content| content.contains(&token)),
        "expected follow-up request to include persisted tool result with token {token}: {tool_result_request}"
    );

    let (_request_id, session_id, behavior_id) =
        wait_for_request(&graphql, &agent_did, prompt).await?;
    assert!(
        !behavior_id.is_empty(),
        "request should be pinned to a behavior"
    );

    let tool_call = wait_for_tool_call(&graphql, &session_id, "read_file").await?;
    assert_eq!(
        tool_call.get("status").and_then(Value::as_str),
        Some("completed")
    );
    assert!(
        tool_call
            .get("result")
            .and_then(Value::as_str)
            .is_some_and(|result| result.contains(&token)),
        "expected persisted tool result to contain token {token}: {tool_call}"
    );

    Ok(())
}
