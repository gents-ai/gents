use super::identity::{normalize_optional_string, resolve_p2p_peer_id};
use super::{
    augment_peer_status_payload_for_desktop, dangerously_overwrite_desktop_home,
    default_agent_home, graphql_endpoint_for_desktop_access, render_human_summary,
    reset_desktop_runtime_state, runtime_graphql_url, runtime_status_url, DesktopInitSummary,
    LOCAL_STANDARD_SOURCE,
};
use crate::client::DesktopPaths;

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
fn default_agent_home_uses_fresh_gents_home() {
    let home = default_agent_home().expect("agent home");

    assert_eq!(
        home.file_name().and_then(|name| name.to_str()),
        Some(".gents")
    );
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
