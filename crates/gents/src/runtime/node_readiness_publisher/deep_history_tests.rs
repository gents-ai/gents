use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use gents_protocol::node_readiness::{
    AgentReadinessEntry, AgentReadinessState, NodeReadinessProcessState, NodeReadinessSnapshot,
    NODE_READINESS_FORMAT_VERSION,
};

use crate::defra_node::{EmbeddedNode, P2PConfig};
use crate::eval::runner::embedded::EmbeddedHome;
use crate::store::graphql::{
    decode_composite_commits, escape_graphql_string, graphql_with_transaction_retry,
    CompositeCommit,
};

use super::upsert_node_readiness;

fn loopback_config(path: &std::path::Path) -> P2PConfig {
    P2PConfig {
        port: 0,
        bind_addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        relay_mode: p2p::iroh::IrohRelayModeConfig::Disabled,
        discovery: p2p::iroh::IrohDiscoveryConfig::Disabled,
        allowlist: p2p::iroh::IrohAllowlistConfig::AcceptAll,
        max_concurrent_multipath_paths: None,
        secret_key_path: Some(path.join("p2p.key")),
        load_persisted_collections: true,
        max_concurrent_dag_fetches: p2p::sync::DEFAULT_MAX_CONCURRENT_DAG_FETCHES,
        max_concurrent_push_tasks: p2p::sync::DEFAULT_MAX_CONCURRENT_PUSH_TASKS,
        max_doc_sync_request_doc_ids: p2p::sync::DEFAULT_MAX_DOC_SYNC_REQUEST_DOC_IDS,
        // This qualifies history depth, independently of the peer intake-rate budget.
        rate_limit_burst: 100_000,
        rate_limit_rate: 10_000.0,
        max_pending_dags: p2p::sync::DEFAULT_MAX_PENDING_DAGS,
        rebroadcast_on_merge: false,
    }
}

async fn fresh_peer() -> Result<EmbeddedHome> {
    EmbeddedHome::in_tempdir(tempfile::tempdir()?, Some(Arc::new(loopback_config))).await
}

async fn loopback_address(node: &EmbeddedNode) -> Result<String> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let addresses = node
                .p2p()
                .context("P2P enabled")?
                .listen_addresses()
                .await?;
            if let Some(address) = addresses
                .into_iter()
                .find(|address| address.starts_with("127.0.0.1:"))
            {
                return Ok(address);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("loopback P2P listener")?
}

async fn readiness_row(node: &EmbeddedNode, did: &str) -> Result<Option<serde_json::Value>> {
    let did = escape_graphql_string(did);
    let response = graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ NodeReadiness(filter: {{ node_did: {{ _eq: "{did}" }} }}) {{
                _docID snapshot_json
            }} }}"#
        ),
        "deep_readiness_history",
    )
    .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data["NodeReadiness"].as_array())
        .context("NodeReadiness rows")?;
    ensure!(rows.len() <= 1, "readiness identity must remain unique");
    Ok(rows.first().cloned())
}

async fn await_snapshot(
    node: &EmbeddedNode,
    did: &str,
    doc_id: &str,
    expected: &NodeReadinessSnapshot,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if let Some(row) = readiness_row(node, did).await? {
                ensure!(
                    row["_docID"] == doc_id,
                    "readiness document identity changed"
                );
                let snapshot: NodeReadinessSnapshot =
                    serde_json::from_str(row["snapshot_json"].as_str().context("snapshot JSON")?)?;
                if snapshot == *expected {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("fresh peer did not converge on the complete readiness snapshot")?
}

async fn readiness_history(node: &EmbeddedNode, doc_id: &str) -> Result<Vec<CompositeCommit>> {
    let doc_id = escape_graphql_string(doc_id);
    let response = graphql_with_transaction_retry(
        node,
        &format!(r#"{{ _commits(docID: "{doc_id}") {{ cid height fieldName }} }}"#),
        "deep_readiness_history_depth",
    )
    .await?;
    Ok(decode_composite_commits(
        response
            .data
            .as_ref()
            .and_then(|data| data["_commits"].as_array())
            .context("readiness commits")?,
    )?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "native P2P qualification with 8,200 durable readiness writes"]
async fn fresh_peer_receives_readiness_past_8192_revisions_and_keeps_following() -> Result<()> {
    let source = fresh_peer().await?;
    let mut snapshot = NodeReadinessSnapshot {
        format_version: NODE_READINESS_FORMAT_VERSION,
        process_state: NodeReadinessProcessState::Ready,
        active_generation: 1,
        router_generation: 0,
        default_agent_id: "default".into(),
        agents: vec![AgentReadinessEntry {
            agent_id: "default".into(),
            state: AgentReadinessState::Ready,
            reason: None,
        }],
    };
    for generation in 1..=8_200 {
        snapshot.router_generation = generation;
        upsert_node_readiness(
            &source.node,
            source.did(),
            &snapshot,
            "2026-10-28T00:00:00Z",
        )
        .await
        .with_context(|| format!("commit readiness generation {generation}"))?;
    }
    let row = readiness_row(&source.node, source.did())
        .await?
        .context("source readiness")?;
    let doc_id = row["_docID"].as_str().context("source document ID")?;
    let history = readiness_history(&source.node, doc_id).await?;
    ensure!(history.len() == 8_200, "fixture revisions were collapsed");
    let height = history
        .first()
        .map(|commit| commit.height)
        .context("source composite height")?;
    ensure!(height > 8_192, "fixture history is too shallow: {height}");

    let receiver = fresh_peer().await?;
    ensure!(readiness_row(&receiver.node, source.did()).await?.is_none());
    let source_address = loopback_address(&source.node).await?;
    let receiver_address = loopback_address(&receiver.node).await?;
    let collections = vec!["NodeReadiness".to_owned()];
    receiver
        .node
        .p2p()
        .context("receiver P2P")?
        .add_collections(collections.clone())
        .await?;
    receiver
        .node
        .p2p()
        .context("receiver P2P")?
        .add_replicator(
            collections.clone(),
            Some(&source_address),
            Default::default(),
            Vec::new(),
            None,
        )
        .await?;
    source
        .node
        .p2p()
        .context("source P2P")?
        .add_replicator(
            collections,
            Some(&receiver_address),
            Default::default(),
            Vec::new(),
            None,
        )
        .await?;
    await_snapshot(&receiver.node, source.did(), doc_id, &snapshot).await?;
    ensure!(
        readiness_history(&receiver.node, doc_id).await? == history,
        "fresh peer must retain every original signed document version"
    );

    snapshot.router_generation += 1;
    snapshot.process_state = NodeReadinessProcessState::Recovering;
    upsert_node_readiness(
        &source.node,
        source.did(),
        &snapshot,
        "2026-10-28T00:00:01Z",
    )
    .await?;
    await_snapshot(&receiver.node, source.did(), doc_id, &snapshot).await?;
    receiver.node.shutdown().await;
    source.node.shutdown().await;
    Ok(())
}
