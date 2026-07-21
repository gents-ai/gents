mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_reconciles_running_runtime_without_restart() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-apply-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-apply-{}", Uuid::new_v4().simple());

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

    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let behaviors_dir = root.join("agent-behaviors");
    let behavior_entry = fs::read_dir(&behaviors_dir)
        .context("reading agent-behaviors dir after export")?
        .next()
        .ok_or_else(|| anyhow!("no agent-behavior subdirs after export"))??;
    let behavior_id = behavior_entry
        .file_name()
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 behavior dir name"))?
        .to_string();
    let behaviors_path = root
        .join("agent-behaviors")
        .join(&behavior_id)
        .join("object.json");
    let mut behavior = read_json_file(&behaviors_path)?;
    let updated_prompt = "Keep responses terse. Mention that desired state was applied.";
    behavior["system_prompt"] = Value::String(updated_prompt.to_string());
    write_json_file(&behaviors_path, &behavior)?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let applied = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(applied.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(applied.get("changed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        applied
            .pointer("/applied/agent_behaviors")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        applied
            .pointer("/remaining/agent_behaviors/update")
            .and_then(Value::as_u64),
        Some(0)
    );

    let generation_after_apply =
        wait_for_runtime_quiescence(&graphql, &agent_did, 2, Duration::from_secs(6)).await?;
    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentBehavior(
                    filter: {{ agent_did: {{ _eq: "{}" }} }},
                    limit: 1
                ) {{
                    system_prompt
                }}
            }}"#,
            escape_graphql_string(&agent_did),
        ),
    )
    .await?;
    let behavior_row = first_graphql_row(&response, "AgentBehavior")?;
    assert_eq!(
        behavior_row.get("system_prompt").and_then(Value::as_str),
        Some(updated_prompt)
    );

    let noop = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(noop.get("status").and_then(Value::as_str), Some("noop"));
    assert_eq!(noop.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(noop.get("changed").and_then(Value::as_bool), Some(false));
    assert_eq!(
        noop.pointer("/applied/agent_behaviors")
            .and_then(Value::as_u64),
        Some(0)
    );

    let generation_after_noop = wait_for_runtime_quiescence(
        &graphql,
        &agent_did,
        generation_after_apply,
        Duration::from_secs(3),
    )
    .await?;
    assert_eq!(generation_after_noop, generation_after_apply);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_diff_bind_live_force_rebinds_concrete_manifest_to_running_runtime() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir
        .path()
        .join("infra")
        .join("agents")
        .join("mini-1-steward");
    fs::create_dir_all(&home_dir)?;

    let concrete_manifest_did = "did:test:mini-1-steward";
    let model_name = format!("mock-live-rebind-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            "mini-1-steward",
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;
    rewrite_manifest_agent_dids(&root, concrete_manifest_did)?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentRuntime(input: {{
                    agent_did: "{}",
                    process_state: "shutdown",
                    updated_at: "2099-01-01T00:00:00Z"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(concrete_manifest_did),
        ),
    )
    .await?;

    let diff = run_cli_json(
        &home_dir,
        &[
            "config",
            "diff",
            "--root",
            root.to_str().expect("utf-8 root"),
            "--graphql",
            &graphql,
            "--bind-agent-did",
            "live",
            "--force-rebind-concrete-did",
        ],
    )?;
    assert_eq!(diff.get("status").and_then(Value::as_str), Some("diffed"));
    assert_eq!(diff.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        diff.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );

    Ok(())
}
