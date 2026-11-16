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

/-- Why a group is quiesced, judged from its member count against its expected
count (the cap-and-count view; `QuiesceReason` below judges each member's
declared count). -/
inductive CountQuiesceReason where
  | exceedsExpectedCount
  deriving DecidableEq, Repr

/-- A group whose membership exceeds its expected count or the hard document
cap can never become exactly complete, so it is quiesced durably instead of
waiting. -/
def countQuiesceReason (members expected : Nat) : Option CountQuiesceReason :=
  if maxGroupDocs < members || expected < members then some .exceedsExpectedCount else none

def EventGroupState.evaluate (s : EventGroupState) (members expected now : Nat) :
    EventGroupState :=
  if (countQuiesceReason members expected).isSome then s.quiesce now else s

/-- Startup recovery prunes quiesced groups before reading their membership. -/
def membershipReads : Option EventGroupState → Nat
  | none => 1
  | some s => if s.quiescedAt.isSome then 0 else 1

theorem invalid_group_is_durably_quiesced_and_pruned_after_restart
    (s : EventGroupState) (members expected now : Nat) (h : expected < members) :
    (s.evaluate members expected now).quiescedAt.isSome ∧
      countQuiesceReason members expected = some .exceedsExpectedCount ∧
      membershipReads (some (s.evaluate members expected now)) = 0 := by
  have hr : countQuiesceReason members expected = some .exceedsExpectedCount := by
    simp [countQuiesceReason, h]
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

def clockKey (owner : String) : EventGroupKey :=
  { nodeDid := owner
  , consumer := .callbackBinding "same-id"
  , consumerConfigKey := "config-1"
  , correlation := "one" }

def ownerKey : EventGroupKey := clockKey "owner-a"

def otherOwnerKey : EventGroupKey := clockKey "owner-b"

theorem ownerKey_ne_otherOwnerKey : ownerKey ≠ otherOwnerKey := by
  decide

def loadOrCreate (store : List EventGroupState) (key : EventGroupKey) (now : Nat) :
    EventGroupState × List EventGroupState :=
  match store.find? (fun s => s.key == key) with
  | some s => (s, store)
  | none =>
    let s : EventGroupState := { key := key, firstSeen := now }
    (s, s :: store)

def restartPreserved (key : EventGroupKey) (first second : Nat) : Bool :=
  let loaded := loadOrCreate [] key first
  let reloaded := loadOrCreate loaded.2 key second
  reloaded.1.key == loaded.1.key && reloaded.1.firstSeen == loaded.1.firstSeen

theorem restart_preserved_keeps_record (key : EventGroupKey) (first second : Nat) :
    restartPreserved key first second = true := by
  simp [restartPreserved, loadOrCreate, List.find?]

inductive QuiesceReason where
  | disagree
  | invalid
  | exceeded
  deriving DecidableEq, Repr

inductive StepOutcome where
  | pending
  | ready
  | quiesced
  deriving DecidableEq, Repr

def resolveMembers : Option Nat → List Int → Except QuiesceReason Nat
  | prior, [] =>
    match prior with
    | some n => .ok n
    | none => .error .invalid
  | prior, m :: rest =>
    if m ≤ 0 ∨ (maxGroupDocs : Int) < m then .error .invalid
    else if prior.any (· != m.toNat) then .error .disagree
    else resolveMembers (some m.toNat) rest

def quiesceReason (members : List Int) : Option QuiesceReason :=
  match resolveMembers none members with
  | .error reason => some reason
  | .ok expected => if expected < members.length then some .exceeded else none

/-- Production appends one member, then evaluates the whole group. A quiesced group stays
quiesced with the reason it recorded first. -/
def evaluateGroup (quiesced : Option QuiesceReason) (members : List Int) :
    StepOutcome × Option QuiesceReason :=
  match quiesced with
  | some reason => (.quiesced, some reason)
  | none =>
    match quiesceReason members with
    | some reason => (.quiesced, some reason)
    | none =>
      match resolveMembers none members with
      | .ok expected => (if members.length = expected then .ready else .pending, none)
      | .error reason => (.quiesced, some reason)

def runGroup (quiesced : Option QuiesceReason) (seen : List Int) :
    List Int → List StepOutcome × Option QuiesceReason
  | [] => ([], quiesced)
  | v :: rest =>
    let members := seen ++ [v]
    let step := evaluateGroup quiesced members
    let tail := runGroup step.2 members rest
    (step.1 :: tail.1, tail.2)

def runSteps (members : List Int) : List StepOutcome × Option QuiesceReason :=
  runGroup none [] members

def firstRecord : EventGroupState := { key := ownerKey, firstSeen := 1 }

def finalRecord (reason : Option QuiesceReason) : EventGroupState :=
  match reason with
  | none => firstRecord
  | some _ => firstRecord.quiesce 2

theorem finalRecord_key (reason : Option QuiesceReason) :
    (finalRecord reason).key = ownerKey := by
  cases reason with
  | none => rfl
  | some _ =>
    have h := (quiesce_preserves_identity_and_deadline firstRecord 2 none).1
    exact h

structure ClockObservation where
  restartSameRecord : Bool
  outcomes : List StepOutcome
  recordedReason : Option QuiesceReason
  independentFresh : Bool
  deriving Repr

def clockObservation (members : List Int) : ClockObservation :=
  let run := runSteps members
  { restartSameRecord := restartPreserved ownerKey 1 2
    outcomes := run.1
    recordedReason := run.2
    independentFresh :=
      (let stored := (loadOrCreate [finalRecord run.2] otherOwnerKey 3).1
       stored.key != (finalRecord run.2).key && stored.quiescedAt.isNone) }

theorem other_owner_group_is_fresh (rec : EventGroupState) (other : EventGroupKey) (now : Nat)
    (h : rec.key ≠ other) :
    ((loadOrCreate [rec] other now).1.key != rec.key &&
      (loadOrCreate [rec] other now).1.quiescedAt.isNone) = true := by
  have hne : (rec.key == other) = false :=
    Bool.eq_false_iff.mpr (fun heq => h (beq_iff_eq.mp heq))
  have h' : other ≠ rec.key := fun e => h e.symm
  simp [loadOrCreate, List.find?, hne, h', bne_iff_ne]

theorem shared_clock_survives_restart_and_quiescence_is_owner_scoped (members : List Int) :
    (clockObservation members).restartSameRecord = true ∧
      (clockObservation members).independentFresh = true := by
  refine ⟨restart_preserved_keeps_record ownerKey 1 2, ?_⟩
  exact other_owner_group_is_fresh (finalRecord (runSteps members).2) otherOwnerKey 3
    (by rw [finalRecord_key]; exact ownerKey_ne_otherOwnerKey)

end EventDelivery.Group
