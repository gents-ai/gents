#[derive(Debug, serde::Deserialize)]
pub(crate) struct BootstrapReplicatorRequest {
    #[serde(rename = "Addresses")]
    addresses: Vec<String>,
    #[serde(rename = "Collections")]
    collections: Vec<String>,
}

pub(crate) struct BootstrapRuntimeApi {
    label: String,
    graphql: String,
    metrics: String,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl BootstrapRuntimeApi {
    fn start(
        runtime: &Arc<Runtime>,
        core: Arc<ClientCore>,
        label: impl Into<String>,
        listen_address: String,
    ) -> Result<Self> {
        let label = label.into();
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let handle = runtime.handle().clone();
        let core_for_thread = Arc::clone(&core);
        let listen_address_for_thread = listen_address.clone();
        let label_for_thread = label.clone();
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
                                    .expect("serialize bootstrap p2p info"),
                            )),
                            ("GET", "/api/v0/p2p/shareable-address") => Ok((
                                "200 OK",
                                "application/json",
                                serde_json::json!({
                                    "address": listen_address_for_thread.clone(),
                                })
                                .to_string(),
                            )),
                            ("GET", "/metrics") => {
                                let metrics = handle.block_on(async {
                                    core_for_thread.refresh_store().await.ok();
                                    let snapshot = core_for_thread.store().snapshot();
                                    let connected_peers = core_for_thread
                                        .p2p()
                                        .connected_peers()
                                        .await
                                        .unwrap_or_default();
                                    let latest_runtime = snapshot.latest_runtime(
                                        snapshot
                                            .behaviors
                                            .first()
                                            .and_then(|row| row.agent_did.as_deref())
                                            .unwrap_or_default(),
                                    );
                                    let mut body = String::new();
                                    body.push_str("# TYPE defra_desktop_test_connected_peers gauge\n");
                                    body.push_str(&format!(
                                        "defra_desktop_test_connected_peers{{label=\"{}\",peer_id=\"{}\"}} {}\n",
                                        escape_prometheus_label(&label_for_thread),
                                        escape_prometheus_label(core_for_thread.local_peer_id()),
                                        connected_peers.len()
                                    ));
                                    body.push_str("# TYPE defra_desktop_test_requests_total gauge\n");
                                    body.push_str(&format!(
                                        "defra_desktop_test_requests_total{{label=\"{}\"}} {}\n",
                                        escape_prometheus_label(&label_for_thread),
                                        snapshot.requests.len()
                                    ));
                                    body.push_str("# TYPE defra_desktop_test_responses_total gauge\n");
                                    body.push_str(&format!(
                                        "defra_desktop_test_responses_total{{label=\"{}\"}} {}\n",
                                        escape_prometheus_label(&label_for_thread),
                                        snapshot.responses.len()
                                    ));
                                    body.push_str("# TYPE defra_desktop_test_conversations_total gauge\n");
                                    body.push_str(&format!(
                                        "defra_desktop_test_conversations_total{{label=\"{}\"}} {}\n",
                                        escape_prometheus_label(&label_for_thread),
                                        snapshot.conversations.len()
                                    ));
                                    body.push_str("# TYPE defra_desktop_test_runtime_generation gauge\n");
                                    body.push_str(&format!(
                                        "defra_desktop_test_runtime_generation{{label=\"{}\"}} {}\n",
                                        escape_prometheus_label(&label_for_thread),
                                        latest_runtime
                                            .and_then(|row| row.router_generation.or(row.active_generation))
                                            .unwrap_or_default()
                                    ));
                                    body
                                });
                                Ok(("200 OK", "text/plain; version=0.0.4", metrics))
                            }
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
                                    serde_json::from_str::<BootstrapReplicatorRequest>(
                                        &request.body,
                                    )
                                    .context("decoding /p2p/replicators payload")
                                    .and_then(|payload| {
                                        let target = payload
                                            .addresses
                                            .first()
                                            .cloned()
                                            .context("missing desktop replicator address")?;
                                        handle.block_on(async {
                                            set_replicator_with_retry(
                                                core_for_thread.as_ref(),
                                                &target,
                                                "bootstrap runtime replicator",
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
            label,
            graphql: format!("http://127.0.0.1:{port}/api/v0/graphql"),
            metrics: format!("http://127.0.0.1:{port}/metrics"),
            port,
            stop,
            handle: Some(thread),
        })
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    fn graphql_url(&self) -> &str {
        &self.graphql
    }

    pub(crate) fn metrics_url(&self) -> &str {
        &self.metrics
    }
}

impl Drop for BootstrapRuntimeApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn escape_prometheus_label(value: &str) -> String {
    value.replace('\\', r#"\\"#).replace('"', "\\\"")
}

pub(crate) fn bootstrap_live_core_options() -> ClientCoreOptions {
    let mut options = ClientCoreOptions::local_only();
    options.max_concurrent_push_tasks = 32;
    options.rate_limit_burst = 5_000;
    options.rate_limit_rate = 500.0;
    options
}

pub(crate) fn seed_agent_home_runtime_state_for_bootstrap(
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
