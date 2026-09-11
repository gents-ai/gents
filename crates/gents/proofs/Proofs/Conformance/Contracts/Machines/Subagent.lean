import Proofs.Background.Bridge
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

/-- Failure observations project into tool terminal states; this is a mapping,
not an independent child lifecycle. Values come from the bridge owner. -/
def childFailureObservations : List Subagent.ChildTerminal :=
  [.failed, .dead, .interrupted, .superseded]

def childFailureNames : List String :=
  childFailureObservations.map Subagent.ChildTerminal.toDefraDB

/-- Execute the bridge owner's failure projection rather than hand-writing a
second transition table over mismatched child/tool state domains. -/
def childFailureProjectionsJson : String :=
  jsonArray (childFailureObservations.map fun child =>
    "{" ++ "\"child_state\":" ++ jsonString child.toDefraDB ++ ","
      ++ "\"tool_state\":" ++ jsonString child.projectedToolState.toDefraDB ++ "}")

def toolRetryDispositions : List ToolExecution.RetryDisposition :=
  ToolExecution.RetryDisposition.all

def toolRetryDispositionNames : List String :=
  toolRetryDispositions.map ToolExecution.RetryDisposition.toDefraDB

def failureClassNames : List String :=
  ToolExecution.FailureClass.all.map ToolExecution.FailureClass.toDefraDB

end Conformance.Contracts
