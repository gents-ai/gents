import Proofs.Runtime.BackendHealth.Cycle
import Proofs.Conformance.Functions.Runtime
import Proofs.Conformance.Family

namespace Proofs.BackendHealth
deriving instance Lean.ToJson for Event, ProbeTarget, TraceBackend, TraceStep, DocumentObservation
end Proofs.BackendHealth

namespace Conformance.BackendHealthRetention

open Lean Proofs.BackendHealth

def thresholdOf (k : Nat) : Threshold := ⟨max k 1, Nat.le_max_right _ _⟩

structure EntryWire where
  id : Nat
  state : HealthState
  failureCount : Nat
  deriving ToJson

def EntryWire.ofEntry (e : Entry) : EntryWire :=
  { id := e.id, state := e.model.state, failureCount := e.model.failureCount }

structure BackendWire where
  id : Nat
  target : ProbeTarget
  statusUnknown : Bool
  deriving ToJson

structure CycleInput where
  entries : List EntryWire
  backends : List BackendWire
  cancelled : Bool
  thresholdK : Nat
  deriving ToJson

structure CycleOutput where
  entries : List EntryWire
  flipped : List Nat
  promotable : List Nat
  deriving ToJson

def entryOptions (id : Nat) : List (List EntryWire) :=
  [[], [{ id, state := .degraded, failureCount := 2 }], [{ id, state := .unhealthy, failureCount := 3 }]]

def backendOptions (id : Nat) : List (List BackendWire) :=
  [[], [{ id, target := .reachable, statusUnknown := true }],
   [{ id, target := .reachable, statusUnknown := false }],
   [{ id, target := .unreachable, statusUnknown := true }],
   [{ id, target := .nodeOauth, statusUnknown := true }]]

def slots (id : Nat) : List (List EntryWire × List BackendWire) :=
  (entryOptions id).flatMap fun entries => (backendOptions id).map fun backends => (entries, backends)

@[conformance_family]
def cycle : Family CycleInput CycleOutput where
  inputs := (slots 0).flatMap fun (e0, b0) => (slots 1).flatMap fun (e1, b1) =>
    [false, true].map fun cancelled =>
      { entries := e0 ++ e1, backends := b0 ++ b1, cancelled, thresholdK := 3 }
  model := fun i =>
    let result := probeCycle
      (i.entries.map fun e =>
        { id := e.id, model := { state := e.state, failureCount := e.failureCount }, revision := none })
      (i.backends.map fun b =>
        { id := b.id, revision := 0, target := b.target, statusUnknown := b.statusUnknown, due := true })
      i.cancelled (thresholdOf i.thresholdK)
    { entries := result.entries.map EntryWire.ofEntry
      flipped := result.flipped
      promotable := result.promotable }

structure TraceInput where
  steps : List TraceStep
  thresholdK : Nat
  deriving ToJson

structure StepOutput where
  entries : List EntryWire
  probed : List Nat
  known : List Nat
  flipped : List Nat
  promotable : List Nat
  documents : List DocumentObservation
  deriving ToJson

def backendAt (id revision : Nat) (enabled reachable : Bool) : TraceBackend :=
  { id, revision, enabled, reachable }

def returningTrace (reachable : Bool) : List TraceStep :=
  [ { backends := [backendAt 0 0 true reachable], periodic := true }
  , { backends := [backendAt 0 0 false reachable], periodic := false }
  , { backends := [backendAt 0 0 true reachable], periodic := false }
  , { backends := [], periodic := false }
  , { backends := [backendAt 0 0 true reachable], periodic := false } ]

def editedTrace : List TraceStep :=
  [ { backends := [backendAt 0 0 true true, backendAt 1 0 true false], periodic := true }
  , { backends := [backendAt 0 0 true true, backendAt 1 0 true false], periodic := false }
  , { backends := [backendAt 0 0 true true, backendAt 1 0 true false], periodic := false }
  , { backends := [backendAt 0 1 true true, backendAt 1 0 true false], periodic := false }
  , { backends := [backendAt 0 1 true true, backendAt 1 0 true false], periodic := true } ]

def demotionTrace : List TraceStep :=
  [ { backends := [backendAt 0 0 true false], periodic := true }
  , { backends := [backendAt 0 0 true false], periodic := true }
  , { backends := [backendAt 0 0 true false], periodic := true }
  , { backends := [backendAt 0 1 true true], periodic := false }
  , { backends := [backendAt 0 1 true true], periodic := true } ]

@[conformance_family]
def trace : Family TraceInput (List StepOutput) where
  inputs := [editedTrace, returningTrace false, returningTrace true, demotionTrace].map
    fun steps => { steps, thresholdK := 3 }
  model := fun i => (runTrace i.steps (thresholdOf i.thresholdK)).map fun o =>
    { entries := o.result.entries.map EntryWire.ofEntry
      probed := o.probed
      known := o.known
      flipped := o.result.flipped
      promotable := o.result.promotable
      documents := o.documents }

structure PriorWire where
  state : HealthState
  failureCount : Nat
  revision : Option Nat
  deriving ToJson

structure MeasurementInput where
  prior : Option PriorWire
  revision : Nat
  event : Event
  thresholdK : Nat
  deriving ToJson

structure MeasurementOutput where
  state : HealthState
  failureCount : Nat
  flipped : Bool
  priorStateFor : Option HealthState
  nextStateFor : Option HealthState
  deriving ToJson

def priors : List (Option PriorWire) :=
  none :: ([(HealthState.healthy, 0), (.degraded, 2), (.unhealthy, 3), (.credentialsRequired, 1)].flatMap
    fun (state, failureCount) => [none, some 0, some 1].map fun revision =>
      some { state, failureCount, revision })

@[conformance_family]
def measurement : Family MeasurementInput MeasurementOutput where
  inputs := priors.flatMap fun prior =>
    [Event.probeSuccess, .probeFail, .credentialsRejected, .subscriptionNotEntitled].map fun event =>
      { prior, revision := 1, event, thresholdK := 3 }
  model := fun i =>
    let prior := i.prior.map fun p =>
      { id := 0, model := { state := p.state, failureCount := p.failureCount }, revision := p.revision }
    let recorded := recordEvent prior i.revision i.event (thresholdOf i.thresholdK)
    { state := recorded.model.state
      failureCount := recorded.model.failureCount
      flipped := recorded.flipped
      priorStateFor := prior.bind (stateFor · i.revision)
      nextStateFor := stateFor { id := 0, model := recorded.model, revision := some i.revision } i.revision }

end Conformance.BackendHealthRetention
