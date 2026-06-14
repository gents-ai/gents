use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_agent::agent::p2p_reconcile::{resolve_network_id, resolve_template};
use defra_agent_protocol::pairing_token::{
    check_freshness, decode, signing_payload, InviteToken, DEFAULT_INVITE_MAX_AGE,
};
use serde_json::{json, Value};

use defra_agent::agent::p2p_reconcile::{
    decide_join_admission, JoinAdmission, RegistryMemberRow, REGISTRY_STALE_AFTER,
};

use crate::cli::args::P2pJoinArgs;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::parse_duration_suffix;
use crate::{graphql_rows, print_json, resolve_config_access, resolve_graphql_endpoint};

use super::invite::{current_invite_token, encode_token, resolve_home_identity};
use super::pairings::{
    peer_pairing_exists, resolve_pairing_template, wait_for_pairing_connected,
    write_pairing_desired,
};

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

    // Freshness gate: reject a token whose signed `issued_at` is outside the max
    // age, bounding the replay window of a leaked invite. (Coarse — not single-use;
    // see check_freshness.)
    check_freshness(&remote, Utc::now(), DEFAULT_INVITE_MAX_AGE)
        .context("pairing invite failed the freshness check")?;

    // Network gate: the token's signed `network_id` must match the local node's
    // resolved discovery network. A token minted for a different network is
    // rejected so an invite cannot bridge a peer into the wrong fleet. (The
    // value is part of the signed payload, so a mismatch is not forgeable.)
    enforce_network_match(&remote)?;

    // NOTE (Task C2): `remote.nonce` is the single-use token nonce. The
    // consumed-nonce ledger + replay rejection lands in C2; this path only
    // round-trips the field today.

    // Resolve the template: explicit --template wins; otherwise use the token's
    // template. The template is the sole source of the pairing's collection scope
    // (v4 dropped the token's `profiles` field); the reconciler also resolves
    // collections from `template`, so this set is informational on the row.
    let template = resolve_join_template(args.template.as_deref(), &remote.template)?;
    let collections = template_collections(&template);
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
    // `profiles` is empty: v4 dropped the token's profiles field and the
    // template alone scopes the pairing. The mutation renders `null` for the
    // empty list (never `[]`, per the DefraDB nillable-array sharp edge).
    let doc_id = write_pairing_desired(
        &access,
        &remote.peer_id,
        Some(&remote.issuer_did),
        &collections,
        &addresses,
        &[],
        &template,
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
        let token = current_invite_token(args.home.as_deref(), &graphql, &template).await?;
        Some(encode_token(&token)?)
    };

    let mut output = json!({
        "status": if existed { "pairing_exists" } else { "pairing_joined" },
        "home": home_dir,
        "graphql": graphql,
        "access_mode": access.mode(),
        "peer_id": remote.peer_id,
        "agent_did": remote.issuer_did,
        "template": template,
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

    // Project the loaded rows into the membership-gate input, then delegate the
    // decision to the shared, conformance-fenced predicate (the Rust mirror of
    // Lean `signedByMember`). NOTE: the registry rows are read from replicated,
    // self-asserted state and are NOT signature-bound to their claimed
    // `agent_did` (PeerRegistry carries no per-row signature). This gate is a
    // TRUSTED-FLEET / TOFU check, not cryptographic authorization — see #490
    // review H4; do not treat membership here as proof of identity.
    let member_rows: Vec<RegistryMemberRow> = rows
        .iter()
        .map(|row| RegistryMemberRow {
            agent_did: row
                .get("agent_did")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            status: row
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            updated_at: row
                .get("updated_at")
                .and_then(Value::as_str)
                .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw.trim()).ok())
                .map(|ts| ts.with_timezone(&Utc)),
        })
        .collect();

    match decide_join_admission(
        issuer_did,
        self_did,
        &member_rows,
        Utc::now(),
        REGISTRY_STALE_AFTER,
    ) {
        JoinAdmission::TofuBootstrap => {
            tracing::debug!(
                "PeerRegistry has no peer members; join admitted via TOFU bootstrap arm"
            );
            Ok(())
        }
        JoinAdmission::MemberAdmitted => {
            tracing::debug!(
                issuer_did,
                "pairing invite issuer is a live registry member"
            );
            Ok(())
        }
        JoinAdmission::Rejected => anyhow::bail!(
            "pairing invite issuer {issuer_did} is not a live member of the local peer registry; \
             join rejected (registry is non-empty, so TOFU bootstrap does not apply)"
        ),
    }
}

/// Resolve the template for a join: explicit `--template` wins over the token's
/// template. The token's template is used when `--template` is not provided.
/// An unknown template id is a hard error.
fn resolve_join_template(cli_template: Option<&str>, token_template: &str) -> Result<String> {
    let template = cli_template.unwrap_or(token_template);
    resolve_pairing_template(template)
}

/// Reject an invite whose signed `network_id` does not match the local node's
/// resolved discovery network. The network id is part of the signed payload, so
/// a mismatch means the token was minted for a different fleet (or tampered with,
/// which the signature check already caught). Comparison is trimmed.
fn enforce_network_match(remote: &InviteToken) -> Result<()> {
    let local = resolve_network_id();
    let token_network = remote.network_id.trim();
    if token_network != local.trim() {
        anyhow::bail!(
            "pairing invite is for network {token_network:?} but this node is on network {local:?}; \
             join rejected (set {} to match, or use an invite for this network)",
            defra_agent::agent::p2p_reconcile::NETWORK_ID_ENV
        );
    }
    Ok(())
}

/// Resolve the collection set a template scopes, as owned strings for the
/// `PeerPairingDesired` row. The reconciler independently resolves collections
/// from `template`, so this is informational; the template id is authoritative.
/// `template` is assumed already validated by `resolve_pairing_template`.
fn template_collections(template: &str) -> Vec<String> {
    resolve_template(template)
        .map(|t| t.collections.iter().map(|&c| c.to_string()).collect())
        .unwrap_or_default()
}
