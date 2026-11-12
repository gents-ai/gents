import Proofs.Shared.Basic
import Mathlib.Data.Finset.Basic

namespace RuntimeReconcile.StartupReadiness

inductive BuildOutcome
  | started
  | failed
  deriving DecidableEq, Repr

inductive AgentStanding
  | pending (failures : Nat)
  | ready
  | demoted
  | superseded
  deriving DecidableEq, Repr

namespace AgentStanding

def released : AgentStanding → Bool
  | .pending _ => false
  | .ready => true
  | .demoted => true
  | .superseded => true

end AgentStanding

def step (budget : Nat) : AgentStanding → BuildOutcome → AgentStanding
  | .pending _, .started => .ready
  | .pending failures, .failed =>
      if failures + 1 < budget then .pending (failures + 1) else .demoted
  | standing, _ => standing

def run (budget : Nat) (standing : AgentStanding) (outcomes : List BuildOutcome) :
    AgentStanding :=
  outcomes.foldl (step budget) standing

def retire : AgentStanding → AgentStanding
  | .pending _ => .superseded
  | standing => standing

def seeded : AgentStanding := .pending 0

def processReady (standings : List AgentStanding) : Bool :=
  standings.all AgentStanding.released

theorem ready_absorbing (budget : Nat) (outcome : BuildOutcome) :
    step budget .ready outcome = .ready := by
  cases outcome <;> rfl

theorem demoted_absorbing (budget : Nat) (outcome : BuildOutcome) :
    step budget .demoted outcome = .demoted := by
  cases outcome <;> rfl

theorem released_absorbing (budget : Nat) (standing : AgentStanding)
    (outcomes : List BuildOutcome) (hReleased : standing.released = true) :
    run budget standing outcomes = standing := by
  induction outcomes with
  | nil => rfl
  | cons outcome rest ih =>
      cases standing with
      | pending _ => simp [AgentStanding.released] at hReleased
      | ready => simp [run, List.foldl_cons, step] at ih ⊢; cases outcome <;> simpa [step] using ih
      | demoted => simp [run, List.foldl_cons, step] at ih ⊢; cases outcome <;> simpa [step] using ih
      | superseded =>
          simp [run, List.foldl_cons, step] at ih ⊢; cases outcome <;> simpa [step] using ih

theorem ready_requires_a_start (budget : Nat) (failures : Nat)
    (outcomes : List BuildOutcome)
    (hReady : run budget (.pending failures) outcomes = .ready) :
    .started ∈ outcomes := by
  induction outcomes generalizing failures with
  | nil => simp [run] at hReady
  | cons outcome rest ih =>
      cases outcome with
      | started => exact List.mem_cons_self ..
      | failed =>
          simp only [run, List.foldl_cons, step] at hReady
          by_cases hBudget : failures + 1 < budget
          · simp only [hBudget, if_true] at hReady
            exact List.mem_cons_of_mem _ (ih _ hReady)
          · simp only [hBudget, if_false] at hReady
            rw [show List.foldl (step budget) AgentStanding.demoted rest
                  = run budget .demoted rest from rfl,
                released_absorbing budget .demoted rest rfl] at hReady
            exact absurd hReady (by simp)

theorem budgeted_attempts_release (budget : Nat) (failures : Nat)
    (outcomes : List BuildOutcome)
    (hInv : failures < budget)
    (hEnough : budget ≤ failures + outcomes.length) :
    (run budget (.pending failures) outcomes).released = true := by
  induction outcomes generalizing failures with
  | nil =>
      simp only [List.length_nil, Nat.add_zero] at hEnough
      omega
  | cons outcome rest ih =>
      cases outcome with
      | started =>
          simp only [run, List.foldl_cons, step]
          rw [show List.foldl (step budget) AgentStanding.ready rest
                = run budget .ready rest from rfl,
              released_absorbing budget .ready rest rfl]
          rfl
      | failed =>
          simp only [run, List.foldl_cons, step]
          by_cases hBudget : failures + 1 < budget
          · simp only [hBudget, if_true]
            exact ih (failures + 1) hBudget (by
              simp only [List.length_cons] at hEnough
              omega)
          · simp only [hBudget, if_false]
            rw [show List.foldl (step budget) AgentStanding.demoted rest
                  = run budget .demoted rest from rfl,
                released_absorbing budget .demoted rest rfl]
            rfl

theorem seeded_release (budget : Nat) (outcomes : List BuildOutcome)
    (hPos : 0 < budget) (hEnough : budget ≤ outcomes.length) :
    (run budget seeded outcomes).released = true :=
  budgeted_attempts_release budget 0 outcomes hPos (by simpa using hEnough)

theorem start_within_budget_is_ready (budget : Nat) (failures : Nat)
    (rest : List BuildOutcome) :
    run budget (.pending failures) (.started :: rest) = .ready := by
  simp only [run, List.foldl_cons, step]
  rw [show List.foldl (step budget) AgentStanding.ready rest
        = run budget .ready rest from rfl,
      released_absorbing budget .ready rest rfl]

theorem demoted_consumed_the_budget (budget : Nat) (failures : Nat)
    (outcomes : List BuildOutcome)
    (hDemoted : run budget (.pending failures) outcomes = .demoted) :
    budget ≤ failures + outcomes.length := by
  induction outcomes generalizing failures with
  | nil =>
      simp [run] at hDemoted
  | cons outcome rest ih =>
      cases outcome with
      | started =>
          rw [start_within_budget_is_ready budget failures rest] at hDemoted
          exact absurd hDemoted (by simp)
      | failed =>
          simp only [run, List.foldl_cons, step] at hDemoted
          by_cases hBudget : failures + 1 < budget
          · simp only [hBudget, if_true] at hDemoted
            have := ih (failures + 1) hDemoted
            simpa [List.length_cons, Nat.add_comm, Nat.add_left_comm] using this
          · simp only [List.length_cons]
            omega

theorem process_ready_accounts (standings : List AgentStanding)
    (hReady : processReady standings = true) :
    ∀ standing ∈ standings,
      standing = .ready ∨ standing = .demoted ∨ standing = .superseded := by
  intro standing hMem
  have := List.all_eq_true.mp hReady standing hMem
  cases standing with
  | pending _ => simp [AgentStanding.released] at this
  | ready => exact Or.inl rfl
  | demoted => exact Or.inr (Or.inl rfl)
  | superseded => exact Or.inr (Or.inr rfl)

theorem retire_releases (standing : AgentStanding) :
    (retire standing).released = true := by
  cases standing <;> rfl

theorem retire_never_claims_ready (standing : AgentStanding)
    (hNotReady : standing ≠ .ready) :
    retire standing ≠ .ready := by
  cases standing with
  | pending _ => simp [retire]
  | ready => exact absurd rfl hNotReady
  | demoted => simp [retire]
  | superseded => simp [retire]

def acrossGeneration (changed : Bool) (standing : AgentStanding) : AgentStanding :=
  if changed then seeded else standing

/- During a reconcile handoff the active and staged slots coexist. Startup
readiness therefore owns one pending obligation per exact slot generation;
releasing a failed staged generation must not release the still-active one. -/
structure PendingGenerationOwners where
  generations : Finset Nat
  deriving DecidableEq

def registerGeneration (generation : Nat)
    (current : PendingGenerationOwners) : PendingGenerationOwners :=
  { generations := insert generation current.generations }

def releaseGeneration (generation : Nat)
    (current : PendingGenerationOwners) : PendingGenerationOwners :=
  { generations := current.generations.erase generation }

def generationPending (generation : Nat) (current : PendingGenerationOwners) : Prop :=
  generation ∈ current.generations

def allGenerationsReleased (current : PendingGenerationOwners) : Prop :=
  current.generations = ∅

theorem registration_adds_exact_pending_obligation
    (current : PendingGenerationOwners) (generation : Nat) :
    generationPending generation (registerGeneration generation current) := by
  simp [generationPending, registerGeneration]

theorem exact_release_removes_pending_obligation
    (current : PendingGenerationOwners) (generation : Nat) :
    ¬ generationPending generation (releaseGeneration generation current) := by
  simp [generationPending, releaseGeneration]

theorem releasing_one_generation_preserves_another
    (current : PendingGenerationOwners) (releasedGeneration pendingGeneration : Nat)
    (hDifferent : pendingGeneration ≠ releasedGeneration)
    (hPending : generationPending pendingGeneration current) :
    generationPending pendingGeneration (releaseGeneration releasedGeneration current) := by
  unfold generationPending at hPending ⊢
  exact Finset.mem_erase.mpr ⟨hDifferent, hPending⟩

theorem staged_abort_preserves_active_pending
    (current : PendingGenerationOwners) (activeGeneration stagedGeneration : Nat)
    (hDifferent : activeGeneration ≠ stagedGeneration)
    (hActive : generationPending activeGeneration current) :
    generationPending activeGeneration
      (releaseGeneration stagedGeneration (registerGeneration stagedGeneration current)) := by
  unfold generationPending at hActive ⊢
  exact Finset.mem_erase.mpr ⟨hDifferent, Finset.mem_insert_of_mem hActive⟩

structure ScopedDemotion where
  generation : Nat
  active : Bool
  deriving DecidableEq, Repr

def visibleDemotion (sourceGeneration : Nat) (demotion : ScopedDemotion) : Bool :=
  demotion.active && demotion.generation = sourceGeneration

def clearDemotion (expectedGeneration : Nat) (demotion : ScopedDemotion) : ScopedDemotion :=
  if demotion.generation = expectedGeneration then { demotion with active := false }
  else demotion

theorem newer_ready_cannot_clear_visible_old_source_demotion
    (sourceGeneration newGeneration : Nat)
    (hDifferent : sourceGeneration ≠ newGeneration) :
    let demotion : ScopedDemotion := { generation := sourceGeneration, active := true }
    visibleDemotion sourceGeneration (clearDemotion newGeneration demotion) = true := by
  simp [clearDemotion, visibleDemotion, hDifferent]

theorem old_demotion_is_not_visible_after_source_advance
    (oldGeneration newGeneration : Nat)
    (hDifferent : oldGeneration ≠ newGeneration) :
    visibleDemotion newGeneration { generation := oldGeneration, active := true } = false := by
  simp [visibleDemotion, hDifferent]

theorem demotion_persists_when_unchanged (standing : AgentStanding) :
    acrossGeneration false standing = standing := rfl

theorem change_restores_the_budget (standing : AgentStanding) :
    acrossGeneration true standing = seeded := rfl

/- Readiness is durable semantic state, not a transport lease. A newly
started publisher has no committed snapshot and must publish Recovering;
subsequent publication requires a semantic change, not elapsed time. -/
def shouldPublish {α : Type} [DecidableEq α] (previous : Option α) (next : α) : Bool :=
  decide (previous ≠ some next)

theorem initial_state_publishes {α : Type} [DecidableEq α] (next : α) :
    shouldPublish none next = true := by simp [shouldPublish]

theorem unchanged_state_does_not_publish {α : Type} [DecidableEq α] (state : α) :
    shouldPublish (some state) state = false := by simp [shouldPublish]

theorem changed_state_publishes {α : Type} [DecidableEq α] (previous next : α)
    (hChanged : previous ≠ next) :
    shouldPublish (some previous) next = true := by simp [shouldPublish, hChanged]

/-- How an agent's resolved tool configuration fares when the runtime admits
it: its surface builds, it resolves but fails to build (a named plugin is not
installed), or it names an agent target that does not resolve. -/
inductive ToolAdmission where
  | builds
  | failsToBuild
  | unresolvedTarget
  deriving DecidableEq, Repr

inductive UnavailableReason where
  | toolSurfaceUnavailable
  | toolConfigurationInvalid
  deriving DecidableEq, Repr

def admissionReason : ToolAdmission → Option UnavailableReason
  | .builds => none
  | .failsToBuild => some .toolSurfaceUnavailable
  | .unresolvedTarget => some .toolConfigurationInvalid

/-- Only an unresolved target is a fault of the agent's own configuration; its
private diagnostic names the target so an operator can repair it. -/
def diagnosticNamesTarget (admission : ToolAdmission) : Bool :=
  admissionReason admission == some .toolConfigurationInvalid

def admittedStanding (admission : ToolAdmission) : AgentStanding :=
  if (admissionReason admission).isSome then .demoted else .ready

def runnable (standing : AgentStanding) : Bool := standing == .ready

/-- What depends on an agent follows its standing: another agent offers it as
a target, its tasks stay active, and its schedules stay active only while it
is runnable; a demoted agent's schedules are reported unavailable. -/
structure Dependents where
  offeredAsTarget : Bool
  taskActive : Bool
  scheduleActive : Bool
  scheduleUnavailable : Bool
  deriving DecidableEq, Repr

def dependents (standing : AgentStanding) : Dependents :=
  { offeredAsTarget := runnable standing
    taskActive := runnable standing
    scheduleActive := runnable standing
    scheduleUnavailable := standing == .demoted }

/-- The control watcher re-admits every resolved snapshot it proposes; a wake
that does not change the agent's source carries its standing forward. -/
def afterWake (admission : ToolAdmission) : AgentStanding :=
  acrossGeneration false (admittedStanding admission)

theorem startup_demotion_cascades_to_targets_schedules_and_tasks :
    dependents (admittedStanding .failsToBuild) =
      { offeredAsTarget := false, taskActive := false, scheduleActive := false,
        scheduleUnavailable := true } := by decide

theorem from_default_agent_documents_rejects_unresolved_agent_target :
    admittedStanding .unresolvedTarget = .demoted ∧
      admissionReason .unresolvedTarget = some .toolConfigurationInvalid ∧
      diagnosticNamesTarget .unresolvedTarget = true := by decide

theorem control_watcher_keeps_an_agent_whose_tools_fail_to_build_demoted :
    afterWake .failsToBuild = .demoted ∧
      admissionReason .failsToBuild = some .toolSurfaceUnavailable := by decide

end RuntimeReconcile.StartupReadiness
