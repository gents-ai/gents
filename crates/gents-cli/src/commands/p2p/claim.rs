//! Claimant side of the scan-one-QR bearer pairing flow (issue #666).
//!
//! `p2p pairings claim <dabear1-token>` runs on the claiming node:
//! 1. verify the issuer's signature over the token (TOFU) and its freshness;
//! 2. pin the token-carried signed network root locally;
//! 3. write the local `PeerPairingDesired` row so the pairing reconciler dials
//!    the issuer and installs the outbound push replicator (same as `join`);
//! 4. write a self-signed `PairingBearerClaim` row and install a small
//!    unfiltered replicator pushing it to the issuer.
//!
//! Everything authoritative happens on the ISSUER when the claim replicates
//! in: its bearer-claim reconciler re-verifies both signatures, burns the
//! nonce (single-use across devices), authors the membership grant, and — for
//! conversation invites — records the reciprocal conversation intent so the
//! reverse conversation edge materializes once this node's signed
//! `PeerEndpoint` (published by the daemon's endpoint heartbeat) replicates.

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use gents::graphql::escape_graphql_string;
use gents_protocol::bearer_token::{
    bearer_signing_payload, check_bearer_freshness, decode_bearer, BearerClaimRecord,
    BearerInviteToken,
};
use gents_protocol::network_token::NetworkRecord;
use serde_json::json;

use crate::cli::args::P2pClaimArgs;
use crate::shared::P2pReplicatorRequest;
use crate::{
    http_get_json, http_post_json, normalize_optional_string, print_json, resolve_config_access,
    resolve_graphql_endpoint,
};

use super::invite::resolve_home_identity;
use super::network_admin::{load_optional_network_record, write_agent_network};
use super::output::resolve_p2p_peer_id;
use super::p2p_http_client;
use super::pairings::{peer_pairing_exists, resolve_pairing_template, write_pairing_desired};

pub(super) async fn p2p_claim(args: P2pClaimArgs) -> Result<()> {
    let token = decode_bearer(&args.token)?;

    // Verify the issuer's signature over the token payload (TOFU bootstrap arm,
    // same shape as `join`). The issuer re-verifies authoritatively at claim
    // processing; this check keeps a tampered QR from wiring anything locally.
    let identity = resolve_home_identity(args.home.as_deref())
        .context("resolving local agent identity for bearer claim signing")?;
    let payload = bearer_signing_payload(&token);
    let valid = identity
        .verify(&token.issuer_did, &payload, &token.sig)
        .await
        .with_context(|| {
            format!(
                "verifying bearer invite signature for issuer {}",
                token.issuer_did
            )
        })?;
    if !valid {
        anyhow::bail!(
            "bearer invite signature invalid for issuer {}",
            token.issuer_did
        );
    }

    // Courtesy freshness gate: a stale bearer token can never be admitted by
    // the issuer, so fail fast here instead of wiring a doomed pairing.
    check_bearer_freshness(&token, Utc::now())
        .context("bearer invite failed the freshness check (re-mint the QR)")?;

    // Defense-in-depth on the token's network root (mint already enforces
    // both, but a claimant must not trust mint): the issuer must BE the
    // network admin, and the root must verify under that admin DID — a
    // tampered token must not pin a forged root locally.
    check_token_network_authority(&token)?;
    let root_valid = identity
        .verify(
            &token.network.admin_did,
            &token.network.signing_payload(),
            &token.network.sig,
        )
        .await
        .context("verifying bearer invite network root signature")?;
    if !root_valid {
        anyhow::bail!(
            "bearer invite network root signature invalid for admin {}",
            token.network.admin_did
        );
    }

    let template = resolve_pairing_template(&token.template)?;
    let collections = super::join::template_collections(&template);
    let addresses = vec![token.ticket.clone()];
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;

    // Same local-network match gate as v5 join: a node already bound to a
    // different network (or a different admin for the same id) must refuse the
    // claim BEFORE any durable write — `write_agent_network` upserts and would
    // otherwise overwrite the local root.
    let local_network = load_optional_network_record(&access)
        .await
        .context("loading local AgentNetwork before claim")?;
    check_local_network_match(local_network.as_ref(), &token)?;

    // Pin the token-carried signed network root locally (TOFU context for
    // later network-derived discovery). No membership is written here — the
    // issuer authors it at claim time; that is the whole point of bearer mode.
    write_agent_network(&access, &token.network).await?;

    let existed = peer_pairing_exists(&access, &token.peer_id).await?;
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let doc_id = write_pairing_desired(
        &access,
        &token.peer_id,
        Some(&token.issuer_did),
        &collections,
        &addresses,
        &template,
        &now,
    )
    .await?;

    // Best-effort self transport info for the claim row (informational; the
    // authoritative dialable address is the signed PeerEndpoint the daemon's
    // endpoint heartbeat publishes).
    let (claimant_node_id, claimant_address) = local_transport_info(&graphql).await;

    let mut record = BearerClaimRecord {
        token: args.token.trim().to_string(),
        claimant_did: identity.did().to_string(),
        claimant_node_id,
        claimant_address,
        claimed_at: now.clone(),
        sig: Vec::new(),
    };
    record.sig = identity
        .sign(&record.signing_payload())
        .await
        .context("signing bearer claim record")?;
    let claim_mutation = bearer_claim_create_mutation(&record);
    access
        .execute(&claim_mutation)
        .await
        .context("writing local PairingBearerClaim row")?;

    // Push the claim to the issuer: a small unfiltered replicator for the
    // claim collection toward the token's ticket. Requires the local daemon
    // (the same daemon whose endpoint heartbeat makes the reverse edge
    // possible), so fail with a pointed message rather than leaving a claim
    // that never travels.
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(&graphql)?;
    let request = P2pReplicatorRequest {
        collections: vec!["PairingBearerClaim".to_string()],
        addresses: vec![token.ticket.clone()],
        filters: Default::default(),
    };
    http_post_json(&client, &format!("{api_base}/p2p/replicators"), &request)
        .await
        .context(
            "installing the claim push replicator (is the local `gents serve` daemon running?)",
        )?;

    print_json(&json!({
        "status": if existed { "claim_submitted_pairing_exists" } else { "claim_submitted" },
        "home": home_dir,
        "graphql": graphql,
        "access_mode": access.mode(),
        "peer_id": token.peer_id,
        "issuer_did": token.issuer_did,
        "network_id": token.network_id,
        "claimant_did": identity.did(),
        "template": template,
        "collections": collections,
        "replicator_addresses": addresses,
        "doc_id": doc_id,
        "note": "the issuer daemon authors the membership grant when this claim replicates in",
    }))?;
    Ok(())
}

/// Best-effort local (node id, shareable address) from the running daemon.
/// Either may be empty when the daemon or discovery has not produced one yet.
async fn local_transport_info(graphql: &str) -> (String, String) {
    use crate::http::version::{NodeIdentityResponse, P2pShareableAddressResponse};

    let Ok(client) = p2p_http_client() else {
        return (String::new(), String::new());
    };
    let Ok(api_base) = crate::graphql_access::graphql_api_base(graphql) else {
        return (String::new(), String::new());
    };
    let node_identity =
        http_get_json::<NodeIdentityResponse>(&client, &format!("{api_base}/node/identity"))
            .await
            .ok();
    let address = http_get_json::<P2pShareableAddressResponse>(
        &client,
        &format!("{api_base}/p2p/shareable-address"),
    )
    .await
    .ok()
    .and_then(|response| normalize_optional_string(response.address.as_deref()))
    .unwrap_or_default();
    let node_id = resolve_p2p_peer_id(
        node_identity.as_ref().and_then(|id| id.peer_id.as_deref()),
        (!address.is_empty()).then_some(address.as_str()),
        &[],
        None,
    )
    .unwrap_or_default();
    (node_id, address)
}

/// The bearer issuer must be the admin of the network its token carries:
/// only admin-issued bearer invites exist (mint enforces it), so a token
/// whose issuer is not the embedded root's admin is forged or corrupted.
fn check_token_network_authority(token: &BearerInviteToken) -> Result<()> {
    if token.issuer_did != token.network.admin_did {
        anyhow::bail!(
            "bearer invite issuer {} is not the network admin {}; claim rejected",
            token.issuer_did,
            token.network.admin_did
        );
    }
    Ok(())
}

/// Mirror of v5 join's `enforce_local_network_match`: never let a claim
/// overwrite an existing local `AgentNetwork` bound to a different network or
/// a different admin.
fn check_local_network_match(
    local: Option<&NetworkRecord>,
    token: &BearerInviteToken,
) -> Result<()> {
    let Some(local) = local else {
        return Ok(());
    };
    if local.network_id != token.network.network_id {
        anyhow::bail!(
            "bearer invite is for network {} but this node is already bound to network {}; \
             claim rejected",
            token.network.network_id,
            local.network_id
        );
    }
    if local.admin_did != token.network.admin_did {
        anyhow::bail!(
            "bearer invite network admin {} does not match local network admin {}; claim rejected",
            token.network.admin_did,
            local.admin_did
        );
    }
    Ok(())
}

fn bearer_claim_create_mutation(record: &BearerClaimRecord) -> String {
    let token = escape_graphql_string(&record.token);
    let claimant_did = escape_graphql_string(&record.claimant_did);
    let claimant_node_id = escape_graphql_string(&record.claimant_node_id);
    let claimant_address = escape_graphql_string(&record.claimant_address);
    let claimed_at = escape_graphql_string(&record.claimed_at);
    let binding_sig = escape_graphql_string(&bs58::encode(&record.sig).into_string());
    format!(
        r#"mutation {{
            create_PairingBearerClaim(input: {{
                token: "{token}",
                claimant_did: "{claimant_did}",
                claimant_node_id: "{claimant_node_id}",
                claimant_address: "{claimant_address}",
                claimed_at: "{claimed_at}",
                binding_sig: "{binding_sig}"
            }}) {{ _docID }}
        }}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::bearer_token::BEARER_TOKEN_VERSION;

    fn network(network_id: &str, admin_did: &str) -> NetworkRecord {
        NetworkRecord {
            network_id: network_id.into(),
            admin_did: admin_did.into(),
            display_name: "Net".into(),
            default_template: "network-control".into(),
            created_at: "2026-07-08T00:00:00Z".into(),
            sig: vec![1],
        }
    }

    fn bearer(network_rec: NetworkRecord, issuer_did: &str) -> BearerInviteToken {
        BearerInviteToken {
            v: BEARER_TOKEN_VERSION,
            issuer_did: issuer_did.into(),
            peer_id: "peer-issuer".into(),
            ticket: "/ticket/issuer".into(),
            nonce: "nonce".into(),
            network_id: network_rec.network_id.clone(),
            issued_at: "2026-07-08T00:00:00Z".into(),
            template: "conversation".into(),
            default_behavior_id: Some("default".into()),
            network: network_rec,
            sig: vec![2],
        }
    }

    #[test]
    fn claim_rejects_issuer_that_is_not_network_admin() {
        let token = bearer(network("default", "did:key:admin"), "did:key:imposter");
        let err = check_token_network_authority(&token)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not the network admin"), "unexpected: {err}");

        let ok = bearer(network("default", "did:key:admin"), "did:key:admin");
        assert!(check_token_network_authority(&ok).is_ok());
    }

    #[test]
    fn claim_refuses_to_overwrite_a_different_local_network() {
        let token = bearer(network("net-b", "did:key:admin-b"), "did:key:admin-b");

        // No local network: fresh node, claim may pin the root.
        assert!(check_local_network_match(None, &token).is_ok());

        // Same network + admin: fine.
        let local = network("net-b", "did:key:admin-b");
        assert!(check_local_network_match(Some(&local), &token).is_ok());

        // Different network id: refused before any durable write.
        let local = network("net-a", "did:key:admin-a");
        let err = check_local_network_match(Some(&local), &token)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("already bound to network"),
            "unexpected: {err}"
        );

        // Same id, different admin: refused.
        let local = network("net-b", "did:key:other-admin");
        let err = check_local_network_match(Some(&local), &token)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("does not match local network admin"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn bearer_claim_mutation_escapes_all_fields_and_emits_no_empty_lists() {
        let record = BearerClaimRecord {
            token: "dabear1-tok\"quoted".into(),
            claimant_did: "did:key:phone".into(),
            claimant_node_id: "peer-phone".into(),
            claimant_address: "/ticket/phone".into(),
            claimed_at: "2026-07-08T00:00:00Z".into(),
            sig: vec![1, 2, 3],
        };
        let mutation = bearer_claim_create_mutation(&record);
        assert!(mutation.contains("create_PairingBearerClaim"));
        assert!(mutation.contains("token: \"dabear1-tok\\\"quoted\""));
        assert!(mutation.contains("claimant_did: \"did:key:phone\""));
        assert!(mutation.contains("binding_sig: "));
        assert!(!mutation.contains("[]"));
    }
}
