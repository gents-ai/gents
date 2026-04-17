mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

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
    let agent_did = format!("did:defra-agent:{agent_name}");

    let init_args = vec![
        "--home".to_string(),
        home_arg.to_string(),
        "--agent-name".to_string(),
        agent_name.clone(),
        "--model-name".to_string(),
        DEFAULT_MODEL_NAME.to_string(),
        "--max-concurrent".to_string(),
        "2".to_string(),
        "--max-queue-depth".to_string(),
        "4".to_string(),
        DEFAULT_MODEL_ENDPOINT.to_string(),
    ];
    let init_arg_refs = init_args.iter().map(String::as_str).collect::<Vec<_>>();
    let init = run_init_json(&home_dir, &init_arg_refs)?;
    let backend_id = init
        .pointer("/init/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_id: {init}"))?
        .to_string();
    let backend_name = init
        .pointer("/init/backend_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_name: {init}"))?
        .to_string();
    let endpoint = init
        .pointer("/init/endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing endpoint: {init}"))?
        .to_string();
    let model_name = init
        .pointer("/init/model_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing model_name: {init}"))?
        .to_string();
    let behavior_id = init
        .pointer("/init/default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing default_behavior_id: {init}"))?
        .to_string();
    let selection_id = init
        .pointer("/init/tool_selection_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing tool_selection_id: {init}"))?
        .to_string();

    let (mut serve, readiness) =
        spawn_server_with_ready_json(&home_dir, port, &["--home", home_arg], &[])?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
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
        desktop_init.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        desktop_init.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );
    assert_eq!(
        desktop_init.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    let desktop_next_steps = desktop_init
        .get("next_steps")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("desktop init output missing next_steps: {desktop_init}"))?;
    assert!(
        desktop_next_steps.iter().any(|step| step
            .as_str()
            .is_some_and(|step| step.contains("replication: subscriptions armed"))),
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
        peer.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        peer.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );

    let backend_args = vec![
        "config",
        "backend",
        "set",
        "--graphql",
        &graphql,
        "--backend-id",
        &backend_id,
        "--name",
        &backend_name,
        "--provider-kind",
        "OpenAiCompatible",
        "--endpoint",
        &endpoint,
        "--max-concurrent",
        "2",
        "--max-queue-depth",
        "4",
    ];
    run_cli_json(&home_dir, &backend_args)?;

    run_cli_json(
        &home_dir,
        &[
            "config",
            "tools",
            "set",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--selection-id",
            &selection_id,
            "--display-name",
            "Standard Onboarding Demo Tools",
            "--enable-file-tools",
            "--file-tools-mode",
            "ReadOnly",
            "--file-tool-root",
            home_dir
                .to_str()
                .ok_or_else(|| anyhow!("demo home path is not UTF-8"))?,
            "--enable-bash",
            "--bash-mode",
            "ReadOnly",
        ],
    )?;

    run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "set",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--behavior-id",
            &behavior_id,
            "--display-name",
            "Standard Onboarding Demo",
            "--system-prompt-file",
            system_prompt
                .to_str()
                .ok_or_else(|| anyhow!("system prompt path is not UTF-8"))?,
            "--backend-id",
            &backend_id,
            "--model-name",
            &model_name,
            "--tool-selection-id",
            &selection_id,
        ],
    )?;
    wait_for_runtime_quiescence(&graphql, &agent_did, 2, Duration::from_secs(6)).await?;

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
            "--output-format",
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
        .pointer("/response/content")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("first live chat output missing response content: {first}"))?;
    assert!(
        first_content.contains(&alpha_token),
        "expected first response to contain {alpha_token}, got {first_content}"
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
            "--output-format",
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
        .pointer("/response/content")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("second live chat output missing response content: {second}"))?;
    assert!(
        second_content.contains(&alpha_token) && second_content.contains(&beta_token),
        "expected second response to contain {alpha_token} and {beta_token}, got {second_content}"
    );

    wait_for_completed_tool_calls(&graphql, &session_id, "list_files", 1).await?;
    let read_calls = wait_for_completed_tool_calls(&graphql, &session_id, "read_file", 2).await?;
    let read_results = read_calls
        .iter()
        .filter_map(|row| row.get("result").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        read_results.contains(&alpha_token) && read_results.contains(&beta_token),
        "expected persisted read_file tool results to contain {alpha_token} and {beta_token}: {read_results}"
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
        behavior_id: String,
        prompt: String,
        tokens: Vec<String>,
    }

    let files_dir = home_dir.join("live-smoke-files");
    fs::create_dir_all(&files_dir)?;
    let agent_name = format!("cli-live-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
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
            behavior_id: format!("{agent_did}:live-{request_index}"),
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
    let model_endpoint = std::env::var("DEFRA_AGENT_CLI_E2E_MODEL_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_MODEL_ENDPOINT.to_string());
    let model_name = std::env::var("DEFRA_AGENT_CLI_E2E_MODEL_NAME")
        .unwrap_or_else(|_| DEFAULT_MODEL_NAME.to_string());
    let mut init_args = vec![
        "--agent-name".to_string(),
        agent_name.clone(),
        "--model-name".to_string(),
        model_name.clone(),
        "--max-concurrent".to_string(),
        "4".to_string(),
        "--max-queue-depth".to_string(),
        "8".to_string(),
    ];
    if std::env::var_os("DEFRA_AGENT_CLI_E2E_API_KEY").is_some() {
        init_args.push("--api-key-env-var".to_string());
        init_args.push("DEFRA_AGENT_CLI_E2E_API_KEY".to_string());
    }
    init_args.push(model_endpoint.clone());
    let init_arg_refs = init_args.iter().map(String::as_str).collect::<Vec<_>>();
    let init = run_init_json(&home_dir, &init_arg_refs)?;
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

    for (index, spec) in request_specs.iter().enumerate() {
        let display_name = format!("Live Smoke {}", index + 1);
        run_cli_json(
            &home_dir,
            &[
                "config",
                "behavior",
                "set",
                "--graphql",
                &graphql,
                "--agent-did",
                &agent_did,
                "--behavior-id",
                &spec.behavior_id,
                "--display-name",
                &display_name,
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
    }
    wait_for_runtime_quiescence(&graphql, &agent_did, 2, Duration::from_secs(6)).await?;

    let mut children = Vec::new();
    for spec in &request_specs {
        let child = spawn_cli(
            &home_dir,
            &[
                "request",
                "submit",
                "--graphql",
                &graphql,
                "--agent-did",
                &agent_did,
                "--behavior-id",
                &spec.behavior_id,
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
            Err(error) => wait_errors.push(format!("{}: {error}", spec.behavior_id)),
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
                spec.behavior_id,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                server_stdout,
                server_stderr
            );
        }
        let result: Value = serde_json::from_slice(&output.stdout)
            .with_context(|| format!("parsing live request JSON for {}", spec.behavior_id))?;
        assert_eq!(
            result.get("behavior_id").and_then(Value::as_str),
            Some(spec.behavior_id.as_str())
        );
        let response = result
            .pointer("/response/content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow!("request submit result did not include response content: {result}")
            })?;
        for token in &spec.tokens {
            assert!(
                response.contains(token),
                "expected response for {} to contain token {token}, got {response}",
                spec.behavior_id
            );
        }
        let session_id = result
            .get("session_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("live request result missing session_id: {result}"))?;
        let tool_calls =
            wait_for_completed_tool_calls(&graphql, session_id, "read_file", spec.tokens.len())
                .await?;
        let tool_results = tool_calls
            .iter()
            .filter_map(|row| row.get("result").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        for token in &spec.tokens {
            assert!(
                tool_results.contains(token),
                "expected persisted read_file tool calls for {} to include token {token}: {tool_results}",
                spec.behavior_id
            );
        }
    }

    wait_for_completed_inference_behaviors(
        &graphql,
        &backend_id,
        &request_specs
            .iter()
            .map(|spec| spec.behavior_id.as_str())
            .collect::<Vec<_>>(),
    )
    .await?;

    Ok(())
}
