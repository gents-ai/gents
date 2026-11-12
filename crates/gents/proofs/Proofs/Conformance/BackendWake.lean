import Proofs.Session.Request.BackendWake
import Proofs.Shared.RenderedCapture
import Proofs.Automation.EventDelivery.Watcher
import Proofs.Session.Request.ResumeAdmission
import Proofs.Session.RequestExecutionLease.Transition
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.BackendWake

open Lean Conformance.Contracts

structure WakeInput where
  nowMs : Nat
  deadlineMs : Nat
  attempts : Nat
  deriving ToJson

@[conformance_family]
def fallbackWake : Family WakeInput (Option Nat) where
  inputs :=
    [ ⟨0, 100000, 0⟩, ⟨1000, 100000, 1⟩, ⟨2000, 100000, 4⟩,
      ⟨3000, 100000, 5⟩, ⟨4000, 100000, 30⟩, ⟨9500, 10000, 2⟩,
      ⟨10000, 10000, 0⟩, ⟨11000, 10000, 3⟩ ]
  model := fun i => RequestBackendWake.fallback i.nowMs i.deadlineMs i.attempts

structure CaptureScopeInput where
  persisted : List Nat
  deriving ToJson

@[conformance_family]
def captureScope : Family CaptureScopeInput Nat where
  inputs := [[], [1], [1, 4, 2], [8, 8, 3], [0, 15]].map fun persisted => { persisted }
  model := fun i => RenderedCapture.nextScope i.persisted

structure HealthInput where
  startedMs : Nat
  nowMs : Nat
  observedMs : Option Nat
  deriving ToJson

@[conformance_family]
def healthyWake : Family HealthInput Bool where
  inputs :=
    [ ⟨10, 20, none⟩, ⟨10, 20, some 9⟩, ⟨10, 20, some 10⟩,
      ⟨10, 20, some 20⟩, ⟨10, 20, some 21⟩ ]
  model := fun i => RequestBackendWake.freshHealthy i.startedMs i.nowMs i.observedMs

structure GenerationInput where
  marked : Option Nat
  observed : Option Nat
  deriving ToJson

@[conformance_family]
def generationMark : Family GenerationInput Bool where
  inputs :=
    [ ⟨none, none⟩, ⟨none, some 1⟩, ⟨some 1, some 1⟩, ⟨some 1, some 2⟩, ⟨some 2, none⟩ ]
  model := fun i => EventDelivery.Watcher.generationMarkApplies i.marked i.observed

deriving instance ToJson for RequestResumeAdmission.ClaimFacts

@[conformance_family]
def resumeAdmission : Family RequestResumeAdmission.ClaimFacts Bool where
  inputs :=
    [ ⟨true, true, true, true, true, true⟩,
      ⟨false, true, true, true, true, true⟩,
      ⟨true, false, true, true, true, true⟩,
      ⟨true, true, false, true, true, true⟩,
      ⟨true, true, true, false, true, true⟩,
      ⟨true, true, true, true, false, true⟩,
      ⟨true, true, true, true, true, false⟩ ]
  model := fun facts => RequestResumeAdmission.deadlineAdmitted false facts

def boolJson (b : Bool) : String := if b then "true" else "false"

structure ReparkInput where
  resuming : Bool
  expectedGeneration : Nat
  expectedExpiry : Nat
  now : Nat

def reparkInputs : List ReparkInput :=
  [ ⟨true, 7, 10, 10⟩, ⟨true, 7, 10, 11⟩, ⟨true, 7, 10, 9⟩,
    ⟨false, 7, 10, 10⟩, ⟨true, 8, 10, 10⟩, ⟨true, 7, 11, 11⟩ ]

def reparkObservedGeneration : Nat := 7

def reparkObservedExpiry : Nat := 10

def reparkInputJson (input : ReparkInput) : String :=
  let pre := { RequestExecutionLease.initial Nat with
    request := if input.resuming then .resumingBackend else .processing
    lease := .active reparkObservedGeneration 5 reparkObservedExpiry,
    usedGenerations := [reparkObservedGeneration], now := input.now }
  let allowed := (RequestExecutionLease.step? pre
    (.reparkExpiredResume .mutationWriteGate input.expectedGeneration input.expectedExpiry)).isSome
  "{\"resuming\":" ++ boolJson input.resuming ++
    ",\"observed_generation\":" ++ toString reparkObservedGeneration ++
    ",\"observed_expiry\":" ++ toString reparkObservedExpiry ++
    ",\"expected_generation\":" ++ toString input.expectedGeneration ++
    ",\"expected_expiry\":" ++ toString input.expectedExpiry ++ ",\"now\":" ++ toString input.now ++
    ",\"allowed\":" ++ boolJson allowed ++ "}"

def reparkCasesJson : String := jsonArray (reparkInputs.map reparkInputJson)

def parkedFinalizeInputs : List (RequestState × RequestExecutionLease.Outcome) :=
  [RequestState.waitingForBackend, .resumingBackend, .processing, .pending].flatMap fun request =>
    [RequestExecutionLease.Outcome.completed, .failed, .interrupted, .dead, .superseded].map
      fun outcome => (request, outcome)

def parkedFinalizeCaseJson (input : RequestState × RequestExecutionLease.Outcome) : String :=
  let (request, outcome) := input
  let pre := { RequestExecutionLease.initial Nat with request := request, usedGenerations := [7] }
  let allowed := (RequestExecutionLease.step? pre
    (.finalizeParked .mutationWriteGate 8 outcome)).isSome
  "{\"request\":\"" ++ request.toDefraDB ++ "\",\"outcome\":\"" ++
    outcome.requestState.toDefraDB ++ "\",\"allowed\":" ++ boolJson allowed ++ "}"

def parkedFinalizeCasesJson : String := jsonArray (parkedFinalizeInputs.map parkedFinalizeCaseJson)

def resumedTurnCases : List (List Nat) :=
  [[], [0], [0, 1, 2], [2, 0, 1], [4], [3, 3]]

def resumedTurnCaseJson (completed : List Nat) : String :=
  "{\"completed\":" ++ jsonArray (completed.map toString) ++
    ",\"next\":" ++ toString (RequestBackendWake.resumedTurn completed) ++ "}"

def resumedTurnCasesJson : String := jsonArray (resumedTurnCases.map resumedTurnCaseJson)

end Conformance.BackendWake
