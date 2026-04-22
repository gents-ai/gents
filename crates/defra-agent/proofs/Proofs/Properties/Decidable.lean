import Proofs.Request
import Proofs.Process
import Proofs.Persistence
import Proofs.Scheduling
import Mathlib.Data.Fintype.Card

/-!
# Decidable Finite-State Checks

Finite enumeration and simple coherence checks over the ideal state space.
-/

open RequestState ProcessState PersistenceState ExecutionOrigin AdmissionState

instance : Fintype RequestState :=
  Fintype.ofList
    [.pending, .claimed, .processing, .inputRequired,
     .completed, .failed, .superseded, .dead, .interrupted]
    (fun s => by cases s <;> simp)

instance : Fintype ProcessState :=
  Fintype.ofList
    [.uninitialized, .recovering, .ready, .shuttingDown, .shutdown]
    (fun s => by cases s <;> simp)

instance : Fintype PersistenceState :=
  Fintype.ofList
    [.uncommitted, .committing, .committed, .lost]
    (fun s => by cases s <;> simp)

instance : Fintype ExecutionOrigin :=
  Fintype.ofList
    [.interactive, .scheduled]
    (fun s => by cases s <;> simp)

instance : Fintype AdmissionState :=
  Fintype.ofList
    [.released, .waiting, .acquired, .executing]
    (fun s => by cases s <;> simp)

theorem request_no_deadlocks (s : RequestState) (h : ¬isTerminal s) :
    ∃ s' : RequestState, s ≠ s' := by
  cases s with
  | pending => exact ⟨.claimed, by decide⟩
  | claimed => exact ⟨.processing, by decide⟩
  | processing => exact ⟨.completed, by decide⟩
  | inputRequired => exact ⟨.processing, by decide⟩
  | completed => exact absurd (Or.inl rfl) h
  | failed => exact absurd (Or.inr (Or.inl rfl)) h
  | superseded => exact absurd (Or.inr (Or.inr (Or.inl rfl))) h
  | dead => exact absurd (Or.inr (Or.inr (Or.inr (Or.inl rfl)))) h
  | interrupted => exact absurd (Or.inr (Or.inr (Or.inr (Or.inr rfl)))) h

theorem process_no_deadlocks (s : ProcessState) (h : ¬isTerminal s) :
    ∃ s' : ProcessState, s ≠ s' := by
  cases s with
  | uninitialized => exact ⟨.recovering, by decide⟩
  | recovering => exact ⟨.ready, by decide⟩
  | ready => exact ⟨.shuttingDown, by decide⟩
  | shuttingDown => exact ⟨.shutdown, by decide⟩
  | shutdown => exact absurd rfl h

theorem persistence_no_deadlocks (s : PersistenceState) (h : ¬isTerminal s) :
    ∃ s' : PersistenceState, s ≠ s' := by
  cases s with
  | uncommitted => exact ⟨.committing, by decide⟩
  | committing => exact ⟨.committed, by decide⟩
  | committed => exact absurd (Or.inl rfl) h
  | lost => exact absurd (Or.inr rfl) h

theorem pending_requires_released (a : AdmissionState) :
    RequestContext.coherentStateAdmission .pending a ↔ a = .released := by
  cases a <;> simp [RequestContext.coherentStateAdmission]

theorem claimed_requires_waiting_or_acquired (a : AdmissionState) :
    RequestContext.coherentStateAdmission .claimed a ↔ a = .waiting ∨ a = .acquired := by
  cases a <;> simp [RequestContext.coherentStateAdmission]

theorem processing_requires_executing (a : AdmissionState) :
    RequestContext.coherentStateAdmission .processing a ↔ a = .executing := by
  cases a <;> simp [RequestContext.coherentStateAdmission]

theorem terminal_requires_released (s : RequestState) (a : AdmissionState)
    (h_terminal : isTerminal s) :
    RequestContext.coherentStateAdmission s a ↔ a = .released := by
  cases h_terminal with
  | inl h =>
    subst h
    cases a <;> simp [RequestContext.coherentStateAdmission]
  | inr h =>
    cases h with
    | inl h =>
      subst h
      cases a <;> simp [RequestContext.coherentStateAdmission]
    | inr h =>
      cases h with
      | inl h =>
        subst h
        cases a <;> simp [RequestContext.coherentStateAdmission]
      | inr h =>
        cases h with
        | inl h =>
          subst h
          cases a <;> simp [RequestContext.coherentStateAdmission]
        | inr h =>
          subst h
          cases a <;> simp [RequestContext.coherentStateAdmission]

#eval Fintype.card RequestState
#eval Fintype.card ProcessState
#eval Fintype.card PersistenceState
#eval Fintype.card RequestState * Fintype.card ProcessState * Fintype.card PersistenceState
#eval Fintype.card ExecutionOrigin
#eval Fintype.card AdmissionState
