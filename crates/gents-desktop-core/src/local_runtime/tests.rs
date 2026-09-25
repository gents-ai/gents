use super::identity::{normalize_optional_string, resolve_p2p_peer_id};
use super::{
    augment_peer_status_payload_for_desktop, await_serving_runtime_within,
    dangerously_overwrite_desktop_home, default_agent_home, graphql_endpoint_for_desktop_access,
    init_standard_local_runtime, load_standard_runtime_identity, render_human_summary,
    reset_desktop_runtime_state, runtime_graphql_url, runtime_status_url, serving_runtime,
    DesktopInitOptions, DesktopInitSummary, StoredRuntimeState, LOCAL_STANDARD_SOURCE,
};
use crate::client::DesktopPaths;
use gents_protocol::serve_lifecycle::ObservedServeLifecycle;
use serde_json::json;

fn sample_summary() -> DesktopInitSummary {
    DesktopInitSummary {
        status: "initialized",
        source: LOCAL_STANDARD_SOURCE,
        agent_home: "/tmp/agent".to_string(),
        desktop_home: "/tmp/desktop".to_string(),
        peer_directory: "/tmp/desktop/peers.json".to_string(),
        label: "Local Agent".to_string(),
        agent_name: "default".to_string(),
        agent_did: "did:test:default".to_string(),
        graphql: "http://127.0.0.1:9191/graphql".to_string(),
        p2p_transport: "iroh".to_string(),
        p2p_peer_id: "peer-runtime".to_string(),
        p2p_listen_address: "iroh://peer-runtime".to_string(),
        peer_record_id: "peer-runtime".to_string(),
        next_steps: vec![
            "Run `gents-desktop` and leave the desktop app open.".to_string(),
            "Wait for the status bar to show `replication subscriptions armed`.".to_string(),
            "Then submit prompts from Chat, or run `gents chat` in another terminal.".to_string(),
        ],
    }
}

#[test]
fn init_summary_serializes_camel_case() {
    let summary = DesktopInitSummary {
        status: "ok",
        source: "local",
        agent_home: "/h".into(),
        desktop_home: "/d".into(),
        peer_directory: "/p".into(),
        label: "L".into(),
        agent_name: "n".into(),
        agent_did: "did:key:z".into(),
        graphql: "http://x".into(),
        p2p_transport: "iroh".into(),
        p2p_peer_id: "pid".into(),
        p2p_listen_address: "addr".into(),
        peer_record_id: "rec".into(),
        next_steps: vec![],
    };
    let value = serde_json::to_value(&summary).unwrap();
    assert_eq!(value["agentDid"], "did:key:z");
    assert!(value.get("agent_did").is_none());
    assert!(value.get("statusEndpoint").is_none());
}

#[test]
fn default_agent_home_uses_fresh_gents_home() {
    let home = default_agent_home().expect("agent home");

    assert_eq!(
        home.file_name().and_then(|name| name.to_str()),
        Some(".gents")
    );
}

#[test]
fn configured_runtime_missing_key_is_not_created_by_desktop_read() {
    let tempdir = tempfile::tempdir().unwrap();
    let key_path = tempdir.path().join("missing.key");
    std::fs::write(
        tempdir.path().join("init.json"),
        serde_json::json!({
            "agent_name": "local",
            "agent_did": "did:key:configured",
            "key_path": key_path,
        })
        .to_string(),
    )
    .unwrap();

    let error = load_standard_runtime_identity(tempdir.path())
        .err()
        .expect("missing configured key must be rejected");
    assert!(format!("{error:#}").contains("identity key does not exist"));
    assert!(!key_path.exists());
}

#[test]
fn configured_runtime_wrong_existing_key_is_preserved() {
    let tempdir = tempfile::tempdir().unwrap();
    let key_path = tempdir.path().join("existing.key");
    gents::identity::KeyIdentity::load_or_create(&key_path, None).unwrap();
    let original = std::fs::read(&key_path).unwrap();
    std::fs::write(
        tempdir.path().join("init.json"),
        serde_json::json!({
            "agent_name": "local",
            "agent_did": "did:key:different",
            "key_path": key_path,
        })
        .to_string(),
    )
    .unwrap();

    let error = load_standard_runtime_identity(tempdir.path())
        .err()
        .expect("wrong configured key must be rejected");
    assert!(format!("{error:#}").contains("identity does not match configured agent DID"));
    assert_eq!(std::fs::read(&key_path).unwrap(), original);
}

#[test]
fn init_summary_tells_demo_to_wait_for_desktop_bootstrap() {
    let summary = sample_summary();
    assert!(summary
        .next_steps
        .iter()
        .any(|step| step.contains("replication subscriptions armed")));

    let rendered = render_human_summary(&summary);
    assert!(rendered.contains("desktop app completes P2P pairing"));
    assert!(rendered.contains("replication subscriptions armed"));
    assert!(rendered.contains("Then submit prompts from Chat"));
}

#[test]
fn runtime_status_url_accepts_bare_host_and_graphql_endpoint() {
    assert_eq!(
        runtime_status_url("127.0.0.1:9181").expect("bare host should normalize"),
        "http://127.0.0.1:9181/status"
    );
    assert_eq!(
        runtime_status_url("http://127.0.0.1:9181/api/v0/graphql")
            .expect("graphql endpoint should normalize"),
        "http://127.0.0.1:9181/status"
    );
}

#[test]
fn runtime_graphql_url_preserves_user_supplied_graphql_endpoint() {
    assert_eq!(
        runtime_graphql_url("100.73.235.38:9181/api/v0/graphql?ignored=true")
            .expect("graphql endpoint should normalize"),
        "http://100.73.235.38:9181/api/v0/graphql"
    );
}

#[test]
fn desktop_graphql_rewrites_loopback_endpoint_for_remote_status_host() {
    let payload = serde_json::json!({
        "graphql": "http://127.0.0.1:9181/api/v0/graphql"
    });

    assert_eq!(
        graphql_endpoint_for_desktop_access(&payload, "http://100.73.235.38:9181/status")
            .as_deref(),
        Some("http://100.73.235.38:9181/api/v0/graphql")
    );
}

#[test]
fn desktop_graphql_is_added_to_status_payload() {
    let payload = augment_peer_status_payload_for_desktop(
        serde_json::json!({
            "agent_did": "did:key:z6MkAgent",
            "graphql": "http://127.0.0.1:9181/api/v0/graphql"
        }),
        "http://100.73.235.38:9181/status",
    );

    assert_eq!(
        payload
            .get("desktop_graphql")
            .and_then(serde_json::Value::as_str),
        Some("http://100.73.235.38:9181/api/v0/graphql")
    );
}

#[test]
fn normalize_optional_string_discards_empty_values() {
    assert_eq!(
        normalize_optional_string(Some(" endpoint-ticket-123 ")).as_deref(),
        Some("endpoint-ticket-123")
    );
    assert_eq!(normalize_optional_string(Some("   ")), None);
    assert_eq!(normalize_optional_string(None), None);
}

#[test]
fn resolve_p2p_peer_id_uses_shareable_address_when_identity_is_missing() {
    let peer_id = resolve_p2p_peer_id(
        None,
        Some("127.0.0.1:56000/p2p/peer-alpha"),
        Some("persisted-peer"),
    );

    assert_eq!(peer_id.as_deref(), Some("peer-alpha"));
}

#[test]
fn resolve_p2p_peer_id_falls_back_to_stored_value() {
    let peer_id = resolve_p2p_peer_id(None, None, Some("persisted-peer"));

    assert_eq!(peer_id.as_deref(), Some("persisted-peer"));
}

#[test]
fn reset_desktop_runtime_state_removes_node_dir_only() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let paths = DesktopPaths::from_root(tempdir.path());
    std::fs::create_dir_all(paths.node_data_dir()).expect("node dir");
    std::fs::write(paths.node_data_dir().join("store.bin"), "x").expect("node data");
    std::fs::write(paths.peer_directory_path(), "{}").expect("peer directory");

    let cleared = reset_desktop_runtime_state(&paths).expect("reset desktop runtime state");

    assert!(cleared);
    assert!(!paths.node_data_dir().exists());
    assert!(paths.peer_directory_path().exists());
}

#[test]
fn dangerously_overwrite_desktop_home_removes_root_dir() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let desktop_root = tempdir.path().join("desktop");
    std::fs::create_dir_all(&desktop_root).expect("desktop root");
    std::fs::write(desktop_root.join("peers.json"), "{}").expect("desktop file");

    dangerously_overwrite_desktop_home(&desktop_root).expect("overwrite desktop home");

    assert!(!desktop_root.exists());
}

#[test]
fn discovery_binds_to_the_live_ready_did() {
    let did = "did:key:z6MkLocal";
    let observe = |status: serde_json::Value| serving_runtime(&status, did);
    assert_eq!(
        observe(json!({ "agent_did": did, "lifecycle": "starting" })).unwrap(),
        ObservedServeLifecycle::Starting
    );
    assert_eq!(
        observe(json!({ "agent_did": did, "lifecycle": "ready" })).unwrap(),
        ObservedServeLifecycle::Ready
    );
    assert_eq!(
        observe(json!({ "agent_did": did, "version": "0.18.2" })).unwrap(),
        ObservedServeLifecycle::Outdated {
            version: Some("0.18.2".to_string())
        }
    );
    for live in [
        json!({ "lifecycle": "ready" }),
        json!({ "agent_did": "", "lifecycle": "ready" }),
        json!({ "agent_did": "   ", "lifecycle": "ready" }),
        json!({ "agent_did": 7, "lifecycle": "ready" }),
        json!({ "agent_did": "z6MkLocal", "lifecycle": "ready" }),
        json!({ "agent_did": "did:", "lifecycle": "ready" }),
        json!({ "agent_did": "did:key:z6MkOther", "lifecycle": "ready" }),
    ] {
        assert!(observe(live.clone()).is_err(), "accepted {live}");
    }
    for blank in ["", "  "] {
        assert!(
            serving_runtime(&json!({ "agent_did": blank, "lifecycle": "ready" }), blank).is_err()
        );
    }
}

/// Answers every request with one JSON body, starting after `delay`.
async fn serve_status_after(
    listener: std::net::TcpListener,
    delay: std::time::Duration,
    body: serde_json::Value,
) -> tokio::task::JoinHandle<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = body.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    })
}

fn seed_home(home: &std::path::Path, did: &str, graphql: &str) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(
        home.join("init.json"),
        json!({ "agent_name": "Local", "agent_did": did }).to_string(),
    )
    .unwrap();
    std::fs::write(
        home.join("runtime.json"),
        json!({
            "graphql": graphql,
            "agent_name": "Local",
            "agent_did": did,
            "p2p_transport": "iroh",
        })
        .to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn discovery_rejects_a_bad_live_did_without_touching_the_peer_store() {
    let did = "did:key:z6MkLocal";
    for live in [
        json!(null),
        json!(""),
        json!("not-a-did"),
        json!("did:key:z6MkOther"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = serve_status_after(
            listener,
            std::time::Duration::ZERO,
            json!({ "agent_did": live, "lifecycle": "ready" }),
        )
        .await;
        let home = temp.path().join("agent");
        seed_home(
            &home,
            did,
            &format!("http://127.0.0.1:{port}/api/v0/graphql"),
        );
        let paths = DesktopPaths::from_root(temp.path().join("desktop"));

        let error = init_standard_local_runtime(DesktopInitOptions {
            agent_home: home,
            desktop_paths: paths.clone(),
            label: "Local".to_string(),
        })
        .await
        .expect_err("a live identity other than the initialized one is rejected");
        assert!(error.to_string().contains("serves"), "{error:#}");
        assert!(
            !paths.peer_directory_path().exists(),
            "no route is saved for {live}"
        );
        server.abort();
    }
}

#[tokio::test]
async fn discovery_waits_for_a_runtime_that_is_not_listening_yet() {
    let did = "did:key:z6MkLocal";
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let runtime = StoredRuntimeState {
        graphql: format!("http://127.0.0.1:{port}/api/v0/graphql"),
        agent_name: "Local".to_string(),
        agent_did: did.to_string(),
        p2p_transport: "iroh".to_string(),
        p2p_peer_id: None,
    };
    // The socket is bound but not accepting, as while the runtime starts.
    let server = serve_status_after(
        listener,
        std::time::Duration::from_millis(600),
        json!({ "agent_did": did, "lifecycle": "ready" }),
    )
    .await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(300))
        .build()
        .unwrap();
    await_serving_runtime_within(&client, &runtime, std::time::Duration::from_secs(10))
        .await
        .expect("discovery waits for the runtime to answer");
    server.abort();
}

#[tokio::test]
async fn discovery_fails_at_once_for_a_runtime_that_predates_readiness() {
    let did = "did:key:z6MkLocal";
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = serve_status_after(
        listener,
        std::time::Duration::ZERO,
        json!({ "agent_did": did, "version": "0.18.2" }),
    )
    .await;
    let runtime = StoredRuntimeState {
        graphql: format!("http://127.0.0.1:{port}/api/v0/graphql"),
        agent_name: "Local".to_string(),
        agent_did: did.to_string(),
        p2p_transport: "iroh".to_string(),
        p2p_peer_id: None,
    };
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        await_serving_runtime_within(
            &reqwest::Client::new(),
            &runtime,
            std::time::Duration::from_secs(60),
        ),
    )
    .await
    .expect("an outdated runtime is not waited on")
    .unwrap_err();
    assert!(
        error.to_string().contains("v0.18.2) predates this app"),
        "{error:#}"
    );
    server.abort();
}
