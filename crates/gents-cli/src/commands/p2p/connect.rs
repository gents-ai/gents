use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::cli::args::{P2pAccessArgs, P2pConnectArgs};
use crate::print_json;
use gents_host::http_client::http_send;
use gents_service::home_state::resolve_graphql_endpoint;
use gents_service::home_state::resolve_home_dir;

use super::p2p_probe_get;
use gents_host::p2p_status::fetch_live_http_p2p_status;
use gents_host::p2p_status::flatten_p2p_fields;
use gents_host::p2p_status::load_live_http_p2p_status;

pub(super) async fn p2p_connect(args: P2pConnectArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client =
        gents_host::p2p_status::p2p_http_client().context("building P2P connect HTTP client")?;
    let api_base = crate::graphql_access::graphql_api_base(graphql.url())?;
    http_send(
        graphql.authorize(
            client
                .post(format!("{api_base}/p2p/connect"))
                .json(&vec![args.peer.clone()]),
        )?,
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
    let api_base = crate::graphql_access::graphql_api_base(graphql.url())?;
    let p2p = load_live_http_p2p_status(args.home.as_deref(), &graphql).await;
    let checks = json!({
        "info": p2p_probe_get(&graphql, &format!("{api_base}/p2p/info")).await,
        "shareable_address": p2p_probe_get(&graphql, &format!("{api_base}/p2p/shareable-address")).await,
        "peers": p2p_probe_get(&graphql, &format!("{api_base}/p2p/active-peers")).await,
        "collections": p2p_probe_get(&graphql, &format!("{api_base}/p2p/collections")).await,
        "replicators": p2p_probe_get(&graphql, &format!("{api_base}/p2p/replicators")).await,
        "documents": p2p_probe_get(&graphql, &format!("{api_base}/p2p/documents")).await,
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
