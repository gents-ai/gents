use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_model_list_enumerates_backend_models() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_backend_id = default_backend_id(&agent_did);
    let default_model_selection = gents_model_selection_id(&default_backend_id, &model_name);
    let extra_model_name = format!("mock-codex-shim-extra-model-{}", Uuid::new_v4().simple());
    let extra_endpoint = MockChatEndpoint::start(&extra_model_name, "irrelevant")?;
    let extra_backend_id = format!("extra-backend-{}", Uuid::new_v4().simple());
    let extra_model_selection = gents_model_selection_id(&extra_backend_id, &extra_model_name);
    let duplicate_backend_id = format!("duplicate-backend-{}", Uuid::new_v4().simple());
    let duplicate_model_selection = gents_model_selection_id(&duplicate_backend_id, &model_name);

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;
    let generation =
        wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let create_extra_backend = format!(
        r#"mutation {{
            create_InferenceBackend(input: {{
                agent_did: "{agent_did}",
                backend_id: "{extra_backend_id}",
                name: "Extra Backend",
                provider_kind: "OpenAiCompatible",
                openai_wire_api: "chat_completions",
                endpoint: "{}",
                auth: {{kind: "unauthenticated"}},
                max_concurrent: 1,
                max_queue_depth: 100,
                enabled: true
            }}) {{ _docID }}
            create_duplicate: create_InferenceBackend(input: {{
                agent_did: "{agent_did}",
                backend_id: "{duplicate_backend_id}",
                name: "Duplicate Backend",
                provider_kind: "OpenAiCompatible",
                openai_wire_api: "chat_completions",
                endpoint: "{}",
                auth: {{kind: "unauthenticated"}},
                max_concurrent: 1,
                max_queue_depth: 100,
                enabled: true
            }}) {{ _docID }}
        }}"#,
        escape_graphql_string(extra_endpoint.endpoint()),
        escape_graphql_string(extra_endpoint.endpoint())
    );
    serve
        .capturing(graphql_query(&graphql, &create_extra_backend))
        .await?;
    seed_backend_catalog(&graphql, &agent_did, &extra_backend_id, &extra_model_name).await?;
    seed_backend_catalog(&graphql, &agent_did, &duplicate_backend_id, &model_name).await?;
    serve
        .capturing(wait_for_runtime_quiescence(
            &graphql,
            &agent_did,
            generation + 1,
            Duration::from_secs(2),
        ))
        .await?;

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "gents-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _initialize: codex::InitializeResponse =
        read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ModelList {
            request_id: request_id(2),
            params: codex::ModelListParams::default(),
        },
    )
    .await?;
    let model_list: codex::ModelListResponse = read_typed_response(&mut ws, request_id(2)).await?;

    let ids: Vec<&str> = model_list
        .data
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert!(
        ids.contains(&default_model_selection.as_str()),
        "expected default model selection {default_model_selection} in model list; got {ids:?}"
    );
    assert!(
        ids.contains(&extra_model_selection.as_str()),
        "expected extra model selection {extra_model_selection} in model list; got {ids:?}"
    );
    assert!(
        ids.contains(&duplicate_model_selection.as_str()),
        "expected duplicate model selection {duplicate_model_selection} in model list; got {ids:?}"
    );
    let default_entry = model_list
        .data
        .iter()
        .find(|entry| entry.id == default_model_selection)
        .expect("default model present");
    assert_eq!(default_entry.model, default_model_selection);
    assert_eq!(default_entry.display_name, model_name);
    assert!(
        default_entry.is_default,
        "default model should be flagged as isDefault"
    );
    let extra_entry = model_list
        .data
        .iter()
        .find(|entry| entry.id == extra_model_selection)
        .expect("extra model present");
    assert!(
        !extra_entry.is_default,
        "non-default model must not be flagged isDefault"
    );
    let duplicate_entry = model_list
        .data
        .iter()
        .find(|entry| entry.id == duplicate_model_selection)
        .expect("duplicate backend model present");
    assert_eq!(duplicate_entry.display_name, model_name);
    assert!(
        !duplicate_entry.is_default,
        "duplicate backend model must not be flagged isDefault"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_config_read_reflects_doc_mutation() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = format!("{agent_did}:default");
    let default_backend_id = default_backend_id(&agent_did);
    let alt_model_name = format!("alt-model-{}", Uuid::new_v4().simple());
    let alt_model_selection = gents_model_selection_id(&default_backend_id, &alt_model_name);

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;
    wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let switch_profile = format!(
        r#"mutation {{
            update_InferenceProfile(
                filter: {{ profile_id: {{ _eq: "{default_behavior_id}-profile" }} }},
                input: {{ model_name: "{alt_model_name}" }}
            ) {{ _docID }}
        }}"#
    );
    serve
        .capturing(graphql_query(&graphql, &switch_profile))
        .await?;

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "gents-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _initialize: codex::InitializeResponse =
        read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ConfigRead {
            request_id: request_id(2),
            params: codex::ConfigReadParams {
                include_layers: false,
                cwd: None,
            },
        },
    )
    .await?;
    let config: codex::ConfigReadResponse = read_typed_response(&mut ws, request_id(2)).await?;
    assert_eq!(
        config.config.model.as_deref(),
        Some(alt_model_selection.as_str()),
        "ConfigRead should reflect the doc-mutated backend-qualified model selection"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_config_value_write_model_mutates_behavior() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = format!("{agent_did}:default");
    let original_profile_id = format!("{agent_did}:default-profile");
    let alt_model_name = format!("mock-codex-shim-alt-model-{}", Uuid::new_v4().simple());
    let alt_endpoint = MockChatEndpoint::start(&alt_model_name, "irrelevant")?;
    let alt_backend_id = format!("alt-backend-{}", Uuid::new_v4().simple());
    let alt_model_selection = gents_model_selection_id(&alt_backend_id, &alt_model_name);

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;
    let generation =
        wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let create_alt_backend = format!(
        r#"mutation {{
            create_InferenceBackend(input: {{
                agent_did: "{agent_did}",
                backend_id: "{alt_backend_id}",
                name: "Alt Backend",
                provider_kind: "OpenAiCompatible",
                openai_wire_api: "chat_completions",
                endpoint: "{}",
                auth: {{kind: "unauthenticated"}},
                max_concurrent: 1,
                max_queue_depth: 100,
                enabled: true
            }}) {{ _docID }}
        }}"#,
        escape_graphql_string(alt_endpoint.endpoint())
    );
    serve
        .capturing(graphql_query(&graphql, &create_alt_backend))
        .await?;
    seed_backend_catalog(&graphql, &agent_did, &alt_backend_id, &alt_model_name).await?;
    serve
        .capturing(wait_for_runtime_quiescence(
            &graphql,
            &agent_did,
            generation + 1,
            Duration::from_secs(2),
        ))
        .await?;

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "gents-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _initialize: codex::InitializeResponse = serve
        .capturing(read_typed_response(&mut ws, request_id(1)))
        .await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ConfigValueWrite {
            request_id: request_id(2),
            params: codex::ConfigValueWriteParams {
                key_path: "model".to_string(),
                value: serde_json::Value::String(alt_model_selection),
                merge_strategy: codex::MergeStrategy::Replace,
                file_path: None,
                expected_version: None,
            },
        },
    )
    .await?;
    let _write: codex::ConfigWriteResponse = serve
        .capturing(read_typed_response(&mut ws, request_id(2)))
        .await?;

    let resp = serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"{{
                AgentBehavior(
                    filter: {{ behavior_id: {{ _eq: "{default_behavior_id}" }} }},
                    limit: 1
                ) {{ inference_profile_id }}
            }}"#
            ),
        ))
        .await?;
    let stored_profile = resp
        .pointer("/data/AgentBehavior/0/inference_profile_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    assert_ne!(
        stored_profile, original_profile_id,
        "model selection must copy the profile instead of mutating the shared original"
    );
    let resp = serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"{{ InferenceProfile(filter: {{agent_did: {{_eq: "{agent_did}"}}, profile_id: {{_eq: "{}"}}}}, limit: 1) {{backend_id model_name}} }}"#,
                escape_graphql_string(&stored_profile)
            ),
        ))
        .await?;
    assert_eq!(
        resp["data"]["InferenceProfile"][0]["backend_id"],
        alt_backend_id
    );
    assert_eq!(
        resp["data"]["InferenceProfile"][0]["model_name"],
        alt_model_name
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_config_value_write_rejects_unknown_model() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = format!("{agent_did}:default");
    let original_backend_id = format!("{agent_did}:backend");
    let original_profile_id = format!("{agent_did}:default-profile");

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "gents-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _initialize: codex::InitializeResponse =
        read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ConfigValueWrite {
            request_id: request_id(2),
            params: codex::ConfigValueWriteParams {
                key_path: "model".to_string(),
                value: serde_json::Value::String("definitely-not-real".to_string()),
                merge_strategy: codex::MergeStrategy::Replace,
                file_path: None,
                expected_version: None,
            },
        },
    )
    .await?;
    let error = read_error_response(&mut ws, request_id(2)).await?;
    assert_eq!(
        error.message,
        "no backend advertises model \"definitely-not-real\" in this principal's credential scope"
    );

    let resp = serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"{{
                AgentBehavior(
                    filter: {{ behavior_id: {{ _eq: "{default_behavior_id}" }} }},
                    limit: 1
                ) {{ inference_profile_id }}
            }}"#
            ),
        ))
        .await?;
    let stored_profile = resp
        .pointer("/data/AgentBehavior/0/inference_profile_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    assert_eq!(
        stored_profile, original_profile_id,
        "behavior inference_profile_id must remain unchanged after rejected write"
    );
    let profile = serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"{{ InferenceProfile(filter: {{agent_did: {{_eq: "{agent_did}"}}, profile_id: {{_eq: "{original_profile_id}"}}}}, limit: 1) {{backend_id model_name}} }}"#,
            ),
        ))
        .await?;
    assert_eq!(
        profile["data"]["InferenceProfile"][0]["backend_id"],
        original_backend_id
    );
    assert_eq!(
        profile["data"]["InferenceProfile"][0]["model_name"],
        model_name
    );
    Ok(())
}
