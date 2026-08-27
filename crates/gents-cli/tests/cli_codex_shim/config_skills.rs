use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_skill_cli_disable_enable_and_rm_round_trip() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-skill-crud-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "ok")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-skill-crud-{}", Uuid::new_v4().simple());
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

    let mut serve = spawn_server_with_env(&home_dir, server_port, &[], &[])?;
    wait_for_port(server_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;
    wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "add",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--skill-id",
            "research",
            "--scope",
            "principal",
            "--name",
            "Research",
            "--description",
            "Find and cite sources",
            "--instructions",
            "Always cite your sources.",
            "--tool-ref",
            "web_search",
        ],
    )?;

    let show = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "show",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    assert_eq!(show.get("enabled").and_then(Value::as_bool), Some(true));
    assert_eq!(
        show.get("tool_refs")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
        "tool_ref should be stored on add"
    );

    run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "add",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--skill-id",
            "research",
            "--scope",
            "principal",
            "--name",
            "Research",
            "--description",
            "Find and cite sources",
            "--instructions",
            "Always cite your sources.",
        ],
    )?;
    let show = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "show",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    let tool_refs_empty = match show.get("tool_refs") {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.is_empty(),
        _ => false,
    };
    assert!(
        tool_refs_empty,
        "re-add without --tool-ref must clear tool_refs; got {:?}",
        show.get("tool_refs")
    );

    let disabled = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "disable",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    assert_eq!(disabled.get("updated").and_then(Value::as_u64), Some(1));
    assert_eq!(
        disabled.get("enabled").and_then(Value::as_bool),
        Some(false)
    );
    let show = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "show",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    assert_eq!(show.get("enabled").and_then(Value::as_bool), Some(false));

    let enabled = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "enable",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    assert_eq!(enabled.get("enabled").and_then(Value::as_bool), Some(true));

    let removed = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "rm",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    assert_eq!(removed.get("deleted").and_then(Value::as_u64), Some(1));
    let list = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "list",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
        ],
    )?;
    assert_eq!(list.get("count").and_then(Value::as_u64), Some(0));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "external fixture: set HERMES_SKILLS_DIR and pass --ignored"]
async fn config_skill_import_export_roundtrip_hermes() -> Result<()> {
    let hermes_dir = std::env::var("HERMES_SKILLS_DIR").context(
        "set HERMES_SKILLS_DIR to the NousResearch/hermes-agent skills directory and pass --ignored",
    )?;
    anyhow::ensure!(
        std::path::Path::new(&hermes_dir).is_dir(),
        "HERMES_SKILLS_DIR must point to an existing Hermes skills directory: {hermes_dir}"
    );

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-skill-import-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "ok")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-skill-import-{}", Uuid::new_v4().simple());
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

    let mut serve = spawn_server_with_env(&home_dir, server_port, &[], &[])?;
    wait_for_port(server_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;
    wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let imported = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "import",
            &hermes_dir,
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--scope",
            "behavior",
        ],
    )?;
    let imported_count = imported
        .get("imported_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert!(
        imported_count >= 50,
        "expected to import many hermes skills, got {imported_count}: {imported}"
    );

    let listed = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "list",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
        ],
    )?;
    let listed_count = listed.get("count").and_then(Value::as_u64).unwrap_or(0);
    assert!(
        listed_count >= 50 && listed_count <= imported_count,
        "list count {listed_count}"
    );

    let out_dir = tempdir.path().join("export");
    let exported = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "export",
            out_dir.to_str().unwrap(),
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
        ],
    )?;
    let exported_count = exported
        .get("exported_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert_eq!(
        exported_count, listed_count,
        "export count must match distinct skills"
    );
    assert!(
        out_dir.join("notion").join("SKILL.md").is_file(),
        "exported notion/SKILL.md should exist"
    );

    let reimported = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "import",
            out_dir.to_str().unwrap(),
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--scope",
            "behavior",
        ],
    )?;
    let reimported_count = reimported
        .get("imported_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert_eq!(
        reimported_count, exported_count,
        "re-import of export must round-trip"
    );

    Ok(())
}
