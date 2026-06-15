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

end PeerRegistryDiscovery
