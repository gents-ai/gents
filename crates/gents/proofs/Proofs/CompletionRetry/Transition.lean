import Proofs.CompletionRetry.State

namespace CompletionRetry

inductive Action where
  | issue
  | observeFailure (failure : FailureClass) (error : String) (wake : Time)
  /-- Enabled only after CanonicalOutput.Execution.retractBeforeRetry has
  durably appended the Retracted closure. -/
  | confirmRetraction (durable : Bool)
  | schedule
  /-- Observe the timer at the actual monotone owner time. The scheduled lower
  bound remains in `.backingOff`; timer overshoot is not rejection by itself. -/
  | wake (observedAt : Time)
  /-- CanonicalOutput.Execution.acceptAndPublish supplies this accepted header. -/
  | accept (header : Nat)
  | acceptedToolFailure
  | repairIssue
  /-- Observe usage already accounted by the InferenceCall owner. The native
  accounting bridge supplies completeness and replay idempotency. -/
  | recordUsage (tokens : Nat)
  deriving DecidableEq, Repr

def step? (s : State) : Action → Option State
  | .issue =>
      if s.phase = .issuing then some { s with phase := .streaming } else none
  | .observeFailure failure error wake =>
      if s.phase = .streaming then
        match failure with
        | .permanent => some { s with phase := .failedPermanent }
        | retryable => some { s with phase := .retractRequired retryable error wake }
      else none
  | .confirmRetraction durable =>
      match s.phase with
      | .retractRequired failure error wake =>
          if durable then some { s with phase := .retracted failure error wake } else none
      | _ => none
  | .schedule =>
      match s.phase with
      | .retracted .transport _ wake =>
          if s.transportUsed < s.budget.transportRetries ∧
              fitsDeadline wake s.deadline ∧ s.now ≤ wake then
            some { s with phase := .backingOff wake,
                          transportUsed := s.transportUsed + 1,
                          attempt := s.attempt + 1 }
          else some { s with phase := .exhausted }
      | .retracted .parseBadRequest error wake =>
          if s.lastParseError ≠ some error ∧
              s.resampleUsed < s.budget.resampleRetries ∧
              fitsDeadline wake s.deadline ∧ s.now ≤ wake then
            some { s with phase := .backingOff wake,
                          resampleUsed := s.resampleUsed + 1,
                          lastParseError := some error,
                          attempt := s.attempt + 1 }
          else if (s.lastParseError = some error ∨
                    s.resampleUsed ≥ s.budget.resampleRetries) ∧
                  s.budget.allowRepair ∧ ¬ s.repairUsed then
            some { s with phase := .repairing, lastParseError := some error }
          else some { s with phase := .exhausted }
      | .retracted .permanent _ _ => some { s with phase := .failedPermanent }
      | _ => none
  | .wake observedAt =>
      match s.phase with
      | .backingOff scheduled =>
          if scheduled ≤ observedAt ∧ s.now ≤ observedAt ∧ fitsDeadline observedAt s.deadline then
            some { s with phase := .issuing, now := observedAt }
          else none
      | _ => none
  | .accept header =>
      if s.phase = .streaming then some { s with phase := .accepted header } else none
  | .acceptedToolFailure =>
      match s.phase with
      | .accepted header => some { s with phase := .acceptedToolFailed header }
      | _ => none
  | .repairIssue =>
      if s.phase = .repairing ∧ ¬ s.repairUsed then
        some { s with phase := .issuing, repairUsed := true, attempt := s.attempt + 1 }
      else none
  | .recordUsage tokens =>
      some { s with usageCharged := s.usageCharged + tokens }

/-- The executable relation is the policy surface. Durable retraction and atomic
acceptance are composition preconditions supplied by CanonicalOutput.Execution,
not re-proved from retry metadata. -/
def Transition (before after : State) : Prop :=
  ∃ action, step? before action = some after

inductive Trace : State → State → Prop
  | refl (state) : Trace state state
  | step {before middle after} : Transition before middle → Trace middle after →
      Trace before after

end CompletionRetry
