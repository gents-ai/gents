-- Reuse the existing merge operator and ordered fold definitions.
import Regolith.MergeOperator

/-!
A commit may replace two adjacent operands with the caller's exact partial
merge. For example, [+1, +1] becomes [+2]. It must not reorder operands,
invent an answer when the caller declines, or rewrite an older commit.
These proofs cover every base and every surrounding operand sequence.
The balanced fold replaces only adjacent subtrees that each reduced to one operand;
each replacement is a Step. A unique-key buffer uses Steps.refl.
Publication and snapshot registration remain the existing pipeline's job.
-/

-- Keep these proofs separate from the existing general merge laws.
namespace Regolith.BatchMergeFold

-- Use the imported operator and fold names without repeating their namespace.
open Regolith.MergeOperator

/-- A successful callback agrees with the original pair from every base. -/
def ExactAccepted {V O : Type} (M : Operator V O)
    -- The callback may decline rather than produce a combined operand.
    (pm : O → O → Option O) : Prop :=
  -- Only successful callbacks authorize replacing the pair.
  ∀ a b out, pm a b = some out → ∀ v,
    M.apply (M.apply v a) b = M.apply v out

/-- One accepted replacement, at the same key within one atomic commit. -/
inductive Step {O : Type} (pm : O → O → Option O) : List O → List O → Prop
  -- The prefix and suffix are unchanged; a declined callback has no step.
  | accepted (xs ys : List O) (a b out : O)
      -- A replacement requires a successful callback for this exact pair.
      (ok : pm a b = some out) :
      -- Replace only the adjacent pair, leaving the surrounding writes unchanged.
      Step pm (xs ++ a :: b :: ys) (xs ++ out :: ys)

/-- Any accepted replacement keeps the read, including its surrounding writes. -/
theorem step_preserves_read {V O : Type} {M : Operator V O}
    -- Assume every accepted callback preserves the pair's meaning.
    {pm : O → O → Option O} (lawful : ExactAccepted M pm)
    -- Consider any accepted replacement and any starting value.
    {before after : List O} (step : Step pm before after) (base : V) :
    -- The read before and after that replacement must agree.
    fullMerge M base before = fullMerge M base after := by
  -- A step carries both the original adjacent pair and the callback's success.
  cases step with
  -- Unpack the surrounding writes and successful callback.
  | accepted xs ys a b out ok =>
    -- Fold the unchanged prefix, then the pair, then the unchanged suffix.
    simp only [fullMerge, List.foldl_append, List.foldl_cons]
    -- Exactness makes the two starting values for the suffix equal.
    rw [lawful a b out ok]

/-- Zero or more accepted replacements within the same commit. -/
inductive Steps {O : Type} (pm : O → O → Option O) : List O → List O → Prop
  -- Not folding is always permitted, including for a declined callback.
  | refl (ops : List O) : Steps pm ops ops
  -- Each new step must carry its own successful callback.
  | tail {a b c : List O} : Steps pm a b → Step pm b c → Steps pm a c

/-- Repeated accepted replacements preserve the read from every base. -/
theorem steps_preserve_read {V O : Type} {M : Operator V O}
    -- Each successful callback keeps the same exactness contract.
    {pm : O → O → Option O} (lawful : ExactAccepted M pm)
    -- The original and folded operand streams may have any length.
    {before after : List O}
    -- All replacements belong to one commit and start from any base.
    (steps : Steps pm before after) (base : V) :
    -- Reading either complete stream must give the same value.
    fullMerge M base before = fullMerge M base after := by
  -- An empty sequence is equality; every additional step keeps that equality.
  induction steps with
  -- No replacement leaves the same operand stream.
  | refl => rfl
  -- Earlier replacements and the final replacement each preserve the read.
  | tail _ step ih => exact ih.trans (step_preserves_read lawful step base)

/-- Folding a later commit cannot change a read of the older commit alone. -/
theorem earlier_read_unchanged {V O : Type} (M : Operator V O)
    -- Keep the older commit separate from both forms of the later commit.
    (base : V) (older : List O) (laterBefore laterAfter : List O)
    -- Assume folding the later commit preserves its read from the older value.
    (read : fullMerge M (fullMerge M base older) laterBefore =
      fullMerge M (fullMerge M base older) laterAfter) :
    -- The old read and the combined history must both remain unchanged.
    fullMerge M base older = fullMerge M base older ∧
      fullMerge M base (older ++ laterBefore) =
        fullMerge M base (older ++ laterAfter) := by
  -- The old sequence is not rewritten; concatenated folds use the supplied equality.
  constructor
  -- The older stream is identical on both sides.
  · rfl
  -- Folding concatenated streams is folding the older and later streams in order.
  · simpa only [fullMerge, List.foldl_append] using read

-- End the namespace of the same-commit folding proofs.
end Regolith.BatchMergeFold
