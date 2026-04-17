use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::cli::args::{P2pAccessArgs, P2pConnectArgs};
use crate::{http_post_json, print_json, resolve_graphql_endpoint, resolve_home_dir};

use super::output::{fetch_live_http_p2p_status, flatten_p2p_fields, load_live_http_p2p_status};
use super::{p2p_api_base, p2p_http_client, p2p_probe_get};

pub(super) async fn p2p_connect(args: P2pConnectArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("building P2P connect HTTP client")?;
    let api_base = p2p_api_base(&graphql)?;
    http_post_json(
        &client,
        &format!("{api_base}/p2p/connect"),
        &vec![args.peer.clone()],
    )
    .await?;
    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    let mut output = json!({
        "status": "connect_requested",
        "home": home_dir,
        "graphql": graphql,
        "peer": args.peer,
        "p2p": p2p,
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}

pub(super) async fn p2p_diagnose(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let p2p = load_live_http_p2p_status(args.home.as_deref(), &graphql).await;
    let checks = json!({
        "info": p2p_probe_get(&client, &format!("{api_base}/p2p/info")).await,
        "shareable_address": p2p_probe_get(&client, &format!("{api_base}/p2p/shareable-address")).await,
        "peers": p2p_probe_get(&client, &format!("{api_base}/p2p/peers")).await,
        "collections": p2p_probe_get(&client, &format!("{api_base}/p2p/collections")).await,
        "replicators": p2p_probe_get(&client, &format!("{api_base}/p2p/replicators")).await,
        "documents": p2p_probe_get(&client, &format!("{api_base}/p2p/documents")).await,
    });
    let ok = checks.as_object().is_some_and(|map| {
        map.values()
            .all(|value| value.get("ok") == Some(&Value::Bool(true)))
    });
    let home_dir = resolve_home_dir(args.home.as_deref());
    let mut output = json!({
        "status": if ok { "ok" } else { "degraded" },
        "home": home_dir,
        "graphql": graphql,
        "p2p": p2p,
        "checks": {
            "p2p": checks
        }
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}
