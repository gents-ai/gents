import Mathlib.Data.List.Basic
import Lean.Elab.Tactic.Omega

namespace Background.Observation

/-- Native subscriptions are live-only hints. A monotonic clock, eventual
polling of ready timers, and termination of each durable read are external
premises; this owner proves wake selection, not host scheduling fairness.
Only the timer owner advances `rescanAt`, independently of change traffic. -/
structure State where
  rescanAt : Nat
  feedOpen : Bool
  deriving DecidableEq, Repr

inductive Change where
  | quiet
  | unrelated
  | relevant
  | overflow
  | closed
  deriving DecidableEq, Repr

structure Input where
  now : Nat
  cancelled : Bool
  deadlineAt : Nat
  change : Change
  deriving DecidableEq, Repr

inductive Wake where
  | cancel
  | deadline
  | rescan
  | changed
  | wait
  deriving DecidableEq, Repr

def wake (state : State) (input : Input) : Wake :=
  if input.cancelled then .cancel
  else if input.deadlineAt ≤ input.now then .deadline
  else if state.rescanAt ≤ input.now then .rescan
  else if state.feedOpen && input.change != .quiet && input.change != .unrelated
    then .changed else .wait

def observeChange (state : State) (change : Change) : State :=
  { state with feedOpen := state.feedOpen && change != .closed }

structure DocumentChange where
  collection : String
  docId : String
  deriving DecidableEq, Repr

structure Batch where
  changes : List DocumentChange
  resyncRequired : Bool
  deriving DecidableEq, Repr

/-- A background wait watches its own tool row and, when known, its caller's
request row. A change without a document id names nothing that can be ruled
out, and a change in any other collection is not classified by this owner. -/
structure Scope where
  toolCollection : String
  toolDocId : String
  requestCollection : String
  callerDocId : Option String
  deriving DecidableEq, Repr

def changeRelevant (scope : Scope) (c : DocumentChange) : Bool :=
  if c.docId == "" then true
  else if c.collection == scope.toolCollection then c.docId == scope.toolDocId
  else if c.collection == scope.requestCollection then
    match scope.callerDocId with
    | none => true
    | some caller => c.docId == caller
  else true

/-- The change one feed poll observes: a closed feed, an overflow that lost
invalidations, a batch naming a watched row, or traffic of other rows. -/
def classify (scope : Scope) : Option Batch → Change
  | none => .closed
  | some batch =>
    if batch.resyncRequired then .overflow
    else if batch.changes.any (changeRelevant scope) then .relevant
    else .unrelated

private def waitScope : Scope :=
  { toolCollection := "tools", toolDocId := "tool", requestCollection := "requests"
  , callerDocId := some "caller" }

private def pending (change : Change) : Wake :=
  wake { rescanAt := 1, feedOpen := true }
    { now := 0, cancelled := false, deadlineAt := 1, change }

theorem background_wait_filters_other_requests_and_tools_before_rereading :
    pending (classify waitScope (some
      { changes := [⟨"tools", "unrelated-physical-document"⟩,
                    ⟨"requests", "unrelated-physical-document"⟩]
      , resyncRequired := false })) = .wait ∧
    pending (classify waitScope (some
      { changes := [⟨"tools", "tool"⟩], resyncRequired := false })) = .changed ∧
    pending (classify waitScope (some
      { changes := [⟨"tools", "unrelated-physical-document"⟩], resyncRequired := true })) =
      .changed := by
  native_decide

theorem unrelated_traffic_keeps_waiting (scope : Scope) (batch : Batch)
    (live : batch.resyncRequired = false)
    (unrelated : ∀ c ∈ batch.changes, changeRelevant scope c = false) :
    classify scope (some batch) = .unrelated := by
  have h : batch.changes.any (changeRelevant scope) = false := by
    simpa [List.any_eq_false] using unrelated
  simp [classify, live, h]

theorem cancellation_precedes_all_wakes (state : State) (input : Input)
    (cancelled : input.cancelled = true) : wake state input = .cancel := by
  simp [wake, cancelled]

theorem due_deadline_precedes_change (state : State) (input : Input)
    (live : input.cancelled = false) (due : input.deadlineAt ≤ input.now) :
    wake state input = .deadline := by simp [wake, live, due]

theorem due_rescan_cannot_be_starved (state : State) (input : Input)
    (live : input.cancelled = false) (before : input.now < input.deadlineAt)
    (due : state.rescanAt ≤ input.now) : wake state input = .rescan := by
  simp [wake, live, Nat.not_le.mpr before, due]

theorem notification_preserves_rescan (state : State) (change : Change) :
    (observeChange state change).rescanAt = state.rescanAt := by rfl

theorem arbitrary_traffic_preserves_rescan (state : State) (changes : List Change) :
    (changes.foldl observeChange state).rescanAt = state.rescanAt := by
  induction changes generalizing state with
  | nil => rfl
  | cons change rest ih => simpa [List.foldl] using ih (observeChange state change)

theorem feed_closure_is_latched (state : State) (changes : List Change) :
    (changes.foldl observeChange (observeChange state .closed)).feedOpen = false := by
  have closed (s : State) (xs : List Change) (h : s.feedOpen = false) :
      (xs.foldl observeChange s).feedOpen = false := by
    induction xs generalizing s with
    | nil => simpa using h
    | cons x xs ih =>
      apply ih
      simp [observeChange, h]
  apply closed
  simp [observeChange]

theorem closed_feed_keeps_durable_recovery (state : State) (input : Input)
    (live : input.cancelled = false) (before : input.now < input.deadlineAt)
    (due : state.rescanAt ≤ input.now) :
    wake (observeChange state .closed) input = .rescan := by
  exact due_rescan_cannot_be_starved (observeChange state .closed) input live before due

end Background.Observation
