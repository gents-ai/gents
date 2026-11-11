import Mathlib.Data.List.Basic
import Proofs.Request.State
import Proofs.ScopeTemplates.State

namespace Eval

/-- Eval evidence may retain backend configuration metadata. Backend auth must
not enter evidence through either a projection or a filter, and other credential
collections remain excluded. DefraDB DID/ACP authorization still governs reads. -/
def documentCaptureAllowed (collection : String) (fields filterFields : List String) : Bool :=
  (!ScopeTemplates.credentialCollections.contains collection || collection == "InferenceBackend") &&
    !(collection == "InferenceBackend" && (fields ++ filterFields).contains "auth")

theorem credential_capture_is_backend (collection : String) (fields filterFields : List String)
    (credential : ScopeTemplates.credentialCollections.contains collection = true)
    (allowed : documentCaptureAllowed collection fields filterFields = true) :
    collection = "InferenceBackend" := by
  have member := List.contains_iff_mem.mp credential
  simp [documentCaptureAllowed, member] at allowed
  exact allowed.1

theorem backend_auth_not_captured (fields filterFields : List String)
    (auth : "auth" ∈ fields ++ filterFields) :
    documentCaptureAllowed "InferenceBackend" fields filterFields = false := by
  have contains : (fields ++ filterFields).contains "auth" = true :=
    List.contains_iff_mem.mpr auth
  rw [documentCaptureAllowed, contains]
  decide

theorem oauth_credentials_not_captured (fields filterFields : List String) :
    documentCaptureAllowed "OAuthCredential" fields filterFields = false := by
  simp [documentCaptureAllowed, ScopeTemplates.credentialCollections]

theorem backend_metadata_captured (fields filterFields : List String)
    (public : "auth" ∉ fields ++ filterFields) :
    documentCaptureAllowed "InferenceBackend" fields filterFields = true := by
  have contains : (fields ++ filterFields).contains "auth" = false := by
    cases h : (fields ++ filterFields).contains "auth" with
    | false => rfl
    | true => exact (public (List.contains_iff_mem.mp h)).elim
  rw [documentCaptureAllowed, contains]
  decide

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
  /-- Set only when the stage opted into a wall-clock deadline and the runner
  interrupted the request at it; a stage without one waits for the request's
  own terminal state, so `.deadline` is reachable only through that opt-in. -/
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

def subjectCausable : OutcomeKind → Option ProviderReason → Bool
  | .modelAcceptance, _ => true
  | .deadline, _ => true
  | .tool, _ => true
  | .runtime, _ => true
  | .skippedPrerequisite, _ => true
  | .provider, some .rejected => true
  | _, _ => false

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

theorem caseClass_cons_rank (v : EvidenceClass) (vs : List EvidenceClass) :
    (caseClass vs).rank ≤ (caseClass (v :: vs)).rank := by
  cases v <;> simp only [caseClass, List.any_cons] <;>
    (split <;> (try split) <;> (try split) <;> simp_all [CaseClass.rank])

theorem caseClass_nil : caseClass [] = .scored := by
  simp [caseClass]

end Eval
