import Proofs.ToolCalls.Background.CompletionContinuation
import Proofs.Conformance.GoalRequestAssignment
import Proofs.Conformance.Family
import Lean

namespace Conformance.GoalWakeCoalescingContracts

open GoalAutomation.RequestAssignment Lean

structure Case where
  name : String
  first : Entry := ⟨100, 1, 10, some ⟨1, none⟩, true⟩
  second : Entry := ⟨101, 1, 10, some ⟨1, some 100⟩, true⟩
  observed : List Assignment := [⟨1, 10, 1, some 100⟩]
  firstKey : Nat := 7
  secondKey : Nat := 7
  retireFirst : Bool := false
  deriving ToJson

def cases : List Case :=
  [ {name := "same_signed_assignment_coalesces"}
  , {name := "new_goal_does_not_reuse_old_wake",
      second := ⟨200, 1, 10, some ⟨2, none⟩, true⟩,
      observed := [⟨1, 10, 1, some 100⟩, ⟨1, 10, 2, some 200⟩]}
  , {name := "reused_goal_id_keeps_distinct_roots",
      second := ⟨200, 1, 10, some ⟨1, none⟩, true⟩,
      observed := [⟨1, 10, 1, some 100⟩, ⟨1, 10, 1, some 200⟩]}
  , {name := "untagged_tool_goal_opening_joins_signed_followup",
      first := ⟨100, 1, 10, none, true⟩}
  , {name := "retired_goal_keeps_its_own_wake", retireFirst := true,
      second := ⟨200, 1, 10, some ⟨2, none⟩, true⟩,
      observed := [⟨1, 10, 1, some 100⟩, ⟨1, 10, 2, some 200⟩]}
  , {name := "ordinary_unassigned_parents_keep_coalescing", observed := [],
      first := ⟨100, 1, 10, none, true⟩, second := ⟨200, 1, 10, none, true⟩}
  , {name := "unresolved_legacy_roots_stay_separate", observed := [⟨1, 10, 1, none⟩],
      first := ⟨100, 1, 10, none, true⟩, second := ⟨200, 1, 10, none, true⟩}
  , {name := "same_unresolved_legacy_parent_still_coalesces", observed := [⟨1, 10, 1, none⟩],
      first := ⟨100, 1, 10, none, true⟩, second := ⟨100, 1, 10, none, true⟩}
  , {name := "missing_goal_preserves_signed_intent", observed := []}
  , {name := "distinct_queue_keys_remain_separate", secondKey := 8}
  ]

def wake (request created key : Nat) : SessionQueue.QueueEntry :=
  {requestId := request, createdAt := created, source := .backgroundCompletion,
    policy := .coalesce, queueKey := some key, queuedAfter := none, origin := .scheduled}

def coalesces (c : Case) : Bool := Id.run do
  let some firstScope := queueScope c.observed c.first | return false
  let some secondScope := queueScope c.observed c.second | return false
  let first := wake 501 1 (queueScopeKey c.firstKey firstScope)
  let second := wake 502 2 (queueScopeKey c.secondKey secondScope)
  let pre : SessionQueue.SessionQueueState :=
    {scope := ⟨1, 10, none⟩, active := none, pending := [first], terminal := ∅}
  let some post := SessionQueue.step? pre (.coalescePending second) | return false
  return post.pending.length == 1

def scopedWakeAccepted (c : Case) : Bool := Id.run do
  let some scope := queueScope c.observed c.first | return false
  let completion := {BackgroundCompletion.canonicalCompletion with
    wake := {BackgroundCompletion.canonicalWake with
      queueKey := some (queueScopeKey c.firstKey scope)}}
  let some notified := BackgroundCompletion.appendNotification? completion
    BackgroundCompletion.canonicalWaitReservedTranscript | return false
  return (BackgroundCompletion.enqueueWake? notified BackgroundCompletion.canonicalQueue).isSome

structure ScopeWire where
  kind : String
  goal : Option Nat
  root : Option Nat
  entry : Option Nat
  deriving ToJson

def ScopeWire.ofModel : QueueScope → ScopeWire
  | .unassigned => ⟨"unassigned", none, none, none⟩
  | .unresolved entry => ⟨"unresolved", none, none, some entry⟩
  | .referenced goal root => ⟨"referenced", some goal, some root, none⟩

structure Output where
  coalesced : Bool
  wake : Bool
  firstScope : Option ScopeWire
  secondScope : Option ScopeWire
  deriving ToJson

@[conformance_family]
def assignmentWakes : Family Case Output where
  inputs := cases
  model := fun c =>
    { coalesced := coalesces c
      wake := scopedWakeAccepted c
      firstScope := (queueScope c.observed c.first).map ScopeWire.ofModel
      secondScope := (queueScope c.observed c.second).map ScopeWire.ofModel }

example : cases.map coalesces =
  [true, false, false, true, false, true, false, true, true, false] := by native_decide
example : cases.all scopedWakeAccepted = true := by native_decide

end Conformance.GoalWakeCoalescingContracts
