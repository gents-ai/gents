use super::*;

struct StatusServer {
    port: u16,
    hold: Arc<std::sync::atomic::AtomicBool>,
    received: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl StatusServer {
    async fn start(body: String) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("status listener");
        let port = listener.local_addr().expect("status address").port();
        let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let received = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (task_hold, task_received, task_release, task_requests) = (
            hold.clone(),
            received.clone(),
            release.clone(),
            requests.clone(),
        );
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                task_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if task_hold.load(std::sync::atomic::Ordering::SeqCst) {
                    task_received.notify_one();
                    task_release.notified().await;
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self {
            port,
            hold,
            received,
            release,
            requests,
            task,
        }
    }

    fn graphql(&self) -> String {
        format!("http://127.0.0.1:{}/api/v0/graphql", self.port)
    }
}

async fn skewed_managed_runtime(
    temp: &tempfile::TempDir,
    node_did: &str,
) -> (Arc<ClientCore>, StatusServer) {
    use gents_desktop_core::client::{ClientCoreOptions, DesktopPaths};

    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(temp.path().join("client")),
            ClientCoreOptions::local_only(),
        )
        .await
        .expect("client core"),
    );
    let mut next_release =
        gents::agent::p2p_reconcile::read_client_replicated_schema(core.node_arc())
            .await
            .expect("local collection versions");
    next_release
        .get_mut("AgentSession")
        .expect("client route replicates AgentSession")
        .version_id = "bafy-next-release".to_string();
    let server = StatusServer::start(
        serde_json::json!({
            "node_did": node_did,
            gents_protocol::peer_schema::STATUS_REPLICATED_SCHEMA_FIELD: next_release,
            gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD: "ready",
        })
        .to_string(),
    )
    .await;
    core.add_managed_enrollment_peer_for_test(node_did, &server.graphql(), "/tmp/managed-home", 1)
        .await
        .expect("paired managed runtime");
    (core, server)
}

#[tokio::test]
async fn paired_managed_runtime_with_skewed_schema_projects_incompatible_sync() {
    use gents_desktop_core::client::{project_sync_health, SyncHealthState};

    let (temp, state) = orchestration_state();
    let node_did = "did:key:managed-runtime";
    let (core, server) = skewed_managed_runtime(&temp, node_did).await;
    assert!(core.peer_records().await.iter().any(|peer| {
        peer.node_did == node_did
            && peer.is_enrollment()
            && peer.is_managed_runtime()
            && peer.is_chat_ready_at(chrono::Utc::now())
    }));

    let mut updates = core.sync_state_updates();
    start_managed_runtime_pairing(
        &state,
        Arc::clone(&core),
        temp.path().join("agent"),
        ManagedPairingTarget {
            node_name: "Managed".to_string(),
            node_did: node_did.to_string(),
            graphql: server.graphql(),
        },
    )
    .await;
    {
        let managed = state.managed_server.lock().await;
        assert!(
            managed.pairing_task.is_none(),
            "a chat-ready runtime is not re-paired"
        );
        assert!(managed.schema_observation_task.is_some());
    }
    let health = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(health) = project_sync_health(&updates.borrow_and_update()) {
                if health.state == SyncHealthState::Incompatible {
                    return health;
                }
            }
            updates.changed().await.expect("sync owner alive");
        }
    })
    .await
    .expect("skewed managed runtime projects incompatible sync");
    assert!(health
        .last_error
        .as_deref()
        .is_some_and(|error| error.contains("AgentSession")));

    drain_managed_runtime_pairing(&state).await;
    assert!(state
        .managed_server
        .lock()
        .await
        .schema_observation_task
        .is_none());
    server.task.abort();
    core.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn drain_cancels_pairing_while_it_waits_for_the_runtime_status() {
    use gents_desktop_core::client::{ClientCoreOptions, DesktopPaths};

    let (temp, state) = orchestration_state();
    let node_did = "did:key:starting-runtime";
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(temp.path().join("client")),
            ClientCoreOptions::local_only(),
        )
        .await
        .expect("client core"),
    );
    let server = StatusServer::start(
        serde_json::json!({
            "node_did": node_did,
            gents_protocol::peer_schema::STATUS_REPLICATED_SCHEMA_FIELD: null,
        })
        .to_string(),
    )
    .await;
    start_managed_runtime_pairing(
        &state,
        Arc::clone(&core),
        temp.path().join("agent"),
        ManagedPairingTarget {
            node_name: "Starting".to_string(),
            node_did: node_did.to_string(),
            graphql: server.graphql(),
        },
    )
    .await;
    assert!(state.managed_server.lock().await.pairing_task.is_some());
    tokio::time::timeout(Duration::from_secs(10), async {
        while server.requests.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pairing is waiting for the runtime status");

    tokio::time::timeout(
        Duration::from_secs(5),
        drain_managed_runtime_pairing(&state),
    )
    .await
    .expect("drain does not wait out the status publish window");
    let managed = state.managed_server.lock().await;
    assert!(managed.pairing_task.is_none());
    assert!(managed.pairing_cancel.is_none());
    assert!(managed.schema_observation_task.is_none());
    drop(managed);

    server.task.abort();
    core.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn managed_schema_observation_refuses_a_target_whose_route_was_replaced() {
    let temp = tempfile::tempdir().expect("temp");
    let node_did = "did:key:managed-runtime";
    let (core, route_a) = skewed_managed_runtime(&temp, node_did).await;
    let target_a = ManagedPairingTarget {
        node_name: "Managed".to_string(),
        node_did: node_did.to_string(),
        graphql: route_a.graphql(),
    };
    let route_b = "http://127.0.0.1:1/api/v0/graphql";
    core.add_managed_enrollment_peer_for_test(node_did, route_b, "/tmp/managed-home", 2)
        .await
        .expect("route B replaces route A");

    route_a
        .hold
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let error = observe_managed_runtime_schema(&core, &target_a)
        .await
        .unwrap_err();
    assert!(error.contains("no longer routes through"), "{error}");
    assert!(
        core.sync_state().peer_schema_skew.is_empty(),
        "route A's status is never recorded against route B"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), route_a.received.notified())
            .await
            .is_err(),
        "route A is not fetched once its route is replaced"
    );

    route_a.task.abort();
    core.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn managed_schema_fetched_across_a_route_change_is_ignored() {
    let temp = tempfile::tempdir().expect("temp");
    let node_did = "did:key:managed-runtime";
    let (core, server) = skewed_managed_runtime(&temp, node_did).await;
    let target = ManagedPairingTarget {
        node_name: "Managed".to_string(),
        node_did: node_did.to_string(),
        graphql: server.graphql(),
    };

    server.hold.store(true, std::sync::atomic::Ordering::SeqCst);
    let in_flight = tokio::spawn({
        let core = Arc::clone(&core);
        let target = target.clone();
        async move { observe_managed_runtime_schema(&core, &target).await }
    });
    tokio::time::timeout(Duration::from_secs(10), server.received.notified())
        .await
        .expect("status request in flight");
    core.add_managed_enrollment_peer_for_test(node_did, &server.graphql(), "/tmp/managed-home", 2)
        .await
        .expect("route generation advances");
    server
        .hold
        .store(false, std::sync::atomic::Ordering::SeqCst);
    server.release.notify_one();
    let late = in_flight.await.expect("observation task");
    assert!(late.is_err_and(|error| error.contains("AgentSession")));
    assert!(
        core.sync_state().peer_schema_skew.is_empty(),
        "a response fetched for the replaced generation is not recorded"
    );

    observe_managed_runtime_schema(&core, &target)
        .await
        .unwrap_err();
    assert_eq!(core.sync_state().peer_schema_skew.len(), 1);

    server.task.abort();
    core.shutdown().await.expect("shutdown");
}
#[tokio::test]
async fn managed_readiness_authenticates_explicit_start_and_keeps_passive_reads_cold() {
    use identity::Identity as _;
    use wiremock::matchers::{header_regex, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("node");
    let root = temp.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    let key_path = home.join("node.key");
    let identity = gents::identity::load_or_create_file_identity(&key_path).unwrap();
    let did = identity.did().unwrap().to_string();
    assert!(!gents::identity::can_mint_defradb_bearer(&did));
    let server = MockServer::start().await;
    let endpoint = reqwest::Url::parse(&server.uri()).unwrap();
    let config = gents_server::server_host::ServerConfig {
        home: home.clone(),
        http_addr: "127.0.0.1".parse().unwrap(),
        http_port: endpoint.port().unwrap(),
    };
    let graphql = format!("{}/api/v0/graphql", server.uri());
    std::fs::write(
        home.join("init.json"),
        serde_json::json!({
            "node_name":"Local", "node_did":did, "key_path":key_path,
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        home.join("runtime.json"),
        serde_json::json!({
            "node_name":"Local", "node_did":did, "graphql":graphql,
        })
        .to_string(),
    )
    .unwrap();
    let public = serde_json::json!({
        "service":"gents", "version":env!("CARGO_PKG_VERSION"),
        "lifecycle":"ready", "node_did":did, "graphql":graphql,
    });
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(public.clone()))
        .with_priority(2)
        .mount(&server)
        .await;
    let mut private = public;
    private["tool_ceiling"] = "readwrite".into();
    private["tool_root"] = root.to_string_lossy().into_owned().into();
    Mock::given(method("GET"))
        .and(path("/status"))
        .and(header_regex("authorization", "^Bearer .+"))
        .respond_with(ResponseTemplate::new(200).set_body_json(private))
        .with_priority(1)
        .mount(&server)
        .await;
    let authority = EffectiveManagedAuthority {
        tool_ceiling: ManagedServerToolCeiling::Readwrite,
        tool_root: Some(root),
    };
    let passive = observe_port_readiness_at(&config, OperatorIdentityInteraction::Prevent)
        .await
        .unwrap();
    let PortReadiness::Ready(passive) = passive else {
        panic!("public status is ready")
    };
    assert!(validate_ready_runtime(&passive, &authority, &home).is_err());
    assert!(!gents::identity::can_mint_defradb_bearer(&did));
    let explicit = observe_port_readiness_at(&config, OperatorIdentityInteraction::Allow)
        .await
        .unwrap();
    let PortReadiness::Ready(explicit) = explicit else {
        panic!("authenticated runtime is ready")
    };
    validate_ready_runtime(&explicit, &authority, &home).unwrap();
    let wrong_root = EffectiveManagedAuthority {
        tool_root: Some(temp.path().join("elsewhere")),
        ..authority.clone()
    };
    assert!(validate_ready_runtime(&explicit, &wrong_root, &home).is_err());
    std::fs::remove_file(&key_path).unwrap();
    let cached = observe_port_readiness_at(&config, OperatorIdentityInteraction::Prevent)
        .await
        .unwrap();
    let PortReadiness::Ready(cached) = cached else {
        panic!("cached authority remains available")
    };
    validate_ready_runtime(&cached, &authority, &home).unwrap();
    let count = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.headers.contains_key("authorization"))
        .count();
    std::fs::write(home.join("init.json"), r#"{"node_did":"did:key:foreign"}"#).unwrap();
    assert!(matches!(
        observe_port_readiness_at(&config, OperatorIdentityInteraction::Allow)
            .await
            .unwrap(),
        PortReadiness::Foreign(_)
    ));
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.headers.contains_key("authorization"))
            .count(),
        count
    );
}
