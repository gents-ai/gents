import Proofs.GoalAutomation.Edit
import Proofs.GoalAutomation.OperatorResume

namespace GoalAutomation.ClientControl

/-- Document identity and the DefraDB composite CID fence one observed assignment.
A later commit has a different CID even when its status returns to an earlier
value. The native adapter uses the existing composite-version decoder and its
transaction owner; status and continuation sequence alone are not this fence. -/
structure Revision where
  document : String
  composite : String
  deriving DecidableEq, Repr

structure Snapshot where
  revision : Revision
  execution : OperatorResume.Snapshot
  objective : String
  deriving DecidableEq, Repr

inductive Operation where
  | edit (fields : Edit.Fields)
  | pause
  | resume
  | stop
  deriving DecidableEq, Repr

structure Facts where
  /-- Verified by the existing session ownership, author signature, current
  enrollment and applied-route owners. Never supplied as a wire grant. -/
  authorized : Bool
  /-- The existing whole-session idle owner still observes unfinished work. -/
  unfinishedRequest : Bool
  /-- Prepared by the existing authenticated continuation owner for an idle session. -/
  resume : Option OperatorResume.Request
  deriving DecidableEq, Repr

def assignment (s : Snapshot) : Edit.Assignment :=
  ⟨s.execution.goal.status, s.execution.sequence, s.objective,
    s.execution.tokenBudget.map (fun budget => (budget : Int))⟩

def withGoal (s : Snapshot) (revision : Revision) (goal : Goals.State) : Snapshot :=
  { s with revision, execution := { s.execution with goal } }

/-- Pause changes continuation permission without interrupting current work.
Only an explicitly paused goal with current owned work can resume without a
new child. Idle resume, blocked recovery and usage-limit recovery retain the
existing atomic continuation owner. Budget-limited goals retain its refusal. -/
def apply? (s : Snapshot) (expected : Revision) (nextComposite : String) (operation : Operation)
    (facts : Facts) : Option Snapshot :=
  if !facts.authorized || s.revision != expected then none
  else
    let next := { s.revision with composite := nextComposite }
    match operation with
  | .edit fields =>
      (Edit.apply? (assignment s) (assignment s) fields .client).map fun edited =>
        { s with
          revision := next
          objective := edited.objective
          execution := { s.execution with tokenBudget := edited.budget.map Int.toNat } }
  | .pause => (Goals.step? s.execution.goal .pause).map (withGoal s next)
  | .stop => (Goals.step? s.execution.goal .stop).map (withGoal s next)
  | .resume =>
      if facts.unfinishedRequest then
        if s.execution.goal.status = .paused then
          (Goals.step? s.execution.goal .resume).map (withGoal s next)
        else none
      else match facts.resume with
      | none => none
      | some request =>
          let result := OperatorResume.resume s.execution request true
          if result.2 = .created ∨ result.2 = .recovered then
            some { s with revision := next, execution := result.1 }
          else none

theorem unauthorized_cannot_change_goal (s : Snapshot) (expected : Revision) (next : String)
    (operation : Operation) (facts : Facts) (h : facts.authorized = false) :
    apply? s expected next operation facts = none := by
  simp [apply?, h]

theorem stale_revision_cannot_change_goal (s : Snapshot) (expected : Revision) (next : String)
    (operation : Operation) (facts : Facts) (h : s.revision ≠ expected) :
    apply? s expected next operation facts = none := by
  simp [apply?, h]

theorem edit_preserves_goal_and_request_owner (s post : Snapshot)
    (expected : Revision) (next : String) (fields : Edit.Fields) (facts : Facts)
    (h : apply? s expected next (.edit fields) facts = some post) :
    post.execution.goal = s.execution.goal ∧
      post.execution.sequence = s.execution.sequence ∧
      post.execution.latestRequest = s.execution.latestRequest ∧
      post.execution.children = s.execution.children := by
  unfold apply? at h
  split at h
  · contradiction
  · dsimp at h
    cases he : Edit.apply? (assignment s) (assignment s) fields .client with
    | none => simp [he] at h
    | some edited =>
      simp only [he, Option.map_some, Option.some.injEq] at h
      cases h
      exact ⟨rfl, rfl, rfl, rfl⟩

theorem pause_preserves_request_owner (s post : Snapshot) (expected : Revision) (next : String)
    (facts : Facts) (h : apply? s expected next .pause facts = some post) :
    post.execution.latestRequest = s.execution.latestRequest ∧
      post.execution.sequence = s.execution.sequence ∧
      post.execution.children = s.execution.children := by
  unfold apply? at h
  split at h
  · contradiction
  · cases hg : Goals.step? s.execution.goal .pause with
    | none => simp [hg] at h
    | some goal =>
      simp only [hg, Option.map_some, Option.some.injEq] at h
      cases h
      exact ⟨rfl, rfl, rfl⟩

theorem stop_preserves_request_owner (s post : Snapshot) (expected : Revision) (next : String)
    (facts : Facts) (h : apply? s expected next .stop facts = some post) :
    post.execution.latestRequest = s.execution.latestRequest ∧
      post.execution.sequence = s.execution.sequence ∧
      post.execution.children = s.execution.children ∧
      post.execution.tokensUsed = s.execution.tokensUsed ∧
      post.execution.tokenBudget = s.execution.tokenBudget := by
  unfold apply? at h
  split at h
  · contradiction
  · cases hg : Goals.step? s.execution.goal .stop with
    | none => simp [hg] at h
    | some goal =>
      simp only [hg, Option.map_some, Option.some.injEq] at h
      cases h
      exact ⟨rfl, rfl, rfl, rfl, rfl⟩

theorem busy_resume_preserves_request_owner (s post : Snapshot)
    (expected : Revision) (next : String) (facts : Facts) (active : facts.unfinishedRequest = true)
    (h : apply? s expected next .resume facts = some post) :
    post.execution.latestRequest = s.execution.latestRequest ∧
      post.execution.sequence = s.execution.sequence ∧
      post.execution.children = s.execution.children := by
  unfold apply? at h
  split at h
  · contradiction
  · simp only [active, ↓reduceIte] at h
    split at h
    · cases hg : Goals.step? s.execution.goal .resume with
      | none => simp [hg] at h
      | some goal =>
        simp only [hg, Option.map_some, Option.some.injEq] at h
        cases h
        exact ⟨rfl, rfl, rfl⟩
    · contradiction

theorem busy_resume_requires_paused_goal (s : Snapshot) (expected : Revision) (next : String)
    (facts : Facts) (active : facts.unfinishedRequest = true)
    (inactive : s.execution.goal.status ≠ .paused) :
    apply? s expected next .resume facts = none := by
  simp [apply?, active, inactive]

theorem preserves_document_identity (s post : Snapshot) (expected : Revision) (next : String)
    (operation : Operation) (facts : Facts)
    (h : apply? s expected next operation facts = some post) :
    post.revision.document = s.revision.document := by
  unfold apply? at h
  split at h
  · contradiction
  · cases operation with
    | edit fields =>
      dsimp at h
      cases he : Edit.apply? (assignment s) (assignment s) fields .client with
      | none => simp [he] at h
      | some edited =>
        simp only [he, Option.map_some, Option.some.injEq] at h
        cases h
        rfl
    | pause =>
      dsimp at h
      cases hp : Goals.step? s.execution.goal .pause with
      | none => simp [hp] at h
      | some goal =>
        simp only [hp, Option.map_some, Option.some.injEq] at h
        cases h
        rfl
    | stop =>
      dsimp at h
      cases hs : Goals.step? s.execution.goal .stop with
      | none => simp [hs] at h
      | some goal =>
        simp only [hs, Option.map_some, Option.some.injEq] at h
        cases h
        rfl
    | resume =>
      dsimp at h
      split at h
      · split at h
        · cases hr : Goals.step? s.execution.goal .resume with
          | none => simp [hr] at h
          | some goal =>
            simp only [hr, Option.map_some, Option.some.injEq] at h
            cases h
            rfl
        · contradiction
      · cases hr : facts.resume with
        | none => simp [hr] at h
        | some request =>
          simp only [hr] at h
          split at h
          · simp only [Option.some.injEq] at h
            cases h
            rfl
          · contradiction

end GoalAutomation.ClientControl
