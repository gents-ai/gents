import Proofs.CompletionRetry.Properties

namespace CompletionRetry

def defaultBudget : Budget :=
  { transportRetries := 3, resampleRetries := 2, allowRepair := true }

def baseState
    (request : CanonicalOutput.DocId := 10)
    (scope : Nat := 0)
    (turn : Nat := 0)
    (phase : Phase := .streaming)
    (budget : Budget := defaultBudget)
    (transportUsed : Nat := 0)
    (resampleUsed : Nat := 0)
    (repairUsed : Bool := false)
    (lastParseError : Option String := none)
    (now : Time := 10)
    (deadline : Option Time := none)
    (attempt : Nat := 0)
    (usageCharged : Nat := 0) : State :=
  { request, scope, turn, phase, budget, transportUsed, resampleUsed, repairUsed, lastParseError,
    now, deadline, attempt, usageCharged }

structure RetryCase where
  name : String
  pre : State
  action : Action
  post : Option State
  deriving Repr

private def witness (name : String) (pre : State) (action : Action) : RetryCase :=
  { name, pre, action, post := step? pre action }

def retryCases : List RetryCase :=
  let transportRequired := step? baseState (.observeFailure .transport "io" 12)
  let transportRetracted := transportRequired.bind fun state =>
    step? state (.confirmRetraction true)
  let accepted := step? baseState (.accept 200)
  let repairIssued := step? { baseState with phase := .repairing } .repairIssue
  [ witness "transport_failure_requires_retraction" baseState
      (.observeFailure .transport "io" 12)
  , witness "cannot_schedule_before_retraction"
      (transportRequired.getD baseState) .schedule
  , witness "uncommitted_retraction_is_rejected"
      (transportRequired.getD baseState) (.confirmRetraction false)
  , witness "durable_retraction_is_observed"
      (transportRequired.getD baseState) (.confirmRetraction true)
  , witness "transport_backoff_only_after_retraction"
      (transportRetracted.getD baseState) .schedule
  , witness "parse_resample_only_after_retraction"
      { baseState with phase := .retracted .parseBadRequest "json" 12 } .schedule
  , witness "deterministic_parse_moves_to_repair"
      { baseState with
          phase := .retracted .parseBadRequest "json" 12
          lastParseError := some "json"
          resampleUsed := 1 } .schedule
  , witness "accepted_publication_closes_retry" baseState (.accept 200)
  , witness "accepted_failure_cannot_retract"
      (accepted.getD baseState) (.observeFailure .transport "late" 12)
  , witness "accepted_tool_failure_is_terminal_observation"
      (accepted.getD baseState) .acceptedToolFailure
  , witness "accepted_publication_cannot_schedule"
      (accepted.getD baseState) .schedule
  , witness "usage_is_charged_before_retraction"
      { baseState with
        phase := .retractRequired .transport "io" 12
        usageCharged := 21 } (.confirmRetraction true)
  , witness "late_usage_is_still_charged"
      (accepted.getD baseState) (.recordUsage 9)
  , witness "transport_budget_exhaustion_is_terminal_for_policy"
      { baseState with
          phase := .retracted .transport "io" 12
          transportUsed := defaultBudget.transportRetries } .schedule
  , witness "parse_budget_without_repair_is_exhausted"
      { baseState with
          phase := .retracted .parseBadRequest "json" 12
          budget := { defaultBudget with allowRepair := false }
          resampleUsed := defaultBudget.resampleRetries } .schedule
  , witness "retry_wake_past_deadline_is_exhausted"
      { baseState with
          phase := .retracted .transport "io" 12
          deadline := some 11 } .schedule
  , witness "repair_issue_consumes_its_only_capability"
      { baseState with phase := .repairing } .repairIssue
  , witness "second_repair_issue_is_rejected"
      (repairIssued.getD baseState) .repairIssue
  ]

theorem retryCases_count : retryCases.length = 18 := by decide

theorem retry_cases_pin_publication_boundary :
    retryCases.map (fun c => c.post.isSome) =
      [true, false, false, true, true, true, true, true, false, true, false, true, true,
       true, true, true, true, false] := by
  native_decide

theorem retry_cases_pin_exhaustion_and_single_repair :
    (retryCases.drop 13).map (fun c => c.post.map (fun state => state.phase)) =
      [some .exhausted, some .exhausted, some .exhausted, some .issuing, none] ∧
    (retryCases.drop 13).map (fun c => c.post.map (fun state => state.repairUsed)) =
      [some false, some false, some false, some true, none] := by
  native_decide

end CompletionRetry
