mod support;
use support::*;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use gents::defra_node::{EmbeddedNode, StorageBackend};
use gents::{load_tool_selection, subagent_target_entry, upsert_tool_selection, KeyIdentity};
use serde_json::{json, Value};
use uuid::Uuid;

const P2P_LOOPBACK_ARGS: &[&str] = &[
    "--p2p-bind-addr",
    "127.0.0.1",
    "--p2p-port",
    "0",
    "--p2p-relay-mode",
    "disabled",
    "--p2p-discovery",
    "disabled",
];

struct Node {
    home: std::path::PathBuf,
    graphql: String,
    agent_did: String,
    peer_id: String,
    #[allow(dead_code)]
    serve: ServeProcess,
}

async fn boot_node(
    tempdir: &Path,
    label: &str,
    model_name: &str,
    model_endpoint: &str,
    auto_pair: bool,
) -> Result<Node> {
    let home = tempdir.join(label);
    fs::create_dir_all(&home)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-{label}-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            model_name,
            model_endpoint,
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;

    let envs: Vec<(&str, &str)> = if auto_pair {
        vec![("GENTS_DISCOVERY_AUTO_PAIR", "1")]
    } else {
        Vec::new()
    };
    let (mut serve, readiness) =
        spawn_server_with_ready_json(&home, port, P2P_LOOPBACK_ARGS, &envs)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let peer_id = readiness
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{label} readiness JSON missing p2p_peer_id: {readiness}"))?
        .to_string();
    Ok(Node {
        home,
        graphql,
        agent_did,
        peer_id,
        serve,
    })
}

fn network_create(node: &Node, name: &str) -> Result<String> {
    let out = run_cli_json(
        &node.home,
        &[
            "p2p", "network", "create", "--name", name, "--output", "json",
        ],
    )?;
    assert_eq!(
        out.get("status").and_then(Value::as_str),
        Some("network_created"),
        "network create output: {out}"
    );
    out.get("network_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("network create missing network_id: {out}"))
}

fn network_grant(admin: &Node, member_did: &str) -> Result<Value> {
    let out = run_cli_json(
        &admin.home,
        &["p2p", "network", "grant", member_did, "--output", "json"],
    )?;
    assert_eq!(
        out.get("status").and_then(Value::as_str),
        Some("membership_granted"),
        "network grant output: {out}"
    );
    Ok(out)
}

fn mint_invite(node: &Node, member_did: &str) -> Result<String> {
    let invite = run_cli_json(
        &node.home,
        &["p2p", "pairings", "invite", "--member-did", member_did],
    )?;
    assert_eq!(
        invite.get("status").and_then(Value::as_str),
        Some("invite_created"),
        "invite output: {invite}"
    );
    invite
        .get("token")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("invite missing token: {invite}"))
}

fn network_register(node: &Node, template: &str) -> Result<Value> {
    let out = run_cli_json(
        &node.home,
        &["p2p", "network", "register", "--template", template],
    )?;
    assert_eq!(
        out.get("status").and_then(Value::as_str),
        Some("registered"),
        "network register output: {out}"
    );
    Ok(out)
}

fn join(node: &Node, token: &str) -> Result<Value> {
    let out = run_cli_json(&node.home, &["p2p", "pairings", "join", token])?;
    let status = out.get("status").and_then(Value::as_str);
    anyhow::ensure!(
        matches!(status, Some("pairing_joined") | Some("pairing_exists")),
        "unexpected join status: {out}"
    );
    Ok(out)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_create_is_singleton_and_writes_admin_membership() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let model_name = format!("mock-net-create-{}", Uuid::new_v4().simple());
    let mock = MockModelEndpoint::start(&model_name)?;
    let admin = boot_node(tempdir.path(), "admin", &model_name, mock.endpoint(), false).await?;

    let created = run_cli_json(
        &admin.home,
        &[
            "p2p",
            "network",
            "create",
            "--name",
            "Fleet One",
            "--output",
            "json",
        ],
    )?;
    let network_id = created
        .get("network_id")
        .and_then(Value::as_str)
        .context("network create output missing network_id")?;
    assert!(network_id.starts_with("net-"), "output: {created}");
    assert_eq!(
        created.get("admin_did").and_then(Value::as_str),
        Some(admin.agent_did.as_str()),
        "network create output: {created}"
    );
    assert!(
        created
            .get("pointer")
            .and_then(Value::as_str)
            .is_some_and(|token| token.starts_with("danet1-")),
        "network create must emit a danet1 pointer: {created}"
    );

    let network = graphql_query(
        &admin.graphql,
        r#"{ AgentNetwork { network_id admin_did display_name default_template admin_sig } }"#,
    )
    .await?;
    let networks = network
        .pointer("/data/AgentNetwork")
        .and_then(Value::as_array)
        .context("AgentNetwork query missing rows")?;
    assert_eq!(networks.len(), 1, "AgentNetwork rows: {network}");
    assert_eq!(networks[0]["network_id"], json!(network_id));
    assert_eq!(networks[0]["admin_did"], json!(admin.agent_did));
    assert_eq!(networks[0]["display_name"], json!("Fleet One"));
    assert_eq!(networks[0]["default_template"], json!("network-control"));
    assert!(
        networks[0]
            .get("admin_sig")
            .and_then(Value::as_str)
            .is_some_and(|sig| !sig.is_empty()),
        "AgentNetwork must carry admin_sig: {network}"
    );

    let memberships = graphql_query(
        &admin.graphql,
        r#"{ NetworkMembership { network_id member_did status admin_sig } }"#,
    )
    .await?;
    let membership_rows = memberships
        .pointer("/data/NetworkMembership")
        .and_then(Value::as_array)
        .context("NetworkMembership query missing rows")?;
    assert!(
        membership_rows.iter().any(|row| {
            row.get("network_id") == Some(&json!(network_id))
                && row.get("member_did") == Some(&json!(admin.agent_did))
                && row.get("status") == Some(&json!("active"))
                && row
                    .get("admin_sig")
                    .and_then(Value::as_str)
                    .is_some_and(|sig| !sig.is_empty())
        }),
        "admin self-membership missing: {memberships}"
    );

    let endpoints = graphql_query(
        &admin.graphql,
        r#"{ PeerEndpoint { did node_id address binding_sig } }"#,
    )
    .await?;
    let endpoint_rows = endpoints
        .pointer("/data/PeerEndpoint")
        .and_then(Value::as_array)
        .context("PeerEndpoint query missing rows")?;
    assert!(
        endpoint_rows.iter().any(|row| {
            row.get("did") == Some(&json!(admin.agent_did))
                && row.get("node_id") == Some(&json!(admin.peer_id))
                && row
                    .get("address")
                    .and_then(Value::as_str)
                    .is_some_and(|addr| !addr.is_empty())
                && row
                    .get("binding_sig")
                    .and_then(Value::as_str)
                    .is_some_and(|sig| !sig.is_empty())
        }),
        "PeerEndpoint row missing: {endpoints}"
    );

    let second = run_cli_failure_stderr(
        &admin.home,
        &[
            "p2p",
            "network",
            "create",
            "--name",
            "Fleet Two",
            "--output",
            "json",
        ],
    )?;
    assert!(
        second.contains("already exists") && second.contains("singleton"),
        "second create should fail singleton guard, got: {second}"
    );

    drop((admin, mock));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grant_then_revoke_writes_active_then_tombstone() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let model_name = format!("mock-net-grant-{}", Uuid::new_v4().simple());
    let mock = MockModelEndpoint::start(&model_name)?;
    let admin = boot_node(tempdir.path(), "admin", &model_name, mock.endpoint(), false).await?;

    let created = run_cli_json(
        &admin.home,
        &[
            "p2p",
            "network",
            "create",
            "--name",
            "Fleet One",
            "--output",
            "json",
        ],
    )?;
    let network_id = created
        .get("network_id")
        .and_then(Value::as_str)
        .context("network create output missing network_id")?
        .to_string();
    let member = format!("did:key:zMember{}", Uuid::new_v4().simple());

    let grant = run_cli_json(
        &admin.home,
        &["p2p", "network", "grant", &member, "--output", "json"],
    )?;
    assert_eq!(
        grant.get("status").and_then(Value::as_str),
        Some("membership_granted"),
        "grant output: {grant}"
    );
    let member_escaped = escape_graphql_string(&member);
    let after_grant = graphql_query(
        &admin.graphql,
        &format!(
            r#"{{
                NetworkMembership(filter: {{ member_did: {{ _eq: "{member_escaped}" }} }}) {{
                    network_id
                    member_did
                    status
                    granted_at
                    revoked_at
                    admin_sig
                }}
            }}"#
        ),
    )
    .await?;
    let rows = after_grant
        .pointer("/data/NetworkMembership")
        .and_then(Value::as_array)
        .context("NetworkMembership query missing rows after grant")?;
    assert_eq!(rows.len(), 1, "grant rows: {after_grant}");
    assert_eq!(rows[0]["network_id"], json!(network_id));
    assert_eq!(rows[0]["member_did"], json!(member));
    assert_eq!(rows[0]["status"], json!("active"));
    assert_eq!(rows[0]["revoked_at"], json!(""));
    assert!(
        rows[0]
            .get("admin_sig")
            .and_then(Value::as_str)
            .is_some_and(|sig| !sig.is_empty()),
        "grant must carry admin_sig: {after_grant}"
    );

    let revoke = run_cli_json(
        &admin.home,
        &["p2p", "network", "revoke", &member, "--output", "json"],
    )?;
    assert_eq!(
        revoke.get("status").and_then(Value::as_str),
        Some("membership_revoked"),
        "revoke output: {revoke}"
    );
    let after_revoke = graphql_query(
        &admin.graphql,
        &format!(
            r#"{{
                NetworkMembership(filter: {{ member_did: {{ _eq: "{member_escaped}" }} }}) {{
                    network_id
                    member_did
                    status
                    granted_at
                    revoked_at
                    admin_sig
                }}
            }}"#
        ),
    )
    .await?;
    let rows = after_revoke
        .pointer("/data/NetworkMembership")
        .and_then(Value::as_array)
        .context("NetworkMembership query missing rows after revoke")?;
    assert_eq!(
        rows.len(),
        1,
        "revoke must retain one tombstone row: {after_revoke}"
    );
    assert_eq!(rows[0]["network_id"], json!(network_id));
    assert_eq!(rows[0]["member_did"], json!(member));
    assert_eq!(rows[0]["status"], json!("revoked"));
    assert!(
        rows[0]
            .get("revoked_at")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "revoke tombstone must keep revoked_at: {after_revoke}"
    );
    assert!(
        rows[0]
            .get("admin_sig")
            .and_then(Value::as_str)
            .is_some_and(|sig| !sig.is_empty()),
        "revoke tombstone must carry admin_sig: {after_revoke}"
    );

    drop((admin, mock));
    Ok(())
}

fn pair_via_signed_invite(joiner: &Node, token: &str) -> Result<()> {
    let joined = join(joiner, token)?;
    anyhow::ensure!(
        joined.get("reciprocal_token").is_none(),
        "v5 join unexpectedly emitted reciprocal token: {joined}"
    );
    Ok(())
}

async fn desired_source(graphql: &str, peer_id: &str) -> Result<Option<String>> {
    let escaped = escape_graphql_string(peer_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{ PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ peer_id source }} }}"#
        ),
    )
    .await?;
    Ok(response
        .pointer("/data/PeerPairingDesired")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("source"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

async fn wait_for_desired_source(
    graphql: &str,
    peer_id: &str,
    expected_source: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if desired_source(graphql, peer_id).await? == Some(expected_source.to_string()) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for PeerPairingDesired({peer_id}) source={expected_source} on {graphql}; saw source={:?}",
                desired_source(graphql, peer_id).await.unwrap_or(None)
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_desired_source_gone(
    graphql: &str,
    peer_id: &str,
    source: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if desired_source(graphql, peer_id).await? != Some(source.to_string()) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for PeerPairingDesired({peer_id}) source={source} to be retracted on {graphql}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Transitive discovery + auto-pair, proven over a real signed P2P pairing plus
/// the real discovery reconciler.
///
/// A joins the seed S's *signed* `discovery` invite — a real bidirectional P2P
/// pairing that reaches `PeerPairingApplied` (replicator + subscription
/// installed). A's row for S is `operator`-owned (the join wrote it). A then
/// learns of a further member, B — exactly what S forwarding B's `PeerRegistry`
/// row would deliver — and, with auto-pair ON, A's running discovery reconciler
/// materializes a `source="registry"` `PeerPairingDesired` row for B **without A
/// ever inviting or joining B**. That auto-materialization is the headline
/// transitive-discovery behavior, asserted against A's real discovery reconciler.
/// Meanwhile A's operator-owned row for S is never converted or touched.
///
/// Scope notes (faithful but bounded — see the R7 report):
/// - The signed-join authorization path and pairing application are real P2P
///   between two live runtimes.
/// - B's `PeerRegistry` row is written onto A directly rather than transited
///   through a third live runtime. DefraDB gates replication on document
///   ownership (a node cannot push a row signed by another DID), and three
///   concurrent runtimes proved too connect/replication-timing-sensitive for a
///   zero-flake gate. A's discovery reconciler reads B's row identically
///   regardless of how it arrived, so the auto-pair property is proven; only the
///   physical multi-hop transit of B's row is stubbed.
/// - The materialized registry-owned row IS asserted to carry the collections
///   expanded from B's offered profile and B's advertised address (R8), so the
///   pairing reconciler has something concrete to subscribe/replicate. Document
///   flow over the auto-wired A<->B pairing is still not asserted (that needs a
///   third live runtime and is out of scope for this zero-flake gate).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_transitive_discovery_auto_pairs_unseen_peer() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let model_name = format!("mock-net-discovery-{}", Uuid::new_v4().simple());
    let mock = MockModelEndpoint::start(&model_name)?;

    let seed = boot_node(tempdir.path(), "seed", &model_name, mock.endpoint(), false).await?;
    let node_a = boot_node(tempdir.path(), "alpha", &model_name, mock.endpoint(), true).await?;
    network_create(&seed, "Discovery Fleet")?;
    network_grant(&seed, &node_a.agent_did)?;

    network_register(&seed, "conversation")?;
    network_register(&node_a, "conversation")?;
    let listed = run_cli_json(
        &node_a.home,
        &["p2p", "network", "list", "--output", "json"],
    )?;
    assert_eq!(listed.get("status").and_then(Value::as_str), Some("ok"));
    assert!(
        listed
            .get("peers")
            .and_then(Value::as_array)
            .is_some_and(|peers| {
                peers.iter().any(|p| {
                    p.get("peer_id").and_then(Value::as_str) == Some(node_a.peer_id.as_str())
                })
            }),
        "network list must show A's own registered row: {listed}"
    );

    let seed_invite = mint_invite(&seed, &node_a.agent_did)?;
    pair_via_signed_invite(&node_a, &seed_invite)?;
    wait_for_pairing_applied(&node_a.graphql, &seed.peer_id, Duration::from_secs(120)).await?;

    let peer_b = format!("12D3KooBravo{}", Uuid::new_v4().simple());
    let did_b = format!("did:key:zBravo{}", Uuid::new_v4().simple());
    upsert_named_registry_peer(&node_a.graphql, &peer_b, &did_b).await?;

    wait_for_desired_source(
        &node_a.graphql,
        &peer_b,
        "registry",
        Duration::from_secs(90),
    )
    .await?;

    let (collections, addresses) = desired_payload(&node_a.graphql, &peer_b)
        .await?
        .context("materialized registry-owned row for B must exist")?;
    assert!(
        collections.contains(&"AgentRequest".to_string()),
        "registry-owned row for B must carry the chat-requests collections (incl. AgentRequest); saw {collections:?}"
    );
    assert!(
        addresses.contains(&synthetic_registry_address(&peer_b)),
        "registry-owned row for B must carry B's advertised address; saw {addresses:?}"
    );

    assert_eq!(
        desired_source(&node_a.graphql, &seed.peer_id).await?,
        Some("operator".to_string()),
        "A's row for the seed must stay operator-owned"
    );

    drop((seed, node_a, mock));
    Ok(())
}

fn synthetic_registry_address(peer_id: &str) -> String {
    format!("/ip4/127.0.0.1/tcp/6001/p2p/{peer_id}")
}

async fn upsert_named_registry_peer(graphql: &str, peer_id: &str, agent_did: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let address = synthetic_registry_address(peer_id);
    let mutation = format!(
        r#"mutation {{
            create_PeerRegistry(input: {{
                peer_id: "{peer_id}",
                agent_did: "{agent_did}",
                addresses: ["{address}"],
                templates: ["conversation"],
                status: "online",
                network_id: "default",
                registered_at: "{now}",
                updated_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        peer_id = escape_graphql_string(peer_id),
        agent_did = escape_graphql_string(agent_did),
        address = escape_graphql_string(&address),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

async fn desired_payload(
    graphql: &str,
    peer_id: &str,
) -> Result<Option<(Vec<String>, Vec<String>)>> {
    let escaped = escape_graphql_string(peer_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{ PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ peer_id collections replicator_addresses }} }}"#
        ),
    )
    .await?;
    let Some(row) = response
        .pointer("/data/PeerPairingDesired")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
    else {
        return Ok(None);
    };
    let as_strings = |field: &str| -> Vec<String> {
        row.get(field)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(Some((
        as_strings("collections"),
        as_strings("replicator_addresses"),
    )))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_signed_invite_authorization() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let model_name = format!("mock-net-auth-{}", Uuid::new_v4().simple());
    let mock = MockModelEndpoint::start(&model_name)?;

    let seed = boot_node(tempdir.path(), "seed", &model_name, mock.endpoint(), false).await?;
    let joiner = boot_node(
        tempdir.path(),
        "joiner",
        &model_name,
        mock.endpoint(),
        false,
    )
    .await?;
    let outsider = boot_node(
        tempdir.path(),
        "outsider",
        &model_name,
        mock.endpoint(),
        false,
    )
    .await?;
    network_create(&seed, "Authorization Fleet")?;
    network_grant(&seed, &joiner.agent_did)?;
    network_grant(&seed, &outsider.agent_did)?;

    let valid_token = mint_invite(&seed, &joiner.agent_did)?;
    let tampered = tamper_token(&valid_token)?;
    let stderr = run_cli_failure_stderr(&joiner.home, &["p2p", "pairings", "join", &tampered])?;
    assert!(
        stderr.contains("signature invalid") || stderr.contains("signature verification"),
        "tampered join should be rejected at the signature boundary, got: {stderr}"
    );

    let wrong_member_token = mint_invite(&seed, &outsider.agent_did)?;
    let stderr = run_cli_failure_stderr(
        &joiner.home,
        &["p2p", "pairings", "join", &wrong_member_token],
    )?;
    assert!(
        stderr.contains("grant is for"),
        "wrong-member join should be rejected with a membership-grant error, got: {stderr}"
    );

    let bootstrap = join(&joiner, &valid_token)?;
    assert_eq!(
        bootstrap.get("agent_did").and_then(Value::as_str),
        Some(seed.agent_did.as_str()),
        "join must record the issuer DID as agent_did: {bootstrap}"
    );
    let row = peer_pairing_row(&joiner.graphql, &seed.peer_id).await?;
    assert_eq!(
        row.get("agent_did").and_then(Value::as_str),
        Some(seed.agent_did.as_str()),
        "desired row agent_did/invited_by must equal the issuer DID: {row}"
    );

    drop((seed, joiner, outsider, mock));
    Ok(())
}

fn tamper_token(token: &str) -> Result<String> {
    use gents_protocol::pairing_token::{decode, encode};
    let mut decoded = decode(token).context("decoding valid token to tamper its signature")?;
    if decoded.sig.is_empty() {
        bail!("token has no signature to tamper");
    }
    let last = decoded.sig.len() - 1;
    decoded.sig[last] ^= 0x01;
    encode(&decoded).context("re-encoding tampered token")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_cross_deployment_delegation_is_gated() -> Result<()> {
    let refused = run_cross_deployment_dispatch(false).await?;
    assert_eq!(
        refused.lifecycle_state.as_deref(),
        Some("failed"),
        "with the gate off the spawn must fail: {refused:?}"
    );
    let result: Value = serde_json::from_str(
        refused
            .result
            .as_deref()
            .context("refused tool call missing result JSON")?,
    )?;
    assert_eq!(
        result["failure_class"], "tool_not_allowed",
        "refused dispatch must be tool_not_allowed: {result}"
    );
    assert_eq!(result["service_id"], "subagent");

    let admitted = run_cross_deployment_dispatch(true).await?;
    assert_ne!(
        admitted.lifecycle_state.as_deref(),
        Some("failed"),
        "with the gate on the cross-deployment denial must be lifted: {admitted:?}"
    );

    Ok(())
}

#[derive(Debug, Default)]
struct ToolCallObservation {
    lifecycle_state: Option<String>,
    result: Option<String>,
}

async fn run_cross_deployment_dispatch(allow: bool) -> Result<ToolCallObservation> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("parent");
    fs::create_dir_all(&home)?;

    let model_name = format!("mock-xdep-{}-{}", allow, Uuid::new_v4().simple());
    let target_prompt = format!("delegate to peer {}", Uuid::new_v4().simple());
    let remote_did = format!("did:key:zRemotePeer{}", Uuid::new_v4().simple());
    let mock = support::mocks::fake_llm::FakeLlm::start(&model_name, None, {
        let target_prompt = target_prompt.clone();
        std::sync::Arc::new(move |request: &Value| {
            use support::mocks::fake_llm::ChatAction;
            if support::mocks::request_contains_role_text(request, "user", &target_prompt)
                && !support::mocks::request_has_tool_result_message(request)
            {
                let args = json!({
                    "name": "peer-researcher",
                    "prompt": "do the remote work",
                    "await_mode": "background"
                })
                .to_string();
                ChatAction::Sse(support::mocks::tool_call_sse("spawn_subagent", &args))
            } else {
                ChatAction::Hang
            }
        })
    })?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home,
        &[
            "--agent-name",
            &format!("cli-xdep-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            mock.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let key_path = init
        .get("key_path")
        .and_then(Value::as_str)
        .context("init output missing key_path")?;
    let _signing_identity = KeyIdentity::load_or_create(key_path, None)
        .with_context(|| format!("loading cross-deployment signing key {key_path}"))?;
    let tool_selection_id = init
        .get("tool_selection_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init missing tool_selection_id: {init}"))?
        .to_string();

    configure_remote_subagent_target(
        &home,
        &tool_selection_id,
        "peer-researcher",
        &remote_did,
        "remote-research-behavior",
        allow,
        &agent_did,
    )
    .await?;

    let mut serve = spawn_server(&home, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let submit = run_cli_json(
        &home,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--content",
            &target_prompt,
            "--no-wait",
        ],
    )?;
    let session_id = submit
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("submit missing session_id: {submit}"))?
        .to_string();

    let observation = if allow {
        wait_for_spawn_tool_call_past_gate(&graphql, &session_id, Duration::from_secs(30)).await?
    } else {
        wait_for_failed_spawn_tool_call(&graphql, &session_id, Duration::from_secs(30)).await?
    };

    drop((serve, mock));
    Ok(observation)
}

async fn configure_remote_subagent_target(
    home: &Path,
    selection_id: &str,
    target_name: &str,
    target_did: &str,
    target_behavior_id: &str,
    allow: bool,
    agent_did: &str,
) -> Result<()> {
    let data_dir = home.join(".gents").join("data");
    let node = EmbeddedNode::builder()
        .data_path(&data_dir)
        .with_storage_backend(StorageBackend::RocksDb)
        .with_node_identity_did(agent_did)
        .build()
        .await
        .with_context(|| format!("opening embedded node at {}", data_dir.display()))?;
    let mut selection = load_tool_selection(&node, selection_id)
        .await?
        .ok_or_else(|| anyhow!("ToolSelection {selection_id} not found"))?;
    selection.subagent_targets = Some(vec![subagent_target_entry(
        target_name,
        target_did,
        target_behavior_id,
        None,
    )]);
    selection.subagent_spawn_enabled = Some(true);
    selection.subagent_background_enabled = Some(true);
    selection.subagent_allow_cross_deployment = Some(allow);
    upsert_tool_selection(&node, &selection)
        .await
        .context("configuring cross-deployment subagent target")?;
    Ok(())
}

async fn fetch_spawn_tool_call(
    graphql: &str,
    session_id: &str,
) -> Result<Option<ToolCallObservation>> {
    let escaped = escape_graphql_string(session_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                AgentToolCall(
                    filter: {{ session_id: {{ _eq: "{escaped}" }}, tool_name: {{ _eq: "spawn_subagent" }} }},
                    order: {{ started_at: DESC }},
                    limit: 1
                ) {{ lifecycle_state result }}
            }}"#
        ),
    )
    .await?;
    Ok(response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .map(|row| ToolCallObservation {
            lifecycle_state: row
                .get("lifecycle_state")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            result: row
                .get("result")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        }))
}

async fn wait_for_failed_spawn_tool_call(
    graphql: &str,
    session_id: &str,
    timeout: Duration,
) -> Result<ToolCallObservation> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(obs) = fetch_spawn_tool_call(graphql, session_id).await? {
            if obs.lifecycle_state.as_deref() == Some("failed") {
                return Ok(obs);
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for a failed spawn_subagent tool call in session {session_id}"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_spawn_tool_call_past_gate(
    graphql: &str,
    session_id: &str,
    timeout: Duration,
) -> Result<ToolCallObservation> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(obs) = fetch_spawn_tool_call(graphql, session_id).await? {
            match obs.lifecycle_state.as_deref() {
                Some("running") => return Ok(obs),
                Some("failed") => {
                    let denied = obs
                        .result
                        .as_deref()
                        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                        .map(|v| v["failure_class"] == "tool_not_allowed")
                        .unwrap_or(false);
                    if !denied {
                        return Ok(obs);
                    }
                }
                Some(_) => return Ok(obs),
                None => {}
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for the spawn_subagent tool call to pass the cross-deployment gate in session {session_id}"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_ownership_retracts_only_registry_rows() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let model_name = format!("mock-net-ownership-{}", Uuid::new_v4().simple());
    let mock = MockModelEndpoint::start(&model_name)?;

    let node = boot_node(tempdir.path(), "owner", &model_name, mock.endpoint(), true).await?;

    let operator_peer = "operator-only-peer";
    let operator_set = run_cli_json(
        &node.home,
        &[
            "p2p",
            "pairings",
            "set",
            "--peer",
            operator_peer,
            "--did",
            "did:key:zOperatorPeer",
            "--address",
            "/ip4/127.0.0.1/tcp/4999/p2p/operator-only-peer",
        ],
    )?;
    assert_eq!(
        operator_set.get("status").and_then(Value::as_str),
        Some("pairing_set"),
        "operator pairings set output: {operator_set}"
    );
    assert_eq!(
        desired_source(&node.graphql, operator_peer).await?,
        Some("operator".to_string())
    );

    let discovered_peer = "discovered-registry-peer";
    upsert_synthetic_registry_peer(&node.graphql, discovered_peer, true).await?;
    wait_for_desired_source(
        &node.graphql,
        discovered_peer,
        "registry",
        Duration::from_secs(30),
    )
    .await?;

    delete_registry_peer(&node.graphql, discovered_peer).await?;
    wait_for_desired_source_gone(
        &node.graphql,
        discovered_peer,
        "registry",
        Duration::from_secs(30),
    )
    .await?;

    assert_eq!(
        desired_source(&node.graphql, operator_peer).await?,
        Some("operator".to_string()),
        "operator-owned pairing must survive registry retraction"
    );

    drop((node, mock));
    Ok(())
}

async fn upsert_synthetic_registry_peer(graphql: &str, peer_id: &str, live: bool) -> Result<()> {
    let ts = if live {
        chrono::Utc::now()
    } else {
        chrono::Utc::now() - chrono::Duration::seconds(600)
    }
    .to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_PeerRegistry(input: {{
                peer_id: "{peer_id}",
                agent_did: "did:key:zDiscovered{peer_id}",
                addresses: ["/ip4/127.0.0.1/tcp/5001/p2p/{peer_id}"],
                templates: ["conversation"],
                status: "online",
                network_id: "default",
                registered_at: "{ts}",
                updated_at: "{ts}"
            }}) {{ _docID }}
        }}"#,
        peer_id = escape_graphql_string(peer_id),
        ts = escape_graphql_string(&ts),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

async fn delete_registry_peer(graphql: &str, peer_id: &str) -> Result<()> {
    let escaped = escape_graphql_string(peer_id);
    graphql_query(
        graphql,
        &format!(
            r#"mutation {{ delete_PeerRegistry(filter: {{ peer_id: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
        ),
    )
    .await?;
    Ok(())
}
