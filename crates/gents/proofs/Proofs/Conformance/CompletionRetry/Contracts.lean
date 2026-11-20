import Proofs.Shared.CompletionRetry.Executable
import Proofs.Shared.CompletionRetry.Directive
import Proofs.Conformance.Family
import Proofs.Conformance.ContractTypes
import Proofs.Runtime.CompletionLoop

namespace CompletionRetry.Contracts

open Lean Conformance CompletionRetry CompletionRetry.Directive

deriving instance ToJson for FailureClass, FailureOrigin, Kind, Entry, Policy, Step, Outcome

structure Trace where
  name : String
  policy : Policy
  now : Nat
  deadline : Option Nat
  steps : List Step
  /-- A bound, not an expectation: the longest delay native jitter can give a
  resample paced from the ladder's last step, in milliseconds. -/
  resampleDelayBoundMs : Option Nat
  deriving ToJson

private def scheduled : Policy :=
  { backoff := [5, 30, 120], maxResample := 1, maxEmptyResample := 1, allowRepair := true,
    accountHops := 0 }

private def interactive : Policy :=
  { backoff := [2], maxResample := 0, maxEmptyResample := 1, allowRepair := true,
    accountHops := 0 }

private def pre (failure : FailureClass) (error : String) : Step :=
  { entry := .preStream, failure, error, effects := false }

private def mid (effects : Bool) : Step :=
  { entry := .midStream, failure := .transport, error := "", effects }

private def empty : Step :=
  { entry := .noVisibleOutput, failure := .emptySample, error := "", effects := false }

private def repairIssued : Step :=
  { entry := .repairIssued, failure := .transport, error := "", effects := false }

private def compactionIssued : Step :=
  { entry := .compactionIssued, failure := .transport, error := "", effects := false }

private def trace (name : String) (policy : Policy) (steps : List Step)
    (deadline : Option Nat := none) : Trace :=
  { name, policy, now := 1000, deadline, steps, resampleDelayBoundMs := none }

/-- Native jitter moves a delay by at most a quarter of it, so a resample paced
from the last ladder step waits at most 1.25 times that step. -/
def resampleDelayBoundMs (p : Policy) : Nat := (p.backoff.getLast?.getD 0) * 1250

private def boundedResample (t : Trace) : Trace :=
  { t with resampleDelayBoundMs := some (resampleDelayBoundMs t.policy) }

/-- Native retry delays are jittered: `jitter` in
`crates/gents-loop/src/completion_retry.rs` moves each base delay by at most a
quarter of it and floors the result at 100 ms, while the model wakes exactly
at the base delay. A trace without a deadline has the same outcome under every
jitter. A trace with a deadline needs a non-empty ladder of whole positive
seconds (an empty ladder waits the 100 ms floor where the model waits zero)
and a deadline behind the clock, closer than three quarters of the smallest
base delay, or further than twice every base delay its steps can sum. -/
def jitterSafe (t : Trace) : Bool :=
  match t.deadline with
  | none => true
  | some d =>
      !t.policy.backoff.isEmpty && t.policy.backoff.all (· > 0) &&
        (d < t.now ||
          4 * (d - t.now) < 3 * t.policy.backoff.foldl min (t.policy.backoff.headD 0) ||
          d - t.now ≥ 2 * t.steps.length * t.policy.backoff.foldl max 0)

def preStreamTraces : List Trace :=
  [ trace "transport_failure_requires_retraction" scheduled [pre .transport "io"]
  , trace "transport_budget_exhaustion_is_terminal_for_policy" scheduled
      (List.replicate 4 (pre .transport "io"))
  , trace "far_deadline_admits_the_whole_ladder" scheduled
      (List.replicate 4 (pre .transport "io")) (some 1001000)
  , trace "retry_wake_past_deadline_is_exhausted" scheduled [pre .transport "io"] (some 999)
  , trace "selected_delay_past_deadline_fails_fast"
      { scheduled with backoff := [30], maxResample := 0, maxEmptyResample := 0 }
      [pre .transport "io"] (some 1010)
  , trace "parse_resample_only_after_retraction" scheduled [pre .parseBadRequest "json"]
  , trace "deterministic_parse_moves_to_repair"
      { scheduled with backoff := [5, 30], maxResample := 2 }
      [pre .parseBadRequest "json", pre .parseBadRequest "json"]
  , boundedResample (trace "resample_budget_outlives_ladder"
      { scheduled with backoff := [5], maxResample := 3 }
      [pre .parseBadRequest "a", pre .parseBadRequest "b", pre .parseBadRequest "c"])
  , boundedResample (trace "resample_exhausts_on_its_own_budget"
      { scheduled with backoff := [5], maxResample := 2 }
      [pre .parseBadRequest "a", pre .parseBadRequest "b", pre .parseBadRequest "c"])
  , trace "empty_ladder_parse_resamples_at_once"
      { scheduled with backoff := [], maxResample := 1, maxEmptyResample := 0 }
      [pre .parseBadRequest "a", pre .parseBadRequest "b"]
  , trace "parse_budget_without_repair_is_exhausted" { scheduled with allowRepair := false }
      [pre .parseBadRequest "a", pre .parseBadRequest "b"]
  , trace "repair_issue_consumes_its_only_capability"
      { scheduled with backoff := [5], maxResample := 0 }
      [pre .parseBadRequest "a", repairIssued, pre .parseBadRequest "b"]
  , trace "reasoning_rejection_moves_directly_to_repair" scheduled
      [pre .reasoningRejected "signature"]
  , trace "reasoning_rejection_after_repair_is_exhausted" scheduled
      [pre .reasoningRejected "signature", repairIssued, pre .reasoningRejected "signature"]
  , trace "context_overflow_moves_directly_to_compaction" scheduled
      [pre .contextOverflow "prompt is too long"]
  , trace "compact_issue_consumes_its_only_capability" scheduled
      [pre .contextOverflow "prompt is too long", compactionIssued, pre .transport "io"]
  , trace "context_overflow_after_compaction_is_exhausted" scheduled
      [pre .contextOverflow "prompt is too long", compactionIssued,
       pre .contextOverflow "prompt is too long"]
  , trace "provider_account_limit_requires_retraction" scheduled
      [pre .accountLimited "account limit reached"]
  , trace "account_limit_reissues_on_next_account_without_backoff"
      { scheduled with accountHops := 2 } [pre .accountLimited "account limit reached"]
  , trace "account_hops_exhausted_is_terminal_for_policy" { scheduled with accountHops := 1 }
      [pre .accountLimited "account limit reached", pre .accountLimited "account limit reached"]
  , trace "local_request_build_fails_permanently_without_retry" scheduled
      [pre .permanent "malformed local request"]
  ]

def midStreamTraces : List Trace :=
  [ trace "transport_backoff_only_after_retraction" scheduled [mid false]
  , trace "effects_turn_closes_and_continues" scheduled [mid true]
  , trace "effects_and_retractions_share_the_transport_budget" scheduled
      [mid false, mid true, mid false, mid true]
  , trace "effects_turn_past_deadline_fails" scheduled [mid true] (some 999)
  ]

def noVisibleOutputTraces : List Trace :=
  [ trace "empty_sample_retry_independent_of_transport" interactive
      [mid false, empty, mid false]
  , trace "empty_sample_retry_exhausted" interactive [empty, empty]
  , trace "empty_sample_retry_respects_deadline" interactive [empty] (some 1000)
  , trace "empty_sample_retry_without_a_ladder" { interactive with backoff := [] } [empty]
  ]

theorem traces_are_jitter_safe :
    (preStreamTraces ++ midStreamTraces ++ noVisibleOutputTraces).all jitterSafe = true := by
  native_decide

def replayTrace (t : Trace) : List Outcome :=
  run t.policy (start t.policy t.now t.deadline) t.steps

@[conformance_family]
def preStream : Family Trace (List Outcome) where
  inputs := preStreamTraces
  model := replayTrace

@[conformance_family]
def midStream : Family Trace (List Outcome) where
  inputs := midStreamTraces
  model := replayTrace

@[conformance_family]
def noVisibleOutput : Family Trace (List Outcome) where
  inputs := noVisibleOutputTraces
  model := replayTrace

example : (midStreamTraces.flatMap replayTrace).map (·.kind) =
    [.retractAndResample, .closeAndContinue, .retractAndResample, .closeAndContinue,
     .retractAndResample, .fail, .fail] := by
  native_decide

structure OriginInput where
  name : String
  failureOrigin : FailureOrigin
  /-- The provider stream delivered a preview item before it broke. -/
  afterPreview : Bool
  deriving ToJson

structure OriginOutput where
  classifiedFailure : FailureClass
  deriving ToJson

@[conformance_family]
def originClass : Family OriginInput OriginOutput where
  inputs :=
    [ { name := "local_request_build_fails_permanently_without_retry"
        failureOrigin := .localRequestBuild, afterPreview := false }
    , { name := "retryable_transport_still_requires_retraction"
        failureOrigin := .retryableTransport, afterPreview := false }
    , { name := "provider_stream_malformed_requires_retraction"
        failureOrigin := .providerStreamMalformed, afterPreview := false }
    , { name := "provider_stream_malformed_after_preview_requires_retraction"
        failureOrigin := .providerStreamMalformed, afterPreview := true }
    , { name := "provider_reasoning_rejection_requires_retraction"
        failureOrigin := .providerReasoningRejected, afterPreview := false }
    , { name := "provider_account_limit_requires_retraction"
        failureOrigin := .providerAccountLimited, afterPreview := false } ]
  model := fun i => { classifiedFailure := i.failureOrigin.class }

def phaseName : Phase → String
  | .issuing => "issuing"
  | .streaming => "streaming"
  | .retractRequired .. => "retract_required"
  | .retracted .. => "retracted"
  | .backingOff _ => "backing_off"
  | .repairing => "repairing"
  | .compacting => "compacting"
  | .accepted _ => "accepted"
  | .acceptedToolFailed _ => "accepted_tool_failed"
  | .exhausted => "exhausted"
  | .failedPermanent => "failed_permanent"

structure WitnessInput where
  name : String
  deriving ToJson

structure WitnessOutput where
  legal : Bool
  phase : Option String
  transportUsed : Option Nat
  resampleUsed : Option Nat
  attempt : Option Nat
  accountUsed : Option Nat
  repairUsed : Option Bool
  usageCharged : Option Nat
  deriving ToJson

def witnessOutput (name : String) : WitnessOutput :=
  let post := (retryCases.find? (·.name == name)).bind (·.post)
  { legal := post.isSome
    phase := post.map (phaseName ·.phase)
    transportUsed := post.map (·.transportUsed)
    resampleUsed := post.map (·.resampleUsed)
    attempt := post.map (·.attempt)
    accountUsed := post.map (·.accountUsed)
    repairUsed := post.map (·.repairUsed)
    usageCharged := post.map (·.usageCharged) }

/-- model-production-gap: production has no API to attempt these retry-machine
actions: a schedule before a durable retraction, an uncommitted retraction, a
failure or a schedule after acceptance, and an accepted-tool-failure
observation on the retry machine. -/
@[conformance_family]
def retractionUnreached : Family WitnessInput WitnessOutput where
  inputs :=
    [ "cannot_schedule_before_retraction", "uncommitted_retraction_is_rejected"
    , "accepted_failure_cannot_retract", "accepted_tool_failure_is_terminal_observation"
    , "accepted_publication_cannot_schedule" ].map fun name => { name }
  model := fun i => witnessOutput i.name

structure LoopWitness where
  name : String
  scenario : CompletionLoop.Input
  deriving ToJson

private def loopDone : CompletionLoop.Call := { text := ["Hello ", "world"] }
private def loopCharged : CompletionLoop.Call :=
  { text := ["done"], usage := some ⟨100, 100, 1000⟩ }
private def loopFailure (kind : String) : CompletionLoop.Call := { failure := some kind }

/-- The retry-machine witnesses whose behaviour the owned loop reaches: a
durable retraction before the retry, acceptance closing the attempt, usage
charged across a retraction and after acceptance, and the single repair. -/
@[conformance_family]
def loopWitness : Family LoopWitness CompletionLoop.Observation where
  inputs :=
    [ { name := "durable_retraction_is_observed"
        scenario := { calls := [{ loopFailure "transport" with text := ["Hel"] }, loopDone] } }
    , { name := "accepted_publication_closes_retry"
        scenario := { calls := [loopDone, loopFailure "transport"] } }
    , { name := "usage_is_charged_before_retraction"
        scenario := { calls := [{ loopFailure "transport" with text := ["partial"] }, loopCharged]
                      limit := some 5000 } }
    , { name := "late_usage_is_still_charged"
        scenario := { calls := [{ tool := true, usage := some ⟨1000, 1000, 2000⟩ }, loopCharged]
                      limit := some 5000 } }
    , { name := "second_repair_issue_is_rejected"
        scenario := { calls := [loopFailure "reasoning", loopFailure "reasoning"] } } ]
  model := fun i => CompletionLoop.run i.scenario

end CompletionRetry.Contracts
