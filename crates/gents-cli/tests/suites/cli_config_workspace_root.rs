use crate::support::*;

use std::fs;
use std::time::Duration;

type Result<T = (), E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_root_set_list_show_rm_round_trip() -> Result<()> {
    let tempdir = tempfile::tempdir().expect("creating tempdir");
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-workspace-root-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-workspace-root-{}", Uuid::new_v4().simple());
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
    let node_did = node_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    let relative_rejection = run_cli_failure_stderr(
        &home_dir,
        &[
            "config",
            "workspace-root",
            "set",
            "--graphql",
            &graphql,
            "--path",
            "relative/workspace",
        ],
    )?;
    assert!(
        relative_rejection.contains("must be an absolute path"),
        "{relative_rejection}"
    );

    let enabled_root = tempdir.path().join("does_not_exist_yet/enabled_root");
    let enabled_root_str = enabled_root.to_str().expect("enabled root utf8");
    let disabled_root = tempdir.path().join("disabled_root");
    fs::create_dir_all(&disabled_root)?;
    let disabled_root_str = disabled_root.to_str().expect("disabled root utf8");

    let set_enabled = run_cli_json(
        &home_dir,
        &[
            "config",
            "workspace-root",
            "set",
            "--graphql",
            &graphql,
            "--path",
            enabled_root_str,
            "--display-name",
            "Enabled Root",
        ],
    )?;
    assert_eq!(
        set_enabled.get("enabled").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        set_enabled.get("root_path").and_then(Value::as_str),
        Some(enabled_root_str)
    );

    let set_disabled = run_cli_json(
        &home_dir,
        &[
            "config",
            "workspace-root",
            "set",
            "--graphql",
            &graphql,
            "--path",
            disabled_root_str,
            "--display-name",
            "Disabled Root",
            "--disabled",
        ],
    )?;
    assert_eq!(
        set_disabled.get("enabled").and_then(Value::as_bool),
        Some(false)
    );

    let list = run_cli_json(
        &home_dir,
        &[
            "config",
            "workspace-root",
            "list",
            "--graphql",
            &graphql,
            "--output",
            "json",
        ],
    )?;
    let items = list
        .get("items")
        .and_then(Value::as_array)
        .expect("workspace-root list must include items");
    let enabled_row = items
        .iter()
        .find(|row| row.get("root_path").and_then(Value::as_str) == Some(enabled_root_str))
        .ok_or_else(|| format!("list missing enabled root: {list}"))?;
    assert_eq!(
        enabled_row.get("enabled").and_then(Value::as_bool),
        Some(true)
    );
    let disabled_row = items
        .iter()
        .find(|row| row.get("root_path").and_then(Value::as_str) == Some(disabled_root_str))
        .ok_or_else(|| format!("list missing disabled root: {list}"))?;
    assert_eq!(
        disabled_row.get("enabled").and_then(Value::as_bool),
        Some(false)
    );

    let show = run_cli_json(
        &home_dir,
        &[
            "config",
            "workspace-root",
            "show",
            "--graphql",
            &graphql,
            disabled_root_str,
        ],
    )?;
    assert_eq!(
        show.get("display_name").and_then(Value::as_str),
        Some("Disabled Root")
    );
    assert_eq!(show.get("enabled").and_then(Value::as_bool), Some(false));

    let enabled_only = graphql_query(
        &graphql,
        r#"{ WorkspaceRoot(filter: { enabled: { _eq: true } }) { root_path } }"#,
    )
    .await?;
    let enabled_paths = enabled_only
        .get("data")
        .and_then(|data| data.get("WorkspaceRoot"))
        .and_then(Value::as_array)
        .expect("WorkspaceRoot query must return an array")
        .iter()
        .filter_map(|row| row.get("root_path").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(
        enabled_paths,
        vec![enabled_root_str],
        "allowed_roots' enabled-filter source must publish only the enabled root"
    );

    let reset_disabled = run_cli_json(
        &home_dir,
        &[
            "config",
            "workspace-root",
            "set",
            "--graphql",
            &graphql,
            "--path",
            disabled_root_str,
            "--display-name",
            "Re-enabled Root",
        ],
    )?;
    assert_eq!(
        reset_disabled.get("enabled").and_then(Value::as_bool),
        Some(true)
    );
    let show_reset = run_cli_json(
        &home_dir,
        &[
            "config",
            "workspace-root",
            "show",
            "--graphql",
            &graphql,
            disabled_root_str,
        ],
    )?;
    assert_eq!(
        show_reset.get("display_name").and_then(Value::as_str),
        Some("Re-enabled Root"),
        "update branch must overwrite mutable fields on the existing row"
    );
    let both_enabled = graphql_query(
        &graphql,
        r#"{ WorkspaceRoot(filter: { enabled: { _eq: true } }) { root_path } }"#,
    )
    .await?;
    let mut both_paths = both_enabled
        .get("data")
        .and_then(|data| data.get("WorkspaceRoot"))
        .and_then(Value::as_array)
        .expect("WorkspaceRoot query must return an array")
        .iter()
        .filter_map(|row| row.get("root_path").and_then(Value::as_str))
        .collect::<Vec<_>>();
    both_paths.sort_unstable();
    let mut expected_paths = vec![enabled_root_str, disabled_root_str];
    expected_paths.sort_unstable();
    assert_eq!(
        both_paths, expected_paths,
        "re-enabling via set must update the existing row, not mint a duplicate"
    );

    let rm_enabled = run_cli_json(
        &home_dir,
        &[
            "config",
            "workspace-root",
            "rm",
            "--graphql",
            &graphql,
            enabled_root_str,
        ],
    )?;
    assert_eq!(rm_enabled.get("deleted").and_then(Value::as_u64), Some(1));

    let rm_disabled = run_cli_json(
        &home_dir,
        &[
            "config",
            "workspace-root",
            "rm",
            "--graphql",
            &graphql,
            disabled_root_str,
        ],
    )?;
    assert_eq!(rm_disabled.get("deleted").and_then(Value::as_u64), Some(1));

    let second_rm = run_cli_failure_stderr(
        &home_dir,
        &[
            "config",
            "workspace-root",
            "rm",
            "--graphql",
            &graphql,
            enabled_root_str,
        ],
    )?;
    assert!(
        second_rm.contains("no WorkspaceRoot document"),
        "{second_rm}"
    );

    Ok(())
}
