import Proofs.ToolExecution
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

def awaitModeMachine : StateMachineContract :=
  let names := Subagent.AwaitMode.all.map Subagent.AwaitMode.toDefraDB
  machineContract
    "AwaitMode"
    names
    []
    []
    []

def cancelPolicyMachine : StateMachineContract :=
  let names := Subagent.CancelPolicy.all.map Subagent.CancelPolicy.toDefraDB
  machineContract
    "CancelPolicy"
    names
    []
    []
    []

def childTerminalMachine : StateMachineContract :=
  let base :=
    machineContract
      "ChildTerminal"
      ["failed", "dead", "interrupted", "superseded"]
      ["failed", "dead", "interrupted", "superseded"]
      []
      []
  { base with
      namedTransitions :=
        [ { name := "project_failed"
          , source := "failed"
          , target := "failed" }
        , { name := "project_dead"
          , source := "dead"
          , target := "failed" }
        , { name := "project_interrupted"
          , source := "interrupted"
          , target := "cancelled" }
        , { name := "project_superseded"
          , source := "superseded"
          , target := "failed" }
        ] }

def toolRetryDispositions : List ToolExecution.RetryDisposition :=
  ToolExecution.RetryDisposition.all

def toolRetryDispositionNames : List String :=
  toolRetryDispositions.map ToolExecution.RetryDisposition.toDefraDB

def failureClassNames : List String :=
  ToolExecution.FailureClass.all.map ToolExecution.FailureClass.toDefraDB

end Conformance.Contracts
