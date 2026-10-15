import Proofs.Eval
import Proofs.Conformance.ContractTypes

namespace Conformance.EvalRequestOutcome

open _root_.Eval Conformance.Contracts

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def reasons : List (Option String) :=
  [none, some "execution lease expired",
    some "execution lease expired or ownership was revoked",
    some "request execution lease: execution lease expired or ownership was revoked",
    some "provider execution lease expired", some "execution lease expired unexpectedly",
    some "execution lease expired before renewal commit admission", some "503"]

private def states : List RequestState :=
  [.completed, .failed, .interrupted, .dead, .superseded, .processing]

private def row (state : RequestState) (deadline budget inference tool : Bool)
    (requestReason inferenceReason : Option String) : String :=
  let requestReason := if budget then some "invalid_tool_call_budget_exhausted" else requestReason
  let evidence : RequestEvidence := ⟨state, deadline, budget,
    (requestReason.toList ++ inferenceReason.toList).any executionLeaseFailure,
    inference || inferenceReason.any (· != ""), tool⟩
  let outcome := classifyRequestOutcome evidence
  "{\"state\":" ++ jsonString state.toDefraDB ++
  ",\"deadline\":" ++ boolJson deadline ++
  ",\"inference_failed\":" ++ boolJson inference ++
  ",\"tool_failed\":" ++ boolJson tool ++
  ",\"request_reason\":" ++ jsonOptionalString requestReason ++
  ",\"inference_reason\":" ++ jsonOptionalString inferenceReason ++
  ",\"outcome\":" ++ jsonOptionalString (outcome.map OutcomeKind.toDefraDB) ++ "}"

def casesJson : String := jsonArray <|
  (states.flatMap fun state =>
    [false, true].flatMap fun deadline =>
      [false, true].flatMap fun budget =>
        [false, true].flatMap fun inference =>
          [false, true].flatMap fun tool =>
            [(none, none), (some "execution lease expired", none),
              (none, some "request execution lease: execution lease expired or ownership was revoked"),
              (some "execution lease expired", some "503")].map fun (requestReason, inferenceReason) =>
                row state deadline budget inference tool requestReason inferenceReason) ++
  (reasons.flatMap fun reason =>
    [row .failed false false false false reason none,
      row .failed false false false false none reason])

end Conformance.EvalRequestOutcome

def main : IO Unit := IO.println Conformance.EvalRequestOutcome.casesJson
