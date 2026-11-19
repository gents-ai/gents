use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::client_projection::store::ClientStore;
use crate::test_support::client_observer::{wait_for_connection, wait_for_name};
use gents_store::config_client::{GraphqlEndpoint, TransactionFailure};
use gents_store::identity::{KeyIdentity, NodeIdentity};

#[test]
fn observer_stops_only_for_an_explicit_authorization_refusal() {
    let denial = gents_store::config_client::ConfigClientError::Database(
        gents_protocol::database_error::GraphqlFailure::new(
            "subscription",
            serde_json::json!([
                {"message":"opaque", "extensions":{"code":"PERMISSION_DENIED"}}
            ]),
        ),
    );
    assert!(anyhow::Error::new(denial).permission_denied());
    assert!(!anyhow::anyhow!("not found or unavailable").permission_denied());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProxyMode {
    Online,
    Offline,
    /// Answers every request with the runtime's HTTP refusal.
    Refusing,
    /// Keeps every armed subscription streaming and drops every other
    /// request: the runtime's reads fail for a transport reason.
    FailingReads,
    /// Keeps every armed subscription streaming and answers every other
    /// request with the runtime's HTTP refusal.
    RefusingReads,
}

type ProxyCommands =
    tokio::sync::mpsc::UnboundedSender<(ProxyMode, tokio::sync::oneshot::Sender<()>)>;

/// Reads the rest of a request whose head is `head`, so the refusal is sent
/// after the client finished writing.
async fn read_request_body(client: &mut tokio::net::TcpStream, head: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut request = head.to_vec();
    let end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or(request.len());
    let length = String::from_utf8_lossy(&request[..end])
        .to_ascii_lowercase()
        .lines()
        .find_map(|line| line.strip_prefix("content-length:").map(str::to_owned))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or_default();
    let mut buffer = [0_u8; 4096];
    while request.len() < end + 4 + length {
        let read = client.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..read]);
    }
    Ok(())
}

async fn request_head(client: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut head = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = client.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        head.extend_from_slice(&buffer[..read]);
    }
    Some(head)
}

fn is_subscription(head: &[u8]) -> bool {
    String::from_utf8_lossy(head)
        .to_ascii_lowercase()
        .contains("accept: text/event-stream")
}

/// Asks the runtime to close a read's connection after its response, so the
/// client never reuses it for a subscription and every connection carries
/// requests of one kind.
fn single_request(head: Vec<u8>) -> Vec<u8> {
    let Some(line_end) = head.windows(2).position(|window| window == b"\r\n") else {
        return head;
    };
    let mut rewritten = head[..line_end + 2].to_vec();
    rewritten.extend_from_slice(b"connection: close\r\n");
    for line in head[line_end + 2..].split_inclusive(|byte| *byte == b'\n') {
        if !String::from_utf8_lossy(line)
            .to_ascii_lowercase()
            .starts_with("connection:")
        {
            rewritten.extend_from_slice(line);
        }
    }
    rewritten
}

/// Forwards a connection whose first bytes, `head`, were already read.
async fn forward(mut client: tokio::net::TcpStream, head: Vec<u8>, upstream: std::net::SocketAddr) {
    use tokio::io::AsyncWriteExt;
    if let Ok(mut server) = tokio::net::TcpStream::connect(upstream).await {
        if server.write_all(&head).await.is_ok() {
            let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
        }
    }
}

async fn interruptible_proxy(
    upstream: std::net::SocketAddr,
) -> (String, ProxyCommands, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let (commands, mut command_rx) =
        tokio::sync::mpsc::unbounded_channel::<(ProxyMode, tokio::sync::oneshot::Sender<()>)>();
    let task = tokio::spawn(async move {
        let mode = Arc::new(std::sync::Mutex::new(ProxyMode::Online));
        // A connection is classified by its first request: subscriptions and
        // reads are cut separately.
        let mut subscriptions = tokio::task::JoinSet::new();
        let mut reads = tokio::task::JoinSet::new();
        let (classified_tx, mut classified) =
            tokio::sync::mpsc::unbounded_channel::<(tokio::net::TcpStream, Vec<u8>)>();
        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    let Some((next, done)) = command else { break };
                    *mode.lock().unwrap() = next;
                    if next != ProxyMode::Online {
                        reads.abort_all();
                        while reads.join_next().await.is_some() {}
                    }
                    if !matches!(
                        next,
                        ProxyMode::Online | ProxyMode::FailingReads | ProxyMode::RefusingReads
                    ) {
                        subscriptions.abort_all();
                        while subscriptions.join_next().await.is_some() {}
                    }
                    let _ = done.send(());
                }
                accepted = listener.accept() => {
                    let (mut client, _) = accepted.unwrap();
                    let classified_tx = classified_tx.clone();
                    reads.spawn(async move {
                        if let Some(head) = request_head(&mut client).await {
                            let _ = classified_tx.send((client, head));
                        }
                    });
                }
                Some((mut client, head)) = classified.recv() => {
                    let current = *mode.lock().unwrap();
                    let subscription = is_subscription(&head);
                    match current {
                        ProxyMode::Online if subscription => {
                            subscriptions.spawn(forward(client, head, upstream));
                        }
                        ProxyMode::Online => {
                            reads.spawn(forward(client, single_request(head), upstream));
                        }
                        ProxyMode::FailingReads | ProxyMode::RefusingReads if subscription => {
                            subscriptions.spawn(forward(client, head, upstream));
                        }
                        ProxyMode::Refusing | ProxyMode::RefusingReads => {
                            reads.spawn(async move {
                                use tokio::io::AsyncWriteExt;
                                if read_request_body(&mut client, &head).await.is_ok() {
                                    let _ = client
                                        .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                                        .await;
                                    let _ = client.shutdown().await;
                                }
                            });
                        }
                        ProxyMode::FailingReads | ProxyMode::Offline => {}
                    }
                }
                _ = subscriptions.join_next(), if !subscriptions.is_empty() => {}
                _ = reads.join_next(), if !reads.is_empty() => {}
            }
        }
    });
    (url, commands, task)
}

async fn set_proxy_mode(commands: &ProxyCommands, mode: ProxyMode) {
    let (done, completed) = tokio::sync::oneshot::channel();
    commands.send((mode, done)).unwrap();
    completed.await.unwrap();
}

async fn set_proxy_online(commands: &ProxyCommands, online: bool) {
    set_proxy_mode(
        commands,
        if online {
            ProxyMode::Online
        } else {
            ProxyMode::Offline
        },
    )
    .await;
}

/// A runtime whose Node document the client reads through `runtime_reader`.
struct ObservedRuntime {
    _directory: tempfile::TempDir,
    client: KeyIdentity,
    runtime_did: String,
    runtime: Arc<defra_node::Node>,
    access: ConfigAccess,
    physical: String,
    peers: RuntimeDirectory,
    proxy_commands: ProxyCommands,
    proxy_task: tokio::task::JoinHandle<()>,
}

impl ObservedRuntime {
    async fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let key_path = directory.path().join("client.key");
        let client = tokio::task::spawn_blocking(move || {
            KeyIdentity::load_or_create(key_path, None).unwrap()
        })
        .await
        .unwrap();
        let node_path = directory.path().join("runtime.key");
        let runtime_identity = tokio::task::spawn_blocking(move || {
            KeyIdentity::load_or_create(node_path, None).unwrap()
        })
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/api/v0/graphql");
        let (proxy_url, proxy_commands, proxy_task) = interruptible_proxy(address).await;
        let runtime = Arc::new(
            gents_store::test_support::node_builder()
                .with_node_identity_did(runtime_identity.did())
                .with_http(
                    defra_node::HttpConfig::with_listener(listener.into_std().unwrap())
                        .unwrap()
                        .with_extra_routes(runtime_status(&directory.path().join("runtime.key"))),
                )
                .build()
                .await
                .unwrap(),
        );
        gents_store::schema::ensure_runtime_schemas(&runtime)
            .await
            .unwrap();
        let access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(url, runtime_identity.did())
                .with_client_signing_capability(
                    runtime_identity.client_signing_capability().unwrap(),
                )
                .unwrap(),
        );
        let owner =
            ConfigAccess::local_as(runtime.clone(), runtime_identity.did().parse().unwrap());
        let did = gents_store::graphql::escape_graphql_string(runtime_identity.did());
        let created = access
            .write(
                "test.observer.seed",
                &format!("mutation {{ create_Node(input: {{node_did: \"{did}\", display_name: \"remote\", enabled: true, created_at: \"2026-11-04T00:00:00Z\", created_by: \"user\"}}) {{ _docID }} }}"),
            )
            .await
            .unwrap();
        let physical = gents_store::graphql::created_doc_id(&created, "Node").unwrap();
        owner
            .add_actor_relationship(
                "Node",
                &physical,
                "runtime_reader",
                &client.did().parse().unwrap(),
            )
            .await
            .unwrap();
        let mut peer = gents_protocol::runtime_connection::RuntimeConnection {
            label: ("runtime").to_string(),
            node_did: (runtime_identity.did()).to_string(),
            graphql: String::new(),
            local_node_home: None,
            access_request: None,
            access_state: None,
        };
        peer.graphql = proxy_url;
        let peers = RuntimeDirectory::open(directory.path().join("connections.json"))
            .await
            .unwrap();
        peers.upsert(peer).await.unwrap();
        Self {
            _directory: directory,
            client,
            runtime_did: runtime_identity.did().to_owned(),
            runtime,
            access,
            physical,
            peers,
            proxy_commands,
            proxy_task,
        }
    }

    async fn stop(self) {
        drop(self.proxy_commands);
        self.proxy_task.await.unwrap();
        self.runtime.shutdown().await.unwrap();
    }
}

/// Arms every subscription and then never answers a snapshot query.
async fn stalled_runtime() -> (String, watch::Receiver<u64>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let (queries_tx, queries) = watch::channel(0_u64);
    let queries_tx = Arc::new(queries_tx);
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut client, _) = listener.accept().await.unwrap();
            let queries_tx = queries_tx.clone();
            connections.spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut head = Vec::new();
                let mut buffer = [0_u8; 4096];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    let Ok(read) = client.read(&mut buffer).await else { return };
                    if read == 0 {
                        return;
                    }
                    head.extend_from_slice(&buffer[..read]);
                }
                if String::from_utf8_lossy(&head)
                    .to_ascii_lowercase()
                    .contains("accept: text/event-stream")
                {
                    let _ = client
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n")
                        .await;
                } else {
                    queries_tx.send_modify(|count| *count += 1);
                }
                std::future::pending::<()>().await;
            });
        }
    });
    (url, queries, task)
}

#[tokio::test]
async fn authenticated_runtime_reconnect_reloads_and_resubscribes() {
    let directory = tempfile::tempdir().unwrap();
    let key_path = directory.path().join("client.key");
    let client =
        tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(key_path, None).unwrap())
            .await
            .unwrap();
    let node_path = directory.path().join("runtime.key");
    let runtime_identity =
        tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(node_path, None).unwrap())
            .await
            .unwrap();
    assert_ne!(client.did(), runtime_identity.did());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}/api/v0/graphql");
    let (proxy_url, proxy_commands, proxy_task) = interruptible_proxy(address).await;
    let runtime = Arc::new(
        gents_store::test_support::node_builder()
            .with_node_identity_did(runtime_identity.did())
            .with_http(
                defra_node::HttpConfig::with_listener(listener.into_std().unwrap())
                    .unwrap()
                    .with_extra_routes(runtime_status(&directory.path().join("runtime.key"))),
            )
            .build()
            .await
            .unwrap(),
    );
    let desktop = Arc::new(
        gents_store::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&runtime)
        .await
        .unwrap();
    gents_store::schema::ensure_runtime_schemas(&desktop)
        .await
        .unwrap();
    let access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(url.clone(), runtime_identity.did())
            .with_client_signing_capability(runtime_identity.client_signing_capability().unwrap())
            .unwrap(),
    );
    let reader_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(url.clone(), client.did())
            .with_client_signing_capability(client.client_signing_capability().unwrap())
            .unwrap(),
    );
    let owner = ConfigAccess::local_as(runtime.clone(), runtime_identity.did().parse().unwrap());
    let reader = client.did().parse().unwrap();
    let did = gents_store::graphql::escape_graphql_string(runtime_identity.did());
    let mutation = |name: &str| {
        format!("mutation {{ create_Node(input: {{node_did: \"{did}\", display_name: \"{name}\", enabled: true, created_at: \"2026-11-04T00:00:00Z\", created_by: \"user\"}}) {{ _docID }} }}")
    };
    let created = access
        .write("test.observer.seed", &mutation("remote"))
        .await
        .unwrap();
    let physical = gents_store::graphql::created_doc_id(&created, "Node").unwrap();
    owner
        .add_actor_relationship("Node", &physical, "runtime_reader", &reader)
        .await
        .unwrap();
    let session = access
        .write(
            "test.observer.visible_session",
            &format!(
                r#"mutation {{ create_AgentSession(input: {{ node_did: "{did}", session_id: "unrelated-visible", requester_did: "{did}", agent_did: "{did}", created_at: "2026-11-04T00:00:00Z" }}) {{ _docID }} }}"#,
            ),
        )
        .await
        .unwrap();
    let session_doc = gents_store::graphql::created_doc_id(&session, "AgentSession").unwrap();
    owner
        .add_actor_relationship("AgentSession", &session_doc, "reader", &reader)
        .await
        .unwrap();
    ConfigAccess::local(desktop.clone())
        .write("test.observer.stale", &mutation("stale replica"))
        .await
        .unwrap();
    let mut peer = gents_protocol::runtime_connection::RuntimeConnection {
        label: ("runtime").to_string(),
        node_did: (runtime_identity.did()).to_string(),
        graphql: String::new(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    };
    peer.graphql = proxy_url;
    let peers = RuntimeDirectory::open(directory.path().join("connections.json"))
        .await
        .unwrap();
    peers.upsert(peer).await.unwrap();
    let configured = peers;
    let store = ObservedStores::new();
    let handle = spawn_observer(
        store.clone(),
        configured,
        Arc::new(
            gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(
                client.client_signing_capability().unwrap(),
            ),
        ),
    );
    wait_for_name(&store, Some("remote")).await;
    let mut notices = store.subscribe();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if store
                .snapshot(runtime_identity.did())
                .sessions
                .iter()
                .any(|row| row.session_id == "unrelated-visible")
            {
                break;
            }
            notices.changed().await.unwrap();
        }
    })
    .await
    .expect("authorized session in authenticated snapshot");
    store
        .ensure(runtime_identity.did())
        .clear_after_refused_read();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if store
                .snapshot(runtime_identity.did())
                .sessions
                .iter()
                .any(|row| row.session_id == "unrelated-visible")
            {
                break;
            }
            notices.changed().await.unwrap();
        }
    })
    .await
    .expect("authorized session restored by authenticated resync");
    assert_eq!(
        store.runtime_connection(runtime_identity.did()),
        Some(RuntimeConnectionStatus::Connected)
    );
    let physical_id = physical;
    let physical = gents_store::graphql::escape_graphql_string(&physical_id);
    set_proxy_online(&proxy_commands, false).await;
    wait_for_connection(
        &store,
        runtime_identity.did(),
        RuntimeConnectionStatus::Disconnected,
    )
    .await;
    access.write("test.observer.offline", &format!("mutation {{ update_Node(docID: \"{physical}\", input: {{display_name: \"offline update\"}}) {{ _docID }} }}")).await.unwrap();
    assert_eq!(
        store.snapshot(runtime_identity.did()).nodes[0]
            .display_name
            .as_deref(),
        Some("remote")
    );
    set_proxy_online(&proxy_commands, true).await;
    wait_for_name(&store, Some("offline update")).await;
    wait_for_connection(
        &store,
        runtime_identity.did(),
        RuntimeConnectionStatus::Connected,
    )
    .await;
    let after_reconnect = handle.metrics_snapshot();
    access.write("test.observer.update", &format!("mutation {{ update_Node(docID: \"{physical}\", input: {{display_name: \"updated\"}}) {{ _docID }} }}")).await.unwrap();
    wait_for_name(&store, Some("updated")).await;
    assert!(handle.metrics_snapshot().events_received > after_reconnect.events_received);
    set_proxy_online(&proxy_commands, false).await;
    wait_for_connection(
        &store,
        runtime_identity.did(),
        RuntimeConnectionStatus::Disconnected,
    )
    .await;
    owner
        .delete_actor_relationship("Node", &physical_id, "runtime_reader", &reader)
        .await
        .unwrap();
    let hidden = reader_access
        .execute(&format!("{{ Node(docID: \"{physical}\") {{ _docID }} }}"))
        .await
        .unwrap();
    assert!(hidden["data"]["Node"].as_array().unwrap().is_empty());
    set_proxy_online(&proxy_commands, true).await;
    wait_for_name(&store, None).await;
    // A hidden selected root is refused, not connected (Lean
    // `missing_selected_root_is_not_connected`).
    wait_for_connection(
        &store,
        runtime_identity.did(),
        RuntimeConnectionStatus::Refused,
    )
    .await;
    owner
        .add_actor_relationship("Node", &physical_id, "runtime_reader", &reader)
        .await
        .unwrap();
    wait_for_name(&store, Some("updated")).await;
    access
        .write(
            "test.observer.delete",
            &format!("mutation {{ delete_Node(docID: \"{physical}\") {{ _docID }} }}"),
        )
        .await
        .unwrap();
    wait_for_name(&store, None).await;
    wait_for_connection(
        &store,
        runtime_identity.did(),
        RuntimeConnectionStatus::Refused,
    )
    .await;
    assert!(handle.metrics_snapshot().events_received >= 2);
    handle.shutdown().await;
    drop(proxy_commands);
    proxy_task.await.unwrap();
    desktop.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_endpoint_never_reads_runtime_data_from_desktop_store() {
    let desktop = Arc::new(
        gents_store::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&desktop)
        .await
        .unwrap();
    ConfigAccess::local(desktop.clone()).write("test.observer.local", "mutation { create_Node(input: {node_did: \"did:test:runtime\", display_name: \"stale\", enabled: true, created_at: \"now\", created_by: \"old\"}) {_docID} }").await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let key_path = directory.path().join("client.key");
    let client = tokio::task::spawn_blocking(move || {
        gents_store::identity::KeyIdentity::load_or_create(key_path, None)
    })
    .await
    .unwrap()
    .unwrap();
    let endpoints = gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(
        client.client_signing_capability().unwrap(),
    );
    assert!(
        crate::client_projection::query::load_node_scoped_snapshot_with_connections(
            "did:test:runtime",
            &[],
            &endpoints,
        )
        .await
        .is_err()
    );
    desktop.shutdown().await.unwrap();
}

#[test]
fn generated_runtime_connection_observations_drive_the_owner() {
    let output = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.RuntimeConnectionObservation:olean",
        "Proofs/Conformance/RuntimeConnectionObservation.lean",
    )
    .unwrap();
    let contract: serde_json::Value = serde_json::from_str(&output).unwrap();
    let cases = contract["cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    let store = ObservedStores::new();
    for case in cases {
        let status = lean_status(case["status"].as_str().unwrap());
        store.set_runtime_connection("node", Some(status));
        store.observe_runtime_connection("node", lean_event(case["event"].as_str().unwrap()));
        assert_eq!(
            store.runtime_connection("node").unwrap().as_str(),
            case["expected"].as_str().unwrap(),
            "{case}"
        );
    }
    let ends = contract["streamEnds"].as_array().unwrap();
    assert_eq!(ends.len(), 4);
    for case in ends {
        let end = match case["end"].as_str().unwrap() {
            "renewalDue" => {
                remote::StreamEnd::Ended(gents_store::config_client::SubscriptionEnd::RenewalDue)
            }
            "completed" => {
                remote::StreamEnd::Ended(gents_store::config_client::SubscriptionEnd::Completed)
            }
            "refusedFailure" => remote::StreamEnd::Failed { refused: true },
            "transportFailure" => remote::StreamEnd::Failed { refused: false },
            other => panic!("unexpected stream end {other}"),
        };
        let expected = case["event"].as_str().map(lean_event);
        assert_eq!(
            format!("{:?}", remote::stream_end_event(end)),
            format!("{expected:?}"),
            "{case}"
        );
    }
    let armed_events = contract["armedEvents"].as_array().unwrap();
    assert_eq!(armed_events.len(), 2);
    for case in armed_events {
        assert_eq!(
            remote::armed_event(case["renewal"].as_bool().unwrap()),
            lean_event(case["event"].as_str().unwrap()),
            "{case}"
        );
    }
    let armed = contract["armed"].as_array().unwrap();
    assert_eq!(armed.len(), 14);
    for case in armed {
        assert_eq!(
            remote::armed_after(
                case["wasArmed"].as_bool().unwrap(),
                lean_event(case["event"].as_str().unwrap())
            ),
            case["armed"].as_bool().unwrap(),
            "{case}"
        );
    }
    let outcomes = contract["snapshotOutcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 3);
    for case in outcomes {
        let outcome = match case["outcome"].as_str().unwrap() {
            "succeeded" => remote::SnapshotOutcome::Succeeded,
            "refused" => remote::SnapshotOutcome::Refused,
            "unreachable" => remote::SnapshotOutcome::Unreachable,
            other => panic!("unexpected snapshot outcome {other}"),
        };
        assert_eq!(
            remote::snapshot_event(outcome),
            lean_event(case["event"].as_str().unwrap()),
            "{case}"
        );
    }
    let base = Instant::now();
    let starts = contract["renewalStarts"].as_array().unwrap();
    assert!(!starts.is_empty());
    for case in starts {
        let started = base + Duration::from_millis(case["startedMs"].as_u64().unwrap());
        let next = remote::renewal_start(
            started,
            Duration::from_millis(case["armMs"].as_u64().unwrap()),
            gents_store::config_client::SUBSCRIPTION_RENEWAL_PERIOD,
        );
        assert_eq!(
            u64::try_from(next.duration_since(base).as_millis()).unwrap(),
            case["nextMs"].as_u64().unwrap(),
            "{case}"
        );
    }
    assert_eq!(
        u64::try_from(gents_store::config_client::SUBSCRIPTION_RENEWAL_PERIOD.as_millis()).unwrap(),
        contract["renewalPeriodMs"].as_u64().unwrap()
    );
    let retries = contract["snapshotRetries"].as_array().unwrap();
    assert!(retries.len() > 30);
    for case in retries {
        let failures = u32::try_from(case["failures"].as_u64().unwrap()).unwrap();
        let settlements: &[Settlement] = match case["outcome"].as_str().unwrap() {
            "succeeded" => &[Settlement::Published, Settlement::Superseded],
            "refused" => &[Settlement::Refused],
            "unreachable" => &[Settlement::TransportFailed],
            other => panic!("unexpected snapshot outcome {other}"),
        };
        for settlement in settlements {
            let mut queue = Queue::default();
            queue.failures.insert("node".into(), failures);
            queue.settle("node", *settlement, base);
            assert_eq!(
                u64::from(queue.failures.get("node").copied().unwrap_or_default()),
                case["failuresAfter"].as_u64().unwrap(),
                "{case} {settlement:?}"
            );
            assert_eq!(
                queue
                    .retry_at
                    .get("node")
                    .map(|due| u64::try_from(due.duration_since(base).as_millis()).unwrap()),
                case["retryDelayMs"].as_u64(),
                "{case} {settlement:?}"
            );
        }
    }
}

#[tokio::test]
async fn retry_now_answers_a_connected_runtime_without_reconnecting() {
    let fixture = ObservedRuntime::start().await;
    let configured = fixture.peers.clone();
    let store = ObservedStores::new();
    let handle = spawn_observer(
        store.clone(),
        configured,
        Arc::new(
            gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(
                fixture.client.client_signing_capability().unwrap(),
            ),
        ),
    );
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Connected,
    )
    .await;
    let mut changes = store.subscribe();

    handle.retry_all_now();
    let answer = tokio::time::timeout(
        Duration::from_secs(1),
        handle.retry_now(&fixture.runtime_did),
    )
    .await
    .expect("a connected runtime is answered at once");
    assert_eq!(answer, Some(RuntimeConnectionStatus::Connected));
    let _ = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            assert_eq!(
                store.runtime_connection(&fixture.runtime_did),
                Some(RuntimeConnectionStatus::Connected),
                "no source restarted"
            );
            changes.changed().await.unwrap();
        }
    })
    .await;
    handle.shutdown().await;
    fixture.stop().await;
}

/// The snapshot owner's failures map to `RuntimeConnectionObservation.SnapshotOutcome`:
/// only the access owner's classification is a refusal.
#[test]
fn snapshot_failures_classify_as_their_lean_outcome() {
    let refused = anyhow::Error::new(gents_store::config_client::ConfigClientError::Database(
        gents_protocol::database_error::GraphqlFailure::new(
            "snapshot",
            serde_json::json!([{"message":"opaque", "extensions":{"code":"PERMISSION_DENIED"}}]),
        ),
    ))
    .context("runtime desktop snapshot");
    let unreachable = anyhow::anyhow!("connection refused");
    assert_eq!(
        remote::snapshot_outcome(&refused.into()),
        remote::SnapshotOutcome::Refused
    );
    assert_eq!(
        remote::snapshot_outcome(&unreachable.into()),
        remote::SnapshotOutcome::Unreachable
    );
}

fn lean_status(name: &str) -> RuntimeConnectionStatus {
    match name {
        "connecting" => RuntimeConnectionStatus::Connecting,
        "connected" => RuntimeConnectionStatus::Connected,
        "disconnected" => RuntimeConnectionStatus::Disconnected,
        "refused" => RuntimeConnectionStatus::Refused,
        "noEndpoint" => RuntimeConnectionStatus::NoEndpoint,
        other => panic!("unexpected status {other}"),
    }
}

fn lean_event(name: &str) -> RuntimeConnectionEvent {
    match name {
        "sourceStarted" => RuntimeConnectionEvent::SourceStarted,
        "subscriptionsReady" => RuntimeConnectionEvent::SubscriptionsReady,
        "snapshotSucceeded" => RuntimeConnectionEvent::SnapshotSucceeded,
        "transportFailed" => RuntimeConnectionEvent::TransportFailed,
        "refused" => RuntimeConnectionEvent::Refused,
        "renewed" => RuntimeConnectionEvent::Renewed,
        "endpointMissing" => RuntimeConnectionEvent::EndpointMissing,
        other => panic!("unexpected event {other}"),
    }
}

/// A runtime that refuses the client (HTTP 403 at every request) is named:
/// the rows are dropped, the status reads refused across every reconnect,
/// and the source reconnects on the backoff until the runtime accepts the
/// client again at the same endpoint.
#[tokio::test]
async fn refused_runtime_reconnects_until_access_returns_at_the_same_endpoint() {
    let fixture = ObservedRuntime::start().await;
    let configured = fixture.peers.clone();
    let store = ObservedStores::new();
    let handle = spawn_observer(
        store.clone(),
        configured,
        Arc::new(
            gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(
                fixture.client.client_signing_capability().unwrap(),
            ),
        ),
    );
    wait_for_name(&store, Some("remote")).await;
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Connected,
    )
    .await;

    set_proxy_mode(&fixture.proxy_commands, ProxyMode::Refusing).await;
    wait_for_name(&store, None).await;
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Refused,
    )
    .await;
    assert!(store.snapshot(&fixture.runtime_did).nodes.is_empty());
    let mut changes = store.subscribe();
    tokio::time::timeout(Duration::from_secs(15), async {
        while handle.metrics_snapshot().reconnects < 3 {
            assert_eq!(
                store.runtime_connection(&fixture.runtime_did),
                Some(RuntimeConnectionStatus::Refused),
                "every reconnect of a refusing runtime keeps naming the refusal"
            );
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("a refusal reconnects on the backoff");
    let physical = gents_store::graphql::escape_graphql_string(&fixture.physical);
    fixture
        .access
        .write(
            "test.observer.regranted",
            &format!("mutation {{ update_Node(docID: \"{physical}\", input: {{display_name: \"regranted\"}}) {{ _docID }} }}"),
        )
        .await
        .unwrap();

    set_proxy_mode(&fixture.proxy_commands, ProxyMode::Online).await;
    wait_for_name(&store, Some("regranted")).await;
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Connected,
    )
    .await;
    handle.shutdown().await;
    fixture.stop().await;
}

/// `ObserverResync.startCollections` against a served runtime: a collection
/// reload reads each self-contained root exactly as the full snapshot does,
/// so replacing those roots in the full snapshot changes nothing, and like
/// the full snapshot it refuses a client that cannot observe the Node.
#[tokio::test]
async fn collection_reload_reads_the_rows_a_full_load_shows() {
    use crate::client_projection::query::{
        load_collection_roots_on, replace_collection_roots, OBSERVED_COLLECTIONS,
    };
    use gents_store::config_client::runtime_endpoints::RuntimeEndpoints;

    let fixture = ObservedRuntime::start().await;
    let peer = fixture.peers.records().into_iter().next().unwrap();
    let read_as = |identity: &KeyIdentity| {
        ConfigAccess::Graphql(
            RuntimeEndpoints::new(identity.client_signing_capability().unwrap())
                .endpoint(&peer.node_did, &peer.graphql)
                .unwrap(),
        )
    };
    let access = read_as(&fixture.client);
    let roots: BTreeSet<_> = OBSERVED_COLLECTIONS
        .iter()
        .filter_map(|collection| CollectionRoot::of(collection))
        .collect();
    let full = load_node_snapshot_on(&access, &fixture.runtime_did)
        .await
        .unwrap();
    let reloaded = load_collection_roots_on(&access, &fixture.runtime_did, &roots)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(replace_collection_roots(&full, &roots, reloaded).to_rows()).unwrap(),
        serde_json::to_value(full.to_rows()).unwrap(),
    );

    let directory = tempfile::tempdir().unwrap();
    let key = directory.path().join("ungranted.key");
    let ungranted =
        tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(key, None).unwrap())
            .await
            .unwrap();
    let refused = load_collection_roots_on(&read_as(&ungranted), &fixture.runtime_did, &roots)
        .await
        .unwrap_err();
    assert!(refused.refused(), "{refused}");
    fixture.stop().await;
}

/// Lean `failed_snapshot_reconnects_iff_refused`: a snapshot read the runtime
/// refuses while every subscription stays armed names the refusal and ends
/// the source, which reconnects on the backoff instead of retrying the read.
#[tokio::test]
async fn refused_snapshot_read_reconnects_the_source() {
    let fixture = ObservedRuntime::start().await;
    let configured = fixture.peers.clone();
    let store = ObservedStores::new();
    let handle = spawn_observer(
        store.clone(),
        configured,
        Arc::new(
            gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(
                fixture.client.client_signing_capability().unwrap(),
            ),
        ),
    );
    wait_for_name(&store, Some("remote")).await;
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Connected,
    )
    .await;
    let before = handle.metrics_snapshot();

    set_proxy_mode(&fixture.proxy_commands, ProxyMode::RefusingReads).await;
    let physical = gents_store::graphql::escape_graphql_string(&fixture.physical);
    fixture
        .access
        .write(
            "test.observer.read_refused",
            &format!("mutation {{ update_Node(docID: \"{physical}\", input: {{display_name: \"after read refusal\"}}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Refused,
    )
    .await;
    let mut changes = store.subscribe();
    tokio::time::timeout(Duration::from_secs(15), async {
        while handle.metrics_snapshot().reconnects < before.reconnects + 2 {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("a refused read reconnects the source");

    set_proxy_mode(&fixture.proxy_commands, ProxyMode::Online).await;
    wait_for_name(&store, Some("after read refusal")).await;
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Connected,
    )
    .await;
    handle.shutdown().await;
    fixture.stop().await;
}

/// Lean `reconnects .snapshot .transportFailed = false`: a snapshot read that
/// fails for a transport reason while every subscription stays armed shows
/// the outage and retries the read; it never ends the source, so the healthy
/// streams are not torn down and no reconnect is scheduled.
#[tokio::test]
async fn failed_snapshot_read_keeps_the_streams_armed() {
    let fixture = ObservedRuntime::start().await;
    let configured = fixture.peers.clone();
    let store = ObservedStores::new();
    let handle = spawn_observer(
        store.clone(),
        configured,
        Arc::new(
            gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(
                fixture.client.client_signing_capability().unwrap(),
            ),
        ),
    );
    wait_for_name(&store, Some("remote")).await;
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Connected,
    )
    .await;
    let before = handle.metrics_snapshot();

    set_proxy_mode(&fixture.proxy_commands, ProxyMode::FailingReads).await;
    let physical = gents_store::graphql::escape_graphql_string(&fixture.physical);
    fixture
        .access
        .write(
            "test.observer.read_outage",
            &format!("mutation {{ update_Node(docID: \"{physical}\", input: {{display_name: \"after read outage\"}}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Disconnected,
    )
    .await;
    let during = handle.metrics_snapshot();
    assert!(
        during.fetch_failures > before.fetch_failures,
        "the read failed"
    );
    assert_eq!(
        during.reconnects, before.reconnects,
        "a failed read never reconnects"
    );

    set_proxy_mode(&fixture.proxy_commands, ProxyMode::Online).await;
    wait_for_name(&store, Some("after read outage")).await;
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Connected,
    )
    .await;
    let after = handle.metrics_snapshot();
    assert_eq!(
        after.reconnects, before.reconnects,
        "the armed streams were kept"
    );
    handle.shutdown().await;
    fixture.stop().await;
}

#[tokio::test]
async fn stalled_runtime_snapshot_delays_neither_another_runtime_nor_shutdown() {
    let fixture = ObservedRuntime::start().await;
    let (stalled_url, mut stalled_queries, stalled_task) = stalled_runtime().await;
    let peers = fixture.peers.clone();
    let mut stalled = gents_protocol::runtime_connection::RuntimeConnection {
        label: ("stalled").to_string(),
        node_did: ("did:test:stalled").to_string(),
        graphql: String::new(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    };
    stalled.graphql = stalled_url;
    peers.upsert(stalled).await.unwrap();
    let configured = peers;
    let store = ObservedStores::new();
    let handle = spawn_observer(
        store.clone(),
        configured,
        Arc::new(
            gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(
                fixture.client.client_signing_capability().unwrap(),
            ),
        ),
    );
    tokio::time::timeout(
        Duration::from_secs(15),
        stalled_queries.wait_for(|count| *count > 0),
    )
    .await
    .expect("stalled runtime snapshot in flight")
    .unwrap();
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Connected,
    )
    .await;
    let mut notices = store.subscribe();
    let physical = gents_store::graphql::escape_graphql_string(&fixture.physical);
    fixture
        .access
        .write(
            "test.observer.beside_stall",
            &format!("mutation {{ update_Node(docID: \"{physical}\", input: {{display_name: \"beside stall\"}}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if store
                .snapshot(&fixture.runtime_did)
                .nodes
                .iter()
                .any(|node| node.display_name.as_deref() == Some("beside stall"))
            {
                break;
            }
            notices.changed().await.unwrap();
        }
    })
    .await
    .expect("healthy runtime update beside a stalled snapshot");
    assert_eq!(
        store.runtime_connection(&fixture.runtime_did),
        Some(RuntimeConnectionStatus::Connected)
    );
    assert_eq!(
        store.runtime_connection("did:test:stalled"),
        Some(RuntimeConnectionStatus::Connecting)
    );
    tokio::time::timeout(Duration::from_secs(2), handle.shutdown())
        .await
        .expect("shutdown while a snapshot is stalled");
    stalled_task.abort();
    fixture.stop().await;
}

#[test]
fn generated_reconnect_backoff_drives_the_sources() {
    let output = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.RuntimeConnectionBackoff:olean",
        "Proofs/Conformance/RuntimeConnectionBackoff.lean",
    )
    .unwrap();
    let contract: serde_json::Value = serde_json::from_str(&output).unwrap();
    let origin = |name: &str| match name {
        "source" => remote::Origin::Source,
        "snapshot" => remote::Origin::Snapshot,
        other => panic!("unexpected origin {other}"),
    };
    let reconnects = contract["reconnects"].as_array().unwrap();
    assert_eq!(reconnects.len(), 14);
    for case in reconnects {
        assert_eq!(
            remote::reconnects(
                origin(case["origin"].as_str().unwrap()),
                lean_event(case["event"].as_str().unwrap())
            ),
            case["reconnects"].as_bool().unwrap(),
            "{case}"
        );
    }
    let schedule = contract["schedule"].as_array().unwrap();
    assert!(schedule.len() > 100);
    for case in schedule {
        let attempts = u32::try_from(case["attempts"].as_u64().unwrap()).unwrap();
        let connected = case["connectedMs"].as_u64().map(Duration::from_millis);
        let attempt = remote::reconnect_attempt(attempts, connected);
        assert_eq!(
            u64::from(attempt),
            case["attempt"].as_u64().unwrap(),
            "{case}"
        );
        assert_eq!(
            u64::try_from(remote::reconnect_delay(attempt).as_millis()).unwrap(),
            case["delayMs"].as_u64().unwrap(),
            "{case}"
        );
    }
    assert_eq!(
        u64::try_from(remote::RECONNECT_STABLE.as_millis()).unwrap(),
        contract["stableMs"].as_u64().unwrap()
    );
    assert_eq!(
        u64::from(remote::RETRY_NOW_COUNT),
        contract["retryNowCount"].as_u64().unwrap()
    );
}

/// Every reconnect, after a transport failure or a refusal of the source or
/// a refused read, is scheduled by the one delay owner and counted until the
/// source stays connected for the stable period. A snapshot that failed for
/// a transport reason is not a reconnect: the source stays armed. A runtime
/// that connects and then drops at once keeps backing off.
#[tokio::test]
async fn every_reconnect_path_uses_the_backoff_and_only_a_stable_connection_resets_it() {
    let store = ObservedStores::new();
    let metrics = ObserverMetrics::default();
    let mut sources = remote::Sources::default();
    let node = "did:test:backoff";
    let base = Instant::now();
    let mut waits = Vec::new();
    let mut at = base;
    for (origin, event) in [
        (
            remote::Origin::Source,
            RuntimeConnectionEvent::TransportFailed,
        ),
        (remote::Origin::Source, RuntimeConnectionEvent::Refused),
        (remote::Origin::Snapshot, RuntimeConnectionEvent::Refused),
        (
            remote::Origin::Source,
            RuntimeConnectionEvent::TransportFailed,
        ),
        (remote::Origin::Source, RuntimeConnectionEvent::Refused),
        (remote::Origin::Snapshot, RuntimeConnectionEvent::Refused),
        (
            remote::Origin::Source,
            RuntimeConnectionEvent::TransportFailed,
        ),
    ] {
        sources.start_inert(node);
        // Each cycle connects and drops before the stable period.
        sources.observe(
            node,
            remote::Origin::Snapshot,
            RuntimeConnectionEvent::SnapshotSucceeded,
            &store,
            at,
            &metrics,
        );
        at += Duration::from_secs(1);
        sources.observe(node, origin, event, &store, at, &metrics);
        let due = sources.next_reconnect().unwrap();
        waits.push(u64::try_from(due.duration_since(at).as_millis()).unwrap());
        assert!(!sources.take_due_reconnects(due - Duration::from_millis(1)));
        assert!(sources.take_due_reconnects(due));
        at = due;
    }
    assert_eq!(waits, vec![250, 250, 250, 250, 250, 500, 1000]);
    assert_eq!(metrics.snapshot().reconnects, 7);

    let generation = sources.start_inert(node);
    sources.observe(
        node,
        remote::Origin::Snapshot,
        RuntimeConnectionEvent::TransportFailed,
        &store,
        at,
        &metrics,
    );
    let source = &sources.entries[node];
    assert_eq!(source.generation, generation);
    assert!(!source.ended(), "a failed read keeps the streams armed");
    assert_eq!(sources.next_reconnect(), None);
    assert_eq!(metrics.snapshot().reconnects, 7);

    sources.observe(
        node,
        remote::Origin::Snapshot,
        RuntimeConnectionEvent::SnapshotSucceeded,
        &store,
        at,
        &metrics,
    );
    let stable = at + remote::RECONNECT_STABLE;
    sources.observe(
        node,
        remote::Origin::Source,
        RuntimeConnectionEvent::TransportFailed,
        &store,
        stable,
        &metrics,
    );
    assert_eq!(
        sources.next_reconnect().unwrap().duration_since(stable),
        remote::reconnect_delay(0),
        "a connection that held for the stable period starts the count again"
    );
}

/// TLA `ReconnectsWaitTheBackoff`: a reconcile between a source task's end
/// and the loop applying its end notice keeps the source, so the notice
/// schedules the reconnect on the backoff instead of the reconcile restarting
/// the source at once.
#[tokio::test]
async fn reconcile_keeps_an_ended_source_until_its_notice_schedules_the_reconnect() {
    let store = ObservedStores::new();
    let store = Arc::new(store);
    let metrics = Arc::new(ObserverMetrics::default());
    let mut sources = remote::Sources::default();
    let (notices, mut notice_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut peer = gents_protocol::runtime_connection::RuntimeConnection {
        label: ("runtime").to_string(),
        node_did: ("did:test:ended").to_string(),
        graphql: String::new(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    };
    // Nothing listens on the discard port, so the first handshake fails.
    peer.graphql = "http://127.0.0.1:9/api/v0/graphql".into();
    let directory = tempfile::tempdir().unwrap();
    let client = tokio::task::spawn_blocking({
        let path = directory.path().join("client.key");
        move || KeyIdentity::load_or_create(path, None).unwrap()
    })
    .await
    .unwrap();
    let endpoints = gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(
        client.client_signing_capability().unwrap(),
    );
    let peers = std::slice::from_ref(&peer);
    sources.reconcile(peers, &endpoints, &notices, &store, &metrics);
    let generation = sources.entries["did:test:ended"].generation;
    let notice = tokio::time::timeout(Duration::from_secs(15), notice_rx.recv())
        .await
        .expect("the failed handshake ends the task")
        .unwrap();
    assert_eq!(notice.event, RuntimeConnectionEvent::TransportFailed);
    assert_eq!(notice.generation, generation);

    sources.reconcile(peers, &endpoints, &notices, &store, &metrics);
    assert_eq!(
        sources.entries["did:test:ended"].generation, generation,
        "the ended source is kept for its notice"
    );
    let now = Instant::now();
    sources.observe(
        &notice.node,
        remote::Origin::Source,
        notice.event,
        &store,
        now,
        &metrics,
    );
    assert_eq!(
        sources.next_reconnect().unwrap().duration_since(now),
        remote::reconnect_delay(0)
    );
    assert_eq!(
        store.runtime_connection("did:test:ended"),
        Some(RuntimeConnectionStatus::Disconnected)
    );
}

/// Replays observer traces through the delivery path, the store fences, the
/// loop's notice and settlement handling (`apply_source_notice`,
/// `apply_settlement`, so the reconnect owner `Sources` runs as in the loop),
/// and checks every step against the executed Lean `ObserverResync` owner. A
/// slot is armed only while its source has not ended for a reconnect.
#[tokio::test]
async fn generated_observer_resync_traces_match_the_lean_owner() {
    use crate::client_projection::store::ClientStoreRows;
    use gents_protocol::schemas::{
        AGENT_MESSAGE_NAME, AGENT_TOOL_CALL_NAME, INFERENCE_BACKEND_OBSERVATION_NAME,
    };

    const NODES: usize = 2;
    let did = |node: usize| format!("did:resync:{node}");
    let session = |node: usize| {
        serde_json::from_value(serde_json::json!({
            "node_did": did(node), "session_id": format!("session-{node}"),
            "agent_did": "agent", "created_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    };
    // `Conformance.ObserverResync.snapshot`.
    let snapshot = |node: usize| {
        ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(node)],
            ..Default::default()
        })
    };
    // `Conformance.ObserverResync.observationRows`.
    let observations = |node: usize| {
        ClientStore::from_rows(ClientStoreRows {
            backend_observations: vec![serde_json::from_value(serde_json::json!({
                "backend_id": format!("observation-{node}"),
                "probe_status": null, "last_probe": null,
            }))
            .unwrap()],
            ..Default::default()
        })
    };
    let op = |op: &str, node: usize, result: &str| serde_json::json!({"op": op, "node": node, "result": result});
    let alphabet = [
        op("armed", 0, ""),
        op("start", 0, ""),
        op("start", 1, ""),
        op("deliver", 0, ""),
        op("deliver", 1, ""),
        op("tool_call_event", 0, ""),
        op("transcript_event", 0, ""),
        op("stream_failed", 0, ""),
        op("stream_refused", 0, ""),
        op("source_started", 0, ""),
        op("publish", 0, ""),
        op("publish", 1, ""),
        op("timer", 0, ""),
        op("retry_now", 0, ""),
        op("finish", 0, "rows"),
        op("finish", 0, "transport_failed"),
        op("finish", 0, "refused"),
        op("finish", 1, "rows"),
        op("deliver_collection", 0, ""),
        op("start_collections", 0, ""),
        op("finish_collections", 0, "rows"),
        op("finish_collections", 0, "transport_failed"),
        op("finish_collections", 0, "refused"),
    ];
    let prefix = [
        op("armed", 0, ""),
        op("armed", 1, ""),
        op("start", 0, ""),
        op("start", 1, ""),
    ];
    // Node 0 reloading its collections, node 1 loading.
    let collection_prefix = [
        op("armed", 0, ""),
        op("armed", 1, ""),
        op("start", 0, ""),
        op("start", 1, ""),
        op("finish", 0, "rows"),
        op("deliver_collection", 0, ""),
        op("start_collections", 0, ""),
    ];
    let suffix = [
        op("finish", 0, "rows"),
        op("finish_collections", 0, "rows"),
        op("timer", 0, ""),
        op("armed", 0, ""),
        op("start", 0, ""),
        op("finish", 0, "rows"),
        op("finish", 1, "rows"),
        op("deliver_collection", 0, ""),
        op("start_collections", 0, ""),
        op("finish_collections", 0, "rows"),
    ];
    let mut traces = Vec::new();
    for a in &alphabet {
        for b in &alphabet {
            let ops: Vec<_> = [a, b].into_iter().chain(suffix.iter()).cloned().collect();
            traces.push(serde_json::json!({"nodes": NODES, "ops": ops}));
            let ops: Vec<_> = collection_prefix
                .iter()
                .chain([a, b])
                .chain(suffix.iter())
                .cloned()
                .collect();
            traces.push(serde_json::json!({"nodes": NODES, "ops": ops}));
            for c in &alphabet {
                let ops: Vec<_> = prefix
                    .iter()
                    .chain([a, b, c])
                    .chain(suffix.iter())
                    .cloned()
                    .collect();
                traces.push(serde_json::json!({"nodes": NODES, "ops": ops}));
            }
        }
    }
    let serialized = serde_json::to_string(&traces).unwrap();
    let output = tokio::task::spawn_blocking(move || {
        gents_lean_contract::run_prepared_generator_with_input(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.ObserverResync:olean",
            "Proofs/Conformance/ObserverResync.lean",
            &serialized,
        )
    })
    .await
    .unwrap()
    .unwrap();
    let modeled: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert_eq!(modeled.len(), traces.len());

    let metrics = ObserverMetrics::default();
    let mut settlements = BTreeSet::new();
    let mut collection_settlements = BTreeSet::new();
    let mut retries = BTreeSet::new();
    for (trace, expected) in traces.iter().zip(&modeled) {
        let stores = ObservedStores::new();
        let mut queue = Queue::default();
        let mut sources = remote::Sources::default();
        let mut generation = [0_u64; NODES];
        // A node's read in flight: its capture, and the roots of a collection
        // reload (`None` for a full load).
        type Reading = (
            projection_store::StoreReadFence,
            Option<BTreeSet<CollectionRoot>>,
        );
        let mut in_flight = BTreeMap::<String, Reading>::new();
        let now = Instant::now();
        for (node, generation) in generation.iter_mut().enumerate() {
            *generation = sources.start_inert(&did(node));
            stores.ensure(&did(node));
            stores.observe_runtime_connection(&did(node), RuntimeConnectionEvent::SourceStarted);
        }
        let ops = trace["ops"].as_array().unwrap();
        let steps = expected["steps"].as_array().unwrap();
        assert_eq!(ops.len(), steps.len());
        for (step, modeled_step) in ops.iter().zip(steps) {
            let node = usize::try_from(step["node"].as_u64().unwrap()).unwrap();
            let node_did = did(node);
            let store = stores.store(&node_did).unwrap();
            let result = step["result"].as_str().unwrap();
            // What `observe/remote.rs` and the loop's notice arm do. A source
            // that ended for a reconnect sends nothing more: its next notice
            // comes from the source its due reconnect starts, which keeps the
            // shown status.
            let notice = |queue: &mut Queue,
                          sources: &mut remote::Sources,
                          in_flight: &mut BTreeMap<String, Reading>,
                          generation: &mut u64,
                          event: RuntimeConnectionEvent| {
                if sources.entries[&node_did].ended() {
                    *generation = sources.start_inert(&node_did);
                }
                if SourceNotice::of(event) != Some(SourceNotice::Armed) {
                    store.observe_request_event();
                }
                let kind = apply_source_notice(
                    queue,
                    sources,
                    &stores,
                    &node_did,
                    *generation,
                    event,
                    &metrics,
                )
                .expect("a source task event");
                if kind.aborts_load() {
                    in_flight.remove(&node_did);
                }
            };
            let mut finished = serde_json::Value::Null;
            match step["op"].as_str().unwrap() {
                "source_started" => {
                    generation[node] = sources.start_inert(&node_did);
                    in_flight.remove(&node_did);
                    stores.observe_runtime_connection(
                        &node_did,
                        RuntimeConnectionEvent::SourceStarted,
                    );
                }
                "armed" => notice(
                    &mut queue,
                    &mut sources,
                    &mut in_flight,
                    &mut generation[node],
                    RuntimeConnectionEvent::SubscriptionsReady,
                ),
                "stream_failed" => notice(
                    &mut queue,
                    &mut sources,
                    &mut in_flight,
                    &mut generation[node],
                    RuntimeConnectionEvent::TransportFailed,
                ),
                "stream_refused" => notice(
                    &mut queue,
                    &mut sources,
                    &mut in_flight,
                    &mut generation[node],
                    RuntimeConnectionEvent::Refused,
                ),
                "start" => {
                    if queue.startable(
                        &node_did,
                        generation[node],
                        in_flight.contains_key(&node_did),
                    ) {
                        queue.start(&node_did);
                        in_flight.insert(node_did.clone(), (store.capture_read(), None));
                    }
                }
                "start_collections" => {
                    if queue.collections_startable(
                        &node_did,
                        generation[node],
                        in_flight.contains_key(&node_did),
                    ) {
                        let roots = queue.start_collections(&node_did);
                        in_flight.insert(node_did.clone(), (store.capture_read(), Some(roots)));
                    }
                }
                "finish_collections" => match in_flight.remove(&node_did) {
                    Some((captured, Some(roots))) => {
                        let read = match result {
                            "rows" => Ok(observations(node)),
                            "refused" => Err(SnapshotReadError::NodeUnavailable),
                            _ => Err(SnapshotReadError::from(anyhow::anyhow!("unreachable"))),
                        };
                        let settlement = settle_collection_reload(
                            &store,
                            &captured,
                            roots.clone(),
                            read,
                            &metrics,
                        );
                        queue.settle_collections(&node_did, settlement, roots);
                        let name = match settlement {
                            CollectionSettlement::Published => "published",
                            CollectionSettlement::Superseded => "superseded",
                            CollectionSettlement::Failed => "failed",
                        };
                        collection_settlements.insert(name);
                        finished = serde_json::json!({"collection_settlement": name});
                    }
                    Some(load) => {
                        in_flight.insert(node_did.clone(), load);
                    }
                    None => {}
                },
                "deliver" | "tool_call_event" | "transcript_event" | "deliver_collection" => {
                    let collection = match step["op"].as_str().unwrap() {
                        "deliver" => "AgentRequest",
                        "tool_call_event" => AGENT_TOOL_CALL_NAME,
                        "deliver_collection" => INFERENCE_BACKEND_OBSERVATION_NAME,
                        _ => AGENT_MESSAGE_NAME,
                    };
                    delivery::deliver(
                        &store,
                        &metrics,
                        collection,
                        &serde_json::json!({"data": {collection: [{"_docID": "changed"}]}}),
                    )
                    .unwrap();
                    queue.take_requests(&stores);
                }
                "publish" => {
                    assert!(store
                        .replace_reloaded_snapshot_at_read(&store.capture_read(), snapshot(node)));
                }
                "timer" => {
                    queue.retry_at.remove(&node_did);
                }
                "retry_now" => {
                    let restarted =
                        apply_retry_now(&mut queue, &mut sources, &stores, &node_did, now);
                    if restarted {
                        in_flight.remove(&node_did);
                    }
                    retries.insert(restarted);
                }
                "finish" => {
                    let loading = matches!(in_flight.get(&node_did), Some((_, None)));
                    if let Some((captured, _)) =
                        loading.then(|| in_flight.remove(&node_did)).flatten()
                    {
                        let patch_accepted = store.accepts_chat_patch_read(&captured);
                        let settlement = match result {
                            "transport_failed" => Settlement::TransportFailed,
                            "refused" => {
                                clear_node(&stores, &node_did);
                                Settlement::Refused
                            }
                            _ => {
                                if store
                                    .replace_reloaded_snapshot_at_read(&captured, snapshot(node))
                                {
                                    Settlement::Published
                                } else {
                                    Settlement::Superseded
                                }
                            }
                        };
                        apply_settlement(
                            &mut queue,
                            &mut sources,
                            &stores,
                            &node_did,
                            settlement,
                            now,
                            &metrics,
                        );
                        let name = match settlement {
                            Settlement::Published => "published",
                            Settlement::Superseded => "superseded",
                            Settlement::TransportFailed => "transport_failed",
                            Settlement::Refused => "refused",
                        };
                        settlements.insert(name);
                        finished = serde_json::json!({
                            "settlement": name, "patch_accepted": patch_accepted,
                        });
                    }
                }
                other => panic!("unknown modeled op {other}"),
            }
            assert_eq!(finished, modeled_step["result"], "{step} in {trace}");
            let slot = &modeled_step["slot"];
            let observed = serde_json::json!({
                "pending": queue.pending.contains(&node_did),
                "waiting": queue.retry_at.contains_key(&node_did),
                "loading": matches!(in_flight.get(&node_did), Some((_, None))),
                "collections": queue.collections.get(&node_did).is_some_and(|roots| !roots.is_empty()),
                "reloading_collections": matches!(in_flight.get(&node_did), Some((_, Some(_)))),
                "armed": queue.armed.get(&node_did) == Some(&generation[node])
                    && !sources.entries[&node_did].ended(),
                "status": stores.runtime_connection(&node_did).unwrap().as_str(),
                "presentation": slot["presentation"],
            });
            assert_eq!(&observed, slot, "{step} in {trace}");
            assert_ne!(slot["presentation"], "stalled", "{step} in {trace}");
        }
        for (node, modeled) in expected["rows"].as_array().unwrap().iter().enumerate() {
            let stored: Vec<_> = stores
                .snapshot(&did(node))
                .sessions
                .iter()
                .map(|row| serde_json::json!(row.session_id))
                .collect();
            assert_eq!(&serde_json::Value::Array(stored), modeled, "{trace}");
        }
        for (node, modeled) in expected["observations"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let stored: Vec<_> = stores
                .snapshot(&did(node))
                .backend_observations
                .iter()
                .map(|row| serde_json::json!(row.backend_id))
                .collect();
            assert_eq!(&serde_json::Value::Array(stored), modeled, "{trace}");
        }
    }
    assert_eq!(settlements.len(), 4, "every settlement is exercised");
    assert_eq!(
        collection_settlements.len(),
        3,
        "every collection settlement is exercised"
    );
    assert_eq!(
        retries.len(),
        2,
        "a retry of a connected and of an unconnected node is exercised"
    );
}

fn runtime_status(key_path: &std::path::Path) -> axum::Router {
    gents_store::test_support::local_authentication_route(Arc::new(
        KeyIdentity::load_or_create(key_path.to_path_buf(), None).unwrap(),
    ))
}
