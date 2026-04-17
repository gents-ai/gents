use super::identity::{normalize_optional_string, resolve_p2p_peer_id};
use super::pairing::P2pReplicatorRequest;
use super::{render_human_summary, DesktopInitSummary, LOCAL_STANDARD_SOURCE};

fn sample_summary() -> DesktopInitSummary {
    DesktopInitSummary {
        status: "initialized",
        source: LOCAL_STANDARD_SOURCE,
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
            "Wait for the status bar to show `replication: subscriptions armed`.".to_string(),
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
        .any(|step| step.contains("replication: subscriptions armed")));

    let rendered = render_human_summary(&summary);
    assert!(rendered.contains("desktop app completes P2P pairing"));
    assert!(rendered.contains("replication: subscriptions armed"));
    assert!(rendered.contains("Then submit prompts from Chat"));
}

#[test]
fn replicator_request_serializes_runtime_api_field_names() {
    let payload = serde_json::to_value(P2pReplicatorRequest {
        collections: vec!["AgentRequest".to_string()],
        addresses: vec!["127.0.0.1:9999/p2p/example".to_string()],
    })
    .expect("serialize replicator request");

    assert_eq!(
        payload,
        serde_json::json!({
            "Collections": ["AgentRequest"],
            "Addresses": ["127.0.0.1:9999/p2p/example"],
        })
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
