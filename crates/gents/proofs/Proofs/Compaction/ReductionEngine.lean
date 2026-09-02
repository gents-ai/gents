import Proofs.PromptAssembly.Budget

/-!
# Shared reduction decision and outcome

The compactor owns one scope-independent decision/outcome engine. It either
leaves the provider view unchanged, reports that no legal reduction can fit, or
returns one checkpoint over an exact prefix together with the exact suffix.
Session compaction and request-local provider-context reduction deliberately
commit that shared outcome through separate persistence functions.
-/

namespace Compaction.ReductionEngine

inductive Decision where
  | notNeeded
  | cannotFit
  | reduce (prefixLength : Nat) (checkpoint : Nat)
  deriving DecidableEq, Repr

abbrev ThresholdDecision := PromptAssembly.Budget.ThresholdDecision

/-- The one input-driven threshold gate. Equality is admitted; only one token
over the effective input budget makes reduction eligible. `cannotFit` remains
an execution outcome after an eligible reduction cannot produce a legal split. -/
def decideThreshold (inputTokens effectiveInputBudget : Nat) : ThresholdDecision :=
  PromptAssembly.Budget.decideThreshold inputTokens effectiveInputBudget

theorem threshold_equality_is_not_needed (effectiveInputBudget : Nat) :
    decideThreshold effectiveInputBudget effectiveInputBudget = .notNeeded := by
  simp [decideThreshold, PromptAssembly.Budget.decideThreshold]

theorem threshold_one_over_is_reduce_eligible (effectiveInputBudget : Nat) :
    decideThreshold (effectiveInputBudget + 1) effectiveInputBudget = .reduceEligible := by
  simp [decideThreshold, PromptAssembly.Budget.decideThreshold]

inductive Outcome where
  | notNeeded (messages : List Nat)
  | cannotFit
  | reduced (compactedPrefix retainedSuffix : List Nat) (checkpoint : Nat)
  deriving DecidableEq, Repr

/-- Apply one reduction decision. A zero-length or overlong requested prefix is
not silently discarded; it becomes the typed `cannotFit` outcome. -/
def applyDecision (source : List Nat) : Decision → Outcome
  | .notNeeded => .notNeeded source
  | .cannotFit => .cannotFit
  | .reduce prefixLength checkpoint =>
      if 0 < prefixLength ∧ prefixLength ≤ source.length then
        .reduced (source.take prefixLength) (source.drop prefixLength) checkpoint
      else
        .cannotFit

/-- Combine the threshold gate with the execution result under one shared
decision owner. Scope-specific callers provide whether a legal bounded split
and checkpoint were produced; they do not reinterpret threshold equality. -/
def reductionDecision
    (inputTokens effectiveInputBudget : Nat) (canFit : Bool)
    (prefixLength checkpoint : Nat) : Decision :=
  match decideThreshold inputTokens effectiveInputBudget with
  | .notNeeded => .notNeeded
  | .reduceEligible =>
      if canFit then .reduce prefixLength checkpoint else .cannotFit

def reduce
    (source : List Nat) (inputTokens effectiveInputBudget : Nat) (canFit : Bool)
    (prefixLength checkpoint : Nat) : Outcome :=
  applyDecision source
    (reductionDecision inputTokens effectiveInputBudget canFit prefixLength checkpoint)

/-- The shared engine's semantic postcondition. A no-op preserves the complete
source, while a reduction returns an exact prefix/suffix partition. -/
def ExactOutcome (source : List Nat) : Outcome → Prop
  | .notNeeded messages => messages = source
  | .cannotFit => True
  | .reduced compactedPrefix retainedSuffix _ =>
      compactedPrefix ++ retainedSuffix = source

instance (source : List Nat) (outcome : Outcome) :
    Decidable (ExactOutcome source outcome) := by
  cases outcome <;> simp [ExactOutcome] <;> infer_instance

theorem apply_decision_is_exact (source : List Nat) (decision : Decision) :
    ExactOutcome source (applyDecision source decision) := by
  cases decision with
  | notNeeded => rfl
  | cannotFit => trivial
  | reduce prefixLength checkpoint =>
      simp only [applyDecision]
      split
      · exact List.take_append_drop prefixLength source
      · trivial

theorem reduce_is_exact
    (source : List Nat) (inputTokens effectiveInputBudget : Nat) (canFit : Bool)
    (prefixLength checkpoint : Nat) :
    ExactOutcome source
      (reduce source inputTokens effectiveInputBudget canFit prefixLength checkpoint) := by
  exact apply_decision_is_exact source _

theorem valid_reduce_has_requested_prefix_length
    (source : List Nat) (prefixLength checkpoint : Nat)
    (valid : 0 < prefixLength ∧ prefixLength ≤ source.length) :
    applyDecision source (.reduce prefixLength checkpoint) =
      .reduced (source.take prefixLength) (source.drop prefixLength) checkpoint ∧
    (source.take prefixLength).length = prefixLength := by
  constructor
  · simp [applyDecision, valid]
  · simp [List.length_take, Nat.min_eq_left valid.2]

theorem invalid_reduce_cannot_fit
    (source : List Nat) (prefixLength checkpoint : Nat)
    (invalid : ¬ (0 < prefixLength ∧ prefixLength ≤ source.length)) :
    applyDecision source (.reduce prefixLength checkpoint) = .cannotFit := by
  simp [applyDecision, invalid]

/-! ## Scope-specific persistence

These states are intentionally different types. The shared engine decides and
constructs the exact reduction; each caller then commits it according to its
own crash-cut and identity contract.
-/

structure SessionState where
  cursor : Nat
  checkpoint : Option Nat
  deriving DecidableEq, Repr

def commitSession (before : SessionState) : Outcome → SessionState
  | .reduced compactedPrefix _ checkpoint =>
      { cursor := before.cursor + compactedPrefix.length
      , checkpoint := some checkpoint }
  | _ => before

structure RequestLocalState where
  reductions : List (Nat × Nat)
  deriving DecidableEq, Repr

def commitRequestLocal (before : RequestLocalState) : Outcome → RequestLocalState
  | .reduced compactedPrefix _ checkpoint =>
      { reductions := before.reductions ++ [(compactedPrefix.length, checkpoint)] }
  | _ => before

theorem session_noop_does_not_commit
    (before : SessionState) (messages : List Nat) :
    commitSession before (.notNeeded messages) = before ∧
    commitSession before .cannotFit = before := by
  constructor <;> rfl

theorem request_local_noop_does_not_commit
    (before : RequestLocalState) (messages : List Nat) :
    commitRequestLocal before (.notNeeded messages) = before ∧
    commitRequestLocal before .cannotFit = before := by
  constructor <;> rfl

theorem session_commit_advances_exact_prefix
    (before : SessionState) (compactedPrefix retainedSuffix : List Nat)
    (checkpoint : Nat) :
    (commitSession before (.reduced compactedPrefix retainedSuffix checkpoint)).cursor =
      before.cursor + compactedPrefix.length := by
  rfl

theorem request_local_commit_appends_exact_prefix
    (before : RequestLocalState) (compactedPrefix retainedSuffix : List Nat)
    (checkpoint : Nat) :
    (commitRequestLocal before
      (.reduced compactedPrefix retainedSuffix checkpoint)).reductions =
      before.reductions ++ [(compactedPrefix.length, checkpoint)] := by
  rfl

end Compaction.ReductionEngine
