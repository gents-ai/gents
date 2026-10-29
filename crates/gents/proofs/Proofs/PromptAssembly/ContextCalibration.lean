import Proofs.PromptAssembly.Budget
import Mathlib.Data.List.Basic

/-! The compaction trigger. Provider-input size is this estimator's raw count
scaled by a provider/estimate ratio, or, when the session's latest accepted
call still describes a prefix of the request, that call's provider count plus
the scaled growth since it. Ratios are basis points: 10000 is one provider
token per estimated token. -/

namespace PromptAssembly.ContextCalibration

open PromptAssembly.Budget

def unitRatio : Nat := 10000

/-- A single malformed usage report must not make every later request look
several times its size. -/
def maxRatio : Nat := 40000

/-- The ratio is the largest of this many latest observations: a higher one
takes effect at once, a lower one only once the higher ones age out.
Overestimating costs an earlier compaction; underestimating can overflow the
provider and end the request. -/
def observationWindow : Nat := 8

def clampRatio (ratio : Nat) : Nat := min (max ratio unitRatio) maxRatio

def ceilDiv (numerator denominator : Nat) : Nat :=
  (numerator + denominator - 1) / denominator

/-- Provider tokens per estimated token for one completed call, floored at
one: the ratio only ever raises an estimate. -/
def observedRatio (observedInput estimatedInput : Nat) : Option Nat :=
  if observedInput = 0 ∨ estimatedInput = 0 then none
  else some (clampRatio (ceilDiv (observedInput * unitRatio) estimatedInput))

/-- The provider's own count of the latest accepted call and this
estimator's raw count of the request it answered. -/
structure Anchor where
  input : Nat
  output : Nat
  estimated : Nat
  deriving DecidableEq, Repr

structure State where
  prior : Nat
  observations : List Nat
  anchor : Option Anchor
  deriving DecidableEq, Repr

def seedWindow (observations : List Nat) : List Nat :=
  (observations.map clampRatio).drop (observations.length - observationWindow)

def seed (prior : Nat) (observations : List Nat) : State :=
  { prior := clampRatio prior
  , observations := seedWindow observations
  , anchor := none }

/-- The window after one more observation: appended, and the oldest dropped
once the window is full. -/
def pushWindow (observations : List Nat) (ratio : Nat) : List Nat :=
  (observations ++ [clampRatio ratio]).drop (observations.length + 1 - observationWindow)

def State.push (s : State) (ratio : Nat) : State :=
  { s with observations := pushWindow s.observations ratio }

def State.observe (s : State) (observedInput estimatedInput : Nat) : State :=
  match observedRatio observedInput estimatedInput with
  | some ratio => s.push ratio
  | none => s

/-- A reduction or rewrite of the transcript: the anchored call no longer
describes a prefix of the next request. -/
def State.invalidate (s : State) : State := { s with anchor := none }

def State.ratio (s : State) : Nat :=
  match s.observations with
  | [] => s.prior
  | first :: rest => rest.foldl max first

def calibrated (raw ratio : Nat) : Nat :=
  ceilDiv (raw * max ratio unitRatio) unitRatio

/-- The anchored call's provider count plus the scaled growth past it, net
of the anchored response the growth replays. A request smaller than the
anchored one has been reduced since and is not anchored. -/
def anchored (anchor : Anchor) (raw ratio : Nat) : Option Nat :=
  if raw < anchor.estimated then none
  else some (anchor.input + anchor.output +
    (calibrated (raw - anchor.estimated) ratio - anchor.output))

/-- What every decision uses: never below the calibrated count, so a missing
or stale anchor never lowers it. -/
def State.tokens (s : State) (raw : Nat) : Nat :=
  match s.anchor.bind (fun anchor => anchored anchor raw s.ratio) with
  | some count => max count (calibrated raw s.ratio)
  | none => calibrated raw s.ratio

def estimatorBudget (budget ratio : Nat) : Nat :=
  budget * unitRatio / max ratio unitRatio

def estimatorUnits (tokens ratio : Nat) : Nat :=
  ceilDiv (tokens * unitRatio) (max ratio unitRatio)

/-- A reduction the trigger admits, in estimator units. -/
structure Admission where
  inputTokens : Nat
  inputBudget : Nat
  deriving DecidableEq, Repr

/-- The compaction trigger: reduce when the context exceeds the compaction
budget. The admission is in estimator units: the raw count, unless the
anchor is what put the request over budget. -/
def State.admission (s : State) (raw contextWindow basisPoints maxOutputTokens : Nat) :
    Option Admission :=
  let tokens := s.tokens raw
  let budget := compactionInputBudget contextWindow basisPoints maxOutputTokens
  match decideThreshold tokens budget with
  | .notNeeded => none
  | .reduceEligible => some
      { inputTokens :=
          if calibrated raw s.ratio < tokens then max (estimatorUnits tokens s.ratio) raw
          else raw
      , inputBudget := estimatorBudget budget s.ratio }

theorem clamp_ratio_bounds (ratio : Nat) :
    unitRatio ≤ clampRatio ratio ∧ clampRatio ratio ≤ maxRatio := by
  unfold clampRatio unitRatio maxRatio
  omega

private theorem foldl_max_ge_init (rest : List Nat) (init : Nat) :
    init ≤ rest.foldl max init := by
  induction rest generalizing init with
  | nil => exact Nat.le_refl _
  | cons head tail ih => exact Nat.le_trans (Nat.le_max_left init head) (ih (max init head))

private theorem foldl_max_ge_mem (rest : List Nat) (init x : Nat) (hx : x ∈ rest) :
    x ≤ rest.foldl max init := by
  induction rest generalizing init with
  | nil => cases hx
  | cons head tail ih =>
    cases hx with
    | head => exact Nat.le_trans (Nat.le_max_right init x) (foldl_max_ge_init tail (max init x))
    | tail _ htail => exact ih (max init head) htail

private theorem foldl_max_bounded (rest : List Nat) (init lo hi : Nat)
    (hinit : lo ≤ init ∧ init ≤ hi) (hrest : ∀ x ∈ rest, lo ≤ x ∧ x ≤ hi) :
    lo ≤ rest.foldl max init ∧ rest.foldl max init ≤ hi := by
  induction rest generalizing init with
  | nil => exact hinit
  | cons head tail ih =>
    apply ih
    · have hhead := hrest head (List.Mem.head _)
      constructor
      · exact Nat.le_trans hinit.1 (Nat.le_max_left _ _)
      · exact Nat.max_le.mpr ⟨hinit.2, hhead.2⟩
    · exact fun x hx => hrest x (List.Mem.tail _ hx)

private theorem ratio_ge_mem (observations : List Nat) (prior x : Nat)
    (hx : x ∈ observations) :
    x ≤ ({ prior := prior, observations := observations, anchor := none } : State).ratio := by
  unfold State.ratio
  cases observations with
  | nil => cases hx
  | cons first rest =>
    cases hx with
    | head => exact foldl_max_ge_init rest x
    | tail _ htail => exact foldl_max_ge_mem rest first x htail

/-- A higher observation takes effect at once. -/
theorem observation_takes_effect_at_once (s : State) (ratio : Nat) :
    clampRatio ratio ≤ (s.push ratio).ratio := by
  have hdrop : s.observations.length + 1 - observationWindow ≤ s.observations.length := by
    unfold observationWindow; omega
  have hmem : clampRatio ratio ∈ pushWindow s.observations ratio := by
    unfold pushWindow
    rw [List.drop_append_of_le_length hdrop]
    exact List.mem_append_right _ (List.Mem.head _)
  exact ratio_ge_mem _ s.prior _ hmem

theorem push_keeps_window (s : State) (ratio : Nat)
    (hwindow : s.observations.length ≤ observationWindow) :
    (s.push ratio).observations.length ≤ observationWindow := by
  unfold State.push pushWindow
  simp only [List.length_drop, List.length_append, List.length_singleton]
  unfold observationWindow at hwindow ⊢
  omega

/-- A seeded state's ratio stays within the clamp. -/
theorem seeded_ratio_bounds (prior : Nat) (observations : List Nat) :
    unitRatio ≤ (seed prior observations).ratio ∧
      (seed prior observations).ratio ≤ maxRatio := by
  have hall : ∀ x ∈ seedWindow observations, unitRatio ≤ x ∧ x ≤ maxRatio := by
    intro x hx
    unfold seedWindow at hx
    obtain ⟨y, _, rfl⟩ := List.mem_map.mp (List.mem_of_mem_drop hx)
    exact clamp_ratio_bounds y
  unfold seed State.ratio
  revert hall
  generalize seedWindow observations = kept
  intro hall
  cases kept with
  | nil => exact clamp_ratio_bounds prior
  | cons first rest =>
    exact foldl_max_bounded rest first unitRatio maxRatio
      (hall first (List.Mem.head _))
      (fun x hx => hall x (List.Mem.tail _ hx))

theorem tokens_ge_calibrated (s : State) (raw : Nat) :
    calibrated raw s.ratio ≤ s.tokens raw := by
  unfold State.tokens
  split
  · exact Nat.le_max_right _ _
  · exact Nat.le_refl _

theorem invalidated_tokens_are_calibrated (s : State) (raw : Nat) :
    s.invalidate.tokens raw = calibrated raw s.ratio := by
  rfl

theorem admission_iff_over_budget
    (s : State) (raw contextWindow basisPoints maxOutputTokens : Nat) :
    (s.admission raw contextWindow basisPoints maxOutputTokens).isSome ↔
      compactionInputBudget contextWindow basisPoints maxOutputTokens < s.tokens raw := by
  unfold State.admission decideThreshold
  by_cases h : s.tokens raw ≤ compactionInputBudget contextWindow basisPoints maxOutputTokens
  · simp [h]
  · simp [h]
    omega

end PromptAssembly.ContextCalibration
