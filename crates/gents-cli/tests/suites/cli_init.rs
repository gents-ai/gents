use crate::support::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use gents::default_agent_id_for_node;
use serde_json::Value;
use uuid::Uuid;

fn generated_backend_id_for_agent(node_did: &str) -> String {
    format!("{node_did}:backend")
}

fn generated_tools_id_for_agent(node_did: &str) -> String {
    let default_agent_id = default_agent_id_for_node(node_did);
    format!("{default_agent_id}-tools")
}

fn add_principal_skill_pair(root: &Path, node_did: &str) -> Result<()> {
    let config_path = root.join("pack_config.json");
    let mut config = read_json_file(&config_path)?;
    let skills = config
        .as_object_mut()
        .context("exported config is not an object")?
        .entry("skills")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .context("exported skills is not an array")?;
    for suffix in ["alpha", "zeta"] {
        let skill_id = format!("{node_did}:skill-{suffix}");
        skills.push(serde_json::json!({
            "skill_id": skill_id,
            "node_did": node_did,
            "name": format!("Skill {suffix}"),
            "description": null,
            "instructions": format!("Instructions for skill {suffix}."),
            "tool_refs": [],
            "display_name": null,
            "interface_json": null,
            "enabled": true,
        }));
    }
    write_json_file(&config_path, &config)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_bootstraps_backend_default_agent_and_tools_idempotently() -> Result<()> {
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
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
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
    let node_did = node_did_from_init(&init)?;
    let backend_id = generated_backend_id_for_agent(&node_did);
    let tools_id = generated_tools_id_for_agent(&node_did);

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &node_did,
            Duration::from_secs(30),
        ))
        .await
        .context("initial boot readiness")?;

    assert_runtime_init_state(
        &graphql,
        &node_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenAiCompatible",
        None,
        None,
        &model_name,
        &tools_id,
        "ReadOnly",
        "ReadOnly",
        "read-only operating mode",
    )
    .await?;

    drop(serve);

    run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &node_did,
            Duration::from_secs(30),
        ))
        .await
        .context("restart readiness after idempotent init")?;

    assert_runtime_init_state(
        &graphql,
        &node_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenAiCompatible",
        None,
        None,
        &model_name,
        &tools_id,
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

    let agent_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                Agent(filter: {{ node_did: {{ _eq: "{}" }} }}) {{
                    agent_id
                }}
            }}"#,
            escape_graphql_string(&node_did),
        ),
    )
    .await?;
    assert_eq!(
        agent_rows
            .pointer("/data/Agent")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    let tools_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                Tools(filter: {{ tools_id: {{ _eq: "{}" }} }}) {{
                    tools_id
                    self_config
                    built_ins
                }}
            }}"#,
            escape_graphql_string(&tools_id),
        ),
    )
    .await?;
    assert_eq!(
        tools_rows
            .pointer("/data/Tools")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    let tools = first_graphql_row(&tools_rows, "Tools")?;
    assert_eq!(
        tools
            .pointer("/built_ins/enable_schema_tool")
            .and_then(Value::as_bool),
        None
    );
    let preset: Value = serde_json::from_str(gents_protocol::ENGINEER_SELF_CONFIG_JSON)?;
    for (field, expected) in preset.as_object().context("preset object")? {
        assert_eq!(
            tools.pointer(&format!("/self_config/{field}")),
            Some(expected),
            "self_config.{field}"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_apply_root_reports_post_apply_default_readiness() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let pack_root = tempdir.path().join("pack");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-apply-root-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-apply-root-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;

    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            pack_root.to_str().context("pack root utf8")?,
        ],
    )?;
    let config_path = pack_root.join("pack_config.json");
    let mut config = read_json_file(&config_path)?;
    let original_agent_id = config["node"]
        .get("default_agent_id")
        .and_then(Value::as_str)
        .context("exported principal missing default agent")?
        .to_string();
    let applied_agent_id = format!("{node_did}:applied-default");
    let mut applied_agent = config["agents"]
        .as_array()
        .context("agents is not an array")?
        .iter()
        .find(|agent| agent["agent_id"] == original_agent_id)
        .context("exported default agent is missing")?
        .clone();
    applied_agent["agent_id"] = Value::String(applied_agent_id.clone());
    applied_agent["display_name"] = Value::String("Applied default".to_string());
    config["agents"]
        .as_array_mut()
        .context("agents is not an array")?
        .push(applied_agent);
    config["node"]["default_agent_id"] = Value::String(applied_agent_id.clone());
    write_json_file(&config_path, &config)?;

    let port = allocate_port()?;
    let pack_root_arg = pack_root.to_str().context("pack root utf8")?;
    let (serve, readiness) =
        spawn_server_with_ready_json(&home_dir, port, &["--apply-root", pack_root_arg], &[])?;

    assert_eq!(
        readiness.get("default_agent_id").and_then(Value::as_str),
        Some(applied_agent_id.as_str())
    );
    assert_eq!(
        readiness
            .pointer("/node_readiness/default_agent_id")
            .and_then(Value::as_str),
        Some(applied_agent_id.as_str())
    );
    assert!(readiness
        .pointer("/node_readiness/agents")
        .and_then(Value::as_array)
        .is_some_and(|agents| agents.iter().any(|agent| {
            agent.get("agent_id").and_then(Value::as_str) == Some(applied_agent_id.as_str())
        })));
    assert_eq!(
        readiness
            .pointer("/apply_root/changed")
            .and_then(Value::as_bool),
        Some(true)
    );

    drop(serve);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_apply_root_accepts_metadata_only_change_without_generation_advance() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let pack_root = tempdir.path().join("pack");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-metadata-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &format!("metadata-apply-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            pack_root.to_str().context("pack root utf8")?,
        ],
    )?;
    add_principal_skill_pair(&pack_root, &node_did)?;
    run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            pack_root.to_str().context("pack root utf8")?,
        ],
    )?;
    let config_path = pack_root.join("pack_config.json");
    let mut config = read_json_file(&config_path)?;
    config["node"]["display_name"] = Value::String("Metadata-only rename".to_string());
    write_json_file(&config_path, &config)?;

    let port = allocate_port()?;
    let (serve, readiness) = spawn_server_with_ready_json(
        &home_dir,
        port,
        &[
            "--apply-root",
            pack_root.to_str().context("pack root utf8")?,
        ],
        &[],
    )?;

    assert_eq!(
        readiness
            .pointer("/node_readiness/active_generation")
            .and_then(Value::as_u64),
        Some(1),
        "principal display name is not part of runtime operational identity",
    );
    drop(serve);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_apply_root_waits_for_task_only_runtime_generation() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let pack_root = tempdir.path().join("pack");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-task-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &format!("task-apply-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            pack_root.to_str().context("pack root utf8")?,
        ],
    )?;
    add_principal_skill_pair(&pack_root, &node_did)?;
    run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            pack_root.to_str().context("pack root utf8")?,
        ],
    )?;
    let config_path = pack_root.join("pack_config.json");
    let mut config = read_json_file(&config_path)?;
    let agent_id = config["node"]
        .get("default_agent_id")
        .and_then(Value::as_str)
        .context("exported principal missing default agent")?
        .to_string();
    let task_id = format!("post-apply-task-{}", Uuid::new_v4().simple());
    config
        .as_object_mut()
        .context("exported config is not an object")?
        .entry("tasks")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .context("tasks is not an array")?
        .push(serde_json::json!({
            "task_id": task_id,
            "display_name": "Post-apply task",
            "description": null,
            "agent_id": agent_id,
            "prompt_template": "Exercise the post-apply runtime generation.",
            "enabled": true,
            "output_schema_ref": null,
        }));
    write_json_file(&config_path, &config)?;

    let port = allocate_port()?;
    let (serve, readiness) = spawn_server_with_ready_json(
        &home_dir,
        port,
        &[
            "--apply-root",
            pack_root.to_str().context("pack root utf8")?,
        ],
        &[],
    )?;

    assert!(
        readiness
            .pointer("/node_readiness/active_generation")
            .and_then(Value::as_u64)
            .is_some_and(|generation| generation > 1),
        "task-only apply must wait for a newer runtime generation: {readiness}",
    );
    drop(serve);
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
            "--node-name",
            &agent_name,
            "--provider-kind",
            "OpenRouter",
            "--api-key",
            raw_api_key,
            "--model-name",
            &model_name,
            "--inference-url",
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
    let node_did = node_did_from_init(&init)?;
    let backend_id = generated_backend_id_for_agent(&node_did);
    let tools_id = generated_tools_id_for_agent(&node_did);

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &node_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenRouter",
        Some(raw_api_key),
        None,
        &model_name,
        &tools_id,
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
            "--node-name",
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
fn init_hosted_preset_error_can_retry_with_the_same_identity_and_store_key() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let output = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .arg("init")
        .args(["--store-key-custody", "file"])
        .args(["--node-name", "retry-original-identity"])
        .arg("--backend-preset")
        .arg("openai")
        .output()
        .context("running gents init with hosted preset and no model")?;

    assert!(!output.status.success(), "init should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--model-name is required for --backend-preset openai"),
        "expected missing model error, got:\n{stderr}"
    );

    let runtime_home = home_dir.join(".gents");
    let stored = read_json_file(&runtime_home.join("init.json"))?;
    let node_did = stored["node_did"].as_str().context("recorded identity")?;
    let identity_path = stored["key_path"].as_str().context("recorded key path")?;
    let identity_bytes = fs::read(identity_path)?;
    let store_key_path = gents::store::local::store_key::home_key_file(&runtime_home);
    let store_key_bytes = fs::read(&store_key_path)?;
    assert!(runtime_home.join("data/MANIFEST").exists());

    let retried = run_init_json(&home_dir, &["--model-name", "retry-model"])?;
    assert_eq!(retried["node_did"], node_did);
    assert_eq!(fs::read(identity_path)?, identity_bytes);
    assert_eq!(fs::read(store_key_path)?, store_key_bytes);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_defaults_to_local_llama_server_and_surfaces_identity() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let agent_name = format!("cli-defaults-{}", Uuid::new_v4().simple());
    let init = run_init_json(&home_dir, &["--node-name", &agent_name])?;

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
            .any(|step| step.as_str() == Some("gents codex")),
        "init should point at the codex subcommand: {init}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_identity_only_writes_stable_real_did_without_runtime_config() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let agent_name = "store";
    let first = run_init_json(&home_dir, &["--identity-only", "--node-name", agent_name])?;
    let first_node_did = node_did_from_init(&first)?;
    let identity_path =
        gents::store::local::home::default_key_path(&home_dir.join(".gents"), agent_name);
    let identity_bytes = fs::read(&identity_path)?;
    assert_ne!(
        identity_path,
        gents::store::local::store_key::home_key_file(&home_dir.join(".gents"))
    );
    assert_eq!(identity_bytes.len(), 64);

    assert_eq!(
        first.get("identity_only").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(first.get("init"), Some(&Value::Null));

    let second = run_init_json(&home_dir, &["--identity-only", "--node-name", agent_name])?;
    let second_node_did = node_did_from_init(&second)?;
    assert_eq!(second_node_did, first_node_did);
    assert_eq!(fs::read(identity_path)?, identity_bytes);

    let init_json = read_json_file(&home_dir.join(".gents").join("init.json"))?;
    assert_eq!(
        init_json.get("node_did").and_then(Value::as_str),
        Some(first_node_did.as_str())
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
        .args(["--store-key-custody", "file"])
        .arg("--model-name")
        .arg("test-model")
        .arg("--api-key")
        .arg("raw-key")
        .arg("--api-key-env-var")
        .arg("TEST_BACKEND_KEY")
        .arg("--inference-url")
        .arg("http://127.0.0.1:65535/v1")
        .output()
        .context("running gents init with conflicting backend auth flags")?;

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
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;

    let runtime_home = home_dir.join(".gents");
    let stale_path = runtime_home.join("stale.txt");
    fs::write(&stale_path, "stale").context("writing stale file into runtime home")?;
    assert!(stale_path.exists(), "expected stale file to exist");

    run_init_json(
        &home_dir,
        &[
            "--dangerously-overwrite",
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
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
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--backend-id",
            &backend_id,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let tools_id = generated_tools_id_for_agent(&node_did);
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &node_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenAiCompatible",
        None,
        None,
        &model_name,
        &tools_id,
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
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--tool-root",
            readonly_root.to_str().expect("utf-8 readonly root"),
            "--inference-url",
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

async fn pin_default_tools_root(
    home_dir: &Path,
    node_did: &str,
    root: &str,
    scratch_dir: &Path,
) -> Result<()> {
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut serve = spawn_server_with_env(home_dir, port, &[], &[])?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, node_did, Duration::from_secs(30)).await?;
    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{ Tools(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ tools_id node_did display_name host remote agents built_ins datastore integrations self_config tags }} }}"#,
            gents::store::graphql::escape_graphql_string(node_did)
        ),
    )
    .await?;
    let tools_id = generated_tools_id_for_agent(node_did);
    let mut tools = response
        .pointer("/data/Tools")
        .and_then(Value::as_array)
        .context("Tools rows")?
        .iter()
        .map(|row| {
            row.as_object()
                .expect("Tools row object")
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(field, value)| {
                    let value = value
                        .as_str()
                        .and_then(|encoded| serde_json::from_str::<Value>(encoded).ok())
                        .filter(|decoded| decoded.is_object())
                        .unwrap_or_else(|| value.clone());
                    (field.clone(), value)
                })
                .collect::<serde_json::Map<_, _>>()
        })
        .find(|row| row.get("tools_id").and_then(Value::as_str) == Some(&tools_id))
        .context("the Tools document init created for the default agent")?;
    tools["host"]["root"] = Value::from(root);
    let tools_path = scratch_dir.join("tools.json");
    write_json_file(&tools_path, &Value::Object(tools))?;
    run_cli_json(
        home_dir,
        &[
            "config",
            "tools",
            "set",
            "--graphql",
            &graphql,
            "--file",
            tools_path.to_str().context("utf-8 tools path")?,
        ],
    )?;
    serve.child.kill()?;
    serve.child.wait()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_startup_logs_the_tool_root_that_escapes_the_ceiling() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let recorded_root = tempdir.path().join("recorded-root");
    let explicit_root = tempdir.path().join("explicit-root");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&recorded_root)?;
    fs::create_dir_all(&explicit_root)?;
    let recorded_root = recorded_root.to_str().context("utf-8 recorded root")?;
    let explicit_root = explicit_root.to_str().context("utf-8 explicit root")?;

    let model_name = format!("tool-root-conflict-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-tool-root-conflict-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--tool-root",
            recorded_root,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    pin_default_tools_root(&home_dir, &node_did, recorded_root, tempdir.path()).await?;

    let port = allocate_port()?;
    let stderr = run_cli_failure_stderr(
        &home_dir,
        &[
            "server",
            "--http-port",
            &port.to_string(),
            "--tool-root",
            explicit_root,
        ],
    )?;

    assert!(
        stderr.contains("escapes operator tool root"),
        "expected startup to log the runtime's root-admission diagnostic, got:\n{stderr}"
    );
    assert!(
        stderr.contains(recorded_root) && stderr.contains(explicit_root),
        "expected the diagnostic to name both the Tools root and --tool-root, got:\n{stderr}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_service_mode_writes_the_startup_diagnostic_to_stderr() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let recorded_root = tempdir.path().join("recorded-root");
    let explicit_root = tempdir.path().join("explicit-root");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&recorded_root)?;
    fs::create_dir_all(&explicit_root)?;
    let recorded_root = recorded_root.to_str().context("utf-8 recorded root")?;
    let explicit_root = explicit_root.to_str().context("utf-8 explicit root")?;

    let model_name = format!("service-diagnostic-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-service-diagnostic-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--tool-root",
            recorded_root,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    pin_default_tools_root(&home_dir, &node_did, recorded_root, tempdir.path()).await?;

    let port = allocate_port()?;
    let stderr = run_cli_failure_stderr_with_env(
        &home_dir,
        &[
            "server",
            "--http-port",
            &port.to_string(),
            "--tool-root",
            explicit_root,
        ],
        &[("GENTS_SYSTEM_LOG", "1")],
    )?;

    assert!(
        stderr.contains("escapes operator tool root"),
        "expected the service-mode stderr sink to carry the root-admission diagnostic, got:\n{stderr}"
    );
    assert!(
        stderr.contains(recorded_root) && stderr.contains(explicit_root),
        "expected the mirrored diagnostic to name both the Tools root and --tool-root, got:\n{stderr}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_admits_a_narrower_tool_root_after_the_live_root_moved() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let workspace_root = tempdir.path().join("workspace");
    let project_root = workspace_root.join("project");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&project_root)?;
    let workspace_root = workspace_root.to_str().context("utf-8 workspace root")?;
    let project_root = project_root.to_str().context("utf-8 project root")?;

    let model_name = format!("moved-tool-root-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-moved-tool-root-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--tool-root",
            workspace_root,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    pin_default_tools_root(&home_dir, &node_did, project_root, tempdir.path()).await?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut serve = spawn_server_with_env(
        &home_dir,
        port,
        &["--tool-ceiling", "readonly", "--tool-root", project_root],
        &[],
    )?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_clamps_the_default_tools_root_to_a_narrowed_operator_root() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let workspace_root = tempdir.path().join("workspace");
    let project_root = workspace_root.join("project");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&project_root)?;
    let workspace_root = workspace_root.to_str().context("utf-8 workspace root")?;
    let project_root = project_root.to_str().context("utf-8 project root")?;

    let model_name = format!("clamped-tool-root-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-clamped-tool-root-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--tool-root",
            workspace_root,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    pin_default_tools_root(&home_dir, &node_did, workspace_root, tempdir.path()).await?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut serve = spawn_server_with_env(
        &home_dir,
        port,
        &["--tool-ceiling", "readwrite", "--tool-root", project_root],
        &[],
    )?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_pins_the_tool_root_under_an_explicit_workspace_root_policy() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let workspace_root = tempdir.path().join("workspace");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&workspace_root)?;
    let workspace_root = workspace_root.to_str().context("utf-8 workspace root")?;

    let expected_reply = format!("pinned-root-{}", Uuid::new_v4().simple());
    let model_name = format!("pinned-root-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;
    let agent_name = format!("cli-pinned-root-{}", Uuid::new_v4().simple());
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init_args = [
        "--node-name",
        agent_name.as_str(),
        "--model-name",
        model_name.as_str(),
        "--tool-root",
        workspace_root,
        "--inference-url",
        mock_endpoint.endpoint(),
    ];

    let init = run_init_json(&home_dir, &init_args)?;
    let node_did = node_did_from_init(&init)?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    run_cli_json(
        &home_dir,
        &[
            "config",
            "workspace-root",
            "set",
            "--graphql",
            &graphql,
            "--path",
            workspace_root,
        ],
    )?;
    serve.child.kill()?;
    serve.child.wait()?;

    run_init_json(&home_dir, &init_args)?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;
    let reply = run_cli_text(
        &home_dir,
        &["chat", "Reply with exactly the configured token."],
    )?;
    assert!(
        reply.contains(&expected_reply),
        "expected the pinned-root chat round trip to deliver the model reply, got:\n{reply}"
    );
    let tools_id = generated_tools_id_for_agent(&node_did);
    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{ Tools(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ tools_id host }} }}"#,
            gents::store::graphql::escape_graphql_string(&node_did)
        ),
    )
    .await?;
    serve.child.kill()?;
    serve.child.wait()?;

    let pinned_root = response
        .pointer("/data/Tools")
        .and_then(Value::as_array)
        .context("Tools rows")?
        .iter()
        .find(|row| row.get("tools_id").and_then(Value::as_str) == Some(&tools_id))
        .and_then(|row| {
            let host = row.get("host")?;
            let decoded = host
                .as_str()
                .and_then(|encoded| serde_json::from_str::<Value>(encoded).ok())
                .unwrap_or_else(|| host.clone());
            decoded.get("root").cloned()
        })
        .and_then(|root| root.as_str().map(str::to_string))
        .context("the Tools document init re-authored under explicit policy")?;
    assert_eq!(
        pinned_root, workspace_root,
        "init must pin the resolved operator root into Tools.host.root under explicit policy"
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
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--write",
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    assert_eq!(
        init.pointer("/init/tool_ceiling").and_then(Value::as_str),
        Some("Readwrite")
    );
    let node_did = node_did_from_init(&init)?;
    let backend_id = generated_backend_id_for_agent(&node_did);
    let tools_id = generated_tools_id_for_agent(&node_did);

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &node_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenAiCompatible",
        None,
        None,
        &model_name,
        &tools_id,
        "ReadWrite",
        "Unrestricted",
        "write-capable local tools",
    )
    .await?;
    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                Tools(filter: {{ tools_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    host
                }}
            }}"#,
            escape_graphql_string(&tools_id)
        ),
    )
    .await?;
    let tools = first_graphql_row(&response, "Tools")?;
    let expected_command_policy = if cfg!(target_os = "macos") {
        Some("workspace_write")
    } else {
        Some("unrestricted")
    };
    assert_eq!(
        tools
            .pointer("/host/bash/execution_mode")
            .and_then(Value::as_str),
        expected_command_policy
    );
    assert_eq!(
        tools
            .pointer("/host/bash/background_enabled")
            .and_then(Value::as_bool),
        Some(true)
    );

    Ok(())
}

#[tokio::test]
async fn readiness_wait_survives_preflight_listener_closing_before_server_start() -> Result<()> {
    let port = allocate_port()?;
    let endpoint = graphql_url(port);
    let did = "did:key:readiness-startup";
    let mut ready = Box::pin(wait_for_runtime_ready(
        &endpoint,
        did,
        Duration::from_secs(5),
    ));
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut ready)
        .await
        .is_err());
    let snapshot = serde_json::json!({
        "format_version": gents_protocol::node_readiness::NODE_READINESS_FORMAT_VERSION, "process_state": "ready",
        "active_generation": 1, "router_generation": 1,
        "default_agent_id": "default",
        "agents": [{"agent_id": "default", "state": "ready", "reason": null}]
    });
    let response = serde_json::json!({"data": {"NodeReadiness": [{
        "node_did": did, "snapshot_json": snapshot.to_string(),
        "updated_at": chrono::Utc::now().to_rfc3339()
    }]}});
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let app = axum::Router::new().route(
        "/api/v0/graphql",
        axum::routing::post(move || async move { axum::Json(response) }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let result = ready.await;
    server.abort();
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_reuses_recorded_store_custody_and_refuses_an_unrecorded_key() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let home = home_dir.join(".gents");
    let agent_name = format!("cli-store-key-{}", Uuid::new_v4().simple());

    run_init_json(&home_dir, &["--identity-only", "--node-name", &agent_name])?;
    let config_path = gents::store::local::home::init_config_path(&home);
    let recorded = read_json_file(&config_path)?["store_encryption"].clone();
    assert_eq!(
        recorded,
        serde_json::json!({"version": 1, "custody": "file"}),
        "{recorded}"
    );
    let key_file = gents::store::local::store_key::home_key_file(&home);
    let key = fs::read(&key_file)?;
    run_init_json(&home_dir, &["--identity-only", "--node-name", &agent_name])?;
    assert_eq!(fs::read(&key_file)?, key, "re-init reuses the recorded key");

    let mut legacy = read_json_file(&config_path)?;
    legacy
        .as_object_mut()
        .context("init.json object")?
        .remove("store_encryption");
    write_json_file(&config_path, &legacy)?;
    let output = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .args(["config", "export", "--root"])
        .arg(tempdir.path().join("export"))
        .output()
        .context("opening a home with unrecorded store custody")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("unrecorded file"), "{stderr}");
    assert!(stderr.contains("refusing to replace"), "{stderr}");
    assert_eq!(fs::read(&key_file)?, key);
    assert_eq!(read_json_file(&config_path)?, legacy);
    assert!(
        !home.join("data/MANIFEST").exists(),
        "the store is never opened"
    );
    Ok(())
}

#[test]
fn init_refuses_an_explicit_identity_path_that_collides_with_the_store_key() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home_dir = temp.path().join("home");
    let key_path = gents::store::local::store_key::home_key_file(&home_dir.join(".gents"));
    gents::store::identity::load_or_create_file_identity(&key_path)?;
    let original = fs::read(&key_path)?;
    let error = run_init_json(
        &home_dir,
        &[
            "--identity-only",
            "--key-path",
            key_path.to_str().context("key path")?,
        ],
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("refusing to replace"),
        "{error:#}"
    );
    assert_eq!(fs::read(&key_path)?, original);
    gents::store::identity::load_file_identity(&key_path)?;
    assert!(!home_dir.join(".gents/init.json").exists());
    Ok(())
}
