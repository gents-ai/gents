import Proofs.Shared.CompletionRetry.Transition

/-!
The directive each native retry entry point returns, projected from the retry
machine. A pre-stream failure, a mid-stream failure and a turn with no visible
output each run `observeFailure`, `confirmRetraction` and `schedule`; the phase
that results names the directive and the counters give the retry count. A
mid-stream failure after this turn's tool effects is never retracted: it keeps
the turn and pays the transport budget for the next attempt.
-/

namespace CompletionRetry.Directive

open CompletionRetry

inductive Kind
  | retryTransport
  | retryResample
  | retryAccount
  | repair
  | compactAndRetry
  | retractAndResample
  | closeAndContinue
  | fail
  deriving DecidableEq, Repr

inductive Entry
  | preStream
  | midStream
  | noVisibleOutput
  | repairIssued
  | compactionIssued
  deriving DecidableEq, Repr

/-- The native retry policy: a transport backoff ladder in seconds, whose
length is the transport budget, and the resample, empty-sample, repair and
account budgets. -/
structure Policy where
  backoff : List Nat
  maxResample : Nat
  maxEmptyResample : Nat
  allowRepair : Bool
  accountHops : Nat
  deriving DecidableEq, Repr

def Policy.budget (p : Policy) : Budget :=
  { transportRetries := p.backoff.length
    resampleRetries := p.maxResample
    emptyRetries := p.maxEmptyResample
    allowRepair := p.allowRepair
    accountHops := p.accountHops }

def Policy.ladder (p : Policy) (used : Nat) : Nat := p.backoff[used]?.getD 0

/-- Resampling paces from the ladder step it has reached and stays on the last
step once past it. -/
def Policy.pacing (p : Policy) (used : Nat) : Nat :=
  p.backoff[used]?.getD (p.backoff.getLast?.getD 0)

structure Step where
  entry : Entry
  failure : FailureClass
  error : String
  effects : Bool
  deriving DecidableEq, Repr

structure Outcome where
  kind : Kind
  retryCount : Nat
  deriving DecidableEq, Repr

def start (p : Policy) (now : Time) (deadline : Option Time) : State :=
  { request := 0, scope := 0, turn := 0, phase := .streaming, budget := p.budget,
    transportUsed := 0, resampleUsed := 0, repairUsed := false, lastParseError := none,
    now, deadline, attempt := 0, usageCharged := 0 }

def retryCount (s : State) : Nat := s.transportUsed + s.resampleUsed + s.emptyUsed

def wakeFor (p : Policy) (s : State) : FailureClass → Time
  | .transport => s.now + p.ladder s.transportUsed
  | .parseBadRequest => s.now + p.pacing s.resampleUsed
  | .emptySample => s.now + p.pacing s.emptyUsed
  | _ => s.now

def retractThenSchedule (s : State) (failure : FailureClass) (error : String) (wake : Time) :
    Option State :=
  (step? s (.observeFailure failure error wake)).bind fun observed =>
    match observed.phase with
    | .failedPermanent => some observed
    | _ => (step? observed (.confirmRetraction true)).bind (step? · .schedule)

/-- A failure after this turn's tool effects keeps the turn: no Retracted
closure is written, and the next attempt is paid from the transport budget
under the transport deadline gate. -/
def closeAndContinue? (s : State) (wake : Time) : Option State :=
  if s.phase = .streaming ∧ s.transportUsed < s.budget.transportRetries ∧
      fitsDeadline wake s.deadline ∧ s.now ≤ wake then
    some { s with phase := .backingOff wake, transportUsed := s.transportUsed + 1,
                  attempt := s.attempt + 1 }
  else none

def resume (s : State) : State :=
  match s.phase with
  | .backingOff wake => ((step? s (.wake wake)).bind (step? · .issue)).getD s
  | .issuing => (step? s .issue).getD s
  | _ => s

def preStreamKind (failure : FailureClass) : Phase → Kind
  | .backingOff _ => if failure = .parseBadRequest then .retryResample else .retryTransport
  | .issuing => .retryAccount
  | .repairing => .repair
  | .compacting => .compactAndRetry
  | _ => .fail

def retractKind : Phase → Kind
  | .backingOff _ => .retractAndResample
  | _ => .fail

def failureOutcome (s : State) (failure : FailureClass) (error : String) (wake : Time)
    (kindOf : Phase → Kind) : List Outcome × Option State :=
  match retractThenSchedule s failure error wake with
  | none => ([], none)
  | some t =>
      let kind := kindOf t.phase
      ([{ kind, retryCount := retryCount t }], if kind = .fail then none else some (resume t))

def midStreamOutcome (p : Policy) (s : State) (effects : Bool) : List Outcome × Option State :=
  if effects then
    match closeAndContinue? s (wakeFor p s .transport) with
    | some t => ([{ kind := .closeAndContinue, retryCount := retryCount t }], some (resume t))
    | none =>
        if s.phase = .streaming then ([{ kind := .fail, retryCount := retryCount s }], none)
        else ([], none)
  else failureOutcome s .transport "" (wakeFor p s .transport) retractKind

def apply (p : Policy) (s : State) (step : Step) : List Outcome × Option State :=
  match step.entry with
  | .preStream =>
      failureOutcome s step.failure step.error (wakeFor p s step.failure)
        (preStreamKind step.failure)
  | .midStream => midStreamOutcome p s step.effects
  | .noVisibleOutput =>
      failureOutcome s .emptySample "" (wakeFor p s .emptySample) retractKind
  | .repairIssued => ([], (step? s .repairIssue).bind (step? · .issue))
  | .compactionIssued => ([], (step? s .compactIssue).bind (step? · .issue))

def run (p : Policy) : State → List Step → List Outcome
  | _, [] => []
  | s, step :: rest =>
      let (out, next) := apply p s step
      out ++ match next with
        | none => []
        | some t => run p t rest

/-- A turn with tool effects is never retried by retraction. -/
theorem effects_turn_is_never_retracted (p : Policy) (s : State) :
    ∀ o ∈ (midStreamOutcome p s true).1, o.kind ≠ .retractAndResample := by
  intro o ho
  unfold midStreamOutcome at ho
  simp only [if_true] at ho
  split at ho
  · simp at ho; subst ho; simp
  · split at ho
    · simp at ho; subst ho; simp
    · simp at ho

/-- Keeping a turn costs exactly what a retracted transport retry costs: the
same budget gate, the same counters, the same wake. -/
theorem effects_turn_pays_like_a_retracted_retry (s t : State) (error : String) (wake : Time)
    (h : closeAndContinue? s wake = some t) :
    retractThenSchedule s .transport error wake = some t := by
  unfold closeAndContinue? at h
  split at h
  · rename_i hc
    obtain ⟨hs, hb, hd, hn⟩ := hc
    cases h
    simp [retractThenSchedule, step?, hs, hb, hd, hn]
  · cases h

/-- A turn with effects either keeps the turn and spends one transport retry,
or fails; it never resamples without paying. -/
theorem effects_turn_spends_the_transport_budget (s t : State) (wake : Time)
    (h : closeAndContinue? s wake = some t) :
    t.transportUsed = s.transportUsed + 1 ∧ s.transportUsed < s.budget.transportRetries := by
  unfold closeAndContinue? at h
  split at h
  · rename_i hc
    cases h
    exact ⟨rfl, hc.2.1⟩
  · cases h

end CompletionRetry.Directive
