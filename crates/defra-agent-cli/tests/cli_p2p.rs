mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[test]
fn p2p_pairings_manage_desired_rows_locally() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("pairings-agent");
    fs::create_dir_all(&home)?;

    let set = run_cli_json(
        &home,
        &[
            "p2p",
            "pairings",
            "set",
            "--peer",
            "peer-one",
            "--did",
            "did:key:peer-one",
            "--address",
            "/ip4/127.0.0.1/tcp/4001/p2p/peer-one",
            "--collection",
            "AgentRequest",
            "--profile",
            "tool-services",
        ],
    )?;
    assert_eq!(
        set.get("status").and_then(Value::as_str),
        Some("pairing_set")
    );
    assert_eq!(
        set.get("access_mode").and_then(Value::as_str),
        Some("local")
    );
    assert_eq!(set.get("peer_id").and_then(Value::as_str), Some("peer-one"));
    assert!(set
        .get("note")
        .and_then(Value::as_str)
        .is_some_and(|note| note.contains("running defra-agent runtime")));

    let list = run_cli_json(&home, &["p2p", "pairings", "list"])?;
    assert_eq!(list.get("count").and_then(Value::as_u64), Some(1));
    let row = list
        .get("pairings")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| anyhow!("pairings list missing row: {list}"))?;
    assert_eq!(row.get("peer_id").and_then(Value::as_str), Some("peer-one"));
    assert_eq!(
        row.get("agent_did").and_then(Value::as_str),
        Some("did:key:peer-one")
    );
    assert!(row
        .get("collections")
        .and_then(Value::as_array)
        .is_some_and(|rows| rows.iter().any(|row| row.as_str() == Some("AgentRequest"))));
    assert!(row
        .get("collections")
        .and_then(Value::as_array)
        .is_some_and(|rows| rows
            .iter()
            .any(|row| row.as_str() == Some("ToolServiceRegistry"))));
    assert!(row
        .get("profiles")
        .and_then(Value::as_array)
        .is_some_and(|rows| rows.iter().any(|row| row.as_str() == Some("tool-services"))));

    let remove = run_cli_json(&home, &["p2p", "unpair", "--peer", "peer-one"])?;
    assert_eq!(
        remove.get("status").and_then(Value::as_str),
        Some("pairing_removed")
    );
    assert_eq!(remove.get("removed_count").and_then(Value::as_u64), Some(1));

    let list = run_cli_json(&home, &["p2p", "pairings", "list"])?;
    assert_eq!(list.get("count").and_then(Value::as_u64), Some(0));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2p_connects_two_local_servers_via_operator_commands() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_a = tempdir.path().join("amy");
    let home_b = tempdir.path().join("coding");
    fs::create_dir_all(&home_a)?;
    fs::create_dir_all(&home_b)?;

    let model_name = format!("mock-p2p-connect-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port_a = allocate_port()?;
    let port_b = allocate_port()?;
    let agent_name_a = format!("cli-amy-{}", Uuid::new_v4().simple());
    let agent_name_b = format!("cli-coding-{}", Uuid::new_v4().simple());
    let graphql_a = graphql_url(port_a);
    let graphql_b = graphql_url(port_b);

    let init_a = run_init_json(
        &home_a,
        &[
            "--agent-name",
            &agent_name_a,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let init_b = run_init_json(
        &home_b,
        &[
            "--agent-name",
            &agent_name_b,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did_a = agent_did_from_init(&init_a)?;
    let agent_did_b = agent_did_from_init(&init_b)?;

    let (mut serve_a, readiness_a) = spawn_server_with_ready_json(
        &home_a,
        port_a,
        &[
            "--p2p-bind-addr",
            "127.0.0.1",
            "--p2p-port",
            "0",
            "--p2p-relay-mode",
            "disabled",
            "--p2p-discovery",
            "disabled",
        ],
        &[],
    )?;
    let (mut serve_b, readiness_b) = spawn_server_with_ready_json(
        &home_b,
        port_b,
        &[
            "--p2p-bind-addr",
            "127.0.0.1",
            "--p2p-port",
            "0",
            "--p2p-relay-mode",
            "disabled",
            "--p2p-discovery",
            "disabled",
        ],
        &[],
    )?;
    wait_for_port(port_a, &mut serve_a)?;
    wait_for_port(port_b, &mut serve_b)?;
    wait_for_runtime_ready(&graphql_a, &agent_did_a, Duration::from_secs(30)).await?;
    wait_for_runtime_ready(&graphql_b, &agent_did_b, Duration::from_secs(30)).await?;

    let peer_id_a = readiness_a
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Amy readiness JSON missing p2p_peer_id: {readiness_a}"))?;
    let peer_id_b = readiness_b
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Coding readiness JSON missing p2p_peer_id: {readiness_b}"))?;
    let peer_addr_a = readiness_a
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Amy readiness JSON missing P2P listen address: {readiness_a}"))?;

    let connect = run_cli_json(&home_b, &["p2p", "connect", "--peer", peer_addr_a])?;
    assert_eq!(
        connect.get("status").and_then(Value::as_str),
        Some("connect_requested")
    );

    let status_b = wait_for_connected_peer(&home_b, peer_id_a, Duration::from_secs(20)).await?;
    let status_a = wait_for_connected_peer(&home_a, peer_id_b, Duration::from_secs(20)).await?;
    assert!(status_b
        .get("p2p_connected_peers")
        .and_then(Value::as_array)
        .is_some_and(|rows| rows
            .iter()
            .filter_map(Value::as_str)
            .any(|row| row.contains(peer_id_a))));
    assert!(status_a
        .get("p2p_connected_peers")
        .and_then(Value::as_array)
        .is_some_and(|rows| rows
            .iter()
            .filter_map(Value::as_str)
            .any(|row| row.contains(peer_id_b))));

    let peers_b = run_cli_json(&home_b, &["p2p", "peers"])?;
    assert_eq!(peers_b.get("count").and_then(Value::as_u64), Some(1));

    let collections_add = run_cli_json(
        &home_b,
        &["p2p", "collections", "add", "--profile", "chat-requests"],
    )?;
    assert_eq!(
        collections_add.get("status").and_then(Value::as_str),
        Some("collections_added")
    );
    assert!(collections_add
        .get("collections")
        .and_then(Value::as_array)
        .is_some_and(|rows| rows.iter().any(|row| row.as_str() == Some("AgentRequest"))));

    let replicator_add = run_cli_json(
        &home_b,
        &[
            "p2p",
            "replicators",
            "add",
            "--peer",
            peer_addr_a,
            "--profile",
            "chat-requests",
        ],
    )?;
    assert_eq!(
        replicator_add.get("status").and_then(Value::as_str),
        Some("replicator_added")
    );

    let diagnose_b = run_cli_json(&home_b, &["p2p", "diagnose"])?;
    assert_eq!(
        diagnose_b
            .pointer("/checks/p2p/info/ok")
            .and_then(Value::as_bool),
        Some(true)
    );
    for path in [
        "/checks/p2p/info/ok",
        "/checks/p2p/shareable_address/ok",
        "/checks/p2p/peers/ok",
        "/checks/p2p/collections/ok",
        "/checks/p2p/replicators/ok",
        "/checks/p2p/documents/ok",
    ] {
        assert!(
            diagnose_b
                .pointer(path)
                .and_then(Value::as_bool)
                .is_some_and(|ok| ok),
            "expected successful diagnostic at {path}: {diagnose_b}"
        );
    }
    assert!(diagnose_b
        .pointer("/checks/p2p/collections/value")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()));
    assert!(diagnose_b
        .pointer("/checks/p2p/replicators/value")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2p_pair_sets_up_bidirectional_delegation_replication() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_a = tempdir.path().join("parent-agent");
    let home_b = tempdir.path().join("child-agent");
    fs::create_dir_all(&home_a)?;
    fs::create_dir_all(&home_b)?;

    let model_name = format!("mock-p2p-pair-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port_a = allocate_port()?;
    let port_b = allocate_port()?;
    let agent_name_a = format!("cli-parent-{}", Uuid::new_v4().simple());
    let agent_name_b = format!("cli-child-{}", Uuid::new_v4().simple());
    let graphql_a = graphql_url(port_a);
    let graphql_b = graphql_url(port_b);

    let init_a = run_init_json(
        &home_a,
        &[
            "--agent-name",
            &agent_name_a,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let init_b = run_init_json(
        &home_b,
        &[
            "--agent-name",
            &agent_name_b,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did_a = agent_did_from_init(&init_a)?;
    let agent_did_b = agent_did_from_init(&init_b)?;

    let (mut serve_a, readiness_a) = spawn_server_with_ready_json(
        &home_a,
        port_a,
        &[
            "--p2p-bind-addr",
            "127.0.0.1",
            "--p2p-port",
            "0",
            "--p2p-relay-mode",
            "disabled",
            "--p2p-discovery",
            "disabled",
        ],
        &[],
    )?;
    let (mut serve_b, readiness_b) = spawn_server_with_ready_json(
        &home_b,
        port_b,
        &[
            "--p2p-bind-addr",
            "127.0.0.1",
            "--p2p-port",
            "0",
            "--p2p-relay-mode",
            "disabled",
            "--p2p-discovery",
            "disabled",
        ],
        &[],
    )?;
    wait_for_port(port_a, &mut serve_a)?;
    wait_for_port(port_b, &mut serve_b)?;
    wait_for_runtime_ready(&graphql_a, &agent_did_a, Duration::from_secs(30)).await?;
    wait_for_runtime_ready(&graphql_b, &agent_did_b, Duration::from_secs(30)).await?;

    let peer_id_a = readiness_a
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Parent readiness JSON missing p2p_peer_id: {readiness_a}"))?;
    let peer_id_b = readiness_b
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Child readiness JSON missing p2p_peer_id: {readiness_b}"))?;
    let peer_addr_a = readiness_a
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!("Parent readiness JSON missing P2P listen address: {readiness_a}")
        })?;
    let peer_addr_b = readiness_b
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Child readiness JSON missing P2P listen address: {readiness_b}"))?;

    // Pair child → parent (child dials, subscribes collections, installs replicator to parent).
    let pair_b = run_cli_json(
        &home_b,
        &[
            "p2p",
            "pair",
            "--peer",
            peer_addr_a,
            "--profile",
            "chat-requests",
        ],
    )?;
    assert_eq!(
        pair_b.get("status").and_then(Value::as_str),
        Some("paired"),
        "child pair status: {pair_b}"
    );
    assert!(
        pair_b
            .get("collections")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.iter().any(|r| r.as_str() == Some("AgentRequest"))),
        "pair output missing AgentRequest in collections: {pair_b}"
    );
    assert!(
        pair_b.get("note").and_then(Value::as_str).is_some(),
        "pair output missing bidirectional note: {pair_b}"
    );

    // Wait for child to see parent as a connected peer.
    let status_b = wait_for_connected_peer(&home_b, peer_id_a, Duration::from_secs(20)).await?;
    assert!(
        status_b
            .get("p2p_connected_peers")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows
                .iter()
                .filter_map(Value::as_str)
                .any(|row| row.contains(peer_id_a))),
        "child not connected to parent: {status_b}"
    );

    // Pair parent → child (parent dials, subscribes collections, installs replicator to child).
    let pair_a = run_cli_json(
        &home_a,
        &[
            "p2p",
            "pair",
            "--peer",
            peer_addr_b,
            "--profile",
            "chat-requests",
        ],
    )?;
    assert_eq!(
        pair_a.get("status").and_then(Value::as_str),
        Some("paired"),
        "parent pair status: {pair_a}"
    );

    // Wait for parent to see child as a connected peer.
    let status_a = wait_for_connected_peer(&home_a, peer_id_b, Duration::from_secs(20)).await?;
    assert!(
        status_a
            .get("p2p_connected_peers")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows
                .iter()
                .filter_map(Value::as_str)
                .any(|row| row.contains(peer_id_b))),
        "parent not connected to child: {status_a}"
    );

    // Verify both servers have collections subscribed (list returns CIDs; count is sufficient here
    // since pair output already validated the collection names above).
    let collections_b = run_cli_json(&home_b, &["p2p", "collections", "list"])?;
    assert!(
        collections_b
            .get("count")
            .and_then(Value::as_u64)
            .is_some_and(|n| n >= 1),
        "child has no subscribed collections: {collections_b}"
    );

    let collections_a = run_cli_json(&home_a, &["p2p", "collections", "list"])?;
    assert!(
        collections_a
            .get("count")
            .and_then(Value::as_u64)
            .is_some_and(|n| n >= 1),
        "parent has no subscribed collections: {collections_a}"
    );

    // Verify both servers have a replicator installed.
    let replicators_b = run_cli_json(&home_b, &["p2p", "replicators", "list"])?;
    assert!(
        replicators_b
            .get("count")
            .and_then(Value::as_u64)
            .is_some_and(|n| n >= 1),
        "child has no replicators: {replicators_b}"
    );

    let replicators_a = run_cli_json(&home_a, &["p2p", "replicators", "list"])?;
    assert!(
        replicators_a
            .get("count")
            .and_then(Value::as_u64)
            .is_some_and(|n| n >= 1),
        "parent has no replicators: {replicators_a}"
    );

    Ok(())
}
