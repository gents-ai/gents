import Proofs.Automation.GoalAutomation.Edit

/-! Starting a session's Goal and setting it, for every client. The CLI, the
desktop and editor (through their signed command) and the working Agent's Goal
tools all reach one handler; these functions are its decisions. Validation of
the objective and budget text, the observed-revision fence and the transition
itself (`Goals.step?`) are shared by every actor and stay with their owners. -/

namespace GoalAutomation.Operations

/-- Who asks. A person signs with a key the client holds, directly or through
a signed command whose signature the runtime verified. The working Agent is the
session's own Agent acting from its current assignment. An actor that cannot be
shown to be either is another Agent: classification fails closed. -/
inductive Actor where
  | person
  | workingAgent
  | otherAgent
  deriving DecidableEq, Repr

inductive Session where
  | missing
  | idle
  | busy
  deriving DecidableEq, Repr

/-- The session's current Goal: none, one that may still run, or one that is
complete or stopped. -/
inductive Existing where
  | none
  | live
  | finished
  deriving DecidableEq, Repr

/-- The session's record of a start compared with this start's objective and
budget: its creation claim, or the current Goal itself when no claim records
it (a Goal a Task assigned). -/
inductive Claim where
  | none
  | same
  | different
  deriving DecidableEq, Repr

inductive StartDecision where
  | fresh
  /-- The claim already records this start; staging converges on it. -/
  | idempotent
  /-- A finished Goal the caller observed is replaced by a new assignment. -/
  | replace
  | denied
  | sessionMissing
  | busy
  | liveGoal
  /-- The caller's observation of the session's Goal is not the current one. -/
  | stale
  | conflict
  deriving DecidableEq, Repr

/-- A person starts a Goal on an idle session with its first request, or
replaces a finished Goal it observed. The working Agent starts one from its own
running request, so the session is busy by construction; it cannot replace a
Goal. `expected` is whether the caller named an observed Goal. -/
def startDecision (actor : Actor) (session : Session) (existing : Existing)
    (expected : Bool) (claim : Claim) : StartDecision :=
  match actor with
  | .otherAgent => .denied
  | .workingAgent =>
      if session = .missing then .sessionMissing
      else match existing, claim with
        | .none, .none => .fresh
        | _, .same => .idempotent
        | _, _ => .conflict
  | .person =>
      if session = .missing then .sessionMissing
      else match existing with
        | .live => .liveGoal
        | .finished =>
            if !expected then .stale
            else if session = .busy then .busy
            else .replace
        | .none =>
            if expected then .stale
            else if session = .busy then .busy
            else match claim with
              | .none => .fresh
              | .same => .idempotent
              | .different => .conflict

theorem other_agent_never_starts (session : Session) (existing : Existing)
    (expected : Bool) (claim : Claim) :
    startDecision .otherAgent session existing expected claim = .denied := rfl

theorem person_never_starts_on_a_busy_session (existing : Existing) (expected : Bool)
    (claim : Claim) :
    startDecision .person .busy existing expected claim ≠ .fresh ∧
      startDecision .person .busy existing expected claim ≠ .replace := by
  cases existing <;> cases expected <;> cases claim <;> simp [startDecision]

theorem only_a_person_replaces (actor : Actor) (session : Session) (existing : Existing)
    (expected : Bool) (claim : Claim)
    (h : startDecision actor session existing expected claim = .replace) :
    actor = .person ∧ existing = .finished ∧ expected = true ∧ session = .idle := by
  cases actor <;> cases session <;> cases existing <;> cases expected <;> cases claim <;>
    simp_all [startDecision]

theorem a_live_goal_is_never_replaced (actor : Actor) (session : Session) (expected : Bool)
    (claim : Claim) :
    startDecision actor session .live expected claim ≠ .replace ∧
      startDecision actor session .live expected claim ≠ .fresh := by
  cases actor <;> cases session <;> cases expected <;> cases claim <;> simp [startDecision]

/-- The status a set asks for. `block` from a person is an operator block;
from the working Agent it is a blocked audit, which blocks only after
`Goals.step?` counts enough of them. -/
inductive Target where
  | keep
  | activate
  | pause
  | stop
  | complete
  | block
  | usageLimited
  | budgetLimited
  deriving DecidableEq, Repr

inductive FieldChange where
  | none
  /-- A new objective, budget or both. -/
  | edit
  /-- Remove the budget. -/
  | clearBudget
  deriving DecidableEq, Repr

inductive SetDecision where
  | apply
  | denied
  | nothingToChange
  /-- Only a person may pause, stop, mark limits, clear the budget or choose
  reset resume. -/
  | personOnly
  /-- The working Agent edits fields or changes status, not both at once. -/
  | fieldsNeedKeep
  /-- The Goal's status does not admit a field edit from this actor. -/
  | notEditable
  /-- Activating a Goal that is not active is a resume, not a set. -/
  | resumeRequired
  /-- The working Agent completes only once its output obligations are met. -/
  | obligationsUnmet
  deriving DecidableEq, Repr

def origin : Actor → Edit.Origin
  | .person => .client
  | .workingAgent | .otherAgent => .agent

/-- The targets the working Agent may name. `activate` keeps an active Goal
active; on any other Goal it is a resume, which only a person asks for. -/
def agentTarget : Target → Bool
  | .keep | .activate | .complete | .block => true
  | _ => false

/-- A target that moves the Goal's status rather than keeping it. -/
def changesStatus : Target → Bool
  | .keep | .activate => false
  | _ => true

/-- Admission of a set. The transition the target names is then judged by
`Goals.step?` on the stored state, for every actor alike. A person's edit
follows the client edit rule (active or paused); the working Agent's follows
the agent rule (active only). -/
def setDecision (actor : Actor) (status : Goals.Status) (target : Target)
    (fields : FieldChange) (autoResume : Bool) (obligationsMet : Bool) : SetDecision :=
  if actor = .otherAgent then .denied
  else if target = .keep ∧ fields = .none ∧ autoResume = false then .nothingToChange
  else if actor = .workingAgent ∧
      (autoResume = true ∨ fields = .clearBudget ∨ agentTarget target = false) then .personOnly
  else if actor = .workingAgent ∧ fields ≠ .none ∧ changesStatus target = true then
    .fieldsNeedKeep
  else if fields ≠ .none ∧ Edit.editable (origin actor) status = false then .notEditable
  else if target = .activate ∧ status ≠ .active then .resumeRequired
  else if actor = .workingAgent ∧ target = .complete ∧ obligationsMet = false then
    .obligationsUnmet
  else .apply

theorem other_agent_never_sets (status : Goals.Status) (target : Target) (fields : FieldChange)
    (autoResume obligationsMet : Bool) :
    setDecision .otherAgent status target fields autoResume obligationsMet = .denied := rfl

theorem working_agent_cannot_pause_or_stop (status : Goals.Status) (fields : FieldChange)
    (autoResume obligationsMet : Bool) :
    setDecision .workingAgent status .pause fields autoResume obligationsMet ≠ .apply ∧
      setDecision .workingAgent status .stop fields autoResume obligationsMet ≠ .apply := by
  cases status <;> cases fields <;> cases autoResume <;> cases obligationsMet <;>
    simp [setDecision, agentTarget, changesStatus]

theorem working_agent_never_clears_the_budget (status : Goals.Status) (target : Target)
    (autoResume obligationsMet : Bool) :
    setDecision .workingAgent status target .clearBudget autoResume obligationsMet ≠ .apply := by
  cases status <;> cases target <;> cases autoResume <;> cases obligationsMet <;>
    simp [setDecision, agentTarget, changesStatus]

theorem applied_edit_is_editable (actor : Actor) (status : Goals.Status) (target : Target)
    (fields : FieldChange) (autoResume obligationsMet : Bool) (edits : fields ≠ .none)
    (h : setDecision actor status target fields autoResume obligationsMet = .apply) :
    Edit.editable (origin actor) status = true := by
  cases actor <;> cases status <;> cases target <;> cases fields <;> cases autoResume <;>
    cases obligationsMet <;> simp_all [setDecision, agentTarget, changesStatus, origin, Edit.editable]

theorem working_agent_completes_only_with_obligations_met (status : Goals.Status)
    (fields : FieldChange) (autoResume : Bool) :
    setDecision .workingAgent status .complete fields autoResume false ≠ .apply := by
  cases status <;> cases fields <;> cases autoResume <;> simp [setDecision, agentTarget, changesStatus]

theorem activation_is_never_a_set (actor : Actor) (status : Goals.Status) (fields : FieldChange)
    (autoResume obligationsMet : Bool) (inactive : status ≠ .active) :
    setDecision actor status .activate fields autoResume obligationsMet ≠ .apply := by
  cases actor <;> cases status <;> cases fields <;> cases autoResume <;> cases obligationsMet <;>
    simp_all [setDecision, agentTarget, changesStatus, origin, Edit.editable]

end GoalAutomation.Operations
