// Soft-cap justified: 9 init scenarios share setup; splitting would fragment the init story across binaries.
mod support;
use support::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use defra_agent::{default_behavior_id_for_agent, default_tool_selection_id_for_behavior};
use serde_json::Value;
use uuid::Uuid;

fn generated_backend_id_for_agent(agent_did: &str) -> String {
    format!("{agent_did}:backend")
}

fn generated_tool_selection_id_for_agent(agent_did: &str) -> String {
    let default_behavior_id = default_behavior_id_for_agent(agent_did);
    default_tool_selection_id_for_behavior(&default_behavior_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_bootstraps_backend_default_behavior_and_tool_selection_idempotently() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-init-{}", Uuid::new_v4().simple());
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
    assert_eq!(
        init.get("status").and_then(Value::as_str),
        Some("initialized")
    );
    assert_eq!(
        init.pointer("/init/tool_ceiling").and_then(Value::as_str),
        Some("Readonly")
    );
    let agent_did = agent_did_from_init(&init)?;
    let backend_id = generated_backend_id_for_agent(&agent_did);
    let tool_selection_id = generated_tool_selection_id_for_agent(&agent_did);

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenAiCompatible",
        None,
        None,
        &model_name,
        &tool_selection_id,
        "ReadOnly",
        "ReadOnly",
        "read-only operating mode",
    )
    .await?;

    drop(serve);

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
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenAiCompatible",
        None,
        None,
        &model_name,
        &tool_selection_id,
        "ReadOnly",
        "ReadOnly",
        "read-only operating mode",
    )
    .await?;

    let backend_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                InferenceBackend(filter: {{ backend_id: {{ _eq: "{}" }} }}) {{
                    backend_id
                }}
            }}"#,
            escape_graphql_string(&backend_id),
        ),
    )
    .await?;
    assert_eq!(
        backend_rows
            .pointer("/data/InferenceBackend")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    let behavior_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentBehavior(filter: {{ agent_did: {{ _eq: "{}" }} }}) {{
                    behavior_id
                }}
            }}"#,
            escape_graphql_string(&agent_did),
        ),
    )
    .await?;
    assert_eq!(
        behavior_rows
            .pointer("/data/AgentBehavior")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    let selection_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}) {{
                    selection_id
                }}
            }}"#,
            escape_graphql_string(&tool_selection_id),
        ),
    )
    .await?;
    assert_eq!(
        selection_rows
            .pointer("/data/ToolSelection")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_supports_provider_auth_backend_fields() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-openrouter-model-{}", Uuid::new_v4().simple());
    let raw_api_key = "openrouter-raw-key";
    let mock_endpoint = MockChatEndpoint::start_with_required_bearer(
        &model_name,
        "OPENROUTER_BACKEND_OK",
        Some(raw_api_key),
    )?;

    let port = allocate_port()?;
    let agent_name = format!("cli-openrouter-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--provider-kind",
            "OpenRouter",
            "--api-key",
            raw_api_key,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    assert_eq!(
        init.pointer("/init/provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );
    assert_eq!(
        init.pointer("/init/api_key").and_then(Value::as_str),
        Some("<redacted>")
    );
    let agent_did = agent_did_from_init(&init)?;
    let backend_id = generated_backend_id_for_agent(&agent_did);
    let tool_selection_id = generated_tool_selection_id_for_agent(&agent_did);

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenRouter",
        Some(raw_api_key),
        None,
        &model_name,
        &tool_selection_id,
        "ReadOnly",
        "ReadOnly",
        "read-only operating mode",
    )
    .await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_openrouter_preset_applies_hosted_defaults() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let agent_name = format!("cli-openrouter-preset-{}", Uuid::new_v4().simple());
    let model_name = format!("openrouter-model-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--backend-preset",
            "openrouter",
            "--model-name",
            &model_name,
        ],
    )?;

    assert_eq!(
        init.pointer("/init/provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );
    assert_eq!(
        init.pointer("/init/endpoint").and_then(Value::as_str),
        Some("https://openrouter.ai/api/v1")
    );
    assert_eq!(
        init.pointer("/init/api_key_env_var")
            .and_then(Value::as_str),
        Some("OPENROUTER_API_KEY")
    );

    Ok(())
}

#[test]
fn init_hosted_preset_without_default_requires_model_name() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let output = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .arg("init")
        .arg("--backend-preset")
        .arg("openai")
        .output()
        .context("running defra-agent init with hosted preset and no model")?;

    assert!(!output.status.success(), "init should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--model-name is required for --backend-preset openai"),
        "expected missing model error, got:\n{stderr}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_defaults_to_local_llama_server_and_surfaces_identity() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let agent_name = format!("cli-defaults-{}", Uuid::new_v4().simple());
    let init = run_init_json(&home_dir, &["--agent-name", &agent_name])?;

    assert_eq!(
        init.pointer("/init/endpoint").and_then(Value::as_str),
        Some("http://127.0.0.1:8080/v1")
    );
    assert_eq!(
        init.pointer("/init/model_name").and_then(Value::as_str),
        Some("google/gemma-4-12B-it-qat-q4_0-gguf")
    );
    let key_path = init
        .get("key_path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing key_path: {init}"))?;
    assert!(
        Path::new(key_path).exists(),
        "init should create the identity key at {key_path}"
    );
    assert!(
        init.pointer("/identity/permission_boundary")
            .and_then(Value::as_str)
            .is_some_and(|value| value.contains("permission boundary")),
        "init should explain the identity boundary: {init}"
    );
    let next_steps = init
        .get("next_steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        next_steps.iter().any(|step| {
            step.as_str() == Some("llama-server -hf google/gemma-4-12B-it-qat-q4_0-gguf")
        }),
        "init should print the default llama-server next step: {init}"
    );
    assert!(
        next_steps
            .iter()
            .any(|step| step.as_str() == Some("defra-agent codex")),
        "init should point at the codex subcommand: {init}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_identity_only_writes_stable_real_did_without_runtime_config() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let agent_name = format!("cli-identity-only-{}", Uuid::new_v4().simple());
    let first = run_init_json(&home_dir, &["--identity-only", "--agent-name", &agent_name])?;
    let first_agent_did = agent_did_from_init(&first)?;
    assert_eq!(
        first.get("identity_only").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(first.get("init"), Some(&Value::Null));

    let second = run_init_json(&home_dir, &["--identity-only", "--agent-name", &agent_name])?;
    let second_agent_did = agent_did_from_init(&second)?;
    assert_eq!(second_agent_did, first_agent_did);

    let init_json = read_json_file(&home_dir.join(".defra-agent").join("init.json"))?;
    assert_eq!(
        init_json.get("agent_did").and_then(Value::as_str),
        Some(first_agent_did.as_str())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_rejects_setting_both_api_key_and_api_key_env_var() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let output = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .arg("init")
        .arg("--model-name")
        .arg("test-model")
        .arg("--api-key")
        .arg("raw-key")
        .arg("--api-key-env-var")
        .arg("TEST_BACKEND_KEY")
        .arg("http://127.0.0.1:65535/v1")
        .output()
        .context("running defra-agent init with conflicting backend auth flags")?;

    assert!(!output.status.success(), "init should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("provide either --api-key or --api-key-env-var, not both"),
        "expected conflicting auth error, got:\n{stderr}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_dangerously_overwrite_replaces_existing_home() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("overwrite-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-overwrite-{}", Uuid::new_v4().simple());

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
    let stale_path = runtime_home.join("stale.txt");
    fs::write(&stale_path, "stale").context("writing stale file into runtime home")?;
    assert!(stale_path.exists(), "expected stale file to exist");

    run_init_json(
        &home_dir,
        &[
            "--dangerously-overwrite",
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;

    assert!(
        !stale_path.exists(),
        "dangerously overwrite should remove stale files in the runtime home"
    );
    assert!(
        runtime_home.join("init.json").exists(),
        "init config should be recreated after dangerously overwrite"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_accepts_explicit_backend_and_model_together() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("explicit-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-explicit-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let backend_id = format!("{agent_name}-custom-backend");

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--backend-id",
            &backend_id,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let tool_selection_id = generated_tool_selection_id_for_agent(&agent_did);
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenAiCompatible",
        None,
        None,
        &model_name,
        &tool_selection_id,
        "ReadOnly",
        "ReadOnly",
        "read-only operating mode",
    )
    .await?;

    Ok(())
}

#[test]
fn init_accepts_tool_root_for_readonly_defaults() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let readonly_root = tempdir.path().join("readonly-root");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&readonly_root)?;

    let model_name = format!("readonly-root-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-readonly-root-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--tool-root",
            readonly_root.to_str().expect("utf-8 readonly root"),
            mock_endpoint.endpoint(),
        ],
    )?;

    assert_eq!(
        init.pointer("/init/tool_ceiling").and_then(Value::as_str),
        Some("Readonly")
    );
    assert_eq!(
        init.pointer("/init/tool_root").and_then(Value::as_str),
        Some(readonly_root.to_str().expect("utf-8 readonly root"))
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_with_write_tools_bootstraps_write_defaults() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("write-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-write-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--write-tools",
            mock_endpoint.endpoint(),
        ],
    )?;
    assert_eq!(
        init.pointer("/init/tool_ceiling").and_then(Value::as_str),
        Some("Readwrite")
    );
    let agent_did = agent_did_from_init(&init)?;
    let backend_id = generated_backend_id_for_agent(&agent_did);
    let tool_selection_id = generated_tool_selection_id_for_agent(&agent_did);

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenAiCompatible",
        None,
        None,
        &model_name,
        &tool_selection_id,
        "ReadWrite",
        "Unrestricted",
        "write-capable local tools",
    )
    .await?;
    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    command_execution_policy
                    backgroundable_tool_names
                }}
            }}"#,
            escape_graphql_string(&tool_selection_id)
        ),
    )
    .await?;
    let tool_selection = first_graphql_row(&response, "ToolSelection")?;
    let expected_command_policy = if cfg!(target_os = "macos") {
        Some("workspace_write")
    } else {
        Some("unrestricted")
    };
    assert_eq!(
        tool_selection
            .get("command_execution_policy")
            .and_then(Value::as_str),
        expected_command_policy
    );
    assert_eq!(
        tool_selection
            .get("backgroundable_tool_names")
            .and_then(Value::as_array)
            .map(|values| { values.iter().filter_map(Value::as_str).collect::<Vec<_>>() }),
        Some(vec!["bash_unrestricted"])
    );

    Ok(())
}
