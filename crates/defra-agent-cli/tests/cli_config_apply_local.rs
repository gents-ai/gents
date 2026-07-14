mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_updates_backend_from_fresh_init_home_locally() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-apply-local-backend-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-apply-local-backend-{}", Uuid::new_v4().simple());

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

    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;

    let backends_dir = root.join("inference-backends");
    let backend_entry = fs::read_dir(&backends_dir)
        .context("reading inference-backends dir after export")?
        .next()
        .ok_or_else(|| anyhow!("no inference-backend subdirs after export"))??;
    let backend_id = backend_entry
        .file_name()
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 backend dir name"))?
        .to_string();
    let backends_path = root
        .join("inference-backends")
        .join(&backend_id)
        .join("object.json");
    let mut backend = read_json_file(&backends_path)?;
    let updated_endpoint = "http://127.0.0.1:9100/v1";
    backend["endpoint"] = Value::String(updated_endpoint.to_string());
    write_json_file(&backends_path, &backend)?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let explicit_home = home_dir.join(".defra-agent");
    let explicit_home_str = explicit_home
        .to_str()
        .ok_or_else(|| anyhow!("explicit home path is not UTF-8"))?;
    let applied = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(applied.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(applied.get("changed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        applied
            .pointer("/applied/inference_backends")
            .and_then(Value::as_u64),
        Some(1)
    );

    let reexport_root = tempdir.path().join("reexport");
    let reexport_root_str = reexport_root
        .to_str()
        .ok_or_else(|| anyhow!("reexport root path is not UTF-8"))?;
    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            reexport_root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    let reexported_backend = read_json_file(
        &reexport_root
            .join("inference-backends")
            .join(&backend_id)
            .join("object.json"),
    )?;
    assert_eq!(
        reexported_backend.get("endpoint").and_then(Value::as_str),
        Some(updated_endpoint)
    );

    let noop = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    assert_eq!(noop.get("status").and_then(Value::as_str), Some("noop"));
    assert_eq!(noop.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(noop.get("changed").and_then(Value::as_bool), Some(false));
    assert_eq!(
        noop.pointer("/applied/inference_backends")
            .and_then(Value::as_u64),
        Some(0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_prunes_live_only_tasks_and_schedules_when_requested_locally() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-prune-local-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-prune-local-{}", Uuid::new_v4().simple());

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

    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;
    let principal = read_json_file(&root.join("agent-principal.json"))?;
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing default_behavior_id after export"))?
        .to_string();

    let task_id = format!("prune-task-{}", Uuid::new_v4().simple());
    let schedule_id = format!("prune-schedule-{}", Uuid::new_v4().simple());
    let task_dir = root.join("tasks").join(&task_id);
    let schedule_dir = root.join("schedules").join(&schedule_id);
    write_json_file(
        &task_dir.join("object.json"),
        &serde_json::json!({
            "task_id": task_id.clone(),
            "name": "Prune Me",
            "description": "Temporary task for prune regression coverage.",
            "behavior_id": behavior_id.clone(),
            "prompt_template": "Summarize stale config.",
            "enabled": false,
        }),
    )?;
    write_json_file(
        &schedule_dir.join("object.json"),
        &serde_json::json!({
            "schedule_id": schedule_id.clone(),
            "task_id": task_id.clone(),
            "interval_secs": 3600,
            "enabled": false,
            "concurrency": "serial",
        }),
    )?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let explicit_home = home_dir.join(".defra-agent");
    let explicit_home_str = explicit_home
        .to_str()
        .ok_or_else(|| anyhow!("explicit home path is not UTF-8"))?;

    let created = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    assert_eq!(
        created.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(
        created.pointer("/applied/tasks").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        created
            .pointer("/applied/schedules")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        created.pointer("/pruned/tasks").and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        created.pointer("/pruned/schedules").and_then(Value::as_u64),
        Some(0)
    );

    fs::remove_dir_all(&schedule_dir)
        .with_context(|| format!("removing {}", schedule_dir.display()))?;
    fs::remove_dir_all(&task_dir).with_context(|| format!("removing {}", task_dir.display()))?;

    let drift = run_cli_json(
        &home_dir,
        &[
            "config",
            "diff",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    assert_eq!(drift.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        drift
            .pointer("/counts/tasks/live_only")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        drift
            .pointer("/counts/schedules/live_only")
            .and_then(Value::as_u64),
        Some(1)
    );

    let no_prune = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    assert_eq!(no_prune.get("status").and_then(Value::as_str), Some("noop"));
    assert_eq!(
        no_prune.get("changed").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        no_prune.get("exact_match").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        no_prune.pointer("/pruned/tasks").and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        no_prune
            .pointer("/remaining/tasks/live_only")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        no_prune
            .pointer("/remaining/schedules/live_only")
            .and_then(Value::as_u64),
        Some(1)
    );

    let first_prune = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
            "--prune",
        ],
    )?;
    assert_eq!(
        first_prune.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(
        first_prune.get("changed").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        first_prune.get("exact_match").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        first_prune
            .pointer("/planned/tasks/delete")
            .and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        first_prune
            .pointer("/planned/schedules/delete")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        first_prune
            .pointer("/applied/tasks")
            .and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        first_prune.pointer("/pruned/tasks").and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        first_prune
            .pointer("/pruned/schedules")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        first_prune
            .pointer("/remaining/tasks/live_only")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        first_prune
            .pointer("/remaining/schedules/live_only")
            .and_then(Value::as_u64),
        Some(0)
    );

    let pruned = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
            "--prune",
        ],
    )?;
    assert_eq!(
        pruned.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(pruned.get("changed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        pruned.get("exact_match").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        pruned
            .pointer("/planned/tasks/delete")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        pruned.pointer("/pruned/tasks").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        pruned.pointer("/pruned/schedules").and_then(Value::as_u64),
        Some(0)
    );

    let exact = run_cli_json(
        &home_dir,
        &[
            "config",
            "diff",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    assert_eq!(exact.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        exact
            .pointer("/counts/tasks/live_only")
            .and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        exact
            .pointer("/counts/schedules/live_only")
            .and_then(Value::as_u64),
        Some(0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_rebinds_placeholder_manifest_to_home_identity_locally() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let source_home_env = tempdir.path().join("source-home-env");
    let target_home_env = tempdir.path().join("target-home-env");
    let root = tempdir
        .path()
        .join("infra")
        .join("agents")
        .join("mini-1")
        .join("mini-1-steward");
    fs::create_dir_all(&source_home_env)?;
    fs::create_dir_all(&target_home_env)?;

    let placeholder_did = "did:defra-agent:mini-1-steward";
    let agent_name = "mini-1-steward";
    let model_name = format!("mock-rebind-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let source_init = run_init_json(
        &source_home_env,
        &[
            "--agent-name",
            agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let source_agent_did = agent_did_from_init(&source_init)?;
    assert_ne!(source_agent_did, placeholder_did);

    run_cli_text(
        &source_home_env,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;
    rewrite_manifest_agent_dids(&root, placeholder_did)?;

    let target_init = run_init_json(
        &target_home_env,
        &["--identity-only", "--agent-name", agent_name],
    )?;
    let target_agent_did = agent_did_from_init(&target_init)?;
    assert_ne!(target_agent_did, placeholder_did);
    assert_ne!(target_agent_did, source_agent_did);
    let target_home = target_home_env.join(".defra-agent");
    let target_home_str = target_home.to_str().expect("utf-8 target home");
    let root_str = root.to_str().expect("utf-8 root");

    let applied = run_cli_json(
        &target_home_env,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            target_home_str,
            "--bind-agent-did",
            "home",
        ],
    )?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(applied.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        applied.get("agent_did").and_then(Value::as_str),
        Some(target_agent_did.as_str())
    );
    assert_eq!(
        applied
            .pointer("/applied/agent_principal")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        applied
            .pointer("/applied/agent_behaviors")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        applied
            .pointer("/applied/tool_selections")
            .and_then(Value::as_u64),
        Some(1)
    );

    let reexport_root = tempdir.path().join("reexport");
    run_cli_text(
        &target_home_env,
        &[
            "config",
            "export",
            "--root",
            reexport_root.to_str().expect("utf-8 reexport root"),
            "--home",
            target_home_str,
        ],
    )?;
    assert_manifest_agent_dids(&reexport_root, &target_agent_did)?;
    assert!(
        !manifest_contains(&reexport_root, placeholder_did)?,
        "placeholder DID should not be written into target home"
    );

    let diff = run_cli_json(
        &target_home_env,
        &[
            "config",
            "diff",
            "--root",
            root_str,
            "--home",
            target_home_str,
            "--bind-agent-did",
            "home",
        ],
    )?;
    assert_eq!(diff.get("status").and_then(Value::as_str), Some("diffed"));
    assert_eq!(diff.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        diff.get("agent_did").and_then(Value::as_str),
        Some(target_agent_did.as_str())
    );

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut serve = spawn_server(&target_home_env, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &target_agent_did, Duration::from_secs(30)).await?;
    let status = run_cli_json(
        &target_home_env,
        &[
            "status",
            "--graphql",
            &graphql,
            "--agent-did",
            &target_agent_did,
        ],
    )?;
    assert_eq!(
        status.get("agent_did").and_then(Value::as_str),
        Some(target_agent_did.as_str())
    );

    Ok(())
}

/// #700 regression: a config row deleted by prune leaves a tombstone, and
/// re-applying a manifest that wants the same unique id back must converge —
/// the tombstone-aware writer issues `create_`, and the store must accept it
/// (a tombstone never holds a unique slot; a stale index entry pointing at one
/// is healed by the store, sourcenetwork/defradb.rs#1111).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reapply_recreates_a_pruned_unique_row() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-recreate-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-recreate-{}", Uuid::new_v4().simple());
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
    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;
    let principal = read_json_file(&root.join("agent-principal.json"))?;
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing default_behavior_id after export"))?
        .to_string();

    let task_id = format!("recreate-task-{}", Uuid::new_v4().simple());
    let task_dir = root.join("tasks").join(&task_id);
    let task_object = serde_json::json!({
        "task_id": task_id.clone(),
        "name": "Recreate Me",
        "description": "Tombstoned unique row recreate regression (#700).",
        "behavior_id": behavior_id.clone(),
        "prompt_template": "Check convergence.",
        "enabled": false,
    });
    write_json_file(&task_dir.join("object.json"), &task_object)?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let explicit_home = home_dir.join(".defra-agent");
    let explicit_home_str = explicit_home
        .to_str()
        .ok_or_else(|| anyhow!("explicit home path is not UTF-8"))?;

    // 1. Create the row.
    let created = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    assert_eq!(
        created.get("status").and_then(Value::as_str),
        Some("applied")
    );

    // 2. Tombstone it: drop the task from the manifest and prune.
    fs::remove_dir_all(&task_dir).with_context(|| format!("removing {}", task_dir.display()))?;
    let pruned = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
            "--prune",
        ],
    )?;
    assert_eq!(pruned.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        pruned.pointer("/pruned/tasks").and_then(Value::as_u64),
        Some(1)
    );

    // 3. Want the SAME unique task_id back. The live bundle does not see the
    //    tombstone, the diff says "create", and the create must succeed —
    //    this is exactly the step #700's wounded store could never pass.
    write_json_file(&task_dir.join("object.json"), &task_object)?;
    let recreated = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    assert_eq!(
        recreated.get("ok").and_then(Value::as_bool),
        Some(true),
        "recreating a pruned unique row must converge: {recreated}"
    );
    assert_eq!(
        recreated.pointer("/applied/tasks").and_then(Value::as_u64),
        Some(1),
        "the tombstoned task must be recreated: {recreated}"
    );

    // 4. And the store must be exactly converged afterwards.
    let settled = run_cli_json(
        &home_dir,
        &[
            "config",
            "diff",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    assert_eq!(
        settled.get("ok").and_then(Value::as_bool),
        Some(true),
        "diff must be exact after the recreate: {settled}"
    );

    Ok(())
}
