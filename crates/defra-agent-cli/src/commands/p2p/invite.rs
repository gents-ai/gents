use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_agent::{AgentIdentity, KeyIdentity};
use defra_agent_protocol::pairing_token::{encode as encode_invite, signing_payload, InviteToken};
use serde_json::json;

use crate::cli::args::{P2pCollectionProfileArg, P2pInviteArgs};
use crate::{
    http_get_json, normalize_optional_string, print_json, read_init_config, read_runtime_state,
    resolve_agent_did, resolve_graphql_endpoint, resolve_home_dir,
};

use super::collections::p2p_collection_profile_id;
use super::output::resolve_p2p_peer_id;

// Re-export so join.rs can import from one place.
pub(super) use defra_agent_protocol::pairing_token::encode as encode_token;

pub(super) async fn p2p_invite(args: P2pInviteArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    let profiles = profile_ids_or_default(&args.profiles);

    let identity = resolve_home_identity(args.home.as_deref())
        .context("resolving local agent identity for invite signing")?;
    let token = current_invite_token_signed(
        args.home.as_deref(),
        &graphql,
        profiles.clone(),
        identity.as_ref(),
    )
    .await?;
    let encoded = encode_invite(&token)?;

    print_json(&json!({
        "status": "invite_created",
        "home": home_dir,
        "graphql": graphql,
        "token": encoded,
        "peer_id": token.peer_id,
        "issuer_did": token.issuer_did,
        "network_id": token.network_id,
        "profiles": token.profiles,
        "ticket": token.ticket,
        "join_command": format!("defra-agent p2p pairings join {encoded}"),
    }))?;
    Ok(())
}

/// Build a signed v2 invite token for the current node.
pub(super) async fn current_invite_token(
    home: Option<&Path>,
    graphql: &str,
    profiles: Vec<String>,
) -> Result<InviteToken> {
    let identity =
        resolve_home_identity(home).context("resolving local agent identity for invite signing")?;
    current_invite_token_signed(home, graphql, profiles, identity.as_ref()).await
}

async fn current_invite_token_signed(
    home: Option<&Path>,
    graphql: &str,
    profiles: Vec<String>,
    identity: &dyn AgentIdentity,
) -> Result<InviteToken> {
    let home_dir = resolve_home_dir(home);
    let mut token = if let Some(t) = build_persisted_token(&home_dir, graphql, profiles.clone(), identity)? {
        t
    } else {
        build_live_token(home, graphql, profiles, identity).await?
    };

    // Sign: compute payload over token with sig=[] then fill in the signature.
    let payload = signing_payload(&token);
    let sig = identity
        .sign(&payload)
        .await
        .context("signing pairing invite token")?;
    token.sig = sig;
    Ok(token)
}

fn build_persisted_token(
    home_dir: &Path,
    graphql: &str,
    profiles: Vec<String>,
    identity: &dyn AgentIdentity,
) -> Result<Option<InviteToken>> {
    let Some(runtime_state) = read_runtime_state(home_dir)? else {
        return Ok(None);
    };
    if runtime_state.graphql != graphql {
        return Ok(None);
    }
    let Some(peer_id) = normalize_optional_string(runtime_state.p2p_peer_id.as_deref()) else {
        return Ok(None);
    };
    let Some(ticket) = runtime_state
        .p2p_listen_addresses
        .iter()
        .find_map(|address| normalize_optional_string(Some(address.as_str())))
    else {
        return Ok(None);
    };

    Ok(Some(InviteToken {
        v: 2,
        issuer_did: identity.did().to_string(),
        peer_id,
        ticket,
        profiles,
        network_id: "default".to_string(),
        issued_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        sig: Vec::new(), // filled in by caller
    }))
}

async fn build_live_token(
    home: Option<&Path>,
    graphql: &str,
    profiles: Vec<String>,
    identity: &dyn AgentIdentity,
) -> Result<InviteToken> {
    use crate::http::version::{NodeIdentityResponse, P2pShareableAddressResponse};

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("building P2P invite HTTP client")?;
    let api_base = crate::graphql_access::graphql_api_base(graphql)?;
    let node_identity =
        http_get_json::<NodeIdentityResponse>(&client, &format!("{api_base}/node/identity"))
            .await
            .ok();
    let shareable_address: P2pShareableAddressResponse =
        http_get_json(&client, &format!("{api_base}/p2p/shareable-address"))
            .await
            .context("loading shareable P2P address")?;
    let ticket = normalize_optional_string(shareable_address.address.as_deref())
        .context("runtime did not report a shareable P2P address")?;
    let peer_id = resolve_p2p_peer_id(
        node_identity
            .as_ref()
            .and_then(|id| id.peer_id.as_deref()),
        Some(&ticket),
        &[],
        None,
    )
    .context("runtime reported a shareable P2P address but no usable peer id")?;

    // Prefer the identity DID; fall back to resolve_agent_did for forward compat.
    let issuer_did = {
        let id_did = identity.did();
        if id_did.is_empty() {
            resolve_agent_did(home, None).context("resolving local agent DID for invite")?
        } else {
            id_did.to_string()
        }
    };

    Ok(InviteToken {
        v: 2,
        issuer_did,
        peer_id,
        ticket,
        profiles,
        network_id: "default".to_string(),
        issued_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        sig: Vec::new(), // filled in by caller
    })
}

/// Load the local agent identity from the home dir's init config.
///
/// Supports file-key (the common case).  macOS keychain / Secure Enclave
/// identities cannot be signed from an offline CLI sub-command today; those
/// paths surface a clear error.
pub(super) fn resolve_home_identity(home: Option<&Path>) -> Result<Arc<dyn AgentIdentity>> {
    let home_dir = resolve_home_dir(home);
    let Some(config) = read_init_config(&home_dir)? else {
        anyhow::bail!(
            "no init config found in {}; run `defra-agent init` first",
            home_dir.display()
        )
    };

    let backend = config
        .identity_backend
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("file");

    match backend {
        "file" | "" => {
            let key_path = config
                .key_path
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    crate::default_key_path(&home_dir, &config.agent_name)
                });
            let identity = KeyIdentity::load_or_create(&key_path, None)
                .context("loading agent identity key for invite signing")?;
            Ok(Arc::new(identity))
        }
        other => anyhow::bail!(
            "identity backend {other:?} is not supported for offline invite signing; \
             start `defra-agent server` and use `--graphql` to connect"
        ),
    }
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
    use defra_agent_protocol::pairing_token::{decode, encode, TOKEN_PREFIX};

    use super::*;

    fn v2_token() -> InviteToken {
        InviteToken {
            v: 2,
            issuer_did: "did:key:agent-a".to_string(),
            peer_id: "peer-a".to_string(),
            ticket: "/ip4/127.0.0.1/tcp/4001/p2p/peer-a".to_string(),
            profiles: vec!["chat-requests".to_string()],
            network_id: "default".to_string(),
            issued_at: "2026-06-13T00:00:00Z".to_string(),
            sig: vec![0xAB, 0xCD],
        }
    }

    #[test]
    fn invite_token_v2_round_trips() {
        let original = v2_token();
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
    fn invite_token_rejects_v1_token() {
        // Encode a v=1 shaped token; decode must reject with a re-issue hint.
        let old = InviteToken {
            v: 1,
            issuer_did: "did:key:agent-a".to_string(),
            peer_id: "peer-a".to_string(),
            ticket: "/ip4/1".to_string(),
            profiles: vec![],
            network_id: "default".to_string(),
            issued_at: "t".to_string(),
            sig: vec![],
        };
        let encoded = encode(&old).expect("encode v1");
        let err = decode(&encoded).unwrap_err().to_string();
        assert!(
            err.contains("re-issue") || err.contains("newer"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn invite_token_rejects_truncated_base58() {
        let encoded = encode(&v2_token()).expect("encode");
        let truncated = &encoded[..encoded.len() - 4];
        let err = decode(truncated).unwrap_err().to_string();
        assert!(
            err.contains("decoding pairing invite token")
                || err.contains("parsing pairing invite token"),
            "unexpected error: {err}"
        );
    }
}
