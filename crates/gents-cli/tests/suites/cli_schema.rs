use crate::support::*;

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;

#[test]
fn schema_apply_registers_sdl_and_additive_patch_idempotently() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let schema_dir = tempdir.path().join("schemas");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&schema_dir)?;
    run_init_json(
        &home_dir,
        &["--identity-only", "--node-name", "schema-apply"],
    )?;

    fs::write(
        schema_dir.join("action_request.graphql"),
        r#"
type ActionRequest {
    task_id: String
}
"#,
    )?;
    fs::write(
        schema_dir.join("action_request.patch.json"),
        r#"[
  {"op":"add","path":"/ActionRequest/Fields/-","value":{"Name":"status","Kind":"String"}}
]"#,
    )?;

    let schema_root = schema_dir.to_str().expect("schema dir is utf-8");
    let first = run_cli_json(&home_dir, &["data", "schema", "apply", schema_root])?;
    assert_eq!(
        first.get("status").and_then(Value::as_str),
        Some("schema_applied")
    );
    assert_eq!(first.get("mode").and_then(Value::as_str), Some("local"));
    assert_eq!(
        first
            .pointer("/schema_files/0/status")
            .and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(
        first
            .pointer("/patch_files/0/status")
            .and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(
        first
            .pointer("/patch_files/0/applied_fields/0")
            .and_then(Value::as_str),
        Some("status")
    );

    let second = run_cli_json(&home_dir, &["data", "schema", "apply", schema_root])?;
    assert_eq!(
        second
            .pointer("/schema_files/0/status")
            .and_then(Value::as_str),
        Some("already_exists")
    );
    assert_eq!(
        second
            .pointer("/patch_files/0/status")
            .and_then(Value::as_str),
        Some("already_exists")
    );
    assert_eq!(
        second
            .pointer("/patch_files/0/skipped_fields/0")
            .and_then(Value::as_str),
        Some("status")
    );

    Ok(())
}

#[tokio::test]
async fn bundled_workspace_owned_files_remain_immutable_on_fresh_install_and_upgrade() -> Result<()>
{
    let bundled = [
        (
            "MaintenanceReport",
            include_str!("../fixtures/workspace_owned_schemas/maintenance_report.graphql"),
            include_str!(
                "../fixtures/workspace_owned_schemas/maintenance_report_owned_files.patch.json"
            ),
        ),
        (
            "DefensePatchAssignment",
            include_str!("../fixtures/workspace_owned_schemas/defense_patch_assignment.graphql"),
            include_str!(
                "../fixtures/workspace_owned_schemas/defense_patch_assignment_owned_files.patch.json"
            ),
        ),
    ];
    for (collection, sdl, patch) in bundled {
        for upgrade in [false, true] {
            let tempdir = tempfile::tempdir()?;
            let home_dir = tempdir.path().join("home");
            let agent_home = tempdir.path().join("agent");
            let schema_dir = tempdir.path().join("schemas");
            fs::create_dir_all(&home_dir)?;
            fs::create_dir_all(&schema_dir)?;
            let home_arg = agent_home.to_str().context("agent home utf8")?;
            let init = run_init_json(
                &home_dir,
                &[
                    "--identity-only",
                    "--node-name",
                    "immutable-pack-schema",
                    "--home",
                    home_arg,
                ],
            )?;
            let did = node_did_from_init(&init)?;
            if upgrade {
                let predecessor = sdl.replace("  owned_files: String @immutable\n", "");
                assert_ne!(
                    predecessor, sdl,
                    "{collection}: expected bundled field declaration"
                );
                let old_path = tempdir.path().join("predecessor.graphql");
                fs::write(&old_path, predecessor)?;
                run_cli_json(
                    &home_dir,
                    &[
                        "data",
                        "schema",
                        "apply",
                        old_path.to_str().context("schema path utf8")?,
                        "--home",
                        home_arg,
                    ],
                )?;
            }
            fs::write(schema_dir.join("collection.graphql"), sdl)?;
            fs::write(schema_dir.join("owned_files.patch.json"), patch)?;
            let schema_arg = schema_dir.to_str().context("schema root utf8")?;
            let first = run_cli_json(
                &home_dir,
                &["data", "schema", "apply", schema_arg, "--home", home_arg],
            )?;
            assert_eq!(
                first
                    .pointer("/patch_files/0/status")
                    .and_then(Value::as_str),
                Some(if upgrade { "applied" } else { "already_exists" }),
                "{collection}, upgrade={upgrade}: {first}"
            );
            let repeated = run_cli_json(
                &home_dir,
                &["data", "schema", "apply", schema_arg, "--home", home_arg],
            )?;
            assert_eq!(
                repeated
                    .pointer("/patch_files/0/status")
                    .and_then(Value::as_str),
                Some("already_exists")
            );
            assert_eq!(
                repeated
                    .pointer("/patch_files/0/skipped_fields/0")
                    .and_then(Value::as_str),
                Some("owned_files")
            );

            let key_path = init
                .get("key_path")
                .and_then(Value::as_str)
                .context("init key path")?;
            let _identity = gents::KeyIdentity::load_or_create(key_path, None)?;
            let data = agent_home.join("data");
            let node = gents::store::local::store_key::open_home_store_key(&agent_home, &data)
                .await?
                .encrypt(
                    gents::defra_node::EmbeddedNode::builder()
                        .data_path(&data)
                        .with_storage_backend(gents::defra_node::StorageBackend::Regolith),
                )
                .with_node_identity_did(&did)
                .build()
                .await?;
            let owned = r#"["src/main.rs"]"#;
            let create = node.execute(&format!(
                "mutation {{ create_{collection}(input: {{ owned_files: \"{}\" }}) {{ _docID owned_files }} }}",
                escape_graphql_string(owned),
            )).await;
            assert!(
                !create.has_errors(),
                "{collection}, upgrade={upgrade}: {:?}",
                create.errors
            );
            let doc_id = gents_protocol::graphql::extract_mutation_doc_id(
                &serde_json::json!({"data": create.data.context("create data")?}),
                collection,
            )?;
            let changed = node.execute(&format!(
                "mutation {{ update_{collection}(filter: {{ _docID: {{ _eq: \"{}\" }} }}, input: {{ owned_files: \"[]\" }}) {{ _docID }} }}",
                escape_graphql_string(&doc_id),
            )).await;
            assert!(
                changed.has_errors(),
                "{collection}, upgrade={upgrade}: owned_files became mutable"
            );
            let after = node
                .execute(&format!(
                    "{{ {collection}(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ owned_files }} }}",
                    escape_graphql_string(&doc_id),
                ))
                .await;
            assert!(!after.has_errors(), "{:?}", after.errors);
            assert_eq!(
                after.data.context("read data")?[collection][0]["owned_files"],
                owned
            );
            node.shutdown().await;
            drop(node);
            // Defra's background task cleanup may outlive shutdown; preserve
            // this isolated test store instead of deleting it underneath them.
            std::mem::forget(tempdir);
        }
    }
    Ok(())
}

#[test]
fn export_and_paged_query_preserve_long_values() -> Result<()> {
    use serde_json::json;
    use sha2::{Digest, Sha256};

    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let schemas = tempdir.path().join("schemas");
    let output = tempdir.path().join("export");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&schemas)?;
    run_init_json(&home, &["--identity-only", "--node-name", "export-test"])?;
    fs::write(
        schemas.join("record.graphql"),
        "type ExportRecord { body: String category: String }",
    )?;
    run_cli_json(
        &home,
        &["data", "schema", "apply", schemas.to_str().unwrap()],
    )?;
    let original = format!("{}😀", "é".repeat(1500));
    let input = json!({"body":original,"category":"keep"}).to_string();
    run_cli_json(
        &home,
        &[
            "data",
            "document",
            "create",
            "ExportRecord",
            "--json",
            &input,
        ],
    )?;
    run_cli_json(
        &home,
        &[
            "data",
            "document",
            "create",
            "ExportRecord",
            "--json",
            r#"{"body":"skip","category":"other"}"#,
        ],
    )?;
    let query = run_cli_json(
        &home,
        &[
            "data",
            "query",
            "find",
            "--collection",
            "ExportRecord",
            "--field",
            "body",
            "--filter",
            r#"{"category":{"_eq":"keep"}}"#,
        ],
    )?;
    assert_eq!(query["truncated"], true);
    assert_eq!(query["truncated_fields"][0]["total_bytes"], original.len());
    let doc = query["field_recovery"][0]["doc_id"].as_str().unwrap();
    let mut offset = 0;
    let mut hash = None::<String>;
    let mut recovered = String::new();
    loop {
        let mut page = json!({"doc_id":doc,"field":"body","offset_bytes":offset});
        if let Some(hash) = &hash {
            page["expected_hash"] = json!(hash);
        }
        let response = run_cli_json(
            &home,
            &[
                "data",
                "query",
                "find",
                "--collection",
                "ExportRecord",
                "--field",
                "body",
                "--field-page",
                &page.to_string(),
            ],
        )?;
        let page = &response["field_page"];
        recovered.push_str(page["text"].as_str().unwrap());
        hash = Some(page["value_hash"].as_str().unwrap().to_owned());
        offset = page["next_offset_bytes"].as_u64().unwrap();
        if page["complete"] == true {
            break;
        }
    }
    assert_eq!(recovered, original);
    let response = run_cli_json(
        &home,
        &[
            "data",
            "export",
            "ExportRecord",
            "--jsonl",
            "--output-dir",
            output.to_str().unwrap(),
            "--filter",
            r#"{"category":{"_eq":"keep"}}"#,
        ],
    )?;
    assert_eq!(response["status"], "complete");
    let bytes = fs::read(output.join("ExportRecord.jsonl"))?;
    let row: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(row["body"], original);
    let digest = format!("{:x}", Sha256::digest(&bytes));
    assert_eq!(
        fs::read_to_string(output.join("MANIFEST.tsv"))?,
        format!(
            "collection\trows\tbytes\tsha256\nExportRecord\t1\t{}\t{digest}\n",
            bytes.len()
        )
    );
    let all_output = tempdir.path().join("all-export");
    let all = run_cli_json(
        &home,
        &[
            "data",
            "export",
            "--all-app-collections",
            "--jsonl",
            "--output-dir",
            all_output.to_str().unwrap(),
        ],
    )?;
    assert_eq!(all["status"], "complete");
    assert_eq!(
        fs::read_to_string(all_output.join("ExportRecord.jsonl"))?
            .lines()
            .count(),
        2
    );
    assert!(!all_output.join("Node.jsonl").exists());
    let failed_output = tempdir.path().join("failed-export");
    run_cli_failure_stderr(
        &home,
        &[
            "data",
            "export",
            "MissingRecord",
            "--jsonl",
            "--output-dir",
            failed_output.to_str().unwrap(),
        ],
    )?;
    assert!(!failed_output.join("MANIFEST.tsv").exists());
    Ok(())
}
