import Mathlib.Data.List.Basic

namespace ToolExecution.Footprint

/-- Resources are conflict classes supplied by the tool owner. A classifier
must include every overlapping effect; an effect it cannot classify is unknown. -/
inductive Effects where
  | unknown
  | known (reads writes : List String)
  deriving DecidableEq, Repr

inductive HookDecision where
  | proceed
  | skip
  | terminate
  deriving DecidableEq, Repr

def intersects (left right : List String) : Bool :=
  left.any fun resource => right.contains resource

def independent : Effects → Effects → Bool
  | .unknown, _ => false
  | _, .unknown => false
  | .known readsA writesA, .known readsB writesB =>
      !(intersects writesA readsB || intersects writesA writesB || intersects readsA writesB)

/-- Hook outcomes are settled in call order. A skipped or terminated call is
not a concurrent dispatch, and termination prevents dispatch of later calls. -/
def dispatchablePrefix : List HookDecision → List Nat
  | [] => []
  | decision :: remaining =>
      match decision with
      | .terminate => []
      | .skip => (dispatchablePrefix remaining).map (· + 1)
      | .proceed => 0 :: (dispatchablePrefix remaining).map (· + 1)

theorem terminate_prevents_later_dispatch (later : List HookDecision) :
    dispatchablePrefix (.terminate :: later) = [] := by rfl

theorem skip_omits_current_call (later : List HookDecision) :
    0 ∉ dispatchablePrefix (.skip :: later) := by
  simp [dispatchablePrefix]

theorem earlier_call_survives_later_termination :
    dispatchablePrefix [.proceed, .terminate, .proceed] = [0] := by rfl

/-- A later call cannot start ahead of an earlier result hook that may
terminate the turn. The hook owner must establish this capability before
the footprint rule can authorize overlapping dispatch. -/
def mayOverlap (earlierResultCannotTerminate : Bool) (earlier later : Effects) : Bool :=
  earlierResultCannotTerminate && independent earlier later

theorem uncertain_result_hook_is_barrier (earlier later : Effects) :
    mayOverlap false earlier later = false := by rfl

theorem unknown_is_left_barrier (other : Effects) : independent .unknown other = false := by
  cases other <;> rfl

theorem unknown_is_right_barrier (other : Effects) : independent other .unknown = false := by
  cases other <;> rfl

theorem read_only_calls_are_independent (a b : List String) :
    independent (.known a []) (.known b []) = true := by
  simp [independent, intersects]

theorem same_resource_writes_conflict (resource : String) :
    independent (.known [] [resource]) (.known [] [resource]) = false := by
  simp [independent, intersects]

theorem write_then_read_conflicts (resource : String) :
    independent (.known [] [resource]) (.known [resource] []) = false := by
  simp [independent, intersects]

theorem read_then_write_conflicts (resource : String) :
    independent (.known [resource] []) (.known [] [resource]) = false := by
  simp [independent, intersects]

def growBatch (safeHooks : Bool) (batch : List Effects) : List Effects → List Effects × List Effects
  | [] => (batch, [])
  | next :: rest =>
      if batch.all (fun current => mayOverlap safeHooks current next) then
        growBatch safeHooks (batch ++ [next]) rest
      else
        (batch, next :: rest)

def nextBatch (safeHooks : Bool) : List Effects → List Effects × List Effects
  | [] => ([], [])
  | first :: rest => growBatch safeHooks [first] rest

theorem grow_batch_preserves_order (safeHooks : Bool) (batch remaining : List Effects) :
    (growBatch safeHooks batch remaining).1 ++ (growBatch safeHooks batch remaining).2 = batch ++ remaining := by
  induction remaining generalizing batch with
  | nil => simp [growBatch]
  | cons next rest ih =>
      simp only [growBatch]
      split
      · simpa [List.append_assoc] using ih (batch ++ [next])
      · simp

theorem next_batch_preserves_order (safeHooks : Bool) (calls : List Effects) :
    (nextBatch safeHooks calls).1 ++ (nextBatch safeHooks calls).2 = calls := by
  cases calls with
  | nil => rfl
  | cons first rest => simpa [nextBatch] using grow_batch_preserves_order safeHooks [first] rest

theorem disjoint_reads_batch_together :
    nextBatch true [.known ["a"] [], .known ["b"] []] =
      ([.known ["a"] [], .known ["b"] []], []) := by decide

theorem unknown_stops_batch_before_later_reads :
    nextBatch true [.known ["a"] [], .unknown, .known ["b"] []] =
      ([.known ["a"] []], [.unknown, .known ["b"] []]) := by decide

theorem conflicting_write_stops_batch :
    nextBatch true [.known ["a"] [], .known [] ["a"], .known ["b"] []] =
      ([.known ["a"] []], [.known [] ["a"], .known ["b"] []]) := by decide

theorem unsafe_hook_starts_only_first (first second : Effects) (rest : List Effects) :
    nextBatch false (first :: second :: rest) = ([first], second :: rest) := by
  simp [nextBatch, growBatch, mayOverlap]

end ToolExecution.Footprint
