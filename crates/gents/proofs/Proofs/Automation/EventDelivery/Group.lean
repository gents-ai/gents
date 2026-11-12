import Proofs.Automation.Triggers.Groups
import Proofs.Automation.Callback.Properties

namespace EventDelivery.Group
open Triggers.Groups

structure EventGroupState where
  key : EventGroupKey
  firstSeen : Nat
  quiescedAt : Option Nat := none
  deriving DecidableEq, Repr

def EventGroupState.quiesce (s : EventGroupState) (now : Nat) : EventGroupState :=
  if s.quiescedAt.isSome then s else { s with quiescedAt := some now }

inductive QuiesceReason where
  | exceedsExpectedCount
  deriving DecidableEq, Repr

/-- A group whose membership exceeds its expected count or the hard document
cap can never become exactly complete, so it is quiesced durably instead of
waiting. -/
def quiesceReason (members expected : Nat) : Option QuiesceReason :=
  if maxGroupDocs < members || expected < members then some .exceedsExpectedCount else none

def EventGroupState.evaluate (s : EventGroupState) (members expected now : Nat) :
    EventGroupState :=
  if (quiesceReason members expected).isSome then s.quiesce now else s

/-- Startup recovery prunes quiesced groups before reading their membership. -/
def membershipReads : Option EventGroupState → Nat
  | none => 1
  | some s => if s.quiescedAt.isSome then 0 else 1

theorem invalid_group_is_durably_quiesced_and_pruned_after_restart
    (s : EventGroupState) (members expected now : Nat) (h : expected < members) :
    (s.evaluate members expected now).quiescedAt.isSome ∧
      quiesceReason members expected = some .exceedsExpectedCount ∧
      membershipReads (some (s.evaluate members expected now)) = 0 := by
  have hr : quiesceReason members expected = some .exceedsExpectedCount := by
    simp [quiesceReason, h]
  refine ⟨?_, hr, ?_⟩ <;>
    cases hq : s.quiescedAt <;>
      simp [EventGroupState.evaluate, hr, EventGroupState.quiesce, hq, membershipReads]

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

def captureCallback (group : EventGroupState) (candidate : Candidate)
    (invocationId input agentDid : String) : Option CallbackInvocation :=
  if group.quiescedAt.isNone && candidate.eligible && decide (candidate.key = group.key) then
    match group.key.consumer with
    | .trigger _ => none
    | .callbackBinding _ => some
        { invocationId := invocationId, ownerNodeDid := group.key.nodeDid, agentDid := agentDid,
          input := input, originGroupKey := some group.key,
          state := .pending, journal := [], resultEmitted := false }
  else none

theorem capture_binds_eligible_group_and_input (group : EventGroupState)
    (candidate : Candidate) (id input agentDid : String) (inv : CallbackInvocation)
    (h : captureCallback group candidate id input agentDid = some inv) :
    group.quiescedAt = none ∧ candidate.eligible = true ∧ candidate.key = group.key ∧
      inv.ownerNodeDid = group.key.nodeDid ∧ inv.originGroupKey = some group.key ∧
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

theorem captured_input_survives_execution {pre post : CallbackInvocation}
    (h : Relation.ReflTransGen CallbackInvocation.Transition pre post) :
    post.input = pre.input ∧ post.originGroupKey = pre.originGroupKey := by
  induction h with
  | refl => exact ⟨rfl, rfl⟩
  | @tail mid post _ step ih =>
      have hp := CallbackInvocation.frozen_input_preserved step
      exact ⟨hp.1.trans ih.1, hp.2.trans ih.2⟩

end EventDelivery.Group
