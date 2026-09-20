import Proofs.CompletionRetry.CanonicalGate
import Proofs.CanonicalOutput.Execution.Examples

namespace CompletionRetry.CanonicalGate.Cases

open CanonicalOutput
open CanonicalOutput.Execution.Examples

def retry (phase : CompletionRetry.Phase) (attempt : Nat := 0) : CompletionRetry.State :=
  { request := 10, scope := 0, turn := 0, phase
    budget := { transportRetries := 2, resampleRetries := 1, allowRepair := true }
    transportUsed := 0, resampleUsed := 0, repairUsed := false
    lastParseError := none, now := 5, deadline := some 10, attempt, usageCharged := 0 }

def held (phase : CompletionRetry.Phase) (attempt : Nat := 0) : Option State := do
  let gate ← CanonicalOutput.Execution.Gate.acquire
    (CanonicalOutput.Execution.Gate.initial (world 5)) 1 true
  pure ⟨gate, retry phase attempt⟩

def acceptance : Operation :=
  .accept 7 (emptyClose 100 0 .complete 5) (emptyAssistant 200 5) [] []

def acceptanceCommitsPolicyAndCanonicalOutput : Bool :=
  match held .streaming with
  | none => false
  | some before => match commit before 1 5 acceptance with
    | .error _ => false
    | .ok after => after.retry.phase == .accepted 200 &&
        (emptyAssistant 200 5) ∈ after.gate.execution.messages

theorem acceptance_commits_policy_and_canonical_output :
    acceptanceCommitsPolicyAndCanonicalOutput = true := by native_decide

def exhaustedCannotAccept : Bool :=
  match held .exhausted with
  | none => false
  | some before => match commit before 1 5 acceptance with
    | .error .policy => true | _ => false

theorem exhausted_policy_cannot_publish : exhaustedCannotAccept = true := by native_decide

def staleAttemptCannotAccept : Bool :=
  match held .streaming 1 with
  | none => false
  | some before => match commit before 1 5 acceptance with
    | .error .source => true | _ => false

theorem stale_attempt_cannot_publish : staleAttemptCannotAccept = true := by native_decide

def staleClockCannotAccept : Bool :=
  match held .streaming with
  | none => false
  | some before =>
      let stale := { before with retry := { before.retry with now := 6 } }
      match commit stale 1 5 acceptance with
      | .error .clock => true | _ => false

theorem stale_retry_clock_cannot_publish : staleClockCannotAccept = true := by native_decide

def exactAcceptanceReplay : Bool :=
  match held .streaming with
  | none => false
  | some before => match commit before 1 5 acceptance with
    | .error _ => false
    | .ok accepted =>
      match CanonicalOutput.Execution.Gate.scheduling accepted.gate 1 .release with
      | none => false
      | some released =>
        match CanonicalOutput.Execution.Gate.acquire released 1 true with
        | none => false
        | some reacquired =>
          match commit { accepted with gate := reacquired } 1 5 acceptance with
          | .error _ => false
          | .ok replayed => replayed.retry == accepted.retry &&
              replayed.gate.execution == accepted.gate.execution

theorem exact_acceptance_replay_is_inert : exactAcceptanceReplay = true := by native_decide

def retractionClose : Segment :=
  { id := 101, coordinate := ⟨10, .provider 0 0 0⟩, writer := .request 7
    flush := none, close := some .retracted, createdAt := 5 }

def heldForRetraction : Option State := do
  let execution := world 5 [raw 100 0 0 5]
  let gate ← CanonicalOutput.Execution.Gate.acquire
    (CanonicalOutput.Execution.Gate.initial execution) 1 true
  pure ⟨gate, retry (.retractRequired .transport "io" 6)⟩

def retractionAndReplay : Bool :=
  match heldForRetraction with
  | none => false
  | some before => match commit before 1 5 (.retract 7 retractionClose) with
    | .error _ => false
    | .ok retracted =>
      match CanonicalOutput.Execution.Gate.scheduling retracted.gate 1 .release with
      | none => false
      | some released =>
        match CanonicalOutput.Execution.Gate.acquire released 1 true with
        | none => false
        | some reacquired =>
          match commit { retracted with gate := reacquired } 1 5 (.retract 7 retractionClose) with
          | .error _ => false
          | .ok replayed =>
              replayed.retry.phase == .retracted .transport "io" 6 &&
                replayed.retry == retracted.retry &&
                replayed.gate.execution == retracted.gate.execution

theorem retraction_and_exact_replay_share_policy_and_gate : retractionAndReplay = true := by
  native_decide

def retryScheduleWakeTrace : Bool :=
  match heldForRetraction with
  | none => false
  | some before => match commit before 1 5 (.retract 7 retractionClose) with
    | .error _ => false
    | .ok retracted =>
      let schedule : PolicyOperation := ⟨.schedule, rfl⟩
      match stepPolicy retracted 5 schedule with
      | none => false
      | some backingOff =>
        let wake : PolicyOperation := ⟨.wake 6, rfl⟩
        match stepPolicy backingOff 6 wake with
        | none => false
        | some issuing => issuing.retry.phase == .issuing && issuing.retry.attempt == 1 &&
            issuing.retry.now == 6 && issuing.gate == retracted.gate

theorem retry_schedule_and_wake_preserve_gate_and_clock : retryScheduleWakeTrace = true := by
  native_decide

def scheduledBackoff : Option State := do
  let before ← heldForRetraction
  let retracted ← (commit before 1 5 (.retract 7 retractionClose)).toOption
  stepPolicy retracted 5 ⟨.schedule, rfl⟩

def delayedWakeSucceeds : Bool :=
  match scheduledBackoff with
  | none => false
  | some backingOff =>
      match stepPolicy backingOff 8 ⟨.wake 8, rfl⟩ with
      | some issuing => issuing.retry.phase == .issuing && issuing.retry.now == 8
      | none => false

theorem delayed_wake_uses_observed_monotone_time : delayedWakeSucceeds = true := by
  native_decide

def expiredWakeRejected : Bool :=
  match scheduledBackoff with
  | none => false
  | some backingOff => (stepPolicy backingOff 11 ⟨.wake 11, rfl⟩).isNone

theorem wake_after_retry_deadline_is_rejected : expiredWakeRejected = true := by
  native_decide

def nonClosureGateOperationRemainsAvailable : Bool :=
  match held .streaming with
  | none => false
  | some before =>
    let renewal : GateOperation := ⟨.renew 7 10, rfl⟩
    match commitGate before 1 8 renewal with
    | none => false
    | some after => after.retry == before.retry &&
        after.gate.execution.lease.lease == .active 7 5 13

theorem restricted_gate_surface_keeps_nonclosure_operations :
    nonClosureGateOperationRemainsAvailable = true := by native_decide

end CompletionRetry.CanonicalGate.Cases
