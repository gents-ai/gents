use super::*;

pub(super) struct LiveCodexShim {
    pub(super) tempdir: tempfile::TempDir,
    pub(super) home_dir: std::path::PathBuf,
    pub(super) codex_home: std::path::PathBuf,
    pub(super) graphql: String,
    pub(super) agent_did: String,
    pub(super) behavior_id: String,
    tools_id: String,
    pub(super) backend_id: String,
    pub(super) inference_profile_id: String,
    pub(super) model_name: String,
    pub(super) shim_port: u16,
    pub(super) shim_trace: std::path::PathBuf,
    pub(super) _server: ServeProcess,
}

pub(super) async fn start_live_codex_shim() -> Result<LiveCodexShim> {
    start_live_codex_shim_with_write_tools(false, None).await
}

pub(super) fn create_existing_client_codex_home(
    smoke: &LiveCodexShim,
    label: &str,
) -> Result<std::path::PathBuf> {
    let codex_home = smoke
        .tempdir
        .path()
        .join(format!("client-codex-home-{label}"));
    fs::create_dir_all(&codex_home)
        .with_context(|| format!("creating client Codex home {}", codex_home.display()))?;
    fs::write(
        codex_home.join("config.toml"),
        "# Existing user Codex config should remain client-side.\n",
    )
    .with_context(|| format!("writing client Codex config in {}", codex_home.display()))?;
    Ok(codex_home)
}

pub(super) async fn start_live_codex_shim_with_write_tools(
    write_tools: bool,
    tool_root: Option<&std::path::Path>,
) -> Result<LiveCodexShim> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-live-{}", Uuid::new_v4().simple());
    let tool_root_string = tool_root.map(|root| root.to_string_lossy().to_string());
    let model_endpoint = std::env::var("GENTS_CLI_E2E_MODEL_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_MODEL_ENDPOINT.to_string());
    let model_name = std::env::var("GENTS_CLI_E2E_MODEL_NAME")
        .unwrap_or_else(|_| DEFAULT_MODEL_NAME.to_string());
    let mut init_args = vec![
        "--agent-name",
        &agent_name,
        "--model-name",
        model_name.as_str(),
        "--inference-url",
        model_endpoint.as_str(),
    ];
    if std::env::var_os("GENTS_CLI_E2E_API_KEY").is_some() {
        init_args.push("--api-key-env-var");
        init_args.push("GENTS_CLI_E2E_API_KEY");
    }
    if write_tools {
        init_args.push("--write");
    }
    if let Some(tool_root) = &tool_root_string {
        init_args.push("--tool-root");
        init_args.push(tool_root.as_str());
    }
    let init = run_init_json(&home_dir, &init_args)?;
    let agent_did = agent_did_from_init(&init)?;
    let behavior_id = init_output_string(&init, "default_behavior_id")?;
    let tools_id = init_output_string(&init, "tools_id")?;
    let backend_id = init_output_string(&init, "backend_id")?;
    let inference_profile_id = init_output_string(&init, "inference_profile_id")?;
    let model_name = init_output_string(&init, "model_name")?;
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let codex_home = home_dir.join(".gents").join("codex-ui");
    let shim_trace = codex_home.join("log").join("codex-shim-events.jsonl");
    let mut server = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-poll-ms",
            "250",
            "--codex-shim-timeout-secs",
            LIVE_CODEX_SHIM_TIMEOUT_SECS,
        ],
        &[("RUST_LOG", "error,gents_cli::commands::codex_shim=info")],
    )?;
    wait_for_port(server_port, &mut server)?;
    wait_for_port(shim_port, &mut server)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    Ok(LiveCodexShim {
        codex_home,
        tempdir,
        home_dir,
        graphql,
        agent_did,
        behavior_id,
        tools_id,
        backend_id,
        inference_profile_id,
        model_name,
        shim_port,
        shim_trace,
        _server: server,
    })
}

fn init_output_string(init: &Value, key: &str) -> Result<String> {
    let nested = format!("/init/{key}");
    init.get(key)
        .or_else(|| init.pointer(&nested))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("init output missing {key}: {init}"))
}
