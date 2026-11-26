use std::sync::Arc;
use std::time::Duration;

use gents_store::config_client::ConfigAccess;
use gents_store::local::backend_protection::{FreshBackendStore, ProtectedBackendStore};
use gents_store::local::document_acp::DocumentAcp;
use tokio_util::sync::CancellationToken;

use super::{home, stored, Home};

const INTERVAL: Duration = Duration::from_millis(200);
const OBSERVED: Duration = Duration::from_secs(3);

fn intervals() -> gents_p2p::p2p_reconcile::intervals::P2pIntervals {
    gents_p2p::p2p_reconcile::intervals::P2pIntervals::with_overrides(
        Some(INTERVAL),
        Some(INTERVAL),
        Some(INTERVAL),
        Some(INTERVAL * 3),
    )
}

/// Start `home` with the peer-to-peer transport its heartbeats publish for.
async fn start_with_transport(
    home: &Home,
    document_acp: DocumentAcp,
) -> anyhow::Result<Arc<defra_node::Node>> {
    let transport = home
        .data
        .parent()
        .expect("a home holds its data directory")
        .to_path_buf();
    let (node, _admission) = ProtectedBackendStore::open(&home.data, &home.key, &home.lock)
        .await?
        .preflight(&home.key, &home.node, document_acp)
        .await?
        .start_node_with_projection(
            &home.key,
            home.node.clone(),
            move |builder: defra_node::NodeBuilder| {
                builder.with_p2p(gents_p2p::test_support::transport::test_p2p_config(
                    &gents_p2p::test_support::transport::TestP2pAdmission::default(),
                    &transport,
                ))
            },
            Arc::new(crate::surface::ToolCeiling::meta_only()),
            Vec::new(),
            &gents_session::test_support::no_lifecycle_plugins(),
        )
        .await?;
    Ok(node)
}

/// What the runtime's periodic peer publications leave on a home: the
/// `updated_at` of every peer record each time it changes, the directory
/// entries, and how each loop ended once cancelled. The directory projection
/// derives no entry from its source today, so it is observed only for what it
/// writes on a Vera home.
#[derive(Debug)]
struct Published {
    registry_updates: Vec<Vec<String>>,
    directory_entries: usize,
    loops: Vec<Result<(), String>>,
}

async fn publish(document_acp: DocumentAcp) -> anyhow::Result<Published> {
    let directory = tempfile::tempdir()?;
    let home = home(directory.path(), "heartbeats").await?;
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, &home.owner, document_acp)
        .await?;
    let node = start_with_transport(&home, document_acp).await?;
    let node_did = home.node.did().to_owned();
    ConfigAccess::local_as(node.clone(), identity::Did::new(&node_did)?)
        .write_with_variables(
            "test.heartbeats.node",
            "mutation($node: String) { create_Node(input: {node_did: $node}) { _docID } }",
            &serde_json::json!({ "node": node_did }),
        )
        .await?;

    let mut changes = gents_store::subscription_source::subscribe_collections(
        &node,
        ["PeerRegistry", "NodeDirectoryEntry"],
    )?;
    let cancel = CancellationToken::new();
    let loops = [
        tokio::spawn(gents_p2p::p2p_reconcile::run_registry_heartbeat(
            node.clone(),
            node_did.clone(),
            gents_p2p::p2p_reconcile::DEFAULT_NETWORK_ID.to_owned(),
            intervals(),
            cancel.child_token(),
        )),
        tokio::spawn(gents_p2p::p2p_reconcile::run_endpoint_heartbeat(
            node.clone(),
            home.node.clone(),
            None,
            intervals(),
            cancel.child_token(),
        )),
        tokio::spawn(gents_p2p::directory_projection::run_directory_projection(
            node.clone(),
            node_did.clone(),
            intervals(),
            cancel.child_token(),
        )),
    ];

    let mut registry_updates: Vec<Vec<String>> = Vec::new();
    let deadline = tokio::time::Instant::now() + OBSERVED;
    loop {
        let updates = stored(&node, &home.owner, "PeerRegistry", "updated_at").await?;
        if registry_updates.last() != Some(&updates) {
            registry_updates.push(updates);
        }
        if document_acp == DocumentAcp::Local
            && registry_updates
                .iter()
                .filter(|rows| !rows.is_empty())
                .count()
                >= 2
        {
            break;
        }
        match tokio::time::timeout_at(deadline, changes.recv()).await {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    let directory_entries = stored(&node, &home.owner, "NodeDirectoryEntry", "node_did")
        .await?
        .len();
    cancel.cancel();
    let mut ended = Vec::new();
    for running in loops {
        ended.push(running.await?.map_err(|error| format!("{error:#}")));
    }
    node.shutdown().await?;
    Ok(Published {
        registry_updates,
        directory_entries,
        loops: ended,
    })
}

/// Lean `Identity.NodePublication.periodic`: on a home on the node store the
/// heartbeats and the directory publish and keep their records current
/// (`heartbeats_on_the_node_store_beat_as_before`); on a Vera home no
/// periodic publication writes a peer record at all
/// (`no_heartbeat_writes_a_peer_record_on_a_vera_home`), so none is readable
/// and no beat fails updating one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vera_homes_heartbeats_stay_idle_and_a_local_homes_beat_as_before() -> anyhow::Result<()>
{
    let local = publish(DocumentAcp::Local).await?;
    assert!(
        local.registry_updates.iter().any(|rows| !rows.is_empty()),
        "{local:?}"
    );
    assert!(
        local
            .registry_updates
            .iter()
            .filter(|rows| !rows.is_empty())
            .count()
            >= 2,
        "a later beat updates the record the first wrote: {local:?}"
    );
    assert!(local.loops.iter().all(Result::is_ok), "{local:?}");

    let vera = publish(DocumentAcp::Vera).await?;
    assert_eq!(vera.registry_updates, [Vec::<String>::new()], "{vera:?}");
    assert_eq!(vera.directory_entries, 0, "{vera:?}");
    assert!(vera.loops.iter().all(Result::is_ok), "{vera:?}");
    Ok(())
}
