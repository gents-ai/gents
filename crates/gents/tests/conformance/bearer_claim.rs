//! Conformance fence for `Proofs/PeerRegistryDiscovery/BearerClaim.lean`.
//!
//! The Lean model admits a bearer claim iff the token verifies under the
//! authority DID, the token is fresh, the claimant signed the claim, and the
//! nonce is not bound to a different claimant (`admits`); an admitted claim
//! atomically binds the nonce and mints the membership plus — for
//! `conversation` tokens — the reciprocal intent (`claimStep`). These tests
//! pin the Rust decision (`decide_bearer_claim`) and tick to the model's six
//! theorem groups.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use gents::agent::p2p_reconcile::{
    decide_bearer_claim, reconcile_bearer_claim_tick, BearerClaimStore, BearerClaimVerdicts,
    BearerRejection, NonceBinding, PreparedBearerClaim,
};

fn verdicts(token: bool, fresh: bool, claim: bool) -> BearerClaimVerdicts {
    BearerClaimVerdicts {
        token_authority_signed: token,
        token_fresh: fresh,
        claimant_signed: claim,
    }
}

fn claim(nonce: &str, claimant: &str, template: &str) -> PreparedBearerClaim {
    PreparedBearerClaim {
        nonce: nonce.to_string(),
        network_id: "default".to_string(),
        template: template.to_string(),
        claimant_did: claimant.to_string(),
        verdicts: verdicts(true, true, true),
    }
}

/// Partitioned store: `claim_minted` is the claim-owned membership partition,
/// `operator_minted` the operator-authored one (Lean `operatorMemberships`).
#[derive(Default)]
struct ClaimPartitionStore {
    claims: Vec<PreparedBearerClaim>,
    bindings: Mutex<BTreeMap<String, String>>,
    claim_minted: Mutex<BTreeSet<String>>,
    intents: Mutex<BTreeSet<String>>,
    /// member_did -> template recorded on its intent row (mirrors the
    /// upgrade-in-place semantics of the real store: a re-claim with a
    /// different conversation-like template overwrites the template here).
    intent_templates: Mutex<BTreeMap<String, String>>,
    operator_minted: BTreeSet<String>,
    operator_touched: Mutex<bool>,
}

#[async_trait]
impl BearerClaimStore for ClaimPartitionStore {
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

    async fn burn_nonce(&self, nonce: &str, _issuer_did: &str, claimant_did: &str) -> Result<()> {
        self.bindings
            .lock()
            .unwrap()
            .entry(nonce.to_string())
            .or_insert_with(|| claimant_did.to_string());
        Ok(())
    }

    async fn ensure_membership(&self, _network_id: &str, member_did: &str) -> Result<()> {
        if self.operator_minted.contains(member_did) {
            *self.operator_touched.lock().unwrap() = true;
        }
        self.claim_minted
            .lock()
            .unwrap()
            .insert(member_did.to_string());
        Ok(())
    }

    async fn ensure_conversation_intent(&self, member_did: &str, template: &str) -> Result<()> {
        self.intents.lock().unwrap().insert(member_did.to_string());
        self.intent_templates
            .lock()
            .unwrap()
            .insert(member_did.to_string(), template.to_string());
        Ok(())
    }
}

/// Mirrors Lean `unsigned_token_grants_nothing`, `stale_token_grants_nothing`,
/// and `unsigned_claim_grants_nothing`: every missing conjunct rejects, and a
/// rejected claim changes no state.
#[tokio::test]
async fn missing_signature_or_freshness_grants_nothing() {
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

    let mut bad = claim("n", "did:key:a", "conversation");
    bad.verdicts = verdicts(true, true, false);
    let store = ClaimPartitionStore {
        claims: vec![bad],
        ..Default::default()
    };
    let outcome = reconcile_bearer_claim_tick(&store, "did:key:server")
        .await
        .expect("tick");
    assert!(outcome.admitted.is_empty() && outcome.repaired.is_empty());
    assert!(store.bindings.lock().unwrap().is_empty());
    assert!(store.claim_minted.lock().unwrap().is_empty());
    assert!(store.intents.lock().unwrap().is_empty());
}

/// Mirrors Lean `claimStep_binds_nonce` + `replay_rejected_across_claimants`:
/// the first admitted claim binds the nonce; a different claimant presenting
/// the same nonce is rejected in every subsequent state.
#[tokio::test]
async fn nonce_is_single_use_across_claimants() {
    let store = ClaimPartitionStore {
        claims: vec![
            claim("nonce-a", "did:key:first", "conversation"),
            claim("nonce-a", "did:key:second", "conversation"),
        ],
        ..Default::default()
    };

    let outcome = reconcile_bearer_claim_tick(&store, "did:key:server")
        .await
        .expect("tick");

    assert_eq!(
        outcome.admitted,
        BTreeSet::from(["did:key:first".to_string()])
    );
    assert_eq!(
        store.bindings.lock().unwrap().get("nonce-a"),
        Some(&"did:key:first".to_string())
    );
    assert!(!store
        .claim_minted
        .lock()
        .unwrap()
        .contains("did:key:second"));

    // And in the post state (a later tick), the second claimant stays rejected.
    let second = reconcile_bearer_claim_tick(&store, "did:key:server")
        .await
        .expect("second tick");
    assert!(!second.admitted.contains("did:key:second"));
    assert!(!second.repaired.contains("did:key:second"));
}

/// Mirrors Lean `claimStep_readmits_same_claim` + `claimStep_idempotent`:
/// re-processing an admitted claim converges (repairs) instead of wedging or
/// double-granting.
#[tokio::test]
async fn reprocessing_is_idempotent_convergence() {
    let store = ClaimPartitionStore {
        claims: vec![claim("nonce-a", "did:key:phone", "conversation")],
        ..Default::default()
    };

    let first = reconcile_bearer_claim_tick(&store, "did:key:server")
        .await
        .expect("first tick");
    let second = reconcile_bearer_claim_tick(&store, "did:key:server")
        .await
        .expect("second tick");

    assert_eq!(
        first.admitted,
        BTreeSet::from(["did:key:phone".to_string()])
    );
    assert!(second.admitted.is_empty());
    assert_eq!(
        second.repaired,
        BTreeSet::from(["did:key:phone".to_string()])
    );
    assert_eq!(
        store.bindings.lock().unwrap().len(),
        1,
        "nonce bound exactly once"
    );
}

/// Mirrors Lean `claimStep_binding_sound` + `claimStep_intent_iff_conversation_like`:
/// an admitted claim mints for exactly the presented claimant, and the intent
/// exists iff the token template is conversation-like (`conversation` or
/// `machine`) — the fleet no-crosswise invariant holds through the bearer path.
#[tokio::test]
async fn binding_is_exact_and_intent_iff_conversation() {
    let store = ClaimPartitionStore {
        claims: vec![
            claim("n1", "did:key:phone", "conversation"),
            claim("n2", "did:key:fleet", "network-control"),
            claim("n3", "did:key:laptop", "machine"),
        ],
        ..Default::default()
    };

    reconcile_bearer_claim_tick(&store, "did:key:server")
        .await
        .expect("tick");

    assert_eq!(
        *store.claim_minted.lock().unwrap(),
        BTreeSet::from([
            "did:key:phone".to_string(),
            "did:key:fleet".to_string(),
            "did:key:laptop".to_string(),
        ])
    );
    assert_eq!(
        *store.intents.lock().unwrap(),
        BTreeSet::from(["did:key:phone".to_string(), "did:key:laptop".to_string()]),
        "conversationLike claims (conversation, machine) record an intent; network-control must not"
    );
}

/// Mirrors the intent-upgrade half of `claimStep_intent_iff_conversation_like`:
/// a member who previously recorded a `conversation` intent and later
/// re-claims with a `machine` bearer token must have the intent row upgraded
/// in place, not left stuck on the narrower template (Task 6's drift test
/// depends on this).
#[tokio::test]
async fn machine_reclaim_upgrades_existing_conversation_intent_template() {
    let store = ClaimPartitionStore {
        claims: vec![claim("n1", "did:key:phone", "machine")],
        ..Default::default()
    };
    store
        .intents
        .lock()
        .unwrap()
        .insert("did:key:phone".to_string());
    store
        .intent_templates
        .lock()
        .unwrap()
        .insert("did:key:phone".to_string(), "conversation".to_string());

    reconcile_bearer_claim_tick(&store, "did:key:server")
        .await
        .expect("tick");

    assert_eq!(
        store.intent_templates.lock().unwrap().get("did:key:phone"),
        Some(&"machine".to_string()),
        "conversation->machine re-claim must upgrade the intent template in place"
    );
}

/// Last-claim-wins in the other direction (I2, #714): a member previously
/// recorded on the wider `machine` template who re-claims with a narrower
/// `conversation` bearer token must have the intent row narrowed in place.
/// This is deliberate, not a bug: re-pairing with a QR is an explicit
/// operator action, and the operator minted that QR — upgrade-only
/// semantics would make narrowing impossible without manual row surgery.
#[tokio::test]
async fn conversation_reclaim_narrows_machine_intent_template() {
    let store = ClaimPartitionStore {
        claims: vec![claim("n1", "did:key:phone", "conversation")],
        ..Default::default()
    };
    store
        .intents
        .lock()
        .unwrap()
        .insert("did:key:phone".to_string());
    store
        .intent_templates
        .lock()
        .unwrap()
        .insert("did:key:phone".to_string(), "machine".to_string());

    reconcile_bearer_claim_tick(&store, "did:key:server")
        .await
        .expect("tick");

    assert_eq!(
        store.intent_templates.lock().unwrap().get("did:key:phone"),
        Some(&"conversation".to_string()),
        "machine->conversation re-claim must narrow the intent template in place"
    );
}

/// Mirrors Lean `claimStep_ownership_safe`: claim processing never mutates
/// operator-authored memberships (ensure-if-absent leaves existing rows
/// untouched; the tick only ever writes for its own admitted claimants).
#[tokio::test]
async fn claim_processing_is_ownership_safe() {
    let store = ClaimPartitionStore {
        claims: vec![claim("n1", "did:key:phone", "conversation")],
        operator_minted: BTreeSet::from(["did:key:operator-member".to_string()]),
        ..Default::default()
    };

    reconcile_bearer_claim_tick(&store, "did:key:server")
        .await
        .expect("tick");

    assert!(
        !*store.operator_touched.lock().unwrap(),
        "bearer claim processing wrote to an operator-authored membership"
    );
}

/// Mirrors Lean `membership_growth_requires_admission`: the only DIDs the tick
/// can add are claimants of admitted claims.
#[tokio::test]
async fn membership_growth_requires_admission() {
    let mut rejected = claim("n1", "did:key:forger", "conversation");
    rejected.verdicts = verdicts(false, true, true);
    let store = ClaimPartitionStore {
        claims: vec![rejected, claim("n2", "did:key:legit", "conversation")],
        ..Default::default()
    };

    reconcile_bearer_claim_tick(&store, "did:key:server")
        .await
        .expect("tick");

    assert_eq!(
        *store.claim_minted.lock().unwrap(),
        BTreeSet::from(["did:key:legit".to_string()]),
        "only the admitted claimant may appear in the claim-minted partition"
    );
}
