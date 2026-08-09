import Proofs.ToolFact.ExecutionSplit

/-! Generated witnesses for the exact invocation/execution/output/approval split. -/

namespace Conformance.ContractCases

open ToolFact.ExecutionSplit

structure ToolExecutionSplitCase where
  name : String
  operation : String
  disposition : String
  exactProjection : Bool
  outputPinsRunning : Bool
  terminalOutputClosed : Bool
  ownerPreserved : Bool
  approvalPinsHeld : Bool
  immutableNoop : Bool
  deriving Repr

private def signed (docId cid signer : Nat) : ToolFact.SignedRef :=
  { version := { docId := docId, compositeCommitCid := cid }
  , signerDid := signer
  , signatureValid := true }

private def invocationRef : ToolFact.SignedRef := signed 100 10 7
private def runningRef : ToolFact.SignedRef := signed 200 20 8
private def outputRef : ToolFact.SignedRef := signed 300 30 8
private def terminalRef : ToolFact.SignedRef := signed 200 21 8
private def heldRef : ToolFact.SignedRef := signed 201 22 8
private def approvalRef : ToolFact.SignedRef := signed 400 40 9
private def approvedRunningRef : ToolFact.SignedRef := signed 201 23 8
private def approvedOutputRef : ToolFact.SignedRef := signed 301 31 8
private def approvedTerminalRef : ToolFact.SignedRef := signed 201 24 8

private def invocationIntent : ToolInvocationIntent :=
  { key := 1, argsHash := 101 }

private def invocationCommit : CommitObservation :=
  commitInvocation State.empty [] invocationIntent invocationRef

private def invocationState : State := invocationCommit.state

private def runningIntent : ToolExecutionIntent :=
  { invocation := invocationRef, ownerDid := 8, epoch := 1, phase := .running }

private def runningCommit : CommitObservation :=
  startExecution invocationState [] runningIntent runningRef

private def runningState : State := runningCommit.state

private def outputIntent : ToolOutputIntent :=
  { key := 2
  , invocation := invocationRef
  , execution := runningRef
  , outputHash := 202
  , fullOutput := true }

private def outputCommit : CommitObservation :=
  commitOutput runningState [] outputIntent outputRef

private def outputState : State := outputCommit.state

private def completionCommit : CommitObservation :=
  completeExecution outputState [runningRef.version.compositeCommitCid]
    runningRef outputRef terminalRef

private def completedState : State := completionCommit.state

private def completionReplay : CommitObservation :=
  completeExecution completedState [terminalRef.version.compositeCommitCid]
    runningRef outputRef terminalRef

private def completionHeadConflict : CommitObservation :=
  completeExecution outputState [999] runningRef outputRef terminalRef

private def competingOutputProposal : CommitObservation :=
  commitOutput runningState [300, 301] outputIntent outputRef

private def wrongExecutionOutput : CommitObservation :=
  commitOutput runningState [] { outputIntent with execution := terminalRef } outputRef

private def heldIntent : ToolExecutionIntent :=
  { invocation := invocationRef, ownerDid := 8, epoch := 2, phase := .awaitingApproval }

private def heldCommit : CommitObservation :=
  startExecution invocationState [] heldIntent heldRef

private def heldState : State := heldCommit.state

private def approvalIntent : ToolApprovalIntent :=
  { key := 3
  , invocation := invocationRef
  , execution := heldRef
  , decision := .approved
  , reasonHash := 303 }

private def approvalCommit : CommitObservation :=
  commitApproval heldState [] approvalIntent approvalRef

private def approvalState : State := approvalCommit.state

private def approvalReplay : CommitObservation :=
  commitApproval approvalState [approvalRef.version.docId] approvalIntent approvalRef

private def wrongHeldApproval : CommitObservation :=
  commitApproval runningState [] { approvalIntent with execution := runningRef } approvalRef

private def approvedRunningCommit : CommitObservation :=
  approveExecution approvalState [heldRef.version.compositeCommitCid]
    heldRef approvalRef approvedRunningRef

private def approvedRunningState : State := approvedRunningCommit.state

private def approvedOutputIntent : ToolOutputIntent :=
  { key := 4
  , invocation := invocationRef
  , execution := approvedRunningRef
  , outputHash := 404
  , fullOutput := true }

private def approvedOutputCommit : CommitObservation :=
  commitOutput approvedRunningState [] approvedOutputIntent approvedOutputRef

private def approvedOutputState : State := approvedOutputCommit.state

private def approvedCompletionCommit : CommitObservation :=
  completeExecution approvedOutputState [approvedRunningRef.version.compositeCommitCid]
    approvedRunningRef approvedOutputRef approvedTerminalRef

private def approvedCompletedState : State := approvedCompletionCommit.state

private def projectionFlags (state : State) (terminal : ToolFact.SignedRef) : Bool × Bool × Bool × Bool × Bool :=
  match projectCompleted state terminal with
  | none => (false, false, false, false, false)
  | some projection =>
      ( true
      , projection.output.execution == projection.running.signed
      , projection.terminal.phase == .completed &&
          projection.terminal.previous == some projection.running.signed &&
          projection.terminal.output == some projection.output.signed
      , projection.terminal.ownerDid == projection.running.ownerDid &&
          projection.terminal.epoch == projection.running.epoch
      , match projection.approval with
        | none => false
        | some approval => projection.running.previous == some approval.execution )

private def caseOf
    (name operation : String)
    (observation : CommitObservation)
    (projectionState : State)
    (terminal : ToolFact.SignedRef)
    (immutableNoop : Bool) : ToolExecutionSplitCase :=
  let flags := projectionFlags projectionState terminal
  { name := name
  , operation := operation
  , disposition := observation.disposition.toContract
  , exactProjection := flags.1
  , outputPinsRunning := flags.2.1
  , terminalOutputClosed := flags.2.2.1
  , ownerPreserved := flags.2.2.2.1
  , approvalPinsHeld := flags.2.2.2.2
  , immutableNoop := immutableNoop }

def toolExecutionSplitCases : List ToolExecutionSplitCase :=
  [ caseOf "completion_requires_exact_running_output" "complete_execution"
      completionCommit completedState terminalRef false
  , caseOf "completion_replay_is_idempotent" "complete_execution"
      completionReplay completedState terminalRef true
  , caseOf "completion_rejects_non_current_head" "complete_execution"
      completionHeadConflict outputState terminalRef true
  , caseOf "competing_output_proposal_is_unbound" "commit_output"
      competingOutputProposal runningState terminalRef false
  , caseOf "output_rejects_wrong_execution_version" "commit_output"
      wrongExecutionOutput runningState terminalRef true
  , caseOf "approval_chain_pins_held_execution" "approve_and_complete"
      approvedCompletionCommit approvedCompletedState approvedTerminalRef false
  , caseOf "approval_replay_is_idempotent" "commit_approval"
      approvalReplay approvalState approvedTerminalRef true
  , caseOf "approval_rejects_non_held_execution" "commit_approval"
      wrongHeldApproval runningState approvedTerminalRef true ]

theorem toolExecutionSplitCases_pinned :
    toolExecutionSplitCases.map (fun row =>
      (row.name, row.disposition, row.exactProjection, row.outputPinsRunning,
        row.terminalOutputClosed)) =
      [ ("completion_requires_exact_running_output", "applied", true, true, true)
      , ("completion_replay_is_idempotent", "observed_identical", true, true, true)
      , ("completion_rejects_non_current_head", "rejected", false, false, false)
      , ("competing_output_proposal_is_unbound", "applied", false, false, false)
      , ("output_rejects_wrong_execution_version", "rejected", false, false, false)
      , ("approval_chain_pins_held_execution", "applied", true, true, true)
      , ("approval_replay_is_idempotent", "observed_identical", false, false, false)
      , ("approval_rejects_non_held_execution", "rejected", false, false, false) ] := by
  native_decide

theorem toolExecutionSplitCases_binding_flags_pinned :
    toolExecutionSplitCases.map (fun row =>
      (row.ownerPreserved, row.approvalPinsHeld, row.immutableNoop)) =
      [ (true, false, false)
      , (true, false, true)
      , (false, false, true)
      , (false, false, false)
      , (false, false, true)
      , (true, true, false)
      , (false, false, true)
      , (false, false, true) ] := by
  native_decide

end Conformance.ContractCases
