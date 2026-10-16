import Proofs.InferenceCall.State

/-!
Write guards at the persisted inference-call database boundary. Failure and
completion require running; cancellation also permits queued. Terminal outcome
and stamp form one first-winner projection. Usage is an independently observed
provider ledger value; updating it does not itself charge an aggregate budget.
-/
namespace InferenceCall.Persistence

structure Row (Usage : Type) where
  call : InferenceCall
  terminalStamp : Option Nat
  usage : Option Usage

variable {Usage : Type}

def start (current : Row Usage) : Row Usage :=
  if current.call.state = .queued then
    { current with call := { current.call with state := .running } }
  else current

def terminalWriteAllowed (current target : InferenceCallState) : Prop :=
  (current = .queued ∨ current = .running) ∧
    isTerminal target ∧ ((target = .completed ∨ target = .failed) → current = .running)

instance (current target : InferenceCallState) :
    Decidable (terminalWriteAllowed current target) := by
  unfold terminalWriteAllowed
  infer_instance

def terminalize (current : Row Usage) (target : InferenceCallState) (stamp : Nat) : Row Usage :=
  if terminalWriteAllowed current.call.state target then
    { current with call := { current.call with state := target }, terminalStamp := some stamp }
  else current

def observeUsage (current : Row Usage) (observed : Usage) : Row Usage :=
  { current with usage := some observed }

theorem start_requires_current_queued (current : Row Usage)
    (h_not_queued : current.call.state ≠ .queued) : start current = current := by
  simp [start, h_not_queued]

theorem terminal_write_requires_current_live (current : Row Usage)
    (h_terminal : isTerminal current.call.state) (target : InferenceCallState) :
    ¬ terminalWriteAllowed current.call.state target := by
  cases h_state : current.call.state <;>
    simp [h_state, HasTerminal.isTerminal, InferenceCallState.instHasTerminal,
      terminalWriteAllowed] at h_terminal ⊢

theorem terminal_winner_preserves_outcome_and_stamp (current : Row Usage)
    (h_terminal : isTerminal current.call.state) (target : InferenceCallState) (stamp : Nat) :
    terminalize current target stamp = current := by
  simp [terminalize, terminal_write_requires_current_live current h_terminal target]

theorem recovery_winner_cannot_be_reopened (current : Row Usage)
    (h_terminal : isTerminal current.call.state) : start current = current := by
  apply start_requires_current_queued
  intro h_queued
  simp [h_queued, HasTerminal.isTerminal, InferenceCallState.instHasTerminal] at h_terminal

theorem late_usage_preserves_terminal_projection (current : Row Usage) (observed : Usage) :
    (observeUsage current observed).call = current.call ∧
    (observeUsage current observed).terminalStamp = current.terminalStamp := by
  exact ⟨rfl, rfl⟩

theorem late_usage_is_recorded (current : Row Usage) (observed : Usage) :
    (observeUsage current observed).usage = some observed := rfl

end InferenceCall.Persistence
