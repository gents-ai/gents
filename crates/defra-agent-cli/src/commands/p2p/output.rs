use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::cli::args::P2pTransportArg;
use crate::shared::{
    P2pCollectionSubscriptionRow, P2pPeerRow, P2pReplicatorOutputRow, P2pReplicatorRow,
    StoredRuntimeState,
};

use crate::{http_get_json, normalize_optional_string, read_runtime_state, resolve_home_dir};

pub(super) fn p2p_collection_rows(
    collection_ids: &[String],
    collection_names_by_id: &BTreeMap<String, String>,
) -> Vec<P2pCollectionSubscriptionRow> {
    collection_ids
        .iter()
        .map(|id| P2pCollectionSubscriptionRow {
            id: id.clone(),
            name: collection_names_by_id.get(id).cloned(),
        })
        .collect()
}

pub(super) fn p2p_collection_names(
    collection_ids: &[String],
    collection_names_by_id: &BTreeMap<String, String>,
) -> Vec<String> {
    collection_ids
        .iter()
        .filter_map(|id| collection_names_by_id.get(id).cloned())
        .collect()
}

pub(super) fn p2p_replicator_rows(
    rows: Vec<P2pReplicatorRow>,
    collection_names_by_id: &BTreeMap<String, String>,
) -> Vec<P2pReplicatorOutputRow> {
    rows.into_iter()
        .map(|row| {
            let collection_names =
                p2p_collection_names(&row.collection_ids, collection_names_by_id);
            P2pReplicatorOutputRow {
                id: row.id,
                addresses: row.addresses,
                collection_ids: row.collection_ids,
                collection_names,
            }
        })
        .collect()
}

pub(super) async fn load_collection_name_by_id(
    client: &reqwest::Client,
    api_base: &str,
) -> BTreeMap<String, String> {
    let Ok(collections) =
        http_get_json::<Vec<Value>>(client, &format!("{api_base}/collections/versions")).await
    else {
        return BTreeMap::new();
    };

    collections
        .into_iter()
        .filter_map(|row| {
            let id = collection_version_string_field(&row, &["CollectionID", "collection_id"])?;
            let name = collection_version_string_field(&row, &["Name", "name"])?;
            Some((id, name))
        })
        .collect()
}

fn collection_version_string_field(row: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        row.get(*name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

pub(crate) fn flatten_p2p_fields(map: &mut serde_json::Map<String, Value>, p2p: &Value) {
    map.insert(
        "p2p_enabled".to_string(),
        p2p.get("enabled").cloned().unwrap_or(Value::Bool(false)),
    );
    for field in [
        "p2p_transport",
        "p2p_peer_id",
        "p2p_listen_addresses",
        "p2p_shareable_address",
        "p2p_connected_peers",
        "p2p_error",
    ] {
        map.insert(
            field.to_string(),
            p2p.get(field).cloned().unwrap_or(Value::Null),
        );
    }
}

pub(crate) async fn load_live_http_p2p_status(home: Option<&Path>, graphql: &str) -> Value {
    let home_dir = resolve_home_dir(home);
    let runtime_state = read_runtime_state(&home_dir)
        .ok()
        .flatten()
        .filter(|state| state.graphql == graphql);
    match fetch_live_http_p2p_status(home, graphql).await {
        Ok(status) => status,
        Err(error) => {
            let mut status = persisted_p2p_status(runtime_state.as_ref());
            if let Some(map) = status.as_object_mut() {
                map.insert("p2p_error".to_string(), Value::String(error.to_string()));
            }
            status
        }
    }
}

pub(super) async fn fetch_live_http_p2p_status(
    home: Option<&Path>,
    graphql: &str,
) -> Result<Value> {
    use crate::http::version::{NodeIdentityResponse, P2pShareableAddressResponse};
    let home_dir = resolve_home_dir(home);
    let runtime_state = read_runtime_state(&home_dir)?.filter(|state| state.graphql == graphql);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("building P2P status HTTP client")?;
    let api_base = crate::graphql_access::graphql_api_base(graphql)?;
    let identity =
        http_get_json::<NodeIdentityResponse>(&client, &format!("{api_base}/node/identity"))
            .await
            .ok();
    let transport = runtime_state
        .as_ref()
        .map(|state| state.p2p_transport.as_str())
        .filter(|transport| !transport.is_empty())
        .unwrap_or(P2pTransportArg::None.as_str());
    let listen_addresses: Vec<String> =
        http_get_json(&client, &format!("{api_base}/p2p/info")).await?;
    let shareable_address: P2pShareableAddressResponse =
        http_get_json(&client, &format!("{api_base}/p2p/shareable-address")).await?;
    let shareable_address = normalize_optional_string(shareable_address.address.as_deref())
        .context("runtime reported an empty shareable P2P address")?;
    let peer_id = resolve_p2p_peer_id(
        identity
            .as_ref()
            .and_then(|identity| identity.peer_id.as_deref()),
        Some(&shareable_address),
        &listen_addresses,
        runtime_state
            .as_ref()
            .and_then(|state| state.p2p_peer_id.as_deref()),
    )
    .context("runtime reported a shareable P2P address but no usable peer id")?;
    let peer_rows: Vec<P2pPeerRow> =
        http_get_json(&client, &format!("{api_base}/p2p/peers")).await?;
    let connected_peers = peer_rows.into_iter().map(|row| row.id).collect::<Vec<_>>();
    Ok(json!({
        "enabled": true,
        "p2p_transport": if transport == P2pTransportArg::None.as_str() {
            P2pTransportArg::Iroh.as_str()
        } else {
            transport
        },
        "p2p_peer_id": peer_id,
        "p2p_listen_addresses": listen_addresses,
        "p2p_shareable_address": shareable_address,
        "p2p_connected_peers": connected_peers,
        "p2p_error": Value::Null,
    }))
}

pub(super) async fn fetch_connected_peer_ids(graphql: &str) -> Result<Vec<String>> {
    let client = super::p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(graphql)?;
    let peer_rows: Vec<P2pPeerRow> =
        http_get_json(&client, &format!("{api_base}/p2p/peers")).await?;
    Ok(peer_rows.into_iter().map(|row| row.id).collect())
}

pub(crate) fn persisted_p2p_status(runtime_state: Option<&StoredRuntimeState>) -> Value {
    match runtime_state {
        Some(runtime_state) => json!({
            "enabled": runtime_state.p2p_transport != P2pTransportArg::None.as_str(),
            "p2p_transport": runtime_state.p2p_transport,
            "p2p_peer_id": runtime_state.p2p_peer_id,
            "p2p_listen_addresses": runtime_state.p2p_listen_addresses,
            "p2p_shareable_address": Value::Null,
            "p2p_connected_peers": [],
            "p2p_error": Value::Null,
        }),
        None => json!({
            "enabled": false,
            "p2p_transport": P2pTransportArg::None.as_str(),
            "p2p_peer_id": Value::Null,
            "p2p_listen_addresses": [],
            "p2p_shareable_address": Value::Null,
            "p2p_connected_peers": [],
            "p2p_error": Value::Null,
        }),
    }
}

fn peer_id_from_public_addr(value: &str) -> Option<String> {
    use p2p::iroh::parse_public_peer_addr;
    let value = normalize_optional_string(Some(value))?;
    parse_public_peer_addr(&value)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string())
}

pub(super) fn resolve_p2p_peer_id(
    live_peer_id: Option<&str>,
    shareable_address: Option<&str>,
    listen_addresses: &[String],
    stored_peer_id: Option<&str>,
) -> Option<String> {
    normalize_optional_string(live_peer_id)
        .or_else(|| shareable_address.and_then(peer_id_from_public_addr))
        .or_else(|| {
            listen_addresses
                .iter()
                .find_map(|addr| peer_id_from_public_addr(addr))
        })
        .or_else(|| normalize_optional_string(stored_peer_id))
}
