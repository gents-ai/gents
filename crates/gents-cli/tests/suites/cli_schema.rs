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
        &["--identity-only", "--agent-name", "schema-apply"],
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
    let first = run_cli_json(&home_dir, &["schema", "apply", schema_root])?;
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

    let second = run_cli_json(&home_dir, &["schema", "apply", schema_root])?;
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
            include_str!("../../../../demo/repo-maintenance/schemas/maintenance_report.graphql"),
            include_str!("../../../../demo/repo-maintenance/schemas/maintenance_report_owned_files.patch.json"),
        ),
        (
            "DefensePatchAssignment",
            include_str!("../../../../demo/defending-code/schemas/defense_patch_assignment.graphql"),
            include_str!("../../../../demo/defending-code/schemas/defense_patch_assignment_owned_files.patch.json"),
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
                    "--agent-name",
                    "immutable-pack-schema",
                    "--home",
                    home_arg,
                ],
            )?;
            let did = agent_did_from_init(&init)?;
            if upgrade {
                // This is the actual shipped predecessor SDL, before this PR's
                // one field addition; do not invent a different schema fixture.
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
                &["schema", "apply", schema_arg, "--home", home_arg],
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
                &["schema", "apply", schema_arg, "--home", home_arg],
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

            // Reopen the same CLI-created store only after those processes exit.
            // Verify actual immutable write behavior, not a patch JSON flag.
            let key_path = init
                .get("key_path")
                .and_then(Value::as_str)
                .context("init key path")?;
            let _identity = gents::KeyIdentity::load_or_create(key_path, None)?;
            let node = gents::defra_node::EmbeddedNode::builder()
                .data_path(agent_home.join("data"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
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
