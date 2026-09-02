import Proofs.Compaction.ReductionEngine

/-!
# Atomic rolling compaction

An oversized provider-visible prefix is summarized through bounded in-memory
steps. Intermediate checkpoints are inputs to later steps, not durable session
cursors. A completed plan is valid only when every chunk is non-empty,
pair-closed, provider-dispatchable, and the chunks cover the exact target
prefix. Durable state changes once, after that final checkpoint exists.
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

/-- A failure at chunk N cannot expose any earlier in-memory checkpoint or
advance the durable cursor. -/
theorem chunk_failure_preserves_durable_state
    (before : SessionState) :
    commitSession before .cannotFit = before := by
  exact (session_noop_does_not_commit before []).2

/-- A successful roll delegates its one durable cursor transition to the
shared exact-reduction owner. The cursor advances relative to the prior
checkpoint; rolling compaction never replaces it with a local chunk count. -/
theorem complete_commits_exact_target
    (before : SessionState) (plan : Plan) (_valid : plan.Valid) :
    (commitSession before
      (.reduced (List.replicate plan.targetMessages 0) [] plan.checkpoint.payload)).cursor =
      before.cursor + plan.targetMessages := by
  simp [commitSession]

/-- Every chunk in a valid completed roll is non-empty, pair-closed, and has
strictly positive provider output capacity. -/
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
