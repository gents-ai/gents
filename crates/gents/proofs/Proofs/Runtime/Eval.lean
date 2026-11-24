import Mathlib.Data.List.Basic
import Proofs.Session.Request.State
import Proofs.Documents.ScopeTemplates.State

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

def storedScore (kind : OutcomeKind) (reason : Option ProviderReason) (score : Option Nat) :
    Option Nat :=
  match classify kind reason with
  | .notEvidence | .unknown => none
  | _ => score

inductive ReasonRecord where
  | absent | recorded
  deriving DecidableEq, Repr

def recordedReason : Option ProviderReason → ReasonRecord
  | none => .absent
  | some _ => .recorded

theorem a_non_evidence_verdict_stores_a_null_score (kind : OutcomeKind)
    (reason : Option ProviderReason) (score : Option Nat)
    (nonEvidence : classify kind reason = .notEvidence) :
    storedScore kind reason score = none := by
  simp [storedScore, nonEvidence]

/-- A Trigger's `last_status` after a fire that wrote no request. External
premise: `unacknowledged` is written by the trigger engine when a fire's
result never reached its acknowledgment channel, so the runtime lost the fire;
`error` and `budget_exhausted` are the fire's own result, decided by the
Trigger and Task the subject configured. -/
inductive FireFailure where
  | error | budgetExhausted | unacknowledged
  deriving DecidableEq, Repr

/-- Failed fires observed since a stage began. A fire the configuration made
fail is the subject's; a fire the runtime lost is not evidence, unless a fire
the configuration made fail sits beside it. -/
def fireFailureKind (failures : List FireFailure) : Option OutcomeKind :=
  if failures.any (· != .unacknowledged) then some .runtime
  else if failures.isEmpty then none
  else some .infrastructure

/-- How a stage that observed its request ends. The request's own outcome
decides first; an accepted request still fails when a trigger fire since the
stage began failed, and an unreadable trigger record leaves nothing known. -/
def stageOutcome (request : Option OutcomeKind) (triggersRead : Bool)
    (failures : List FireFailure) : Option OutcomeKind :=
  match request with
  | some kind => some kind
  | none => if triggersRead then fireFailureKind failures else some .infrastructure

/-- One observation of a seed stage after its document was written.
`expectFire` is the case's premise: the subject was asked to build an
automation the seed fires (`true`), or one that must skip it (`false`).
`observable` holds while the trial runtime runs and every read succeeds.
`fired` holds once a request names the seed as its `caused_by_source_doc_id`.
`failures` are the failed fires recorded for the seed since the stage began.
`quiet` holds once every request of the home has been terminal, and no request
was added, for the settle period. `windowEnded` holds once the stage's deadline,
or the default fire window, has passed since the seed was written. -/
structure SeedObservation where
  expectFire : Bool
  observable : Bool
  fired : Bool
  failures : List FireFailure
  quiet : Bool
  windowEnded : Bool
  deriving DecidableEq, Repr

inductive SeedAction where
  | wait | observe | pass | fail
  deriving DecidableEq, Repr

/-- `kind` is present exactly when `action` is `fail`. -/
structure SeedStep where
  action : SeedAction
  kind : Option OutcomeKind
  deriving DecidableEq, Repr

def SeedStep.failed (kind : OutcomeKind) : SeedStep := ⟨.fail, some kind⟩
def SeedStep.just (action : SeedAction) : SeedStep := ⟨action, none⟩

/-- A seed nothing fired for, in a runtime that kept running and could be
read, is the subject's: either it built no automation the seed reaches, or the
automation it built skipped the seed. A seed that must be skipped passes only
once the home is quiet; a window that ends first leaves it undecided. -/
def seedStep (o : SeedObservation) : SeedStep :=
  if !o.observable then .failed .infrastructure
  else if o.fired then
    if o.expectFire then .just .observe else .failed .modelAcceptance
  else match fireFailureKind o.failures with
    | some kind => .failed kind
    | none =>
      if o.expectFire then
        if o.windowEnded then .failed .modelAcceptance else .just .wait
      else if o.quiet then .just .pass
      else if o.windowEnded then .failed .inconclusive
      else .just .wait

theorem seed_step_fails_with_a_kind (o : SeedObservation) :
    (seedStep o).action = .fail ↔ (seedStep o).kind.isSome := by
  unfold seedStep fireFailureKind
  repeat' split
  all_goals simp_all [SeedStep.failed, SeedStep.just]

theorem an_unfired_seed_is_the_subjects_failure (o : SeedObservation)
    (expected : o.expectFire = true) (observable : o.observable = true)
    (unfired : o.fired = false) (clean : o.failures = []) (ended : o.windowEnded = true) :
    seedStep o = .failed .modelAcceptance ∧ classify .modelAcceptance none = .fail := by
  simp [seedStep, fireFailureKind, expected, observable, unfired, clean, ended, classify]

theorem an_unobservable_seed_is_not_evidence (o : SeedObservation)
    (lost : o.observable = false) :
    seedStep o = .failed .infrastructure ∧ classify .infrastructure none = .notEvidence := by
  simp [seedStep, lost, classify]

theorem fire_failure_kind_none_is_empty (failures : List FireFailure)
    (clean : fireFailureKind failures = none) : failures = [] := by
  unfold fireFailureKind at clean
  split at clean
  · simp at clean
  · split at clean
    · simp_all
    · simp at clean

theorem a_skipped_seed_passes_only_unfired_and_quiet (o : SeedObservation)
    (passed : (seedStep o).action = .pass) :
    o.expectFire = false ∧ o.observable = true ∧ o.fired = false ∧ o.failures = [] ∧
      o.quiet = true := by
  unfold seedStep at passed
  split at passed
  · simp [SeedStep.failed] at passed
  · split at passed
    · split at passed <;> simp [SeedStep.failed, SeedStep.just] at passed
    · split at passed
      · simp [SeedStep.failed] at passed
      · rename_i clean
        have empty := fire_failure_kind_none_is_empty _ clean
        split at passed
        · split at passed <;> simp [SeedStep.failed, SeedStep.just] at passed
        · split at passed
          · simp_all
          · split at passed <;> simp [SeedStep.failed, SeedStep.just] at passed

theorem a_seed_that_must_be_skipped_fails_when_it_fires (o : SeedObservation)
    (skipped : o.expectFire = false) (observable : o.observable = true)
    (fired : o.fired = true) :
    seedStep o = .failed .modelAcceptance := by
  simp [seedStep, skipped, observable, fired]

theorem a_failed_fire_is_never_a_pass (failures : List FireFailure) (kind : OutcomeKind)
    (failed : fireFailureKind failures = some kind) : classify kind none ≠ .pass := by
  unfold fireFailureKind at failed
  repeat' split at failed
  all_goals simp_all [classify]
  all_goals (subst failed; simp [classify])

theorem a_runtime_lost_fire_beside_a_configured_failure_is_the_subjects
    (rest : List FireFailure) :
    fireFailureKind (.unacknowledged :: .error :: rest) = some .runtime := by
  simp [fireFailureKind]

/-- A request in the trial home, as a stage's `runs` capture reads it:
`trigger` is its `caused_by_trigger_id`, `source` its `caused_by_source_doc_id`,
`watched` the `source_collection` of the `TriggerFire` that wrote it, and
`priorToStage` whether it existed before the capturing stage began. -/
structure FiredRun where
  trigger : Option String
  watched : Option String
  source : Option String
  priorToStage : Bool
  deriving DecidableEq, Repr

/-- A `runs` capture's selectors; each one given narrows the runs. -/
structure RunSelector where
  trigger : Option String
  collection : Option String
  ofSeed : Bool
  deriving DecidableEq, Repr

/-- A stage's `runs` capture addresses the trigger-fired requests that came
into being while the stage ran, whoever started them: a chain's later link, a
grouped run, a run routed into a continuing session. `ofSeed` keeps the runs
the stage's own seed document caused. -/
def runAddressed (selector : RunSelector) (seed : Option String) (run : FiredRun) : Bool :=
  run.trigger.isSome && !run.priorToStage &&
    selector.trigger.all (fun trigger => run.trigger == some trigger) &&
    selector.collection.all (fun collection => run.watched == some collection) &&
    (!selector.ofSeed || (seed.isSome && run.source == seed))

theorem an_addressed_run_was_fired_during_the_stage (selector : RunSelector)
    (seed : Option String) (run : FiredRun) (addressed : runAddressed selector seed run = true) :
    run.trigger.isSome ∧ run.priorToStage = false := by
  simp only [runAddressed, Bool.and_eq_true, Bool.not_eq_true'] at addressed
  exact ⟨addressed.1.1.1.1, addressed.1.1.1.2⟩

theorem runs_of_no_seed_are_none (selector : RunSelector) (run : FiredRun)
    (ofSeed : selector.ofSeed = true) : runAddressed selector none run = false := by
  simp [runAddressed, ofSeed]

end Eval
