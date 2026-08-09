import Proofs.ToolFact.ExecutionSplit

/-! Generated witnesses for exact full-output/model-projection authority. -/

namespace Conformance.ContractCases

open ToolFact.ExecutionSplit

structure ToolOutputProjectionCase where
  name : String
  observation : String
  observedHash : Nat
  accepted : Bool
  fullOutputPreserved : Bool
  deriving Repr

private def signed (docId cid signer : Nat) : ToolFact.SignedRef :=
  { version := { docId := docId, compositeCommitCid := cid }
  , signerDid := signer
  , signatureValid := true }

private def invocationRef : ToolFact.SignedRef := signed 100 10 7
private def runningRef : ToolFact.SignedRef := signed 200 20 8
private def outputRef : ToolFact.SignedRef := signed 300 30 8

private def invocationState : State :=
  (commitInvocation State.empty [] { key := 1, argsHash := 101 } invocationRef).state

private def runningState : State :=
  (startExecution invocationState []
    { invocation := invocationRef, ownerDid := 8, epoch := 1, phase := .running }
    runningRef).state

private def outputState : State :=
  (commitOutput runningState []
    { key := 2
    , invocation := invocationRef
    , execution := runningRef
    , outputHash := 202
    , truncationContractHash := 101
    , modelProjectionHash := 303
    , fullOutput := true }
    outputRef).state

private def projectionCase
    (name observation : String) (observedHash : Nat) : ToolOutputProjectionCase :=
  match exactOutput? outputState.outputs outputRef with
  | none =>
      { name := name
      , observation := observation
      , observedHash := observedHash
      , accepted := false
      , fullOutputPreserved := false }
  | some output =>
      { name := name
      , observation := observation
      , observedHash := observedHash
      , accepted := (exactModelProjection? output observedHash).isSome
      , fullOutputPreserved := output.fullOutput && output.outputHash == 202 }

def toolOutputProjectionCases : List ToolOutputProjectionCase :=
  [ projectionCase "canonical_bounded_projection_is_accepted" "canonical" 303
  , projectionCase "full_output_hash_is_not_a_model_projection" "full_output" 202
  , projectionCase "forged_terminal_projection_is_rejected" "forged" 999 ]

theorem toolOutputProjectionCases_pinned :
    toolOutputProjectionCases.map (fun row =>
      (row.name, row.observation, row.observedHash, row.accepted,
        row.fullOutputPreserved)) =
      [ ("canonical_bounded_projection_is_accepted", "canonical", 303, true, true)
      , ("full_output_hash_is_not_a_model_projection", "full_output", 202, false, true)
      , ("forged_terminal_projection_is_rejected", "forged", 999, false, true) ] := by
  native_decide

end Conformance.ContractCases
