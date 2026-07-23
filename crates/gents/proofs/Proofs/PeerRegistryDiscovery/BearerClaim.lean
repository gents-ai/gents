import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image
import Mathlib.Data.Finset.Prod

/-!
# Bearer-claim pairing admission

Model for the claim-at-join bearer invite (issue #666). A bearer invite token
is audience-unbound at mint: it carries the issuer's signature, a single-use
nonce, and a scope template, but no membership grant. A claiming device binds
itself at claim time by presenting the token together with its own DID and a
signature over the claim.

The authority (the invite issuer's node) processes claims. Admission requires
BOTH signatures (the issuer's over the token and the claimant's over the
claim), token freshness, and that the nonce is not already bound to a
*different* claimant. An admitted claim atomically — in one step — binds the
nonce to the claimant, mints the membership, and (for `conversation` tokens
only) records the reciprocal conversation intent consumed by the
`ReciprocalConversation` derivation.

Key differences from the v5 DID-bound join (`Transition.lean`,
`NetworkMembership.lean`):

- The nonce ledger lives on the *authority*, not the joiner: bearer tokens are
  exactly the case where two distinct devices can race one nonce, so
  single-use must be decided where grants are minted.
- The consumed nonce is bound to the admitting claimant, `(nonce, did)`, not
  just the nonce. Re-processing the same claim is then admissible-but-idempotent
  (crash-safe convergence: a re-sweep repairs a partially applied claim), while
  a second claimant on the same nonce is rejected.
- Membership growth still requires the authority's signature — the claim row
  itself grants nothing (mirrors `join_request_grants_nothing`); here that is
  structural: only `claimStep` on an admitted claim grows `memberships`, and
  admission requires `tokenAuthoritySigned`.
-/

namespace PeerRegistryDiscovery
namespace BearerClaim

abbrev Did := String
abbrev Nonce := String

/-- The authority-relevant content of a bearer invite token. Signature validity
and freshness are abstracted to booleans the way `PeerEndpoint.live` is in
`ReciprocalConversation.lean`: the Rust seam verifies the issuer signature and
the bearer freshness window and hands the model the verdicts. -/
structure BearerToken where
  nonce : Nonce
  template : String
  /-- The issuer's signature over the token verifies under the authority DID. -/
  authoritySigned : Bool
  /-- `issued_at` is within the bearer replay window at claim-processing time. -/
  fresh : Bool
  deriving DecidableEq, Repr

/-- A claim presented to the authority: the token plus the claimant's identity
and signature verdict over the claim payload. -/
structure Claim where
  token : BearerToken
  claimant : Did
  claimantSigned : Bool
  deriving DecidableEq, Repr

/-- Authority-side claim-processing state. `operatorMemberships` is the
operator-authored partition, carried to make ownership safety structural, the
same way `ReciprocalState` partitions desired rows. -/
structure ClaimState where
  /-- Burned nonces, each bound to the claimant it admitted. -/
  consumed : Finset (Nonce × Did)
  /-- Bearer-minted memberships (authority-signed by construction of `claimStep`). -/
  memberships : Finset Did
  /-- Reciprocal conversation intents recorded at claim time. -/
  intents : Finset Did
  /-- Operator-authored memberships; never touched by claim processing. -/
  operatorMemberships : Finset Did
  deriving DecidableEq

/-- The nonce is already bound to a different claimant. -/
def nonceBoundElsewhere (s : ClaimState) (c : Claim) : Prop :=
  ∃ p ∈ s.consumed, p.1 = c.token.nonce ∧ p.2 ≠ c.claimant

instance (s : ClaimState) (c : Claim) : Decidable (nonceBoundElsewhere s c) := by
  unfold nonceBoundElsewhere
  infer_instance

/-- Bearer-claim admission: both signatures, freshness, and the nonce not bound
to another claimant. Binding to the *same* claimant stays admissible so that
re-processing a claim after a partial apply converges instead of wedging. -/
def admits (s : ClaimState) (c : Claim) : Prop :=
  c.token.authoritySigned = true ∧
  c.token.fresh = true ∧
  c.claimantSigned = true ∧
  ¬ nonceBoundElsewhere s c

instance (s : ClaimState) (c : Claim) : Decidable (admits s c) := by
  unfold admits
  infer_instance

/-- Template class whose claims record a reciprocal conversation intent.
`conversation` is the 1:1 chat pairing; `machine` is the fleet-discovery
pairing (issue #714) — identical claim consequences, wider replicated
collection set (decided below the model's abstraction). Bool, not Prop, so
`claimStep` stays a plain `if` and existing case splits keep working. -/
def conversationLike (template : String) : Bool :=
  template = "conversation" || template = "machine"

/-- Atomic claim processing: an admitted claim binds the nonce, mints the
membership, and records the conversation intent when (and only when) the token
template is conversation-like (`conversation` or `machine`). A rejected claim
changes nothing. -/
def claimStep (s : ClaimState) (c : Claim) : ClaimState :=
  if admits s c then
    { s with
        consumed := insert (c.token.nonce, c.claimant) s.consumed,
        memberships := insert c.claimant s.memberships,
        intents :=
          if conversationLike c.token.template then insert c.claimant s.intents
          else s.intents }
  else s

/-! ## (1) No forgery: both signatures are required -/

/-- A claim whose token does not verify under the authority DID grants
nothing. -/
theorem unsigned_token_grants_nothing (s : ClaimState) (c : Claim)
    (h : c.token.authoritySigned = false) : claimStep s c = s := by
  unfold claimStep
  rw [if_neg]
  intro ⟨hsig, _, _, _⟩
  rw [h] at hsig
  exact Bool.noConfusion hsig

/-- A claim the claimant did not sign grants nothing: the claim row is not
self-authorizing (mirrors `join_request_grants_nothing`). -/
theorem unsigned_claim_grants_nothing (s : ClaimState) (c : Claim)
    (h : c.claimantSigned = false) : claimStep s c = s := by
  unfold claimStep
  rw [if_neg]
  intro ⟨_, _, hsig, _⟩
  rw [h] at hsig
  exact Bool.noConfusion hsig

/-- A stale token (outside the bearer replay window) grants nothing. -/
theorem stale_token_grants_nothing (s : ClaimState) (c : Claim)
    (h : c.token.fresh = false) : claimStep s c = s := by
  unfold claimStep
  rw [if_neg]
  intro ⟨_, hfresh, _, _⟩
  rw [h] at hfresh
  exact Bool.noConfusion hfresh

/-! ## (2) Single use across claimants -/

/-- An admitted claim binds its nonce: the pair `(nonce, claimant)` is consumed
in the post state. -/
theorem claimStep_binds_nonce {s : ClaimState} {c : Claim} (h : admits s c) :
    (c.token.nonce, c.claimant) ∈ (claimStep s c).consumed := by
  unfold claimStep
  rw [if_pos h]
  exact Finset.mem_insert_self _ _

/-- Replay by a different claimant is rejected: once a claim is admitted, any
claim presenting the same nonce under a different DID fails admission in every
state reachable by processing it. -/
theorem replay_rejected_across_claimants {s : ClaimState} {c c' : Claim}
    (hadm : admits s c)
    (hnonce : c'.token.nonce = c.token.nonce)
    (hdid : c'.claimant ≠ c.claimant) :
    ¬ admits (claimStep s c) c' := by
  intro ⟨_, _, _, hnotbound⟩
  exact hnotbound
    ⟨(c.token.nonce, c.claimant), claimStep_binds_nonce hadm,
      by simp [hnonce], by simpa using hdid.symm⟩

/-! ## (3) Idempotence: re-processing an admitted claim converges -/

/-- Binding the nonce to the *same* claimant does not defeat admission: the
claim stays admissible after its own application. This is what makes a
crash between the nonce burn and the grant write recoverable — the next sweep
re-admits the same claim and the inserts below repair the missing rows. -/
theorem claimStep_readmits_same_claim {s : ClaimState} {c : Claim}
    (h : admits s c) : admits (claimStep s c) c := by
  obtain ⟨hsig, hfresh, hcsig, hnotbound⟩ := h
  refine ⟨hsig, hfresh, hcsig, ?_⟩
  intro ⟨p, hp, hpn, hpd⟩
  unfold claimStep at hp
  rw [if_pos ⟨hsig, hfresh, hcsig, hnotbound⟩] at hp
  simp only [Finset.mem_insert] at hp
  cases hp with
  | inl heq =>
      apply hpd
      rw [heq]
  | inr hmem => exact hnotbound ⟨p, hmem, hpn, hpd⟩

/-- Processing the same claim twice equals processing it once. -/
theorem claimStep_idempotent (s : ClaimState) (c : Claim) :
    claimStep (claimStep s c) c = claimStep s c := by
  by_cases h : admits s c
  · have h' := claimStep_readmits_same_claim h
    unfold claimStep
    rw [if_pos h]
    rw [if_pos] <;> [skip; exact (by unfold claimStep at h'; rwa [if_pos h] at h')]
    by_cases htmpl : conversationLike c.token.template = true <;>
      simp [htmpl, Finset.insert_idem]
  · unfold claimStep
    rw [if_neg h, if_neg h]

/-! ## (4) Binding soundness: the grant is exactly for the presented claimant -/

/-- An admitted claim mints a membership for exactly the claiming DID (plus
what was already there): bind-at-claim binds to the presenter of the valid
claimant signature, nothing else. -/
theorem claimStep_binding_sound {s : ClaimState} {c : Claim} (h : admits s c) :
    (claimStep s c).memberships = insert c.claimant s.memberships := by
  unfold claimStep
  rw [if_pos h]

/-- The conversation intent is recorded iff the token's template is
conversation-like (`conversation` or `machine`) — a bearer network-control
claim never creates a conversation edge (fleet no-crosswise invariant holds
through the bearer path). -/
theorem claimStep_intent_iff_conversation_like {s : ClaimState} {c : Claim}
    (h : admits s c) :
    (claimStep s c).intents =
      (if conversationLike c.token.template then insert c.claimant s.intents
       else s.intents) := by
  unfold claimStep
  rw [if_pos h]

/-- A machine-template claim records the intent (fleet discovery pairing). -/
theorem machine_claim_records_intent {s : ClaimState} {c : Claim}
    (hadm : admits s c) (htmpl : c.token.template = "machine") :
    c.claimant ∈ (claimStep s c).intents := by
  unfold claimStep
  simp [hadm, htmpl, conversationLike]

/-- A network-control claim still never wires a conversation edge. -/
theorem network_control_claim_records_no_intent {s : ClaimState} {c : Claim}
    (htmpl : c.token.template = "network-control")
    (hni : c.claimant ∉ s.intents) :
    c.claimant ∉ (claimStep s c).intents := by
  unfold claimStep
  by_cases hadm : admits s c <;> simp [hadm, htmpl, conversationLike, hni]

/-! ## (5) Ownership safety -/

/-- Claim processing never mutates operator-authored memberships. -/
theorem claimStep_ownership_safe (s : ClaimState) (c : Claim) :
    (claimStep s c).operatorMemberships = s.operatorMemberships := by
  unfold claimStep
  by_cases h : admits s c
  · rw [if_pos h]
  · rw [if_neg h]

/-! ## (6) Membership growth requires admission -/

/-- The only way `claimStep` grows memberships is an admitted claim, and the
only DID it can add is the claimant: any DID in the post-memberships is either
pre-existing or is the claimant of an admitted claim. -/
theorem membership_growth_requires_admission {s : ClaimState} {c : Claim}
    {d : Did} (hmem : d ∈ (claimStep s c).memberships) :
    d ∈ s.memberships ∨ (admits s c ∧ d = c.claimant) := by
  unfold claimStep at hmem
  by_cases h : admits s c
  · rw [if_pos h] at hmem
    simp only [Finset.mem_insert] at hmem
    cases hmem with
    | inl heq => exact Or.inr ⟨h, heq⟩
    | inr hpre => exact Or.inl hpre
  · rw [if_neg h] at hmem
    exact Or.inl hmem

end BearerClaim
end PeerRegistryDiscovery
