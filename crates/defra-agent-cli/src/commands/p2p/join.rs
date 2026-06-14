use std::collections::BTreeSet;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_agent::agent::p2p_reconcile::expand_p2p_collection_profile_ids;
use defra_agent_protocol::pairing_token::{decode, signing_payload};
use serde_json::{json, Value};

use defra_agent::agent::p2p_reconcile::REGISTRY_STALE_AFTER;

use crate::cli::args::P2pJoinArgs;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::parse_duration_suffix;
use crate::{graphql_rows, print_json, resolve_config_access, resolve_graphql_endpoint};

use super::invite::{
    current_invite_token, encode_token, profile_ids_or_default, resolve_home_identity,
};
use super::pairings::{peer_pairing_exists, wait_for_pairing_connected, write_pairing_desired};

pub(super) async fn p2p_join(args: P2pJoinArgs) -> Result<()> {
    let remote = decode(&args.token)?;

    // Verify the issuer's signature over the token payload (TOFU bootstrap arm).
    let identity = resolve_home_identity(args.home.as_deref())
        .context("resolving local agent identity for invite verification")?;
    let payload = signing_payload(&remote);
    let valid = identity
        .verify(&remote.issuer_did, &payload, &remote.sig)
        .await
        .with_context(|| {
            format!(
                "verifying pairing invite signature for issuer {}",
                remote.issuer_did
            )
        })?;
    if !valid {
        anyhow::bail!(
            "pairing invite signature invalid for issuer {}",
            remote.issuer_did
        );
    }
    tracing::debug!(
        issuer_did = %remote.issuer_did,
        "pairing invite signature verified"
    );

    let profiles = accepted_profiles(&remote.profiles, &args)?;
    let collections = expand_p2p_collection_profile_ids(
        std::iter::empty::<&str>(),
        profiles.iter().map(String::as_str),
    )
    .context("expanding accepted pairing profiles")?
    .into_iter()
    .collect::<Vec<_>>();
    let addresses = vec![remote.ticket.clone()];
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;

    // Registry-membership gate (mirrors Lean `signedByMember`): once a local
    // `PeerRegistry` is populated with *other members*, an invite is only
    // admissible if its issuer is a live member. An empty/absent registry — or
    // one that holds only this node's own self-registration row — is the TOFU
    // bootstrap arm: the operator handed the token out-of-band and there is no
    // peer trust set to check against, so the verified signature above suffices.
    //
    // `--reciprocal` gate bypass — trust model:
    //
    // A reciprocal join completes the second leg of a bidirectional handshake:
    // peer A called `p2p pairings join <token>` (or used auto-pair), received a
    // `reciprocal_token` in the response, and now peer B calls
    // `p2p pairings join --reciprocal <reciprocal_token>` to wire the return
    // direction. Both sides have already agreed to pair; re-gating this leg on
    // registry membership would spuriously reject it whenever our registry has
    // since converged with new peers, making `--reciprocal` unreliable during
    // network formation.
    //
    // Safety invariant: the SIGNATURE IS STILL VERIFIED above — only the
    // registry-membership arm is bypassed. This is safe under the current
    // TOFU/trusted-fleet model where:
    //   - membership authority is cryptographic identity (DID / key material),
    //     not in-band registry state;
    //   - revocation is one-sided and deferred pending upstream primitives
    //     (defradb.rs#1012 for admin channels / #180 for ACP);
    //   - the trusted-fleet boundary is operator-controlled (the token was
    //     issued by a node that passed this same verification).
    //
    // TODO: once wire-admission lands (defradb.rs#1012/#180), `--reciprocal`
    // should be bound to a tracked pending/known reciprocal pairing (e.g., a
    // session nonce or pending-pair document) rather than a free CLI flag,
    // to prevent an unrelated actor from supplying a valid signature with
    // `--reciprocal` to bypass the membership gate entirely.
    if args.reciprocal {
        tracing::debug!(
            issuer_did = %remote.issuer_did,
            "reciprocal join: signature verified, membership gate bypassed (TOFU/trusted-fleet; see join.rs comment)"
        );
    } else {
        enforce_registry_membership(&access, &remote.issuer_did, identity.did()).await?;
    }

    let existed = peer_pairing_exists(&access, &remote.peer_id).await?;
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    // Thread issuer_did through as the `invited_by` value on the desired row.
    let doc_id = write_pairing_desired(
        &access,
        &remote.peer_id,
        Some(&remote.issuer_did),
        &collections,
        &addresses,
        &profiles,
        // Joins materialize the default `conversation` template; a dedicated
        // join `--template` front door lands with the registry-offers slice.
        "conversation",
        &now,
    )
    .await?;

    let p2p = if args.wait {
        let timeout = parse_duration_suffix(&args.timeout)?;
        Some(
            wait_for_pairing_connected(args.home.as_deref(), &graphql, &remote.peer_id, timeout)
                .await?,
        )
    } else {
        None
    };

    let reciprocal = if existed {
        None
    } else {
        let token = current_invite_token(args.home.as_deref(), &graphql, profiles.clone()).await?;
        Some(encode_token(&token)?)
    };

    let mut output = json!({
        "status": if existed { "pairing_exists" } else { "pairing_joined" },
        "home": home_dir,
        "graphql": graphql,
        "access_mode": access.mode(),
        "peer_id": remote.peer_id,
        "agent_did": remote.issuer_did,
        "profiles": profiles,
        "collections": collections,
        "replicator_addresses": addresses,
        "doc_id": doc_id,
        "waited": args.wait,
    });
    if let Some(reciprocal) = reciprocal {
        output["reciprocal_token"] = Value::String(reciprocal.clone());
        // The reciprocal command carries `--reciprocal` so the issuer pairing back
        // completes the handshake without being re-gated on registry membership.
        output["reciprocal_join_command"] = Value::String(format!(
            "defra-agent p2p pairings join --reciprocal {reciprocal}"
        ));
    }
    if let Some(p2p) = p2p {
        output["p2p"] = p2p;
    }

    print_json(&output)?;
    Ok(())
}

/// Registry-membership gate on join. If a local `PeerRegistry` holds members
/// *other than this node itself*, the invite issuer must be a *live* member
/// (status `online` and a fresh heartbeat); otherwise the join is rejected. An
/// empty or absent registry — or one whose only entry is this node's own
/// self-registration row (`self_did`) — is the TOFU bootstrap arm and is allowed
/// (the signature check already ran).
///
/// Excluding `self_did` is essential: every running node self-registers into
/// `PeerRegistry` via the heartbeat daemon, so without this exclusion a node's
/// own row would defeat its own bootstrap arm and it could never accept a first
/// (or reciprocal) invite.
///
/// Mirrors the Lean `signedByMember` predicate: `sigValid ∧ (tofuBootstrap ∨
/// isMember issuer reg)`.
async fn enforce_registry_membership(
    access: &ConfigAccess,
    issuer_did: &str,
    self_did: &str,
) -> Result<()> {
    let query = r#"query {
        PeerRegistry {
            agent_did
            status
            updated_at
        }
    }"#;
    // A missing PeerRegistry collection (older DB / no discovery) is treated as
    // the bootstrap arm: nothing to check against.
    let rows = match graphql_rows(access, "PeerRegistry", query).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::debug!(error = %error, "PeerRegistry unreadable; treating join as TOFU bootstrap");
            return Ok(());
        }
    };

    let now = Utc::now();
    let self_did = self_did.trim();
    let mut any_members = false;
    let mut issuer_is_live_member = false;
    for row in &rows {
        let did = row
            .get("agent_did")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(did) = did else { continue };
        // This node's own self-registration row does not count as a peer member:
        // a registry holding only ourselves is still the TOFU bootstrap arm.
        if !self_did.is_empty() && did == self_did {
            continue;
        }
        any_members = true;

        let status_online =
            row.get("status").and_then(Value::as_str).map(str::trim) == Some("online");
        let fresh = row
            .get("updated_at")
            .and_then(Value::as_str)
            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw.trim()).ok())
            .map(|ts| ts.with_timezone(&Utc))
            .map(|ts| {
                now.signed_duration_since(ts)
                    .to_std()
                    .map(|age| age <= REGISTRY_STALE_AFTER)
                    .unwrap_or(true)
            })
            .unwrap_or(false);

        if did == issuer_did && status_online && fresh {
            issuer_is_live_member = true;
        }
    }

    // Bootstrap arm: no members yet → TOFU, allow.
    if !any_members {
        tracing::debug!("PeerRegistry empty; join admitted via TOFU bootstrap arm");
        return Ok(());
    }

    if !issuer_is_live_member {
        anyhow::bail!(
            "pairing invite issuer {issuer_did} is not a live member of the local peer registry; \
             join rejected (registry is non-empty, so TOFU bootstrap does not apply)"
        );
    }

    tracing::debug!(
        issuer_did,
        "pairing invite issuer is a live registry member"
    );
    Ok(())
}

fn accepted_profiles(offered: &[String], args: &P2pJoinArgs) -> Result<Vec<String>> {
    let offered_set = offered
        .iter()
        .map(|profile| profile.trim())
        .filter(|profile| !profile.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    if offered_set.is_empty() {
        anyhow::bail!("pairing invite did not offer any profiles");
    }

    if args.profiles.is_empty() {
        return Ok(offered_set.into_iter().collect());
    }

    let requested = profile_ids_or_default(&args.profiles)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let accepted = offered_set
        .intersection(&requested)
        .cloned()
        .collect::<Vec<_>>();
    if accepted.is_empty() {
        anyhow::bail!(
            "pairing invite does not offer any requested profiles; offered: {}; requested: {}",
            offered.join(","),
            requested.into_iter().collect::<Vec<_>>().join(",")
        );
    }
    Ok(accepted)
}
