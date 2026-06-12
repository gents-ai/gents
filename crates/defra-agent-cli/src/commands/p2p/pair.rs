use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::json;

use crate::cli::args::P2pPairArgs;
use crate::request_helpers::parse_duration_suffix;
use crate::{print_json, resolve_config_access, resolve_graphql_endpoint};

use super::collections::{expand_p2p_collection_args, p2p_collection_profile_id};
use super::pairings::{wait_for_pairing_connected, write_pairing_desired};

/// `p2p pair` is a compatibility shortcut for writing a desired pairing row
/// from a peer address. DID-carrying pairing should use `p2p invite`/`p2p join`.
pub(super) async fn p2p_pair(args: P2pPairArgs) -> Result<()> {
    let peer_id = peer_id_from_addr(&args.peer)?;
    let collections = expand_p2p_collection_args(&[], &[args.profile])?;
    let profile = p2p_collection_profile_id(args.profile).to_string();
    let profiles = vec![profile.clone()];
    let addresses = vec![args.peer.clone()];
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let doc_id = write_pairing_desired(
        &access,
        &peer_id,
        None,
        &collections,
        &addresses,
        &profiles,
        &now,
    )
    .await?;

    let p2p = if args.wait {
        let timeout = parse_duration_suffix(&args.timeout)?;
        Some(wait_for_pairing_connected(args.home.as_deref(), &graphql, &peer_id, timeout).await?)
    } else {
        None
    };

    let mut output = json!({
        "status": "pairing_set",
        "home": home_dir,
        "graphql": graphql,
        "access_mode": access.mode(),
        "peer_id": peer_id,
        "agent_did": serde_json::Value::Null,
        "peer": args.peer,
        "profile": profile,
        "profiles": profiles,
        "collections": collections,
        "replicator_addresses": addresses,
        "doc_id": doc_id,
        "waited": args.wait,
        "note": "Desired pairing written. The running defra-agent runtime applies P2P wiring on its pairing sweep.",
    });
    if let Some(p2p) = p2p {
        output["p2p"] = p2p;
    }
    print_json(&output)?;
    Ok(())
}

fn peer_id_from_addr(peer: &str) -> Result<String> {
    let peer = peer.trim();
    if peer.is_empty() {
        anyhow::bail!("--peer must not be empty");
    }
    p2p::iroh::parse_public_peer_addr(peer)
        .map(|(peer_id, _)| peer_id.to_string())
        .map_err(|error| anyhow::anyhow!("--peer must be a shareable P2P address: {error}"))
        .with_context(|| format!("parsing peer id from {peer}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_rejects_empty_peer() {
        let err = peer_id_from_addr(" ").unwrap_err().to_string();
        assert!(err.contains("--peer must not be empty"));
    }
}
