import Proofs.GoalAutomation

/-! #1354: transaction-boundary refinement, not a new Goal status machine.
`children` projects existing AgentRequest rows; no new durable registry.
A binding represents the immutable semantic fingerprint prepared by the one
existing goal-continuation RequestSpec builder, after authenticated ancestry
validation. Readiness is deliberately absent: this slice is ready-backend only.
-/
namespace GoalAutomation.OperatorResume

structure Binding where
  goal : Nat
  owner : String
  session : String
  predecessor : Nat
  predecessorDoc : Nat
  correlation : Option String
  sourceDocument : Option String
  triggerContext : Option String
  workspaceFingerprint : Option String
  /-- Abstract full signed-semantic fingerprint, excluding runtime mutations. -/
  semanticFingerprint : String
  child : Nat
  sequence : Nat
  deriving DecidableEq, Repr

structure Snapshot where
  goal : Goals.State
  sequence : Nat
  lastContinuedFrom : Option Nat
  latestRequest : Nat
  children : List Binding
  tokensUsed : Nat
  tokenBudget : Option Nat
  deriving DecidableEq, Repr

structure Request where
  expectedStatus : Goals.Status
  expectedSequence : Nat
  /-- Results of authenticated owner/canonical parent checks in the transaction. -/
  authorized : Bool
  parentBelongsToGoal : Bool
  terminalParent : Bool
  sessionIdle : Bool
  /-- Prepared from parent for fresh publication; on retry reconstructed only
      after verifying the stored child signature and independent stable ancestry.
      This is not caller-controlled payload or recomputed mutable Goal text. -/
  binding : Binding
  deriving DecidableEq, Repr

inductive Outcome where
  | denied | stale | illegal | conflict | rolledBack
  | created | recovered
  deriving DecidableEq, Repr

def sameKey (a b : Binding) : Bool :=
  a.goal == b.goal && a.predecessor == b.predecessor

/-- One transaction publishes both Goal resume and its child. `commit=false`
includes staging failure/discard; an acknowledgement lost AFTER commit is
`commit=true` followed by retry against the resulting durable snapshot. -/
def resume (s : Snapshot) (r : Request) (commit : Bool) : Snapshot × Outcome :=
  if !r.authorized || !r.parentBelongsToGoal then (s, .denied)
  else match s.children.find? (sameKey r.binding) with
  | some existing => if existing = r.binding then (s, .recovered) else (s, .conflict)
  | none =>
      if s.goal.status ≠ r.expectedStatus || s.sequence != r.expectedSequence then (s, .stale)
      else if !r.terminalParent || !r.sessionIdle || r.binding.predecessor != s.latestRequest || r.binding.sequence != s.sequence + 1 then
        (s, .illegal)
      else match Goals.step? s.goal .resume with
      | none => (s, .illegal)
      | some post =>
          if commit then
            ({ s with goal := post, sequence := s.sequence + 1, lastContinuedFrom := some r.binding.predecessor, latestRequest := r.binding.child, children := r.binding :: s.children }, .created)
          else (s, .rolledBack)

/-- Strengthen the existing GoalSource update guard with its observed sequence.
The actual fields/transition remain owned by existing Goals.step?. -/
def controllerWrite (s : Snapshot) (expectedStatus : Goals.Status)
    (expectedSequence : Nat) (action : Goals.Action) : Snapshot :=
  if s.goal.status = expectedStatus ∧ s.sequence = expectedSequence then
    match Goals.step? s.goal action with
    | none => s
    | some post => { s with goal := post }
  else s

theorem unauthorized_resume_is_noop (s : Snapshot) (r : Request)
    (h : r.authorized = false) (commit : Bool) : resume s r commit = (s, .denied) := by
  simp [resume, h]

theorem discarded_resume_publishes_nothing (s : Snapshot) (r : Request) :
    (resume s r false).1 = s := by
  unfold resume
  split
  · rfl
  · split
    · split <;> rfl
    · split
      · rfl
      · split
        · rfl
        · split <;> rfl

theorem stale_controller_epoch_is_noop (s : Snapshot) (status : Goals.Status)
    (sequence : Nat) (action : Goals.Action) (h : s.sequence ≠ sequence) :
    controllerWrite s status sequence action = s := by
  simp [controllerWrite, h]

theorem resume_preserves_budget_and_usage (s : Snapshot) (r : Request) (commit : Bool) :
    (resume s r commit).1.tokensUsed = s.tokensUsed ∧
    (resume s r commit).1.tokenBudget = s.tokenBudget := by
  unfold resume
  split
  · exact ⟨rfl, rfl⟩
  · split
    · split <;> exact ⟨rfl, rfl⟩
    · split
      · exact ⟨rfl, rfl⟩
      · split
        · exact ⟨rfl, rfl⟩
        · split
          · exact ⟨rfl, rfl⟩
          · split <;> exact ⟨rfl, rfl⟩

theorem recovered_is_noop (s : Snapshot) (r : Request) (commit : Bool)
    (h : (resume s r commit).2 = .recovered) : (resume s r commit).1 = s := by
  unfold resume at *
  split at * <;> try simp_all
  split at *
  · split at * <;> try simp_all
  · split at * <;> try simp_all
    split at * <;> try simp_all
    split at * <;> try simp_all
    split at * <;> try simp_all

theorem created_publishes_atomically (s : Snapshot) (r : Request) (commit : Bool)
    (h : (resume s r commit).2 = .created) :
    (resume s r commit).1.goal.status = .active ∧
    (resume s r commit).1.children = r.binding :: s.children ∧
    (resume s r commit).1.sequence = s.sequence + 1 ∧
    (resume s r commit).1.lastContinuedFrom = some r.binding.predecessor ∧
    (resume s r commit).1.latestRequest = r.binding.child := by
  unfold resume at *
  split at * <;> try simp_all
  split at *
  · split at * <;> try simp_all
  · split at * <;> try simp_all
    split at * <;> try simp_all
    split at * <;> try simp_all
    rename_i post hpost
    split at * <;> try simp_all
    unfold Goals.step? at hpost
    split at hpost <;> try simp_all
    exact congrArg Goals.State.status hpost.2.symm

theorem successful_commit_retry_recovers_same_child (s : Snapshot) (r : Request)
    (h : (resume s r true).2 = .created) :
    resume (resume s r true).1 r true = ((resume s r true).1, .recovered) := by
  have publication := created_publishes_atomically s r true h
  have auth : r.authorized = true ∧ r.parentBelongsToGoal = true := by
    unfold resume at h
    split at h
    · simp at h
    · rename_i allowed
      simpa using allowed
  generalize hx : (resume s r true).1 = post at *
  simp only [resume, auth.1, auth.2, Bool.not_true, Bool.false_or, Bool.false_eq_true,
    ↓reduceIte, publication.2.1]
  simp [List.find?, sameKey]

/-- Configuration may preserve Active but cannot reactivate an existing Goal.
The actual non-resume transition remains the existing Goals.step? policy. -/
def configMaySetStatus (current target : Goals.Status) : Bool :=
  !(current != .active && target == .active)

theorem config_cannot_reactivate (current : Goals.Status) (h : current ≠ .active) :
    configMaySetStatus current .active = false := by
  cases current <;> try simp_all [configMaySetStatus]

theorem active_config_remains_allowed : configMaySetStatus .active .active = true := rfl

end GoalAutomation.OperatorResume
