import Proofs.Basic

/-!
# Startup readiness under bounded build failures

The startup barrier seeds every snapshot-runnable behavior as *pending* and the
process reports `Ready` only when no behavior is pending. A behavior leaves
pending today in exactly one way: its daemon starts (`mark_behavior_ready`),
which happens only **after** its completion client builds. A behavior that is
runnable at snapshot time but persistently fails to *build* therefore never
leaves pending — the slot hot-restarts forever, `wait_ready()` never returns,
and the process never reports `Ready` (defra-agent#559). The blast radius is
worse than a wrong status: the trigger engine is also gated on the barrier, so
one un-buildable behavior silently disables all schedules and event triggers.

The fix modeled here: build attempts consume a **budget**. Each failed build of
a still-pending behavior counts against it; at the budget the behavior is
**demoted** — released from the barrier *without being claimed healthy*. Ready
and demoted are both absorbing for the barrier, so the barrier's pending set
only shrinks and `wait_ready` terminates after boundedly many attempts.

Two facts of the Rust runtime this model pins:

* Demotion is *not* readiness. `demoted` is a distinct terminal standing; the
  theorems forbid any path from a failed budget to `ready`.
* Slot keep/recreate across reconcile generations carries the standing: an
  **unchanged** behavior keeps its slot (and so its demotion — nothing changed,
  the build will still fail), while a **changed** behavior gets a fresh slot
  with a fresh budget (the operator's fix earns a fresh chance).

There is a third way to leave the barrier, with no build outcome at all:
reconcile can **retire** a behavior's slot (config change or removal) before it
ever starts. The barrier previously had no retirement path, so a behavior
retired mid-startup orphaned its pending entry — the same hang with no failure
anywhere. `retire` supersedes the standing: released, unclaimed, accounted.

Model boundary: the outcome list is the sequence of build attempts the slot
actually observes (across all of its executor workers — the runtime counts them
on one shared counter). A build that *hangs* is surfaced back into this machine
by the runtime's per-attempt build timeout, which converts it into a `failed`
outcome — so the termination theorem covers hangs too, and `Ready` never has to
be force-flipped by a deadline that would weaken what it claims.
-/

namespace RuntimeReconcile.StartupReadiness

/-- One build attempt's outcome, as the slot loop observes it. -/
inductive BuildOutcome
  /-- The daemon began `run()`: the behavior is serving. -/
  | started
  /-- The build returned an error before the daemon started. -/
  | failed
  deriving DecidableEq, Repr

/-- A behavior's standing with the startup barrier. -/
inductive BehaviorStanding
  /-- Seeded runnable, not yet started; carries consecutive build failures. -/
  | pending (failures : Nat)
  /-- Started successfully. The only standing that claims health. -/
  | ready
  /-- Released from the barrier after exhausting the build budget — accounted
      for, observable, and *never* claimed healthy. -/
  | demoted
  /-- Released because reconcile retired the slot before it started (config
      change or removal). Released, unclaimed, accounted. -/
  | superseded
  deriving DecidableEq, Repr

namespace BehaviorStanding

/-- The barrier no longer waits on this behavior. -/
def released : BehaviorStanding → Bool
  | .pending _ => false
  | .ready => true
  | .demoted => true
  | .superseded => true

end BehaviorStanding

/-- How one behavior's standing responds to one build outcome, under `budget`
tolerated failures. Ready and demoted are absorbing: post-start crashes restart
the daemon but never re-enter the barrier. -/
def step (budget : Nat) : BehaviorStanding → BuildOutcome → BehaviorStanding
  | .pending _, .started => .ready
  | .pending failures, .failed =>
      if failures + 1 < budget then .pending (failures + 1) else .demoted
  | standing, _ => standing

/-- A behavior's standing after a sequence of build attempts. -/
def run (budget : Nat) (standing : BehaviorStanding) (outcomes : List BuildOutcome) :
    BehaviorStanding :=
  outcomes.foldl (step budget) standing

/-- Reconcile retires the slot: a still-pending behavior is superseded; a
standing that already left the barrier keeps its verdict. -/
def retire : BehaviorStanding → BehaviorStanding
  | .pending _ => .superseded
  | standing => standing

/-- The fresh standing every snapshot-runnable behavior is seeded with. -/
def seeded : BehaviorStanding := .pending 0

/-- `Ready` fires exactly when no seeded behavior is still pending. -/
def processReady (standings : List BehaviorStanding) : Bool :=
  standings.all BehaviorStanding.released

/-! ## Absorption: leaving the barrier is permanent -/

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

/-! ## Soundness: demotion never claims health -/

/-- A behavior is `ready` only if some attempt actually `started`. Exhausting a
budget cannot manufacture health. -/
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

/-! ## Termination: the barrier cannot hang past the budget -/

/-- **The #559 theorem.** After `budget` build attempts — whatever their
outcomes — a seeded behavior has left the barrier. With every behavior's
attempts bounded, `wait_ready` terminates; a persistently un-buildable behavior
is demoted instead of wedging the process.

The `failures < budget` hypothesis is the reachability invariant: `step` demotes
the moment the budget is consumed, so a still-pending behavior always has spent
strictly less than the budget. `seeded` satisfies it whenever `0 < budget`
(which the runtime enforces). -/
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

/-- The seeded corollary in the form the runtime actually uses: with a positive
budget and at least `budget` attempts, a freshly seeded behavior is released. -/
theorem seeded_release (budget : Nat) (outcomes : List BuildOutcome)
    (hPos : 0 < budget) (hEnough : budget ≤ outcomes.length) :
    (run budget seeded outcomes).released = true :=
  budgeted_attempts_release budget 0 outcomes hPos (by simpa using hEnough)

/-! ## No premature demotion: success within budget wins -/

/-- A behavior whose build succeeds before the budget is exhausted becomes
`ready` — demotion never races a viable build. -/
theorem start_within_budget_is_ready (budget : Nat) (failures : Nat)
    (rest : List BuildOutcome) :
    run budget (.pending failures) (.started :: rest) = .ready := by
  simp only [run, List.foldl_cons, step]
  rw [show List.foldl (step budget) BehaviorStanding.ready rest
        = run budget .ready rest from rfl,
      released_absorbing budget .ready rest rfl]

/-- Demotion requires the whole budget: a demoted behavior really did fail
`budget` builds with no success interleaved. -/
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

/-! ## Process accounting -/

/-- `Ready` accounts for every behavior: each is genuinely serving, demoted
with its budget consumed, or superseded by reconcile — never silently dropped. -/
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

/-- Retirement always releases: reconcile retiring a slot mid-startup cannot
orphan a pending entry — the second hang path of #559. -/
theorem retire_releases (standing : BehaviorStanding) :
    (retire standing).released = true := by
  cases standing <;> rfl

/-- Retirement never claims health: only a real start produces `ready`. -/
theorem retire_never_claims_ready (standing : BehaviorStanding)
    (hNotReady : standing ≠ .ready) :
    retire standing ≠ .ready := by
  cases standing with
  | pending _ => simp [retire]
  | ready => exact absurd rfl hNotReady
  | demoted => simp [retire]
  | superseded => simp [retire]

/-! ## Generations: keep carries the standing, recreate resets it -/

/-- Reconcile's slot diffing, restricted to what matters here: an unchanged
behavior keeps its slot; a changed behavior gets a fresh one. -/
def acrossGeneration (changed : Bool) (standing : BehaviorStanding) : BehaviorStanding :=
  if changed then seeded else standing

/-- A no-op republish never resurrects a known-bad build. -/
theorem demotion_persists_when_unchanged (standing : BehaviorStanding) :
    acrossGeneration false standing = standing := rfl

/-- A config change earns a fresh budget: the operator's fix gets a real
chance instead of inheriting a spent counter. -/
theorem change_restores_the_budget (standing : BehaviorStanding) :
    acrossGeneration true standing = seeded := rfl

end RuntimeReconcile.StartupReadiness
