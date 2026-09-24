import Proofs.InferenceCall.Registry
import Proofs.Conformance.ContractTypes

namespace Conformance.InferenceRegistry
open InferenceCall.Registry
open Conformance.Contracts

/-- Metadata labels are intentionally projected out of resource identity. -/
structure Desired where
  config : Config
  name : String
  catalog : String
  deriving DecidableEq, Repr

inductive Action where
  | reconcile (desired : Option Desired)
  /-- A caller whose provider client was built for connection `slot`. -/
  | acquire (slot : Nat)
  /-- The oldest admitted call releases its permit. -/
  | release
  deriving DecidableEq, Repr

/-- `attributed` is order-free: Rust observes admissions as calls finish. -/
structure Observation where
  admittingGeneration : Option Nat
  capacity : Nat
  held : Nat
  queued : Nat
  tally : Tally
  attributed : List Nat
  deriving DecidableEq, Repr

def observe (s : State) : Observation :=
  ⟨s.admitting.map (·.generation), InferenceCall.Registry.capacity s, s.held, s.queue.length,
    s.tally, s.attributed.mergeSort⟩

def step (s : State) : Action → State
  | .reconcile d => InferenceCall.Registry.reconcile s (d.map (·.config))
  | .acquire slot => InferenceCall.Registry.acquire s slot
  | .release => InferenceCall.Registry.release s

def replay (s : State) : List Action → List Observation
  | [] => []
  | action :: rest => let post := step s action; observe post :: replay post rest

def initial : State := ⟨none, none, 0, [], ⟨0, 0, 0⟩, []⟩

def up (connection generation capacity queueDepth : Nat) (name := "backend") : Action :=
  .reconcile (some ⟨⟨connection, generation, capacity, queueDepth, true⟩, name, "model-a"⟩)

def down (connection generation capacity queueDepth : Nat) : Action :=
  .reconcile (some ⟨⟨connection, generation, capacity, queueDepth, false⟩, "backend", "model-a"⟩)

structure Case where
  name : String
  actions : List Action
  deriving DecidableEq, Repr

/-- Traces only: every expected observation is derived by `replay`. -/
def cases : List Case := [
  ⟨"metadata_name_catalog_stutter",
    [up 7 1 2 0, .acquire 7, up 7 2 2 0 (name := "renamed"), .acquire 7, .acquire 7, .release]⟩,
  ⟨"capacity_increase_admits_during_drain",
    [up 7 1 2 0, .acquire 7, up 7 2 3 0, .acquire 7, .release, .acquire 7]⟩,
  ⟨"capacity_decrease_waits_for_excess",
    [up 7 1 2 0, .acquire 7, .acquire 7, up 7 2 1 0, .acquire 7, .release, .acquire 7,
     .release, .acquire 7, .acquire 7]⟩,
  ⟨"removed_while_in_flight_and_queued",
    [up 7 1 2 1, .acquire 7, .acquire 7, .acquire 7, .reconcile none, .release, .acquire 7]⟩,
  ⟨"outage_carries_held_permits",
    [up 7 1 2 0, .acquire 7, .acquire 7, down 7 2 2 0, .acquire 7, up 7 3 2 0, .acquire 7,
     .release, .acquire 7]⟩,
  ⟨"re_added_backend_keeps_held_permits",
    [up 7 1 1 0, .acquire 7, .reconcile none, up 7 1 1 0, .acquire 7, .release, .acquire 7]⟩,
  ⟨"latest_configuration_wins",
    [up 7 1 2 0, .acquire 7, up 7 2 3 0, up 7 3 4 0, up 7 4 1 0, .acquire 7, .release,
     .acquire 7, .acquire 7]⟩,
  ⟨"capacity_only_rewrite_keeps_queued_calls",
    [up 7 1 1 2, .acquire 7, .acquire 7, .acquire 7, up 7 2 2 2, .release, .release, .release]⟩,
  ⟨"shrinking_rewrite_keeps_queued_calls",
    [up 7 1 2 2, .acquire 7, .acquire 7, .acquire 7, up 7 2 1 2, .release, .release, .release]⟩,
  ⟨"connection_change_keeps_in_progress_calls",
    [up 7 1 1 2, .acquire 7, .acquire 7, up 8 2 1 2, .acquire 7, .acquire 8, .release,
     .release, .release]⟩,
  ⟨"key_rotation_shares_the_pool",
    [up 7 1 2 1, .acquire 7, up 8 2 2 1, .acquire 8, .acquire 7, .release, .release,
     .acquire 8]⟩
  ]

/-- Coverage the generated traces must keep: queueing, an admission from a
replaced connection, and closed admission are exercised. -/
theorem cases_cover_queueing_snapshots_and_closure :
    (cases.map fun c => (replay initial c.actions).any (fun o => 0 < o.queued)).any id = true ∧
      (cases.any fun c => c.actions.any (· == up 8 2 1 2) &&
        ((replay initial c.actions).getLast?.map (·.attributed)).any (·.contains 7)) = true ∧
      (cases.map fun c => (replay initial c.actions).getLast?.map (·.tally)).any
        (fun t => t.any (fun t => 0 < t.gone)) = true := by
  native_decide

def natOptionJson : Option Nat → String
  | none => "null"
  | some n => toString n

def configJson (d : Desired) : String :=
  "{\"connection\":" ++ toString d.config.connection
  ++ ",\"generation\":" ++ toString d.config.generation
  ++ ",\"capacity\":" ++ toString d.config.capacity
  ++ ",\"queue_depth\":" ++ toString d.config.queueDepth
  ++ ",\"available\":" ++ toString d.config.available
  ++ ",\"name\":" ++ jsonString d.name ++ ",\"catalog\":" ++ jsonString d.catalog ++ "}"

def actionJson : Action → String
  | .reconcile d => "{\"kind\":\"reconcile\",\"desired\":" ++ (d.map configJson).getD "null" ++ "}"
  | .acquire slot => "{\"kind\":\"acquire\",\"slot\":" ++ toString slot ++ "}"
  | .release => "{\"kind\":\"release\"}"

def observationJson (o : Observation) : String :=
  "{\"admitting_generation\":" ++ natOptionJson o.admittingGeneration
  ++ ",\"capacity\":" ++ toString o.capacity ++ ",\"held\":" ++ toString o.held
  ++ ",\"queued\":" ++ toString o.queued
  ++ ",\"admitted\":" ++ toString o.tally.admitted
  ++ ",\"queue_full\":" ++ toString o.tally.queueFull
  ++ ",\"gone\":" ++ toString o.tally.gone
  ++ ",\"attributed\":" ++ jsonArray (o.attributed.map toString) ++ "}"

def casesJson : String := jsonArray (cases.map fun c =>
  "{\"name\":" ++ jsonString c.name ++ ",\"actions\":" ++ jsonArray (c.actions.map actionJson)
  ++ ",\"expected\":" ++ jsonArray ((replay initial c.actions).map observationJson) ++ "}")

end Conformance.InferenceRegistry
