import Proofs.Session.Request.BackendWake
import Proofs.Shared.RenderedCapture
import Proofs.Automation.EventDelivery.Watcher
import Proofs.Session.Request.ResumeAdmission
import Proofs.Session.RequestExecutionLease.Transition
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Family

namespace Conformance.BackendWake

open Lean

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

structure ReparkInput where
  resuming : Bool
  observedGeneration : Nat
  observedExpiry : Nat
  expectedGeneration : Nat
  expectedExpiry : Nat
  now : Nat
  deriving ToJson

@[conformance_family]
def repark : Family ReparkInput Bool where
  inputs :=
    [ ⟨true, 7, 10, 7, 10, 10⟩, ⟨true, 7, 10, 7, 10, 11⟩, ⟨true, 7, 10, 7, 10, 9⟩,
      ⟨false, 7, 10, 7, 10, 10⟩, ⟨true, 7, 10, 8, 10, 10⟩, ⟨true, 7, 10, 7, 11, 11⟩ ]
  model := fun i =>
    let pre := { RequestExecutionLease.initial Nat with
      request := if i.resuming then .resumingBackend else .processing
      lease := .active i.observedGeneration 5 i.observedExpiry,
      usedGenerations := [i.observedGeneration], now := i.now }
    (RequestExecutionLease.step? pre
      (.reparkExpiredResume .mutationWriteGate i.expectedGeneration i.expectedExpiry)).isSome

structure ParkedFinalizeInput where
  request : RequestState
  outcome : RequestExecutionLease.Outcome
  deriving ToJson

@[conformance_family]
def parkedFinalize : Family ParkedFinalizeInput Bool where
  inputs :=
    [RequestState.waitingForBackend, .resumingBackend, .processing, .pending].flatMap fun request =>
      [RequestExecutionLease.Outcome.completed, .failed, .interrupted, .dead, .superseded].map
        fun outcome => { request, outcome }
  model := fun i =>
    let pre := { RequestExecutionLease.initial Nat with request := i.request, usedGenerations := [7] }
    (RequestExecutionLease.step? pre (.finalizeParked .mutationWriteGate 8 i.outcome)).isSome

example : parkedFinalize.inputs.any fun i => parkedFinalize.model i && i.outcome == .superseded := by
  native_decide

example : parkedFinalize.inputs.any fun i => !parkedFinalize.model i := by native_decide

structure ResumedTurnInput where
  completed : List Nat
  deriving ToJson

@[conformance_family]
def resumedTurnIndex : Family ResumedTurnInput Nat where
  inputs := [[], [0], [0, 1, 2], [2, 0, 1], [4], [3, 3]].map fun completed => { completed }
  model := fun i => RequestBackendWake.resumedTurn i.completed

example : resumedTurnIndex.inputs.any fun i => resumedTurnIndex.model i > 1 := by native_decide

end Conformance.BackendWake
