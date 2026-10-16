import Proofs.Compaction.ReductionEngine

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

theorem complete_chunks_are_valid
    (plan : Plan) (valid : plan.Valid) (chunk : Chunk)
    (member : chunk ∈ plan.chunks) : chunk.Valid := by
  exact valid.2.1 chunk member

def stepInput (prior : Option Checkpoint) (chunk : List Nat) : List Nat :=
  match prior with
  | none => chunk
  | some checkpoint => checkpoint.payload :: chunk

theorem step_input_starts_with_prior
    (checkpoint : Checkpoint) (chunk : List Nat) :
    stepInput (some checkpoint) chunk = checkpoint.payload :: chunk := by
  simp [stepInput]

end Compaction.Rolling
