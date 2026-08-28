use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_fs_routes_are_unsupported() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("unused-fs-unsupported-reply-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-codex-shim-fs-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-fs-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--write-tools",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim",
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-poll-ms",
            "100",
            "--codex-shim-timeout-secs",
            "60",
        ],
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
    initialize_config_and_thread(&mut ws, &home_dir).await?;

    for (idx, method, params) in [
        (
            0,
            "fs/readFile",
            json!({ "path": home_dir.join("file.txt").display().to_string() }),
        ),
        (
            1,
            "fs/writeFile",
            json!({
                "path": home_dir.join("file.txt").display().to_string(),
                "dataBase64": "ZGVmcmE=",
            }),
        ),
        (
            2,
            "fs/createDirectory",
            json!({
                "path": home_dir.join("dir").display().to_string(),
                "recursive": true,
            }),
        ),
        (
            3,
            "fs/getMetadata",
            json!({ "path": home_dir.display().to_string() }),
        ),
        (
            4,
            "fs/readDirectory",
            json!({ "path": home_dir.display().to_string() }),
        ),
        (
            5,
            "fs/remove",
            json!({
                "path": home_dir.join("file.txt").display().to_string(),
                "recursive": true,
                "force": true,
            }),
        ),
        (
            6,
            "fs/copy",
            json!({
                "sourcePath": home_dir.join("file.txt").display().to_string(),
                "destinationPath": home_dir.join("copy.txt").display().to_string(),
                "recursive": false,
            }),
        ),
        (
            7,
            "fs/watch",
            json!({
                "watchId": "watch-unsupported",
                "path": home_dir.display().to_string(),
            }),
        ),
        (8, "fs/unwatch", json!({ "watchId": "watch-unsupported" })),
    ] {
        let id = request_id(501 + idx);
        send_raw_client_request(&mut ws, id.clone(), method, params).await?;
        let error = read_error_response(&mut ws, id).await?;
        assert_eq!(error.code, -32601);
        assert!(
            error.message.contains("unsupported Codex shim method"),
            "unexpected fs/* unsupported message for {method}: {error:?}"
        );
        assert!(
            error
                .message
                .contains("model filesystem activity must run through GENTS"),
            "fs/* error should describe the GENTS tool-call boundary for {method}: {error:?}"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_host_runtime_routes_cover_low_risk_paths() -> Result<()> {
    require_command("git")?;
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("unused-host-runtime-reply-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-codex-shim-host-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-host-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--write-tools",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim",
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-poll-ms",
            "100",
            "--codex-shim-timeout-secs",
            "60",
        ],
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
    initialize_config_and_thread(&mut ws, &home_dir).await?;

    send_raw_client_request(
        &mut ws,
        request_id(551),
        "command/exec",
        json!({
            "command": ["/bin/sh", "-lc", "printf gents-host-exec"],
            "cwd": home_dir.display().to_string(),
            "timeoutMs": 5000,
        }),
    )
    .await?;
    let exec_error = read_error_response(&mut ws, request_id(551)).await?;
    assert_eq!(exec_error.code, -32601);
    assert!(exec_error.message.contains("GENTS tool-call"));

    send_raw_client_request(
        &mut ws,
        request_id(581),
        "process/spawn",
        json!({
            "command": ["/bin/sh", "-lc", "printf gents-process-spawn"],
            "processHandle": format!("process-{}", Uuid::new_v4().simple()),
            "cwd": home_dir.display().to_string(),
            "streamStdoutStderr": true,
            "timeoutMs": 5000,
        }),
    )
    .await?;
    let process_error = read_error_response(&mut ws, request_id(581)).await?;
    assert_eq!(process_error.code, -32601);
    assert!(process_error
        .message
        .contains("managed-exec state machines"));

    fs::write(home_dir.join("alpha_notes.txt"), "alpha")?;
    fs::create_dir_all(home_dir.join("nested"))?;
    fs::write(home_dir.join("nested/beta_alpha.md"), "alpha")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::FuzzyFileSearch {
            request_id: request_id(552),
            params: codex::FuzzyFileSearchParams {
                query: "alpha".to_string(),
                roots: vec![home_dir.display().to_string()],
                cancellation_token: None,
            },
        },
    )
    .await?;
    let fuzzy: codex::FuzzyFileSearchResponse =
        read_typed_response(&mut ws, request_id(552)).await?;
    assert!(
        fuzzy
            .files
            .iter()
            .any(|file| file.path == "alpha_notes.txt" && file.file_name == "alpha_notes.txt"),
        "fuzzy search did not include alpha_notes.txt: {fuzzy:?}"
    );

    let session_id = format!("fuzzy-{}", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::FuzzyFileSearchSessionStart {
            request_id: request_id(553),
            params: codex::FuzzyFileSearchSessionStartParams {
                session_id: session_id.clone(),
                roots: vec![home_dir.display().to_string()],
            },
        },
    )
    .await?;
    let _: codex::FuzzyFileSearchSessionStartResponse =
        read_typed_response(&mut ws, request_id(553)).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::FuzzyFileSearchSessionUpdate {
            request_id: request_id(554),
            params: codex::FuzzyFileSearchSessionUpdateParams {
                session_id: session_id.clone(),
                query: "beta".to_string(),
            },
        },
    )
    .await?;
    let _: codex::FuzzyFileSearchSessionUpdateResponse =
        read_typed_response(&mut ws, request_id(554)).await?;
    let fuzzy_update = read_fuzzy_file_search_update(&mut ws).await?;
    assert_eq!(fuzzy_update.session_id, session_id);
    assert_eq!(fuzzy_update.query, "beta");
    assert!(
        fuzzy_update
            .files
            .iter()
            .any(|file| file.path == "nested/beta_alpha.md"),
        "fuzzy search session update did not include nested/beta_alpha.md: {fuzzy_update:?}"
    );
    let fuzzy_completed = read_fuzzy_file_search_completed(&mut ws).await?;
    assert_eq!(fuzzy_completed.session_id, session_id);

    send_client_request(
        &mut ws,
        codex::ClientRequest::FuzzyFileSearchSessionStop {
            request_id: request_id(555),
            params: codex::FuzzyFileSearchSessionStopParams {
                session_id: session_id.clone(),
            },
        },
    )
    .await?;
    let _: codex::FuzzyFileSearchSessionStopResponse =
        read_typed_response(&mut ws, request_id(555)).await?;

    let repo = home_dir.join("git-repo");
    fs::create_dir_all(&repo)?;
    run_git_command(&repo, &["init"])?;
    fs::write(repo.join("tracked.txt"), "base\n")?;
    run_git_command(&repo, &["add", "tracked.txt"])?;
    run_git_command(
        &repo,
        &[
            "-c",
            "user.name=Gents Test",
            "-c",
            "user.email=gents-test@example.invalid",
            "commit",
            "-m",
            "base",
        ],
    )?;
    fs::write(repo.join("tracked.txt"), "base\nchanged\n")?;
    fs::write(repo.join("untracked.txt"), "new\n")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::GitDiffToRemote {
            request_id: request_id(556),
            params: codex::GitDiffToRemoteParams { cwd: repo },
        },
    )
    .await?;
    let diff: codex::GitDiffToRemoteResponse =
        read_typed_response(&mut ws, request_id(556)).await?;
    assert!(
        diff.diff.contains("+changed"),
        "git diff did not include tracked change: {diff:?}"
    );
    assert!(
        diff.diff.contains("untracked.txt"),
        "git diff did not include untracked file: {diff:?}"
    );

    Ok(())
}
