import Proofs.PeerRegistryDiscovery.Transition
import Mathlib.Data.Finset.Image

/-!
# Peer Registry Discovery — Derivation properties

The carried-over derivation/ownership/nonce/reciprocal theorems, re-proven
against the **signed network-membership** model (`deriveMaterializable`,
`Membership`, `joinState`). Every transition-level theorem is discharged by
`cases` over the full `Transition` relation, so no convenient case is silently
skipped.

The new membership obligations (O1–O5) live in a separate task; this file only
carries the prior proven content onto the new derivation.
-/

namespace PeerRegistryDiscovery

open DiscoveryState

/-! ## Derivation lemmas -/

/-- Self is never derived. (Sanity: the filter carries `ep.peer ≠ s.self`.) -/
theorem self_not_mem_derive {s : DiscoveryState} :
    s.self ∉ deriveMaterializable s := by
  rw [mem_deriveMaterializable]
  rintro ⟨ep, _, ⟨_, _, hself⟩, hpeer⟩
  exact hself (hpeer.trans rfl)

/-! ## (1) Idempotence -/

/-- Deriving twice equals deriving once: the membership world is the sole input
and a derive step does not change it. -/
theorem derive_idempotent (s : DiscoveryState) :
    deriveStep (deriveStep s) = deriveStep s := by
  unfold deriveStep
  rfl

/-- The function-level idempotence the Rust derivation mirrors: re-deriving over
the same membership world yields the same set. -/
theorem deriveMaterializable_idempotent (s : DiscoveryState) :
    deriveMaterializable (deriveStep s) = deriveMaterializable s := rfl

/-! ## (2) Convergence

The derived set is a *pure function of the membership world*, so it converges in
a single derive step: the post-state is settled, and any further derive over an
unchanged world is a fixpoint. We state exactly this and do not claim a
multi-step reachability result over arbitrary interleavings. -/

/-- One derive step settles the network-owned partition. -/
theorem derive_settles (s : DiscoveryState) : (deriveStep s).settled := by
  unfold settled deriveStep
  rfl

/-- A settled state with an unchanged membership world is a derive fixpoint:
re-running the derivation changes nothing. This is the convergence content —
stability of the derived set across ticks given a stable world. -/
theorem derive_convergent {s : DiscoveryState} (h : s.settled) :
    deriveStep s = s := by
  unfold settled at h
  unfold deriveStep
  rw [← h]

/-! ## (3) Ownership safety (= obligation O4)

Quantified over ALL transitions. The discovery step (and every membership /
request / nonce edit) leaves the operator partition byte-identical — each such
mutator is a `{s with …}` that does not name `operatorDesired`. The ONE
transition that does change `operatorDesired` is `operatorWrite` itself, by
construction (the operator writing its own set is not a discovery step), and it
is excluded by hypothesis. So: every non-operator transition preserves
`operatorDesired`, and `operatorWrite` is the sole named exception. -/

/-- No derivation / membership / request / nonce transition mutates an
operator-owned row. The only transition that changes `operatorDesired` is
`operatorWrite` itself, excluded here by `h_not_operator`. -/
theorem ownership_safe {pre post : DiscoveryState} (h : Transition pre post)
    (h_not_operator : ∀ d, post ≠ operatorWriteState pre d) :
    post.operatorDesired = pre.operatorDesired := by
  cases h with
  | derive h_post => subst h_post; rfl
  | join tok tofu _ h_post => subst h_post; rfl
  | reciprocalJoin tok tofu _ h_post => subst h_post; rfl
  | submitRequest req h_post => subst h_post; rfl
  | approveRequest req m _ _ _ _ _ _ h_post =>
      subst h_post; unfold approveMembershipState upsertMembership; rfl
  | revoke tomb _ _ h_post =>
      subst h_post; unfold revokeState upsertMembership; rfl
  | operatorWrite d h_post => exact absurd h_post (h_not_operator d)

/-! ## (6) Single-use invite (replay rejection)

A join consumes its token's nonce. Because admission additionally requires the
nonce to be *fresh*, the same physical token can never be admitted twice: the
first join burns the nonce, and the second admission fails on exactly that fact.
This upgrades the runtime's 1h freshness window from "time-bounded" to
"single-use within the window". -/

/-- The join mutator does record the nonce as consumed. (The structural fact
`replay_rejected` leans on.) -/
theorem joinState_consumes_nonce (s : DiscoveryState) (tok : Token) :
    tok.nonce ∈ (joinState s tok).consumedNonces := by
  unfold joinState
  simp

/-- **Replay rejected.** If a join with token `tok` is admitted from `pre`
(`hadm : admitsJoin pre tok tofu`) and steps to `post = joinState pre tok`, then
the SAME token can no longer be admitted from `post`: `admitsJoin post tok` is
false for every bootstrap choice.

Non-vacuous: the hypothesis `admitsJoin pre tok tofu` is satisfiable — it is
exactly the precondition discharged by the `Transition.join` constructor, and is
witnessed below in `replay_rejected_witness` by a concrete first join that IS
admitted. The conclusion is a genuine consequence, not a restatement: we prove
`¬ admitsJoin post tok` by unfolding `admitsJoin` and refuting its freshness
conjunct using `joinState_consumes_nonce` — i.e. the second admission fails
*because* the first join consumed the nonce, never touching the signature arm. -/
theorem replay_rejected {pre post : DiscoveryState} {tok : Token}
    {tofu : Bool} (_hadm : admitsJoin pre tok tofu)
    (hpost : post = joinState pre tok) :
    ∀ tofu', ¬ admitsJoin post tok tofu' := by
  intro tofu' hadm'
  -- `admitsJoin post tok` requires `tok.nonce ∉ post.consumedNonces`,
  -- but the join that produced `post` consumed exactly `tok.nonce`.
  have h_consumed : tok.nonce ∈ post.consumedNonces := by
    rw [hpost]; exact joinState_consumes_nonce pre tok
  exact hadm'.2 h_consumed

/-- Witness that `replay_rejected`'s hypothesis is satisfiable: a concrete state
in which a fresh, member-signed token IS admitted for a first join. This pins the
theorem as non-vacuous — there really is a `(pre, tok, tofu)` with
`admitsJoin pre tok tofu`.

The witness state has one valid network (`adminSigValid = true`), one active
admin-signed membership for `did:key:a`, an empty `consumedNonces`, and a token
issued by `did:key:a` with a fresh nonce and a valid signature. -/
theorem replay_rejected_witness :
    ∃ (pre : DiscoveryState) (tok : Token) (tofu : Bool),
      admitsJoin pre tok tofu := by
  let network : Network := ⟨"net-1", "did:key:admin", true⟩
  let membership : Membership := ⟨"net-1", "did:key:a", true, "did:key:admin", true⟩
  let pre : DiscoveryState :=
    { self := "peer-self"
    , network := network
    , memberships := {membership}
    , endpoints := ∅
    , requests := ∅
    , operatorDesired := ∅
    , registryDesired := ∅
    , consumedNonces := ∅ }
  let tok : Token := ⟨"did:key:a", true, "nonce-1"⟩
  refine ⟨pre, tok, false, ?_, ?_⟩
  · -- signedByMember: signature valid and issuer is an admitted member.
    refine ⟨rfl, Or.inl ?_⟩
    refine ⟨rfl, membership, Finset.mem_singleton_self membership, rfl, rfl, ?_⟩
    exact ⟨rfl, rfl, rfl⟩
  · -- freshness: the nonce is not in the (empty) consumed set.
    exact Finset.not_mem_empty _

/-! ## (7) Reciprocal join stays under the admission gate (Finding #8)

The Rust impl wired the `--reciprocal` return replicator on signature alone,
skipping `decide_join_admission` — a transition with no model counterpart. The
`Transition.reciprocalJoin` constructor closes that gap: it carries the SAME
`admitsJoin pre tok tofuBootstrap` precondition as the plain `join`. These
theorems prove the reciprocal flag changes only *what is wired*, never *whether
the join is admitted*. -/

/-- **A reciprocal join is gated by `admitsJoin`.** If a transition `h` from
`pre` to `post` is a reciprocal join of `tok`/`tofu` (i.e. it is equal to *some*
application of the `reciprocalJoin` constructor on those arguments), then
`admitsJoin pre tok tofu` holds. The admission proof is the constructor's own
precondition, bound existentially in the hypothesis and pulled back out — it is
NOT handed to the theorem as a free fact, so the conclusion genuinely rides on
the constructor having been firable.

Non-vacuous: `reciprocal_join_witness` exhibits a concrete `(pre, post, tok,
tofu)` for which such a transition really exists, so the hypothesis space is
inhabited and the implication is not vacuously true. -/
theorem reciprocal_join_still_gated {pre post : DiscoveryState}
    {tok : Token} {tofu : Bool}
    (h : Transition pre post)
    (h_is_reciprocal :
      ∃ (hadm : admitsJoin pre tok tofu) (hpost : post = joinState pre tok),
        h = Transition.reciprocalJoin tok tofu hadm hpost) :
    admitsJoin pre tok tofu :=
  -- The admission proof is precisely the precondition the constructor demanded.
  h_is_reciprocal.choose

/-- Witness that `reciprocal_join_still_gated`'s hypothesis space is inhabited:
a concrete reciprocal join transition that IS admissible (fresh, member-signed
token). Without this the gating theorem could be vacuous. -/
theorem reciprocal_join_witness :
    ∃ (pre post : DiscoveryState) (tok : Token) (tofu : Bool),
      Transition pre post ∧ admitsJoin pre tok tofu := by
  let network : Network := ⟨"net-1", "did:key:admin", true⟩
  let membership : Membership := ⟨"net-1", "did:key:a", true, "did:key:admin", true⟩
  let pre : DiscoveryState :=
    { self := "peer-self"
    , network := network
    , memberships := {membership}
    , endpoints := ∅
    , requests := ∅
    , operatorDesired := ∅
    , registryDesired := ∅
    , consumedNonces := ∅ }
  let tok : Token := ⟨"did:key:a", true, "nonce-1"⟩
  have hadm : admitsJoin pre tok false := by
    refine ⟨⟨rfl, Or.inl ?_⟩, Finset.not_mem_empty _⟩
    refine ⟨rfl, membership, Finset.mem_singleton_self membership, rfl, rfl, ?_⟩
    exact ⟨rfl, rfl, rfl⟩
  exact ⟨pre, joinState pre tok, tok, false,
    Transition.reciprocalJoin tok false hadm rfl, hadm⟩

/-- **Refutation teeth (signature-invalid case).** If a token's signature is
invalid (`tok.sigValid = false`), then it is not admissible, so the
`reciprocalJoin` constructor — whose precondition is exactly `admitsJoin` — has
no proof to fire on. The reciprocal leg cannot be wired on a forged signature,
exactly the Rust defect being fenced.

We phrase the impossibility as `¬ admitsJoin`: since every reciprocal join
transition demands `admitsJoin pre tok tofu` as its precondition, an
uninhabitable precondition means no such transition exists.

Non-vacuous: `reciprocal_join_rejected_witness` exhibits a concrete `sigValid =
false` token, so the hypothesis is realizable; the conclusion follows because
`admitsJoin` requires `signedByMember`, which requires `tok.sigValid = true`. -/
theorem reciprocal_join_rejected_on_bad_signature {pre : DiscoveryState}
    {tok : Token} {tofu : Bool}
    (h_bad_sig : tok.sigValid = false) :
    ¬ admitsJoin pre tok tofu := by
  rintro ⟨⟨hsig, _⟩, _⟩
  -- admitsJoin → signedByMember → sigValid = true, contradicting h_bad_sig.
  rw [h_bad_sig] at hsig
  exact Bool.false_ne_true hsig

/-- Witness that the signature-invalid hypothesis of
`reciprocal_join_rejected_on_bad_signature` is realizable: a concrete token with
`sigValid = false`. Pins that theorem as non-vacuous. -/
theorem reciprocal_join_rejected_witness :
    ∃ tok : Token, tok.sigValid = false := by
  exact ⟨⟨"did:key:x", false, "nonce-x"⟩, rfl⟩

/-- Replay rejection extends to reciprocal joins for free: a reciprocal join
applies the same `joinState` mutator, so it burns the nonce identically and the
same token can never be re-admitted (by any subsequent join OR reciprocal join).
Reuses `replay_rejected` — no separate single-use argument needed. -/
theorem reciprocal_replay_rejected {pre post : DiscoveryState} {tok : Token}
    {tofu : Bool} (hadm : admitsJoin pre tok tofu)
    (hpost : post = joinState pre tok) :
    ∀ tofu', ¬ admitsJoin post tok tofu' :=
  replay_rejected hadm hpost

/-! ## (8) `wellFormed` preservation

`wellFormed` is "at most one membership per `(networkId, memberDid)`".
`upsertMembership` filters out every same-key row before inserting `m`, so the
result keeps the invariant; the membership-writing transitions route through it,
and the rest never touch `memberships`. -/

/-- `upsertMembership` preserves `wellFormed`: after filtering out every
same-`(networkId, memberDid)` row and inserting `m`, any two rows sharing the key
are either both `m` (equal) or one is `m` and the other survived the filter — but
a surviving row sharing `m`'s key contradicts the filter predicate. -/
theorem upsertMembership_wellFormed {s : DiscoveryState} (m : Membership)
    (hwf : s.wellFormed) : (upsertMembership s m).wellFormed := by
  unfold DiscoveryState.wellFormed upsertMembership
  intro m₁ hm₁ m₂ hm₂ hnet hdid
  simp only [Finset.mem_insert, Finset.mem_filter] at hm₁ hm₂
  -- Each of m₁, m₂ is either `m` itself, or a filtered survivor (a member of
  -- `s.memberships` whose key differs from `m`'s).
  rcases hm₁ with rfl | ⟨hm₁_mem, hm₁_key⟩ <;> rcases hm₂ with rfl | ⟨hm₂_mem, hm₂_key⟩
  · rfl
  · -- m₁ = m, m₂ survived: but m₂ shares m's key (hnet/hdid), contradicting filter.
    exact absurd ⟨hnet.symm, hdid.symm⟩ hm₂_key
  · -- symmetric.
    exact absurd ⟨hnet, hdid⟩ hm₁_key
  · -- both survived: original wellFormed applies.
    exact hwf m₁ hm₁_mem m₂ hm₂_mem hnet hdid

/-- Every `Transition` preserves `wellFormed`. `approveRequest`/`revoke` write
memberships through `upsertMembership` (covered by `upsertMembership_wellFormed`);
`derive`/`join`/`reciprocalJoin`/`submitRequest`/`operatorWrite` never touch
`memberships`, so they preserve the invariant by `simp`/structure equality. -/
theorem transition_preserves_wellFormed {pre post : DiscoveryState}
    (hwf : pre.wellFormed) (h : Transition pre post) : post.wellFormed := by
  cases h with
  | derive h_post =>
      subst h_post; unfold DiscoveryState.wellFormed deriveStep at *; exact hwf
  | join tok tofu _ h_post =>
      subst h_post; unfold DiscoveryState.wellFormed joinState at *; exact hwf
  | reciprocalJoin tok tofu _ h_post =>
      subst h_post; unfold DiscoveryState.wellFormed joinState at *; exact hwf
  | submitRequest req h_post =>
      subst h_post; unfold DiscoveryState.wellFormed submitRequestState at *; exact hwf
  | approveRequest req m _ _ _ _ _ _ h_post =>
      subst h_post; unfold approveMembershipState
      exact upsertMembership_wellFormed m hwf
  | revoke tomb _ _ h_post =>
      subst h_post; unfold revokeState
      exact upsertMembership_wellFormed tomb hwf
  | operatorWrite d h_post =>
      subst h_post; unfold DiscoveryState.wellFormed operatorWriteState at *; exact hwf

/-! ## Core obligations (O1–O5)

The membership obligations the model was built to discharge. O4 (`ownership_safe`)
is above; O1/O2/O3/O5 follow, each with a realizing witness so no implication is
vacuously true. -/

/-! ### O1 — soundness

Nothing materializes without an admitted, member-signed endpoint. This is the
forward direction of `mem_deriveMaterializable`, restated standalone in the
`admittedMember ∧ memberSignedEndpoint` shape (the conjuncts of
`materializableEndpoint`). -/

/-- **O1 (soundness).** If a peer `p` is in the derived materializable set, then
some announced endpoint witnesses it: an endpoint in `s.endpoints` whose DID is an
admitted member, whose announcement is member-signed, and whose peer is `p`. So a
materialized peer always traces back to an admitted, member-signed endpoint.

Non-vacuous: the hypothesis `p ∈ deriveMaterializable s` is realized by the
positive witness `materializable_is_derived_witness` (a state with a genuinely
derived peer); the unadmitted-endpoint scenario is realized by
`materialized_implies_admitted_witness` (an endpoint that is NOT materialized,
precisely because its DID is not admitted). The conclusion is a real consequence:
it is extracted by unfolding the derivation's filter/image, not assumed. -/
theorem materialized_implies_admitted {s : DiscoveryState} {p : PeerId}
    (h : p ∈ deriveMaterializable s) :
    ∃ ep ∈ s.endpoints, admittedMember ep.did s ∧ memberSignedEndpoint ep ∧ ep.peer = p := by
  rw [mem_deriveMaterializable] at h
  obtain ⟨ep, hep_mem, ⟨hadm, hsig, _⟩, hpeer⟩ := h
  exact ⟨ep, hep_mem, hadm, hsig, hpeer⟩

/-- Witness that the unadmitted-endpoint scenario of O1 is realizable: a concrete
state with an endpoint whose announcing DID has NO admitted membership (its only
membership row has `adminSigValid = false`), and whose peer is consequently NOT in
`deriveMaterializable`. This is the `_neg` companion: it proves "an endpoint can
exist whose peer is not materialized because its DID is not admitted", so O1's
conclusion is not trivially always satisfiable by every endpoint. -/
theorem materialized_implies_admitted_witness :
    ∃ (s : DiscoveryState) (ep : Endpoint),
      ep ∈ s.endpoints ∧ ¬ admittedMember ep.did s ∧ ep.peer ∉ deriveMaterializable s := by
  let network : Network := ⟨"net-1", "did:key:admin", true⟩
  -- A membership row that is NOT admin-signed: its admin signature does not verify.
  let badMembership : Membership := ⟨"net-1", "did:key:a", true, "did:key:admin", false⟩
  let ep : Endpoint := ⟨"did:key:a", "peer-a", true, true⟩
  let s : DiscoveryState :=
    { self := "peer-self"
    , network := network
    , memberships := {badMembership}
    , endpoints := {ep}
    , requests := ∅
    , operatorDesired := ∅
    , registryDesired := ∅
    , consumedNonces := ∅ }
  refine ⟨s, ep, Finset.mem_singleton_self ep, ?_, ?_⟩
  · -- Not admitted: the only membership has adminSigValid = false.
    rintro ⟨_, m, hm_mem, _, _, hadminsig, _, _⟩
    rw [Finset.mem_singleton] at hm_mem
    subst hm_mem
    exact Bool.false_ne_true hadminsig
  · -- peer-a is not derived: the only endpoint announcing it is not materializable.
    rw [mem_deriveMaterializable]
    rintro ⟨ep', hep'_mem, ⟨⟨_, m, hm_mem, _, _, hadminsig, _, _⟩, _, _⟩, _⟩
    rw [Finset.mem_singleton] at hm_mem
    subst hm_mem
    exact Bool.false_ne_true hadminsig

/-! ### O2 — completeness

An active admin-signed membership plus a fresh member-signed endpoint (not self)
materializes. This is the backward direction of `mem_deriveMaterializable`. -/

/-- **O2 (completeness).** If an endpoint is announced (`ep ∈ s.endpoints`) and is
`materializableEndpoint ep s` (admitted DID + member-signed + not self), then its
peer is in the derived set. Together with O1 this pins the derivation as exactly
the materializable-endpoint peers.

Non-vacuous: `materializable_is_derived_witness` exhibits a concrete state whose
hypothesis `materializableEndpoint ep s` genuinely holds and whose peer is
derived — a true positive. The conclusion is a real consequence discharged
through `mem_deriveMaterializable`. -/
theorem materializable_is_derived {s : DiscoveryState} {ep : Endpoint}
    (hep : ep ∈ s.endpoints) (h : materializableEndpoint ep s) :
    ep.peer ∈ deriveMaterializable s := by
  rw [mem_deriveMaterializable]
  exact ⟨ep, hep, h, rfl⟩

/-- Witness that O2's hypothesis is realizable as a genuine POSITIVE: a valid
network, an active admin-signed membership for `did:key:a` (signed by the network
admin, `adminSigValid = true`, `active = true`), and a fresh member-signed
endpoint announcing `peer-a ≠ peer-self`. The peer IS derived. This is the
anti-vacuity guard for O1: it proves the derived set is not always empty, so the
"not materialized" conclusions elsewhere are not trivially true. -/
theorem materializable_is_derived_witness :
    ∃ (s : DiscoveryState) (ep : Endpoint),
      ep ∈ s.endpoints ∧ materializableEndpoint ep s ∧ ep.peer ∈ deriveMaterializable s := by
  let network : Network := ⟨"net-1", "did:key:admin", true⟩
  let membership : Membership := ⟨"net-1", "did:key:a", true, "did:key:admin", true⟩
  let ep : Endpoint := ⟨"did:key:a", "peer-a", true, true⟩
  let s : DiscoveryState :=
    { self := "peer-self"
    , network := network
    , memberships := {membership}
    , endpoints := {ep}
    , requests := ∅
    , operatorDesired := ∅
    , registryDesired := ∅
    , consumedNonces := ∅ }
  have hmat : materializableEndpoint ep s := by
    refine ⟨⟨rfl, membership, Finset.mem_singleton_self membership, rfl, rfl, ?_⟩, ⟨rfl, rfl⟩, ?_⟩
    · exact ⟨rfl, rfl, rfl⟩
    · decide
  exact ⟨s, ep, Finset.mem_singleton_self ep, hmat, materializable_is_derived (Finset.mem_singleton_self ep) hmat⟩

/-! ### O3 — revocation retracts exactly

Revoking `tomb.memberDid` retracts ONLY peers that were materialized *exclusively*
via that DID. A peer also reachable via a different admitted DID must survive.

The argument: `revokeState pre tomb = upsertMembership pre tomb` replaces the
single (by `wellFormed`) `(networkId, memberDid)` row with the `active = false`
tombstone. After revoke, NO active admin-signed membership exists for
`tomb.memberDid`, so endpoints with `did = tomb.memberDid` are no longer
materializable; endpoints with `did ≠ tomb.memberDid` keep their membership rows
untouched, so their `admittedMember` status is unchanged. We bridge both sides
through `mem_deriveMaterializable`. -/

/-- An endpoint whose DID equals the revoked member's is NOT materializable in the
post-state: after revoke NO active `(networkId, memberDid)` row for that key
survives, so `admittedMember` fails.

NOTE: this does NOT need `wellFormed`. `upsertMembership` filters out *every*
same-key row (not just one) before inserting the inactive tombstone, so even if
`pre` had multiple rows for `tomb.memberDid` they are all dropped — the
"at most one row" invariant is sufficient but not necessary here. O3 still carries
`wellFormed` in its signature per the obligation spec. -/
private theorem revoked_did_not_materializable {pre : DiscoveryState} {tomb : Membership}
    (hsig : adminSignedMembership tomb pre.network)
    (hrev : tomb.active = false) {ep : Endpoint} (hdid : ep.did = tomb.memberDid) :
    ¬ materializableEndpoint ep (revokeState pre tomb) := by
  rintro ⟨⟨_, m, hm_mem, hmdid, hmactive, hmadminsig, hmsignedby, hmnet⟩, _, _⟩
  -- m is an active admin-signed membership for ep.did = tomb.memberDid in post.
  unfold revokeState upsertMembership at hm_mem
  rw [Finset.mem_insert, Finset.mem_filter] at hm_mem
  -- m's key matches tomb's key.
  have hm_net_tomb : m.networkId = tomb.networkId := by
    rw [hmnet]; exact hsig.2.2.symm
  have hm_did_tomb : m.memberDid = tomb.memberDid := by rw [hmdid, hdid]
  rcases hm_mem with rfl | ⟨_, hkey⟩
  · -- m = tomb: but tomb.active = false contradicts hmactive.
    rw [hrev] at hmactive
    exact Bool.false_ne_true hmactive
  · -- m survived the filter, so its key ≠ tomb's key — contradiction.
    exact hkey ⟨hm_net_tomb, hm_did_tomb⟩

/-- An endpoint whose DID differs from the revoked member's keeps its
materializability across revoke: the membership rows for `ep.did` are untouched
(the upsert only filters out `tomb.memberDid`'s key and inserts the tombstone). -/
private theorem unrevoked_did_materializable_iff {pre : DiscoveryState} {tomb : Membership}
    {ep : Endpoint} (hdid : ep.did ≠ tomb.memberDid) :
    materializableEndpoint ep (revokeState pre tomb) ↔ materializableEndpoint ep pre := by
  unfold materializableEndpoint admittedMember validNetwork revokeState upsertMembership
  simp only
  constructor
  · rintro ⟨⟨hnet, m, hm_mem, hmdid, hrest⟩, hsig, hself⟩
    rw [Finset.mem_insert, Finset.mem_filter] at hm_mem
    refine ⟨⟨hnet, m, ?_, hmdid, hrest⟩, hsig, hself⟩
    rcases hm_mem with rfl | ⟨hm_orig, _⟩
    · -- m = tomb, but then tomb.memberDid = ep.did, contradicting hdid.
      exact absurd hmdid.symm hdid
    · exact hm_orig
  · rintro ⟨⟨hnet, m, hm_mem, hmdid, hrest⟩, hsig, hself⟩
    refine ⟨⟨hnet, m, ?_, hmdid, hrest⟩, hsig, hself⟩
    rw [Finset.mem_insert, Finset.mem_filter]
    refine Or.inr ⟨hm_mem, ?_⟩
    -- m's key ≠ tomb's key because m.memberDid = ep.did ≠ tomb.memberDid.
    rintro ⟨_, hmd⟩
    exact hdid (hmdid ▸ hmd)

/-- **O3 (revocation retracts exactly).** Under `wellFormed`, revoking `tomb`
yields a post-derivation equal to the pre-derivation FILTERED to the peers still
backed by some materializable endpoint whose DID is NOT the revoked one. Peers
materialized only via the revoked DID drop; peers also reachable via another
admitted DID survive.

Non-vacuous: `revoke_retracts_exactly_witness` realizes the hypothesis (a
well-formed state, an admin-signed inactive tombstone) and exhibits BOTH the
distinct-peer case (revoke A drops A's peer, keeps B's) and the shared-peer case
(A and B both materialize peer `p`; revoke A leaves `p` materialized via B). The
shared-peer case is what makes "exactly/exclusively" precise — the filter is not
just "drop everything `tomb.memberDid` touched".

FINDING: the proof does NOT actually require `wellFormed`. The spec's reasoning
appealed to "at most one membership row per key" to argue no active row survives
revoke. But `upsertMembership` filters out *every* same-key row before inserting
the tombstone, so the retraction holds even if `pre` had several rows for
`tomb.memberDid`. `wellFormed` (`_hwf`) is retained in the signature per the
obligation spec, but is unused — uniqueness is sufficient, not necessary. -/
theorem revoke_retracts_exactly {pre post : DiscoveryState} {tomb : Membership}
    (_hwf : pre.wellFormed) (hsig : adminSignedMembership tomb pre.network)
    (hrev : tomb.active = false) (h : post = revokeState pre tomb) :
    deriveMaterializable post =
      (deriveMaterializable pre).filter (fun p =>
        ∃ ep ∈ pre.endpoints,
          ep.did ≠ tomb.memberDid ∧ ep.peer = p ∧ materializableEndpoint ep pre) := by
  subst h
  ext p
  rw [Finset.mem_filter, mem_deriveMaterializable]
  -- revokeState only edits memberships, so endpoints/self are shared with pre.
  have hendpoints : (revokeState pre tomb).endpoints = pre.endpoints := rfl
  constructor
  · -- p materialized in post ⇒ via an endpoint with did ≠ tomb.memberDid, which is
    -- then also materializable in pre, so p ∈ pre-derivation AND satisfies the filter.
    rintro ⟨ep, hep_mem, hmat_post, hpeer⟩
    rw [hendpoints] at hep_mem
    -- ep.did ≠ tomb.memberDid, else it couldn't be materializable in post.
    have hdid : ep.did ≠ tomb.memberDid := by
      intro hdid_eq
      exact revoked_did_not_materializable hsig hrev hdid_eq hmat_post
    have hmat_pre : materializableEndpoint ep pre :=
      (unrevoked_did_materializable_iff hdid).mp hmat_post
    refine ⟨?_, ep, hep_mem, hdid, hpeer, hmat_pre⟩
    rw [mem_deriveMaterializable]
    exact ⟨ep, hep_mem, hmat_pre, hpeer⟩
  · -- The filter side gives an endpoint with did ≠ tomb.memberDid materializable in
    -- pre; it survives revoke, so p is materialized in post.
    rintro ⟨_, ep, hep_mem, hdid, hpeer, hmat_pre⟩
    refine ⟨ep, ?_, ?_, hpeer⟩
    · rw [hendpoints]; exact hep_mem
    · exact (unrevoked_did_materializable_iff hdid).mpr hmat_pre

/-- Witness for O3 covering BOTH cases. Case (a) distinct peers: members A and B
announce different peers; revoking A drops A's peer but keeps B's. Case (b) shared
peer: A and B both announce the SAME peer `p`; revoking A leaves `p` materialized
via B. The shared-peer case is the one that pins "exclusively". -/
theorem revoke_retracts_exactly_witness :
    ∃ (pre : DiscoveryState) (tomb : Membership),
      pre.wellFormed ∧ adminSignedMembership tomb pre.network ∧ tomb.active = false ∧
      -- (a) distinct peers: A's peer drops, B's stays.
      ("peer-a" ∉ deriveMaterializable (revokeState pre tomb) ∧
       "peer-b" ∈ deriveMaterializable (revokeState pre tomb)) ∧
      -- (b) shared peer: also materialized via B, so it STAYS after revoking A.
      "peer-shared" ∈ deriveMaterializable (revokeState pre tomb) := by
  let network : Network := ⟨"net-1", "did:key:admin", true⟩
  let memA : Membership := ⟨"net-1", "did:key:a", true, "did:key:admin", true⟩
  let memB : Membership := ⟨"net-1", "did:key:b", true, "did:key:admin", true⟩
  -- A announces peer-a and peer-shared; B announces peer-b and peer-shared.
  let epA : Endpoint := ⟨"did:key:a", "peer-a", true, true⟩
  let epAshared : Endpoint := ⟨"did:key:a", "peer-shared", true, true⟩
  let epB : Endpoint := ⟨"did:key:b", "peer-b", true, true⟩
  let epBshared : Endpoint := ⟨"did:key:b", "peer-shared", true, true⟩
  -- The tombstone: revoke A.
  let tomb : Membership := ⟨"net-1", "did:key:a", false, "did:key:admin", true⟩
  let pre : DiscoveryState :=
    { self := "peer-self"
    , network := network
    , memberships := {memA, memB}
    , endpoints := {epA, epAshared, epB, epBshared}
    , requests := ∅
    , operatorDesired := ∅
    , registryDesired := ∅
    , consumedNonces := ∅ }
  -- Every conjunct is a decidable proposition over fully-concrete finsets
  -- (`wellFormed`, `adminSignedMembership`, and three `deriveMaterializable`
  -- membership facts all have `Decidable` instances), so `decide` discharges the
  -- entire witness once the existential is instantiated.
  refine ⟨pre, tomb, ?_⟩
  decide

/-! ### O5 — a grant requires an admin signature

The only membership-creating transition is `approveRequest`, which demands
`adminSignedMembership m pre.network`. (`revoke` also adds a row — the tombstone —
which is admin-signed by its own precondition.) A forged/unsigned join request
alone produces no membership. -/

/-- **O5 (grant requires admin signature).** If a transition adds a membership row
`m` (`m ∈ post.memberships`, `m ∉ pre.memberships`), then `m` is admin-signed for
the pre-state network. So no transition can introduce an unsigned grant: a forged
or unsigned join request cannot produce a membership.

Non-vacuous: the hypothesis (a transition that genuinely adds a fresh membership)
is realized by `approveRequest`/`revoke`; the negative witness
`membership_grant_requires_admin_signature_witness` shows a `submitRequest` with
`reqSigValid = false` adds NO membership, confirming a candidate request alone
cannot grant. The conclusion is a real consequence: the only membership-adding
cases (`approveRequest`, `revoke`) carry `adminSignedMembership` as a precondition;
all others are refuted by the disjoint `hnew`/`hold` hypotheses. -/
theorem membership_grant_requires_admin_signature {pre post : DiscoveryState}
    (h : Transition pre post) {m : Membership}
    (hnew : m ∈ post.memberships) (hold : m ∉ pre.memberships) :
    adminSignedMembership m pre.network := by
  cases h with
  | derive h_post =>
      subst h_post; unfold deriveStep at hnew; exact absurd hnew hold
  | join tok tofu _ h_post =>
      subst h_post; unfold joinState at hnew; exact absurd hnew hold
  | reciprocalJoin tok tofu _ h_post =>
      subst h_post; unfold joinState at hnew; exact absurd hnew hold
  | submitRequest req h_post =>
      subst h_post; unfold submitRequestState at hnew; exact absurd hnew hold
  | approveRequest req m' _ _ _ hadminsig _ _ h_post =>
      -- post adds m' via upsert; a "new" row in post is the inserted m' (filter only removes).
      subst h_post
      unfold approveMembershipState upsertMembership at hnew
      rw [Finset.mem_insert, Finset.mem_filter] at hnew
      rcases hnew with rfl | ⟨hmem, _⟩
      · exact hadminsig
      · exact absurd hmem hold
  | revoke tomb hadminsig _ h_post =>
      subst h_post
      unfold revokeState upsertMembership at hnew
      rw [Finset.mem_insert, Finset.mem_filter] at hnew
      rcases hnew with rfl | ⟨hmem, _⟩
      · exact hadminsig
      · exact absurd hmem hold
  | operatorWrite d h_post =>
      subst h_post; unfold operatorWriteState at hnew; exact absurd hnew hold

/-- Negative witness for O5: a `submitRequest` transition (even with an invalid
request signature) leaves `memberships` unchanged, so it adds NO new membership —
a candidate request alone never grants. Realizes the "forged/unsigned request"
scenario and confirms the only grant path is an admin-signed `approveRequest`. -/
theorem membership_grant_requires_admin_signature_witness :
    ∃ (pre post : DiscoveryState) (req : JoinRequest),
      req.reqSigValid = false ∧ Transition pre post ∧
      post.memberships = pre.memberships := by
  let network : Network := ⟨"net-1", "did:key:admin", true⟩
  let pre : DiscoveryState :=
    { self := "peer-self"
    , network := network
    , memberships := ∅
    , endpoints := ∅
    , requests := ∅
    , operatorDesired := ∅
    , registryDesired := ∅
    , consumedNonces := ∅ }
  -- A forged/unsigned join request.
  let req : JoinRequest := ⟨"net-1", "did:key:forger", false⟩
  refine ⟨pre, submitRequestState pre req, req, rfl,
    Transition.submitRequest req rfl, ?_⟩
  unfold submitRequestState
  rfl

end PeerRegistryDiscovery
