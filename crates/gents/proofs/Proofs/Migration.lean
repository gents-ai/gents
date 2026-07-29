import Proofs.Basic

/-!
# Schema migration engine model (Phase A)

Ledger-free migration over DefraDB's version DAG. Verification is a predicate
over current state — never a stored fact. Crash position is derived from
observable version kind + active flag.

See `docs/superpowers/specs/2026-07-28-lens-first-migration-design.md`.

## Modeling boundary

- Version/transform IDs are opaque `Nat`s (content addressing is a substrate
  assumption).
- Document materialization is an abstract counter; P2P arrivals during a run
  are outside the termination theorem (quiescent snapshot).
- DefraDB storage-engine correctness is not modeled.
- Full multi-step `ensure` convergence and pin soundness over foreign DAGs are
  fenced by Rust conformance tests in `crates/gents-migration/tests`; this
  module holds the core predicates and the no-unverified-activation /
  materialization-termination arguments.
-/

namespace Migration

abbrev VersionId := Nat
abbrev TransformId := Nat

inductive VersionKind where
  | complete
  | placeholder
  deriving DecidableEq, Repr

structure VersionNode where
  id : VersionId
  kind : VersionKind
  active : Bool
  transform : Option TransformId
  expectationHolds : Bool
  deriving Repr, DecidableEq

structure CollectionState where
  versions : List VersionNode
  deriving Repr

/-- Crash position derived from observable state (design §1). -/
inductive CrashPos where
  | destAbsent
  | destPlaceholder
  | destCompleteInactive
  | destCompleteActive
  deriving DecidableEq, Repr

def findVersion (c : CollectionState) (vid : VersionId) : Option VersionNode :=
  c.versions.find? (fun v => v.id = vid)

def crashPos (c : CollectionState) (dest : VersionId) : CrashPos :=
  match findVersion c dest with
  | none => CrashPos.destAbsent
  | some v =>
      match v.kind with
      | VersionKind.placeholder => CrashPos.destPlaceholder
      | VersionKind.complete =>
          if v.active then CrashPos.destCompleteActive else CrashPos.destCompleteInactive

/-- Verification predicate: complete descriptor + expectation + optional transform. -/
def verified (c : CollectionState) (dest : VersionId) (expectedTx : Option TransformId) : Bool :=
  match findVersion c dest with
  | none => false
  | some v =>
      (v.kind == VersionKind.complete) &&
      v.expectationHolds &&
      match expectedTx with
      | none => true
      | some tx => v.transform == some tx

def setActive (c : CollectionState) (dest : VersionId) : CollectionState :=
  { versions := c.versions.map (fun v => { v with active := decide (v.id = dest) }) }

/-- Guarded activation: `none` when verification fails. -/
def activate (c : CollectionState) (dest : VersionId) (expectedTx : Option TransformId) :
    Option CollectionState :=
  if verified c dest expectedTx then some (setActive c dest) else none

/-- No unverified activation: every successful activate has a verified dest. -/
theorem activate_requires_verified
    (c : CollectionState) (dest : VersionId) (tx : Option TransformId)
    (c' : CollectionState) (h : activate c dest tx = some c') :
    verified c dest tx = true := by
  unfold activate at h
  by_cases hv : verified c dest tx = true
  · exact hv
  · simp [hv] at h

/-- After activation, a version is active iff its id is the destination. -/
theorem activate_sets_active
    (c : CollectionState) (dest : VersionId) (tx : Option TransformId)
    (c' : CollectionState) (h : activate c dest tx = some c')
    (v : VersionNode) (hin : v ∈ c'.versions) :
    v.active = true ↔ v.id = dest := by
  unfold activate at h
  by_cases hv : verified c dest tx = true
  · simp [hv] at h
    cases h
    simp [setActive] at hin
    rcases hin with ⟨v₀, hin₀, rfl⟩
    constructor
    · intro ha
      exact of_decide_eq_true ha
    · intro hid
      exact decide_eq_true hid
  · simp [hv] at h

/-- Activate refuses incomplete / unverified destinations. -/
theorem activate_none_when_unverified
    (c : CollectionState) (dest : VersionId) (tx : Option TransformId)
    (h : verified c dest tx = false) :
    activate c dest tx = none := by
  simp [activate, h]

/-- Materialization step: strictly decrease remaining work when positive. -/
def materializeStep (n : Nat) : Nat := n - 1

theorem materialize_decreases (n : Nat) (h : 0 < n) :
    materializeStep n < n := by
  simp [materializeStep]
  exact Nat.sub_lt h (by decide : (0 : Nat) < 1)

/-- Exhaustive materialization over a quiescent snapshot (count only). -/
def materializeAll : Nat → Nat
  | 0 => 0
  | n + 1 => materializeAll n

theorem materializeAll_terminates (n : Nat) : materializeAll n = 0 := by
  induction n with
  | zero => rfl
  | succ n ih => exact ih

/-- Crash positions partition the version-lookup outcomes. -/
theorem crashPos_complete_active
    (c : CollectionState) (dest : VersionId) (v : VersionNode)
    (hfind : findVersion c dest = some v)
    (hk : v.kind = VersionKind.complete) (ha : v.active = true) :
    crashPos c dest = CrashPos.destCompleteActive := by
  simp [crashPos, hfind, hk, ha]

theorem crashPos_complete_inactive
    (c : CollectionState) (dest : VersionId) (v : VersionNode)
    (hfind : findVersion c dest = some v)
    (hk : v.kind = VersionKind.complete) (ha : v.active = false) :
    crashPos c dest = CrashPos.destCompleteInactive := by
  simp [crashPos, hfind, hk, ha]

theorem crashPos_placeholder
    (c : CollectionState) (dest : VersionId) (v : VersionNode)
    (hfind : findVersion c dest = some v)
    (hk : v.kind = VersionKind.placeholder) :
    crashPos c dest = CrashPos.destPlaceholder := by
  simp [crashPos, hfind, hk]

theorem crashPos_absent
    (c : CollectionState) (dest : VersionId)
    (hfind : findVersion c dest = none) :
    crashPos c dest = CrashPos.destAbsent := by
  simp [crashPos, hfind]

end Migration
