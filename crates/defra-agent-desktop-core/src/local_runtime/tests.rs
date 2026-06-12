use super::identity::{normalize_optional_string, resolve_p2p_peer_id};
use super::{
    augment_peer_status_payload_for_desktop, dangerously_overwrite_desktop_home,
    extract_status_endpoint_connection, graphql_endpoint_for_desktop_access, render_human_summary,
    reset_desktop_runtime_state, runtime_discovery_url, runtime_graphql_url, runtime_status_url,
    DesktopInitSummary, LOCAL_STANDARD_SOURCE,
};
use crate::client::DesktopPaths;

fn sample_summary() -> DesktopInitSummary {
    DesktopInitSummary {
        status: "initialized",
        source: LOCAL_STANDARD_SOURCE,
        status_endpoint: None,
        agent_home: "/tmp/agent".to_string(),
        desktop_home: "/tmp/desktop".to_string(),
        peer_directory: "/tmp/desktop/peers.json".to_string(),
        label: "Local Agent".to_string(),
        agent_name: "default".to_string(),
        agent_did: "did:defra-agent:default".to_string(),
        graphql: "http://127.0.0.1:9191/graphql".to_string(),
        p2p_transport: "iroh".to_string(),
        p2p_peer_id: "peer-runtime".to_string(),
        p2p_listen_address: "iroh://peer-runtime".to_string(),
        peer_record_id: "peer-runtime".to_string(),
        next_steps: vec![
            "Run `defra-agent-desktop` and leave the desktop app open.".to_string(),
            "Wait for the status bar to show `replication subscriptions armed`.".to_string(),
            "Then submit prompts from Chat, or run `defra-agent chat` in another terminal."
                .to_string(),
        ],
    }
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
fn remote_status_summary_render_does_not_call_it_local() {
    let mut summary = sample_summary();
    summary.source = "server-status";
    summary.status_endpoint = Some("http://100.73.235.38:9181/status".to_string());
    summary.agent_home.clear();

    let rendered = render_human_summary(&summary);

    assert!(rendered.contains("Discovered defra-agent runtime from discovery endpoint"));
    assert!(!rendered.contains("Discovered local defra-agent runtime"));
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
    assert_eq!(
        runtime_discovery_url("http://100.73.235.38:9181/api/v0/graphql")
            .expect("graphql endpoint should be a discovery URL"),
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
fn status_endpoint_connection_extracts_defra_agent_status_shape() {
    let payload = augment_peer_status_payload_for_desktop(
        serde_json::json!({
            "agent_name": "studio-1-steward",
            "agent_did": "did:key:z6MkStudio",
            "graphql": "http://127.0.0.1:9181/api/v0/graphql",
            "p2p": {
                "p2p_transport": "iroh",
                "p2p_peer_id": "peer-alpha",
                "p2p_shareable_address": "127.0.0.1:56000/p2p/peer-alpha"
            }
        }),
        "http://100.73.235.38:9181/status",
    );

    let connection =
        extract_status_endpoint_connection(&payload, "http://100.73.235.38:9181/status", None)
            .expect("status payload should be accepted");

    assert_eq!(connection.label, "studio-1-steward");
    assert_eq!(connection.agent_did, "did:key:z6MkStudio");
    assert_eq!(
        connection.graphql,
        "http://100.73.235.38:9181/api/v0/graphql"
    );
    assert_eq!(connection.p2p_peer_id, "peer-alpha");
    assert_eq!(
        connection.p2p_listen_address,
        "127.0.0.1:56000/p2p/peer-alpha"
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
