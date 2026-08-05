mod support;
use support::*;

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

/// Ties Task 6 (materializer-backed `config behavior create|clone|disable` +
/// enriched `show`) to the shared persona materializer landed earlier on
/// this branch (`gents::agent::persona_ops` + the persona-request
/// reconciler, `crates/gents/src/agent/p2p_reconcile/persona_requests.rs`).
/// The CLI submits a `PersonaConfigRequest` row over HTTP GraphQL and polls
/// it to a terminal status — the exact channel the reconciler and the
/// agent's own `configure_persona` self-config tool use — so this test
/// exercises the real end-to-end path against a running `gents server`,
/// following the harness precedent in `cli_config_workspace_root.rs`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn behavior_create_clone_disable_round_trip_and_enriched_show() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    std::fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-persona-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-persona-{}", Uuid::new_v4().simple());
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
    let agent_did = agent_did_from_init(&init)?;
    let backend_id = init
        .get("init")
        .and_then(|value| value.get("backend_id"))
        .and_then(Value::as_str)
        .context("init output missing init.backend_id")?
        .to_string();
    let profile_id = init
        .get("inference_profile_id")
        .and_then(Value::as_str)
        .context("init output missing inference_profile_id")?
        .to_string();
    let model = format!("{backend_id}|{model_name}");

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    // -- bad model rejected with the catalog copy verbatim, exit non-zero --
    let rejection = run_cli_failure_stderr(
        &home_dir,
        &[
            "config",
            "behavior",
            "create",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--display-name",
            "Bad Persona",
            "--preset",
            "write",
            "--profile-id",
            &profile_id,
            "--model",
            "nope|nope",
        ],
    )?;
    assert!(
        rejection.contains(r#"unknown model "nope|nope""#),
        "{rejection}"
    );
    assert!(
        rejection.contains("available_models"),
        "rejection must name the catalog source: {rejection}"
    );

    // -- create --
    let created = run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "create",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--display-name",
            "Research Assistant",
            "--preset",
            "write",
            "--profile-id",
            &profile_id,
            "--model",
            &model,
        ],
    )?;
    assert_eq!(
        created.get("status").and_then(Value::as_str),
        Some("applied")
    );
    let behavior_id = created
        .get("behavior_id")
        .and_then(Value::as_str)
        .context("create output missing behavior_id")?
        .to_string();
    assert_eq!(behavior_id, format!("{agent_did}:research-assistant"));

    // -- clone --
    let cloned = run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "clone",
            &behavior_id,
            "--graphql",
            &graphql,
            "--display-name",
            "Cloned Assistant",
        ],
    )?;
    assert_eq!(
        cloned.get("status").and_then(Value::as_str),
        Some("applied")
    );
    let cloned_id = cloned
        .get("behavior_id")
        .and_then(Value::as_str)
        .context("clone output missing behavior_id")?
        .to_string();
    assert_eq!(cloned_id, format!("{agent_did}:cloned-assistant"));

    // -- disable the clone --
    let disabled = run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "disable",
            &cloned_id,
            "--graphql",
            &graphql,
        ],
    )?;
    assert_eq!(
        disabled.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(
        disabled.get("behavior_id").and_then(Value::as_str),
        Some(cloned_id.as_str())
    );

    let cloned_show = run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "show",
            &cloned_id,
            "--graphql",
            &graphql,
        ],
    )?;
    assert_eq!(
        cloned_show.get("enabled").and_then(Value::as_bool),
        Some(false)
    );

    // -- enriched show: preset classification for a readonly-template
    //    selection, and "custom" once hand-tuned --
    let write_show = run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "show",
            &behavior_id,
            "--graphql",
            &graphql,
        ],
    )?;
    assert_eq!(
        write_show
            .get("resolved")
            .and_then(|resolved| resolved.get("preset_name"))
            .and_then(Value::as_str),
        Some("write")
    );
    assert_eq!(
        write_show
            .get("resolved")
            .and_then(|resolved| resolved.get("profile"))
            .and_then(|profile| profile.get("profile_id"))
            .and_then(Value::as_str),
        Some(profile_id.as_str())
    );

    let readonly_created = run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "create",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--display-name",
            "Readonly Persona",
            "--preset",
            "readonly",
            "--profile-id",
            &profile_id,
            "--model",
            &model,
        ],
    )?;
    let readonly_id = readonly_created
        .get("behavior_id")
        .and_then(Value::as_str)
        .context("create output missing behavior_id")?
        .to_string();

    let readonly_show = run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "show",
            &readonly_id,
            "--graphql",
            &graphql,
        ],
    )?;
    assert_eq!(
        readonly_show
            .get("resolved")
            .and_then(|resolved| resolved.get("preset_name"))
            .and_then(Value::as_str),
        Some("readonly")
    );
    let selection_id = readonly_show
        .get("tool_selection_id")
        .and_then(Value::as_str)
        .context("show output missing tool_selection_id")?
        .to_string();

    // Hand-tune the readonly-template selection: this is exactly the
    // "one extra argv prefix classifies as custom" case
    // `persona_presets::preset_name` fences.
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
            "--command-allowed-argv-prefix",
            "git status",
        ],
    )?;

    let tuned_show = run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "show",
            &readonly_id,
            "--graphql",
            &graphql,
        ],
    )?;
    assert_eq!(
        tuned_show
            .get("resolved")
            .and_then(|resolved| resolved.get("preset_name"))
            .and_then(Value::as_str),
        Some("custom")
    );

    Ok(())
}
