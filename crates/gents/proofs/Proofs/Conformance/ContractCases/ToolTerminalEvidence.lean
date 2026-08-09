import Proofs.ToolFact.ExecutionSplit

/-! Generated witnesses for terminal tool output-or-omission closure. -/

namespace Conformance.ContractCases

open ToolFact.ExecutionSplit

structure ToolTerminalEvidenceCase where
  name : String
  operation : String
  disposition : String
  evidenceKind : String
  terminalPhase : String
  omissionReason : String
  exactProjection : Bool
  evidenceClosed : Bool
  mutuallyExclusive : Bool
  ownerPreserved : Bool
  phaseReasonValid : Bool
  exactApprovalBound : Bool
  immutableNoop : Bool
  deriving Repr

private def signed (docId cid signer : Nat) : ToolFact.SignedRef :=
  { version := { docId := docId, compositeCommitCid := cid }
  , signerDid := signer
  , signatureValid := true }

private def invocationRef : ToolFact.SignedRef := signed 100 10 7

private def invocationState : State :=
  (commitInvocation State.empty [] { key := 1, argsHash := 101 } invocationRef).state

private def acceptedState (accepted : ToolFact.SignedRef) (phase : ExecutionPhase) : State :=
  (startExecution invocationState []
    { invocation := invocationRef, ownerDid := 8, epoch := 1, phase := phase }
    accepted).state

private def omissionCommit
    (state : State) (accepted omission : ToolFact.SignedRef)
    (reason : OmissionReason) (approval : Option ToolFact.SignedRef := none) :
    CommitObservation :=
  commitOmission state []
    { key := omission.version.docId
    , invocation := invocationRef
    , execution := accepted
    , reason := reason
    , approval := approval }
    omission

private def terminalCommit
    (state : State) (accepted omission terminal : ToolFact.SignedRef)
    (phase : ExecutionPhase) : CommitObservation :=
  terminalizeWithOmission state [accepted.version.compositeCommitCid]
    accepted omission phase terminal

private def runningRef : ToolFact.SignedRef := signed 200 20 8
private def outputRef : ToolFact.SignedRef := signed 300 30 8
private def failedOutputRef : ToolFact.SignedRef := signed 200 21 8
private def runningState : State := acceptedState runningRef .running
private def outputState : State :=
  (commitOutput runningState []
    { key := 300
    , invocation := invocationRef
    , execution := runningRef
    , outputHash := 202
    , fullOutput := true }
    outputRef).state
private def failedOutputCommit : CommitObservation :=
  failExecutionWithOutput outputState [runningRef.version.compositeCommitCid]
    runningRef outputRef failedOutputRef
private def failedOutputState : State := failedOutputCommit.state
private def failedOutputReplay : CommitObservation :=
  failExecutionWithOutput failedOutputState [failedOutputRef.version.compositeCommitCid]
    runningRef outputRef failedOutputRef

private def timeoutAccepted : ToolFact.SignedRef := signed 201 22 8
private def timeoutOmission : ToolFact.SignedRef := signed 401 41 8
private def timeoutTerminal : ToolFact.SignedRef := signed 201 23 8
private def timeoutAcceptedState : State := acceptedState timeoutAccepted .running
private def timeoutOmissionCommit : CommitObservation :=
  omissionCommit timeoutAcceptedState timeoutAccepted timeoutOmission .timedOut
private def timeoutOmissionState : State := timeoutOmissionCommit.state
private def timeoutCommit : CommitObservation :=
  terminalCommit timeoutOmissionState timeoutAccepted timeoutOmission timeoutTerminal .timedOut
private def timeoutState : State := timeoutCommit.state
private def timeoutOmissionReplay : CommitObservation :=
  commitOmission timeoutOmissionState [timeoutOmission.version.docId]
    { key := timeoutOmission.version.docId
    , invocation := invocationRef
    , execution := timeoutAccepted
    , reason := .timedOut }
    timeoutOmission
private def timeoutTerminalReplay : CommitObservation :=
  terminalizeWithOmission timeoutState [timeoutTerminal.version.compositeCommitCid]
    timeoutAccepted timeoutOmission .timedOut timeoutTerminal
private def competingTimeoutOmissionProposal : CommitObservation :=
  commitOmission timeoutAcceptedState [401, 402]
    { key := timeoutOmission.version.docId
    , invocation := invocationRef
    , execution := timeoutAccepted
    , reason := .timedOut }
    timeoutOmission

private def heldTimeoutAccepted : ToolFact.SignedRef := signed 209 40 8
private def heldTimeoutOmission : ToolFact.SignedRef := signed 409 49 8
private def heldTimeoutTerminal : ToolFact.SignedRef := signed 209 41 8
private def heldTimeoutAcceptedState : State := acceptedState heldTimeoutAccepted .awaitingApproval
private def heldTimeoutOmissionCommit : CommitObservation :=
  omissionCommit heldTimeoutAcceptedState heldTimeoutAccepted heldTimeoutOmission .timedOut
private def heldTimeoutCommit : CommitObservation :=
  terminalCommit heldTimeoutOmissionCommit.state heldTimeoutAccepted heldTimeoutOmission
    heldTimeoutTerminal .timedOut

private def cancellationAccepted : ToolFact.SignedRef := signed 202 24 8
private def cancellationOmission : ToolFact.SignedRef := signed 402 42 8
private def cancellationTerminal : ToolFact.SignedRef := signed 202 25 8
private def cancellationAcceptedState : State := acceptedState cancellationAccepted .running
private def cancellationOmissionCommit : CommitObservation :=
  omissionCommit cancellationAcceptedState cancellationAccepted cancellationOmission .cancelled
private def cancellationCommit : CommitObservation :=
  terminalCommit cancellationOmissionCommit.state cancellationAccepted cancellationOmission
    cancellationTerminal .cancelled

private def predispatchAccepted : ToolFact.SignedRef := signed 203 26 8
private def predispatchOmission : ToolFact.SignedRef := signed 403 43 8
private def predispatchTerminal : ToolFact.SignedRef := signed 203 27 8
private def predispatchAcceptedState : State := acceptedState predispatchAccepted .pending
private def predispatchOmissionCommit : CommitObservation :=
  omissionCommit predispatchAcceptedState predispatchAccepted predispatchOmission
    .preDispatchFailure
private def predispatchCommit : CommitObservation :=
  terminalCommit predispatchOmissionCommit.state predispatchAccepted predispatchOmission
    predispatchTerminal .failed

private def failureRefs (offset : Nat) : ToolFact.SignedRef × ToolFact.SignedRef × ToolFact.SignedRef :=
  (signed (204 + offset) (28 + offset * 2) 8,
    signed (404 + offset) (44 + offset) 8,
    signed (204 + offset) (29 + offset * 2) 8)

private def runningFailure
    (offset : Nat) (reason : OmissionReason) : CommitObservation × State × ToolFact.SignedRef :=
  let refs := failureRefs offset
  let accepted := refs.1
  let omission := refs.2.1
  let terminal := refs.2.2
  let state := acceptedState accepted .running
  let omissionResult := omissionCommit state accepted omission reason
  let terminalResult := terminalCommit omissionResult.state accepted omission terminal .failed
  (terminalResult, terminalResult.state, terminal)

private def executionLostResult := runningFailure 0 .executionLost
private def recoveryFailureResult := runningFailure 1 .recoveryFailure
private def childDeadResult := runningFailure 2 .childDead
private def childSupersededResult := runningFailure 3 .childSuperseded

private def deniedAccepted : ToolFact.SignedRef := signed 208 36 8
private def deniedApproval : ToolFact.SignedRef := signed 500 50 9
private def deniedOmission : ToolFact.SignedRef := signed 408 48 8
private def deniedTerminal : ToolFact.SignedRef := signed 208 37 8
private def deniedAcceptedState : State := acceptedState deniedAccepted .awaitingApproval
private def deniedApprovalCommit : CommitObservation :=
  commitApproval deniedAcceptedState []
    { key := 500
    , invocation := invocationRef
    , execution := deniedAccepted
    , decision := .denied
    , reasonHash := 505 }
    deniedApproval
private def deniedOmissionCommit : CommitObservation :=
  omissionCommit deniedApprovalCommit.state deniedAccepted deniedOmission .approvalDenied
    (some deniedApproval)
private def deniedCommit : CommitObservation :=
  terminalCommit deniedOmissionCommit.state deniedAccepted deniedOmission deniedTerminal .failed
private def deniedState : State := deniedCommit.state

private def wrongPhaseReason : CommitObservation :=
  omissionCommit timeoutAcceptedState timeoutAccepted timeoutOmission .preDispatchFailure

private def projectionFlags
    (state : State) (terminal : ToolFact.SignedRef) :
    String × String × String × Bool × Bool × Bool × Bool × Bool × Bool :=
  match projectTerminalEvidence state terminal with
  | none => ("none", "", "", false, false, false, false, false, false)
  | some projection =>
      let evidenceKind := if projection.output.isSome then "output" else "omission"
      let reason := match projection.omission with
        | none => ""
        | some omission => omission.reason.toContract
      let phaseReasonValid := match projection.output, projection.omission with
        | some _, none =>
            projection.accepted.phase == .running &&
              (projection.terminal.phase == .completed || projection.terminal.phase == .failed)
        | none, some omission =>
            decide (omissionTransitionAllowed projection.accepted.phase
              projection.terminal.phase omission.reason)
        | _, _ => false
      ( evidenceKind
      , projection.terminal.phase.toContract
      , reason
      , true
      , (projection.output.isSome && projection.omission.isNone) ||
          (projection.output.isNone && projection.omission.isSome)
      , !(projection.output.isSome && projection.omission.isSome)
      , projection.terminal.ownerDid == projection.accepted.ownerDid &&
          projection.terminal.epoch == projection.accepted.epoch
      , phaseReasonValid
      , (projectDeniedTerminal state terminal).isSome )

private def caseOf
    (name operation : String) (observation : CommitObservation)
    (state : State) (terminal : ToolFact.SignedRef) (immutableNoop : Bool) :
    ToolTerminalEvidenceCase :=
  let flags := projectionFlags state terminal
  { name := name
  , operation := operation
  , disposition := observation.disposition.toContract
  , evidenceKind := flags.1
  , terminalPhase := flags.2.1
  , omissionReason := flags.2.2.1
  , exactProjection := flags.2.2.2.1
  , evidenceClosed := flags.2.2.2.2.1
  , mutuallyExclusive := flags.2.2.2.2.2.1
  , ownerPreserved := flags.2.2.2.2.2.2.1
  , phaseReasonValid := flags.2.2.2.2.2.2.2.1
  , exactApprovalBound := flags.2.2.2.2.2.2.2.2
  , immutableNoop := immutableNoop }

def toolTerminalEvidenceCases : List ToolTerminalEvidenceCase :=
  [ caseOf "failed_execution_retains_full_output" "fail_with_output"
      failedOutputCommit failedOutputState failedOutputRef false
  , caseOf "failed_output_terminal_replay_is_idempotent" "fail_with_output"
      failedOutputReplay failedOutputState failedOutputRef true
  , caseOf "timeout_has_typed_omission" "terminalize_with_omission"
      timeoutCommit timeoutState timeoutTerminal false
  , caseOf "held_timeout_has_typed_omission_without_verdict" "terminalize_with_omission"
      heldTimeoutCommit heldTimeoutCommit.state heldTimeoutTerminal false
  , caseOf "cancellation_has_typed_omission" "terminalize_with_omission"
      cancellationCommit cancellationCommit.state cancellationTerminal false
  , caseOf "predispatch_failure_has_typed_omission" "terminalize_with_omission"
      predispatchCommit predispatchCommit.state predispatchTerminal false
  , caseOf "execution_loss_has_typed_omission" "terminalize_with_omission"
      executionLostResult.1 executionLostResult.2.1 executionLostResult.2.2 false
  , caseOf "recovery_failure_has_typed_omission" "terminalize_with_omission"
      recoveryFailureResult.1 recoveryFailureResult.2.1 recoveryFailureResult.2.2 false
  , caseOf "dead_child_has_typed_omission" "terminalize_with_omission"
      childDeadResult.1 childDeadResult.2.1 childDeadResult.2.2 false
  , caseOf "superseded_child_has_typed_omission" "terminalize_with_omission"
      childSupersededResult.1 childSupersededResult.2.1 childSupersededResult.2.2 false
  , caseOf "approval_denial_pins_approval_and_omission" "deny_with_omission"
      deniedCommit deniedState deniedTerminal false
  , caseOf "omission_replay_is_idempotent" "commit_omission"
      timeoutOmissionReplay timeoutState timeoutTerminal true
  , caseOf "competing_omission_proposal_is_unbound" "commit_omission"
      competingTimeoutOmissionProposal timeoutAcceptedState timeoutTerminal false
  , caseOf "wrong_phase_reason_pair_is_rejected" "commit_omission"
      wrongPhaseReason timeoutAcceptedState timeoutTerminal true
  , caseOf "omission_terminal_replay_is_idempotent" "terminalize_with_omission"
      timeoutTerminalReplay timeoutState timeoutTerminal true ]

theorem toolTerminalEvidenceCases_evidence_pinned :
    toolTerminalEvidenceCases.map (fun row =>
      (row.name, row.disposition, row.evidenceKind, row.terminalPhase,
        row.omissionReason)) =
      [ ("failed_execution_retains_full_output", "applied", "output", "failed", "")
      , ("failed_output_terminal_replay_is_idempotent", "observed_identical", "output", "failed", "")
      , ("timeout_has_typed_omission", "applied", "omission", "timed_out", "timed_out")
      , ("held_timeout_has_typed_omission_without_verdict", "applied", "omission", "timed_out", "timed_out")
      , ("cancellation_has_typed_omission", "applied", "omission", "cancelled", "cancelled")
      , ("predispatch_failure_has_typed_omission", "applied", "omission", "failed", "pre_dispatch_failure")
      , ("execution_loss_has_typed_omission", "applied", "omission", "failed", "execution_lost")
      , ("recovery_failure_has_typed_omission", "applied", "omission", "failed", "recovery_failure")
      , ("dead_child_has_typed_omission", "applied", "omission", "failed", "child_dead")
      , ("superseded_child_has_typed_omission", "applied", "omission", "failed", "child_superseded")
      , ("approval_denial_pins_approval_and_omission", "applied", "omission", "failed", "approval_denied")
      , ("omission_replay_is_idempotent", "observed_identical", "omission", "timed_out", "timed_out")
      , ("competing_omission_proposal_is_unbound", "applied", "none", "", "")
      , ("wrong_phase_reason_pair_is_rejected", "rejected", "none", "", "")
      , ("omission_terminal_replay_is_idempotent", "observed_identical", "omission", "timed_out", "timed_out") ] := by
  native_decide

theorem toolTerminalEvidenceCases_invariants_pinned :
    toolTerminalEvidenceCases.map (fun row =>
      (row.exactProjection, row.evidenceClosed, row.mutuallyExclusive,
        row.ownerPreserved, row.phaseReasonValid)) =
      [ (true, true, true, true, true)
      , (true, true, true, true, true)
      , (true, true, true, true, true)
      , (true, true, true, true, true)
      , (true, true, true, true, true)
      , (true, true, true, true, true)
      , (true, true, true, true, true)
      , (true, true, true, true, true)
      , (true, true, true, true, true)
      , (true, true, true, true, true)
      , (true, true, true, true, true)
      , (true, true, true, true, true)
      , (false, false, false, false, false)
      , (false, false, false, false, false)
      , (true, true, true, true, true) ] := by
  native_decide

theorem toolTerminalEvidenceCases_binding_pinned :
    toolTerminalEvidenceCases.map (fun row => (row.exactApprovalBound, row.immutableNoop)) =
      [ (false, false), (false, true), (false, false), (false, false)
      , (false, false), (false, false), (false, false), (false, false)
      , (false, false), (false, false), (true, false), (false, true)
      , (false, false), (false, true), (false, true) ] := by
  native_decide

end Conformance.ContractCases
