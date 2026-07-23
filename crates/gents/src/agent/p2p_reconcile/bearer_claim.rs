//! Issuer-side bearer-claim reconciler (issue #666).
//!
//! Claimant devices redeem an audience-unbound `dabear1-` invite by pushing a
//! self-signed `PairingBearerClaim` row to the issuer. This reconciler is the
//! authority that turns a valid claim into state: it verifies the embedded
//! token's issuer signature and the row's claimant signature, checks the
//! bearer freshness window, burns the token nonce in the issuer-side
//! `ConsumedInviteNonce` ledger (bound to the claimant DID), authors the
//! admin-signed `NetworkMembership`, and — for `conversation` tokens — records
//! the `ReciprocalConversationIntent` consumed by the reciprocal reconciler.
//!
//! Fenced by `Proofs/PeerRegistryDiscovery/BearerClaim.lean`:
//! - `decide_bearer_claim` mirrors `admits` (both signatures + freshness +
//!   nonce not bound elsewhere);
//! - the tick mirrors `claimStep` (atomic bind + mint, idempotent
//!   re-processing, ownership safety, intent iff `conversation`).
//!
//! Effects are ensure-if-absent, never update: re-processing a claim after a
//! crash between the nonce burn and the grant write repairs the missing rows,
//! but an operator's later revocation (a `NetworkMembership` row in any
//! status, including revoked) is never overwritten. The bearer freshness
//! window bounds the repair horizon: once a token is stale its claim rows are
//! inert forever, so operator deletions after the window can never be
//! resurrected by a lingering claim row.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_node::{EmbeddedNode, EventName, QueryResponse};
use gents_protocol::bearer_token::{
    bearer_signing_payload, check_bearer_freshness, decode_bearer, BearerClaimRecord,
};
use gents_protocol::network_token::{derive_membership_key, MembershipRecord};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;

pub const BEARER_CONVERSATION_TEMPLATE: &str = "conversation";

/// Signature/freshness verdicts for one claim, computed at the store seam and
/// consumed by the pure admission decision. Mirrors the Lean booleans on
/// `BearerToken`/`Claim` (`authoritySigned`, `fresh`, `claimantSigned`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BearerClaimVerdicts {
    pub token_authority_signed: bool,
    pub token_fresh: bool,
    pub claimant_signed: bool,
}

/// The issuer-side ledger's answer for a nonce, relative to one claimant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonceBinding {
    /// Nonce not in the ledger: first admission burns it.
    Unbound,
    /// Nonce burned by this same claimant: re-processing repairs idempotently.
    BoundToClaimant,
    /// Nonce burned by a different claimant: replay, rejected.
    BoundElsewhere,
}

/// Why a claim was not admitted. Mirrors the conjuncts of Lean `admits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearerRejection {
    UnsignedToken,
    StaleToken,
    UnsignedClaim,
    NonceBoundElsewhere,
}

/// Pure bearer-claim admission. Mirrors Lean `BearerClaim.admits`: both
/// signatures, freshness, and the nonce not bound to a different claimant.
/// Binding to the *same* claimant stays admissible (idempotent repair).
pub fn decide_bearer_claim(
    verdicts: BearerClaimVerdicts,
    binding: NonceBinding,
) -> Result<(), BearerRejection> {
    if !verdicts.token_authority_signed {
        return Err(BearerRejection::UnsignedToken);
    }
    if !verdicts.token_fresh {
        return Err(BearerRejection::StaleToken);
    }
    if !verdicts.claimant_signed {
        return Err(BearerRejection::UnsignedClaim);
    }
    if matches!(binding, NonceBinding::BoundElsewhere) {
        return Err(BearerRejection::NonceBoundElsewhere);
    }
    Ok(())
}

/// One claim after the store seam decoded and verified it: the fields the tick
/// needs plus the signature/freshness verdicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedBearerClaim {
    pub nonce: String,
    pub network_id: String,
    pub template: String,
    pub claimant_did: String,
    pub verdicts: BearerClaimVerdicts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BearerClaimTickOutcome {
    /// Claimant DIDs admitted (nonce newly burned) this tick.
    pub admitted: BTreeSet<String>,
    /// Claimant DIDs whose previously admitted claim was repaired (missing
    /// membership/intent rows re-ensured after a partial apply).
    pub repaired: BTreeSet<String>,
}

#[async_trait]
pub trait BearerClaimStore: Send + Sync {
    /// All replicated claim rows, decoded and verified. Malformed rows are
    /// dropped at the seam (they can never become admissible).
    async fn load_prepared_claims(&self) -> Result<Vec<PreparedBearerClaim>>;
    async fn nonce_binding(&self, nonce: &str, claimant_did: &str) -> Result<NonceBinding>;
    /// Burn the nonce bound to the claimant. The ledger's unique nonce index
    /// is the race backstop: losing the race is not an error — the tick
    /// re-reads the binding and proceeds only if this claimant won.
    async fn burn_nonce(&self, nonce: &str, issuer_did: &str, claimant_did: &str) -> Result<()>;
    /// Author the admin-signed membership IF ABSENT. A row in any status
    /// (including operator-revoked) is left untouched.
    async fn ensure_membership(&self, network_id: &str, member_did: &str) -> Result<()>;
    /// Record the reciprocal conversation intent IF ABSENT, or upgrade its
    /// template in place if the existing row's template differs from this
    /// claim's (a conversation→machine re-claim widens the row).
    async fn ensure_conversation_intent(&self, member_did: &str, template: &str) -> Result<()>;
}

pub async fn reconcile_bearer_claim_tick(
    store: &dyn BearerClaimStore,
    self_did: &str,
) -> Result<BearerClaimTickOutcome> {
    let claims = store
        .load_prepared_claims()
        .await
        .context("load prepared bearer claims")?;

    let mut outcome = BearerClaimTickOutcome::default();
    for claim in claims {
        let binding = store
            .nonce_binding(&claim.nonce, &claim.claimant_did)
            .await
            .with_context(|| format!("read nonce binding for bearer claim {}", claim.nonce))?;
        if let Err(rejection) = decide_bearer_claim(claim.verdicts, binding) {
            tracing::debug!(
                claimant_did = %claim.claimant_did,
                rejection = ?rejection,
                "bearer claim not admitted"
            );
            continue;
        }

        let newly_admitted = matches!(binding, NonceBinding::Unbound);
        if newly_admitted {
            store
                .burn_nonce(&claim.nonce, self_did, &claim.claimant_did)
                .await
                .with_context(|| format!("burn bearer nonce {}", claim.nonce))?;
            // The unique index is the race backstop: re-read and proceed only
            // if this claimant holds the binding (mirrors the Lean atomic
            // `claimStep` — at most one claimant per nonce).
            match store
                .nonce_binding(&claim.nonce, &claim.claimant_did)
                .await?
            {
                NonceBinding::BoundToClaimant => {}
                _ => {
                    tracing::debug!(
                        claimant_did = %claim.claimant_did,
                        "bearer claim lost the nonce race; rejected"
                    );
                    continue;
                }
            }
        }

        store
            .ensure_membership(&claim.network_id, &claim.claimant_did)
            .await
            .with_context(|| {
                format!(
                    "ensure membership for bearer claimant {}",
                    claim.claimant_did
                )
            })?;
        if super::templates::conversation_like(&claim.template) {
            store
                .ensure_conversation_intent(&claim.claimant_did, &claim.template)
                .await
                .with_context(|| {
                    format!(
                        "ensure conversation intent for bearer claimant {}",
                        claim.claimant_did
                    )
                })?;
        }

        if newly_admitted {
            outcome.admitted.insert(claim.claimant_did.clone());
        } else {
            outcome.repaired.insert(claim.claimant_did.clone());
        }
    }
    Ok(outcome)
}

pub async fn run_bearer_claim_reconciler(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    if node.p2p_arc().is_none() {
        tracing::debug!("bearer-claim reconciler idle because embedded node has no P2P transport");
        cancel.cancelled().await;
        return Ok(());
    }

    let store = GraphqlBearerClaimStore::new(node.clone(), identity.clone());
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    sweep_bearer_claims(&store, identity.did()).await;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => sweep_bearer_claims(&store, identity.did()).await,
            message = subscription.recv() => {
                if message.is_none() {
                    tracing::warn!("bearer-claim reconciler update subscription closed; continuing with periodic sweeps");
                    continue;
                }
                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "bearer-claim reconciler update subscription dropped messages");
                }
                sweep_bearer_claims(&store, identity.did()).await;
            }
        }
    }
}

async fn sweep_bearer_claims(store: &GraphqlBearerClaimStore, self_did: &str) {
    match reconcile_bearer_claim_tick(store, self_did).await {
        Ok(outcome) => {
            if !outcome.admitted.is_empty() || !outcome.repaired.is_empty() {
                tracing::info!(
                    admitted = ?outcome.admitted,
                    repaired = ?outcome.repaired,
                    "processed bearer pairing claims"
                );
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "bearer-claim reconcile sweep failed")
        }
    }
}

pub struct GraphqlBearerClaimStore {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
}

impl GraphqlBearerClaimStore {
    pub fn new(node: Arc<EmbeddedNode>, identity: Arc<dyn AgentIdentity>) -> Self {
        Self { node, identity }
    }

    /// True when this node administers `network_id` locally: the token's
    /// network must be one whose grants this identity is entitled to author.
    async fn administers_network(&self, network_id: &str) -> Result<bool> {
        let escaped = escape_graphql_string(network_id);
        let query = format!(
            r#"{{
                AgentNetwork(filter: {{ network_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                    admin_did
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query AgentNetwork for bearer claim")?;
        Ok(first_row::<NetworkAdminRow>(&response, "AgentNetwork")?
            .and_then(|row| row.admin_did)
            .map(|admin| admin.trim() == self.identity.did())
            .unwrap_or(false))
    }
}

#[async_trait]
impl BearerClaimStore for GraphqlBearerClaimStore {
    async fn load_prepared_claims(&self) -> Result<Vec<PreparedBearerClaim>> {
        let query = r#"{
            PairingBearerClaim {
                token
                claimant_did
                claimant_node_id
                claimant_address
                claimed_at
                binding_sig
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query PairingBearerClaim rows")?;

        let mut prepared = Vec::new();
        for row in rows::<ClaimRow>(&response, "PairingBearerClaim")? {
            let Some(encoded_token) = row
                .token
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
            else {
                continue;
            };
            let Some(claimant_did) = row
                .claimant_did
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
            else {
                continue;
            };
            let token = match decode_bearer(encoded_token) {
                Ok(token) => token,
                Err(error) => {
                    tracing::debug!(error = %error, "skipping malformed bearer claim token");
                    continue;
                }
            };
            // Only the issuer processes a claim, and only for a network it
            // administers: claims replicated onward to other peers are inert.
            if token.issuer_did.trim() != self.identity.did() {
                continue;
            }
            if !self.administers_network(&token.network_id).await? {
                tracing::warn!(
                    network_id = %token.network_id,
                    "skipping bearer claim for a network this node does not administer"
                );
                continue;
            }

            let token_authority_signed = verify_sig(
                self.identity.as_ref(),
                &token.issuer_did,
                &bearer_signing_payload(&token),
                &token.sig,
            )
            .await;
            let token_fresh = check_bearer_freshness(&token, Utc::now()).is_ok();
            let claim_record = BearerClaimRecord {
                token: encoded_token.to_string(),
                claimant_did: claimant_did.to_string(),
                claimant_node_id: row.claimant_node_id.clone().unwrap_or_default(),
                claimant_address: row.claimant_address.clone().unwrap_or_default(),
                claimed_at: row.claimed_at.clone().unwrap_or_default(),
                sig: Vec::new(),
            };
            let claimant_signed = match row
                .binding_sig
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| bs58::decode(s).into_vec())
            {
                Some(Ok(sig)) => {
                    verify_sig(
                        self.identity.as_ref(),
                        claimant_did,
                        &claim_record.signing_payload(),
                        &sig,
                    )
                    .await
                }
                _ => false,
            };

            prepared.push(PreparedBearerClaim {
                nonce: token.nonce.trim().to_string(),
                network_id: token.network_id.trim().to_string(),
                template: token.template.trim().to_string(),
                claimant_did: claimant_did.to_string(),
                verdicts: BearerClaimVerdicts {
                    token_authority_signed,
                    token_fresh,
                    claimant_signed,
                },
            });
        }
        Ok(prepared)
    }

    async fn nonce_binding(&self, nonce: &str, claimant_did: &str) -> Result<NonceBinding> {
        let nonce = nonce.trim();
        if nonce.is_empty() {
            bail!("bearer claim token is missing its single-use nonce");
        }
        let escaped = escape_graphql_string(nonce);
        let query = format!(
            r#"{{
                ConsumedInviteNonce(filter: {{ nonce: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                    claimant_did
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query ConsumedInviteNonce for bearer claim")?;
        Ok(
            match first_row::<NonceRow>(&response, "ConsumedInviteNonce")? {
                None => NonceBinding::Unbound,
                Some(row) => {
                    if row.claimant_did.as_deref().map(str::trim) == Some(claimant_did.trim()) {
                        NonceBinding::BoundToClaimant
                    } else {
                        NonceBinding::BoundElsewhere
                    }
                }
            },
        )
    }

    async fn burn_nonce(&self, nonce: &str, issuer_did: &str, claimant_did: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let mutation = burn_bearer_nonce_mutation(nonce, issuer_did, claimant_did, &now);
        match self.node.execute(&mutation).await {
            response if response.has_errors() => {
                let message = format!("{:?}", response.errors);
                // Losing the unique-index race is not an error: the tick
                // re-reads the binding and rejects if another claimant won.
                if message.contains("unique") || message.contains("duplicate") {
                    Ok(())
                } else {
                    bail!("burn bearer nonce failed: {message}");
                }
            }
            _ => Ok(()),
        }
    }

    async fn ensure_membership(&self, network_id: &str, member_did: &str) -> Result<()> {
        let membership_key = derive_membership_key(network_id, member_did);
        let escaped_key = escape_graphql_string(&membership_key);
        let query = format!(
            r#"{{
                NetworkMembership(filter: {{ membership_key: {{ _eq: "{escaped_key}" }} }}, limit: 1) {{
                    membership_key
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query NetworkMembership for bearer claim")?;
        if first_row::<MembershipKeyRow>(&response, "NetworkMembership")?.is_some() {
            // Ensure-if-absent: never overwrite an existing row — an
            // operator-revoked membership must stay revoked.
            return Ok(());
        }

        let mut record = MembershipRecord {
            network_id: network_id.to_string(),
            member_did: member_did.to_string(),
            status: "active".to_string(),
            granted_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            revoked_at: String::new(),
            sig: Vec::new(),
        };
        record.sig = self
            .identity
            .sign(&record.signing_payload())
            .await
            .context("signing bearer-claim membership grant")?;

        let mutation = bearer_membership_create_mutation(&membership_key, &record);
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "create NetworkMembership for bearer claim")
    }

    async fn ensure_conversation_intent(&self, member_did: &str, template: &str) -> Result<()> {
        let escaped = escape_graphql_string(member_did);
        let query = format!(
            r#"{{
                ReciprocalConversationIntent(filter: {{ member_did: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                    member_did
                    template
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(
            &response,
            "query ReciprocalConversationIntent for bearer claim",
        )?;
        if let Some(existing) =
            first_row::<IntentKeyRow>(&response, "ReciprocalConversationIntent")?
        {
            if existing.template.as_deref().map(str::trim) == Some(template.trim()) {
                // Already recorded with this exact template: nothing to do.
                return Ok(());
            }
        }

        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let mutation = bearer_intent_upsert_mutation(member_did, template, &now);
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(
            &response,
            "upsert ReciprocalConversationIntent for bearer claim",
        )
    }
}

pub fn burn_bearer_nonce_mutation(
    nonce: &str,
    issuer_did: &str,
    claimant_did: &str,
    now: &str,
) -> String {
    let nonce = escape_graphql_string(nonce);
    let issuer_did = escape_graphql_string(issuer_did);
    let claimant_did = escape_graphql_string(claimant_did);
    let now = escape_graphql_string(now);
    format!(
        r#"mutation {{
            create_ConsumedInviteNonce(input: {{
                nonce: "{nonce}",
                issuer_did: "{issuer_did}",
                claimant_did: "{claimant_did}",
                consumed_at: "{now}"
            }}) {{ _docID }}
        }}"#
    )
}

fn bearer_membership_create_mutation(membership_key: &str, record: &MembershipRecord) -> String {
    let membership_key = escape_graphql_string(membership_key);
    let network_id = escape_graphql_string(&record.network_id);
    let member_did = escape_graphql_string(&record.member_did);
    let status = escape_graphql_string(&record.status);
    let granted_at = escape_graphql_string(&record.granted_at);
    let revoked_at = escape_graphql_string(&record.revoked_at);
    let admin_sig = escape_graphql_string(&bs58::encode(&record.sig).into_string());
    format!(
        r#"mutation {{
            create_NetworkMembership(input: {{
                membership_key: "{membership_key}",
                network_id: "{network_id}",
                member_did: "{member_did}",
                status: "{status}",
                granted_at: "{granted_at}",
                revoked_at: "{revoked_at}",
                admin_sig: "{admin_sig}"
            }}) {{ _docID }}
        }}"#
    )
}

fn bearer_intent_upsert_mutation(member_did: &str, template: &str, now: &str) -> String {
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

async fn verify_sig(identity: &dyn AgentIdentity, did: &str, payload: &[u8], sig: &[u8]) -> bool {
    match identity.verify(did, payload, sig).await {
        Ok(valid) => valid,
        Err(error) => {
            // Best-effort swallow: a transient verifier failure skips this row
            // now and retries on the next sweep instead of halting the tick.
            tracing::warn!(error = %error, did = %did, "bearer claim signature verification errored");
            false
        }
    }
}

fn ensure_no_errors(response: &QueryResponse, label: &str) -> Result<()> {
    if response.has_errors() {
        bail!("{label} failed: {:?}", response.errors);
    }
    Ok(())
}

fn rows<T>(response: &QueryResponse, field: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(value) = response.data.as_ref().and_then(|data| data.get(field)) else {
        return Ok(Vec::new());
    };
    serde_json::from_value(value.clone()).with_context(|| format!("decode {field} rows"))
}

fn first_row<T>(response: &QueryResponse, field: &str) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    Ok(rows::<T>(response, field)?.into_iter().next())
}

#[derive(Deserialize)]
struct ClaimRow {
    token: Option<String>,
    claimant_did: Option<String>,
    claimant_node_id: Option<String>,
    claimant_address: Option<String>,
    claimed_at: Option<String>,
    #[serde(default)]
    binding_sig: Option<String>,
}

#[derive(Deserialize)]
struct NonceRow {
    #[serde(default)]
    claimant_did: Option<String>,
}

#[derive(Deserialize)]
struct NetworkAdminRow {
    admin_did: Option<String>,
}

#[derive(Deserialize)]
struct MembershipKeyRow {
    #[allow(dead_code)]
    membership_key: Option<String>,
}

#[derive(Deserialize)]
struct IntentKeyRow {
    #[allow(dead_code)]
    member_did: Option<String>,
    template: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;

    fn verdicts(token: bool, fresh: bool, claim: bool) -> BearerClaimVerdicts {
        BearerClaimVerdicts {
            token_authority_signed: token,
            token_fresh: fresh,
            claimant_signed: claim,
        }
    }

    /// Mirrors Lean `unsigned_token_grants_nothing`, `stale_token_grants_nothing`,
    /// `unsigned_claim_grants_nothing`, and the nonce conjunct of `admits`.
    #[test]
    fn decide_bearer_claim_requires_all_conjuncts() {
        assert_eq!(
            decide_bearer_claim(verdicts(false, true, true), NonceBinding::Unbound),
            Err(BearerRejection::UnsignedToken)
        );
        assert_eq!(
            decide_bearer_claim(verdicts(true, false, true), NonceBinding::Unbound),
            Err(BearerRejection::StaleToken)
        );
        assert_eq!(
            decide_bearer_claim(verdicts(true, true, false), NonceBinding::Unbound),
            Err(BearerRejection::UnsignedClaim)
        );
        assert_eq!(
            decide_bearer_claim(verdicts(true, true, true), NonceBinding::BoundElsewhere),
            Err(BearerRejection::NonceBoundElsewhere)
        );
        assert_eq!(
            decide_bearer_claim(verdicts(true, true, true), NonceBinding::Unbound),
            Ok(())
        );
        // Bound to the same claimant stays admissible: idempotent repair.
        assert_eq!(
            decide_bearer_claim(verdicts(true, true, true), NonceBinding::BoundToClaimant),
            Ok(())
        );
    }

    #[derive(Default)]
    struct MockBearerStore {
        claims: Vec<PreparedBearerClaim>,
        /// nonce -> claimant it is bound to
        bindings: Mutex<BTreeMap<String, String>>,
        memberships: Mutex<BTreeSet<(String, String)>>,
        intents: Mutex<BTreeSet<String>>,
        membership_writes: Mutex<Vec<(String, String)>>,
        intent_writes: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl BearerClaimStore for MockBearerStore {
        async fn load_prepared_claims(&self) -> Result<Vec<PreparedBearerClaim>> {
            Ok(self.claims.clone())
        }

        async fn nonce_binding(&self, nonce: &str, claimant_did: &str) -> Result<NonceBinding> {
            Ok(match self.bindings.lock().unwrap().get(nonce) {
                None => NonceBinding::Unbound,
                Some(bound) if bound == claimant_did => NonceBinding::BoundToClaimant,
                Some(_) => NonceBinding::BoundElsewhere,
            })
        }

        async fn burn_nonce(
            &self,
            nonce: &str,
            _issuer_did: &str,
            claimant_did: &str,
        ) -> Result<()> {
            self.bindings
                .lock()
                .unwrap()
                .entry(nonce.to_string())
                .or_insert_with(|| claimant_did.to_string());
            Ok(())
        }

        async fn ensure_membership(&self, network_id: &str, member_did: &str) -> Result<()> {
            let key = (network_id.to_string(), member_did.to_string());
            if self.memberships.lock().unwrap().insert(key.clone()) {
                self.membership_writes.lock().unwrap().push(key);
            }
            Ok(())
        }

        async fn ensure_conversation_intent(
            &self,
            member_did: &str,
            _template: &str,
        ) -> Result<()> {
            if self.intents.lock().unwrap().insert(member_did.to_string()) {
                self.intent_writes
                    .lock()
                    .unwrap()
                    .push(member_did.to_string());
            }
            Ok(())
        }
    }

    fn claim(
        nonce: &str,
        claimant: &str,
        template: &str,
        v: BearerClaimVerdicts,
    ) -> PreparedBearerClaim {
        PreparedBearerClaim {
            nonce: nonce.to_string(),
            network_id: "default".to_string(),
            template: template.to_string(),
            claimant_did: claimant.to_string(),
            verdicts: v,
        }
    }

    #[tokio::test]
    async fn admitted_conversation_claim_burns_nonce_mints_membership_and_intent() {
        let store = MockBearerStore {
            claims: vec![claim(
                "nonce-a",
                "did:key:phone",
                "conversation",
                verdicts(true, true, true),
            )],
            ..Default::default()
        };

        let outcome = reconcile_bearer_claim_tick(&store, "did:key:server")
            .await
            .unwrap();

        assert_eq!(
            outcome.admitted,
            BTreeSet::from(["did:key:phone".to_string()])
        );
        assert!(outcome.repaired.is_empty());
        assert_eq!(
            store.bindings.lock().unwrap().get("nonce-a"),
            Some(&"did:key:phone".to_string())
        );
        assert_eq!(
            *store.membership_writes.lock().unwrap(),
            vec![("default".to_string(), "did:key:phone".to_string())]
        );
        assert_eq!(
            *store.intent_writes.lock().unwrap(),
            vec!["did:key:phone".to_string()]
        );
    }

    #[tokio::test]
    async fn network_control_claim_creates_no_conversation_intent() {
        let store = MockBearerStore {
            claims: vec![claim(
                "nonce-a",
                "did:key:node",
                "network-control",
                verdicts(true, true, true),
            )],
            ..Default::default()
        };

        reconcile_bearer_claim_tick(&store, "did:key:server")
            .await
            .unwrap();

        assert!(store.intent_writes.lock().unwrap().is_empty());
        assert_eq!(store.membership_writes.lock().unwrap().len(), 1);
    }

    /// Mirrors Lean `claimStep_intent_iff_conversation_like`: `machine` is
    /// conversation-like and must mint the reciprocal intent too.
    #[tokio::test]
    async fn machine_claim_creates_conversation_intent() {
        let store = MockBearerStore {
            claims: vec![claim(
                "nonce-a",
                "did:key:laptop",
                "machine",
                verdicts(true, true, true),
            )],
            ..Default::default()
        };

        reconcile_bearer_claim_tick(&store, "did:key:server")
            .await
            .unwrap();

        assert_eq!(
            *store.intent_writes.lock().unwrap(),
            vec!["did:key:laptop".to_string()]
        );
        assert_eq!(store.membership_writes.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unsigned_or_stale_claims_grant_nothing() {
        let store = MockBearerStore {
            claims: vec![
                claim(
                    "n1",
                    "did:key:a",
                    "conversation",
                    verdicts(false, true, true),
                ),
                claim(
                    "n2",
                    "did:key:b",
                    "conversation",
                    verdicts(true, false, true),
                ),
                claim(
                    "n3",
                    "did:key:c",
                    "conversation",
                    verdicts(true, true, false),
                ),
            ],
            ..Default::default()
        };

        let outcome = reconcile_bearer_claim_tick(&store, "did:key:server")
            .await
            .unwrap();

        assert_eq!(outcome, BearerClaimTickOutcome::default());
        assert!(store.bindings.lock().unwrap().is_empty());
        assert!(store.membership_writes.lock().unwrap().is_empty());
        assert!(store.intent_writes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn second_claimant_on_same_nonce_is_rejected() {
        let store = MockBearerStore {
            claims: vec![
                claim(
                    "nonce-a",
                    "did:key:first",
                    "conversation",
                    verdicts(true, true, true),
                ),
                claim(
                    "nonce-a",
                    "did:key:second",
                    "conversation",
                    verdicts(true, true, true),
                ),
            ],
            ..Default::default()
        };

        let outcome = reconcile_bearer_claim_tick(&store, "did:key:server")
            .await
            .unwrap();

        assert_eq!(
            outcome.admitted,
            BTreeSet::from(["did:key:first".to_string()])
        );
        assert_eq!(
            *store.membership_writes.lock().unwrap(),
            vec![("default".to_string(), "did:key:first".to_string())]
        );
    }

    #[tokio::test]
    async fn reprocessing_admitted_claim_is_idempotent_repair() {
        let store = MockBearerStore {
            claims: vec![claim(
                "nonce-a",
                "did:key:phone",
                "conversation",
                verdicts(true, true, true),
            )],
            ..Default::default()
        };

        let first = reconcile_bearer_claim_tick(&store, "did:key:server")
            .await
            .unwrap();
        assert_eq!(
            first.admitted,
            BTreeSet::from(["did:key:phone".to_string()])
        );

        // Simulate a partial apply: the intent row vanished after the burn.
        store.intents.lock().unwrap().clear();

        let second = reconcile_bearer_claim_tick(&store, "did:key:server")
            .await
            .unwrap();
        assert!(second.admitted.is_empty());
        assert_eq!(
            second.repaired,
            BTreeSet::from(["did:key:phone".to_string()])
        );
        // Membership ensure-if-absent produced exactly one write across both
        // ticks; the intent was re-ensured once after the simulated loss.
        assert_eq!(store.membership_writes.lock().unwrap().len(), 1);
        assert_eq!(store.intent_writes.lock().unwrap().len(), 2);
    }

    #[test]
    fn burn_nonce_mutation_escapes_and_binds_claimant() {
        let mutation = burn_bearer_nonce_mutation(
            "nonce\"x",
            "did:key:issuer",
            "did:key:phone",
            "2026-07-08T00:00:00Z",
        );
        assert!(mutation.contains("create_ConsumedInviteNonce"));
        assert!(mutation.contains("nonce: \"nonce\\\"x\""));
        assert!(mutation.contains("claimant_did: \"did:key:phone\""));
        assert!(!mutation.contains("[]"));
    }
}
