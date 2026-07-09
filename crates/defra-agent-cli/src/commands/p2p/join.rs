use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use defra_agent::agent::p2p_reconcile::network::{decide_v5_admission, V5AdmissionClaim};
use defra_agent::agent::p2p_reconcile::resolve_template;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::AgentIdentity;
use defra_agent_protocol::network_token::{derive_network_id, MembershipRecord};
use defra_agent_protocol::pairing_token::{
    check_freshness, decode, signing_payload, InviteToken, DEFAULT_INVITE_MAX_AGE,
};
use serde_json::json;

use crate::cli::args::P2pJoinArgs;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::parse_duration_suffix;
use crate::{graphql_rows, print_json, resolve_config_access, resolve_graphql_endpoint};

use super::invite::resolve_home_identity;
use super::network_admin::{
    load_membership_record, load_optional_network_record, write_agent_network, write_membership,
};
use super::pairings::{
    complement_subagent_template, peer_pairing_exists, resolve_pairing_template,
    wait_for_pairing_connected, write_pairing_desired,
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

    // Single-use gate (Task C2 / #16): `remote.nonce` is the token's single-use
    // nonce. The replay check + nonce burn happen below, after `access` is
    // resolved and the membership gate has run — atomically with admission, so a
    // replayed token can never be wired twice (mirrors Lean `admitsJoin` +
    // `replay_rejected`, which consume the nonce in the admit transition).

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

    // v5 admission is membership-gated by two admin-signed records carried in
    // the token: the AgentNetwork root and an active NetworkMembership grant for
    // this local DID. The old registry TOFU arm is intentionally retired here:
    // PeerRegistry rows are discovery state, not cryptographic authorization.
    if args.reciprocal {
        tracing::debug!(
            issuer_did = %remote.issuer_did,
            "deprecated --reciprocal flag supplied; v5 joins still require a signed membership grant"
        );
    }
    enforce_v5_membership(identity.as_ref(), &remote).await?;
    enforce_local_network_match(&access, &remote).await?;
    enforce_local_membership_can_import_grant(&access, &remote).await?;

    // Single-use enforcement (Task C2 / #16): consume the token's nonce against
    // the `ConsumedInviteNonce` ledger before importing any token-carried
    // control-plane documents. A replayed invite must be rejected without
    // mutating local AgentNetwork / NetworkMembership state.
    consume_invite_nonce(&access, &remote.nonce, &remote.issuer_did).await?;

    // Keep a durable copy of the signed network root and grant on the joiner so
    // subsequent network-derived discovery has local membership context before
    // the desired pairing is reconciled.
    write_agent_network(&access, &remote.network).await?;
    write_membership(&access, &remote.grant).await?;

    let existed = peer_pairing_exists(&access, &remote.peer_id).await?;
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    // Thread issuer_did through as the `invited_by` value on the desired row. The
    // template alone scopes the pairing (v4 dropped the token's profiles field);
    // the mutation always writes the dead `profiles` column `null` (never `[]`,
    // per the DefraDB nillable-array sharp edge).
    let doc_id = write_pairing_desired(
        &access,
        &remote.peer_id,
        Some(&remote.issuer_did),
        &collections,
        &addresses,
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

    let mut output = json!({
        "status": if existed { "pairing_exists" } else { "pairing_joined" },
        "home": home_dir,
        "graphql": graphql,
        "access_mode": access.mode(),
        "peer_id": remote.peer_id,
        "agent_did": remote.issuer_did,
        "network_id": remote.network_id,
        "member_did": identity.did(),
        "template": template,
        "collections": collections,
        "replicator_addresses": addresses,
        "doc_id": doc_id,
        "waited": args.wait,
    });
    if let Some(p2p) = p2p {
        output["p2p"] = p2p;
    }

    print_json(&output)?;
    Ok(())
}

/// Single-use invite enforcement (Task C2 / #16). Consume `nonce` against the
/// `ConsumedInviteNonce` ledger: reject the join if the nonce was already
/// redeemed, otherwise record it and let the join proceed.
///
/// Ordering is record-then-wire: this runs before the `PeerPairingDesired` row is
/// written, so a replayed token cannot be wired a second time. Two backstops
/// against a concurrent double-redeem:
///   1. a presence query (clear "already used" error in the common case), and
///   2. the unique index on `nonce` (declared in the SDL) — if two joins race
///      past the query, the second insert fails the unique constraint and we
///      treat that as a replay rejection.
///
/// Mirrors the Lean `admitsJoin` precondition (`tok.nonce ∉ s.consumedNonces`)
/// and the nonce burn in the admit transition (`replay_rejected`).
async fn consume_invite_nonce(access: &ConfigAccess, nonce: &str, issuer_did: &str) -> Result<()> {
    let nonce = nonce.trim();
    if nonce.is_empty() {
        anyhow::bail!(
            "pairing invite is missing its single-use nonce; join rejected (re-issue the invite)"
        );
    }

    // 1) Presence check: a nonce already in the ledger is a replay.
    let escaped = escape_graphql_string(nonce);
    let query = format!(
        r#"query {{
            ConsumedInviteNonce(filter: {{ nonce: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                nonce
            }}
        }}"#
    );
    let existing = graphql_rows(access, "ConsumedInviteNonce", &query)
        .await
        .context("checking ConsumedInviteNonce ledger for a replayed nonce")?;
    if !existing.is_empty() {
        anyhow::bail!(
            "pairing invite already used (replay rejected): this invite's single-use \
             nonce was already redeemed; ask the issuer for a fresh invite"
        );
    }

    // 2) Record the nonce. The unique index on `nonce` is the race backstop: a
    //    concurrent second redeem that slipped past the query above fails here.
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mutation = create_consumed_invite_nonce_mutation(nonce, issuer_did, &now);
    match access.execute(&mutation).await {
        Ok(_) => {
            tracing::debug!(
                issuer_did = %issuer_did,
                "recorded consumed invite nonce; invite is now single-use spent"
            );
            Ok(())
        }
        Err(error) if is_unique_nonce_violation(&error) => anyhow::bail!(
            "pairing invite already used (replay rejected): this invite's single-use \
             nonce was redeemed concurrently; ask the issuer for a fresh invite"
        ),
        Err(error) => Err(error).context("recording consumed invite nonce in the ledger"),
    }
}

/// Build the `ConsumedInviteNonce` create mutation. Every interpolated value is
/// escaped; there are no list fields, so the empty-list-`[]` sharp edge does not
/// apply here.
fn create_consumed_invite_nonce_mutation(nonce: &str, issuer_did: &str, now: &str) -> String {
    let nonce = escape_graphql_string(nonce);
    let issuer_did = escape_graphql_string(issuer_did);
    let now = escape_graphql_string(now);
    format!(
        r#"mutation {{
            create_ConsumedInviteNonce(input: {{
                nonce: "{nonce}",
                issuer_did: "{issuer_did}",
                consumed_at: "{now}"
            }}) {{ _docID }}
        }}"#
    )
}

/// Recognise a unique-index violation on the `nonce` field, the race backstop for
/// concurrent invite redemption. DefraDB surfaces this as a "unique" error
/// mentioning the index/field; match conservatively on either marker.
fn is_unique_nonce_violation(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_lowercase();
    message.contains("unique") && (message.contains("nonce") || message.contains("index"))
}

/// Resolve the template for a join: explicit `--template` wins over the token's
/// template. The token's template is used when `--template` is not provided.
/// An unknown template id is a hard error.
fn resolve_join_template(cli_template: Option<&str>, token_template: &str) -> Result<String> {
    match cli_template {
        Some(template) => resolve_pairing_template(template),
        None => {
            let joiner_template = complement_subagent_template(token_template);
            resolve_pairing_template(&joiner_template)
        }
    }
}

/// Enforce v5 join admission: the runtime side of Lean `admitsV5Join`. The
/// structural + signature + grantee decision is the conformance-fenced
/// [`decide_v5_admission`]; this function resolves its inputs (the async
/// signature verifications and the deterministic network-id recompute) and maps
/// a rejection to a descriptive error. Single-use / replay of the nonce is
/// enforced separately by [`consume_invite_nonce`].
async fn enforce_v5_membership(identity: &dyn AgentIdentity, remote: &InviteToken) -> Result<()> {
    if remote.network.default_template.trim().is_empty() {
        anyhow::bail!("signed AgentNetwork default_template is empty");
    }

    // Deterministic-id recompute + token/network/grant network_id agreement,
    // folded into the decision's `network_id_consistent`.
    let expected_network_id =
        derive_network_id(&remote.network.admin_did, &remote.network.display_name);
    let network_id_consistent = remote.network.network_id == expected_network_id
        && remote.network_id == remote.network.network_id
        && remote.grant.network_id == remote.network.network_id;

    // Async signature verifications. An unverifiable/malformed signature counts
    // as not-valid and surfaces as the matching rejection from the decision.
    let network_sig_valid = identity
        .verify(
            &remote.network.admin_did,
            &remote.network.signing_payload(),
            &remote.network.sig,
        )
        .await
        .unwrap_or(false);
    let grant_sig_valid = identity
        .verify(
            &remote.network.admin_did,
            &remote.grant.signing_payload(),
            &remote.grant.sig,
        )
        .await
        .unwrap_or(false);

    let claim = V5AdmissionClaim {
        issuer_did: &remote.issuer_did,
        joiner_did: identity.did(),
        network_admin_did: &remote.network.admin_did,
        network_sig_valid,
        network_id_consistent,
        grant_member_did: &remote.grant.member_did,
        grant_status: &remote.grant.status,
        grant_sig_valid,
    };

    if let Err(rejection) = decide_v5_admission(&claim) {
        anyhow::bail!(
            "v5 join admission rejected: {} (issuer={}, network_admin={}, network_id={}, grantee={}, joiner={})",
            rejection.reason(),
            remote.issuer_did,
            remote.network.admin_did,
            remote.network.network_id,
            remote.grant.member_did,
            identity.did(),
        );
    }

    tracing::debug!(
        network_id = %remote.network.network_id,
        admin_did = %remote.network.admin_did,
        member_did = %remote.grant.member_did,
        "v5 invite membership grant verified"
    );
    Ok(())
}

/// Reject an invite whose signed network conflicts with an existing local
/// AgentNetwork. A fresh joiner with no AgentNetwork yet is admitted and then
/// persists the signed network root from the token.
async fn enforce_local_network_match(access: &ConfigAccess, remote: &InviteToken) -> Result<()> {
    let Some(local) = load_optional_network_record(access)
        .await
        .context("loading local AgentNetwork before join")?
    else {
        return Ok(());
    };

    if local.network_id != remote.network.network_id {
        anyhow::bail!(
            "pairing invite is for network {} but this node is already bound to network {}; \
             join rejected",
            remote.network.network_id,
            local.network_id
        );
    }
    if local.admin_did != remote.network.admin_did {
        anyhow::bail!(
            "pairing invite network admin {} does not match local network admin {}; join rejected",
            remote.network.admin_did,
            local.admin_did
        );
    }
    Ok(())
}

/// Do not let an invite-carried active grant regress a locally known revocation
/// tombstone. A future re-grant is accepted only when the token's `granted_at`
/// is strictly newer than the local `revoked_at`.
async fn enforce_local_membership_can_import_grant(
    access: &ConfigAccess,
    remote: &InviteToken,
) -> Result<()> {
    let Some(existing) =
        load_membership_record(access, &remote.grant.network_id, &remote.grant.member_did)
            .await
            .context("loading local NetworkMembership before join")?
    else {
        return Ok(());
    };

    if existing.status.trim() != "revoked" {
        return Ok(());
    }

    if active_grant_supersedes_revocation(&remote.grant, &existing)? {
        return Ok(());
    }

    anyhow::bail!(
        "pairing invite carries an active grant for {} but this node already knows a \
         revoked membership in network {}; join rejected until a newer signed grant is used",
        remote.grant.member_did,
        remote.grant.network_id
    );
}

fn active_grant_supersedes_revocation(
    incoming: &MembershipRecord,
    existing: &MembershipRecord,
) -> Result<bool> {
    if existing.status.trim() != "revoked" {
        return Ok(true);
    }
    if incoming.status.trim() != "active" {
        return Ok(false);
    }

    let revoked_at = parse_rfc3339(&existing.revoked_at, "existing revoked_at")?;
    let granted_at = parse_rfc3339(&incoming.granted_at, "incoming granted_at")?;
    Ok(granted_at > revoked_at)
}

fn parse_rfc3339(value: &str, label: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .map(|datetime| datetime.with_timezone(&Utc))
        .with_context(|| format!("parsing {label} timestamp {value:?}"))
}

/// Resolve the collection set a template scopes, as owned strings for the
/// `PeerPairingDesired` row. The reconciler independently resolves collections
/// from `template`, so this is informational; the template id is authoritative.
/// `template` is assumed already validated by `resolve_pairing_template`.
pub(super) fn template_collections(template: &str) -> Vec<String> {
    resolve_template(template)
        .map(|t| t.collections.iter().map(|&c| c.to_string()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn membership(status: &str, granted_at: &str, revoked_at: &str) -> MembershipRecord {
        MembershipRecord {
            network_id: "network-1".to_string(),
            member_did: "did:key:member".to_string(),
            status: status.to_string(),
            granted_at: granted_at.to_string(),
            revoked_at: revoked_at.to_string(),
            sig: Vec::new(),
        }
    }

    #[test]
    fn active_grant_does_not_supersede_newer_revocation() {
        let incoming = membership("active", "2026-06-17T10:00:00Z", "");
        let existing = membership("revoked", "2026-06-17T09:00:00Z", "2026-06-17T11:00:00Z");

        assert!(!active_grant_supersedes_revocation(&incoming, &existing).unwrap());
    }

    #[test]
    fn active_grant_supersedes_older_revocation() {
        let incoming = membership("active", "2026-06-17T12:00:00Z", "");
        let existing = membership("revoked", "2026-06-17T09:00:00Z", "2026-06-17T11:00:00Z");

        assert!(active_grant_supersedes_revocation(&incoming, &existing).unwrap());
    }

    #[test]
    fn existing_active_membership_allows_import() {
        let incoming = membership("active", "2026-06-17T10:00:00Z", "");
        let existing = membership("active", "2026-06-17T09:00:00Z", "");

        assert!(active_grant_supersedes_revocation(&incoming, &existing).unwrap());
    }

    #[test]
    fn join_complements_token_subagent_role_only_without_explicit_override() {
        assert_eq!(
            resolve_join_template(None, "subagent-coordinator").unwrap(),
            "subagent-host"
        );
        assert_eq!(
            resolve_join_template(Some("subagent-host"), "subagent-coordinator").unwrap(),
            "subagent-host"
        );
        assert_eq!(
            resolve_join_template(None, "conversation").unwrap(),
            "conversation"
        );
    }
}
