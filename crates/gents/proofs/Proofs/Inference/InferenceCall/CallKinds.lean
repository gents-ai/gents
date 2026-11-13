import Proofs.Inference.InferenceCall.Registry
import Proofs.Inference.InferenceCall.DroppedPermit

/-! Inference, compaction, scheduled and one-off calls are kinds of one call.
Each is admitted through its backend's single capacity pool, whatever its kind,
and its row records the kind it was scoped with. -/
namespace InferenceCall.CallKinds

inductive CallKind where
  | inference
  | compaction
  | scheduled
  | oneOff
  deriving DecidableEq, Repr

def CallKind.toDefraDB : CallKind → String
  | .inference => "inference"
  | .compaction => "compaction"
  | .scheduled => "scheduled"
  | .oneOff => "oneoff"

def pool (capacity queueDepth : Nat) : Registry.Config :=
  { connection := 0, generation := 0, capacity, queueDepth, available := true }

def opened (config : Registry.Config) : Registry.State :=
  { desired := some config, admitting := some config, held := 0, queue := []
    tally := { admitted := 0, queueFull := 0, gone := 0 }, attributed := [] }

/-- The state a call's row records when it asks for a slot, and the waiters
counted when it was placed (zero when it was admitted at once). -/
def placement (s : Registry.State) (slot : Nat) : InferenceCallState × Nat :=
  if s.attributed.contains slot then (.running, 0)
  else if s.queue.contains slot then (.queued, s.queue.length)
  else (.failed, s.queue.length)

/-- Calls ask in order while every admitted call is still held. Admission sees
only the pool and the slot: the kind never enters it. -/
def admissions (config : Registry.Config) (count : Nat) :
    Registry.State × List (InferenceCallState × Nat) :=
  (List.range count).foldl (fun (acc : Registry.State × List (InferenceCallState × Nat)) slot =>
    let next := Registry.acquire acc.1 slot
    (next, acc.2 ++ [placement next slot])) (opened config, [])

/-- Every held call finishes in order; each release admits the next waiter. -/
def drain (s : Registry.State) : Registry.State :=
  (List.range (s.held + s.queue.length)).foldl (fun s _ => Registry.release s) s

structure Placed where
  kind : CallKind
  state : InferenceCallState
  queueDepthAtEnqueue : Nat
  finished : InferenceCallState
  deriving DecidableEq, Repr

def place (config : Registry.Config) (kinds : List CallKind) : List Placed :=
  let (held, placements) := admissions config kinds.length
  let finished := drain held
  (kinds.zip placements).zipIdx.map fun ((kind, (state, depth)), slot) =>
    { kind, state, queueDepthAtEnqueue := depth
      finished := if finished.attributed.contains slot then .completed else .failed }

def slotsHeld (states : List InferenceCallState) : Nat :=
  (states.map DroppedPermit.slotsHeld).sum

theorem compaction_calls_share_backend_capacity_with_inference_calls :
    (place (pool 1 1) [.inference, .compaction]).map
        (fun p => (p.kind.toDefraDB, p.state.toDefraDB, p.finished.toDefraDB, p.queueDepthAtEnqueue)) =
      [("inference", "running", "completed", 0), ("compaction", "queued", "completed", 1)] ∧
    (place (pool 1 1) [.compaction, .inference]).map (·.state) =
      (place (pool 1 1) [.inference, .compaction]).map (·.state) := by
  decide

theorem scoped_scheduled_calls_are_persisted_with_scheduled_kind :
    (place (pool 1 1) [.scheduled]).map (fun p => (p.kind.toDefraDB, p.finished)) =
      [("scheduled", .completed)] := by
  decide

theorem scoped_oneoff_calls_are_persisted_with_oneoff_kind :
    (place (pool 1 1) [.oneOff]).map (fun p => (p.kind.toDefraDB, p.finished)) =
      [("oneoff", .completed)] := by
  decide

end InferenceCall.CallKinds
