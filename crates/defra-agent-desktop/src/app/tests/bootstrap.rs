use super::*;

use crate::client::PeerDirectory;

#[derive(Debug, serde::Deserialize)]
struct MockReplicatorRequest {
    #[serde(rename = "Addresses")]
    addresses: Vec<String>,
    #[serde(rename = "Collections")]
    collections: Vec<String>,
}

struct MockLocalRuntimeApi {
    graphql: String,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockLocalRuntimeApi {
    fn start(
        runtime: &Arc<Runtime>,
        core: Arc<ClientCore>,
        listen_address: String,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let handle = runtime.handle().clone();
        let core_for_thread = Arc::clone(&core);
        let listen_address_for_thread = listen_address.clone();
        let thread = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = match read_http_request(&mut stream) {
                            Ok(request) => request,
                            Err(_) => {
                                let _ = stream.shutdown(Shutdown::Both);
                                continue;
                            }
                        };
                        let response = match (request.method.as_str(), request.path.as_str()) {
                            ("GET", "/api/v0/node/identity") => Ok((
                                "200 OK",
                                "application/json",
                                serde_json::json!({
                                    "peer_id": core_for_thread.local_peer_id(),
                                })
                                .to_string(),
                            )),
                            ("GET", "/api/v0/p2p/info") => Ok((
                                "200 OK",
                                "application/json",
                                serde_json::to_string(&vec![listen_address_for_thread.clone()])
                                    .expect("serialize mock p2p info"),
                            )),
                            ("GET", "/api/v0/p2p/shareable-address") => Ok((
                                "200 OK",
                                "application/json",
                                serde_json::json!({
                                    "address": listen_address_for_thread.clone(),
                                })
                                .to_string(),
                            )),
                            ("POST", "/api/v0/p2p/connect") => {
                                let connect_result =
                                    serde_json::from_str::<Vec<String>>(&request.body)
                                        .context("decoding /p2p/connect payload")
                                        .and_then(|addresses| {
                                            let target = addresses
                                                .first()
                                                .cloned()
                                                .context("missing desktop listen address")?;
                                            handle.block_on(async {
                                                core_for_thread
                                                    .p2p()
                                                    .connect_peer(&target)
                                                    .await
                                                    .map_err(anyhow::Error::msg)
                                            })
                                        });
                                connect_result.map(|_| {
                                    (
                                        "200 OK",
                                        "application/json",
                                        r#"{"status":"ok"}"#.to_string(),
                                    )
                                })
                            }
                            ("POST", "/api/v0/p2p/collections") => {
                                serde_json::from_str::<Vec<String>>(&request.body)
                                    .context("decoding /p2p/collections payload")
                                    .map(|_| {
                                        (
                                            "200 OK",
                                            "application/json",
                                            r#"{"status":"ok"}"#.to_string(),
                                        )
                                    })
                            }
                            ("POST", "/api/v0/p2p/replicators") => {
                                let replicator =
                                    serde_json::from_str::<MockReplicatorRequest>(&request.body)
                                        .context("decoding /p2p/replicators payload")
                                        .and_then(|payload| {
                                            let target =
                                                payload.addresses.first().cloned().context(
                                                    "missing desktop replicator address",
                                                )?;
                                            handle.block_on(async {
                                                set_replicator_with_retry(
                                                    core_for_thread.as_ref(),
                                                    &target,
                                                    "mock runtime bootstrap replicator",
                                                    payload.collections,
                                                )
                                                .await
                                            })
                                        });
                                replicator.map(|_| {
                                    (
                                        "200 OK",
                                        "application/json",
                                        r#"{"status":"ok"}"#.to_string(),
                                    )
                                })
                            }
                            _ => Ok((
                                "404 Not Found",
                                "application/json",
                                r#"{"error":"not found"}"#.to_string(),
                            )),
                        };

                        let (status, content_type, body) = match response {
                            Ok(response) => response,
                            Err(error) => (
                                "500 Internal Server Error",
                                "application/json",
                                serde_json::json!({ "error": error.to_string() }).to_string(),
                            ),
                        };
                        let _ = write_http_response(&mut stream, status, content_type, &body);
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            graphql: format!("http://127.0.0.1:{port}/api/v0/graphql"),
            port,
            stop,
            handle: Some(thread),
        })
    }

    fn graphql_url(&self) -> &str {
        &self.graphql
    }
}

impl Drop for MockLocalRuntimeApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn bootstrap_core_options() -> ClientCoreOptions {
    let mut options = ClientCoreOptions::local_only();
    options.rate_limit_burst = 5_000;
    options.rate_limit_rate = 500.0;
    options
}

fn seed_agent_home_runtime_state(
    agent_home: &std::path::Path,
    agent_name: &str,
    agent_did: &str,
    graphql: &str,
    peer_id: &str,
    listen_address: &str,
) -> Result<()> {
    std::fs::create_dir_all(agent_home)?;
    std::fs::write(
        agent_home.join("init.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "agent_name": agent_name,
            "agent_did": agent_did,
        }))?,
    )?;
    std::fs::write(
        agent_home.join("runtime.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "graphql": graphql,
            "agent_name": agent_name,
            "agent_did": agent_did,
            "p2p_transport": "iroh",
            "p2p_peer_id": peer_id,
            "p2p_listen_addresses": [listen_address],
        }))?,
    )?;
    Ok(())
}

#[test]
fn desktop_bootstrap_init_launch_and_gui_chat_round_trip_without_manual_refresh() -> Result<()> {
    init_test_tracing();

    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let mock_endpoint = MockModelEndpoint::start("default")?;
    let backend = AgentBackendConfig::mock(mock_endpoint.endpoint());

    let remote_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("remote")),
        bootstrap_core_options(),
    ))?);
    let agent_name = "bootstrap-demo";
    let running_agent = runtime.block_on(spawn_backed_agent(
        remote_core.node_arc(),
        tempdir.path().join("agent").join("bootstrap-demo.key"),
        agent_name,
        &backend,
    ))?;
    let docs = runtime.block_on(seed_live_operator_documents(
        remote_core.as_ref(),
        &running_agent.did,
        agent_name,
        &backend,
    ))?;
    let remote_addr = runtime.block_on(wait_for_connectable_iroh_addr(
        remote_core.as_ref(),
        "mock runtime",
    ))?;
    let runtime_api =
        MockLocalRuntimeApi::start(&runtime, Arc::clone(&remote_core), remote_addr.clone())?;

    let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
    let agent_home = tempdir.path().join("agent-home");
    seed_agent_home_runtime_state(
        &agent_home,
        agent_name,
        &running_agent.did,
        runtime_api.graphql_url(),
        remote_core.local_peer_id(),
        &remote_addr,
    )?;

    let init_summary = runtime.block_on(crate::local_runtime::init_standard_local_runtime(
        crate::local_runtime::DesktopInitOptions {
            agent_home: agent_home.clone(),
            desktop_paths: desktop_paths.clone(),
            label: "Bootstrap Demo".to_string(),
        },
    ))?;
    assert_eq!(init_summary.status, "initialized");
    assert_eq!(init_summary.graphql, runtime_api.graphql_url());
    assert_eq!(init_summary.p2p_transport, "iroh");

    let peer_record = runtime.block_on(async {
        let directory = PeerDirectory::load(desktop_paths.peer_directory_path()).await?;
        directory
            .records()
            .first()
            .cloned()
            .context("desktop init did not persist a peer record")
    })?;

    let desktop_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        desktop_paths,
        bootstrap_core_options(),
    ))?;
    assert!(
        desktop_core.bootstrap_errors().is_empty(),
        "unexpected bootstrap errors: {:?}",
        desktop_core.bootstrap_errors()
    );
    runtime.block_on(wait_for_connected_peer(
        &desktop_core,
        remote_core.local_peer_id(),
        "desktop bootstrap",
    ))?;
    runtime.block_on(wait_for_connected_peer(
        remote_core.as_ref(),
        desktop_core.local_peer_id(),
        "mock runtime bootstrap",
    ))?;
    drop(runtime_api);

    wait_for_value(
        "bootstrapped behavior docs on desktop",
        Duration::from_secs(20),
        || {
            desktop_core
                .store()
                .snapshot()
                .behaviors
                .iter()
                .find(|row| row.behavior_id == docs.behavior_id)
                .map(|row| row.behavior_id.clone())
        },
    )?;
    wait_for_value(
        "bootstrapped inference backend docs on desktop",
        Duration::from_secs(20),
        || {
            desktop_core
                .store()
                .snapshot()
                .inference_backends
                .iter()
                .find(|row| row.backend_id == docs.backend_id)
                .map(|row| row.backend_id.clone())
        },
    )?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(desktop_core)),
        Vec::new(),
        global_log_store(),
    );
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);

    wait_for_value("desktop bootstrap status", Duration::from_secs(10), || {
        let texts = driver.render();
        texts
            .iter()
            .any(|text| text.contains("replication: subscriptions armed"))
            .then_some(texts)
    })?;

    let deployment_target = audit::targets::chat_deployment(&peer_record.peer_id);
    driver.wait_for_target(
        "bootstrapped deployment row",
        Duration::from_secs(10),
        &deployment_target,
    )?;
    driver.click_target(&deployment_target);
    assert_eq!(
        driver.app.state.chat.selected_peer_id.as_deref(),
        Some(peer_record.peer_id.as_str())
    );
    assert_eq!(
        driver.app.state.chat.selected_agent_did.as_deref(),
        Some(running_agent.did.as_str())
    );

    let session_id = ensure_chat_session_selected(
        &mut driver,
        "desktop-created session",
        Duration::from_secs(10),
    )?;

    driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
    driver.type_text("say hello from the saved-peer bootstrap journey");
    driver.click_target(audit::targets::CHAT_SEND);
    assert_eq!(driver.app.state.chat.last_submission_error, None);
    assert!(driver.app.state.chat.composer_text.is_empty());

    let request_id = wait_for_value(
        "bootstrapped focused request id",
        Duration::from_secs(10),
        || {
            driver
                .app
                .client
                .as_ref()
                .and_then(|client| client.store().focused_request_id())
        },
    )?;
    wait_for_value(
        "mock runtime received bootstrapped request",
        Duration::from_secs(20),
        || {
            runtime
                .block_on(query_has_row_by_unique_field(
                    remote_core.as_ref(),
                    "AgentRequest",
                    "request_id",
                    &request_id,
                ))
                .ok()
                .filter(|received| *received)
                .map(|_| ())
        },
    )?;
    wait_for_value(
        "bootstrapped response row on desktop",
        Duration::from_secs(30),
        || {
            driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .latest_response_for_request(&request_id)
                    .and_then(|row| row.content.as_deref())
                    .filter(|content| !content.trim().is_empty())
                    .map(str::to_string)
            })
        },
    )?;
    let transcript = wait_for_value(
        "bootstrapped transcript response",
        Duration::from_secs(30),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains("mock response"))
                .then_some(texts)
        },
    )?;
    assert!(transcript
        .iter()
        .any(|text| { text.contains("say hello from the saved-peer bootstrap journey") }));
    assert_eq!(
        driver.app.state.chat.selected_session_id.as_deref(),
        Some(session_id.as_str())
    );

    driver.app.shutdown_client();
    drop(driver);
    runtime.block_on(running_agent.shutdown())?;
    runtime.block_on(remote_core.shutdown())?;
    Ok(())
}

#[test]
fn desktop_bootstrap_multi_agent_gui_switching_round_trip_without_manual_refresh() -> Result<()> {
    init_test_tracing();

    let mock_endpoint = MockModelEndpoint::start("default")?;
    let backend = AgentBackendConfig::mock(mock_endpoint.endpoint());
    let mut fixture = build_multi_agent_desktop_fixture_with_backend(
        "audit-bootstrap-multi-agent",
        &backend,
        global_log_store(),
    )?;
    assert_eq!(fixture.deployments.len(), 2);

    let alpha = live_deployment_case(&fixture.deployments[0]);
    let bravo = live_deployment_case(&fixture.deployments[1]);

    let (alpha_submission, bravo_submission);
    {
        let driver = &mut fixture.driver;
        alpha_submission =
            submit_live_prompt_for_deployment(driver, &alpha, "ALPHA_BOOTSTRAP_READY")?;
        bravo_submission =
            submit_live_prompt_for_deployment(driver, &bravo, "BRAVO_BOOTSTRAP_READY")?;
    }

    wait_for_value(
        "alpha mock runtime received bootstrapped request",
        Duration::from_secs(20),
        || {
            fixture
                .runtime
                .block_on(query_has_row_by_unique_field(
                    alpha.remote_core,
                    "AgentRequest",
                    "request_id",
                    &alpha_submission.request_id,
                ))
                .ok()
                .filter(|received| *received)
                .map(|_| ())
        },
    )?;
    wait_for_value(
        "bravo mock runtime received bootstrapped request",
        Duration::from_secs(20),
        || {
            fixture
                .runtime
                .block_on(query_has_row_by_unique_field(
                    bravo.remote_core,
                    "AgentRequest",
                    "request_id",
                    &bravo_submission.request_id,
                ))
                .ok()
                .filter(|received| *received)
                .map(|_| ())
        },
    )?;

    {
        let driver = &mut fixture.driver;
        driver.open_activity(Activity::Chat);
        driver.click_target(&audit::targets::chat_deployment(&alpha.peer_id));
        assert_chat_context(driver, &alpha, None);
        let alpha_texts = driver.click_target(&audit::targets::chat_conversation(
            &alpha_submission.session_id,
        ));
        assert_chat_context(driver, &alpha, Some(alpha_submission.session_id.as_str()));
        assert!(alpha_texts
            .iter()
            .any(|text| text.contains(alpha_submission.prompt.as_str())));
        assert!(alpha_texts
            .iter()
            .any(|text| text.contains(alpha_submission.response.trim())));
        assert!(
            !alpha_texts
                .iter()
                .any(|text| text.contains(bravo_submission.prompt.as_str())),
            "alpha transcript leaked bravo prompt after bootstrap switching"
        );

        driver.click_target(&audit::targets::chat_deployment(&bravo.peer_id));
        assert_chat_context(driver, &bravo, None);
        let bravo_texts = driver.click_target(&audit::targets::chat_conversation(
            &bravo_submission.session_id,
        ));
        assert_chat_context(driver, &bravo, Some(bravo_submission.session_id.as_str()));
        assert!(bravo_texts
            .iter()
            .any(|text| text.contains(bravo_submission.prompt.as_str())));
        assert!(bravo_texts
            .iter()
            .any(|text| text.contains(bravo_submission.response.trim())));
        assert!(
            !bravo_texts
                .iter()
                .any(|text| text.contains(alpha_submission.prompt.as_str())),
            "bravo transcript leaked alpha prompt after bootstrap switching"
        );

        driver.open_activity(Activity::Operator);
        driver.click_target(&audit::targets::operator_deployment(&alpha.peer_id));
        driver.click_target(&audit::targets::operator_agent(&alpha.agent_did));
        driver.click_target(&audit::targets::operator_section(
            OperatorSection::Behaviors,
        ));
        assert_operator_context(driver, &alpha, OperatorSection::Behaviors, None);
        driver.wait_for_target(
            "alpha behavior row after bootstrap operator switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha.docs.behavior_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&bravo.docs.behavior_id)));
        driver.click_target(&audit::targets::operator_entity(&alpha.docs.behavior_id));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::Behaviors,
            Some(alpha.docs.behavior_id.as_str()),
        );

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::RequestTimeline,
        ));
        assert_operator_context(driver, &alpha, OperatorSection::RequestTimeline, None);
        driver.wait_for_target(
            "alpha request row after bootstrap operator switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha_submission.request_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &bravo_submission.request_id,
        )));

        driver.click_target(&audit::targets::operator_deployment(&bravo.peer_id));
        driver.click_target(&audit::targets::operator_agent(&bravo.agent_did));
        driver.click_target(&audit::targets::operator_section(
            OperatorSection::Behaviors,
        ));
        assert_operator_context(driver, &bravo, OperatorSection::Behaviors, None);
        driver.wait_for_target(
            "bravo behavior row after bootstrap operator switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&bravo.docs.behavior_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&alpha.docs.behavior_id)));
        driver.click_target(&audit::targets::operator_entity(&bravo.docs.behavior_id));
        assert_operator_context(
            driver,
            &bravo,
            OperatorSection::Behaviors,
            Some(bravo.docs.behavior_id.as_str()),
        );

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::RequestTimeline,
        ));
        assert_operator_context(driver, &bravo, OperatorSection::RequestTimeline, None);
        driver.wait_for_target(
            "bravo request row after bootstrap operator switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&bravo_submission.request_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &alpha_submission.request_id,
        )));
    }

    fixture.shutdown()
}
