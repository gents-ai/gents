import Mathlib.Data.List.Basic
import Proofs.Request.State

/-! Eval core contract (#1515): the closed outcome vocabulary and its projection
onto evidence classes.

The governing rule: anything the subject under evaluation could cause counts
against it; only what it cannot cause is excluded. A `provider` outcome needs a
reason, because a candidate prompt can cause a context overflow (`rejected`) but
not an outage (`unavailable`). A `provider` outcome with no reason is `unknown`:
the evaluator could not tell, and an unknown never helps a candidate.

Refinement boundaries: checks, reducers over numeric scores, and trial scheduling
are outside this model. -/
namespace Eval

inductive OutcomeKind where
  | passed | modelAcceptance | deadline | tool | runtime | skippedPrerequisite
  | provider | infrastructure | inconclusive | grader | unknown
  deriving DecidableEq, Repr

namespace OutcomeKind
def toDefraDB : OutcomeKind → String
  | .passed => "passed"
  | .modelAcceptance => "model_acceptance"
  | .deadline => "deadline"
  | .tool => "tool"
  | .runtime => "runtime"
  | .skippedPrerequisite => "skipped_prerequisite"
  | .provider => "provider"
  | .infrastructure => "infrastructure"
  | .inconclusive => "inconclusive"
  | .grader => "grader"
  | .unknown => "unknown"
end OutcomeKind

/-- These are the durable recovery reason and the owned execution errors.
An arbitrary provider or tool message mentioning a lease is not native evidence
of lost execution authority. -/
def executionLeaseFailure (reason : String) : Bool :=
  reason == "execution lease expired" ||
  reason == "execution lease expired or ownership was revoked" ||
  reason == "request execution lease: execution lease expired or ownership was revoked"

structure RequestEvidence where
  state : Option RequestState
  interruptedOnDeadline : Bool
  toolBudgetExhausted : Bool
  executionLeaseLost : Bool
  inferenceFailed : Bool
  toolFailed : Bool
  deriving DecidableEq, Repr

/-- An interrupt records intent; only an observed durable terminal state is
terminal evidence. A deadline or expired grace cannot supply that state. -/
def observedTerminalState (state : Option RequestState) : Option RequestState :=
  state.filter fun state => decide (HasTerminal.isTerminal state)

theorem observed_terminal_is_durable (state : Option RequestState) (terminal : RequestState)
    (observed : observedTerminalState state = some terminal) : state = some terminal := by
  cases state with
  | none => simp [observedTerminalState] at observed
  | some state =>
    simp only [observedTerminalState, Option.filter] at observed
    split at observed <;> simp_all

theorem nonterminal_is_not_terminal_evidence (state : RequestState)
    (nonterminal : ¬ HasTerminal.isTerminal state) :
    observedTerminalState (some state) = none := by
  simp [observedTerminalState, Option.filter, nonterminal]

theorem observed_terminal_is_terminal (state : Option RequestState) (terminal : RequestState)
    (observed : observedTerminalState state = some terminal) :
    HasTerminal.isTerminal terminal := by
  have durable := observed_terminal_is_durable state terminal observed
  subst state
  by_contra nonterminal
  rw [nonterminal_is_not_terminal_evidence terminal nonterminal] at observed
  contradiction

/-- Native execution leases belong to runtime infrastructure. Their loss alone
does not establish a subject failure. A completed request, an observed stage
deadline, or an explicit tool budget result retains its existing attribution. -/
def classifyRequestOutcome (e : RequestEvidence) : Option OutcomeKind :=
  if e.interruptedOnDeadline then some .deadline
  else if e.state == none then some .infrastructure
  else if e.state == some .completed then none
  else if e.toolBudgetExhausted then some .tool
  else if e.executionLeaseLost then some .infrastructure
  else if e.inferenceFailed && e.toolFailed then some .unknown
  else if e.inferenceFailed then some .provider
  else if e.toolFailed then some .tool
  else match e.state with
    | some .failed | some .interrupted | some .dead | some .superseded => some .runtime
    | _ => some .unknown

theorem lease_loss_is_infrastructure (e : RequestEvidence)
    (deadline : e.interruptedOnDeadline = false)
    (observed : e.state ≠ none)
    (unfinished : e.state ≠ some .completed)
    (budget : e.toolBudgetExhausted = false)
    (lost : e.executionLeaseLost = true) :
    classifyRequestOutcome e = some .infrastructure := by
  simp [classifyRequestOutcome, deadline, observed, unfinished, budget, lost]

theorem completed_request_is_accepted (e : RequestEvidence)
    (deadline : e.interruptedOnDeadline = false)
    (completed : e.state = some .completed) : classifyRequestOutcome e = none := by
  simp [classifyRequestOutcome, deadline, completed]

theorem missing_terminal_evidence_is_infrastructure (e : RequestEvidence)
    (deadline : e.interruptedOnDeadline = false)
    (unobserved : e.state = none) : classifyRequestOutcome e = some .infrastructure := by
  simp [classifyRequestOutcome, deadline, unobserved]

theorem observed_deadline_is_a_deadline (e : RequestEvidence)
    (deadline : e.interruptedOnDeadline = true) :
    classifyRequestOutcome e = some .deadline := by
  simp [classifyRequestOutcome, deadline]

theorem exhausted_tool_budget_is_a_tool_failure (e : RequestEvidence)
    (deadline : e.interruptedOnDeadline = false)
    (observed : e.state ≠ none)
    (unfinished : e.state ≠ some .completed)
    (budget : e.toolBudgetExhausted = true) :
    classifyRequestOutcome e = some .tool := by
  simp [classifyRequestOutcome, deadline, observed, unfinished, budget]

def allKinds : List OutcomeKind :=
  [.passed, .modelAcceptance, .deadline, .tool, .runtime, .skippedPrerequisite,
   .provider, .infrastructure, .inconclusive, .grader, .unknown]

inductive ProviderReason where
  | rejected | unavailable
  deriving DecidableEq, Repr

namespace ProviderReason
def toDefraDB : ProviderReason → String
  | .rejected => "rejected"
  | .unavailable => "unavailable"
end ProviderReason

def allReasons : List ProviderReason := [.rejected, .unavailable]

inductive EvidenceClass where
  | pass | fail | notEvidence | unknown
  deriving DecidableEq, Repr

namespace EvidenceClass
def toDefraDB : EvidenceClass → String
  | .pass => "pass"
  | .fail => "fail"
  | .notEvidence => "not_evidence"
  | .unknown => "unknown"
end EvidenceClass

def allClasses : List EvidenceClass := [.pass, .fail, .notEvidence, .unknown]

def classify : OutcomeKind → Option ProviderReason → EvidenceClass
  | .passed, _ => .pass
  | .modelAcceptance, _ => .fail
  | .deadline, _ => .fail
  | .tool, _ => .fail
  | .runtime, _ => .fail
  | .skippedPrerequisite, _ => .fail
  | .provider, some .rejected => .fail
  | .provider, some .unavailable => .notEvidence
  | .provider, none => .unknown
  | .infrastructure, _ => .notEvidence
  | .inconclusive, _ => .unknown
  | .grader, _ => .unknown
  | .unknown, _ => .unknown

/-- Outcomes a subject can bring about through its own behavior. -/
def subjectCausable : OutcomeKind → Option ProviderReason → Bool
  | .modelAcceptance, _ => true
  | .deadline, _ => true
  | .tool, _ => true
  | .runtime, _ => true
  | .skippedPrerequisite, _ => true
  | .provider, some .rejected => true
  | _, _ => false

/-- A subject can never move one of its own failures out of the denominator. -/
theorem subject_causable_is_never_excluded (k : OutcomeKind) (r : Option ProviderReason)
    (h : subjectCausable k r = true) : classify k r ≠ .notEvidence := by
  cases k <;> cases r <;> simp_all [subjectCausable, classify]
  all_goals (rename_i reason; cases reason <;> simp_all [subjectCausable, classify])

theorem subject_causable_fails (k : OutcomeKind) (r : Option ProviderReason)
    (h : subjectCausable k r = true) : classify k r = .fail := by
  cases k <;> cases r <;> simp_all [subjectCausable, classify]
  all_goals (rename_i reason; cases reason <;> simp_all [subjectCausable, classify])

theorem only_passed_passes (k : OutcomeKind) (r : Option ProviderReason) :
    classify k r = .pass ↔ k = .passed := by
  cases k <;> cases r <;> simp [classify]
  all_goals (rename_i reason; cases reason <;> simp [classify])

/-! ## Case class: the class of a case-trial is the class of its worst verdict -/

inductive CaseClass where
  | scored | unknown | notEvidence
  deriving DecidableEq, Repr

namespace CaseClass
def rank : CaseClass → Nat
  | .scored => 0
  | .unknown => 1
  | .notEvidence => 2
end CaseClass

def caseClass (verdicts : List EvidenceClass) : CaseClass :=
  if verdicts.any (· == .notEvidence) then .notEvidence
  else if verdicts.any (· == .unknown) then .unknown
  else .scored

/-- Adding a verdict never improves a case's class. -/
theorem caseClass_cons_rank (v : EvidenceClass) (vs : List EvidenceClass) :
    (caseClass vs).rank ≤ (caseClass (v :: vs)).rank := by
  cases v <;> simp only [caseClass, List.any_cons] <;>
    (split <;> (try split) <;> (try split) <;> simp_all [CaseClass.rank])

theorem caseClass_nil : caseClass [] = .scored := by
  simp [caseClass]

end Eval
