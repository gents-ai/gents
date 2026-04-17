use anyhow::Result;
use serde_json::{json, Value};

use crate::cli::args::P2pAccessArgs;
use crate::{print_json, resolve_graphql_endpoint, resolve_home_dir};

use super::output::{fetch_live_http_p2p_status, flatten_p2p_fields};

pub(super) async fn p2p_status(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    let mut output = json!({
        "home": home_dir,
        "graphql": graphql,
        "p2p": p2p,
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}

pub(super) async fn p2p_peers(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let peers = p2p
        .get("p2p_connected_peers")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let count = peers.as_array().map(|rows| rows.len()).unwrap_or(0);
    let home_dir = resolve_home_dir(args.home.as_deref());
    let mut output = json!({
        "home": home_dir,
        "graphql": graphql,
        "p2p": p2p,
        "peers": peers,
        "count": count,
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}
