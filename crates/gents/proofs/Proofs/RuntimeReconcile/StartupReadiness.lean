import Proofs.Basic
import Mathlib.Data.Finset.Basic

namespace RuntimeReconcile.StartupReadiness

inductive BuildOutcome
  | started
  | failed
  deriving DecidableEq, Repr

inductive BehaviorStanding
  | pending (failures : Nat)
  | ready
  | demoted
  | superseded
  deriving DecidableEq, Repr

namespace BehaviorStanding

def released : BehaviorStanding → Bool
  | .pending _ => false
  | .ready => true
  | .demoted => true
  | .superseded => true

end BehaviorStanding

def step (budget : Nat) : BehaviorStanding → BuildOutcome → BehaviorStanding
  | .pending _, .started => .ready
  | .pending failures, .failed =>
      if failures + 1 < budget then .pending (failures + 1) else .demoted
  | standing, _ => standing

def run (budget : Nat) (standing : BehaviorStanding) (outcomes : List BuildOutcome) :
    BehaviorStanding :=
  outcomes.foldl (step budget) standing

def retire : BehaviorStanding → BehaviorStanding
  | .pending _ => .superseded
  | standing => standing

def seeded : BehaviorStanding := .pending 0

def processReady (standings : List BehaviorStanding) : Bool :=
  standings.all BehaviorStanding.released

theorem ready_absorbing (budget : Nat) (outcome : BuildOutcome) :
    step budget .ready outcome = .ready := by
  cases outcome <;> rfl

theorem demoted_absorbing (budget : Nat) (outcome : BuildOutcome) :
    step budget .demoted outcome = .demoted := by
  cases outcome <;> rfl

theorem released_absorbing (budget : Nat) (standing : BehaviorStanding)
    (outcomes : List BuildOutcome) (hReleased : standing.released = true) :
    run budget standing outcomes = standing := by
  induction outcomes with
  | nil => rfl
  | cons outcome rest ih =>
      cases standing with
      | pending _ => simp [BehaviorStanding.released] at hReleased
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
            rw [show List.foldl (step budget) BehaviorStanding.demoted rest
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
          rw [show List.foldl (step budget) BehaviorStanding.ready rest
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
            rw [show List.foldl (step budget) BehaviorStanding.demoted rest
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
  rw [show List.foldl (step budget) BehaviorStanding.ready rest
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

theorem process_ready_accounts (standings : List BehaviorStanding)
    (hReady : processReady standings = true) :
    ∀ standing ∈ standings,
      standing = .ready ∨ standing = .demoted ∨ standing = .superseded := by
  intro standing hMem
  have := List.all_eq_true.mp hReady standing hMem
  cases standing with
  | pending _ => simp [BehaviorStanding.released] at this
  | ready => exact Or.inl rfl
  | demoted => exact Or.inr (Or.inl rfl)
  | superseded => exact Or.inr (Or.inr rfl)

theorem retire_releases (standing : BehaviorStanding) :
    (retire standing).released = true := by
  cases standing <;> rfl

theorem retire_never_claims_ready (standing : BehaviorStanding)
    (hNotReady : standing ≠ .ready) :
    retire standing ≠ .ready := by
  cases standing with
  | pending _ => simp [retire]
  | ready => exact absurd rfl hNotReady
  | demoted => simp [retire]
  | superseded => simp [retire]

def acrossGeneration (changed : Bool) (standing : BehaviorStanding) : BehaviorStanding :=
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

theorem demotion_persists_when_unchanged (standing : BehaviorStanding) :
    acrossGeneration false standing = standing := rfl

theorem change_restores_the_budget (standing : BehaviorStanding) :
    acrossGeneration true standing = seeded := rfl

end RuntimeReconcile.StartupReadiness
