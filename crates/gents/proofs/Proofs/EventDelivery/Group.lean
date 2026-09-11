import Proofs.Triggers.Groups
import Proofs.Callback.Properties

/-! One durable event-group clock serves both consumer kinds. Callback input
lives on the existing invocation, whose transitions preserve the captured value.
Projected input is opaque here; projection/JSON encoding has its existing owner. -/
namespace EventDelivery.Group
open Triggers.Groups

/-- Durable row projection. The identity carries owner, typed consumer,
configuration and correlation; firstSeen is never rewritten. -/
structure EventGroupState where
  key : EventGroupKey
  firstSeen : Nat
  quiescedAt : Option Nat := none
  deriving DecidableEq, Repr

def EventGroupState.quiesce (s : EventGroupState) (now : Nat) : EventGroupState :=
  if s.quiescedAt.isSome then s else { s with quiescedAt := some now }

/-- Reuse the existing timeout/cache observation instead of another clock. -/
def EventGroupState.observe (s : EventGroupState) (cached : Option Nat) : TimeoutObservation :=
  ⟨s.firstSeen, cached⟩

theorem quiesce_preserves_identity_and_deadline (s : EventGroupState) (now : Nat)
    (cached : Option Nat) :
    (s.quiesce now).key = s.key ∧
      (s.quiesce now).observe cached = s.observe cached := by
  unfold EventGroupState.quiesce
  split <;> simp_all [EventGroupState.observe]

theorem quiesce_once (s : EventGroupState) (first later : Nat) :
    (s.quiesce first).quiesce later = s.quiesce first := by
  cases h : s.quiescedAt <;> simp [EventGroupState.quiesce, h]

/-- Capture the caller's already ordered/projected input on the existing callback
invocation. Quiescence marks an invalid group, not sealed input; it prevents
delivery even if current membership would otherwise be eligible. -/
def captureCallback (group : EventGroupState) (candidate : Candidate)
    (invocationId input : String) : Option CallbackInvocation :=
  if group.quiescedAt.isNone && candidate.eligible && decide (candidate.key = group.key) then
    match group.key.consumer with
    | .trigger _ => none
    | .callbackBinding _ => some
        { invocationId := invocationId, ownerAgentDid := group.key.agentDid,
          input := input, originGroupKey := some group.key,
          state := .pending, journal := [], resultEmitted := false }
  else none

theorem capture_binds_eligible_group_and_input (group : EventGroupState)
    (candidate : Candidate) (id input : String) (inv : CallbackInvocation)
    (h : captureCallback group candidate id input = some inv) :
    group.quiescedAt = none ∧ candidate.eligible = true ∧ candidate.key = group.key ∧
      inv.ownerAgentDid = group.key.agentDid ∧ inv.originGroupKey = some group.key ∧
      inv.input = input := by
  unfold captureCallback at h
  split at h
  · rename_i guard
    have hg : group.quiescedAt = none ∧ candidate.eligible = true ∧
        candidate.key = group.key := by simpa [Bool.and_assoc] using guard
    split at h
    · contradiction
    · have he := Option.some.inj h
      subst inv
      exact ⟨hg.1, hg.2.1, hg.2.2, rfl, rfl, rfl⟩
  · contradiction

/-- All existing lifecycle continuations keep the sealed payload and origin;
changing source documents is not an input to any transition. -/
theorem captured_input_survives_execution {pre post : CallbackInvocation}
    (h : Relation.ReflTransGen CallbackInvocation.Transition pre post) :
    post.input = pre.input ∧ post.originGroupKey = pre.originGroupKey := by
  induction h with
  | refl => exact ⟨rfl, rfl⟩
  | @tail mid post _ step ih =>
      have hp := CallbackInvocation.frozen_input_preserved step
      exact ⟨hp.1.trans ih.1, hp.2.trans ih.2⟩

end EventDelivery.Group
