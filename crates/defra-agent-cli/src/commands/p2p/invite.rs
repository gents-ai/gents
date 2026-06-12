use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::cli::args::{P2pCollectionProfileArg, P2pInviteArgs};
use crate::{print_json, resolve_agent_did, resolve_graphql_endpoint, resolve_home_dir};

use super::collections::p2p_collection_profile_id;
use super::output::fetch_live_http_p2p_status;

/// Versioned pairing-invite envelope. CBOR-encoded, bs58-encoded, prefixed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InviteToken {
    pub(crate) v: u8,
    pub(crate) ticket: String,
    pub(crate) peer_id: String,
    pub(crate) did: String,
    pub(crate) profiles: Vec<String>,
}

pub(crate) const TOKEN_PREFIX: &str = "dapair1-";

pub(crate) fn encode(token: &InviteToken) -> Result<String> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(token, &mut bytes).context("encoding pairing invite token")?;
    Ok(format!(
        "{TOKEN_PREFIX}{}",
        bs58::encode(bytes).into_string()
    ))
}

pub(crate) fn decode(raw: &str) -> Result<InviteToken> {
    let encoded = raw
        .trim()
        .strip_prefix(TOKEN_PREFIX)
        .context("invalid pairing invite token prefix")?;
    let bytes = bs58::decode(encoded)
        .into_vec()
        .context("decoding pairing invite token")?;
    let token: InviteToken =
        ciborium::de::from_reader(Cursor::new(bytes)).context("parsing pairing invite token")?;
    match token.v {
        1 => Ok(token),
        version if version > 1 => {
            anyhow::bail!("pairing invite token version {version} requires a newer defra-agent")
        }
        version => anyhow::bail!("unsupported pairing invite token version {version}"),
    }
}

pub(super) async fn p2p_invite(args: P2pInviteArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let profiles = profile_ids_or_default(&args.profiles);
    let token = current_invite_token(args.home.as_deref(), &graphql, profiles.clone()).await?;
    let encoded = encode(&token)?;
    let home_dir = resolve_home_dir(args.home.as_deref());

    print_json(&json!({
        "status": "invite_created",
        "home": home_dir,
        "graphql": graphql,
        "token": encoded,
        "peer_id": token.peer_id,
        "did": token.did,
        "profiles": token.profiles,
        "ticket": token.ticket,
        "join_command": format!("defra-agent p2p join {encoded}"),
    }))?;
    Ok(())
}

pub(super) async fn current_invite_token(
    home: Option<&Path>,
    graphql: &str,
    profiles: Vec<String>,
) -> Result<InviteToken> {
    let status = fetch_live_http_p2p_status(home, graphql).await?;
    let peer_id = status
        .get("p2p_peer_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("runtime did not report a P2P peer id")?
        .to_string();
    let ticket = status
        .get("p2p_listen_addresses")
        .and_then(serde_json::Value::as_array)
        .and_then(|addresses| addresses.first())
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            status
                .get("p2p_shareable_address")
                .and_then(serde_json::Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("runtime did not report a shareable P2P address")?
        .to_string();
    let did = resolve_agent_did(home, None).context("resolving local agent DID for invite")?;

    Ok(InviteToken {
        v: 1,
        ticket,
        peer_id,
        did,
        profiles,
    })
}

pub(super) fn profile_ids_or_default(profiles: &[P2pCollectionProfileArg]) -> Vec<String> {
    let profiles = if profiles.is_empty() {
        vec![P2pCollectionProfileArg::ChatRequests]
    } else {
        profiles.to_vec()
    };
    profiles
        .into_iter()
        .map(|profile| p2p_collection_profile_id(profile).to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(version: u8) -> InviteToken {
        InviteToken {
            v: version,
            ticket: "/ip4/127.0.0.1/tcp/4001/p2p/peer-a".to_string(),
            peer_id: "peer-a".to_string(),
            did: "did:key:agent-a".to_string(),
            profiles: vec!["chat-requests".to_string()],
        }
    }

    #[test]
    fn invite_token_round_trips() {
        let original = token(1);
        let encoded = encode(&original).expect("encode");
        assert!(encoded.starts_with(TOKEN_PREFIX));
        let decoded = decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn invite_token_rejects_wrong_prefix() {
        let err = decode("wrong-prefix").unwrap_err().to_string();
        assert!(err.contains("invalid pairing invite token prefix"));
    }

    #[test]
    fn invite_token_rejects_newer_version() {
        let encoded = encode(&token(2)).expect("encode");
        let err = decode(&encoded).unwrap_err().to_string();
        assert!(err.contains("requires a newer defra-agent"));
    }

    #[test]
    fn invite_token_rejects_truncated_base58() {
        let encoded = encode(&token(1)).expect("encode");
        let truncated = &encoded[..encoded.len() - 4];
        let err = decode(truncated).unwrap_err().to_string();
        assert!(
            err.contains("decoding pairing invite token")
                || err.contains("parsing pairing invite token")
        );
    }
}
