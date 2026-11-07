import Proofs.Request.BackendWake
import Proofs.RenderedCapture
import Proofs.EventDelivery.Watcher
import Proofs.Request.ResumeAdmission
import Proofs.RequestExecutionLease.Transition
import Proofs.Conformance.ContractTypes

namespace Conformance.BackendWake

open Conformance.Contracts

structure Input where
  now : Nat
  deadline : Nat
  attempts : Nat

def inputs : List Input :=
  [ ⟨0, 100000, 0⟩, ⟨1000, 100000, 1⟩, ⟨2000, 100000, 4⟩,
    ⟨3000, 100000, 5⟩, ⟨4000, 100000, 30⟩, ⟨9500, 10000, 2⟩,
    ⟨10000, 10000, 0⟩, ⟨11000, 10000, 3⟩ ]

def inputJson (input : Input) : String :=
  let wake := RequestBackendWake.fallback input.now input.deadline input.attempts
  "{\"now_ms\":" ++ toString input.now ++ ",\"deadline_ms\":" ++
    toString input.deadline ++ ",\"attempts\":" ++ toString input.attempts ++
    ",\"wake_ms\":" ++ (wake.map toString).getD "null" ++ "}"

def casesJson : String := jsonArray (inputs.map inputJson)

def captureScopeCases : List (List Nat) :=
  [[], [1], [1, 4, 2], [8, 8, 3], [0, 15]]

def captureScopeCaseJson (scopes : List Nat) : String :=
  "{\"persisted\":" ++ jsonArray (scopes.map toString) ++
    ",\"next\":" ++ toString (RenderedCapture.nextScope scopes) ++ "}"

def captureScopeCasesJson : String := jsonArray (captureScopeCases.map captureScopeCaseJson)

def healthCases : List (Nat × Nat × Option Nat) :=
  [(10, 20, none), (10, 20, some 9), (10, 20, some 10),
   (10, 20, some 20), (10, 20, some 21)]

def healthCaseJson (input : Nat × Nat × Option Nat) : String :=
  let (started, now, observed) := input
  "{\"started_ms\":" ++ toString started ++ ",\"now_ms\":" ++ toString now ++
    ",\"observed_ms\":" ++ (observed.map toString).getD "null" ++
    ",\"fresh\":" ++ (if RequestBackendWake.freshHealthy started now observed then "true" else "false") ++ "}"

def healthCasesJson : String := jsonArray (healthCases.map healthCaseJson)

def generationCases : List (Option Nat × Option Nat) :=
  [(none, none), (none, some 1), (some 1, some 1), (some 1, some 2), (some 2, none)]

def generationCaseJson (input : Option Nat × Option Nat) : String :=
  let (marked, observed) := input
  "{\"marked\":" ++ (marked.map toString).getD "null" ++
    ",\"observed\":" ++ (observed.map toString).getD "null" ++
    ",\"applies\":" ++ (if EventDelivery.Watcher.generationMarkApplies marked observed then "true" else "false") ++ "}"

def generationCasesJson : String := jsonArray (generationCases.map generationCaseJson)

def admissionFacts : List RequestResumeAdmission.ClaimFacts :=
  [ ⟨true, true, true, true, true, true⟩,
    ⟨false, true, true, true, true, true⟩,
    ⟨true, false, true, true, true, true⟩,
    ⟨true, true, false, true, true, true⟩,
    ⟨true, true, true, false, true, true⟩,
    ⟨true, true, true, true, false, true⟩,
    ⟨true, true, true, true, true, false⟩ ]

def boolJson (b : Bool) : String := if b then "true" else "false"

def admissionCaseJson (facts : RequestResumeAdmission.ClaimFacts) : String :=
  "{\"waiting\":" ++ boolJson facts.waiting ++ ",\"version_present\":" ++ boolJson facts.versionPresent ++
  ",\"claimed\":" ++ boolJson facts.claimed ++ ",\"same_identity\":" ++ boolJson facts.sameIdentity ++
  ",\"same_deadline\":" ++ boolJson facts.sameDeadline ++ ",\"same_backend\":" ++ boolJson facts.sameBackend ++
  ",\"admitted\":" ++ boolJson (RequestResumeAdmission.deadlineAdmitted false facts) ++ "}"

def admissionCasesJson : String := jsonArray (admissionFacts.map admissionCaseJson)

structure ReparkInput where
  resuming : Bool
  expectedGeneration : Nat
  expectedExpiry : Nat
  now : Nat

def reparkInputs : List ReparkInput :=
  [ ⟨true, 7, 10, 10⟩, ⟨true, 7, 10, 11⟩, ⟨true, 7, 10, 9⟩,
    ⟨false, 7, 10, 10⟩, ⟨true, 8, 10, 10⟩, ⟨true, 7, 11, 11⟩ ]

def reparkInputJson (input : ReparkInput) : String :=
  let pre := { RequestExecutionLease.initial Nat with
    request := if input.resuming then .resumingBackend else .processing
    lease := .active 7 5 10, usedGenerations := [7], now := input.now }
  let allowed := (RequestExecutionLease.step? pre
    (.reparkExpiredResume .mutationWriteGate input.expectedGeneration input.expectedExpiry)).isSome
  "{\"resuming\":" ++ boolJson input.resuming ++
    ",\"expected_generation\":" ++ toString input.expectedGeneration ++
    ",\"expected_expiry\":" ++ toString input.expectedExpiry ++ ",\"now\":" ++ toString input.now ++
    ",\"allowed\":" ++ boolJson allowed ++ "}"

def reparkCasesJson : String := jsonArray (reparkInputs.map reparkInputJson)

end Conformance.BackendWake
