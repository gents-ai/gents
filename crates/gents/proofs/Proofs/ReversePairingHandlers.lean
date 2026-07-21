import Proofs.ApplyReconcile.Collections

/-!
# Reverse-Pairing Handler State

Receiver-side state and pure handler effects for reverse-pairing subscription
management. This discharges the handler-idempotency obligation surfaced by the
TLA+ reverse-pairing model: install is set insertion, teardown is set removal,
and repeated delivery of the same logical RPC leaves the persisted receiver
state unchanged after the first application.
-/

namespace ReversePairingHandlers

/-- Runtime collection vocabulary shared with the apply/reconcile model. -/
abbrev Collection := ApplyReconcile.Collection

/-- Minimal persisted receiver state for subscription installation. -/
structure ReceiverState where
  subscribed : Finset Collection

/-- Apply an install RPC by persisting membership of the collection. -/
def applyInstall (s : ReceiverState) (c : Collection) : ReceiverState :=
  { s with subscribed := insert c s.subscribed }

/-- Apply a teardown RPC by removing persisted membership of the collection. -/
def applyTeardown (s : ReceiverState) (c : Collection) : ReceiverState :=
  { s with subscribed := s.subscribed.erase c }

theorem install_idempotent (s : ReceiverState) (c : Collection) :
    applyInstall (applyInstall s c) c = applyInstall s c := by
  simp [applyInstall]

theorem teardown_idempotent (s : ReceiverState) (c : Collection) :
    applyTeardown (applyTeardown s c) c = applyTeardown s c := by
  simp [applyTeardown]

theorem install_then_teardown (s : ReceiverState) (c : Collection) :
    applyTeardown (applyInstall s c) c = applyTeardown s c := by
  simp [applyInstall, applyTeardown]

theorem teardown_then_install (s : ReceiverState) (c : Collection) :
    applyInstall (applyTeardown s c) c = applyInstall s c := by
  cases s with
  | mk subscribed =>
      simp [applyInstall, applyTeardown]
      ext x
      by_cases h : x = c <;> simp [h]

end ReversePairingHandlers
