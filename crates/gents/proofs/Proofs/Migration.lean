import Proofs.Basic

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

def activate (c : CollectionState) (dest : VersionId) (expectedTx : Option TransformId) :
    Option CollectionState :=
  if verified c dest expectedTx then some (setActive c dest) else none

theorem activate_requires_verified
    (c : CollectionState) (dest : VersionId) (tx : Option TransformId)
    (c' : CollectionState) (h : activate c dest tx = some c') :
    verified c dest tx = true := by
  unfold activate at h
  by_cases hv : verified c dest tx = true
  · exact hv
  · simp [hv] at h

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

theorem activate_none_when_unverified
    (c : CollectionState) (dest : VersionId) (tx : Option TransformId)
    (h : verified c dest tx = false) :
    activate c dest tx = none := by
  simp [activate, h]

def materializeStep (n : Nat) : Nat := n - 1

theorem materialize_decreases (n : Nat) (h : 0 < n) :
    materializeStep n < n := by
  simp [materializeStep]
  exact Nat.sub_lt h (by decide : (0 : Nat) < 1)

def materializeAll : Nat → Nat
  | 0 => 0
  | n + 1 => materializeAll n

theorem materializeAll_terminates (n : Nat) : materializeAll n = 0 := by
  induction n with
  | zero => rfl
  | succ n ih => exact ih

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
