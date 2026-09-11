import Proofs.Compaction.ReductionEngine

/-!
# Rolling compaction plan checks

This model checks non-empty chunk counts, supplied pair/dispatch observations,
and their total against the declared target. It also constructs the next step's
input from a prior checkpoint. Counts alone do not establish exact source
coverage, and these functions do not model chunk execution or durable atomicity.
Exact prefix/suffix construction belongs to ReductionEngine.applyDecision.
-/

namespace Compaction.Rolling

open Compaction.ReductionEngine

structure Checkpoint where
  payload : Nat
  messagesCovered : Nat
  deriving DecidableEq, Repr

structure Chunk where
  messages : Nat
  pairClosed : Bool
  canDispatch : Bool
  deriving DecidableEq, Repr

def Chunk.Valid (chunk : Chunk) : Prop :=
  0 < chunk.messages ∧ chunk.pairClosed = true ∧ chunk.canDispatch = true

instance (chunk : Chunk) : Decidable chunk.Valid := by
  unfold Chunk.Valid
  infer_instance

structure Plan where
  targetMessages : Nat
  chunks : List Chunk
  checkpoint : Checkpoint
  deriving DecidableEq, Repr

def Plan.Valid (plan : Plan) : Prop :=
  plan.chunks ≠ [] ∧
  (∀ chunk ∈ plan.chunks, chunk.Valid) ∧
  (plan.chunks.map Chunk.messages).sum = plan.targetMessages ∧
  plan.checkpoint.messagesCovered = plan.targetMessages

instance (plan : Plan) : Decidable plan.Valid := by
  unfold Plan.Valid
  infer_instance

/-- Every chunk in a valid completed roll is non-empty, pair-closed, and has
a supplied affirmative dispatch observation. -/
theorem complete_chunks_are_valid
    (plan : Plan) (valid : plan.Valid) (chunk : Chunk)
    (member : chunk ∈ plan.chunks) : chunk.Valid := by
  exact valid.2.1 chunk member

/-- Every step after the first receives the prior checkpoint before its next
bounded chunk. -/
def stepInput (prior : Option Checkpoint) (chunk : List Nat) : List Nat :=
  match prior with
  | none => chunk
  | some checkpoint => checkpoint.payload :: chunk

theorem step_input_starts_with_prior
    (checkpoint : Checkpoint) (chunk : List Nat) :
    stepInput (some checkpoint) chunk = checkpoint.payload :: chunk := by
  simp [stepInput]

end Compaction.Rolling
