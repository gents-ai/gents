//! Scope-template install behavior + filtered-replication acceptance tests.
//!
//! These cover the template-driven pairing reconciler at the *applied-row*
//! level (`PeerPairingApplied`), proving the install shape each delivery mode
//! produces:
//!
//! - **Push** (`conversation`): a filtered replicator (non-empty
//!   `replicator_addresses` + a recorded `replicator_filter`) and NO subscribed
//!   collections — filtered push with no gossip subscription.
//! - **Replicate** (`agent-config` / `backup`): subscribed collections AND a
//!   replicator with an EMPTY filter — whole-collection subscribe+replicate.
//! - **Filter change**: changing the scoped value (the peer's `agent_did`)
//!   re-resolves the filter and reinstalls the replicator, updating the applied
//!   `replicator_filter`.
//!
//! Tests 1-3 assert on a single node's *local* reconcile result: the `pairings
//! set`/`join` command writes a `PeerPairingDesired` row, and that node's
//! reconciler installs the wiring and records `PeerPairingApplied`. No
//! cross-node document transit is required, which keeps them robust under the
//! parallel server-spawning suite (iroh loopback transit starves under load).
//!
//! Test 4 is the END-TO-END acceptance test for live scoped filtering. It is
//! `#[ignore]`d until the defra.rs #1033 pin bump, because today the
//! defradb-filter translation in `RemoteP2pAdmin::add_replicator` is STUBBED
//! (non-empty filters warn + fall back to an unfiltered install). Enable it as
//! the closeout of that pin bump — do NOT delete it.

mod support;
use support::*;

use std::fs;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

/// Generous timeout for a local reconcile tick to land an applied row. The
/// reconciler sweeps periodically AND on Update events; pairings set triggers a
/// write that wakes it, but we stay generous because the suite runs many
/// server-spawning tests in parallel.
const APPLIED_TIMEOUT: Duration = Duration::from_secs(90);

/// Poll until a `PeerPairingApplied` row exists for `peer_id`, returning the row
/// with `replicator_filter` selected (the shared helper omits it). Times out.
async fn wait_for_applied_with_filter(
    graphql: &str,
    peer_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        match applied_row_with_filter(graphql, peer_id).await {
            Ok(row) => return Ok(row),
            Err(error) => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for PeerPairingApplied({peer_id}): {error}");
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Like `wait_for_applied_with_filter` but waits until the row also carries a
/// replicator. A Replicate-delivery template subscribes collections first and
/// installs the replicator second, so the applied row exists (with collections)
/// before `replicator_addresses` is populated — poll past that window.
async fn wait_for_applied_replicator(
    graphql: &str,
    peer_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(row) = applied_row_with_filter(graphql, peer_id).await {
            if nonempty_replicator(&row) {
                return Ok(row);
            }
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for PeerPairingApplied({peer_id}) to gain a replicator");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn applied_row_with_filter(graphql: &str, peer_id: &str) -> Result<Value> {
    let peer_id = escape_graphql_string(peer_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}, limit: 1) {{
                    peer_id
                    collections
                    replicator_addresses
                    replicator_filter
                }}
            }}"#
        ),
    )
    .await?;
    Ok(first_graphql_row(&response, "PeerPairingApplied")?.clone())
}

/// Poll until the applied row's `replicator_filter` JSON contains `needle`
/// (e.g. a specific scoped DID), or time out. Returns the matching row.
async fn wait_for_applied_filter_contains(
    graphql: &str,
    peer_id: &str,
    needle: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    let mut last_filter = String::from("<none>");
    loop {
        if let Ok(row) = applied_row_with_filter(graphql, peer_id).await {
            let filter = row
                .get("replicator_filter")
                .and_then(Value::as_str)
                .unwrap_or_default();
            last_filter = filter.to_string();
            if filter.contains(needle) {
                return Ok(row);
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for PeerPairingApplied({peer_id}) replicator_filter to contain {needle:?}; last filter={last_filter}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn empty_collections(row: &Value) -> bool {
    match row.get("collections") {
        None | Some(Value::Null) => true,
        Some(Value::Array(rows)) => rows.is_empty(),
        _ => false,
    }
}

fn nonempty_replicator(row: &Value) -> bool {
    row.get("replicator_addresses")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty())
}

/// Spawn a P2P-enabled server node (discovery + relay disabled, ephemeral P2P
/// port) and wait for it to be runtime-ready. Returns the live process handle
/// (kept alive for the test) and its readiness JSON.
async fn spawn_p2p_node(
    home: &std::path::Path,
    port: u16,
    graphql: &str,
    agent_did: &str,
) -> Result<(ServeProcess, Value)> {
    let (mut serve, readiness) = spawn_server_with_ready_json(
        home,
        port,
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
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(graphql, agent_did, Duration::from_secs(30)).await?;
    Ok((serve, readiness))
}

fn listen_address(readiness: &Value) -> Result<String> {
    readiness
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("readiness JSON missing P2P listen address: {readiness}"))
}

fn peer_id_of(readiness: &Value) -> Result<String> {
    readiness
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("readiness JSON missing p2p_peer_id: {readiness}"))
}

/// Test 1 (green now). Pair via the `conversation` (Push) template and assert
/// the local applied row is a *filtered push*: a replicator is installed
/// (replicator_addresses non-empty), the scope filter is recorded
/// (replicator_filter carries the peer's agent_did), and NO collections are
/// subscribed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_push_template_installs_filtered_replicator_no_subscription() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_src = tempdir.path().join("push-src");
    let home_peer = tempdir.path().join("push-peer");
    fs::create_dir_all(&home_src)?;
    fs::create_dir_all(&home_peer)?;

    let model_name = format!("mock-tmpl-push-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port_src = allocate_port()?;
    let port_peer = allocate_port()?;
    let graphql_src = graphql_url(port_src);
    let graphql_peer = graphql_url(port_peer);

    let init_src = run_init_json(
        &home_src,
        &[
            "--agent-name",
            &format!("tmpl-push-src-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let init_peer = run_init_json(
        &home_peer,
        &[
            "--agent-name",
            &format!("tmpl-push-peer-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did_src = agent_did_from_init(&init_src)?;
    let agent_did_peer = agent_did_from_init(&init_peer)?;

    let (_serve_src, readiness_src) =
        spawn_p2p_node(&home_src, port_src, &graphql_src, &agent_did_src).await?;
    let (_serve_peer, _readiness_peer) =
        spawn_p2p_node(&home_peer, port_peer, &graphql_peer, &agent_did_peer).await?;

    let peer_addr_src = listen_address(&readiness_src)?;
    let peer_id_src = peer_id_of(&readiness_src)?;

    // The peer node pairs toward the source via the conversation (Push)
    // template. Its local reconciler installs the filtered replicator and
    // records the applied row — no document transit required.
    let set = run_cli_json(
        &home_peer,
        &[
            "p2p",
            "pairings",
            "set",
            "--did",
            agent_did_src.as_str(),
            "--address",
            peer_addr_src.as_str(),
            // `--collection` is a legacy front-door requirement; the reconciler
            // ignores the row's collections and derives them from the template.
            "--collection",
            "AgentRequest",
            "--template",
            "conversation",
        ],
    )?;
    assert_eq!(
        set.get("status").and_then(Value::as_str),
        Some("pairing_set"),
        "pairings set output: {set}"
    );
    assert_eq!(
        set.get("template").and_then(Value::as_str),
        Some("conversation")
    );

    let applied =
        wait_for_applied_with_filter(&graphql_peer, &peer_id_src, APPLIED_TIMEOUT).await?;
    assert!(
        nonempty_replicator(&applied),
        "Push template must install a replicator: {applied}"
    );
    assert!(
        empty_collections(&applied),
        "Push template must NOT subscribe collections (no gossip): {applied}"
    );
    let filter = applied
        .get("replicator_filter")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Push applied row missing replicator_filter: {applied}"))?;
    assert!(
        filter.contains(agent_did_src.as_str()),
        "Push replicator_filter must record the scoped peer DID {agent_did_src}: {filter}"
    );
    assert!(
        filter.contains("agent_did"),
        "Push replicator_filter must scope on the agent_did field: {filter}"
    );

    Ok(())
}

/// Test 2 (green now). Pair via the `agent-config` (Replicate) template and
/// assert the local applied row is a *whole-collection subscribe+replicate*:
/// collections ARE subscribed AND a replicator is installed with an EMPTY
/// (unfiltered) filter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_config_replicate_template_subscribes_with_empty_filter() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_src = tempdir.path().join("repl-src");
    let home_peer = tempdir.path().join("repl-peer");
    fs::create_dir_all(&home_src)?;
    fs::create_dir_all(&home_peer)?;

    let model_name = format!("mock-tmpl-repl-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port_src = allocate_port()?;
    let port_peer = allocate_port()?;
    let graphql_src = graphql_url(port_src);
    let graphql_peer = graphql_url(port_peer);

    let init_src = run_init_json(
        &home_src,
        &[
            "--agent-name",
            &format!("tmpl-repl-src-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let init_peer = run_init_json(
        &home_peer,
        &[
            "--agent-name",
            &format!("tmpl-repl-peer-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did_src = agent_did_from_init(&init_src)?;
    let agent_did_peer = agent_did_from_init(&init_peer)?;

    let (_serve_src, readiness_src) =
        spawn_p2p_node(&home_src, port_src, &graphql_src, &agent_did_src).await?;
    let (_serve_peer, _readiness_peer) =
        spawn_p2p_node(&home_peer, port_peer, &graphql_peer, &agent_did_peer).await?;

    let peer_addr_src = listen_address(&readiness_src)?;
    let peer_id_src = peer_id_of(&readiness_src)?;

    let set = run_cli_json(
        &home_peer,
        &[
            "p2p",
            "pairings",
            "set",
            "--did",
            agent_did_src.as_str(),
            "--address",
            peer_addr_src.as_str(),
            // `--collection` is a legacy front-door requirement; the reconciler
            // ignores the row's collections and derives them from the template.
            "--collection",
            "AgentRequest",
            "--template",
            "agent-config",
        ],
    )?;
    assert_eq!(
        set.get("status").and_then(Value::as_str),
        Some("pairing_set"),
        "pairings set output: {set}"
    );
    assert_eq!(
        set.get("template").and_then(Value::as_str),
        Some("agent-config")
    );

    let applied =
        wait_for_applied_replicator(&graphql_peer, &peer_id_src, APPLIED_TIMEOUT).await?;
    assert!(
        applied
            .get("collections")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.iter().any(|c| c.as_str() == Some("AgentBehavior"))),
        "Replicate template must subscribe its collection set (AgentBehavior): {applied}"
    );
    assert!(
        nonempty_replicator(&applied),
        "Replicate template must install a replicator: {applied}"
    );
    // EMPTY filter is persisted as a null/absent String column (unfiltered).
    let filter = applied
        .get("replicator_filter")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        filter.is_empty(),
        "Replicate template must install an UNFILTERED replicator (empty filter): {applied}"
    );

    Ok(())
}

/// Test 3 (green now, local). Pair via the `conversation` (Push) template, then
/// change the scoped value (the peer's `agent_did`) so the resolved filter
/// differs. Assert the applied `replicator_filter` updates — the reconciler
/// reinstalls the replicator under the new filter identity. Asserted entirely
/// on one node's applied row; no cross-node transit needed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filter_change_reinstalls_replicator_under_new_scope() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_src = tempdir.path().join("flip-src");
    let home_peer = tempdir.path().join("flip-peer");
    fs::create_dir_all(&home_src)?;
    fs::create_dir_all(&home_peer)?;

    let model_name = format!("mock-tmpl-flip-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port_src = allocate_port()?;
    let port_peer = allocate_port()?;
    let graphql_src = graphql_url(port_src);
    let graphql_peer = graphql_url(port_peer);

    let init_src = run_init_json(
        &home_src,
        &[
            "--agent-name",
            &format!("tmpl-flip-src-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let init_peer = run_init_json(
        &home_peer,
        &[
            "--agent-name",
            &format!("tmpl-flip-peer-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did_src = agent_did_from_init(&init_src)?;
    let agent_did_peer = agent_did_from_init(&init_peer)?;

    let (_serve_src, readiness_src) =
        spawn_p2p_node(&home_src, port_src, &graphql_src, &agent_did_src).await?;
    let (_serve_peer, _readiness_peer) =
        spawn_p2p_node(&home_peer, port_peer, &graphql_peer, &agent_did_peer).await?;

    let peer_addr_src = listen_address(&readiness_src)?;
    let peer_id_src = peer_id_of(&readiness_src)?;

    // First scope: filter on the real source DID. (Use the same --peer on both
    // sets so the desired row is upserted in place and the peer_id is stable.)
    let set_a = run_cli_json(
        &home_peer,
        &[
            "p2p",
            "pairings",
            "set",
            "--peer",
            peer_id_src.as_str(),
            "--did",
            agent_did_src.as_str(),
            "--address",
            peer_addr_src.as_str(),
            // `--collection` is a legacy front-door requirement; the reconciler
            // ignores the row's collections and derives them from the template.
            "--collection",
            "AgentRequest",
            "--template",
            "conversation",
        ],
    )?;
    assert_eq!(
        set_a.get("status").and_then(Value::as_str),
        Some("pairing_set"),
        "first pairings set output: {set_a}"
    );

    let applied_a = wait_for_applied_filter_contains(
        &graphql_peer,
        &peer_id_src,
        agent_did_src.as_str(),
        APPLIED_TIMEOUT,
    )
    .await?;
    let filter_a = applied_a
        .get("replicator_filter")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        filter_a.contains(agent_did_src.as_str()),
        "initial filter must scope on the first DID: {filter_a}"
    );

    // Change the scoped value: re-set the SAME peer with a DIFFERENT DID. The
    // template re-resolves the per-peer filter to the new DID, making the
    // replicator's (address, filter) identity distinct and forcing reinstall.
    let new_did = format!("{agent_did_src}-rescoped");
    let set_b = run_cli_json(
        &home_peer,
        &[
            "p2p",
            "pairings",
            "set",
            "--peer",
            peer_id_src.as_str(),
            "--did",
            new_did.as_str(),
            "--address",
            peer_addr_src.as_str(),
            // `--collection` is a legacy front-door requirement; the reconciler
            // ignores the row's collections and derives them from the template.
            "--collection",
            "AgentRequest",
            "--template",
            "conversation",
        ],
    )?;
    assert_eq!(
        set_b.get("status").and_then(Value::as_str),
        Some("pairing_set"),
        "second pairings set output: {set_b}"
    );

    // The applied filter must update to the new scope. Poll for the new DID to
    // appear in the recorded filter (reinstall under the new identity).
    let applied_b = wait_for_applied_filter_contains(
        &graphql_peer,
        &peer_id_src,
        new_did.as_str(),
        APPLIED_TIMEOUT,
    )
    .await?;
    let filter_b = applied_b
        .get("replicator_filter")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        filter_b.contains(new_did.as_str()),
        "applied filter must update to the rescoped DID: {filter_b}"
    );
    assert_ne!(
        filter_a, filter_b,
        "applied replicator_filter must change after the scope flip"
    );
    // A replicator is still installed (reinstalled, not torn down to nothing).
    assert!(
        nonempty_replicator(&applied_b),
        "replicator must remain installed after reinstall: {applied_b}"
    );

    Ok(())
}

/// Test 4 (BUMP-GATED acceptance test). End-to-end scoped filtering: write docs
/// for two different `agent_did`s on the source node, pair via the
/// `conversation` (Push) template, and assert ONLY the scoped DID's docs appear
/// on the peer.
///
/// IGNORED until the defra.rs #1033 pin bump. Today the defradb-filter
/// translation in `RemoteP2pAdmin::add_replicator` is STUBBED — a non-empty
/// filter warns and falls back to an UNFILTERED install — so live filtering is
/// not yet in effect and this cannot pass. This is the acceptance test for the
/// bump: enable it (remove `#[ignore]`) as the closeout of the #1033 pin bump.
/// Do NOT delete it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "enable at defra.rs #1033 pin bump — filtered replication is live only then"]
async fn end_to_end_scoped_filtering_only_replicates_scoped_did() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_src = tempdir.path().join("e2e-src");
    let home_peer = tempdir.path().join("e2e-peer");
    fs::create_dir_all(&home_src)?;
    fs::create_dir_all(&home_peer)?;

    let model_name = format!("mock-tmpl-e2e-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port_src = allocate_port()?;
    let port_peer = allocate_port()?;
    let graphql_src = graphql_url(port_src);
    let graphql_peer = graphql_url(port_peer);

    let init_src = run_init_json(
        &home_src,
        &[
            "--agent-name",
            &format!("tmpl-e2e-src-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let init_peer = run_init_json(
        &home_peer,
        &[
            "--agent-name",
            &format!("tmpl-e2e-peer-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did_src = agent_did_from_init(&init_src)?;
    let agent_did_peer = agent_did_from_init(&init_peer)?;

    let (_serve_src, _readiness_src) =
        spawn_p2p_node(&home_src, port_src, &graphql_src, &agent_did_src).await?;
    let (_serve_peer, readiness_peer) =
        spawn_p2p_node(&home_peer, port_peer, &graphql_peer, &agent_did_peer).await?;

    // The peer's DID is the scope: conversation Push filters the source's
    // documents down to those tagged with the peer's agent_did.
    let scoped_did = agent_did_peer.clone();
    let other_did = format!("{agent_did_peer}-other");

    // Write one AgentRequest for the scoped DID and one for an unrelated DID on
    // the SOURCE node. Only the scoped one should ever land on the peer.
    let scoped_request_id = format!("scoped-{}", Uuid::new_v4().simple());
    let other_request_id = format!("other-{}", Uuid::new_v4().simple());
    write_agent_request(&graphql_src, &scoped_request_id, &scoped_did).await?;
    write_agent_request(&graphql_src, &other_request_id, &other_did).await?;

    // Source pairs toward the peer via conversation (Push): it installs a
    // filtered replicator pushing only the peer-scoped slice.
    let peer_addr_peer = listen_address(&readiness_peer)?;
    let set = run_cli_json(
        &home_src,
        &[
            "p2p",
            "pairings",
            "set",
            "--did",
            scoped_did.as_str(),
            "--address",
            peer_addr_peer.as_str(),
            // `--collection` is a legacy front-door requirement; the reconciler
            // ignores the row's collections and derives them from the template.
            "--collection",
            "AgentRequest",
            "--template",
            "conversation",
        ],
    )?;
    assert_eq!(
        set.get("status").and_then(Value::as_str),
        Some("pairing_set"),
        "pairings set output: {set}"
    );

    // The scoped doc must replicate to the peer.
    wait_for_agent_request(&graphql_peer, &scoped_request_id, Duration::from_secs(90)).await?;

    // The unscoped doc must NEVER replicate to the peer. Give transit a fair
    // window, then assert absence.
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert!(
        !agent_request_exists(&graphql_peer, &other_request_id).await?,
        "unscoped AgentRequest {other_request_id} leaked to the peer — filtering is not enforced"
    );

    Ok(())
}

/// Write a minimal `AgentRequest` document for `agent_did` on `graphql`.
async fn write_agent_request(graphql: &str, request_id: &str, agent_did: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                session_id: "{request_id}-session",
                content: "scope filtering probe",
                status: "pending",
                created_at: "{now}",
                updated_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        agent_did = escape_graphql_string(agent_did),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

async fn agent_request_exists(graphql: &str, request_id: &str) -> Result<bool> {
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    request_id
                }}
            }}"#,
            escape_graphql_string(request_id),
        ),
    )
    .await?;
    Ok(first_graphql_row(&response, "AgentRequest").is_ok())
}

async fn wait_for_agent_request(graphql: &str, request_id: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if agent_request_exists(graphql, request_id).await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for AgentRequest {request_id} to replicate");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
