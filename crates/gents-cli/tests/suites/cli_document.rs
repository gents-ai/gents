use crate::support::*;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use uuid::Uuid;

/// `document create` publishes only application collections, signed by the
/// held User against SDL it authored; canonical collections have their own
/// publication owners.
const NOTE_SDL: &str =
    "type CliDocumentNote {\n    note_id: String\n    objective: String\n    status: String\n}\n";

fn write_note_sdl(dir: &Path) -> Result<PathBuf> {
    let path = dir.join("cli_document_note.graphql");
    fs::write(&path, NOTE_SDL)?;
    Ok(path)
}

fn apply_note_schema(home_dir: &Path, graphql: &str, sdl: &str) -> Result<()> {
    run_cli_json(home_dir, &["schema", "apply", "--graphql", graphql, sdl])?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_create_signs_as_the_home_principal_and_keeps_schema_validation() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let sdl = write_note_sdl(tempdir.path())?;
    let sdl = sdl.to_str().context("utf-8 SDL path")?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-document-{}", Uuid::new_v4().simple());
    let init = run_init_json(&home_dir, &["--node-name", &agent_name])?;
    let node_did = node_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    apply_note_schema(&home_dir, &graphql, sdl)?;

    let note_id = format!("note-{}", Uuid::new_v4().simple());
    let fields = serde_json::json!({
        "note_id": note_id,
        "objective": "created by the operator \"command\"",
        "status": "paused",
    })
    .to_string();
    let created = run_cli_json(
        &home_dir,
        &[
            "document",
            "create",
            "CliDocumentNote",
            "--schema",
            sdl,
            "--graphql",
            &graphql,
            "--json",
            &fields,
        ],
    )?;
    assert_eq!(created["collection"], "CliDocumentNote");
    let doc_id = created["doc_id"].as_str().context("doc_id")?;

    let rows = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "CliDocumentNote",
            "--field",
            "_docID",
            "--field",
            "objective",
            "--filter",
            &format!(r#"{{"note_id":{{"_eq":"{note_id}"}}}}"#),
        ],
    )?;
    assert_eq!(rows["returned_count"], 1, "{rows}");
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
            "CliDocumentNote",
            "--schema",
            sdl,
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
            "CliDocumentNote",
            "--schema",
            sdl,
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
            "--schema",
            sdl,
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

    let canonical = run_cli_failure_stderr(
        &home_dir,
        &[
            "document",
            "create",
            "Goal",
            "--schema",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../gents-schemas/schemas/agent/goal.graphql"
            ),
            "--graphql",
            &graphql,
            "--json",
            r#"{"goal_id":"g","objective":"o"}"#,
        ],
    )?;
    assert!(
        canonical.contains("dedicated publication owner"),
        "{canonical}"
    );

    serve.child.kill()?;
    serve.child.wait()?;
    let offline = run_cli_failure_stderr(
        &home_dir,
        &[
            "document",
            "create",
            "CliDocumentNote",
            "--schema",
            sdl,
            "--json",
            r#"{"note_id":"offline","objective":"o","status":"paused"}"#,
        ],
    )?;
    assert!(offline.contains("running HTTP endpoint"), "{offline}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_create_cannot_write_to_a_home_it_does_not_own() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("served");
    let other_dir = tempdir.path().join("other");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&other_dir)?;
    let sdl = write_note_sdl(tempdir.path())?;
    let sdl = sdl.to_str().context("utf-8 SDL path")?;
    run_init_json(&other_dir, &["--node-name", "cli-document-other"])?;
    let init = run_init_json(&home_dir, &["--node-name", "cli-document-served"])?;
    let node_did = node_did_from_init(&init)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    apply_note_schema(&home_dir, &graphql, sdl)?;

    let note_id = format!("note-{}", Uuid::new_v4().simple());
    let fields = serde_json::json!({
        "note_id": note_id,
        "objective": "must not be written",
        "status": "paused",
    })
    .to_string();
    let refusal = run_cli_failure_stderr(
        &other_dir,
        &[
            "document",
            "create",
            "CliDocumentNote",
            "--schema",
            sdl,
            "--graphql",
            &graphql,
            "--json",
            &fields,
        ],
    )?;

    // A User the Node has not admitted is refused by node access control,
    // before or at the signed create.
    assert!(
        refusal.contains("not authorized to perform operation")
            || refusal.contains("401 Unauthorized"),
        "{refusal}"
    );

    let rows = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "CliDocumentNote",
            "--field",
            "_docID",
            "--filter",
            &format!(r#"{{"note_id":{{"_eq":"{note_id}"}}}}"#),
        ],
    )?;
    assert_eq!(rows["returned_count"], 0, "{rows}");
    Ok(())
}
