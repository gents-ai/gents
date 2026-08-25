import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image
import Mathlib.Data.Finset.Prod

namespace PeerRegistryDiscovery
namespace BearerClaim

abbrev Did := String
abbrev Nonce := String

structure BearerToken where
  nonce : Nonce
  template : String
  priority : Nat
  authoritySigned : Bool
  fresh : Bool
  deriving DecidableEq, Repr

structure Claim where
  token : BearerToken
  claimant : Did
  claimantSigned : Bool
  deriving DecidableEq, Repr

structure ClaimState where
  consumed : Finset (Nonce × Did)
  memberships : Finset Did
  intents : Finset Did
  operatorMemberships : Finset Did
  deriving DecidableEq

def nonceBoundElsewhere (s : ClaimState) (c : Claim) : Prop :=
  ∃ p ∈ s.consumed, p.1 = c.token.nonce ∧ p.2 ≠ c.claimant

instance (s : ClaimState) (c : Claim) : Decidable (nonceBoundElsewhere s c) := by
  unfold nonceBoundElsewhere
  infer_instance

def admits (s : ClaimState) (c : Claim) : Prop :=
  c.token.authoritySigned = true ∧
  c.token.fresh = true ∧
  c.claimantSigned = true ∧
  ¬ nonceBoundElsewhere s c

instance (s : ClaimState) (c : Claim) : Decidable (admits s c) := by
  unfold admits
  infer_instance

def conversationLike (template : String) : Bool :=
  template = "conversation" || template = "machine" || template = "client"

def preferredClaim (current candidate : Claim) : Claim :=
  if current.token.priority < candidate.token.priority then candidate else current

theorem preferredClaim_newer {current candidate : Claim}
    (h : current.token.priority < candidate.token.priority) :
    preferredClaim current candidate = candidate := by
  simp [preferredClaim, h]

theorem preferredClaim_older_or_equal {current candidate : Claim}
    (h : candidate.token.priority ≤ current.token.priority) :
    preferredClaim current candidate = current := by
  simp [preferredClaim, Nat.not_lt.mpr h]

def claimStep (s : ClaimState) (c : Claim) : ClaimState :=
  if admits s c then
    { s with
        consumed := insert (c.token.nonce, c.claimant) s.consumed,
        memberships := insert c.claimant s.memberships,
        intents :=
          if conversationLike c.token.template then insert c.claimant s.intents
          else s.intents }
  else s

theorem unsigned_token_grants_nothing (s : ClaimState) (c : Claim)
    (h : c.token.authoritySigned = false) : claimStep s c = s := by
  unfold claimStep
  rw [if_neg]
  intro ⟨hsig, _, _, _⟩
  rw [h] at hsig
  exact Bool.noConfusion hsig

theorem unsigned_claim_grants_nothing (s : ClaimState) (c : Claim)
    (h : c.claimantSigned = false) : claimStep s c = s := by
  unfold claimStep
  rw [if_neg]
  intro ⟨_, _, hsig, _⟩
  rw [h] at hsig
  exact Bool.noConfusion hsig

theorem stale_token_grants_nothing (s : ClaimState) (c : Claim)
    (h : c.token.fresh = false) : claimStep s c = s := by
  unfold claimStep
  rw [if_neg]
  intro ⟨_, hfresh, _, _⟩
  rw [h] at hfresh
  exact Bool.noConfusion hfresh

theorem claimStep_binds_nonce {s : ClaimState} {c : Claim} (h : admits s c) :
    (c.token.nonce, c.claimant) ∈ (claimStep s c).consumed := by
  unfold claimStep
  rw [if_pos h]
  exact Finset.mem_insert_self _ _

theorem replay_rejected_across_claimants {s : ClaimState} {c c' : Claim}
    (hadm : admits s c)
    (hnonce : c'.token.nonce = c.token.nonce)
    (hdid : c'.claimant ≠ c.claimant) :
    ¬ admits (claimStep s c) c' := by
  intro ⟨_, _, _, hnotbound⟩
  exact hnotbound
    ⟨(c.token.nonce, c.claimant), claimStep_binds_nonce hadm,
      by simp [hnonce], by simpa using hdid.symm⟩

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

theorem claimStep_binding_sound {s : ClaimState} {c : Claim} (h : admits s c) :
    (claimStep s c).memberships = insert c.claimant s.memberships := by
  unfold claimStep
  rw [if_pos h]

theorem claimStep_intent_iff_conversation_like {s : ClaimState} {c : Claim}
    (h : admits s c) :
    (claimStep s c).intents =
      (if conversationLike c.token.template then insert c.claimant s.intents
       else s.intents) := by
  unfold claimStep
  rw [if_pos h]

theorem machine_claim_records_intent {s : ClaimState} {c : Claim}
    (hadm : admits s c) (htmpl : c.token.template = "machine") :
    c.claimant ∈ (claimStep s c).intents := by
  unfold claimStep
  simp [hadm, htmpl, conversationLike]

theorem client_claim_records_intent {s : ClaimState} {c : Claim}
    (hadm : admits s c) (htmpl : c.token.template = "client") :
    c.claimant ∈ (claimStep s c).intents := by
  unfold claimStep
  simp [hadm, htmpl, conversationLike]

theorem network_control_claim_records_no_intent {s : ClaimState} {c : Claim}
    (htmpl : c.token.template = "network-control")
    (hni : c.claimant ∉ s.intents) :
    c.claimant ∉ (claimStep s c).intents := by
  unfold claimStep
  by_cases hadm : admits s c <;> simp [hadm, htmpl, conversationLike, hni]

theorem claimStep_ownership_safe (s : ClaimState) (c : Claim) :
    (claimStep s c).operatorMemberships = s.operatorMemberships := by
  unfold claimStep
  by_cases h : admits s c
  · rw [if_pos h]
  · rw [if_neg h]

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
