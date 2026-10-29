use crate::support::*;

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

fn document_pack(
    root: &Path,
    version: &str,
    plugin: Option<(&str, Option<&str>)>,
) -> Result<Vec<u8>> {
    std::fs::create_dir_all(root)?;
    std::fs::write(root.join("README.md"), "# Pack safety fixture\n")?;
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&json!({
            "manifest_version": 1, "name": "safety", "namespace": "gents",
            "version": version, "description": "Pack safety fixture", "authors": ["tests"],
            "kind": "documents", "config": "pack_config.json",
            "assets": ["README.md", "pack_config.json"]
        }))?,
    )?;
    let mut config = json!({
        "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
        "tools": [{"tools_id": "safety-tools", "display_name": version}],
    });
    if let Some((coordinate, digest)) = plugin {
        let mut reference = json!({"plugin": coordinate});
        if let Some(digest) = digest {
            reference["digest"] = digest.into();
        }
        config["tools"][0]["integrations"] = json!({"plugins": [reference]});
    }
    std::fs::write(root.join("pack_config.json"), serde_json::to_vec(&config)?)?;
    Ok(gents::pack_archive::pack_dir(root)?.0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_pack_install_update_and_diagnose_keep_plugin_failures_isolated() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home)?;
    let endpoint = MockModelEndpoint::start("pack-safety")?;
    let init = run_init_json(
        &home,
        &[
            "--node-name",
            "pack-safety",
            "--model-name",
            "pack-safety",
            "--inference-url",
            endpoint.endpoint(),
        ],
    )?;
    let owner = node_did_from_init(&init)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut server = spawn_server_with_env(&home, port, &[], &[])?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &owner, Duration::from_secs(30)).await?;
    let root = temp.path().join("pack");
    document_pack(&root, "1.0.0", None)?;
    let root_arg = root.to_str().unwrap();
    run_cli_json(&home, &["pack", "install", root_arg, "--graphql", &graphql])?;
    let query = "{ Tools { tools_id integrations display_name } PackInstallation { coordinate version digest } }";
    let before = graphql_query(&graphql, query).await?;

    let bytes = document_pack(&root, "1.1.0", Some(("private/missing", None)))?;
    let error =
        run_cli_failure_stderr(&home, &["pack", "install", root_arg, "--graphql", &graphql])?;
    assert!(
        error.contains("private/missing") && error.contains("neither the pack"),
        "{error}"
    );
    assert!(
        error.contains("gents pack install gents/safety --home <node-home>"),
        "{error}"
    );
    assert_eq!(graphql_query(&graphql, query).await?, before);

    use sha2::Digest;
    let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
    let registry = Router::new()
        .route(
            "/api/v1/packs/{ns}/{name}",
            get(|| async { Json(json!({"latest": "1.1.0"})) }),
        )
        .route(
            "/api/v1/packs/{ns}/{name}/{version}",
            get(move || {
                let digest = digest.clone();
                async move { Json(json!({"digest": digest})) }
            }),
        )
        .route(
            "/api/v1/packs/{ns}/{name}/{version}/download",
            get(move || {
                let bytes = bytes.clone();
                async move { bytes }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let registry_url = format!("http://{}", listener.local_addr()?);
    let _registry = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, registry).await.unwrap();
    }));
    let failed = run_cli_failure_stdout_json(
        &home,
        &[
            "pack",
            "update",
            "gents/safety",
            "--graphql",
            &graphql,
            "--registry",
            &registry_url,
        ],
    )?;
    assert!(
        failed["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("private/missing"),
        "{failed}"
    );
    assert_eq!(graphql_query(&graphql, query).await?, before);

    // Install actual plugin bytes through the existing local pack owner, then
    // remotely install an exact pin through the runtime's guarded graph owner.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gents/tests/fixtures/packs/job_plugin_fixture");
    run_cli_json(&home, &["pack", "install", fixture.to_str().unwrap()])?;
    let plugin = gents::plugin::store::read_record(&home, "fixture", "job_fixture")?;
    document_pack(
        &root,
        "1.2.0",
        Some(("fixture/job_fixture", Some(&plugin.digest))),
    )?;
    run_cli_json(&home, &["pack", "install", root_arg, "--graphql", &graphql])?;
    let installed = graphql_query(&graphql, query).await?;
    document_pack(
        &root,
        "1.3.0",
        Some((
            "fixture/job_fixture",
            Some("sha256:0000000000000000000000000000000000000000000000000000000000000000"),
        )),
    )?;
    let wrong_pin =
        run_cli_failure_stderr(&home, &["pack", "install", root_arg, "--graphql", &graphql])?;
    assert!(
        wrong_pin.contains("pins plugin fixture/job_fixture"),
        "{wrong_pin}"
    );
    assert_eq!(graphql_query(&graphql, query).await?, installed);

    let owner_escaped = escape_graphql_string(&owner);
    let profile = escape_graphql_string(
        &gents::document_config::default_inference_profile_id_for_node(&owner),
    );
    graphql_query(&graphql, &format!(r#"mutation {{
        create_Tools(input: {{tools_id: "broken-tools", node_did: "{owner_escaped}", integrations: {{plugins: [{{plugin: "private/missing"}}]}}}}) {{_docID}}
        create_AgentContext(input: {{context_id: "broken-context", node_did: "{owner_escaped}", tools_id: "broken-tools"}}) {{_docID}}
        create_Agent(input: {{agent_id: "broken-agent", node_did: "{owner_escaped}", context_id: "broken-context", inference_profile_id: "{profile}", enabled: true}}) {{_docID}}
    }}"#)).await?;
    wait_for_runtime_quiescence(&graphql, &owner, 2, Duration::from_millis(200)).await?;
    let diagnosed = run_cli_json(&home, &["diagnose", "--graphql", &graphql])?;
    let unavailable = diagnosed
        .pointer("/checks/runtime_node_readiness/unavailable_agents")
        .and_then(Value::as_array)
        .unwrap();
    let broken = unavailable
        .iter()
        .find(|entry| entry["agent_id"] == "broken-agent")
        .unwrap();
    assert_eq!(broken["reason"], "tool_surface_unavailable");
    assert_eq!(
        broken["message"],
        gents_protocol::node_readiness::AgentReadinessUnavailableReason::ToolSurfaceUnavailable
            .public_message()
    );
    assert_eq!(
        broken["next_step"],
        "Check installed plugins and Tools configuration on the node."
    );
    assert!(!broken.to_string().contains("private/missing"));
    assert_eq!(
        diagnosed.pointer("/checks/runtime_node_readiness/ready_agent_count"),
        Some(&json!(1))
    );
    assert_eq!(
        diagnosed.pointer("/runtime_node_readiness/process_state"),
        Some(&json!("ready"))
    );
    Ok(())
}
