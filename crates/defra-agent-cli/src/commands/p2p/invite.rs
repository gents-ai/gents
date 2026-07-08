use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_agent::{graphql::escape_graphql_string, AgentIdentity, KeyIdentity};
use defra_agent_protocol::network_token::{MembershipRecord, NetworkRecord};
use defra_agent_protocol::pairing_token::{encode as encode_invite, signing_payload, InviteToken};
use serde_json::json;

use crate::cli::args::P2pInviteArgs;
use crate::config_writes::ConfigAccess;
use crate::{
    http_get_json, normalize_optional_string, print_json, read_init_config, read_runtime_state,
    resolve_agent_did, resolve_config_access, resolve_graphql_endpoint, resolve_home_dir,
};

use super::network_admin::{load_membership_record, load_single_network_record};
use super::output::resolve_p2p_peer_id;
use super::pairings::resolve_pairing_template;

pub(super) async fn p2p_invite(args: P2pInviteArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    let template = resolve_pairing_template(&args.template)?;
    let member_did = args
        .member_did
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("p2p pairings invite requires --member-did for v5 membership-gated invites")?;
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let network = load_single_network_record(&access)
        .await
        .context("loading local AgentNetwork for v5 invite")?;
    let grant = load_membership_record(&access, &network.network_id, member_did)
        .await?
        .with_context(|| format!("no NetworkMembership grant found for {member_did}"))?;
    validate_invite_grant(&network, &grant, member_did)?;

    let identity = resolve_home_identity(args.home.as_deref())
        .context("resolving local agent identity for invite signing")?;
    if identity.did() != network.admin_did {
        anyhow::bail!(
            "local DID {} is not network admin {}; only admin-issued v5 invites are supported",
            identity.did(),
            network.admin_did
        );
    }
    let token = current_invite_token_signed(
        args.home.as_deref(),
        &graphql,
        &template,
        identity.as_ref(),
        grant,
        network,
    )
    .await?;
    record_reciprocal_conversation_intent(&access, member_did, &template).await?;
    let encoded = encode_invite(&token)?;

    print_json(&json!({
        "status": "invite_created",
        "home": home_dir,
        "graphql": graphql,
        "token": encoded,
        "peer_id": token.peer_id,
        // `issuer_did` is the v3 vocabulary; `did` is kept as a backward-compatible
        // alias so existing tooling/scripts that read `.did` keep working.
        "issuer_did": token.issuer_did,
        "did": token.issuer_did,
        "network_id": token.network_id,
        "template": token.template,
        "ticket": token.ticket,
        "join_command": format!("defra-agent p2p pairings join {encoded}"),
    }))?;
    Ok(())
}

async fn current_invite_token_signed(
    home: Option<&Path>,
    graphql: &str,
    template: &str,
    identity: &dyn AgentIdentity,
    grant: MembershipRecord,
    network: NetworkRecord,
) -> Result<InviteToken> {
    let home_dir = resolve_home_dir(home);
    // Prefer the LIVE shareable address: it is the runtime's best-known *dialable*
    // address (NAT/relay-aware), whereas the persisted runtime-state file carries
    // only listen-form addresses, which are not guaranteed dialable under
    // no-relay/no-discovery. An un-dialable invite ticket is a permanent
    // replication-liveness failure, not a slow one — see PairingTransport.tla
    // (the `Dialable = FALSE` counterexample) and the Lean
    // `convergence_requires_successful_install` obligation. The persisted path is
    // an offline fallback used only when the live HTTP endpoint is unreachable.
    let mut token = match build_live_token(home, graphql, template, identity, &grant, &network)
        .await
    {
        Ok(t) => t,
        Err(live_err) => {
            match build_persisted_token(&home_dir, graphql, template, identity, &grant, &network)? {
                Some(t) => t,
                None => return Err(live_err),
            }
        }
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

/// Generate a fresh single-use invite nonce. A v4 UUID is the established
/// random-id primitive in this crate (see `request_helpers`); the join path
/// records it in a consumed-nonce ledger to make a token single-use (Task C2).
fn mint_nonce() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn build_persisted_token(
    home_dir: &Path,
    graphql: &str,
    template: &str,
    identity: &dyn AgentIdentity,
    grant: &MembershipRecord,
    network: &NetworkRecord,
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
    // Offline fallback only (the live shareable address is preferred — see
    // `current_invite_token_signed`). Listen-form addresses are not guaranteed
    // dialable under no-relay/no-discovery; this path runs only when the live
    // HTTP endpoint is unreachable, where no better address is available.
    let Some(ticket) = runtime_state
        .p2p_listen_addresses
        .iter()
        .find_map(|address| normalize_optional_string(Some(address.as_str())))
    else {
        return Ok(None);
    };

    Ok(Some(InviteToken {
        v: 5,
        issuer_did: identity.did().to_string(),
        peer_id,
        ticket,
        nonce: mint_nonce(),
        network_id: network.network_id.clone(),
        issued_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        template: template.to_string(),
        grant: grant.clone(),
        network: network.clone(),
        sig: Vec::new(), // filled in by caller
    }))
}

async fn build_live_token(
    home: Option<&Path>,
    graphql: &str,
    template: &str,
    identity: &dyn AgentIdentity,
    grant: &MembershipRecord,
    network: &NetworkRecord,
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
        node_identity.as_ref().and_then(|id| id.peer_id.as_deref()),
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
        v: 5,
        issuer_did,
        peer_id,
        ticket,
        nonce: mint_nonce(),
        network_id: network.network_id.clone(),
        issued_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        template: template.to_string(),
        grant: grant.clone(),
        network: network.clone(),
        sig: Vec::new(), // filled in by caller
    })
}

async fn record_reciprocal_conversation_intent(
    access: &ConfigAccess,
    member_did: &str,
    template: &str,
) -> Result<()> {
    if template != "conversation" {
        return Ok(());
    }

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mutation = reciprocal_conversation_intent_upsert_mutation(member_did, template, &now);
    access
        .execute(&mutation)
        .await
        .context("recording ReciprocalConversationIntent for conversation invite")?;
    tracing::debug!(
        member_did = %member_did,
        template = %template,
        "recorded reciprocal conversation intent for invite"
    );
    Ok(())
}

fn reciprocal_conversation_intent_upsert_mutation(
    member_did: &str,
    template: &str,
    now: &str,
) -> String {
    let member_did = escape_graphql_string(member_did);
    let template = escape_graphql_string(template);
    let now = escape_graphql_string(now);
    format!(
        r#"mutation {{
            upsert_ReciprocalConversationIntent(
                filter: {{ member_did: {{ _eq: "{member_did}" }} }},
                add: {{
                    member_did: "{member_did}",
                    template: "{template}",
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    template: "{template}",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

fn validate_invite_grant(
    network: &NetworkRecord,
    grant: &MembershipRecord,
    member_did: &str,
) -> Result<()> {
    if grant.network_id != network.network_id {
        anyhow::bail!(
            "NetworkMembership grant is for network {} but AgentNetwork is {}",
            grant.network_id,
            network.network_id
        );
    }
    if grant.member_did != member_did {
        anyhow::bail!(
            "NetworkMembership grant is for {} but invite requested {member_did}",
            grant.member_did
        );
    }
    if grant.status.trim() != "active" {
        anyhow::bail!(
            "NetworkMembership grant for {member_did} is not active (status={})",
            grant.status
        );
    }
    Ok(())
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
                .unwrap_or_else(|| crate::default_key_path(&home_dir, &config.agent_name));
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

#[cfg(test)]
mod tests {
    use defra_agent_protocol::network_token::{MembershipRecord, NetworkRecord};
    use defra_agent_protocol::pairing_token::{decode, encode, TOKEN_PREFIX};

    use super::*;

    fn network_record() -> NetworkRecord {
        NetworkRecord {
            network_id: "default".to_string(),
            admin_did: "did:key:agent-a".to_string(),
            display_name: "Default".to_string(),
            default_template: "network-control".to_string(),
            created_at: "2026-06-13T00:00:00Z".to_string(),
            sig: vec![1, 2, 3],
        }
    }

    fn grant_record() -> MembershipRecord {
        MembershipRecord {
            network_id: "default".to_string(),
            member_did: "did:key:agent-b".to_string(),
            status: "active".to_string(),
            granted_at: "2026-06-13T00:00:00Z".to_string(),
            revoked_at: String::new(),
            sig: vec![4, 5, 6],
        }
    }

    fn v5_token() -> InviteToken {
        InviteToken {
            v: 5,
            issuer_did: "did:key:agent-a".to_string(),
            peer_id: "peer-a".to_string(),
            ticket: "/ip4/127.0.0.1/tcp/4001/p2p/peer-a".to_string(),
            nonce: "nonce-a".to_string(),
            network_id: "default".to_string(),
            issued_at: "2026-06-13T00:00:00Z".to_string(),
            template: "conversation".to_string(),
            grant: grant_record(),
            network: network_record(),
            sig: vec![0xAB, 0xCD],
        }
    }

    #[test]
    fn invite_token_v5_round_trips_with_template_nonce_and_grant() {
        let original = v5_token();
        let encoded = encode(&original).expect("encode");
        assert!(encoded.starts_with(TOKEN_PREFIX));
        let decoded = decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
        assert_eq!(decoded.template, "conversation");
        assert_eq!(decoded.nonce, "nonce-a");
        assert_eq!(decoded.grant.member_did, "did:key:agent-b");
        assert_eq!(decoded.network.admin_did, "did:key:agent-a");
    }

    #[test]
    fn invite_token_rejects_wrong_prefix() {
        let err = decode("wrong-prefix").unwrap_err().to_string();
        assert!(err.contains("invalid pairing invite token prefix"));
    }

    #[test]
    fn invite_token_rejects_v4_token() {
        let mut old = v5_token();
        old.v = 4;
        let encoded = encode(&old).expect("encode v4");
        let err = decode(&encoded).unwrap_err().to_string();
        assert!(
            err.contains("re-issue") || err.contains("newer"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn invite_token_rejects_truncated_base58() {
        let encoded = encode(&v5_token()).expect("encode");
        let truncated = &encoded[..encoded.len() - 4];
        let err = decode(truncated).unwrap_err().to_string();
        assert!(
            err.contains("decoding pairing invite token")
                || err.contains("parsing pairing invite token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn reciprocal_intent_upsert_mutation_escapes_member_template_and_timestamps() {
        let mutation = reciprocal_conversation_intent_upsert_mutation(
            "did:key:phone\"quoted",
            "conversation",
            "2026-07-08T00:00:00Z",
        );

        assert!(mutation.contains("upsert_ReciprocalConversationIntent"));
        assert!(mutation.contains("member_did: { _eq: \"did:key:phone\\\"quoted\" }"));
        assert!(mutation.contains("template: \"conversation\""));
        assert!(mutation.contains("created_at: \"2026-07-08T00:00:00Z\""));
        assert!(mutation.contains("updated_at: \"2026-07-08T00:00:00Z\""));
        assert!(
            !mutation.contains("[]"),
            "mutation must not emit empty GraphQL list literals"
        );
    }

    #[test]
    fn validate_invite_grant_requires_active_matching_member_and_network() {
        let network = network_record();
        let grant = grant_record();
        assert!(validate_invite_grant(&network, &grant, "did:key:agent-b").is_ok());

        let mut wrong_member = grant.clone();
        wrong_member.member_did = "did:key:other".to_string();
        assert!(validate_invite_grant(&network, &wrong_member, "did:key:agent-b").is_err());

        let mut revoked = grant.clone();
        revoked.status = "revoked".to_string();
        assert!(validate_invite_grant(&network, &revoked, "did:key:agent-b").is_err());

        let mut wrong_network = grant;
        wrong_network.network_id = "net-other".to_string();
        assert!(validate_invite_grant(&network, &wrong_network, "did:key:agent-b").is_err());
    }
}
