import Proofs.Inference.InferenceCall.SlotAccounting

/-! The terminal an admission permit records when it is dropped before it was
finished (#1366). A cancelled call token wins; otherwise the reason observed
for the call (a stream stall, or the reason an observer was given) is the
failure; otherwise the drop is a stream drop before any terminal response.
The observed reason is recorded as given; redaction and bounding of its text
are a separate owner. -/
namespace InferenceCall.DroppedPermit

inductive DropOutcome where
  | cancelled
  | observed
  | streamDropped
  deriving DecidableEq, Repr

def outcome (cancelled : Bool) (observed : Option String) : DropOutcome :=
  if cancelled then .cancelled
  else match observed with
    | some _ => .observed
    | none => .streamDropped

theorem cancelled_token_wins (observed : Option String) :
    outcome true observed = .cancelled := by
  simp [outcome]

theorem observed_reason_is_the_failure (reason : String) :
    outcome false (some reason) = .observed := by
  simp [outcome]

theorem silent_drop_is_a_stream_drop :
    outcome false none = .streamDropped := by
  simp [outcome]

theorem cancellation_never_becomes_a_failure (observed : Option String) :
    outcome true observed ≠ .observed ∧ outcome true observed ≠ .streamDropped := by
  simp [outcome]

/-- The state the dropped call's row records. -/
def recordedState : DropOutcome → InferenceCallState
  | .cancelled => .cancelled
  | .observed => .failed
  | .streamDropped => .failed

/-- Which failure reason the dropped call's row records: the cancellation
reason, the observed reason (as the failure-text owner records it), or the
stream-drop reason. -/
inductive RecordedReason where
  | cancelled
  | observedReason
  | streamDropped
  deriving DecidableEq, Repr

def recordedReason : DropOutcome → RecordedReason
  | .cancelled => .cancelled
  | .observed => .observedReason
  | .streamDropped => .streamDropped

/-- Backend slots a single call holds, reconstructed from its row's state. -/
def slotsHeld (state : InferenceCallState) : Nat :=
  if InferenceCallState.holdsBackendSlot state then 1 else 0

theorem a_dropped_permit_releases_its_slot (cancelled : Bool) (observed : Option String) :
    slotsHeld (recordedState (outcome cancelled observed)) = 0 := by
  cases cancelled <;> cases observed <;>
    simp [slotsHeld, recordedState, outcome, InferenceCallState.cancelled_no_backend_slot,
      InferenceCallState.failed_no_backend_slot]

theorem a_held_permit_holds_one_slot : slotsHeld .running = 1 := by
  simp [slotsHeld, InferenceCallState.running_holds_backend_slot]

theorem cancellation_is_recorded_as_cancelled (observed : Option String) :
    recordedState (outcome true observed) = .cancelled ∧
      recordedReason (outcome true observed) = .cancelled := by
  simp [recordedState, recordedReason, outcome]

/-! The runtime's exit. `Drop` cannot await, so the terminal above is written
by a cleanup task detached onto the runtime the permit was created on, and a
runtime that exits discards the cleanup still in flight. The interleaving is
`tla/DroppedPermitCleanup.tla`. -/

inductive Exit where
  /-- An ordered stop: the tasks that hold permits are joined, then the
  registry's cleanup tracker is awaited, then the runtime exits. -/
  | graceful
  /-- The teardown deadline cut the ordered stop. The flush has no deadline of
  its own. -/
  | deadline
  /-- A crash or a kill. -/
  | abrupt
  deriving DecidableEq, Repr

/-- Whether a dropped permit's cleanup runs, given the exit of its runtime and
whether the cleanup was still in flight at that exit. A graceful stop awaits
the tracker, so it never exits over cleanup in flight (`GracefulExitFlushed`).
The tracker counts a stream's finalizer while it owns the permit, because the
stop aborts that task without joining it. TLC's counterexample is Start,
BeginShutdown, BeginFinish, Exit both for a stop that does not await the
tracker (`UnawaitedFlush`) and for one that awaits it without counting the
finalizer (`UncountedFinalizer`). -/
def cleanupRuns (exit : Exit) (inFlightAtExit : Bool) : Bool :=
  match exit with
  | .graceful => true
  | .deadline | .abrupt => !inFlightAtExit

/-- Who installs the terminal of a call whose permit was dropped. -/
inductive TerminalWriter where
  /-- The permit's own cleanup: the terminal of `outcome`, or the terminal and
  usage a finish dropped mid-write had already marked. -/
  | cleanup
  /-- Inference-call recovery, at the next start or in the periodic sweep,
  once the call's request has ended: cancelled or failed, without usage. -/
  | recovery
  deriving DecidableEq, Repr

def terminalWriter (exit : Exit) (inFlightAtExit : Bool) : TerminalWriter :=
  if cleanupRuns exit inFlightAtExit then .cleanup else .recovery

/-- Provider usage the dropped permit had observed reaches the row only
through its own cleanup; recovery has no usage to record. -/
def keepsObservedUsage : TerminalWriter → Bool
  | .cleanup => true
  | .recovery => false

theorem a_graceful_stop_keeps_the_dropped_terminal (inFlightAtExit : Bool) :
    terminalWriter .graceful inFlightAtExit = .cleanup := by
  simp [terminalWriter, cleanupRuns]

theorem a_graceful_stop_keeps_observed_usage (inFlightAtExit : Bool) :
    keepsObservedUsage (terminalWriter .graceful inFlightAtExit) = true := by
  simp [terminalWriter, cleanupRuns, keepsObservedUsage]

/-- Recovery inherits a dropped permit's row only from an exit that was not
graceful and that found the cleanup in flight. -/
theorem recovery_owns_only_a_cut_or_abrupt_exit (exit : Exit) (inFlightAtExit : Bool)
    (h : terminalWriter exit inFlightAtExit = .recovery) :
    exit ≠ .graceful ∧ inFlightAtExit = true := by
  cases exit <;> cases inFlightAtExit <;> simp_all [terminalWriter, cleanupRuns]

theorem a_finished_cleanup_survives_any_exit (exit : Exit) :
    terminalWriter exit false = .cleanup := by
  cases exit <;> simp [terminalWriter, cleanupRuns]

end InferenceCall.DroppedPermit
