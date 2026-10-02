use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_create_signs_as_the_home_principal_and_keeps_schema_validation() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-document-{}", Uuid::new_v4().simple());
    let init = run_init_json(&home_dir, &["--agent-name", &agent_name])?;
    let agent_did = agent_did_from_init(&init)?;
    let offline_fields = serde_json::json!({
        "goal_id": "offline-goal",
        "session_id": "offline-session",
        "agent_did": agent_did,
        "objective": "created before the server starts",
        "status": "paused",
        "created_at": "2026-07-16T00:00:00Z",
    })
    .to_string();
    let offline = run_cli_json(
        &home_dir,
        &["document", "create", "Goal", "--json", &offline_fields],
    )?;
    assert!(offline["doc_id"].as_str().is_some(), "{offline}");
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let goal_id = format!("goal-{}", Uuid::new_v4().simple());
    let fields = serde_json::json!({
        "goal_id": goal_id,
        "session_id": "document-create-session",
        "agent_did": agent_did,
        "objective": "created by the operator \"command\"",
        "status": "paused",
        "created_at": "2026-07-16T00:00:00Z",
    })
    .to_string();
    let created = run_cli_json(
        &home_dir,
        &[
            "document",
            "create",
            "Goal",
            "--graphql",
            &graphql,
            "--json",
            &fields,
        ],
    )?;
    assert_eq!(created["collection"], "Goal");
    let doc_id = created["doc_id"].as_str().context("doc_id")?;

    let rows = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "Goal",
            "--field",
            "_docID",
            "--field",
            "objective",
            "--filter",
            &format!(r#"{{"goal_id":{{"_eq":"{goal_id}"}}}}"#),
        ],
    )?;
    assert_eq!(rows["count"], 1, "{rows}");
    assert_eq!(rows["results"][0]["_docID"], doc_id);
    assert_eq!(
        rows["results"][0]["objective"].as_str(),
        Some(r#"created by the operator "command""#)
    );

    let unknown_field = run_cli_failure_stderr(
        &home_dir,
        &[
            "document",
            "create",
            "Goal",
            "--graphql",
            &graphql,
            "--json",
            r#"{"no_such_field":"x"}"#,
        ],
    )?;
    assert!(unknown_field.contains("no_such_field"), "{unknown_field}");

    let metadata_field = run_cli_failure_stderr(
        &home_dir,
        &[
            "document",
            "create",
            "Goal",
            "--graphql",
            &graphql,
            "--json",
            r#"{"_docID":"not-an-input"}"#,
        ],
    )?;
    assert!(
        metadata_field.contains("no field \"_docID\""),
        "{metadata_field}"
    );

    let unknown_collection = run_cli_failure_stderr(
        &home_dir,
        &[
            "document",
            "create",
            "NoSuchCollection",
            "--graphql",
            &graphql,
            "--json",
            r#"{"a":"b"}"#,
        ],
    )?;
    assert!(
        unknown_collection.contains("NoSuchCollection"),
        "{unknown_collection}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_create_cannot_write_to_a_home_it_does_not_own() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("served");
    let other_dir = tempdir.path().join("other");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&other_dir)?;
    run_init_json(&other_dir, &["--agent-name", "cli-document-other"])?;
    let init = run_init_json(&home_dir, &["--agent-name", "cli-document-served"])?;
    let agent_did = agent_did_from_init(&init)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    // The other home's principal is not the served home's, so the write is refused.
    let goal_id = format!("goal-{}", Uuid::new_v4().simple());
    let fields = serde_json::json!({
        "goal_id": goal_id,
        "session_id": "document-create-foreign",
        "agent_did": agent_did,
        "objective": "must not be written",
        "status": "paused",
        "created_at": "2026-07-16T00:00:00Z",
    })
    .to_string();
    let refusal = run_cli_failure_stderr(
        &other_dir,
        &[
            "document",
            "create",
            "Goal",
            "--graphql",
            &graphql,
            "--json",
            &fields,
        ],
    )?;

    assert!(
        refusal.contains("not authorized to perform operation"),
        "{refusal}"
    );

    let rows = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "Goal",
            "--field",
            "_docID",
            "--filter",
            &format!(r#"{{"goal_id":{{"_eq":"{goal_id}"}}}}"#),
        ],
    )?;
    assert_eq!(rows["count"], 0, "{rows}");
    Ok(())
}
