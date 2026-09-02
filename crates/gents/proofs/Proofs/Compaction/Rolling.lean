/-!
# Atomic rolling compaction

An oversized provider-visible prefix is summarized through bounded in-memory
steps. Intermediate checkpoints are inputs to later steps, not durable session
cursors. A completed plan is valid only when every chunk is non-empty,
pair-closed, provider-dispatchable, and the chunks cover the exact target
prefix. Durable state changes once, after that final checkpoint exists.
-/

namespace Compaction.Rolling

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

structure DurableState where
  checkpoint : Option Checkpoint
  cursor : Nat
  deriving DecidableEq, Repr

inductive Result where
  | failed (completedSteps : Nat)
  | complete (plan : Plan) (valid : plan.Valid)

/-- Only a valid complete rolling plan is eligible for the single durable commit. -/
def commit (before : DurableState) : Result → DurableState
  | .failed _ => before
  | .complete plan _ =>
      { checkpoint := some plan.checkpoint
      , cursor := plan.checkpoint.messagesCovered }

/-- A failure at chunk N cannot expose any earlier in-memory checkpoint or
advance the durable cursor. -/
theorem chunk_failure_preserves_durable_state
    (before : DurableState) (completedSteps : Nat) :
    commit before (.failed completedSteps) = before := by
  rfl

/-- A successful roll publishes exactly the final checkpoint and advances to
the complete target prefix, never an intermediate step. -/
theorem complete_commits_exact_target
    (before : DurableState) (plan : Plan) (valid : plan.Valid) :
    commit before (.complete plan valid) =
      { checkpoint := some plan.checkpoint
      , cursor := plan.targetMessages } := by
  simp only [commit]
  rw [valid.2.2.2]

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
