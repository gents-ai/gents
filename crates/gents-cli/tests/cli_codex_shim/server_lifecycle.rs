use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_handshake_requires_configured_bearer_token() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-auth-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "unused")?;
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            "authenticated-shim",
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let server_port = allocate_port()?;
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-auth-token-env",
            "GENTS_SHIM_TEST_TOKEN",
        ],
        &[("GENTS_SHIM_TEST_TOKEN", "correct-secret")],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    let url = format!("ws://127.0.0.1:{shim_port}/");
    let health = reqwest::get(format!("http://127.0.0.1:{server_port}/healthz"))
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(
        health
            .pointer("/checks/codex_shim/auth_required")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        health
            .pointer("/checks/codex_shim/bound_agent_did")
            .and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert!(!health.to_string().contains("correct-secret"));

    let unauthenticated = connect_async(&url)
        .await
        .expect_err("missing token rejected");
    assert_http_status(unauthenticated, StatusCode::UNAUTHORIZED)?;

    let mut wrong_request = url.clone().into_client_request()?;
    wrong_request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer wrong-secret"),
    );
    let wrong = connect_async(wrong_request)
        .await
        .expect_err("wrong token rejected");
    assert_http_status(wrong, StatusCode::UNAUTHORIZED)?;

    let mut request = url.into_client_request()?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer correct-secret"),
    );
    let (mut websocket, _) = connect_async(request).await?;
    websocket.close(None).await?;
    Ok(())
}

fn assert_http_status(
    error: tokio_tungstenite::tungstenite::Error,
    expected: StatusCode,
) -> Result<()> {
    let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
        bail!("expected HTTP {expected}, got {error}");
    };
    if response.status() != expected {
        bail!("expected HTTP {expected}, got {}", response.status());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_keeps_running_when_codex_shim_port_is_taken() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-shim-degrade-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "unused")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-shim-degrade-{}", Uuid::new_v4().simple());
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

    let occupied = std::net::TcpListener::bind("127.0.0.1:0").context("occupying a port")?;
    let shim_port = occupied.local_addr()?.port();
    let shim_port_string = shim_port.to_string();
    let (mut serve, readiness) = spawn_server_with_ready_json(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;

    assert_eq!(
        readiness
            .pointer("/codex_shim/disabled")
            .and_then(Value::as_bool),
        Some(true),
        "server readiness must report the shim as disabled: {readiness}"
    );

    let (_stdout, stderr) = serve.captured_output()?;
    assert!(
        stderr.contains("Codex endpoint disabled"),
        "server should report the degraded Codex endpoint; stderr:\n{stderr}"
    );
    drop(occupied);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_waits_for_a_missing_bound_behavior_instead_of_disabling() -> Result<()> {
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
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let (mut serve, readiness) = spawn_server_with_ready_json(
        &home_dir,
        server_port,
        &[
            "--codex-shim",
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-behavior-id",
            "behavior-that-does-not-exist",
        ],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;

    assert_eq!(
        readiness
            .pointer("/codex_shim/pending")
            .and_then(Value::as_bool),
        Some(true),
        "server readiness must report the shim as pending: {readiness}"
    );
    assert_eq!(
        readiness
            .pointer("/codex_shim/bound_behavior_id")
            .and_then(Value::as_str),
        Some("behavior-that-does-not-exist"),
        "server readiness must name the missing bound behavior: {readiness}"
    );

    let (_stdout, stderr) = serve.captured_output()?;
    assert!(
        stderr.contains("Codex endpoint pending"),
        "a missing bound behavior is suppliable, so the shim must wait rather than \
         disable itself; got:\n{stderr}"
    );
    assert!(
        !stderr.contains("Codex endpoint disabled"),
        "a missing behavior must not be reported as a terminal disable (#699); got:\n{stderr}"
    );
    assert!(
        stderr.contains("behavior-that-does-not-exist"),
        "expected stderr to name the behavior it is waiting for; got:\n{stderr}"
    );
    assert!(
        stderr.contains("no restart needed"),
        "the operator must be told the shim converges on its own; got:\n{stderr}"
    );
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", shim_port)).is_err(),
        "the shim port must stay closed while its bound behavior does not exist"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_binds_when_config_apply_supplies_its_behavior() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
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
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;

    let root_str = root.to_str().expect("utf-8 root");
    run_cli_text(&home_dir, &["config", "export", "--root", root_str])?;

    const LATE_BEHAVIOR: &str = "late-arriving-behavior";
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim",
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-behavior-id",
            LATE_BEHAVIOR,
        ],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;
    wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    assert!(
        std::net::TcpStream::connect(("127.0.0.1", shim_port)).is_err(),
        "the shim must not listen before its bound behavior exists"
    );

    let behaviors_dir = root.join("agent-behaviors");
    let existing = fs::read_dir(&behaviors_dir)
        .context("reading agent-behaviors dir after export")?
        .next()
        .ok_or_else(|| anyhow!("no agent-behavior subdirs after export"))??;
    let late_dir = behaviors_dir.join(LATE_BEHAVIOR);
    fs::create_dir_all(&late_dir)?;
    for entry in fs::read_dir(existing.path()).context("reading exported behavior dir")? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            fs::copy(entry.path(), late_dir.join(entry.file_name()))?;
        }
    }
    let mut behavior = read_json_file(&late_dir.join("object.json"))?;
    behavior["behavior_id"] = Value::String(LATE_BEHAVIOR.to_string());
    write_json_file(&late_dir.join("object.json"), &behavior)?;

    let applied = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        applied.get("ok").and_then(Value::as_bool),
        Some(true),
        "config apply must succeed: {applied}"
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    loop {
        if std::net::TcpStream::connect(("127.0.0.1", shim_port)).is_ok() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let (_stdout, stderr) = serve.captured_output()?;
            panic!(
                "the shim never bound after `config apply` supplied behavior {LATE_BEHAVIOR:?} \
                 — this is #699: the port stays closed until the process restarts.\nstderr:\n{stderr}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let (_stdout, stderr) = serve.captured_output()?;
    assert!(
        stderr.contains("Codex endpoint bound"),
        "the operator must see the shim converge; got:\n{stderr}"
    );
    assert!(
        stderr.contains("no restart was needed"),
        "the fix is that no restart is needed; got:\n{stderr}"
    );
    Ok(())
}
