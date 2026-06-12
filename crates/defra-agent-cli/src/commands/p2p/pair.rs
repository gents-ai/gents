use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::cli::args::P2pPairArgs;
use crate::shared::{P2pReplicatorRequest, P2pReplicatorRow};
use crate::{http_post_json, print_json, resolve_graphql_endpoint, resolve_home_dir};

use super::collections::{expand_p2p_collection_args, p2p_collection_profile_id};
use super::output::{fetch_live_http_p2p_status, flatten_p2p_fields};

/// `p2p pair` sets up one direction of delegation replication in three steps:
///   1. connect to the peer
///   2. subscribe the profile's collections for P2P
///   3. install a push replicator to the peer for those collections
///
/// Replication is directional. Run this command on both servers (each pointing
/// at the other's listen address) to enable bidirectional delegation replication.
pub(super) async fn p2p_pair(args: P2pPairArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("building P2P pair HTTP client")?;
    let api_base = crate::graphql_access::graphql_api_base(&graphql)?;

    // Resolve the collection list from the profile (single profile, no extras).
    let collections = expand_p2p_collection_args(&[], &[args.profile])?;
    let profile = p2p_collection_profile_id(args.profile);

    // Step 1: connect to peer.
    http_post_json(
        &client,
        &format!("{api_base}/p2p/connect"),
        &vec![args.peer.clone()],
    )
    .await
    .context("p2p connect")?;

    // Step 2: subscribe collections.
    http_post_json(
        &client,
        &format!("{api_base}/p2p/collections"),
        &collections,
    )
    .await
    .context("p2p collections add")?;

    // Step 3: install replicator.
    let replicator_request = P2pReplicatorRequest {
        collections: collections.clone(),
        addresses: vec![args.peer.clone()],
    };
    http_post_json(
        &client,
        &format!("{api_base}/p2p/replicators"),
        &replicator_request,
    )
    .await
    .context("p2p replicators add")?;

    // Fetch a live snapshot of the replicators list so the output can report
    // how many replicators are now installed on this server.
    let replicators: Vec<P2pReplicatorRow> =
        crate::http_get_json(&client, &format!("{api_base}/p2p/replicators"))
            .await
            .unwrap_or_default();
    let replicator_count = replicators.len();

    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());

    let mut output = json!({
        "status": "paired",
        "home": home_dir,
        "graphql": graphql,
        "peer": args.peer,
        "profile": profile,
        "collections": collections,
        "replicator_count": replicator_count,
        "note": "Replication is one-directional. Run `p2p pair` on the other server (with --peer set to this server's listen address) to complete bidirectional delegation replication.",
        "p2p": p2p,
    });

    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }

    print_json(&output)?;
    Ok(())
}
