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
    assert_eq!(row.get("connected").and_then(Value::as_bool), Some(false));
    assert_eq!(row.get("subscribed").and_then(Value::as_bool), Some(false));
    assert_eq!(row.get("replicating").and_then(Value::as_bool), Some(false));

    let table = run_cli_text(&home, &["p2p", "pairings", "list", "--output", "table"])?;
    assert!(table.contains("PEER"));
    assert!(table.contains("DID"));
    assert!(table.contains("PROFILES"));
    assert!(table.contains("CONNECTED"));
    assert!(table.contains("SUBSCRIBED"));
    assert!(table.contains("REPLICATING"));

    let remove = run_cli_json(&home, &["p2p", "pairings", "unpair", "--peer", "peer-one"])?;
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

    let connect = run_cli_json(&home_b, &["p2p", "admin", "connect", "--peer", peer_addr_a])?;
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
        &[
            "p2p",
            "admin",
            "collections",
            "add",
            "--profile",
            "chat-requests",
        ],
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
            "admin",
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
async fn p2p_pairings_set_writes_desired_row_for_runtime_reconcile() -> Result<()> {
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
    let _peer_id_b = readiness_b
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
    // Pair child -> parent by writing PeerPairingDesired with `pairings set`.
    // --peer is omitted to exercise peer-id derivation from the shareable
    // --address. The command never mutates live P2P state; the runtime
    // reconciler consumes the row on its sweep.
    let pair_b = run_cli_json(
        &home_b,
        &[
            "p2p",
            "pairings",
            "set",
            "--did",
            agent_did_a.as_str(),
            "--address",
            peer_addr_a,
            "--profile",
            "chat-requests",
        ],
    )?;
    assert_eq!(
        pair_b.get("status").and_then(Value::as_str),
        Some("pairing_set"),
        "child pairings set status: {pair_b}"
    );
    assert_eq!(
        pair_b.get("peer_id").and_then(Value::as_str),
        Some(peer_id_a),
        "peer id should be derived from the shareable address: {pair_b}"
    );
    assert_eq!(
        pair_b.get("agent_did").and_then(Value::as_str),
        Some(agent_did_a.as_str())
    );
    assert!(
        pair_b
            .get("collections")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.iter().any(|r| r.as_str() == Some("AgentRequest"))),
        "pairings set output missing AgentRequest in collections: {pair_b}"
    );
    assert!(
        pair_b.get("note").and_then(Value::as_str).is_some(),
        "pairings set output missing runtime reconcile note: {pair_b}"
    );

    let row = peer_pairing_row(&graphql_b, peer_id_a).await?;
    assert_eq!(
        row.get("agent_did").and_then(Value::as_str),
        Some(agent_did_a.as_str())
    );
    assert!(row
        .get("replicator_addresses")
        .and_then(Value::as_array)
        .is_some_and(|addresses| addresses
            .iter()
            .any(|address| address.as_str() == Some(peer_addr_a))));
    assert!(row
        .get("profiles")
        .and_then(Value::as_array)
        .is_some_and(|profiles| profiles
            .iter()
            .any(|profile| profile.as_str() == Some("chat-requests"))));

    let list = run_cli_json(&home_b, &["p2p", "pairings", "list"])?;
    assert_eq!(list.get("count").and_then(Value::as_u64), Some(1));
    let table = run_cli_text(&home_b, &["p2p", "pairings", "list", "--output", "table"])?;
    assert!(table.contains("PEER"));
    assert!(table.contains("CONNECTED"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2p_invite_join_round_trips_pairing_rows() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_a = tempdir.path().join("invite-a");
    let home_b = tempdir.path().join("invite-b");
    fs::create_dir_all(&home_a)?;
    fs::create_dir_all(&home_b)?;

    let model_name = format!("mock-p2p-invite-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port_a = allocate_port()?;
    let port_b = allocate_port()?;
    let agent_name_a = format!("cli-invite-a-{}", Uuid::new_v4().simple());
    let agent_name_b = format!("cli-invite-b-{}", Uuid::new_v4().simple());
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
        .ok_or_else(|| anyhow!("A readiness JSON missing p2p_peer_id: {readiness_a}"))?;
    let peer_id_b = readiness_b
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("B readiness JSON missing p2p_peer_id: {readiness_b}"))?;

    let invite_a = run_cli_json(
        &home_a,
        &["p2p", "pairings", "invite", "--profile", "chat-requests"],
    )?;
    assert_eq!(
        invite_a.get("status").and_then(Value::as_str),
        Some("invite_created")
    );
    assert_eq!(
        invite_a.get("peer_id").and_then(Value::as_str),
        Some(peer_id_a)
    );
    assert_eq!(
        invite_a.get("did").and_then(Value::as_str),
        Some(agent_did_a.as_str())
    );
    let token_a = invite_a
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("invite A missing token: {invite_a}"))?;

    let join_b = run_cli_json(&home_b, &["p2p", "pairings", "join", token_a])?;
    assert_eq!(
        join_b.get("status").and_then(Value::as_str),
        Some("pairing_joined"),
        "join B output: {join_b}"
    );
    assert_eq!(
        join_b.get("peer_id").and_then(Value::as_str),
        Some(peer_id_a)
    );
    assert_eq!(
        join_b.get("agent_did").and_then(Value::as_str),
        Some(agent_did_a.as_str())
    );
    let reciprocal = join_b
        .get("reciprocal_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("join B missing reciprocal token: {join_b}"))?;

    let join_a = run_cli_json(&home_a, &["p2p", "pairings", "join", reciprocal])?;
    assert_eq!(
        join_a.get("status").and_then(Value::as_str),
        Some("pairing_joined"),
        "join A output: {join_a}"
    );
    assert_eq!(
        join_a.get("peer_id").and_then(Value::as_str),
        Some(peer_id_b)
    );
    assert_eq!(
        join_a.get("agent_did").and_then(Value::as_str),
        Some(agent_did_b.as_str())
    );

    let row_b = peer_pairing_row(&graphql_b, peer_id_a).await?;
    assert_eq!(
        row_b.get("agent_did").and_then(Value::as_str),
        Some(agent_did_a.as_str())
    );
    let row_a = peer_pairing_row(&graphql_a, peer_id_b).await?;
    assert_eq!(
        row_a.get("agent_did").and_then(Value::as_str),
        Some(agent_did_b.as_str())
    );

    let applied_b =
        wait_for_pairing_applied(&graphql_b, peer_id_a, Duration::from_secs(70)).await?;
    assert!(
        applied_b
            .get("collections")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.iter().any(|row| row.as_str() == Some("AgentRequest"))),
        "B applied row missing AgentRequest after joining A: {applied_b}"
    );
    let applied_a =
        wait_for_pairing_applied(&graphql_a, peer_id_b, Duration::from_secs(70)).await?;
    assert!(
        applied_a
            .get("collections")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.iter().any(|row| row.as_str() == Some("AgentRequest"))),
        "A applied row missing AgentRequest after joining B: {applied_a}"
    );

    Ok(())
}

async fn peer_pairing_row(graphql: &str, peer_id: &str) -> Result<Value> {
    let peer_id = escape_graphql_string(peer_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}, limit: 1) {{
                    peer_id
                    agent_did
                    collections
                    replicator_addresses
                    profiles
                }}
            }}"#
        ),
    )
    .await?;
    Ok(first_graphql_row(&response, "PeerPairingDesired")?.clone())
}

async fn wait_for_pairing_applied(
    graphql: &str,
    peer_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_error = None;
    loop {
        if std::time::Instant::now() >= deadline {
            let detail = last_error
                .map(|error: anyhow::Error| error.to_string())
                .unwrap_or_else(|| "no row observed".to_string());
            anyhow::bail!("timed out waiting for PeerPairingApplied({peer_id}): {detail}");
        }
        match peer_pairing_applied_row(graphql, peer_id).await {
            Ok(row) => return Ok(row),
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn peer_pairing_applied_row(graphql: &str, peer_id: &str) -> Result<Value> {
    let peer_id = escape_graphql_string(peer_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}, limit: 1) {{
                    peer_id
                    collections
                    replicator_addresses
                }}
            }}"#
        ),
    )
    .await?;
    Ok(first_graphql_row(&response, "PeerPairingApplied")?.clone())
}
