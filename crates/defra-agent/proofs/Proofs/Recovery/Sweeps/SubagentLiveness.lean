import Proofs.Recovery.Contract
import Proofs.Request.State

/-!
# Subagent Liveness Recovery Sweeps (#465)

Periodic sweeps over `AgentRequest` rows reached through running subagent
bridges. Without them, a background child whose executor died past its
deadline stays `processing` forever: the bridge never projects a terminal
result, the parent's response wait wedges, and queued descendants of
already-terminal parents never drain.

Two staleness sources, two sweeps:

* `expiredSubagentChildSweep` — a claimed/processing child whose deadline has
  passed. A live executor enforces its own request deadline, so an expired
  non-terminal row means the executor is gone; recovery terminalizes it to
  `dead` (the same transition `terminalize_expired_local_child_request`
  applies at startup).
* `queuedDescendantSweep` — a pending child whose parent request is already
  terminal can never legally run; recovery interrupts it (the queued-side
  analogue of the cascade interrupt for running children). `bridgeLinked`
  is the scope discriminator: only rows referenced by an `AgentToolCall`
  bridge are spawn descendants. Queue rows that merely CARRY spawn lineage —
  background-completion wake notifications, steering messages — are never
  bridge-linked and must survive a terminal caller.
-/

namespace Recovery

/-! ## Expired subagent child terminalization -/

structure ExpiredChildRow where
  state : RequestState
  deadlineExpired : Bool
  deriving Repr

def expiredChildStale (row : ExpiredChildRow) : Prop :=
  (row.state = .claimed ∨ row.state = .processing) ∧ row.deadlineExpired = true

instance (row : ExpiredChildRow) : Decidable (expiredChildStale row) := by
  unfold expiredChildStale
  infer_instance

def expiredChildRecover (row : ExpiredChildRow) : ExpiredChildRow :=
  { row with state := .dead }

def expiredChildUninterruptedTerminalize (row : ExpiredChildRow) : ExpiredChildRow :=
  { row with state := .dead }

def expiredChildMeasure (row : ExpiredChildRow) : Nat :=
  if expiredChildStale row then 1 else 0

theorem expiredChildRecover_matches_uninterrupted :
    ∀ row, expiredChildStale row →
      expiredChildRecover row = expiredChildUninterruptedTerminalize row := by
  intro row _h_stale
  simp [expiredChildRecover, expiredChildUninterruptedTerminalize]

theorem expiredChild_stale_positive :
    ∀ row, expiredChildStale row → expiredChildMeasure row > 0 := by
  intro row h_stale
  simp [expiredChildMeasure, h_stale]

theorem expiredChildRecover_terminal :
    ∀ row, expiredChildStale row → isTerminal (expiredChildRecover row).state := by
  intro row _h_stale
  simp [expiredChildRecover, HasTerminal.isTerminal, RequestState.instHasTerminal]

theorem expiredChildRecover_zero :
    ∀ row, expiredChildStale row →
      expiredChildMeasure (expiredChildRecover row) = 0 := by
  intro row _h_stale
  have h_not : ¬ expiredChildStale (expiredChildRecover row) := by
    intro h_stale
    rcases h_stale with ⟨h_state, _⟩
    cases h_state with
    | inl h_claimed => simp [expiredChildRecover] at h_claimed
    | inr h_processing => simp [expiredChildRecover] at h_processing
  simp [expiredChildMeasure, h_not]

def expiredSubagentChildSweep : RecoverySweep :=
  { Row := ExpiredChildRow
  , collection := .agentRequest
  , sweepId := "subagent_liveness_terminalize_expired_children"
  , rustFunction := "ToolCallLifecycle::reconcile_subagent_liveness"
  , cadence := .periodic
  , implementationStatus := .implemented
  , stale := expiredChildStale
  , recover := expiredChildRecover
  , terminal := fun row => isTerminal row.state
  , measure := expiredChildMeasure
  , h_stale_positive := expiredChild_stale_positive
  , h_recover_terminal := expiredChildRecover_terminal
  , h_recover_zero := expiredChildRecover_zero
  }

def expiredChildRecoveryEquivalence : RecoveryEquivalence expiredSubagentChildSweep :=
  { uninterrupted := expiredChildUninterruptedTerminalize
  , h_recover_eq_uninterrupted := expiredChildRecover_matches_uninterrupted
  }

/-! ## Queued descendants of terminal parents -/

structure QueuedDescendantRow where
  state : RequestState
  parentTerminal : Bool
  /-- True iff an `AgentToolCall` bridge references this row as its child
  (`child_request_id == request_id`). Lineage-only queue rows (wake
  notifications, steering messages) are NOT bridge-linked and never stale.
  Rust derives this predicate at sweep time via a bridge-existence query
  (`load_bridged_child_ids`); it is not a persisted column. -/
  bridgeLinked : Bool
  deriving Repr

def queuedDescendantStale (row : QueuedDescendantRow) : Prop :=
  row.state = .pending ∧ row.parentTerminal = true ∧ row.bridgeLinked = true

instance (row : QueuedDescendantRow) : Decidable (queuedDescendantStale row) := by
  unfold queuedDescendantStale
  infer_instance

def queuedDescendantRecover (row : QueuedDescendantRow) : QueuedDescendantRow :=
  { row with state := .interrupted }

def queuedDescendantUninterruptedTerminalize
    (row : QueuedDescendantRow) : QueuedDescendantRow :=
  { row with state := .interrupted }

def queuedDescendantMeasure (row : QueuedDescendantRow) : Nat :=
  if queuedDescendantStale row then 1 else 0

theorem queuedDescendantRecover_matches_uninterrupted :
    ∀ row, queuedDescendantStale row →
      queuedDescendantRecover row = queuedDescendantUninterruptedTerminalize row := by
  intro row _h_stale
  simp [queuedDescendantRecover, queuedDescendantUninterruptedTerminalize]

theorem queuedDescendant_stale_positive :
    ∀ row, queuedDescendantStale row → queuedDescendantMeasure row > 0 := by
  intro row h_stale
  simp [queuedDescendantMeasure, h_stale]

theorem queuedDescendantRecover_terminal :
    ∀ row, queuedDescendantStale row →
      isTerminal (queuedDescendantRecover row).state := by
  intro row _h_stale
  simp [queuedDescendantRecover, HasTerminal.isTerminal, RequestState.instHasTerminal]

theorem queuedDescendantRecover_zero :
    ∀ row, queuedDescendantStale row →
      queuedDescendantMeasure (queuedDescendantRecover row) = 0 := by
  intro row _h_stale
  have h_not : ¬ queuedDescendantStale (queuedDescendantRecover row) := by
    intro h_stale
    rcases h_stale with ⟨h_pending, _, _⟩
    simp [queuedDescendantRecover] at h_pending
  simp [queuedDescendantMeasure, h_not]

def queuedDescendantSweep : RecoverySweep :=
  { Row := QueuedDescendantRow
  , collection := .agentRequest
  , sweepId := "subagent_liveness_interrupt_queued_descendants"
  , rustFunction := "ToolCallLifecycle::reconcile_subagent_liveness"
  , cadence := .periodic
  , implementationStatus := .implemented
  , stale := queuedDescendantStale
  , recover := queuedDescendantRecover
  , terminal := fun row => isTerminal row.state
  , measure := queuedDescendantMeasure
  , h_stale_positive := queuedDescendant_stale_positive
  , h_recover_terminal := queuedDescendantRecover_terminal
  , h_recover_zero := queuedDescendantRecover_zero
  }

def queuedDescendantRecoveryEquivalence : RecoveryEquivalence queuedDescendantSweep :=
  { uninterrupted := queuedDescendantUninterruptedTerminalize
  , h_recover_eq_uninterrupted := queuedDescendantRecover_matches_uninterrupted
  }

end Recovery
