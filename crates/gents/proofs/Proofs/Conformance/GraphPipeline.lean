import Proofs.GraphPipeline
import Proofs.Conformance.ContractTypes

namespace Conformance.GraphPipelineContracts

open Conformance.Contracts

structure ValidationCase where
  name : String
  typesValid : Bool
  topologyValid : Bool
  capabilitiesAuthorized : Bool
  withinBounds : Bool
  terminalResultDeclared : Bool
  expectedValid : Bool
  deriving DecidableEq, Repr

def boolValues : List Bool := [false, true]

def validationCases : List ValidationCase :=
  boolValues.flatMap fun typesValid =>
    boolValues.flatMap fun topologyValid =>
      boolValues.flatMap fun capabilitiesAuthorized =>
        boolValues.flatMap fun withinBounds =>
          boolValues.map fun terminalResultDeclared =>
          { name :=
              "types=" ++ toString typesValid ++
              ",topology=" ++ toString topologyValid ++
              ",authorized=" ++ toString capabilitiesAuthorized ++
              ",bounds=" ++ toString withinBounds ++
              ",terminal_result=" ++ toString terminalResultDeclared
          , typesValid := typesValid
          , topologyValid := topologyValid
          , capabilitiesAuthorized := capabilitiesAuthorized
          , withinBounds := withinBounds
          , terminalResultDeclared := terminalResultDeclared
          , expectedValid :=
              typesValid && topologyValid && capabilitiesAuthorized && withinBounds &&
                terminalResultDeclared
          }

theorem validationCases_count : validationCases.length = 32 := by native_decide

private def boolJson (value : Bool) : String :=
  if value then "true" else "false"

def validationCaseJson (testCase : ValidationCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString testCase.name ++ ","
    ++ "\"types_valid\":" ++ boolJson testCase.typesValid ++ ","
    ++ "\"topology_valid\":" ++ boolJson testCase.topologyValid ++ ","
    ++ "\"capabilities_authorized\":" ++ boolJson testCase.capabilitiesAuthorized ++ ","
    ++ "\"within_bounds\":" ++ boolJson testCase.withinBounds ++ ","
    ++ "\"terminal_result_declared\":" ++
      boolJson testCase.terminalResultDeclared ++ ","
    ++ "\"expected_valid\":" ++ boolJson testCase.expectedValid
    ++ "}"

def validationCasesJson : String :=
  jsonArray (validationCases.map validationCaseJson)

structure RevisionGateCase where
  name : String
  status : String
  artifactsComplete : Bool
  activationPreconditionMet : Bool
  pointerMatches : Bool
  expectedActivate : Bool
  expectedStart : Bool
  deriving DecidableEq, Repr

def revisionStatuses : List String := ["draft", "validated", "active", "retired"]

def revisionGateCases : List RevisionGateCase :=
  revisionStatuses.flatMap fun status =>
    boolValues.flatMap fun artifactsComplete =>
      boolValues.flatMap fun activationPreconditionMet =>
        boolValues.map fun pointerMatches =>
          { name :=
              "status=" ++ status ++
                ",complete=" ++ toString artifactsComplete ++
                ",activation_precondition=" ++ toString activationPreconditionMet ++
                ",pointer_matches=" ++ toString pointerMatches
          , status := status
          , artifactsComplete := artifactsComplete
          , activationPreconditionMet := activationPreconditionMet
          , pointerMatches := pointerMatches
          , expectedActivate :=
              status == "validated" && artifactsComplete && activationPreconditionMet
          , expectedStart := status == "active" && artifactsComplete && pointerMatches
          }

theorem revisionGateCases_count : revisionGateCases.length = 32 := by native_decide

def revisionGateCaseJson (testCase : RevisionGateCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString testCase.name ++ ","
    ++ "\"status\":" ++ jsonString testCase.status ++ ","
    ++ "\"artifacts_complete\":" ++ boolJson testCase.artifactsComplete ++ ","
    ++ "\"activation_precondition_met\":" ++
      boolJson testCase.activationPreconditionMet ++ ","
    ++ "\"pointer_matches\":" ++ boolJson testCase.pointerMatches ++ ","
    ++ "\"expected_activate\":" ++ boolJson testCase.expectedActivate ++ ","
    ++ "\"expected_start\":" ++ boolJson testCase.expectedStart
    ++ "}"

def revisionGateCasesJson : String :=
  jsonArray (revisionGateCases.map revisionGateCaseJson)

structure RunTerminalCase where
  name : String
  status : String
  cancellationRequested : Bool
  resultContractSatisfied : Bool
  activeWorkTerminal : Bool
  failureProven : Bool
  expectedSucceed : Bool
  expectedFail : Bool
  expectedCancel : Bool
  deriving DecidableEq, Repr

def runStatuses : List (GraphPipeline.RunStatus × String) :=
  [ (.running, "running")
  , (.succeeded, "succeeded")
  , (.failed, "failed")
  , (.cancelled, "cancelled")
  ]

private def runState
    (status : GraphPipeline.RunStatus)
    (cancellationRequested : Bool) : GraphPipeline.State :=
  { revision :=
      { graphId := 1
      , revisionId := 2
      , digest := 3
      , status := .active
      , typesValid := true
      , topologyValid := true
      , capabilitiesAuthorized := true
      , withinBounds := true
      , terminalResultDeclared := true
      , artifactsComplete := true
      }
  , activeRevision := some 2
  , run := some
      { runId := 4
      , graphId := 1
      , revisionId := 2
      , revisionDigest := 3
      , status := status
      , seedCommitted := true
      , cancellationRequested := cancellationRequested
      , resultsCommitted := false
      }
  }

private def transitionAllowed
    (state : GraphPipeline.State)
    (action : GraphPipeline.Action) : Bool :=
  (GraphPipeline.step? state action).isSome

def runTerminalCases : List RunTerminalCase :=
  runStatuses.flatMap fun (status, statusName) =>
    boolValues.flatMap fun cancellationRequested =>
      boolValues.flatMap fun resultContractSatisfied =>
      boolValues.flatMap fun activeWorkTerminal =>
        boolValues.map fun failureProven =>
            let state := runState status cancellationRequested
            { name :=
                "status=" ++ statusName ++
                  ",cancel_requested=" ++ toString cancellationRequested ++
                  ",results_satisfied=" ++ toString resultContractSatisfied ++
                  ",work_terminal=" ++ toString activeWorkTerminal ++
                  ",failure_proven=" ++ toString failureProven
            , status := statusName
            , cancellationRequested := cancellationRequested
            , resultContractSatisfied := resultContractSatisfied
            , activeWorkTerminal := activeWorkTerminal
            , failureProven := failureProven
            , expectedSucceed :=
                transitionAllowed state
                  (.succeedRun resultContractSatisfied activeWorkTerminal)
            , expectedFail := transitionAllowed state (.failRun failureProven)
            , expectedCancel := transitionAllowed state (.cancelRun activeWorkTerminal)
            }

theorem runTerminalCases_count : runTerminalCases.length = 64 := by native_decide

def runTerminalCaseJson (testCase : RunTerminalCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString testCase.name ++ ","
    ++ "\"status\":" ++ jsonString testCase.status ++ ","
    ++ "\"cancellation_requested\":" ++
      boolJson testCase.cancellationRequested ++ ","
    ++ "\"result_contract_satisfied\":" ++
      boolJson testCase.resultContractSatisfied ++ ","
    ++ "\"active_work_terminal\":" ++ boolJson testCase.activeWorkTerminal ++ ","
    ++ "\"failure_proven\":" ++ boolJson testCase.failureProven ++ ","
    ++ "\"expected_succeed\":" ++ boolJson testCase.expectedSucceed ++ ","
    ++ "\"expected_fail\":" ++ boolJson testCase.expectedFail ++ ","
    ++ "\"expected_cancel\":" ++ boolJson testCase.expectedCancel
    ++ "}"

def runTerminalCasesJson : String :=
  jsonArray (runTerminalCases.map runTerminalCaseJson)

end Conformance.GraphPipelineContracts
